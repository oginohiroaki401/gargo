use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;

use crossterm::style::Color;

use crate::core::editor::Editor;
use crate::input::action::{Action, AppAction, WorkspaceAction};

use super::git_backend;
use super::registry::{CommandEffect, CommandEntry, CommandRegistry, copy_to_clipboard};

// ---------------------------------------------------------------------------
// Public git helpers for GitView
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GitFileEntry {
    pub path: String,
    pub status_char: char,
    pub staged: bool,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitBatchOperationResult {
    pub successes: usize,
    pub failures: Vec<(String, String)>,
}

impl GitBatchOperationResult {
    pub fn total(&self) -> usize {
        self.successes + self.failures.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    Modified,
    Added,
    Untracked,
    Deleted,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitLineStatus {
    Added,
    Modified,
    Deleted,
}

impl GitLineStatus {
    pub fn gutter_symbol(&self) -> char {
        match self {
            GitLineStatus::Added | GitLineStatus::Modified => '▍',
            GitLineStatus::Deleted => '▔',
        }
    }

    pub fn gutter_bg(&self) -> Color {
        match self {
            GitLineStatus::Added => Color::DarkGreen,
            GitLineStatus::Modified => Color::DarkYellow,
            GitLineStatus::Deleted => Color::DarkRed,
        }
    }
}

impl GitFileStatus {
    pub fn color(&self) -> Color {
        match self {
            GitFileStatus::Modified => Color::Yellow,
            GitFileStatus::Added => Color::Green,
            GitFileStatus::Untracked => Color::DarkGreen,
            GitFileStatus::Deleted => Color::Red,
            GitFileStatus::Conflict => Color::Magenta,
        }
    }

    pub fn indicator(&self) -> char {
        match self {
            GitFileStatus::Modified => 'M',
            GitFileStatus::Added => 'A',
            GitFileStatus::Untracked => '?',
            GitFileStatus::Deleted => 'D',
            GitFileStatus::Conflict => 'U',
        }
    }

    fn priority(&self) -> u8 {
        match self {
            GitFileStatus::Conflict => 4,
            GitFileStatus::Deleted => 3,
            GitFileStatus::Modified => 2,
            GitFileStatus::Added => 1,
            GitFileStatus::Untracked => 0,
        }
    }
}

/// Parse `git status --porcelain` into a map of relative paths to their git status.
pub fn git_status_map() -> HashMap<String, GitFileStatus> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return HashMap::new(),
    };
    git_backend::status_map(&cwd)
}

/// Get the aggregated git status for a directory prefix.
/// Returns the highest-priority status of any file under the given prefix.
pub fn dir_git_status(
    map: &HashMap<String, GitFileStatus>,
    dir_prefix: &str,
) -> Option<GitFileStatus> {
    let mut best: Option<GitFileStatus> = None;
    for (path, status) in map {
        if path.starts_with(dir_prefix) {
            match best {
                None => best = Some(*status),
                Some(current) => {
                    if status.priority() > current.priority() {
                        best = Some(*status);
                    }
                }
            }
        }
    }
    best
}

pub fn git_branch() -> Result<String, String> {
    current_branch_in(None)
}

pub fn git_branch_in(project_root: &Path) -> Result<String, String> {
    current_branch_in(Some(project_root))
}

pub fn git_status_files() -> Result<(Vec<GitFileEntry>, Vec<GitFileEntry>), String> {
    git_status_files_in_impl(None)
}

pub fn git_status_files_in(
    project_root: &Path,
) -> Result<(Vec<GitFileEntry>, Vec<GitFileEntry>), String> {
    git_status_files_in_impl(Some(project_root))
}

fn git_status_files_in_impl(
    project_root: Option<&Path>,
) -> Result<(Vec<GitFileEntry>, Vec<GitFileEntry>), String> {
    let raw = git_output_in(project_root, &["status", "--porcelain"])?;
    let mut changed = Vec::new();
    let mut staged = Vec::new();
    for line in raw.lines() {
        if line.len() < 4 {
            continue;
        }
        let bytes = line.as_bytes();
        let index_status = bytes[0] as char;
        let worktree_status = bytes[1] as char;
        let path = line[3..].to_string();

        // Index (staged) changes
        if index_status != ' ' && index_status != '?' {
            staged.push(GitFileEntry {
                path: path.clone(),
                status_char: index_status,
                staged: true,
                additions: 0,
                deletions: 0,
            });
        }
        // Worktree (unstaged) changes
        if worktree_status != ' ' {
            changed.push(GitFileEntry {
                path: path.clone(),
                status_char: if worktree_status == '?' {
                    '?'
                } else {
                    worktree_status
                },
                staged: false,
                additions: 0,
                deletions: 0,
            });
        }
    }

    let unstaged_stats = numstat_map(project_root, false);
    let staged_stats = numstat_map(project_root, true);
    for entry in changed.iter_mut() {
        if entry.status_char == '?' {
            entry.additions = count_file_lines(project_root, &entry.path);
            entry.deletions = 0;
        } else if let Some(&(adds, dels)) = unstaged_stats.get(&entry.path) {
            entry.additions = adds;
            entry.deletions = dels;
        }
    }
    for entry in staged.iter_mut() {
        if let Some(&(adds, dels)) = staged_stats.get(&entry.path) {
            entry.additions = adds;
            entry.deletions = dels;
        }
    }
    Ok((changed, staged))
}

fn numstat_map(
    project_root: Option<&Path>,
    cached: bool,
) -> HashMap<String, (usize, usize)> {
    let args: &[&str] = if cached {
        &["diff", "--cached", "--numstat"]
    } else {
        &["diff", "--numstat"]
    };
    let raw = match git_output_in(project_root, args) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    let mut map = HashMap::new();
    for line in raw.lines() {
        let mut parts = line.splitn(3, '\t');
        let adds = parts.next().unwrap_or("0");
        let dels = parts.next().unwrap_or("0");
        let path = match parts.next() {
            Some(p) => p,
            None => continue,
        };
        let adds = adds.parse::<usize>().unwrap_or(0);
        let dels = dels.parse::<usize>().unwrap_or(0);
        map.insert(path.to_string(), (adds, dels));
    }
    map
}

fn count_file_lines(project_root: Option<&Path>, rel_path: &str) -> usize {
    let abs = match project_root {
        Some(root) => root.join(rel_path),
        None => match std::env::current_dir() {
            Ok(cwd) => cwd.join(rel_path),
            Err(_) => return 0,
        },
    };
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    if bytes.is_empty() {
        return 0;
    }
    let nl_count = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.last() == Some(&b'\n') {
        nl_count
    } else {
        nl_count + 1
    }
}

pub fn git_diff(path: &str, staged: bool) -> Result<String, String> {
    git_diff_in_impl(None, path, staged)
}

pub fn git_diff_in(project_root: &Path, path: &str, staged: bool) -> Result<String, String> {
    git_diff_in_impl(Some(project_root), path, staged)
}

fn git_diff_in_impl(
    project_root: Option<&Path>,
    path: &str,
    staged: bool,
) -> Result<String, String> {
    if staged {
        git_output_in(project_root, &["diff", "--cached", "--", path])
    } else {
        git_output_in(project_root, &["diff", "--", path])
    }
}

pub fn git_local_branches_in(project_root: &Path) -> Result<Vec<(String, bool)>, String> {
    let raw = git_output_in(
        Some(project_root),
        &["branch", "--format=%(refname:short)|%(HEAD)"],
    )?;
    let mut branches = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (name, head_marker) = line.split_once('|').unwrap_or((line, ""));
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let is_current = head_marker.trim() == "*";
        branches.push((name.to_string(), is_current));
    }
    branches.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(branches)
}

pub fn git_branch_preview_in(project_root: &Path, branch: &str) -> Result<Vec<String>, String> {
    let mut lines = vec![
        format!("Branch: {}", branch),
        String::new(),
        "Working tree status:".to_string(),
    ];
    match git_output_in(Some(project_root), &["status", "--short", "--branch"]) {
        Ok(status) => {
            if status.trim().is_empty() {
                lines.push("(clean)".to_string());
            } else {
                lines.extend(status.lines().map(|line| line.to_string()));
            }
        }
        Err(err) => {
            lines.push(format!("(status unavailable: {})", err));
        }
    }

    lines.push(String::new());
    lines.push(format!("Recent commits on {}:", branch));
    match git_output_in(
        Some(project_root),
        &["log", "--oneline", "--decorate", "-n", "15", branch],
    ) {
        Ok(log) => {
            if log.trim().is_empty() {
                lines.push("(no commits)".to_string());
            } else {
                lines.extend(log.lines().map(|line| line.to_string()));
            }
        }
        Err(err) => {
            lines.push(format!("(log unavailable: {})", err));
        }
    }
    Ok(lines)
}

pub fn git_switch_branch_in(project_root: &Path, branch: &str) -> Result<(), String> {
    match git_output_in(Some(project_root), &["switch", branch]) {
        Ok(_) => Ok(()),
        Err(switch_err) => match git_output_in(Some(project_root), &["checkout", branch]) {
            Ok(_) => Ok(()),
            Err(checkout_err) => Err(format!(
                "git switch failed: {}; git checkout failed: {}",
                switch_err, checkout_err
            )),
        },
    }
}

pub fn git_has_staged_changes_in(project_root: &Path) -> Result<bool, String> {
    let output = ProcessCommand::new("git")
        .current_dir(project_root)
        .args(["diff", "--cached", "--quiet"])
        .output()
        .map_err(|e| format!("git error: {}", e))?;

    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                Err("git error: failed to check staged changes".to_string())
            } else {
                Err(format!("git error: {}", stderr))
            }
        }
    }
}

pub fn git_commit_editmsg_path_in(project_root: &Path) -> Result<PathBuf, String> {
    let path = git_output_in(
        Some(project_root),
        &["rev-parse", "--git-path", "COMMIT_EDITMSG"],
    )?;
    if path.is_empty() {
        return Err("git error: failed to resolve COMMIT_EDITMSG path".to_string());
    }
    let path_buf = PathBuf::from(path);
    if path_buf.is_absolute() {
        Ok(path_buf)
    } else {
        Ok(project_root.join(path_buf))
    }
}

pub fn git_prepare_commit_editmsg_template_in(
    project_root: &Path,
    commit_editmsg_path: &Path,
) -> Result<(), String> {
    // Force regeneration so stale content never leaks into a new commit flow.
    let _ = std::fs::remove_file(commit_editmsg_path);

    let mut cmd = ProcessCommand::new("git");
    cmd.current_dir(project_root)
        .args(["commit", "--no-verify"]);
    if cfg!(windows) {
        cmd.env("GIT_EDITOR", "cmd /C exit 1");
    } else {
        cmd.env("GIT_EDITOR", "false");
    }

    let output = cmd.output().map_err(|e| format!("git error: {}", e))?;
    if commit_editmsg_path.is_file() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "git commit did not produce COMMIT_EDITMSG".to_string()
    };
    Err(format!("git error: {}", detail))
}

pub fn git_strip_commit_message_in(
    project_root: &Path,
    raw_message: &str,
) -> Result<String, String> {
    let mut child = ProcessCommand::new("git")
        .current_dir(project_root)
        .args(["stripspace", "--strip-comments"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git error: {}", e))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "git error: failed to open stdin for stripspace".to_string())?;
        stdin
            .write_all(raw_message.as_bytes())
            .map_err(|e| format!("git error: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("git error: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err("git error: stripspace failed".to_string());
        }
        return Err(format!("git error: {}", stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn git_commit_with_message_file_in(
    project_root: &Path,
    message_file: &Path,
) -> Result<String, String> {
    let output = ProcessCommand::new("git")
        .current_dir(project_root)
        .args(["commit", "--cleanup=strip", "-F"])
        .arg(message_file)
        .output()
        .map_err(|e| format!("git error: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err("git error: commit failed".to_string());
        }
        return Err(format!("git error: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let summary = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Committed");
    Ok(summary.to_string())
}

pub fn parse_diff_hunks(diff: &str) -> HashMap<usize, GitLineStatus> {
    let mut map = HashMap::new();
    for line in diff.lines() {
        if !line.starts_with("@@") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _marker = parts.next();
        let old_range = match parts.next() {
            Some(value) => value,
            None => continue,
        };
        let new_range = match parts.next() {
            Some(value) => value,
            None => continue,
        };

        let (_, old_count) = match parse_hunk_range(old_range) {
            Some(value) => value,
            None => continue,
        };
        let (new_start, new_count) = match parse_hunk_range(new_range) {
            Some(value) => value,
            None => continue,
        };

        if old_count == 0 && new_count > 0 {
            for offset in 0..new_count {
                let line_num = new_start + offset;
                if line_num > 0 {
                    map.insert(line_num - 1, GitLineStatus::Added);
                }
            }
        } else if new_count == 0 && old_count > 0 {
            if new_start > 0 {
                map.insert(new_start - 1, GitLineStatus::Deleted);
            }
        } else if new_count > 0 {
            for offset in 0..new_count {
                let line_num = new_start + offset;
                if line_num > 0 {
                    map.insert(line_num - 1, GitLineStatus::Modified);
                }
            }
        }
    }
    map
}

pub fn git_diff_line_status(path: &str) -> HashMap<usize, GitLineStatus> {
    git_backend::diff_line_status_for_file(Path::new(path))
}

pub fn git_stage(path: &str) -> Result<(), String> {
    git_stage_in_impl(None, path)?;
    Ok(())
}

pub fn git_unstage(path: &str) -> Result<(), String> {
    git_unstage_in_impl(None, path)?;
    Ok(())
}

pub fn git_stage_in(project_root: &Path, path: &str) -> Result<(), String> {
    git_stage_in_impl(Some(project_root), path)?;
    Ok(())
}

pub fn git_stage_many_in(project_root: &Path, paths: &[String]) -> GitBatchOperationResult {
    let mut result = GitBatchOperationResult::default();
    for path in paths {
        match git_stage_in_impl(Some(project_root), path) {
            Ok(()) => result.successes = result.successes.saturating_add(1),
            Err(err) => result.failures.push((path.clone(), err)),
        }
    }
    result
}

pub fn git_unstage_in(project_root: &Path, path: &str) -> Result<(), String> {
    git_unstage_in_impl(Some(project_root), path)?;
    Ok(())
}

pub fn git_unstage_many_in(project_root: &Path, paths: &[String]) -> GitBatchOperationResult {
    let mut result = GitBatchOperationResult::default();
    for path in paths {
        match git_unstage_in_impl(Some(project_root), path) {
            Ok(()) => result.successes = result.successes.saturating_add(1),
            Err(err) => result.failures.push((path.clone(), err)),
        }
    }
    result
}

fn git_stage_in_impl(project_root: Option<&Path>, path: &str) -> Result<(), String> {
    git_output_in(project_root, &["add", "--", path])?;
    Ok(())
}

fn git_unstage_in_impl(project_root: Option<&Path>, path: &str) -> Result<(), String> {
    git_output_in(project_root, &["restore", "--staged", "--", path])?;
    Ok(())
}

fn open_url(url: &str) -> Result<(), String> {
    let result = if cfg!(target_os = "macos") {
        ProcessCommand::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        ProcessCommand::new("cmd").args(["/C", "start", "", url]).spawn()
    } else {
        ProcessCommand::new("xdg-open").arg(url).spawn()
    };
    result.map(|_| ()).map_err(|e| e.to_string())
}

fn remote_to_github_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let url = if remote.starts_with("git@github.com:") {
        let path = remote.strip_prefix("git@github.com:")?;
        format!("https://github.com/{}", path)
    } else if remote.starts_with("https://github.com/") || remote.starts_with("http://github.com/")
    {
        remote.to_string()
    } else {
        return None;
    };
    let url = url.strip_suffix(".git").unwrap_or(&url);
    Some(url.to_string())
}

/// Resolve the git repository root from a file path by running
/// `git rev-parse --show-toplevel` in the file's parent directory.
pub fn repo_root_for_path(path: &Path) -> Result<PathBuf, String> {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    git_output_in(Some(dir), &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

fn git_output_in(project_root: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = ProcessCommand::new("git");
    cmd.args(args);
    if let Some(root) = project_root {
        cmd.current_dir(root);
    }
    let output = cmd.output().map_err(|e| format!("git error: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git error: {}", stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn parse_hunk_range(token: &str) -> Option<(usize, usize)> {
    let token = token.trim();
    let token = token
        .strip_prefix('-')
        .or_else(|| token.strip_prefix('+'))?;
    let mut parts = token.splitn(2, ',');
    let start = parts.next()?.parse::<usize>().ok()?;
    let count = match parts.next() {
        Some(value) if !value.is_empty() => value.parse::<usize>().ok()?,
        _ => 1,
    };
    Some((start, count))
}

fn build_github_file_url(editor: &Editor, branch: &str) -> Result<String, String> {
    let buf = editor.active_buffer();
    let file_path = buf
        .file_path
        .as_ref()
        .ok_or_else(|| "No file path".to_string())?;

    let repo_root = repo_root_for_path(file_path)?;
    let remote = git_output_in(Some(&repo_root), &["config", "--get", "remote.origin.url"])?;
    let base_url = remote_to_github_url(&remote)
        .ok_or_else(|| format!("Could not parse remote URL: {}", remote))?;

    let repo_root = repo_root.as_path();

    let rel_path = file_path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve file path: {}", e))?;
    let rel_path = rel_path
        .strip_prefix(repo_root)
        .map_err(|_| "File is not inside the git repository".to_string())?;

    let line = buf.cursor_line() + 1;

    Ok(format!(
        "{}/blob/{}/{}#L{}",
        base_url,
        branch,
        rel_path.display(),
        line
    ))
}

fn default_branch_for(project_root: Option<&Path>) -> Result<String, String> {
    git_output_in(project_root, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .map(|s| {
            s.strip_prefix("refs/remotes/origin/")
                .unwrap_or(&s)
                .to_string()
        })
        .or_else(|_| Ok("main".to_string()))
}

fn current_branch_for(project_root: Option<&Path>) -> Result<String, String> {
    current_branch_in(project_root)
}

fn current_branch_in(project_root: Option<&Path>) -> Result<String, String> {
    let branch = git_output_in(project_root, &["branch", "--show-current"])?;
    if branch.is_empty() {
        Err("Not on a branch (detached HEAD)".to_string())
    } else {
        Ok(branch)
    }
}

// ---------------------------------------------------------------------------
// Commit log helpers
// ---------------------------------------------------------------------------

pub fn git_log_oneline_in(
    project_root: &Path,
    skip: usize,
    count: usize,
) -> Result<String, String> {
    git_output_in(
        Some(project_root),
        &[
            "log",
            "--pretty=format:%h%x00%H%x00%an%x00%ar%x00%s",
            "-n",
            &count.to_string(),
            "--skip",
            &skip.to_string(),
        ],
    )
}

pub fn git_show_metadata_in(project_root: &Path, hash: &str) -> Result<String, String> {
    git_output_in(
        Some(project_root),
        &["show", "--format=%H%n%an%n%ae%n%ar%n%B", "-s", hash],
    )
}

pub fn git_diff_tree_in(project_root: &Path, hash: &str) -> Result<String, String> {
    git_output_in(
        Some(project_root),
        &["diff-tree", "--no-commit-id", "-r", "--name-status", hash],
    )
}

pub fn git_show_diff_in(project_root: &Path, hash: &str) -> Result<String, String> {
    git_output_in(Some(project_root), &["show", "--format=", hash])
}

pub fn register(registry: &mut CommandRegistry) {
    registry.register(CommandEntry {
        id: "core.copy_github_url_main".into(),
        label: "Copy GitHub URL (master/main)".into(),
        category: Some("Git".into()),
        action: Box::new(|ctx| {
            let file_path = match &ctx.editor().active_buffer().file_path {
                Some(p) => p.clone(),
                None => return CommandEffect::Message("No file path (scratch buffer)".into()),
            };
            let repo_root = repo_root_for_path(&file_path).ok();
            let branch = match default_branch_for(repo_root.as_deref()) {
                Ok(b) => b,
                Err(e) => return CommandEffect::Message(e),
            };
            let url = match build_github_file_url(ctx.editor(), &branch) {
                Ok(u) => u,
                Err(e) => return CommandEffect::Message(e),
            };
            match copy_to_clipboard(&url) {
                Ok(()) => CommandEffect::Message(format!("Copied: {}", url)),
                Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "core.copy_github_url_branch".into(),
        label: "Copy GitHub URL (current branch)".into(),
        category: Some("Git".into()),
        action: Box::new(|ctx| {
            let file_path = match &ctx.editor().active_buffer().file_path {
                Some(p) => p.clone(),
                None => return CommandEffect::Message("No file path (scratch buffer)".into()),
            };
            let repo_root = repo_root_for_path(&file_path).ok();
            let branch = match current_branch_for(repo_root.as_deref()) {
                Ok(b) => b,
                Err(e) => return CommandEffect::Message(e),
            };
            let url = match build_github_file_url(ctx.editor(), &branch) {
                Ok(u) => u,
                Err(e) => return CommandEffect::Message(e),
            };
            match copy_to_clipboard(&url) {
                Ok(()) => CommandEffect::Message(format!("Copied: {}", url)),
                Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "core.open_in_github_main".into(),
        label: "Open in GitHub (master/main)".into(),
        category: Some("Git".into()),
        action: Box::new(|ctx| {
            let file_path = match &ctx.editor().active_buffer().file_path {
                Some(p) => p.clone(),
                None => return CommandEffect::Message("No file path (scratch buffer)".into()),
            };
            let repo_root = repo_root_for_path(&file_path).ok();
            let branch = match default_branch_for(repo_root.as_deref()) {
                Ok(b) => b,
                Err(e) => return CommandEffect::Message(e),
            };
            let url = match build_github_file_url(ctx.editor(), &branch) {
                Ok(u) => u,
                Err(e) => return CommandEffect::Message(e),
            };
            match open_url(&url) {
                Ok(()) => CommandEffect::Message(format!("Opened: {}", url)),
                Err(e) => CommandEffect::Message(format!("Open failed: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "core.open_in_github_branch".into(),
        label: "Open in GitHub (current branch)".into(),
        category: Some("Git".into()),
        action: Box::new(|ctx| {
            let file_path = match &ctx.editor().active_buffer().file_path {
                Some(p) => p.clone(),
                None => return CommandEffect::Message("No file path (scratch buffer)".into()),
            };
            let repo_root = repo_root_for_path(&file_path).ok();
            let branch = match current_branch_for(repo_root.as_deref()) {
                Ok(b) => b,
                Err(e) => return CommandEffect::Message(e),
            };
            let url = match build_github_file_url(ctx.editor(), &branch) {
                Ok(u) => u,
                Err(e) => return CommandEffect::Message(e),
            };
            match open_url(&url) {
                Ok(()) => CommandEffect::Message(format!("Opened: {}", url)),
                Err(e) => CommandEffect::Message(format!("Open failed: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "git.pr_list".into(),
        label: "GitHub PR List".into(),
        category: Some("Git".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenPrList,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "git.issue_list".into(),
        label: "GitHub Issue List".into(),
        category: Some("Git".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenIssueList,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "git.commit_log".into(),
        label: "Git: Commit Log".into(),
        category: Some("Git".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenCommitLog,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "git.switch_branch".into(),
        label: "Git: Switch Branch".into(),
        category: Some("Git".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenGitBranchPicker,
            )))
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    }

    fn setup_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        temp
    }

    #[test]
    fn test_ssh_remote() {
        assert_eq!(
            remote_to_github_url("git@github.com:user/repo.git"),
            Some("https://github.com/user/repo".into())
        );
    }

    #[test]
    fn test_https_remote() {
        assert_eq!(
            remote_to_github_url("https://github.com/user/repo.git"),
            Some("https://github.com/user/repo".into())
        );
    }

    #[test]
    fn test_https_remote_no_git_suffix() {
        assert_eq!(
            remote_to_github_url("https://github.com/user/repo"),
            Some("https://github.com/user/repo".into())
        );
    }

    #[test]
    fn test_non_github_remote() {
        assert_eq!(remote_to_github_url("git@gitlab.com:user/repo.git"), None);
    }

    #[test]
    fn test_remote_with_whitespace() {
        assert_eq!(
            remote_to_github_url("  git@github.com:user/repo.git\n"),
            Some("https://github.com/user/repo".into())
        );
    }

    #[test]
    fn test_parse_diff_hunks_additions() {
        let diff = "@@ -0,0 +1,2 @@\n+foo\n+bar\n";
        let map = parse_diff_hunks(diff);
        assert_eq!(map.get(&0), Some(&GitLineStatus::Added));
        assert_eq!(map.get(&1), Some(&GitLineStatus::Added));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_parse_diff_hunks_deletions() {
        let diff = "@@ -3,2 +3,0 @@\n-foo\n-bar\n";
        let map = parse_diff_hunks(diff);
        assert_eq!(map.get(&2), Some(&GitLineStatus::Deleted));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_parse_diff_hunks_modified_with_implicit_counts() {
        let diff = "@@ -4 +4 @@\n-foo\n+bar\n";
        let map = parse_diff_hunks(diff);
        assert_eq!(map.get(&3), Some(&GitLineStatus::Modified));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn git_stage_many_in_continues_after_failure() {
        let repo = setup_repo();
        fs::write(repo.path().join("a.txt"), "a\n").expect("write a");
        fs::write(repo.path().join("b.txt"), "b\n").expect("write b");
        let paths = vec![
            "a.txt".to_string(),
            "missing.txt".to_string(),
            "b.txt".to_string(),
        ];

        let result = git_stage_many_in(repo.path(), &paths);

        assert_eq!(result.successes, 2);
        assert_eq!(result.total(), 3);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].0, "missing.txt");

        let staged = git_output_in(Some(repo.path()), &["diff", "--cached", "--name-only"])
            .expect("read staged files");
        assert!(staged.lines().any(|line| line == "a.txt"));
        assert!(staged.lines().any(|line| line == "b.txt"));
    }

    #[test]
    fn git_unstage_many_in_continues_after_failure() {
        let repo = setup_repo();
        run_git(repo.path(), &["commit", "--allow-empty", "-m", "init"]);
        fs::write(repo.path().join("a.txt"), "a\n").expect("write a");
        fs::write(repo.path().join("b.txt"), "b\n").expect("write b");
        run_git(repo.path(), &["add", "a.txt", "b.txt"]);
        let paths = vec![
            "a.txt".to_string(),
            "missing.txt".to_string(),
            "b.txt".to_string(),
        ];

        let result = git_unstage_many_in(repo.path(), &paths);

        assert_eq!(result.successes, 2);
        assert_eq!(result.total(), 3);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].0, "missing.txt");

        let staged = git_output_in(Some(repo.path()), &["diff", "--cached", "--name-only"])
            .expect("read staged files");
        assert!(
            staged.trim().is_empty(),
            "expected no staged files after bulk unstage, got: {staged}"
        );
    }
}

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Stdio;

use crossterm::style::Color;

use crate::core::editor::Editor;
use crate::input::action::{Action, AppAction, ProjectAction, WorkspaceAction};

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
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("git error: {}", e))?,
    };
    git_backend::status_files(&root).ok_or_else(|| "git error: failed to read status".to_string())
}

fn diff_file_entries(diff: &str) -> Vec<GitFileEntry> {
    crate::diff_render::parse_unified_diff(diff)
        .into_iter()
        .map(|file| GitFileEntry {
            path: file.path,
            status_char: file.status.as_str().chars().next().unwrap_or('M'),
            staged: false,
            additions: file.additions,
            deletions: file.deletions,
        })
        .collect()
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
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("git error: {}", e))?,
    };
    git_backend::file_diff_text(&root, path, staged)
        .ok_or_else(|| "git error: failed to read diff".to_string())
}

/// List of files that differ between `base_branch` and HEAD, parsed
/// from `git diff --name-status <base>...HEAD`.
pub fn git_branch_diff_files_in(
    project_root: &Path,
    base_branch: &str,
) -> Result<Vec<GitFileEntry>, String> {
    let diff = git_backend::compare_diff_text(project_root, base_branch, "HEAD", None)
        .ok_or_else(|| "git error: failed to read branch diff".to_string())?;
    Ok(diff_file_entries(&diff))
}

pub fn git_local_branches_in(project_root: &Path) -> Result<Vec<(String, bool)>, String> {
    git_backend::list_local_branches(project_root)
        .ok_or_else(|| "git error: failed to list branches".to_string())
}

pub fn git_branch_preview_in(project_root: &Path, branch: &str) -> Result<Vec<String>, String> {
    let mut lines = vec![
        format!("Branch: {}", branch),
        String::new(),
        "Working tree status:".to_string(),
    ];
    match git_backend::status_files(project_root) {
        Some((changed, staged)) if changed.is_empty() && staged.is_empty() => {
            lines.push("(clean)".to_string());
        }
        Some((changed, staged)) => {
            if let Some(current) = git_backend::current_branch(project_root) {
                lines.push(format!("## {current}"));
            }
            for entry in staged.iter().chain(changed.iter()) {
                let index = if entry.staged { entry.status_char } else { ' ' };
                let worktree = if entry.staged { ' ' } else { entry.status_char };
                lines.push(format!("{index}{worktree} {}", entry.path));
            }
        }
        None => lines.push("(status unavailable: git error: failed to read status)".to_string()),
    }

    lines.push(String::new());
    lines.push(format!("Recent commits on {}:", branch));
    match git_backend::commit_log_for_rev(project_root, branch, 0, 15) {
        Some(rows) if rows.is_empty() => lines.push("(no commits)".to_string()),
        Some(rows) => {
            lines.extend(
                rows.into_iter()
                    .map(|row| format!("{} {}", row.hash, row.message)),
            );
        }
        None => {
            lines.push("(log unavailable: git error: failed to read commit log)".to_string());
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
    git_backend::has_staged_changes(project_root)
        .ok_or_else(|| "git error: failed to check staged changes".to_string())
}

pub fn git_commit_editmsg_path_in(project_root: &Path) -> Result<PathBuf, String> {
    git_backend::git_path(project_root, "COMMIT_EDITMSG")
        .ok_or_else(|| "git error: failed to resolve COMMIT_EDITMSG path".to_string())
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
        ProcessCommand::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
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
    git_backend::repo_root(dir).ok_or_else(|| "git error: failed to resolve repo root".to_string())
}

fn git_output_in(project_root: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = ProcessCommand::new("git");
    // Disable C-style octal quoting of non-ASCII paths in git output.
    cmd.args(["-c", "core.quotepath=off"]);
    // Avoid taking .git/index.lock for index refresh during status/diff so
    // background reads don't race with a user-initiated `git commit`.
    cmd.args(["-c", "core.optionalLocks=false"]);
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

/// Run `git push` in `project_root` (or the current directory when `None`),
/// returning a one-line status message. git writes its progress and the
/// "Everything up-to-date" notice to stderr, so both streams are combined.
///
/// This blocks on the network, so callers should run it off the UI thread.
pub fn git_push_in(project_root: Option<&Path>) -> Result<String, String> {
    let mut cmd = ProcessCommand::new("git");
    cmd.arg("push");
    if let Some(root) = project_root {
        cmd.current_dir(root);
    }
    let output = cmd.output().map_err(|e| format!("git error: {}", e))?;
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let summary = combined
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("");
    if output.status.success() {
        if summary.is_empty() {
            Ok("git push: done".to_string())
        } else {
            Ok(format!("git push: {}", summary))
        }
    } else if summary.is_empty() {
        Err("git push failed".to_string())
    } else {
        Err(format!("git push failed: {}", summary))
    }
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
    let remote = git_backend::remote_origin_url(&repo_root)
        .ok_or_else(|| "git error: remote.origin.url is not set".to_string())?;
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
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("git error: {}", e))?,
    };
    Ok(git_backend::origin_head_short(&root)
        .and_then(|s| s.strip_prefix("origin/").map(|name| name.to_string()))
        .unwrap_or_else(|| "main".to_string()))
}

fn current_branch_for(project_root: Option<&Path>) -> Result<String, String> {
    current_branch_in(project_root)
}

fn current_branch_in(project_root: Option<&Path>) -> Result<String, String> {
    let root = match project_root {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir().map_err(|e| format!("git error: {}", e))?,
    };
    git_backend::current_branch(&root).ok_or_else(|| "Not on a branch (detached HEAD)".to_string())
}

// ---------------------------------------------------------------------------
// Commit log helpers
// ---------------------------------------------------------------------------

pub fn git_log_oneline_in(
    project_root: &Path,
    skip: usize,
    count: usize,
) -> Result<String, String> {
    let rows = git_backend::commit_log(project_root, skip, count)
        .ok_or_else(|| "git error: failed to read commit log".to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| {
            format!(
                "{}\0{}\0{}\0{}\0{}",
                row.hash, row.full_hash, row.author, row.date, row.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn git_show_metadata_in(project_root: &Path, hash: &str) -> Result<String, String> {
    let meta = git_backend::commit_meta(project_root, hash)
        .ok_or_else(|| "git error: failed to read commit metadata".to_string())?;
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}",
        meta.full_hash, meta.author, meta.author_email, meta.date, meta.message
    ))
}

pub fn git_diff_tree_in(project_root: &Path, hash: &str) -> Result<String, String> {
    let diff = git_backend::commit_diff_text(project_root, hash, None)
        .ok_or_else(|| "git error: failed to read commit diff".to_string())?;
    Ok(crate::diff_render::parse_unified_diff(&diff)
        .into_iter()
        .map(|file| {
            format!(
                "{}\t{}",
                file.status.as_str().chars().next().unwrap_or('M'),
                file.path
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn git_show_diff_in(project_root: &Path, hash: &str) -> Result<String, String> {
    git_backend::commit_diff_text(project_root, hash, None)
        .ok_or_else(|| "git error: failed to read commit diff".to_string())
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
        id: "git.push".into(),
        label: "Git Push".into(),
        category: Some("Git".into()),
        // Runs `git push` in the background so the network round-trip never
        // blocks the editor; the result is reported in the status bar.
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Project(ProjectAction::GitPush)))
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

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&output.stdout).into_owned()
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
    fn git_status_files_matches_porcelain_oracle() {
        let repo = setup_repo();
        fs::write(repo.path().join("tracked.txt"), "one\n").expect("write tracked");
        fs::write(repo.path().join("staged.txt"), "old\n").expect("write staged");
        run_git(repo.path(), &["add", "tracked.txt", "staged.txt"]);
        run_git(repo.path(), &["commit", "-m", "init"]);

        fs::write(repo.path().join("tracked.txt"), "one\ntwo\n").expect("modify tracked");
        fs::write(repo.path().join("staged.txt"), "new\n").expect("modify staged");
        run_git(repo.path(), &["add", "staged.txt"]);
        fs::write(repo.path().join("new.txt"), "new\n").expect("write new");

        let (changed, staged) = git_status_files_in(repo.path()).expect("gix status");
        let changed_facts: Vec<(String, char)> = changed
            .into_iter()
            .map(|entry| (entry.path, entry.status_char))
            .collect();
        let staged_facts: Vec<(String, char)> = staged
            .into_iter()
            .map(|entry| (entry.path, entry.status_char))
            .collect();

        let raw = git_stdout(repo.path(), &["status", "--porcelain"]);
        let mut expected_changed = Vec::new();
        let mut expected_staged = Vec::new();
        for line in raw.lines() {
            let bytes = line.as_bytes();
            let index_status = bytes[0] as char;
            let worktree_status = bytes[1] as char;
            let path = line[3..].to_string();
            if index_status != ' ' && index_status != '?' {
                expected_staged.push((path.clone(), index_status));
            }
            if worktree_status != ' ' {
                expected_changed.push((path, worktree_status));
            }
        }
        expected_changed.sort();
        expected_staged.sort();

        assert_eq!(changed_facts, expected_changed);
        assert_eq!(staged_facts, expected_staged);
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

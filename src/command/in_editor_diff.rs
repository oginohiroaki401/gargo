use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::input::action::{Action, AppAction, WorkspaceAction};

pub const IN_EDITOR_DIFF_TITLE: &str = "IN-EDITOR DIFF VIEW";
pub const BRANCH_COMPARE_DIFF_TITLE: &str = "BRANCH COMPARE DIFF";
pub const COMMIT_DIFF_TITLE: &str = "COMMIT DIFF";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffJumpTarget {
    pub path: PathBuf,
    pub line: usize,
    pub char_col: usize,
}

#[derive(Debug, Clone, Default)]
pub struct InEditorDiffView {
    pub text: String,
    pub line_targets: HashMap<usize, DiffJumpTarget>,
}

#[derive(Debug, Default)]
struct DiffParseState {
    current_rel_path: Option<String>,
    fallback_rel_path: Option<String>,
    new_line: Option<usize>,
}

impl DiffParseState {
    fn consume_line(&mut self, line: &str, project_root: &Path) -> Option<DiffJumpTarget> {
        if let Some((_, new_path)) = parse_diff_git_paths(line) {
            self.fallback_rel_path = parse_diff_path_token(new_path);
            return None;
        }

        if let Some(path_token) = line.strip_prefix("+++ ") {
            self.current_rel_path = parse_diff_path_token(path_token)
                .or_else(|| self.fallback_rel_path.clone())
                .filter(|p| !p.is_empty());
            return None;
        }

        if line.starts_with("@@") {
            self.new_line = parse_hunk_new_start(line).map(|start| start.saturating_sub(1));
            return None;
        }

        if line.starts_with("--- ") || line.starts_with("index ") || line.starts_with("new file ") {
            return None;
        }

        let rel_path = self.current_rel_path.as_deref()?;
        let current_line = self.new_line.unwrap_or(0);
        let path = project_root.join(rel_path);

        if line.starts_with(' ') {
            self.new_line = Some(current_line.saturating_add(1));
            return Some(DiffJumpTarget {
                path,
                line: current_line,
                char_col: 0,
            });
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            self.new_line = Some(current_line.saturating_add(1));
            return Some(DiffJumpTarget {
                path,
                line: current_line,
                char_col: 0,
            });
        }
        if line.starts_with('-') && !line.starts_with("---") {
            return Some(DiffJumpTarget {
                path,
                line: current_line,
                char_col: 0,
            });
        }

        None
    }
}

pub fn register(registry: &mut CommandRegistry) {
    registry.register(CommandEntry {
        id: "diff.open_in_editor".into(),
        label: "Open Diff View (In Editor)".into(),
        category: Some("Diff".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenInEditorDiffView,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "diff.compare_branch".into(),
        label: "Compare Branch Diff".into(),
        category: Some("Diff".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenBranchComparePicker,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "diff.refresh_in_editor".into(),
        label: "Refresh Diff View (In Editor)".into(),
        category: Some("Diff".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::RefreshInEditorDiffView,
            )))
        }),
    });
}

pub fn build_in_editor_diff_view(project_root: &Path) -> Result<InEditorDiffView, String> {
    let unstaged_diff = git_output_in_repo(project_root, &["diff"])?;
    let staged_diff = git_output_in_repo(project_root, &["diff", "--cached"])?;
    let untracked_files = git_output_in_repo(
        project_root,
        &["ls-files", "--others", "--exclude-standard"],
    )?
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect::<Vec<_>>();

    let mut untracked_patches = Vec::with_capacity(untracked_files.len());
    for path in &untracked_files {
        let patch = git_output_in_repo_allow_codes(
            project_root,
            &["diff", "--no-index", "--", "/dev/null", path],
            &[0, 1],
        )?;
        if !patch.trim().is_empty() {
            untracked_patches.push(patch);
        }
    }
    let untracked_diff = untracked_patches.join("\n");

    let mut lines = Vec::new();
    let mut line_targets = HashMap::new();

    lines.push(IN_EDITOR_DIFF_TITLE.to_string());
    lines.push(format!("Project: {}", project_root.display()));
    lines.push(format!(
        "Changed files: {}",
        count_diff_files(&unstaged_diff)
    ));
    lines.push(format!("Staged files: {}", count_diff_files(&staged_diff)));
    lines.push(format!("Untracked files: {}", untracked_files.len()));
    lines.push("gd on a diff line opens that file location.".to_string());
    lines.push("Use command diff.refresh_in_editor or key r to refresh.".to_string());
    lines.push(String::new());

    append_patch_section(
        &mut lines,
        &mut line_targets,
        "Changed (unstaged)",
        &unstaged_diff,
        project_root,
    );
    append_patch_section(
        &mut lines,
        &mut line_targets,
        "Staged",
        &staged_diff,
        project_root,
    );
    append_patch_section(
        &mut lines,
        &mut line_targets,
        "Untracked",
        &untracked_diff,
        project_root,
    );

    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }

    Ok(InEditorDiffView { text, line_targets })
}

pub fn build_branch_compare_diff_view(
    project_root: &Path,
    other_branch: &str,
) -> Result<InEditorDiffView, String> {
    let current_branch = git_output_in_repo(project_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let current_branch = current_branch.trim();

    let diff = git_output_in_repo_allow_codes(
        project_root,
        &["diff", &format!("{}...HEAD", other_branch)],
        &[0, 1],
    )?;

    let file_count = count_diff_files(&diff);

    let mut lines = Vec::new();
    let mut line_targets = HashMap::new();

    lines.push(BRANCH_COMPARE_DIFF_TITLE.to_string());
    lines.push(format!("Comparing: {} → {}", other_branch, current_branch));
    lines.push(format!("Changed files: {}", file_count));
    lines.push("gd on a diff line opens that file location.".to_string());
    lines.push(String::new());

    if diff.trim().is_empty() {
        lines.push("(no differences)".to_string());
    } else {
        let mut parser = DiffParseState::default();
        for raw_line in diff.lines() {
            let line_idx = lines.len();
            lines.push(raw_line.to_string());
            if let Some(target) = parser.consume_line(raw_line, project_root) {
                line_targets.insert(line_idx, target);
            }
        }
    }

    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }

    Ok(InEditorDiffView { text, line_targets })
}

pub fn build_commit_diff_view(project_root: &Path, hash: &str) -> Result<InEditorDiffView, String> {
    use crate::command::git;

    let meta_raw = git::git_show_metadata_in(project_root, hash)?;
    let meta_lines: Vec<&str> = meta_raw.splitn(5, '\n').collect();
    let full_hash = meta_lines.first().unwrap_or(&"");
    let author = meta_lines.get(1).unwrap_or(&"");
    let author_email = meta_lines.get(2).unwrap_or(&"");
    let date = meta_lines.get(3).unwrap_or(&"");
    let message = meta_lines.get(4).unwrap_or(&"");

    let diff_raw = git::git_show_diff_in(project_root, hash)?;

    let mut lines = Vec::new();
    let mut line_targets = HashMap::new();

    lines.push(COMMIT_DIFF_TITLE.to_string());
    lines.push(format!("Commit: {}", full_hash));
    lines.push(format!("Author: {} <{}>", author, author_email));
    lines.push(format!("Date:   {}", date));
    lines.push(String::new());
    for msg_line in message.lines() {
        lines.push(format!("    {}", msg_line));
    }
    lines.push(String::new());
    lines.push("gd on a diff line opens that file location.".to_string());
    lines.push(String::new());

    if diff_raw.trim().is_empty() {
        lines.push("(no diff)".to_string());
    } else {
        let mut parser = DiffParseState::default();
        for raw_line in diff_raw.lines() {
            let line_idx = lines.len();
            lines.push(raw_line.to_string());
            if let Some(target) = parser.consume_line(raw_line, project_root) {
                line_targets.insert(line_idx, target);
            }
        }
    }

    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }

    Ok(InEditorDiffView { text, line_targets })
}

fn append_patch_section(
    lines: &mut Vec<String>,
    line_targets: &mut HashMap<usize, DiffJumpTarget>,
    title: &str,
    patch: &str,
    project_root: &Path,
) {
    lines.push(format!("## {}", title));
    if patch.trim().is_empty() {
        lines.push("(no changes)".to_string());
        lines.push(String::new());
        return;
    }

    let mut parser = DiffParseState::default();
    for raw_line in patch.lines() {
        let line_idx = lines.len();
        lines.push(raw_line.to_string());
        if let Some(target) = parser.consume_line(raw_line, project_root) {
            line_targets.insert(line_idx, target);
        }
    }
    lines.push(String::new());
}

fn count_diff_files(diff: &str) -> usize {
    diff.lines()
        .filter(|line| line.starts_with("diff --git "))
        .count()
}

fn parse_diff_git_paths(line: &str) -> Option<(&str, &str)> {
    if !line.starts_with("diff --git ") {
        return None;
    }
    let mut parts = line.split_whitespace();
    let _diff = parts.next()?;
    let _git = parts.next()?;
    let old_path = parts.next()?;
    let new_path = parts.next()?;
    Some((old_path, new_path))
}

fn parse_diff_path_token(token: &str) -> Option<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() || trimmed == "/dev/null" {
        return None;
    }
    let unquoted = trimmed.trim_matches('"');
    let normalized = unquoted
        .strip_prefix("a/")
        .or_else(|| unquoted.strip_prefix("b/"))
        .unwrap_or(unquoted);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized.to_string())
    }
}

fn parse_hunk_new_start(line: &str) -> Option<usize> {
    let plus_idx = line.find('+')?;
    let after_plus = &line[plus_idx + 1..];
    let digits: String = after_plus
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<usize>().ok()
}

fn git_output_in_repo(project_root: &Path, args: &[&str]) -> Result<String, String> {
    git_output_in_repo_allow_codes(project_root, args, &[0])
}

fn git_output_in_repo_allow_codes(
    project_root: &Path,
    args: &[&str],
    allowed_codes: &[i32],
) -> Result<String, String> {
    let output = ProcessCommand::new("git")
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .current_dir(project_root)
        .output()
        .map_err(|e| format!("git command failed ({}): {}", args.join(" "), e))?;

    let status_code = output.status.code().unwrap_or(-1);
    if !allowed_codes.contains(&status_code) {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "git {} failed (code {}): {}",
            args.join(" "),
            status_code,
            stderr
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_patch_section_maps_changed_added_and_removed_lines() {
        let mut lines = Vec::new();
        let mut targets = HashMap::new();
        let root = Path::new("/tmp/repo");
        let patch = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 line1
-line2
+line2_mod
+line3";

        append_patch_section(&mut lines, &mut targets, "Changed", patch, root);

        let removed_idx = lines
            .iter()
            .position(|line| line == "-line2")
            .expect("removed line should exist");
        let added_idx = lines
            .iter()
            .position(|line| line == "+line2_mod")
            .expect("added line should exist");
        let added2_idx = lines
            .iter()
            .position(|line| line == "+line3")
            .expect("second added line should exist");

        assert_eq!(
            targets.get(&removed_idx),
            Some(&DiffJumpTarget {
                path: root.join("src/main.rs"),
                line: 1,
                char_col: 0
            })
        );
        assert_eq!(
            targets.get(&added_idx),
            Some(&DiffJumpTarget {
                path: root.join("src/main.rs"),
                line: 1,
                char_col: 0
            })
        );
        assert_eq!(
            targets.get(&added2_idx),
            Some(&DiffJumpTarget {
                path: root.join("src/main.rs"),
                line: 2,
                char_col: 0
            })
        );
    }

    #[test]
    fn append_patch_section_handles_deleted_file_with_dev_null_header() {
        let mut lines = Vec::new();
        let mut targets = HashMap::new();
        let root = Path::new("/tmp/repo");
        let patch = "\
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-gone";

        append_patch_section(&mut lines, &mut targets, "Changed", patch, root);

        let removed_idx = lines
            .iter()
            .position(|line| line == "-gone")
            .expect("removed line should exist");
        assert_eq!(
            targets.get(&removed_idx),
            Some(&DiffJumpTarget {
                path: root.join("old.txt"),
                line: 0,
                char_col: 0
            })
        );
    }

    #[test]
    fn parse_hunk_new_start_parses_basic_and_zero_ranges() {
        assert_eq!(parse_hunk_new_start("@@ -10,3 +25,4 @@"), Some(25));
        assert_eq!(parse_hunk_new_start("@@ -1 +0,0 @@"), Some(0));
        assert_eq!(parse_hunk_new_start("not a hunk"), None);
    }
}

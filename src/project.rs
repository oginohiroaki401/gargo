use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve a project root by probing `start` first, then falling back to CWD.
/// Uses the first ancestor directory containing a `.git` marker (dir or file).
/// If `start` is provided and no git root is found from that path, returns the
/// provided directory (or file parent). If `start` is absent, falls back to CWD.
pub fn find_project_root(start: Option<&Path>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    find_project_root_with_cwd(start, &cwd)
}

fn find_project_root_with_cwd(start: Option<&Path>, cwd: &Path) -> PathBuf {
    if let Some(start_path) = start {
        let candidate = if start_path.is_absolute() {
            start_path.to_path_buf()
        } else {
            cwd.join(start_path)
        };
        let candidate = std::fs::canonicalize(&candidate).unwrap_or(candidate);
        let probe_dir = if candidate.is_dir() {
            candidate.clone()
        } else {
            candidate
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(candidate.clone())
        };

        if let Some(root) = find_git_root_from(&probe_dir) {
            return root;
        }

        return probe_dir;
    }

    if let Some(root) = find_git_root_from(cwd) {
        return root;
    }

    cwd.to_path_buf()
}

fn find_git_root_from(start_dir: &Path) -> Option<PathBuf> {
    let mut dir = start_dir.to_path_buf();
    loop {
        if has_git_marker(&dir) {
            return Some(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Collect files under `root`.
/// If inside a git repo, uses `git ls-files` to respect `.gitignore`.
/// Otherwise falls back to a recursive directory walk.
pub fn collect_files(root: &Path) -> Vec<String> {
    if has_git_marker(root)
        && let Some(files) = collect_files_git(root)
    {
        return files;
    }
    collect_files_walk(root, root)
}

/// Discover git repos accessible from `project_root`.
/// If `project_root` itself is a git repo, returns `vec![project_root]`.
/// Otherwise scans one level of subdirectories for git repos.
pub fn discover_sub_repos(project_root: &Path) -> Vec<PathBuf> {
    if has_git_marker(project_root) {
        return vec![project_root.to_path_buf()];
    }
    let entries = match std::fs::read_dir(project_root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut repos: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() && has_git_marker(&path) {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    repos.sort();
    repos
}

fn has_git_marker(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    dot_git.is_dir() || dot_git.is_file()
}

fn collect_files_git(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.quotepath=off",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // `--cached` lists index entries even when their working-tree copy is gone
    // (e.g. a tracked file deleted with a plain `rm` instead of `git rm`, which
    // is what the web editor's Delete does). Without this, such a file — and any
    // directory that only held it — lingers in the sidebar as a phantom after a
    // restart. Drop anything git reports as deleted from the working tree.
    let deleted = git_deleted_set(root);
    let files: Vec<String> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| !deleted.contains(*l))
        .map(|l| l.to_string())
        .collect();
    Some(files)
}

/// Paths that git tracks but whose working-tree copy is missing (`git ls-files
/// --deleted`). Empty on any error so the caller simply keeps the full list.
fn git_deleted_set(root: &Path) -> std::collections::HashSet<String> {
    Command::new("git")
        .args(["-c", "core.quotepath=off", "ls-files", "--deleted"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn collect_files_walk(dir: &Path, root: &Path) -> Vec<String> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
            continue;
        }

        if path.is_dir() {
            if has_git_marker(&path) {
                if let Some(git_files) = collect_files_git(&path)
                    && let Ok(prefix) = path.strip_prefix(root)
                {
                    let prefix_str = prefix.to_string_lossy();
                    for f in git_files {
                        result.push(format!("{prefix_str}/{f}"));
                    }
                }
            } else {
                result.extend(collect_files_walk(&path, root));
            }
        } else if let Ok(rel) = path.strip_prefix(root) {
            result.push(rel.to_string_lossy().to_string());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_git_repo(path: &Path) {
        let output = Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    /// Canonicalize the tempdir path. On macOS `tempdir()` returns `/var/...`
    /// but the project-root resolver canonicalizes to `/private/var/...`, so
    /// assertions built from the raw tempdir path would never match.
    fn canonical(p: &Path) -> PathBuf {
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn find_project_root_finds_git_from_nested_path() {
        let tmp = tempdir().unwrap();
        let tmp_root = canonical(tmp.path());
        let repo = tmp_root.join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        init_git_repo(&repo);

        let root = find_project_root(Some(&nested));
        assert_eq!(root, repo);
    }

    #[test]
    fn find_project_root_with_non_git_arg_returns_explicit_directory() {
        let tmp = tempdir().unwrap();
        let tmp_root = canonical(tmp.path());
        let repo = tmp_root.join("cwd-repo");
        let cwd = repo.join("subdir");
        std::fs::create_dir_all(&cwd).unwrap();
        init_git_repo(&repo);

        let non_git_arg = tmp_root.join("outside").join("a").join("b");
        std::fs::create_dir_all(&non_git_arg).unwrap();

        let root_with_arg = find_project_root_with_cwd(Some(&non_git_arg), &cwd);
        let root_without_arg = find_project_root_with_cwd(None, &cwd);
        assert_eq!(root_with_arg, non_git_arg);
        assert_eq!(root_without_arg, repo);
    }

    #[test]
    fn find_project_root_finds_git_from_file_path_parent_chain() {
        let tmp = tempdir().unwrap();
        let tmp_root = canonical(tmp.path());
        let repo = tmp_root.join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        init_git_repo(&repo);

        let file = nested.join("note.txt");
        std::fs::write(&file, "hello").unwrap();

        let root = find_project_root(Some(&file));
        assert_eq!(root, repo);
    }

    #[test]
    fn find_project_root_returns_explicit_file_parent_when_no_git_in_arg_chain() {
        let tmp = tempdir().unwrap();
        let tmp_root = canonical(tmp.path());
        let repo = tmp_root.join("cwd-repo");
        let cwd = repo.join("subdir");
        let outside = tmp_root.join("outside").join("a");
        let file = outside.join("note.txt");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(&file, "hello").unwrap();
        init_git_repo(&repo);

        let root = find_project_root_with_cwd(Some(&file), &cwd);
        assert_eq!(root, outside);
    }

    #[test]
    fn find_project_root_resolves_relative_path_before_git_search() {
        let tmp = tempdir().unwrap();
        let tmp_root = canonical(tmp.path());
        let workspace = tmp_root.join("workspace");
        let repo_a = workspace.join("repo-a");
        let repo_b = workspace.join("repo-b");
        let cwd = repo_a.join("nested");
        let target = repo_b.join("child");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        init_git_repo(&repo_a);
        init_git_repo(&repo_b);

        let root = find_project_root_with_cwd(Some(Path::new("../../repo-b/child")), &cwd);
        assert_eq!(root, repo_b);
    }

    #[test]
    fn find_project_root_returns_cwd_when_no_git_in_cwd_chain() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path().join("cwd").join("subdir");
        std::fs::create_dir_all(&cwd).unwrap();

        let root_no_arg = find_project_root_with_cwd(None, &cwd);
        assert_eq!(root_no_arg, cwd);
    }

    #[test]
    fn collect_files_uses_git_and_respects_gitignore() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        std::fs::write(repo.join("tracked.txt"), "tracked").unwrap();
        let output = Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());

        std::fs::write(repo.join("untracked.txt"), "untracked").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "ignored").unwrap();

        let files = collect_files(&repo);
        assert!(files.contains(&"tracked.txt".to_string()));
        assert!(files.contains(&"untracked.txt".to_string()));
        assert!(!files.contains(&"ignored.txt".to_string()));
    }

    #[test]
    fn collect_files_excludes_tracked_files_deleted_from_working_tree() {
        // Regression: a tracked file removed from disk with a plain `rm` (as the
        // web editor's Delete does) stays in git's index, so `git ls-files
        // --cached` still reports it — and the sidebar would show it (and its
        // now-empty parent dir) as a phantom after a restart.
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        std::fs::create_dir_all(repo.join("foo")).unwrap();
        std::fs::write(repo.join("foo/bar.md"), "content").unwrap();
        std::fs::write(repo.join("keep.txt"), "keep").unwrap();
        let output = Command::new("git")
            .args(["add", "--all"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());

        // Delete the working-tree copy without touching the index.
        std::fs::remove_dir_all(repo.join("foo")).unwrap();

        let files = collect_files(&repo);
        assert!(
            !files.contains(&"foo/bar.md".to_string()),
            "deleted file should not appear: {files:?}"
        );
        assert!(files.contains(&"keep.txt".to_string()));
    }

    #[test]
    fn collect_files_returns_japanese_filenames_unquoted() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        // Filename with Japanese chars; without `core.quotepath=off` git would
        // emit them as `"\346\227\245..."` C-style octal escapes.
        let jp_name = "日本語.txt";
        std::fs::write(repo.join(jp_name), "hello").unwrap();
        let output = Command::new("git")
            .args(["add", "--all"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());

        let files = collect_files(&repo);
        assert!(
            files.contains(&jp_name.to_string()),
            "expected {:?} in {:?}",
            jp_name,
            files
        );
    }

    #[test]
    fn collect_files_walk_fallback_collects_files() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join("dir")).unwrap();
        std::fs::write(tmp.path().join("dir").join("b.txt"), "y").unwrap();

        let files = collect_files(tmp.path());
        assert!(files.contains(&"a.txt".to_string()));
        assert!(files.contains(&"dir/b.txt".to_string()));
    }

    #[test]
    fn collect_files_walk_delegates_to_git_for_nested_repos() {
        // Create a tempdir with NO .git at root (so collect_files uses walk)
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        // Create a plain file at root level
        std::fs::write(root.join("root.txt"), "root").unwrap();

        // Create a nested git repo
        let nested = root.join("nested_repo");
        std::fs::create_dir_all(&nested).unwrap();
        init_git_repo(&nested);

        // Add a tracked file
        std::fs::write(nested.join("tracked.txt"), "tracked").unwrap();
        let output = Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(&nested)
            .output()
            .unwrap();
        assert!(output.status.success());

        // Add an untracked (but not ignored) file
        std::fs::write(nested.join("untracked.txt"), "untracked").unwrap();

        // Add a gitignored file
        std::fs::write(nested.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(nested.join("ignored.txt"), "ignored").unwrap();

        let files = collect_files(root);

        // Root-level file should be present
        assert!(files.contains(&"root.txt".to_string()));

        // Nested repo files should be prefixed with the subdirectory name
        assert!(files.contains(&"nested_repo/tracked.txt".to_string()));
        assert!(files.contains(&"nested_repo/untracked.txt".to_string()));

        // Gitignored file inside nested repo should be excluded
        assert!(!files.contains(&"nested_repo/ignored.txt".to_string()));
    }
}

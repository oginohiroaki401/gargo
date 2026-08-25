use std::path::{Path, PathBuf};

use crate::config::Config;

/// Decides which entries the file tree and the file pickers may show.
///
/// One predicate shared by the sidebar, the tree popup, the non-git walk and
/// the git file list. Keeping it in a single place is the point: when each
/// call site had its own `if name.starts_with('.')`, `.github/workflows/ci.yml`
/// showed up in the picker but was unreachable in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFilter {
    /// Show entries whose name starts with `.`. Default on — not seeing
    /// `.github/`, `.claude/` or `.env` is a real cost, and only the people who
    /// already know about the setting can fix it.
    pub show_dotfiles: bool,
    /// Also hide what git ignores. A separate switch from `show_dotfiles`
    /// because they are separate ideas: `build/` is ignored but not hidden,
    /// `.github/` is hidden but not ignored.
    pub respect_gitignore: bool,
}

impl Default for FileFilter {
    fn default() -> Self {
        Self {
            show_dotfiles: true,
            respect_gitignore: false,
        }
    }
}

impl FileFilter {
    pub fn from_config(config: &Config) -> Self {
        Self {
            show_dotfiles: config.ui.show_dotfiles,
            respect_gitignore: config.ui.respect_gitignore,
        }
    }

    /// True when an entry with this file name may be shown. A directory that
    /// fails this is not descended into either.
    pub fn accepts_name(&self, name: &str) -> bool {
        // The repository's own database is never useful to browse, and it is
        // large enough to drown the tree. Hidden regardless of the setting.
        if name == ".git" {
            return false;
        }
        self.show_dotfiles || !name.starts_with('.')
    }

    /// Which of `candidates` (project-root-relative paths, each flagged as a
    /// directory) to hide as git-ignored. Empty unless `respect_gitignore` is
    /// on or `project_root` is not a repository.
    ///
    /// Queried per listing, never cached: an ignore rule that has just been
    /// deleted must stop hiding files right away, and a set built earlier
    /// would keep hiding them.
    pub fn ignored_among(
        &self,
        project_root: &Path,
        candidates: &[(String, bool)],
    ) -> std::collections::HashSet<String> {
        if !self.respect_gitignore {
            return std::collections::HashSet::new();
        }
        crate::command::git_backend::ignored_paths(project_root, candidates)
    }

    /// Same test applied to a `/`-separated path relative to the project root.
    /// Any hidden component hides the whole path: a file under `.github/` is
    /// unreachable in the tree, so it must not surface in the picker either.
    pub fn accepts_rel_path(&self, rel: &str) -> bool {
        rel.split('/').all(|component| self.accepts_name(component))
    }
}

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
/// If inside a git repo, uses gix to respect `.gitignore`.
/// Otherwise falls back to a recursive directory walk.
///
/// Sees everything the filter allows by default. Callers that feed a file
/// picker should use [`collect_files_with_filter`] so the picker and the file
/// tree agree on what exists; the background indexes (search, symbols) stay on
/// the default so a hidden-in-the-tree file is still searchable.
pub fn collect_files(root: &Path) -> Vec<String> {
    collect_files_with_filter(root, FileFilter::default())
}

pub fn collect_files_with_filter(root: &Path, filter: FileFilter) -> Vec<String> {
    collect_files_inner(root, filter, None)
}

/// [`collect_files`] for a caller that already scanned the working tree. The
/// untracked half of a git file list otherwise costs a second full status walk.
pub fn collect_files_with_status(
    root: &Path,
    status: &std::collections::HashMap<String, crate::command::git::GitFileStatus>,
) -> Vec<String> {
    collect_files_inner(root, FileFilter::default(), Some(status))
}

fn collect_files_inner(
    root: &Path,
    filter: FileFilter,
    status: Option<&std::collections::HashMap<String, crate::command::git::GitFileStatus>>,
) -> Vec<String> {
    if has_git_marker(root)
        && let Some(files) = collect_files_git(root, filter, status)
    {
        return files;
    }
    collect_files_walk(root, root, filter)
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

pub fn has_git_marker(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    dot_git.is_dir() || dot_git.is_file()
}

/// The git file list is unfiltered by construction (index entries plus
/// untracked files), so the predicate is applied here — otherwise dotfiles
/// would keep reaching the picker after the tree learned to hide them.
fn collect_files_git(
    root: &Path,
    filter: FileFilter,
    status: Option<&std::collections::HashMap<String, crate::command::git::GitFileStatus>>,
) -> Option<Vec<String>> {
    let files = crate::command::git_backend::collect_files_with_status(root, status)?;
    Some(
        files
            .into_iter()
            .filter(|rel| filter.accepts_rel_path(rel))
            .collect(),
    )
}

fn collect_files_walk(dir: &Path, root: &Path, filter: FileFilter) -> Vec<String> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // `target` / `node_modules` are build output, not hidden files; they
        // stay excluded regardless of the dotfile setting (a future
        // `respect_gitignore` is where they really belong).
        if name_str == "target" || name_str == "node_modules" {
            continue;
        }
        if !filter.accepts_name(&name_str) {
            continue;
        }

        if path.is_dir() {
            if has_git_marker(&path) {
                if let Some(git_files) = collect_files_git(&path, filter, None)
                    && let Ok(prefix) = path.strip_prefix(root)
                {
                    let prefix_str = prefix.to_string_lossy();
                    for f in git_files {
                        result.push(format!("{prefix_str}/{f}"));
                    }
                }
            } else {
                result.extend(collect_files_walk(&path, root, filter));
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

    fn git_ls_files_present(repo: &Path) -> Vec<String> {
        let output = Command::new("git")
            .args([
                "-c",
                "core.quotepath=off",
                "ls-files",
                "-t",
                "--cached",
                "--others",
                "--exclude-standard",
                "--deleted",
            ])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let deleted: std::collections::HashSet<&str> = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("R "))
            .collect();
        let mut files: Vec<String> = stdout
            .lines()
            .filter_map(|line| {
                let (tag, path) = line.split_once(' ')?;
                (matches!(tag, "H" | "?") && !path.is_empty() && !deleted.contains(path))
                    .then(|| path.to_string())
            })
            .collect();
        files.sort();
        files
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
    fn collect_files_gix_matches_git_ls_files_oracle() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        init_git_repo(&repo);

        std::fs::create_dir_all(repo.join("tracked_dir")).unwrap();
        std::fs::write(repo.join("tracked_dir/keep.txt"), "keep\n").unwrap();
        std::fs::write(repo.join("tracked_dir/delete.txt"), "delete\n").unwrap();
        std::fs::write(repo.join("日本語.txt"), "hello\n").unwrap();
        let output = Command::new("git")
            .args(["add", "--all"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());

        std::fs::remove_file(repo.join("tracked_dir/delete.txt")).unwrap();
        std::fs::write(repo.join("untracked.txt"), "new\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "ignored\n").unwrap();

        let mut got = collect_files(&repo);
        got.sort();
        assert_eq!(got, git_ls_files_present(&repo));
    }

    #[test]
    fn file_filter_hides_dotfiles_only_when_asked() {
        let shown = FileFilter::default();
        let hidden = FileFilter {
            show_dotfiles: false,
            ..FileFilter::default()
        };

        assert!(shown.accepts_name(".github"));
        assert!(!hidden.accepts_name(".github"));
        assert!(shown.accepts_name("src"));
        assert!(hidden.accepts_name("src"));

        // `.git` is not browsable either way.
        assert!(!shown.accepts_name(".git"));
        assert!(!hidden.accepts_name(".git"));

        // A hidden component hides the whole path: a file the tree cannot
        // reach must not show up in the picker.
        assert!(!hidden.accepts_rel_path(".github/workflows/ci.yml"));
        assert!(shown.accepts_rel_path(".github/workflows/ci.yml"));
        assert!(hidden.accepts_rel_path("src/main.rs"));
    }

    #[test]
    fn collect_files_git_respects_the_filter() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".github")).unwrap();
        init_git_repo(&repo);
        std::fs::write(repo.join(".github").join("ci.yml"), "on: push").unwrap();
        std::fs::write(repo.join("keep.txt"), "keep").unwrap();

        let shown = collect_files_with_filter(
            &repo,
            FileFilter {
                show_dotfiles: true,
                ..FileFilter::default()
            },
        );
        assert!(shown.contains(&".github/ci.yml".to_string()), "{shown:?}");

        let hidden = collect_files_with_filter(
            &repo,
            FileFilter {
                show_dotfiles: false,
                ..FileFilter::default()
            },
        );
        assert!(
            !hidden.contains(&".github/ci.yml".to_string()),
            "{hidden:?}"
        );
        assert!(hidden.contains(&"keep.txt".to_string()), "{hidden:?}");
    }

    #[test]
    fn collect_files_walk_respects_the_filter() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".config")).unwrap();
        std::fs::write(tmp.path().join(".config").join("x.toml"), "x").unwrap();
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();

        let shown = collect_files_with_filter(
            tmp.path(),
            FileFilter {
                show_dotfiles: true,
                ..FileFilter::default()
            },
        );
        assert!(shown.contains(&".config/x.toml".to_string()), "{shown:?}");

        let hidden = collect_files_with_filter(
            tmp.path(),
            FileFilter {
                show_dotfiles: false,
                ..FileFilter::default()
            },
        );
        assert!(
            !hidden.contains(&".config/x.toml".to_string()),
            "{hidden:?}"
        );
        assert!(hidden.contains(&"a.txt".to_string()));
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

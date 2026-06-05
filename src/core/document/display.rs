use super::*;

impl Document {
    pub fn display_name(&self) -> String {
        self.file_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "[scratch]".to_string())
    }

    /// Returns a formatted path suitable for status bar display:
    /// - Git repos: "[repo_name] relative/path"
    /// - Non-git: Full path
    /// - Scratch: "[scratch]"
    pub fn status_bar_path(&self) -> &str {
        &self.cached_status_bar_path
    }

    /// Compute the status bar path (called once during document creation)
    pub(super) fn compute_status_bar_path(file_path: &Option<PathBuf>) -> String {
        let Some(path) = file_path else {
            return "[scratch]".to_string();
        };

        // Try to get git repo info
        match Self::get_git_repo_info(path) {
            Some((repo_name, relative_path)) => {
                format!("[{}] {}", repo_name, relative_path)
            }
            None => {
                // Not in a git repo, show full path
                path.display().to_string()
            }
        }
    }

    /// On wasm there is no local git or filesystem access, so the status-bar
    /// path falls back to the plain path. The browser editor receives repo
    /// context from the server, not from this code path.
    #[cfg(target_arch = "wasm32")]
    fn get_git_repo_info(_file_path: &std::path::Path) -> Option<(String, String)> {
        None
    }

    /// Returns (repo_name, relative_path) if the file is in a git repo
    #[cfg(not(target_arch = "wasm32"))]
    fn get_git_repo_info(file_path: &std::path::Path) -> Option<(String, String)> {
        let file_dir = file_path.parent()?;
        let repo_root = crate::command::git_backend::repo_root(file_dir).or_else(|| {
            let root = crate::project::find_project_root(Some(file_path));
            root.join(".git").exists().then_some(root)
        })?;
        let repo_root = repo_root.canonicalize().ok().unwrap_or(repo_root);

        // Extract repo name from remote URL (preferred)
        let repo_name = crate::command::git_backend::remote_origin_url(&repo_root)
            .and_then(|remote| Self::extract_repo_name_from_remote(&remote))
            .or_else(|| {
                // Fallback: use the directory name of the repo root
                repo_root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })?;

        // Compute relative path
        let relative_path = file_path
            .canonicalize()
            .ok()?
            .strip_prefix(repo_root)
            .ok()?
            .display()
            .to_string();

        Some((repo_name, relative_path))
    }

    /// Extract repository name from git remote URL
    /// Examples:
    ///   git@github.com:user/repo.git -> repo
    ///   https://github.com/user/repo.git -> repo
    ///   https://github.com/user/repo -> repo
    // Pure string logic with no native deps; on wasm `get_git_repo_info` is a
    // stub so this is unused there, but native tests exercise it directly.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(super) fn extract_repo_name_from_remote(remote: &str) -> Option<String> {
        let remote = remote.trim();

        // Extract the last component (repo name)
        let path_part = if remote.starts_with("git@github.com:") {
            remote.strip_prefix("git@github.com:")?
        } else if remote.starts_with("https://github.com/") {
            remote.strip_prefix("https://github.com/")?
        } else if remote.starts_with("http://github.com/") {
            remote.strip_prefix("http://github.com/")?
        } else if remote.starts_with("git@") {
            // Generic git SSH format: git@host:path
            remote.split(':').nth(1)?
        } else if remote.starts_with("https://") || remote.starts_with("http://") {
            // Generic HTTPS format
            remote.split('/').next_back()?
        } else {
            return None;
        };

        // Extract just the repo name (last part after /)
        let repo_name = path_part
            .trim_end_matches(".git")
            .split('/')
            .next_back()?
            .to_string();

        if repo_name.is_empty() {
            None
        } else {
            Some(repo_name)
        }
    }
}

//! Small helpers + page-specific CSS for the gargo server pages.

use crate::command::gargo_preview_server::{self};

pub(crate) fn parse_commit_hash(hash: &str) -> Option<String> {
    if hash.is_empty() || hash.len() > 64 {
        return None;
    }
    if hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_string())
    } else {
        None
    }
}

/// Normalize a client-supplied API path to a repo-relative path, returning
/// `None` if it contains a `..` traversal segment.
///
/// `normalize_rel_path_for_compare` only strips empty and `.` segments, so a
/// `..` would survive and `repo_root.join("../../etc")` would escape the repo —
/// the caller's lexical `Path::starts_with(repo_root)` check does not resolve
/// `..`, so it would wrongly pass. Rejecting `..` here closes that hole. The
/// empty/root path maps to ".".
pub(crate) fn normalize_api_path(path: &str) -> Option<String> {
    let normalized = gargo_preview_server::normalize_rel_path_for_compare(path);
    if normalized.split('/').any(|segment| segment == "..") {
        return None;
    }
    if normalized.is_empty() {
        Some(".".to_string())
    } else {
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_api_path_rejects_traversal() {
        assert_eq!(normalize_api_path("../../etc"), None);
        assert_eq!(normalize_api_path("src/../../etc"), None);
        assert_eq!(normalize_api_path("a/../../b"), None);
        // Percent-decoding happens in axum before we see the path, so the value
        // we validate is already the decoded `..` form.
        assert_eq!(normalize_api_path("%2e%2e/etc"), Some("%2e%2e/etc".to_string()));
    }

    #[test]
    fn normalize_api_path_accepts_normal_paths() {
        assert_eq!(normalize_api_path("src"), Some("src".to_string()));
        assert_eq!(normalize_api_path("src/command/mod.rs"), Some("src/command/mod.rs".to_string()));
        assert_eq!(normalize_api_path(""), Some(".".to_string()));
        assert_eq!(normalize_api_path("./src/./x"), Some("src/x".to_string()));
    }
}

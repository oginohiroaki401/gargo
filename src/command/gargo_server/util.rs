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

pub(crate) fn normalize_api_path(path: &str) -> String {
    let normalized = gargo_preview_server::normalize_rel_path_for_compare(path);
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

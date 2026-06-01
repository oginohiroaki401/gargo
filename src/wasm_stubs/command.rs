//! WASM stub for the `command` module.
//!
//! The real `command` module pulls in gix, tokio, axum, rusqlite and other
//! native-only crates. The browser editor only needs the git-gutter types that
//! `core` references, so this stub exposes just `command::git`. Git status is
//! unavailable in the browser, so the gutter is always empty.

pub mod git {
    use std::collections::HashMap;

    /// Mirror of the real `GitLineStatus` enum (kept variant-compatible).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum GitLineStatus {
        Added,
        Modified,
        Deleted,
    }

    /// No git backend in the browser — always returns an empty gutter map.
    pub fn git_diff_line_status(_path: &str) -> HashMap<usize, GitLineStatus> {
        HashMap::new()
    }
}

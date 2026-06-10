//! Diff-server request handlers and rendering shared by the gargo HTTP server.
//!
//! These were originally a standalone HTTP server; the live server is now
//! [`crate::command::gargo_server`], which mounts the `/api/*` JSON handlers and
//! the server-rendered `/split` page defined here. This module provides the
//! handlers, shared [`DiffServerState`], and rendering helpers — not a server.

use std::path::PathBuf;
use std::sync::Arc;

use crate::command::ai_summary::{AiConfig, AiSummaryStore};
use crate::command::diff_viewed::ViewedStore;

mod ai_api;
mod compare_api;
mod git_ops;
mod render;
mod split;
mod status_api;
mod templates;
mod validation;

pub(crate) use ai_api::*;
pub(crate) use compare_api::*;
pub(crate) use git_ops::*;
pub(crate) use render::*;
pub(crate) use split::*;
pub(crate) use status_api::*;
pub(crate) use templates::*;
pub(crate) use validation::*;

pub(crate) struct DiffServerState {
    pub(crate) project_root: PathBuf,
    /// On-disk persistence for per-file "Viewed" checkboxes.
    pub(crate) viewed: ViewedStore,
    /// In-memory cache of rendered immutable (compare/commit) file diffs.
    pub(crate) diff_cache: Arc<DiffRenderCache>,
    /// Non-secret AI settings (the API key is read from the environment).
    pub(crate) ai_config: AiConfig,
    /// On-disk cache of generated AI diff summaries, keyed by content hash.
    pub(crate) ai_store: AiSummaryStore,
}

impl DiffServerState {
    /// Stable key for this repo in the viewed-state database.
    fn repo_key(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }
}

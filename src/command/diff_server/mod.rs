//! Diff-server request handlers and rendering shared by the gargo HTTP server.
//!
//! These were originally a standalone HTTP server; the live server is now
//! [`crate::command::gargo_server`], which mounts the `/api/*` JSON handlers and
//! the server-rendered `/split` page defined here. This module provides the
//! handlers, shared [`DiffServerState`], and rendering helpers — not a server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::command::ai_summary::{AiConfig, AiSummaryStore};
use crate::command::diff_viewed::ViewedStore;

/// How long a fetched `base...compare` diff stays reusable for the AI endpoints.
/// Short enough that an advancing branch is picked up quickly, long enough that
/// the rapid turns of one chat session skip the gix recompute.
const AI_DIFF_TTL: Duration = Duration::from_secs(30);

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
    /// Short-TTL in-memory cache of `base...compare` diff text, so a multi-turn
    /// chat (or a summary refetch) against an unchanged ref pair doesn't rerun
    /// the gix diff every call. Keyed by `"{base}\0{compare}"`.
    pub(crate) ai_diff_cache: Mutex<HashMap<String, (Instant, String)>>,
}

impl DiffServerState {
    /// Stable key for this repo in the viewed-state database.
    fn repo_key(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }

    /// Return a still-fresh cached diff for the ref pair, if any.
    fn cached_compare_diff(&self, base: &str, compare: &str) -> Option<String> {
        let key = format!("{base}\u{0}{compare}");
        let cache = self.ai_diff_cache.lock().ok()?;
        let (stored_at, diff) = cache.get(&key)?;
        (stored_at.elapsed() <= AI_DIFF_TTL).then(|| diff.clone())
    }

    /// Store a freshly computed diff, dropping expired entries so the map stays
    /// bounded to the handful of ref pairs an active session touches.
    fn store_compare_diff(&self, base: &str, compare: &str, diff: &str) {
        if let Ok(mut cache) = self.ai_diff_cache.lock() {
            cache.retain(|_, (t, _)| t.elapsed() <= AI_DIFF_TTL);
            cache.insert(
                format!("{base}\u{0}{compare}"),
                (Instant::now(), diff.to_string()),
            );
        }
    }
}

//! `/api/ai/*` JSON handlers: AI-generated summaries of a diff.
//!
//! Phase 1 implements `/api/ai/summary` for the compare page (`base...compare`).
//! The heavy lifting — the git diff and the provider HTTP call — runs on the
//! blocking pool so the async runtime is never stalled. Results are cached by
//! diff content hash so an unchanged comparison is summarised at most once.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::Response;

use super::{DiffServerState, bad_request, ok_json, parse_compare_branches};
use crate::command::ai_summary::{self, MAX_DIFF_BYTES};
use crate::command::diff_viewed::PAGE_COMPARE;

/// `GET /api/ai/summary?base=<ref>&compare=<ref>`
///
/// Returns one of:
/// - `{ "enabled": false }` when AI summaries are turned off in config,
/// - `{ "error": "<message>" }` for a recoverable problem (key missing, diff
///   too large, provider error) — the page shows it inline,
/// - `{ "summary": "<markdown>", "model": "...", "cached": <bool> }` on success.
pub(crate) async fn handle_api_ai_summary_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if !state.ai_config.enabled {
        return ok_json(serde_json::json!({ "enabled": false }));
    }

    let (base, compare) = match parse_compare_branches(&params) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // `base...compare` unified diff via in-process gix (no subprocess).
    let repo_root = state.project_root.clone();
    let (base_c, compare_c) = (base.clone(), compare.clone());
    let diff = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::compare_diff_text(&repo_root, &base_c, &compare_c, None)
    })
    .await
    .ok()
    .flatten();
    let diff = match diff {
        Some(output) => output,
        None => return bad_request("invalid base/compare ref"),
    };

    if diff.trim().is_empty() {
        return ok_json(serde_json::json!({
            "summary": "_No changes between the selected branches._",
            "model": state.ai_config.model,
            "cached": false,
        }));
    }

    if diff.len() > MAX_DIFF_BYTES {
        return ok_json(serde_json::json!({
            "error": format!(
                "Diff is too large to summarise ({} KB > {} KB). Per-file summaries are coming in a later phase.",
                diff.len() / 1024,
                MAX_DIFF_BYTES / 1024,
            ),
        }));
    }

    let repo_key = state.repo_key();
    let content_hash = ai_summary::diff_hash(&diff);
    let model = state.ai_config.model.clone();

    // Cache hit: never re-bill an unchanged comparison.
    if let Some(summary) = state.ai_store.get(
        &repo_key,
        PAGE_COMPARE,
        &base,
        &compare,
        &content_hash,
        &model,
    ) {
        return ok_json(serde_json::json!({
            "summary": summary,
            "model": model,
            "cached": true,
        }));
    }

    // Miss: call the provider on the blocking pool (ureq is blocking).
    let ai_config = state.ai_config.clone();
    let diff_for_call = diff.clone();
    let result = tokio::task::spawn_blocking(move || {
        ai_summary::generate_summary(&ai_config, &diff_for_call)
    })
    .await;

    let summary = match result {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => return ok_json(serde_json::json!({ "error": message })),
        Err(_) => return ok_json(serde_json::json!({ "error": "summary task failed" })),
    };

    let _ = state.ai_store.set(
        &repo_key,
        PAGE_COMPARE,
        &base,
        &compare,
        &content_hash,
        &model,
        &summary,
    );

    ok_json(serde_json::json!({
        "summary": summary,
        "model": model,
        "cached": false,
    }))
}

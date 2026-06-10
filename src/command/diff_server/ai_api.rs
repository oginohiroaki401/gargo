//! `/api/ai/*` JSON handlers: AI summary and chat over a compare diff.
//!
//! `/api/ai/summary` returns a one-shot summary of `base...compare`;
//! `/api/ai/chat` answers a conversation grounded in the same diff. The heavy
//! lifting — the git diff and the provider HTTP call — runs on the blocking
//! pool so the async runtime is never stalled. Summaries are cached by diff
//! content hash; chat turns are not cached (each is unique).

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::Response;

use super::{DiffServerState, bad_request, ok_json, parse_compare_branches};
use crate::command::ai_summary;
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
        return summary_response(
            "_No changes between the selected branches._",
            &state.ai_config.model,
            false,
        );
    }

    if diff.len() > state.ai_config.max_diff_bytes {
        return ok_json(serde_json::json!({
            "error": format!(
                "Diff is too large to summarise ({} KB > {} KB limit). Raise [plugin.gargo_server.ai].max_diff_bytes in config to summarise bigger diffs.",
                diff.len() / 1024,
                state.ai_config.max_diff_bytes / 1024,
            ),
        }));
    }

    let repo_key = state.repo_key();
    // Fold the output language into the hash so changing it invalidates the
    // cache (otherwise a stale English summary would be returned for Japanese).
    let content_hash = ai_summary::diff_hash(&format!("{}\u{0}{}", state.ai_config.language, diff));
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
        return summary_response(&summary, &model, true);
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

    summary_response(&summary, &model, false)
}

/// Build a success response carrying both the raw Markdown summary and the
/// server-rendered HTML (via the shared comrak config) so the WASM client can
/// inject it directly without a client-side Markdown parser.
fn summary_response(summary: &str, model: &str, cached: bool) -> Response {
    let html = crate::command::gargo_preview_server::render_markdown(summary);
    ok_json(serde_json::json!({
        "summary": summary,
        "summary_html": html,
        "model": model,
        "cached": cached,
    }))
}

/// Request body for `/api/ai/chat`: the ref pair plus the running conversation.
#[derive(serde::Deserialize)]
pub(crate) struct ChatRequest {
    base: String,
    compare: String,
    #[serde(default)]
    messages: Vec<ai_summary::ChatMessage>,
}

/// `POST /api/ai/chat` — answer a question about the `base...compare` diff.
///
/// Body: `{ base, compare, messages: [{role, content}, ...] }`. Returns
/// `{ enabled:false }` when off, `{ error }` on a recoverable problem, or
/// `{ reply, reply_html, model }` on success. The full conversation is sent by
/// the client each turn (the server is stateless); the diff is injected as
/// grounding context server-side.
pub(crate) async fn handle_api_ai_chat_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    if !state.ai_config.enabled {
        return ok_json(serde_json::json!({ "enabled": false }));
    }

    // Validate the ref pair through the same path the compare endpoint uses.
    let mut params = HashMap::new();
    params.insert("base".to_string(), req.base.clone());
    params.insert("compare".to_string(), req.compare.clone());
    let (base, compare) = match parse_compare_branches(&params) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    if req.messages.is_empty() {
        return bad_request("no messages");
    }

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

    if diff.len() > state.ai_config.max_diff_bytes {
        return ok_json(serde_json::json!({
            "error": format!(
                "Diff is too large to chat about ({} KB > {} KB limit). Raise [plugin.gargo_server.ai].max_diff_bytes in config.",
                diff.len() / 1024,
                state.ai_config.max_diff_bytes / 1024,
            ),
        }));
    }

    let ai_config = state.ai_config.clone();
    let messages = req.messages;
    let result = tokio::task::spawn_blocking(move || {
        ai_summary::generate_chat(&ai_config, &diff, &messages)
    })
    .await;

    let reply = match result {
        Ok(Ok(reply)) => reply,
        Ok(Err(message)) => return ok_json(serde_json::json!({ "error": message })),
        Err(_) => return ok_json(serde_json::json!({ "error": "chat task failed" })),
    };

    let reply_html = crate::command::gargo_preview_server::render_markdown(&reply);
    ok_json(serde_json::json!({
        "reply": reply,
        "reply_html": reply_html,
        "model": state.ai_config.model,
    }))
}

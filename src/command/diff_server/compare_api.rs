//! `/api/branches` and `/api/compare*` endpoints.

use super::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::{Json, Response},
};

use crate::command::diff_viewed::PAGE_COMPARE;
use crate::diff_render::{DiffFile, content_hash_of, parse_unified_diff};

pub(crate) async fn handle_api_compare_context_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let path_raw = match params.get("path") {
        Some(v) => v,
        None => return bad_request("missing `path` query parameter"),
    };
    let path = match parse_diff_path(path_raw) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", path_raw)),
    };
    let git_ref = match params.get("ref") {
        Some(v) if !v.is_empty() => v.as_str(),
        _ => return bad_request("missing `ref` query parameter"),
    };
    let start = match parse_usize_param(&params, "start") {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    let end = match parse_usize_param(&params, "end") {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    match read_file_range_at_ref(&state.project_root, Some(git_ref), &path, start, end).await {
        Ok(lines) => ok_json(serde_json::json!({ "lines": lines })),
        Err(e) => bad_request(e),
    }
}

/// List local and remote branches in the repo along with the current HEAD.
///
/// `for-each-ref` lets us tell which side a ref came from via its full
/// `refname` (so callers can compare e.g. `origin/master` against a local
/// branch without ambiguity), and lets us skip the `*/HEAD` symbolic refs
/// that would otherwise duplicate a remote's default branch.
pub(crate) async fn handle_api_branches_request(
    State(state): State<Arc<DiffServerState>>,
) -> Response {
    // Listing refs + reading `origin/HEAD` is in-process gix work (no subprocess);
    // it still touches the ref store on disk, so run it on the blocking pool.
    let repo_root = state.project_root.clone();
    let result = tokio::task::spawn_blocking(move || {
        let list = crate::command::git_backend::list_branches(&repo_root)?;
        let origin_head = crate::command::git_backend::origin_head_short(&repo_root);
        Some((list, origin_head))
    })
    .await;

    let Ok(Some((list, origin_head))) = result else {
        return bad_request("not a git repository");
    };

    let default = resolve_default_from(origin_head, &list.branches);

    // Per-branch tip info keyed by name, so the picker can show each ref's
    // last commit (hash · subject · date) without another request.
    let info: serde_json::Map<String, serde_json::Value> = list
        .tips
        .iter()
        .map(|t| {
            (
                t.name.clone(),
                serde_json::json!({
                    "hash": t.hash,
                    "message": t.summary,
                    "time": t.time,
                }),
            )
        })
        .collect();

    ok_json(serde_json::json!({
        "current": list.current,
        "default": default,
        "branches": list.branches,
        "remotes": list.remotes,
        "info": info,
    }))
}

/// Best-effort detection of the repository's default branch from an already-fetched
/// `origin/HEAD` symbolic-ref output (pass `None` when the probe failed).
///
/// Tries `origin/HEAD` first (set by `git clone` or `git remote set-head`), then
/// falls back to the well-known `main` / `master` names if either exists
/// locally. Returns `None` only for repos without remote and without either
/// conventional name. Kept as a pure function so the `symbolic-ref` spawn can run
/// concurrently with `for-each-ref` in the caller.
pub(crate) fn resolve_default_from(
    origin_head: Option<String>,
    known: &[String],
) -> Option<String> {
    if let Some(output) = origin_head {
        let trimmed = output.trim();
        if let Some(rest) = trimmed.strip_prefix("origin/")
            && !rest.is_empty()
            && known.iter().any(|b| b == rest)
        {
            return Some(rest.to_string());
        }
    }
    for candidate in ["main", "master"] {
        if known.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Compute `git diff base...compare` for the requested branches.
pub(crate) async fn handle_api_compare_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (base, compare) = match parse_compare_branches(&params) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // In-process gix `base...compare` diff (no `git diff` subprocess).
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

    // Viewed records are scoped to this exact base/compare pair, so switching
    // either branch naturally resets the checkboxes.
    let viewed = load_viewed_map(&state, PAGE_COMPARE, base.clone(), compare.clone()).await;
    let files: Vec<serde_json::Value> = parse_unified_diff(&diff)
        .iter()
        .map(|f| file_metadata_json(f, diff_file_is_viewed(&viewed, "", f)))
        .collect();

    ok_json(serde_json::json!({
        "base": base,
        "compare": compare,
        "files": files,
    }))
}

/// `base...compare` diff for a single file via in-process gix, parsed.
pub(crate) async fn load_compare_diff_file(
    repo_root: &Path,
    base: &str,
    compare: &str,
    path: &str,
) -> Result<Option<DiffFile>, String> {
    let (root, base, compare, path) = (
        repo_root.to_path_buf(),
        base.to_string(),
        compare.to_string(),
        path.to_string(),
    );
    let diff = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::compare_diff_text(&root, &base, &compare, Some(&path))
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "invalid base/compare ref".to_string())?;
    Ok(parse_unified_diff(&diff).into_iter().next())
}

pub(crate) async fn handle_api_compare_file_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let (base, compare) = match parse_compare_branches(&params) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let path_raw = match params.get("path") {
        Some(v) => v,
        None => return bad_request("missing `path` query parameter"),
    };
    let path = match parse_diff_path(path_raw) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", path_raw)),
    };

    // `base...compare` for a fixed pair of object ids is immutable, so render
    // once and cache by resolved OIDs. Resolution, the cache lookup, the gix
    // diff and the syntax highlighting all run on the blocking pool.
    let root = state.project_root.clone();
    let cache = state.diff_cache.clone();
    let (base_c, compare_c, path_c) = (base.clone(), compare.clone(), path.clone());
    let result = tokio::task::spawn_blocking(move || -> Option<serde_json::Value> {
        let oids = crate::command::git_backend::resolve_oids(&root, &[&base_c, &compare_c])?;
        let key = format!("cmp\u{1f}{}\u{1f}{}\u{1f}{}", oids[0], oids[1], path_c);
        if let Some(hit) = cache.get(&key) {
            return Some(hit);
        }
        let diff = crate::command::git_backend::compare_diff_text(
            &root,
            &base_c,
            &compare_c,
            Some(&path_c),
        )?;
        let value = file_diff_json_from_text(&diff, &path_c, "modified");
        cache.insert(key, value.clone());
        Some(value)
    })
    .await;

    match result {
        Ok(Some(value)) => ok_json(value),
        Ok(None) => bad_request("invalid base/compare ref"),
        Err(e) => bad_request(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct CompareViewedRequest {
    base: String,
    compare: String,
    path: String,
    viewed: bool,
}

/// POST endpoint: persist the "Viewed" checkbox for one compare-page file.
///
/// The record is scoped to the `base`/`compare` branch pair and pinned to the
/// file's current content hash.
pub(crate) async fn handle_api_compare_viewed_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<CompareViewedRequest>,
) -> Response {
    let base = match parse_branch_name(&req.base) {
        Some(b) => b,
        None => return bad_request(format!("invalid branch name: {}", req.base)),
    };
    let compare = match parse_branch_name(&req.compare) {
        Some(c) => c,
        None => return bad_request(format!("invalid branch name: {}", req.compare)),
    };
    let path = match parse_diff_path(&req.path) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", req.path)),
    };

    if !req.viewed {
        store_viewed(
            &state,
            PAGE_COMPARE,
            base,
            compare,
            String::new(),
            path,
            None,
        )
        .await;
        return ok_json(serde_json::json!({ "viewed": false }));
    }

    let hash = match load_compare_diff_file(&state.project_root, &base, &compare, &path).await {
        Ok(Some(file)) => content_hash_of(&file),
        Ok(None) => String::new(),
        Err(e) => return bad_request(e),
    };
    if hash.is_empty() {
        return ok_json(serde_json::json!({ "viewed": false }));
    }
    store_viewed(
        &state,
        PAGE_COMPARE,
        base,
        compare,
        String::new(),
        path,
        Some(hash),
    )
    .await;
    ok_json(serde_json::json!({ "viewed": true }))
}

#[allow(clippy::result_large_err)]
pub(crate) fn parse_compare_branches(
    params: &HashMap<String, String>,
) -> Result<(String, String), Response> {
    let base_raw = params
        .get("base")
        .ok_or_else(|| bad_request("missing `base` query parameter"))?;
    let compare_raw = params
        .get("compare")
        .ok_or_else(|| bad_request("missing `compare` query parameter"))?;
    let base = parse_branch_name(base_raw)
        .ok_or_else(|| bad_request(format!("invalid branch name: {}", base_raw)))?;
    let compare = parse_branch_name(compare_raw)
        .ok_or_else(|| bad_request(format!("invalid branch name: {}", compare_raw)))?;
    Ok((base, compare))
}

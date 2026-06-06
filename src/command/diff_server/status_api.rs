//! `/api/status*` and commit endpoints (stage/unstage/viewed/commit).

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::{Json, Response},
};

use crate::command::diff_viewed::PAGE_STATUS;
use crate::command::git_backend;
use crate::diff_render::{content_hash_of, parse_unified_diff};

/// API endpoint that returns unstaged/staged diffs and untracked files.
pub(crate) async fn handle_api_status_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let show_untracked = parse_bool_param(params.get("show_untracked"), true);
    let repo_root = &state.project_root;

    let (unstaged_res, staged_res) = (
        status_diff_text(repo_root, false),
        status_diff_text(repo_root, true),
    );
    let viewed = load_viewed_map(&state, PAGE_STATUS, String::new(), String::new()).await;
    let unstaged_raw = match unstaged_res {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };
    let staged_raw = match staged_res {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    let unstaged_files: Vec<serde_json::Value> = parse_unified_diff(&unstaged_raw)
        .iter()
        .map(|f| file_metadata_json(f, diff_file_is_viewed(&viewed, "unstaged", f)))
        .collect();
    let staged_files: Vec<serde_json::Value> = parse_unified_diff(&staged_raw)
        .iter()
        .map(|f| file_metadata_json(f, diff_file_is_viewed(&viewed, "staged", f)))
        .collect();

    let untracked_files: Vec<serde_json::Value> = if show_untracked {
        let paths: Vec<String> = match git_backend::status_files(repo_root) {
            Some((changed, _)) => changed
                .into_iter()
                .filter(|entry| entry.status_char == '?')
                .map(|entry| entry.path)
                .collect(),
            None => return bad_request("failed to read git status"),
        };

        // Scan every untracked file concurrently — each scan is independent
        // file I/O, so a serial loop needlessly waited on one read at a time.
        // Results are slotted back by index to preserve `ls-files` order.
        let mut set = tokio::task::JoinSet::new();
        for (idx, path) in paths.iter().cloned().enumerate() {
            let root = repo_root.to_path_buf();
            set.spawn(async move { (idx, scan_untracked_file(&root, &path).await) });
        }
        let mut scans: Vec<(usize, bool, String)> = vec![(0, false, String::new()); paths.len()];
        while let Some(joined) = set.join_next().await {
            if let Ok((idx, scan)) = joined {
                scans[idx] = scan;
            }
        }

        paths
            .iter()
            .zip(scans)
            .map(|(path, (additions, binary, hash))| {
                // A whole untracked file shows up as an all-additions diff, so
                // its line count drives the client's huge-diff collapse decision.
                let is_viewed = viewed
                    .get(&("untracked".to_string(), path.to_string()))
                    .is_some_and(|stored| !hash.is_empty() && *stored == hash);
                serde_json::json!({
                    "path": path,
                    "old_path": serde_json::Value::Null,
                    "status": "untracked",
                    "binary": binary,
                    "additions": additions,
                    "deletions": 0,
                    "viewed": is_viewed,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    ok_json(serde_json::json!({
        "branch": git_backend::current_branch(repo_root).unwrap_or_default(),
        "unstaged": unstaged_files,
        "staged": staged_files,
        "untracked": untracked_files,
    }))
}

pub(crate) async fn handle_api_status_file_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let section = match params.get("section").map(String::as_str) {
        Some(s) if matches!(s, "staged" | "unstaged" | "untracked") => s,
        _ => return bad_request("missing or invalid `section` query parameter"),
    };
    let path_raw = match params.get("path") {
        Some(v) => v,
        None => return bad_request("missing `path` query parameter"),
    };
    let path = match parse_diff_path(path_raw) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", path_raw)),
    };
    let file = match load_status_diff_file(&state.project_root, section, &path).await {
        Ok(file) => file,
        Err(e) => return bad_request(e),
    };
    match file {
        Some(file) => {
            let html = render_highlighted(&file);
            ok_json(serde_json::json!({
                "path": file.path,
                "status": file.status.as_str(),
                "additions": file.additions,
                "deletions": file.deletions,
                "binary": file.binary,
                "html": html,
            }))
        }
        None => ok_json(serde_json::json!({
            "path": path,
            "status": section,
            "additions": 0,
            "deletions": 0,
            "binary": false,
            "html": empty_diff_html(),
        })),
    }
}

pub(crate) fn parse_usize_param(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<usize, String> {
    params
        .get(key)
        .ok_or_else(|| format!("missing `{}` query parameter", key))?
        .parse::<usize>()
        .map_err(|e| format!("invalid `{}`: {}", key, e))
}

pub(crate) async fn handle_api_status_context_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let git_ref: Option<&str> = match params.get("section").map(String::as_str) {
        Some("staged") => Some("HEAD"),
        Some("unstaged") | Some("untracked") | None => None,
        _ => return bad_request("invalid `section` query parameter"),
    };
    let path_raw = match params.get("path") {
        Some(v) => v,
        None => return bad_request("missing `path` query parameter"),
    };
    let path = match parse_diff_path(path_raw) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", path_raw)),
    };
    let start = match parse_usize_param(&params, "start") {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    let end = match parse_usize_param(&params, "end") {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };
    match read_file_range_at_ref(&state.project_root, git_ref, &path, start, end).await {
        Ok(lines) => ok_json(serde_json::json!({ "lines": lines })),
        Err(e) => bad_request(e),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct StatusViewedRequest {
    section: String,
    path: String,
    viewed: bool,
}

/// POST endpoint: persist the "Viewed" checkbox for one status-page file.
///
/// When `viewed` is true the file's current content hash is computed and
/// stored, so the checkbox is later honored only while the content matches.
pub(crate) async fn handle_api_status_viewed_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<StatusViewedRequest>,
) -> Response {
    let section = match req.section.as_str() {
        s @ ("staged" | "unstaged" | "untracked") => s,
        _ => return bad_request("missing or invalid `section`"),
    };
    let path = match parse_diff_path(&req.path) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", req.path)),
    };

    if !req.viewed {
        store_viewed(
            &state,
            PAGE_STATUS,
            String::new(),
            String::new(),
            section.to_string(),
            path,
            None,
        )
        .await;
        return ok_json(serde_json::json!({ "viewed": false }));
    }

    // Pin the viewed record to the file's current content.
    let hash = if section == "untracked" {
        let (_, _, h) = scan_untracked_file(&state.project_root, &path).await;
        h
    } else {
        match load_status_diff_file(&state.project_root, section, &path).await {
            Ok(Some(file)) => content_hash_of(&file),
            Ok(None) => String::new(),
            Err(e) => return bad_request(e),
        }
    };
    if hash.is_empty() {
        // No content to anchor the record to (e.g. the file vanished).
        return ok_json(serde_json::json!({ "viewed": false }));
    }
    store_viewed(
        &state,
        PAGE_STATUS,
        String::new(),
        String::new(),
        section.to_string(),
        path,
        Some(hash),
    )
    .await;
    ok_json(serde_json::json!({ "viewed": true }))
}

#[derive(serde::Deserialize)]
pub(crate) struct StagePathRequest {
    path: String,
}

/// Content hash anchoring a "Viewed" record for `path` as rendered in
/// `section`, or `None` when the file has no content in that section right now
/// (e.g. a fully-staged file no longer appears under `unstaged`).
pub(crate) async fn viewed_hash_for_section(
    state: &Arc<DiffServerState>,
    section: &str,
    path: &str,
) -> Option<String> {
    if section == "untracked" {
        let (_, _, h) = scan_untracked_file(&state.project_root, path).await;
        return (!h.is_empty()).then_some(h);
    }
    match load_status_diff_file(&state.project_root, section, path).await {
        Ok(Some(file)) => Some(content_hash_of(&file)),
        _ => None,
    }
}

/// The first of `from_sections` in which `path` is genuinely viewed *right now*
/// — a stored record whose hash still matches the file's content. Call this
/// before a stage/unstage so the checkbox carries over, while a record left
/// stale by an edit (content no longer matches) is not silently revived.
pub(crate) async fn viewed_source_section(
    state: &Arc<DiffServerState>,
    path: &str,
    from_sections: &[&'static str],
) -> Option<&'static str> {
    let viewed = load_viewed_map(state, PAGE_STATUS, String::new(), String::new()).await;
    for &section in from_sections {
        if let Some(stored) = viewed.get(&(section.to_string(), path.to_string()))
            && viewed_hash_for_section(state, section, path)
                .await
                .as_deref()
                == Some(stored.as_str())
        {
            return Some(section);
        }
    }
    None
}

/// Move a file's "Viewed" record from `from` to the first of `to_sections`
/// whose representation now has content, re-pinning it to a freshly computed
/// hash. The staged (`index..HEAD`) and unstaged (`worktree..index`) diffs hash
/// differently, so the record must be re-anchored rather than copied verbatim.
/// Call this after the stage/unstage git op has run.
pub(crate) async fn move_viewed_record(
    state: &Arc<DiffServerState>,
    path: &str,
    from: &str,
    to_sections: &[&'static str],
) {
    store_viewed(
        state,
        PAGE_STATUS,
        String::new(),
        String::new(),
        from.to_string(),
        path.to_string(),
        None,
    )
    .await;
    for &to in to_sections {
        if let Some(hash) = viewed_hash_for_section(state, to, path).await {
            store_viewed(
                state,
                PAGE_STATUS,
                String::new(),
                String::new(),
                to.to_string(),
                path.to_string(),
                Some(hash),
            )
            .await;
            return;
        }
    }
}

/// POST endpoint: stage one file (`git add -- <path>`). Works for modified,
/// deleted, and untracked paths alike — `git add` records each appropriately.
pub(crate) async fn handle_api_status_stage_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<StagePathRequest>,
) -> Response {
    let path = match parse_diff_path(&req.path) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", req.path)),
    };
    // Capture whether the file is viewed before it hops sections, then carry the
    // checkbox over to the staged section once the move has happened.
    let viewed_from = viewed_source_section(&state, &path, &["unstaged", "untracked"]).await;
    match git_output_in_repo(&state.project_root, &["add", "--", &path]).await {
        Ok(_) => {
            if let Some(from) = viewed_from {
                move_viewed_record(&state, &path, from, &["staged"]).await;
            }
            ok_json(serde_json::json!({ "ok": true }))
        }
        Err(e) => bad_request(e),
    }
}

/// POST endpoint: unstage one file. `git reset -- <path>` restores the index
/// entry from HEAD (and works before the first commit, where it just removes
/// the path from the index).
pub(crate) async fn handle_api_status_unstage_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<StagePathRequest>,
) -> Response {
    let path = match parse_diff_path(&req.path) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {}", req.path)),
    };
    // Carry the "Viewed" checkbox back to wherever the file lands after the
    // reset: a tracked file returns to `unstaged`, a freshly-added one to
    // `untracked`.
    let viewed_from = viewed_source_section(&state, &path, &["staged"]).await;
    match git_output_in_repo(&state.project_root, &["reset", "--quiet", "--", &path]).await {
        Ok(_) => {
            if let Some(from) = viewed_from {
                move_viewed_record(&state, &path, from, &["unstaged", "untracked"]).await;
            }
            ok_json(serde_json::json!({ "ok": true }))
        }
        Err(e) => bad_request(e),
    }
}

/// GET endpoint backing the commit page: the list of staged files, the current
/// branch, and HEAD's subject+body (so the amend toggle can prefill it).
pub(crate) async fn handle_api_commit_prepare_request(
    State(state): State<Arc<DiffServerState>>,
) -> Response {
    let repo_root = &state.project_root;
    let staged_raw = match status_diff_text(repo_root, true) {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };
    let staged: Vec<serde_json::Value> = parse_unified_diff(&staged_raw)
        .iter()
        .map(|f| file_metadata_json(f, false))
        .collect();

    let branch = git_backend::current_branch(repo_root).unwrap_or_default();

    // HEAD's full message for the amend toggle to prefill. Empty before the
    // first commit, in which case amend is not offered.
    let head_meta = git_backend::commit_meta(repo_root, "HEAD");
    let last_message = head_meta
        .as_ref()
        .map(|m| m.message.trim_end().to_string())
        .unwrap_or_default();
    let has_head = head_meta.is_some();

    ok_json(serde_json::json!({
        "staged": staged,
        "branch": branch,
        "last_message": last_message,
        "has_head": has_head,
    }))
}

#[derive(serde::Deserialize)]
pub(crate) struct CommitRequest {
    message: String,
    #[serde(default)]
    amend: bool,
}

/// POST endpoint: create a commit from the staged changes. With `amend` it
/// rewrites HEAD instead. The message is passed via stdin-free `-m`, and the
/// commit runs with `--cleanup=strip` so trailing whitespace is normalized.
pub(crate) async fn handle_api_commit_request(
    State(state): State<Arc<DiffServerState>>,
    Json(req): Json<CommitRequest>,
) -> Response {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return bad_request("commit message must not be empty");
    }

    let mut args: Vec<&str> = vec!["commit", "--cleanup=strip"];
    if req.amend {
        args.push("--amend");
    }
    // `-m` consumes the next argument literally, so the message can never be
    // parsed as a flag even if it begins with `-`.
    args.push("-m");
    args.push(&message);

    match git_output_in_repo(&state.project_root, &args).await {
        Ok(_) => ok_json(serde_json::json!({ "ok": true })),
        Err(e) => bad_request(e),
    }
}

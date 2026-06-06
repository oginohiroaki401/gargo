//! Repo-browsing JSON API: tree, blob, commits, commit, commit file.

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    response::Response,
};

use crate::command::diff_server::{self};
use crate::command::gargo_preview_server::{
    self,
};
use crate::diff_render::parse_unified_diff;

pub(crate) async fn handle_api_tree(
    State(state): State<Arc<GargoServerState>>,
    AxumPath(path): AxumPath<String>,
) -> Response {
    let path = normalize_api_path(&path);
    let full_path = state.repo_root.join(&path);
    if !full_path.starts_with(&state.repo_root) {
        return diff_server::bad_request("invalid path");
    }
    let mut entries = match tokio::fs::read_dir(&full_path).await {
        Ok(entries) => entries,
        Err(err) => return diff_server::bad_request(format!("failed to read directory: {}", err)),
    };
    let mut items = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        let entry_path = if path == "." {
            name.clone()
        } else {
            format!("{}/{}", path, name)
        };
        items.push(serde_json::json!({
            "name": name,
            "path": entry_path,
            "kind": if metadata.is_dir() { "dir" } else { "file" },
        }));
    }
    items.sort_by(|a, b| {
        let ak = a["kind"].as_str().unwrap_or("");
        let bk = b["kind"].as_str().unwrap_or("");
        ak.cmp(bk).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        })
    });
    diff_server::ok_json(serde_json::json!({ "path": path, "entries": items }))
}

pub(crate) async fn handle_api_blob(
    State(state): State<Arc<GargoServerState>>,
    AxumPath(path): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = diff_server::parse_diff_path(&path) else {
        return diff_server::bad_request("invalid path");
    };
    let full_path = state.repo_root.join(&path);
    if !full_path.starts_with(&state.repo_root) {
        return diff_server::bad_request("invalid path");
    }
    let bytes = match tokio::fs::read(&full_path).await {
        Ok(bytes) => bytes,
        Err(err) => return diff_server::bad_request(format!("failed to read file: {}", err)),
    };
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => return diff_server::bad_request("binary file"),
    };
    let plain = params.get("plain").map(|v| v == "1").unwrap_or(false);
    let html = if path.ends_with(".md") && !plain {
        gargo_preview_server::render_markdown_with_source_lines(&text)
    } else {
        format!(
            "<div class=\"code-view\">{}</div>",
            gargo_preview_server::render_code_with_line_ids_for_path(&text, &path)
        )
    };
    diff_server::ok_json(serde_json::json!({ "path": path, "content": text, "html": html }))
}

pub(crate) async fn handle_api_commits(
    State(state): State<Arc<GargoServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let skip = params
        .get("skip")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let count = params
        .get("count")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .min(200);
    // In-process gix commit walk (no `git log` subprocess). Over-fetch one row
    // to compute `has_more`, exactly as the subprocess path did.
    let repo_root = state.repo_root.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::commit_log(&repo_root, skip, count + 1)
    })
    .await;
    let Ok(Some(mut rows)) = result else {
        return diff_server::bad_request("not a git repository");
    };
    let has_more = rows.len() > count;
    rows.truncate(count);
    let commits: Vec<_> = rows
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "hash": c.hash,
                "full_hash": c.full_hash,
                "author": c.author,
                "date": c.date,
                "message": c.message,
            })
        })
        .collect();
    diff_server::ok_json(serde_json::json!({ "commits": commits, "has_more": has_more }))
}

pub(crate) async fn handle_api_commit(
    State(state): State<Arc<GargoServerState>>,
    AxumPath(hash): AxumPath<String>,
) -> Response {
    let Some(hash) = parse_commit_hash(&hash) else {
        return diff_server::bad_request("invalid commit hash");
    };
    // Commit metadata + full diff, both in-process via gix (no subprocess). The
    // file list is derived from the parsed diff, so there's no separate
    // `diff-tree --name-status` call.
    let repo_root = state.repo_root.clone();
    let hash_c = hash.clone();
    let result = tokio::task::spawn_blocking(move || {
        let meta = crate::command::git_backend::commit_meta(&repo_root, &hash_c)?;
        let text = crate::command::git_backend::commit_diff_text(&repo_root, &hash_c, None)?;
        Some((meta, parse_unified_diff(&text)))
    })
    .await;
    let Ok(Some((meta, parsed))) = result else {
        return diff_server::bad_request("invalid commit hash");
    };
    let files: Vec<_> = parsed
        .iter()
        .map(|f| {
            let letter = match f.status.as_str() {
                "added" | "untracked" => "A",
                "deleted" => "D",
                "renamed" => "R",
                _ => "M",
            };
            serde_json::json!({ "path": f.path, "status": letter })
        })
        .collect();
    let diff_files: Vec<_> = parsed
        .iter()
        .map(|f| diff_server::file_metadata_json(f, false))
        .collect();
    diff_server::ok_json(serde_json::json!({
        "hash": hash,
        "full_hash": meta.full_hash,
        "author": meta.author,
        "author_email": meta.author_email,
        "date": meta.date,
        "message": meta.message,
        "files": files,
        "diff_files": diff_files,
    }))
}

pub(crate) async fn handle_api_commit_file(
    State(state): State<Arc<GargoServerState>>,
    AxumPath(hash): AxumPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(hash) = parse_commit_hash(&hash) else {
        return diff_server::bad_request("invalid commit hash");
    };
    let Some(path_raw) = params.get("path") else {
        return diff_server::bad_request("missing `path` query parameter");
    };
    let Some(path) = diff_server::parse_diff_path(path_raw) else {
        return diff_server::bad_request("invalid path");
    };
    // In-process gix diff for just this file (no `git show -- <path>` subprocess).
    let repo_root = state.repo_root.clone();
    let hash_c = hash.clone();
    let path_c = path.clone();
    let diff = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::commit_diff_text(&repo_root, &hash_c, Some(&path_c))
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    match parse_unified_diff(&diff).into_iter().next() {
        Some(file) => diff_server::ok_json(serde_json::json!({
            "path": file.path,
            "status": file.status.as_str(),
            "additions": file.additions,
            "deletions": file.deletions,
            "binary": file.binary,
            "html": diff_server::render_highlighted(&file),
        })),
        None => diff_server::ok_json(serde_json::json!({
            "path": path,
            "status": "modified",
            "additions": 0,
            "deletions": 0,
            "binary": false,
            "html": diff_server::empty_diff_html(),
        })),
    }
}

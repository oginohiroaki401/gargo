//! HTTP endpoints for the browser editor.
//!
//! Serves the editor shell + assets and provides file read/write with
//! VSCode-style conflict detection: the client reads a file (`/api/file`),
//! edits locally in wasm, then saves (`/api/save`) sending the hash it loaded.
//! If the on-disk content changed since (hash mismatch) the save is rejected
//! with `409 Conflict` so the client can warn before overwriting.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::{
    extract::{Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};

use crate::command::github_server::GithubServerState;

const EDITOR_HTML: &str = include_str!("../../assets/web_editor/index.html");
const EDITOR_JS: &str = include_str!("../../assets/web_editor/editor.js");

/// Directory holding the wasm-bindgen output, relative to the crate root.
/// Build it with:
///   cargo build --lib --target wasm32-unknown-unknown --release
///   wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
///     --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web
fn pkg_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/web_editor/pkg")
}

pub(crate) async fn handle_edit_page() -> Html<&'static str> {
    Html(EDITOR_HTML)
}

pub(crate) async fn handle_editor_js() -> Response {
    js_response(EDITOR_JS.to_string())
}

pub(crate) async fn handle_wasm_js() -> Response {
    match std::fs::read_to_string(pkg_dir().join("gargo_wasm.js")) {
        Ok(body) => js_response(body),
        Err(_) => wasm_not_built(),
    }
}

pub(crate) async fn handle_wasm_binary() -> Response {
    match std::fs::read(pkg_dir().join("gargo_wasm_bg.wasm")) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/wasm")], bytes).into_response(),
        Err(_) => wasm_not_built(),
    }
}

#[derive(Deserialize)]
pub(crate) struct FileQuery {
    path: String,
}

#[derive(Serialize)]
struct FileResponse {
    path: String,
    content: String,
    mtime: u64,
    hash: String,
}

pub(crate) async fn handle_api_file(
    State(state): State<Arc<GithubServerState>>,
    Query(q): Query<FileQuery>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &q.path) else {
        return bad_request("invalid path");
    };
    match std::fs::read(&full) {
        Ok(bytes) => {
            let content = String::from_utf8_lossy(&bytes).into_owned();
            ok_json(&FileResponse {
                path: q.path,
                content,
                mtime: mtime_ms(&full),
                hash: hash_bytes(&bytes),
            })
        }
        Err(e) => bad_request(format!("cannot read file: {e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct SaveRequest {
    path: String,
    /// Hash the client loaded; empty for a brand-new file.
    base_hash: String,
    content: String,
}

#[derive(Serialize)]
struct SaveResponse {
    saved: bool,
    mtime: u64,
    hash: String,
}

#[derive(Serialize)]
struct ConflictResponse {
    conflict: bool,
    current_mtime: u64,
    current_hash: String,
}

pub(crate) async fn handle_api_save(
    State(state): State<Arc<GithubServerState>>,
    Json(req): Json<SaveRequest>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };

    // Conflict detection: if the file exists and its current content differs
    // from what the client loaded, refuse to overwrite.
    if let Ok(current) = std::fs::read(&full) {
        let current_hash = hash_bytes(&current);
        if !req.base_hash.is_empty() && current_hash != req.base_hash {
            let payload = ConflictResponse {
                conflict: true,
                current_mtime: mtime_ms(&full),
                current_hash,
            };
            let mut resp = (StatusCode::CONFLICT, Json(payload)).into_response();
            no_store(&mut resp);
            return resp;
        }
    }

    if let Some(parent) = full.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return bad_request(format!("cannot create parent dir: {e}"));
    }

    match std::fs::write(&full, req.content.as_bytes()) {
        Ok(_) => ok_json(&SaveResponse {
            saved: true,
            mtime: mtime_ms(&full),
            hash: hash_bytes(req.content.as_bytes()),
        }),
        Err(e) => bad_request(format!("cannot write file: {e}")),
    }
}

/// Resolve a client-supplied relative path within `repo_root`, rejecting
/// absolute paths and any `..` traversal.
fn resolve_in_repo(repo_root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches('/');
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return None;
    }
    for comp in candidate.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            _ => return None, // ParentDir / RootDir / Prefix
        }
    }
    let full = repo_root.join(candidate);
    if full.starts_with(repo_root) {
        Some(full)
    } else {
        None
    }
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn js_response(body: String) -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        body,
    )
        .into_response()
}

fn wasm_not_built() -> Response {
    (
        StatusCode::NOT_FOUND,
        "wasm bundle not built. Run: cargo build --lib --target wasm32-unknown-unknown --release \
         && wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
         --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web",
    )
        .into_response()
}

fn ok_json<T: Serialize>(payload: &T) -> Response {
    let mut resp = Json(payload).into_response();
    no_store(&mut resp);
    resp
}

fn bad_request(message: impl Into<String>) -> Response {
    let mut resp = (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": message.into() })),
    )
        .into_response();
    no_store(&mut resp);
    resp
}

fn no_store(resp: &mut Response) {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
}

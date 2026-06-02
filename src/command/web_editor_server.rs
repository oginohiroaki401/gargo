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

/// The browser editor: an emacs/VSCode-style always-insert editor whose modal
/// core runs in-tab as wasm. The page template carries `{{APP_CSS}}` and
/// `{{APP_RAIL}}` slots so it shows the same top nav as the rest of the server.
const EDITOR_HTML: &str = include_str!("../../assets/web_editor/editor.html");

/// Directory holding the wasm-bindgen output, relative to the crate root.
/// Build it with:
///   cargo build --lib --target wasm32-unknown-unknown --release
///   wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
///     --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web
fn pkg_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/web_editor/pkg")
}

pub(crate) async fn handle_editor_page(
    State(state): State<Arc<GithubServerState>>,
) -> Html<String> {
    let rail = crate::command::app_shell::app_rail_html(&state.url_ctx, None, "editor");
    let css = format!(
        "<style>\n{}</style>",
        crate::command::server_shared::SHARED_CSS
    );
    let page = EDITOR_HTML
        .replace("{{APP_CSS}}", &css)
        .replace("{{APP_RAIL}}", &rail);
    Html(page)
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

#[derive(Serialize)]
struct FilesResponse {
    files: Vec<String>,
}

/// List the repository's files for the editor's Cmd+P picker — the same set the
/// terminal file picker uses (`git ls-files` when in a repo, else a filtered
/// directory walk; see [`crate::project::collect_files`]).
pub(crate) async fn handle_api_files(State(state): State<Arc<GithubServerState>>) -> Response {
    let files = crate::project::collect_files(&state.repo_root);
    ok_json(&FilesResponse { files })
}

#[derive(Deserialize)]
pub(crate) struct HighlightRequest {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct HighlightSpanDto {
    /// Character offset within the tab-expanded line (matches the strings the
    /// wasm renderer produces, so the client can wrap substrings directly).
    start: usize,
    end: usize,
    /// Top-level capture category (e.g. "keyword", "string"), → CSS `tok-*`.
    scope: String,
}

#[derive(Serialize)]
struct HighlightResponse {
    /// Per-line spans keyed by line index (as a string, JSON object key).
    lines: std::collections::HashMap<String, Vec<HighlightSpanDto>>,
}

/// Compute tree-sitter highlight spans for `content` (language inferred from
/// `path`'s extension). Spans are byte ranges within each line from the syntax
/// layer; we convert them to character offsets into the tab-expanded line so
/// the browser can color substrings of the rows it already renders. Returns an
/// empty map for unknown / unsupported languages.
pub(crate) async fn handle_api_highlight(Json(req): Json<HighlightRequest>) -> Response {
    use crate::syntax::language::LanguageRegistry;

    let registry = LanguageRegistry::new();
    let Some(lang_def) = registry.detect_by_extension(&req.path) else {
        return ok_json(&HighlightResponse {
            lines: std::collections::HashMap::new(),
        });
    };

    let by_line = crate::syntax::highlight::highlight_text(&req.content, lang_def);
    let line_texts: Vec<&str> = req.content.split('\n').collect();

    let mut lines = std::collections::HashMap::new();
    for (line_idx, spans) in by_line {
        let Some(text) = line_texts.get(line_idx) else {
            continue;
        };
        let dtos: Vec<HighlightSpanDto> = spans
            .into_iter()
            .map(|s| HighlightSpanDto {
                start: byte_to_expanded_col(text, s.start),
                end: byte_to_expanded_col(text, s.end),
                scope: capture_to_scope(&s.capture_name).to_string(),
            })
            .filter(|s| s.start < s.end)
            .collect();
        if !dtos.is_empty() {
            lines.insert(line_idx.to_string(), dtos);
        }
    }

    ok_json(&HighlightResponse { lines })
}

/// Map a tree-sitter capture name to the CSS `tok-*` scope the editor styles.
///
/// Most grammars use dotted names whose first segment is the scope we want
/// (`keyword.control` → `keyword`). Markdown (tree-sitter-md) instead emits
/// `text.*` names whose first segment (`text`) has no style, so headings, code
/// and links rendered uncolored. Map those to the existing token classes.
fn capture_to_scope(capture_name: &str) -> &str {
    match capture_name {
        "text.title" => "title",
        "text.literal" => "string", // code spans / fenced & indented code blocks
        "text.uri" | "text.reference" => "link",
        "text.emphasis" => "emphasis",
        "text.strong" => "strong",
        _ => capture_name.split('.').next().unwrap_or(""),
    }
}

/// Map a byte offset within `line` to a character offset in the tab-expanded
/// rendering of that line (each tab → 4 chars, every other char → 1), matching
/// the wasm renderer's `expand_tabs`.
fn byte_to_expanded_col(line: &str, byte_off: usize) -> usize {
    const TAB: usize = 4;
    let mut col = 0;
    let mut b = 0;
    for ch in line.chars() {
        if b >= byte_off {
            break;
        }
        b += ch.len_utf8();
        col += if ch == '\t' { TAB } else { 1 };
    }
    col
}

#[derive(Deserialize)]
pub(crate) struct GitGutterRequest {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct GitGutterResponse {
    /// Per-line git status keyed by 0-based line index (as a string JSON key):
    /// `"added"`, `"modified"`, or `"deleted"`. Matches `/api/highlight`'s
    /// string-keyed-map shape so the client parses both the same way.
    lines: std::collections::HashMap<String, String>,
}

/// Compute the git change gutter for `content` (line-level diff against `HEAD`).
/// Mirrors `/api/highlight`: the client posts the live editor content so the
/// gutter updates as you type. The diff backend (`gix`) is native-only — the
/// browser's wasm core can't compute it — so it must run here on the server.
/// Returns an empty map outside a repo or when nothing changed.
pub(crate) async fn handle_api_git_gutter(
    State(state): State<Arc<GithubServerState>>,
    Json(req): Json<GitGutterRequest>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    let status = crate::command::git_backend::diff_line_status_for_content(&full, &req.content);
    let lines = status
        .into_iter()
        .map(|(line, st)| {
            let label = match st {
                crate::command::git::GitLineStatus::Added => "added",
                crate::command::git::GitLineStatus::Modified => "modified",
                crate::command::git::GitLineStatus::Deleted => "deleted",
            };
            (line.to_string(), label.to_string())
        })
        .collect();
    ok_json(&GitGutterResponse { lines })
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

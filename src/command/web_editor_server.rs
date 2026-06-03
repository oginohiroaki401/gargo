//! HTTP endpoints for the browser editor.
//!
//! Serves the editor shell + assets and provides file read/write with
//! VSCode-style conflict detection: the client reads a file (`/api/file`),
//! edits locally in wasm, then saves (`/api/save`) sending the hash it loaded.
//! If the on-disk content changed since (hash mismatch) the save is rejected
//! with `409 Conflict` so the client can warn before overwriting.
//!
//! Scope note: this module owns file read/write, project search, and the
//! `/api/fs/*` filesystem operations. The git status / stage / unstage / commit
//! endpoints (`/api/status*`) live in [`crate::command::diff_server`], which
//! backs the separate status page the editor links to.

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

use crate::command::gargo_server::GargoServerState;

/// The browser editor: an emacs/VSCode-style always-insert editor whose modal
/// core runs in-tab as wasm. The page template carries `{{APP_CSS}}` and
/// `{{APP_RAIL}}` slots so it shows the same top nav as the rest of the server,
/// plus `{{EDITOR_CSS}}`/`{{EDITOR_JS}}` slots filled from the sibling files
/// below (kept separate from the HTML for maintainability; still embedded so the
/// page is served as one self-contained document with no extra requests).
const EDITOR_HTML: &str = include_str!("../../assets/web_editor/editor.html");
const EDITOR_CSS: &str = include_str!("../../assets/web_editor/editor.css");
const EDITOR_JS: &str = include_str!("../../assets/web_editor/editor.js");

/// The wasm-bindgen output, embedded at compile time so `gargo` is a single
/// self-contained binary (the editor then survives `gargo --update`, which
/// replaces only the executable). `build.rs` stages these out of
/// `assets/web_editor/pkg/` into `OUT_DIR`; when the bundle hasn't been built it
/// stages empty placeholders, so an empty value here means "wasm not built".
/// Build the bundle with:
///   cargo build --lib --target wasm32-unknown-unknown --release
///   wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
///     --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web
const WASM_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/gargo_wasm.js"));
const WASM_BG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/gargo_wasm_bg.wasm"));

pub(crate) async fn handle_editor_page(State(state): State<Arc<GargoServerState>>) -> Html<String> {
    let rail = crate::command::app_shell::app_rail_html(&state.url_ctx, None, "editor");
    let css = format!(
        "<style>\n{}</style>",
        crate::command::server_shared::SHARED_CSS
    );
    // Syntax + chrome colors come from `[theme.editor]` in the user's config,
    // mirroring the terminal editor's `[theme]`. Defaults to a light palette.
    let config = crate::config::Config::load();
    let theme_css = crate::command::web_editor_theme::editor_theme_css(&config.theme);
    // Default soft-wrap state (`[theme.editor] wrap`); the client persists a
    // per-tab override in localStorage on top of this.
    let wrap_default = if config.theme.editor.wrap {
        "true"
    } else {
        "false"
    };
    // The repo root, JSON-encoded, so the client can build absolute paths for
    // "Copy Path" in the sidebar context menu. `to_string` yields a quoted,
    // escaped JS string literal we drop straight into the inline script.
    let repo_root = serde_json::to_string(&state.repo_root.to_string_lossy())
        .unwrap_or_else(|_| "\"\"".to_string());
    let page = EDITOR_HTML
        .replace("{{EDITOR_CSS}}", EDITOR_CSS)
        .replace("{{EDITOR_JS}}", EDITOR_JS)
        .replace("{{APP_CSS}}", &css)
        .replace("{{APP_RAIL}}", &rail)
        .replace("{{THEME_CSS}}", &theme_css)
        .replace("{{REPO_ROOT}}", &repo_root)
        .replace("{{EDITOR_WRAP}}", wrap_default);
    Html(page)
}

pub(crate) async fn handle_wasm_js() -> Response {
    if WASM_BG.is_empty() {
        return wasm_not_built();
    }
    js_response(WASM_JS.to_string())
}

pub(crate) async fn handle_wasm_binary() -> Response {
    if WASM_BG.is_empty() {
        return wasm_not_built();
    }
    ([(header::CONTENT_TYPE, "application/wasm")], WASM_BG).into_response()
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
    State(state): State<Arc<GargoServerState>>,
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
pub(crate) async fn handle_api_files(State(state): State<Arc<GargoServerState>>) -> Response {
    let files = crate::project::collect_files(&state.repo_root);
    ok_json(&FilesResponse { files })
}

#[derive(Serialize)]
struct GitStatusResponse {
    /// Repo-relative path -> working-tree git status: `"modified"`, `"added"`,
    /// `"untracked"`, `"deleted"`, or `"conflict"`. Only changed paths appear;
    /// the client treats absent paths as clean.
    statuses: std::collections::HashMap<String, String>,
}

/// Working-tree git status per file, for the editor sidebar's change decorations.
/// Mirrors the terminal status colors via [`crate::command::git_backend::status_map`]
/// (`gix`, native-only). Returns an empty map outside a repo or with no changes.
pub(crate) async fn handle_api_git_status(State(state): State<Arc<GargoServerState>>) -> Response {
    let statuses = crate::command::git_backend::status_map(&state.repo_root)
        .into_iter()
        .map(|(path, st)| {
            let label = match st {
                crate::command::git::GitFileStatus::Modified => "modified",
                crate::command::git::GitFileStatus::Added => "added",
                crate::command::git::GitFileStatus::Untracked => "untracked",
                crate::command::git::GitFileStatus::Deleted => "deleted",
                crate::command::git::GitFileStatus::Conflict => "conflict",
            };
            (path, label.to_string())
        })
        .collect();
    ok_json(&GitStatusResponse { statuses })
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    q: String,
    /// Max hits to return; 0 (the default) means "use the server default".
    #[serde(default)]
    max: usize,
}

#[derive(Serialize)]
struct SearchHitDto {
    /// Repo-relative path, as the editor opens files (`/editor/<path>`).
    path: String,
    /// 0-based line index of the match.
    line: usize,
    /// 0-based character column where the match starts.
    col: usize,
    /// The full matched line (trimmed of trailing whitespace).
    excerpt: String,
}

#[derive(Serialize)]
struct SearchResponse {
    hits: Vec<SearchHitDto>,
    /// True when more hits existed than `max` (results were capped).
    truncated: bool,
}

/// Project-wide text search for the editor's Cmd+Shift+F overlay. Reuses the
/// trigram-indexed backend ([`crate::command::global_search_index::search_repo`]):
/// case-insensitive literal substring, `.gitignore`-aware, 3-char minimum
/// (shorter queries return no hits). Results arrive sorted by path so the
/// client can group them by file.
pub(crate) async fn handle_api_search(
    State(state): State<Arc<GargoServerState>>,
    Query(q): Query<SearchQuery>,
) -> Response {
    const DEFAULT_MAX: usize = 500;
    const HARD_MAX: usize = 1000;
    // Cap matches per file so one match-heavy file can't consume the whole
    // budget and hide other files (a common term like `test` otherwise stops
    // after the first few files). Files with more get the per-file cap shown.
    const PER_FILE_MAX: usize = 50;
    let max = if q.max == 0 { DEFAULT_MAX } else { q.max }.min(HARD_MAX);

    let repo = crate::command::global_search_index::GlobalIndexedRepo {
        root: state.repo_root.clone(),
        display_name: String::new(),
    };
    // Ask for one extra so we can tell whether results were truncated.
    let mut hits = crate::command::global_search_index::search_repo_limited(
        &repo,
        &q.q,
        max + 1,
        PER_FILE_MAX,
    );
    let truncated = hits.len() > max;
    hits.truncate(max);

    let hits = hits
        .into_iter()
        .map(|h| SearchHitDto {
            path: h.rel_path,
            line: h.line,
            col: h.char_col,
            excerpt: h.excerpt,
        })
        .collect();

    ok_json(&SearchResponse { hits, truncated })
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

#[derive(Deserialize)]
pub(crate) struct PreviewRequest {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct PreviewResponse {
    /// `"markdown"` | `"html"` | `"none"` — tells the client how to treat `html`.
    kind: String,
    /// Rendered HTML (markdown → HTML), or the raw file content for HTML files.
    html: String,
}

/// Render a Markdown or HTML file for the editor's split preview pane. Markdown
/// reuses the GitHub preview server's comrak config ([`render_markdown`]) so the
/// output (GFM tables, task lists, mermaid blocks) matches the blob view; HTML
/// files are returned verbatim since the file *is* the document. The client
/// wraps the result in a sandboxed iframe (markdown gets a styled `markdown-body`
/// document; HTML is shown as-is).
///
/// Relative links/images are intentionally left unresolved — the editor has no
/// repo-blob URL context like the preview server, and a preview pane doesn't need
/// working navigation.
pub(crate) async fn handle_api_preview(Json(req): Json<PreviewRequest>) -> Response {
    let ext = Path::new(&req.path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let (kind, html) = match ext.as_str() {
        "md" | "markdown" => (
            "markdown",
            crate::command::gargo_preview_server::render_markdown(&req.content),
        ),
        "html" | "htm" => ("html", req.content),
        _ => ("none", String::new()),
    };
    ok_json(&PreviewResponse {
        kind: kind.to_string(),
        html,
    })
}

#[derive(Deserialize)]
pub(crate) struct SymbolsRequest {
    path: String,
    content: String,
}

#[derive(Serialize)]
struct SymbolDto {
    /// Symbol name (function/class/method/heading/…).
    name: String,
    /// Capture kind (`function`, `class`, `section`, …) → shown as a hint.
    kind: String,
    /// 0-based line of the symbol's name.
    line: usize,
    /// 0-based character column of the symbol's name.
    col: usize,
}

#[derive(Serialize)]
struct SymbolsResponse {
    symbols: Vec<SymbolDto>,
}

/// Extract the document's symbol outline (functions, types, headings, …) for the
/// editor's `@` palette ("Go to Symbol in File"). Mirrors `/api/highlight`:
/// language is inferred from `path`'s extension and the tree-sitter tags query
/// runs server-side (it can't run in the browser's wasm core). Returns an empty
/// list for unknown / unsupported languages.
pub(crate) async fn handle_api_symbols(Json(req): Json<SymbolsRequest>) -> Response {
    use crate::syntax::language::LanguageRegistry;

    let registry = LanguageRegistry::new();
    let Some(lang_def) = registry.detect_by_extension(&req.path) else {
        return ok_json(&SymbolsResponse { symbols: vec![] });
    };

    let symbols = crate::syntax::symbol::extract_symbols(&req.content, lang_def)
        .into_iter()
        .map(|s| SymbolDto {
            name: s.name,
            kind: s.kind,
            line: s.line,
            col: s.char_col,
        })
        .collect();

    ok_json(&SymbolsResponse { symbols })
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
    State(state): State<Arc<GargoServerState>>,
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
    State(state): State<Arc<GargoServerState>>,
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

#[derive(Deserialize)]
pub(crate) struct CreateRequest {
    path: String,
    /// `"file"` or `"dir"`.
    kind: String,
}

/// Create an empty file (with any missing parent dirs) or a directory at a
/// repo-relative path, for the sidebar's "New File" / "New Folder" actions.
/// Refuses to clobber an existing entry.
pub(crate) async fn handle_api_fs_create(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<CreateRequest>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    if full.exists() {
        return bad_request("already exists");
    }
    let result = match req.kind.as_str() {
        "dir" => std::fs::create_dir_all(&full),
        "file" => {
            if let Some(parent) = full.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return bad_request(format!("cannot create parent dir: {e}"));
            }
            std::fs::write(&full, b"")
        }
        _ => return bad_request("invalid kind"),
    };
    match result {
        Ok(_) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => bad_request(format!("cannot create: {e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct RenameRequest {
    from: String,
    to: String,
}

/// Rename/move a file or directory within the repo (the sidebar's "Rename"
/// action). Refuses if the source is missing or the destination exists.
pub(crate) async fn handle_api_fs_rename(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<RenameRequest>,
) -> Response {
    let (Some(from), Some(to)) = (
        resolve_in_repo(&state.repo_root, &req.from),
        resolve_in_repo(&state.repo_root, &req.to),
    ) else {
        return bad_request("invalid path");
    };
    if !from.exists() {
        return bad_request("source does not exist");
    }
    if to.exists() {
        return bad_request("target already exists");
    }
    if let Some(parent) = to.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return bad_request(format!("cannot create parent dir: {e}"));
    }
    match std::fs::rename(&from, &to) {
        Ok(_) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => bad_request(format!("cannot rename: {e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct DeleteRequest {
    path: String,
}

/// Delete a file or directory (recursively) within the repo, for the sidebar's
/// "Delete" action. Refuses to delete the repo root itself.
pub(crate) async fn handle_api_fs_delete(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<DeleteRequest>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    if full == state.repo_root || req.path.trim_matches('/').is_empty() {
        return bad_request("refusing to delete the repo root");
    }
    let result = if full.is_dir() {
        std::fs::remove_dir_all(&full)
    } else {
        std::fs::remove_file(&full)
    };
    match result {
        Ok(_) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => bad_request(format!("cannot delete: {e}")),
    }
}

#[derive(Deserialize)]
pub(crate) struct RevealRequest {
    path: String,
}

/// Reveal a repo path in the host's file manager (macOS Finder, Windows
/// Explorer, or the containing dir via `xdg-open` elsewhere). Runs on the
/// machine hosting the server, which for the editor is the user's own box.
pub(crate) async fn handle_api_fs_reveal(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<RevealRequest>,
) -> Response {
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    if !full.exists() {
        return bad_request("path does not exist");
    }
    match reveal_in_file_manager(&full) {
        Ok(_) => ok_json(&serde_json::json!({ "ok": true })),
        Err(e) => bad_request(format!("cannot reveal: {e}")),
    }
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "windows")]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
fn reveal_in_file_manager(path: &Path) -> std::io::Result<()> {
    // No portable "reveal" on Linux desktops; open the containing directory.
    let target = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
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

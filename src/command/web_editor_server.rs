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
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use axum::{
    extract::{Query, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};

use crate::command::gargo_server::{
    ChildGargoServer, FileEntry, GargoServerCommand, GargoServerEvent, GargoServerHandle,
    GargoServerState,
};

/// The browser application: a keyboard-driven code and Git browser. Its Explorer
/// editor and the read-only previews used by History, Compare, and Status share
/// one client-side code-surface implementation. Assets stay embedded so `gargo`
/// remains a self-contained binary.
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

pub(crate) async fn handle_editor_page() -> Html<String> {
    let config = crate::config::Config::load();
    let theme_css = crate::command::web_editor_theme::editor_theme_css(&config.theme);
    let diff_css = crate::diff_render::render_diff_styles();
    let page = EDITOR_HTML
        .replace("{{EDITOR_CSS}}", EDITOR_CSS)
        .replace("{{EDITOR_JS}}", EDITOR_JS)
        .replace("{{THEME_CSS}}", &theme_css)
        .replace("{{DIFF_CSS}}", diff_css);
    Html(page)
}

pub(crate) async fn handle_wasm_js(headers: axum::http::HeaderMap) -> Response {
    if WASM_BG.is_empty() {
        return wasm_not_built();
    }
    cached_asset_response(
        &headers,
        wasm_js_etag(),
        "text/javascript; charset=utf-8",
        WASM_JS.as_bytes(),
    )
}

pub(crate) async fn handle_wasm_binary(headers: axum::http::HeaderMap) -> Response {
    if WASM_BG.is_empty() {
        return wasm_not_built();
    }
    cached_asset_response(&headers, wasm_bg_etag(), "application/wasm", WASM_BG)
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
            // Record the open in the shared history so the Cmd+P picker's recency
            // sort sees it. Best-effort and off the response path (opens a SQLite
            // connection), so detach it.
            let root = state.repo_root.clone();
            let opened = full.clone();
            tokio::task::spawn_blocking(move || {
                let _ = crate::command::recent_projects::RecentProjectsStore::new()
                    .record_file_open(&root, &opened);
            });
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
    entries: Vec<FileEntryResponse>,
    ready: bool,
    search_ready: bool,
    truncated: bool,
    total: usize,
    next_offset: Option<usize>,
}

#[derive(Serialize)]
struct FileEntryResponse {
    path: String,
    mtime: u64,
    changed: bool,
    /// Last time the file was opened in gargo (CLI or web editor), ms since the
    /// epoch; 0 if never opened. The picker sorts the empty query by
    /// `max(mtime, opened)` descending.
    opened: u64,
}

#[derive(Deserialize, Default)]
pub(crate) struct FilesQuery {
    offset: Option<usize>,
    limit: Option<usize>,
    /// When set, force a fresh workspace rescan before paging so files created
    /// outside the editor (by the agent or the terminal) appear. The picker
    /// sends this on open; subsequent pagination requests omit it.
    refresh: Option<u8>,
}

/// Schedule a shared index refresh after an in-editor filesystem change.
fn bump_fs_generation(state: &GargoServerState) {
    state
        .fs_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.workspace_index.request_refresh();
}

/// List the repository's files for the editor's Cmd+P picker — the same set the
/// terminal file picker uses (`git ls-files` when in a repo, else a filtered
/// directory walk; see [`crate::project::collect_files`]).
///
/// The shared index publishes partial batches while it scans. New clients page
/// through those batches; legacy callers without pagination wait briefly for a
/// complete snapshot and retain the old all-files response shape.
pub(crate) async fn handle_api_files(
    State(state): State<Arc<GargoServerState>>,
    Query(query): Query<FilesQuery>,
) -> Response {
    if query.refresh.unwrap_or(0) != 0 {
        // Trigger a rescan and wait until a generation newer than the one we
        // started from has been published, so the response reflects files
        // created since the last scan. Bounded so a huge repo can't hang the
        // request — the picker keeps showing its cached list until this returns.
        let before = state.workspace_index.generation();
        state.workspace_index.request_refresh();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (state.workspace_index.generation() == before || !state.workspace_index.is_ready())
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    } else if query.limit.is_none() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !state.workspace_index.is_ready() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    let page = state
        .workspace_index
        .page(query.offset.unwrap_or(0), query.limit);
    files_response(
        page.entries,
        page.ready,
        page.search_ready,
        page.truncated,
        page.total,
        page.next_offset,
    )
}

fn files_response(
    entries: Vec<FileEntry>,
    ready: bool,
    search_ready: bool,
    truncated: bool,
    total: usize,
    next_offset: Option<usize>,
) -> Response {
    let files = entries.iter().map(|entry| entry.0.clone()).collect();
    let entries = entries
        .into_iter()
        .map(|(path, mtime, changed, opened)| FileEntryResponse {
            path,
            mtime,
            changed,
            opened,
        })
        .collect();
    ok_json(&FilesResponse {
        files,
        entries,
        ready,
        search_ready,
        truncated,
        total,
        next_offset,
    })
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
    // `status_map` scans the working tree via `gix` (blocking I/O) — keep it off
    // the async worker thread.
    let repo_root = state.repo_root.clone();
    let statuses = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::status_map(&repo_root)
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
            .collect::<std::collections::HashMap<String, String>>()
    })
    .await
    .unwrap_or_default();
    ok_json(&GitStatusResponse { statuses })
}

#[derive(Serialize)]
struct RepoInfoResponse {
    /// GitHub owner (or `local` when there is no GitHub remote).
    owner: String,
    /// Repo name (from the remote, else the working-tree folder name).
    repo: String,
    /// Current branch (or short hash when detached).
    branch: String,
    /// Default branch (`main`/`master`/origin HEAD), when resolvable.
    default_branch: Option<String>,
    /// Normalized `https://github.com/owner/repo` remote, when present.
    remote_url: Option<String>,
    /// Absolute repo root, for copy-absolute-path actions.
    root: String,
    /// Running gargo version, for the header version label.
    version: String,
    git: bool,
}

/// Repository identity for the editor header and the file "open" menu: owner,
/// repo, current/default branch, and the GitHub remote URL (used to build
/// `…/blob/<branch>/<path>` links). Wraps [`resolve_page_context`], which caches
/// the remote/default-branch git lookups per repo root.
pub(crate) async fn handle_api_repo_info(State(state): State<Arc<GargoServerState>>) -> Response {
    let (ctx, remote_url, default_branch) =
        crate::command::gargo_preview_server::resolve_page_context(&state.repo_root).await;
    ok_json(&RepoInfoResponse {
        owner: ctx.owner,
        repo: ctx.repo,
        branch: ctx.branch,
        default_branch,
        remote_url,
        root: state.repo_root.display().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git: state.workspace_index.is_git(),
    })
}

#[derive(Serialize)]
struct UpdateCheckResponse {
    /// Running version.
    current: String,
    /// Latest released version, when the check succeeded.
    latest: Option<String>,
    /// True when a newer release is available.
    has_update: bool,
}

/// Lightweight update probe for the header's version badge. Runs the same
/// GitHub release check as `gargo --check` on the blocking pool so it never
/// stalls the async runtime. Network/parse failures degrade to
/// `has_update: false` (the badge simply stays hidden) rather than erroring.
pub(crate) async fn handle_api_update_check(
    State(_state): State<Arc<GargoServerState>>,
) -> Response {
    let status = tokio::task::spawn_blocking(crate::upgrade::check_status).await;
    let current = env!("CARGO_PKG_VERSION").to_string();
    let body = match status {
        Ok(Ok(status)) => UpdateCheckResponse {
            current: status.current_version().to_string(),
            latest: Some(status.latest_version().to_string()),
            has_update: status.has_update(),
        },
        _ => UpdateCheckResponse {
            current,
            latest: None,
            has_update: false,
        },
    };
    ok_json(&body)
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
    indexing: bool,
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

    if !state.workspace_index.is_search_ready() {
        return ok_json(&SearchResponse {
            hits: Vec::new(),
            truncated: false,
            indexing: true,
        });
    }

    let repo = crate::command::global_search_index::GlobalIndexedRepo {
        root: state.repo_root.clone(),
        display_name: String::new(),
    };
    let query = q.q;
    let generation = state
        .search_generation
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
        + 1;
    let search_state = state.clone();
    let mut hits = tokio::task::spawn_blocking(move || {
        crate::command::global_search_index::search_repo_cached_limited_cancelled(
            &repo,
            &query,
            max + 1,
            PER_FILE_MAX,
            || {
                search_state
                    .search_generation
                    .load(std::sync::atomic::Ordering::Acquire)
                    != generation
            },
        )
    })
    .await
    .unwrap_or_default();
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

    ok_json(&SearchResponse {
        hits,
        truncated,
        indexing: false,
    })
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
    // Tree-sitter parse + query is CPU-bound and would block the async worker
    // thread (the editor highlights fresh content per keystroke, so the internal
    // cache mostly misses). Offload it to the blocking pool.
    let HighlightRequest { path, content } = req;
    let lines = tokio::task::spawn_blocking(move || {
        use crate::syntax::language::LanguageRegistry;

        let registry = LanguageRegistry::new();
        let Some(lang_def) = registry.detect_by_extension(&path) else {
            return std::collections::HashMap::new();
        };

        let by_line = crate::syntax::highlight::highlight_text(&content, lang_def);
        let line_texts: Vec<&str> = content.split('\n').collect();

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
        lines
    })
    .await
    .unwrap_or_default();

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
/// Relative links/images are left unresolved in the HTML — the editor has no
/// repo-blob URL context like the preview server. Instead the client intercepts
/// clicks on the rendered links and resolves relative targets against the open
/// file, opening them in the same preview pane (see `navigateEditorLink`).
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
    // Tree-sitter parse + tags query is CPU-bound — offload off the async worker.
    let SymbolsRequest { path, content } = req;
    let symbols = tokio::task::spawn_blocking(move || {
        use crate::syntax::language::LanguageRegistry;

        let registry = LanguageRegistry::new();
        let Some(lang_def) = registry.detect_by_extension(&path) else {
            return Vec::new();
        };

        crate::syntax::symbol::extract_symbols(&content, lang_def)
            .into_iter()
            .map(|s| SymbolDto {
                name: s.name,
                kind: s.kind,
                line: s.line,
                col: s.char_col,
            })
            .collect()
    })
    .await
    .unwrap_or_default();

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
    if !state.workspace_index.is_git() {
        return ok_json(&GitGutterResponse {
            lines: std::collections::HashMap::new(),
        });
    }
    let Some(full) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    // The `gix` line-diff is blocking — keep it off the async worker thread.
    let content = req.content;
    let lines = tokio::task::spawn_blocking(move || {
        crate::command::git_backend::diff_line_status_for_content(&full, &content)
            .into_iter()
            .map(|(line, st)| {
                let label = match st {
                    crate::command::git::GitLineStatus::Added => "added",
                    crate::command::git::GitLineStatus::Modified => "modified",
                    crate::command::git::GitLineStatus::Deleted => "deleted",
                };
                (line.to_string(), label.to_string())
            })
            .collect::<std::collections::HashMap<String, String>>()
    })
    .await
    .unwrap_or_default();
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

    // Saving a brand-new file adds it to the listing; an overwrite of an existing
    // file doesn't, so only invalidate the cache in the former case to avoid
    // thrashing it on ordinary saves.
    let is_new = !full.exists();
    match std::fs::write(&full, req.content.as_bytes()) {
        Ok(_) => {
            if is_new {
                bump_fs_generation(&state);
            }
            ok_json(&SaveResponse {
                saved: true,
                mtime: mtime_ms(&full),
                hash: hash_bytes(req.content.as_bytes()),
            })
        }
        Err(e) => bad_request(format!("cannot write file: {e}")),
    }
}

#[derive(Serialize)]
struct LastFileResponse {
    /// Repo-relative path of the last file opened in this repo, or `null` when
    /// nothing is recorded yet or the recorded file no longer exists.
    path: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct LastFileRequest {
    /// The file to remember, or `null` to forget the current record (used when
    /// an auto-reopened file turns out to be gone).
    path: Option<String>,
}

/// Return the last file opened in this repo, for a bare `/editor` to reopen.
///
/// The record is persisted server-side (keyed by repo root) rather than in the
/// browser's `localStorage`, because the server binds a fresh random port on
/// every start — and `localStorage` is per-origin, so a new port is a new
/// origin that can't see the previous session's record. We validate the stored
/// path still resolves inside the repo and exists before handing it back, so a
/// deleted/renamed file degrades to "no record" instead of a broken redirect.
pub(crate) async fn handle_api_last_file(State(state): State<Arc<GargoServerState>>) -> Response {
    let path = read_last_file_record(&state.repo_root)
        .filter(|rel| resolve_in_repo(&state.repo_root, rel).is_some_and(|full| full.is_file()));
    ok_json(&LastFileResponse { path })
}

/// Record (or, with a `null` path, forget) the last file opened in this repo.
pub(crate) async fn handle_api_last_file_set(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<LastFileRequest>,
) -> Response {
    // Only persist a path we can resolve inside the repo; reject traversal.
    let to_store = match req.path.as_deref() {
        Some(rel) if !rel.is_empty() => {
            if resolve_in_repo(&state.repo_root, rel).is_none() {
                return bad_request("invalid path");
            }
            Some(rel.to_string())
        }
        _ => None,
    };
    write_last_file_record(&state.repo_root, to_store.as_deref());
    ok_json(&LastFileResponse { path: to_store })
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
        Ok(_) => {
            bump_fs_generation(&state);
            ok_json(&serde_json::json!({ "ok": true }))
        }
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
        Ok(_) => {
            bump_fs_generation(&state);
            ok_json(&serde_json::json!({ "ok": true }))
        }
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
        Ok(_) => {
            bump_fs_generation(&state);
            ok_json(&serde_json::json!({ "ok": true }))
        }
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

#[derive(Deserialize)]
pub(crate) struct OpenServerRequest {
    path: String,
}

#[derive(Serialize)]
struct OpenServerResponse {
    url: String,
    root: String,
    reused: bool,
}

/// Start or reuse a gargo server rooted at a selected workspace directory.
pub(crate) async fn handle_api_server_open(
    State(state): State<Arc<GargoServerState>>,
    Json(req): Json<OpenServerRequest>,
) -> Response {
    let Some(candidate) = resolve_in_repo(&state.repo_root, &req.path) else {
        return bad_request("invalid path");
    };
    let Ok(candidate) = std::fs::canonicalize(candidate) else {
        return bad_request("path does not exist");
    };
    let canonical_root =
        std::fs::canonicalize(&state.repo_root).unwrap_or_else(|_| state.repo_root.clone());
    if !candidate.starts_with(&canonical_root) {
        return bad_request("path escapes server root");
    }
    if !candidate.is_dir() {
        return bad_request("path is not a directory");
    }

    let discovered_root = crate::project::find_project_root(Some(&candidate));
    let discovered_root = std::fs::canonicalize(&discovered_root).unwrap_or(discovered_root);
    let target_root = if discovered_root.starts_with(&canonical_root) {
        discovered_root
    } else {
        candidate
    };
    if target_root == canonical_root {
        return ok_json(&OpenServerResponse {
            url: format!("http://127.0.0.1:{}/", state.port),
            root: target_root.display().to_string(),
            reused: true,
        });
    }

    if let Ok(children) = state.child_servers.lock()
        && let Some(child) = children.get(&target_root)
    {
        return ok_json(&OpenServerResponse {
            url: child.url.clone(),
            root: target_root.display().to_string(),
            reused: true,
        });
    }

    let root_for_start = target_root.clone();
    let started =
        tokio::task::spawn_blocking(move || -> Result<(GargoServerHandle, String), String> {
            let handle = GargoServerHandle::new()?;
            handle
                .command_tx
                .send(GargoServerCommand::Start {
                    repo_root: root_for_start,
                    port: None,
                    host: None,
                })
                .map_err(|e| format!("failed to start child server: {e}"))?;
            match handle
                .event_rx
                .recv_timeout(std::time::Duration::from_secs(5))
            {
                Ok(GargoServerEvent::Started { root_url, .. }) => Ok((handle, root_url)),
                Ok(GargoServerEvent::Error(error)) => Err(error),
                Ok(event) => Err(format!("unexpected child server event: {event:?}")),
                Err(error) => Err(format!("child server did not start: {error}")),
            }
        })
        .await;

    let (handle, url) = match started {
        Ok(Ok(started)) => started,
        Ok(Err(error)) => return bad_request(error),
        Err(error) => return bad_request(format!("failed to join child server startup: {error}")),
    };

    let child = ChildGargoServer {
        url: url.clone(),
        _handle: handle,
    };
    if let Ok(mut children) = state.child_servers.lock() {
        if let Some(existing) = children.get(&target_root) {
            return ok_json(&OpenServerResponse {
                url: existing.url.clone(),
                root: target_root.display().to_string(),
                reused: true,
            });
        }
        children.insert(target_root.clone(), child);
    } else {
        return bad_request("failed to retain child server");
    }

    ok_json(&OpenServerResponse {
        url,
        root: target_root.display().to_string(),
        reused: false,
    })
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

/// Path of the shared JSON file mapping each repo root to its last-opened file.
/// One file under the app data dir keeps the records out of the repos themselves
/// and lets them survive server restarts (and the random port each start picks).
fn last_files_store_path() -> PathBuf {
    crate::config::app_data_dir().join("editor_last_files.json")
}

/// The map persisted in [`last_files_store_path`]: repo root (as a string) ->
/// last repo-relative file path. A plain map read-modify-written in full; the
/// editor is single-user so contention isn't a concern.
fn load_last_files_map() -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(last_files_store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn read_last_file_record(repo_root: &Path) -> Option<String> {
    let key = repo_root.to_string_lossy();
    load_last_files_map().remove(key.as_ref())
}

fn write_last_file_record(repo_root: &Path, rel_path: Option<&str>) {
    let key = repo_root.to_string_lossy().into_owned();
    let mut map = load_last_files_map();
    match rel_path {
        Some(p) => {
            map.insert(key, p.to_string());
        }
        None => {
            map.remove(&key);
        }
    }
    let path = last_files_store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&map) {
        let _ = std::fs::write(&path, json);
    }
}

/// ETag for the wasm-bindgen JS glue, derived from its bytes (stable per build,
/// changes when the bundle is rebuilt). Computed once.
fn wasm_js_etag() -> &'static str {
    static ETAG: OnceLock<String> = OnceLock::new();
    ETAG.get_or_init(|| format!("\"{}\"", hash_bytes(WASM_JS.as_bytes())))
}

/// ETag for the wasm binary, derived from its bytes. Computed once.
fn wasm_bg_etag() -> &'static str {
    static ETAG: OnceLock<String> = OnceLock::new();
    ETAG.get_or_init(|| format!("\"{}\"", hash_bytes(WASM_BG)))
}

/// Serve an immutable, embedded asset with an `ETag` so the browser can cache it
/// across editor opens. The bundle is fingerprinted by content, so a long-lived
/// `immutable` `Cache-Control` is safe — a rebuild changes the ETag and busts
/// the cache. Returns `304 Not Modified` when the client's `If-None-Match`
/// already carries this build's ETag, skipping the body entirely.
fn cached_asset_response(
    headers: &axum::http::HeaderMap,
    etag: &'static str,
    content_type: &'static str,
    body: &'static [u8],
) -> Response {
    if let Some(if_none_match) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && if_none_match.split(',').any(|tag| tag.trim() == etag)
    {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        set_immutable_cache(&mut resp, etag);
        return resp;
    }

    let mut resp = ([(header::CONTENT_TYPE, content_type)], body).into_response();
    set_immutable_cache(&mut resp, etag);
    resp
}

fn set_immutable_cache(resp: &mut Response, etag: &str) {
    let headers = resp.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if let Ok(value) = header::HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
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

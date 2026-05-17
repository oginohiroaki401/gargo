//! Unified local GitHub-style server for repository browsing, diffs, compares,
//! and commit history.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use tower_http::cors::CorsLayer;

use crate::command::diff_server::{self, DiffServerState};
use crate::command::github_preview_server::{
    self, GithubPreviewEvent, PreviewBrowserEvent, PreviewBrowserEventKind, PreviewServerState,
};
use crate::diff_render::{parse_unified_diff, render_diff_styles};

#[derive(Debug, Clone)]
pub enum GithubServerRoute {
    Root,
    Tree { path: String },
    Blob { path: String },
    Changes,
    Compare,
    Commits,
    Commit { hash: String },
}

#[derive(Debug, Clone)]
pub enum GithubServerCommand {
    Start { repo_root: PathBuf },
    Stop,
    OpenRoute { route: GithubServerRoute },
    SetActivePath { rel_path: Option<String> },
    RefreshActive,
    UpdateBufferContent { content: String, cursor_line: usize },
    UpdateCursorLine { line: usize },
}

#[derive(Debug, Clone)]
pub enum GithubServerEvent {
    Started { port: u16, root_url: String },
    Stopped,
    Detached { requested_path: String },
    Opened { url: String },
    Error(String),
}

pub struct GithubServerHandle {
    pub command_tx: mpsc::Sender<GithubServerCommand>,
    pub event_rx: mpsc::Receiver<GithubServerEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl GithubServerHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = GithubServerWorker {
            command_rx,
            event_tx,
            tokio_runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to build tokio runtime: {}", e))?,
            server_shutdown_tx: None,
            preview_state: None,
            pending_active_rel_path: None,
            port: None,
        };
        let worker_thread = thread::Builder::new()
            .name("github-server".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("Failed to spawn worker thread: {}", e))?;
        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }
}

struct GithubServerWorker {
    command_rx: mpsc::Receiver<GithubServerCommand>,
    event_tx: mpsc::Sender<GithubServerEvent>,
    tokio_runtime: tokio::runtime::Runtime,
    server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    preview_state: Option<Arc<Mutex<PreviewServerState>>>,
    pending_active_rel_path: Option<String>,
    port: Option<u16>,
}

impl GithubServerWorker {
    fn run(mut self) {
        loop {
            match self.command_rx.recv() {
                Ok(GithubServerCommand::Start { repo_root }) => self.handle_start(repo_root),
                Ok(GithubServerCommand::Stop) => self.handle_stop(),
                Ok(GithubServerCommand::OpenRoute { route }) => self.handle_open_route(route),
                Ok(GithubServerCommand::SetActivePath { rel_path }) => {
                    self.handle_set_active_path(rel_path)
                }
                Ok(GithubServerCommand::RefreshActive) => self.handle_refresh_active(),
                Ok(GithubServerCommand::UpdateBufferContent {
                    content,
                    cursor_line,
                }) => self.handle_update_buffer_content(content, cursor_line),
                Ok(GithubServerCommand::UpdateCursorLine { line }) => {
                    self.handle_update_cursor_line(line)
                }
                Err(_) => break,
            }
        }
    }

    fn handle_start(&mut self, repo_root: PathBuf) {
        if self.server_shutdown_tx.is_some() {
            let _ = self.event_tx.send(GithubServerEvent::Error(
                "Server already running".to_string(),
            ));
            return;
        }

        let listener = match self
            .tokio_runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        {
            Ok(listener) => listener,
            Err(err) => {
                let _ = self.event_tx.send(GithubServerEvent::Error(format!(
                    "Failed to bind Gargo server on localhost: {}",
                    err
                )));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(err) => {
                let _ = self.event_tx.send(GithubServerEvent::Error(format!(
                    "Failed to read Gargo server local address: {}",
                    err
                )));
                return;
            }
        };
        let repo_root = std::fs::canonicalize(&repo_root).unwrap_or(repo_root);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.server_shutdown_tx = Some(shutdown_tx);
        self.port = Some(port);

        let url_ctx = self
            .tokio_runtime
            .block_on(github_preview_server::resolve_repo_url_context(&repo_root));
        let root_url = format!(
            "http://127.0.0.1:{}{}",
            port,
            github_preview_server::repo_home_url(&url_ctx)
        );

        let bridge_tx = bridge_preview_events(self.event_tx.clone());
        let preview_state = Arc::new(Mutex::new(PreviewServerState {
            repo_root: repo_root.clone(),
            url_ctx: url_ctx.clone(),
            port,
            active_rel_path: self.pending_active_rel_path.clone(),
            detached: false,
            last_detach_path: None,
            version: 1,
            last_browser_event: Some(PreviewBrowserEvent {
                kind: PreviewBrowserEventKind::Navigate,
                path: None,
                url: Some(root_url.clone()),
                detached: false,
                version: 1,
                cursor_line: None,
            }),
            event_tx: bridge_tx,
            buffer_content: None,
            cursor_line: None,
        }));
        self.preview_state = Some(preview_state.clone());
        let diff_state = Arc::new(DiffServerState {
            project_root: repo_root.clone(),
        });
        let github_state = Arc::new(GithubServerState { repo_root, url_ctx });

        self.tokio_runtime.spawn(async move {
            run_server(
                listener,
                shutdown_rx,
                preview_state,
                diff_state,
                github_state,
            )
            .await;
        });
        let _ = self
            .event_tx
            .send(GithubServerEvent::Started { port, root_url });
    }

    fn handle_stop(&mut self) {
        if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
            let _ = shutdown_tx.send(());
            self.preview_state = None;
            self.port = None;
            let _ = self.event_tx.send(GithubServerEvent::Stopped);
        } else {
            let _ = self
                .event_tx
                .send(GithubServerEvent::Error("Server not running".to_string()));
        }
    }

    fn handle_open_route(&self, route: GithubServerRoute) {
        let Some(port) = self.port else {
            return;
        };
        let Some(state) = &self.preview_state else {
            return;
        };
        let Ok(state) = state.lock() else {
            return;
        };
        let path = route.path(&state.url_ctx);
        let _ = self.event_tx.send(GithubServerEvent::Opened {
            url: format!("http://127.0.0.1:{}{}", port, path),
        });
    }

    fn handle_set_active_path(&mut self, rel_path: Option<String>) {
        let normalized =
            rel_path.map(|p| github_preview_server::normalize_rel_path_for_compare(&p));
        self.pending_active_rel_path = normalized.clone();
        if let Some(state) = &self.preview_state
            && let Ok(mut state) = state.lock()
        {
            state.active_rel_path = normalized;
            state.detached = false;
            state.last_detach_path = None;
            state.buffer_content = None;
            state.cursor_line = None;
            state.version = state.version.wrapping_add(1);
            let event = PreviewBrowserEvent {
                kind: PreviewBrowserEventKind::Navigate,
                path: state.active_rel_path.clone(),
                url: Some(github_preview_server::preview_url_for_rel_path(
                    state.port,
                    &state.url_ctx,
                    state.active_rel_path.as_deref(),
                )),
                detached: state.detached,
                version: state.version,
                cursor_line: None,
            };
            github_preview_server::broadcast_preview_event(&mut state, event);
        }
    }

    fn handle_refresh_active(&mut self) {
        let Some(state) = &self.preview_state else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        if state.detached {
            return;
        }
        state.version = state.version.wrapping_add(1);
        let event = PreviewBrowserEvent {
            kind: PreviewBrowserEventKind::Refresh,
            path: state.active_rel_path.clone(),
            url: Some(github_preview_server::preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: state.cursor_line,
        };
        github_preview_server::broadcast_preview_event(&mut state, event);
    }

    fn handle_update_buffer_content(&mut self, content: String, cursor_line: usize) {
        let Some(state) = &self.preview_state else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        if state.detached {
            return;
        }
        state.buffer_content = Some(content);
        state.cursor_line = Some(cursor_line);
        state.version = state.version.wrapping_add(1);
        let event = PreviewBrowserEvent {
            kind: PreviewBrowserEventKind::Refresh,
            path: state.active_rel_path.clone(),
            url: Some(github_preview_server::preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: Some(cursor_line),
        };
        github_preview_server::broadcast_preview_event(&mut state, event);
    }

    fn handle_update_cursor_line(&mut self, line: usize) {
        let Some(state) = &self.preview_state else {
            return;
        };
        let Ok(mut state) = state.lock() else {
            return;
        };
        if state.detached {
            return;
        }
        state.cursor_line = Some(line);
        state.version = state.version.wrapping_add(1);
        let event = PreviewBrowserEvent {
            kind: PreviewBrowserEventKind::ScrollTo,
            path: state.active_rel_path.clone(),
            url: None,
            detached: state.detached,
            version: state.version,
            cursor_line: Some(line),
        };
        github_preview_server::broadcast_preview_event(&mut state, event);
    }
}

impl GithubServerRoute {
    fn path(&self, ctx: &github_preview_server::RepoUrlContext) -> String {
        match self {
            Self::Root => github_preview_server::repo_home_url(ctx),
            Self::Tree { path } => github_preview_server::tree_url(ctx, path),
            Self::Blob { path } => github_preview_server::blob_url(ctx, path),
            Self::Changes => "/status".to_string(),
            Self::Compare => "/branches".to_string(),
            Self::Commits => github_preview_server::commits_url(ctx),
            Self::Commit { hash } => github_preview_server::commit_url(ctx, hash),
        }
    }
}

fn bridge_preview_events(
    event_tx: mpsc::Sender<GithubServerEvent>,
) -> mpsc::Sender<GithubPreviewEvent> {
    let (tx, rx) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("github-server-preview-events".to_string())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    GithubPreviewEvent::Detached { requested_path } => {
                        let _ = event_tx.send(GithubServerEvent::Detached { requested_path });
                    }
                    GithubPreviewEvent::Error(msg) => {
                        let _ = event_tx.send(GithubServerEvent::Error(msg));
                    }
                    GithubPreviewEvent::Started { .. } | GithubPreviewEvent::Stopped => {}
                }
            }
        });
    tx
}

#[derive(Debug)]
struct GithubServerState {
    repo_root: PathBuf,
    url_ctx: github_preview_server::RepoUrlContext,
}

async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    preview_state: Arc<Mutex<PreviewServerState>>,
    diff_state: Arc<DiffServerState>,
    github_state: Arc<GithubServerState>,
) {
    // URLs mirror github.com: `/{owner}/{repo}/blob/{branch}/{path}` etc., so
    // swapping `http://127.0.0.1:PORT/` for `github.com/` yields the same page.
    // The dynamic `/{owner}/{repo}` pattern is 2 segments; static prefixes
    // (`/events`, `/assets`, `/status`, `/api`, ...) rank above it in axum's
    // router, so do not add new top-level 2-segment static routes.
    let preview_routes = Router::new()
        .route("/", get(github_preview_server::handle_bare_root))
        .route("/events", get(github_preview_server::handle_events))
        .route(
            "/assets/mermaid.min.js",
            get(github_preview_server::handle_mermaid_asset),
        )
        .route("/{owner}/{repo}", get(github_preview_server::handle_root))
        .route(
            "/{owner}/{repo}/tree/{*rest}",
            get(github_preview_server::handle_tree),
        )
        .route(
            "/{owner}/{repo}/blob/{*rest}",
            get(github_preview_server::handle_blob),
        )
        .route(
            "/{owner}/{repo}/preview/{*rest}",
            get(github_preview_server::handle_preview),
        )
        .with_state(preview_state);

    let diff_routes = Router::new()
        .route("/diff", get(diff_server::handle_html_request))
        .route("/changes", get(diff_server::handle_html_request))
        .route("/status", get(diff_server::handle_html_request))
        .route("/compare", get(diff_server::handle_compare_html_request))
        .route("/branches", get(diff_server::handle_compare_html_request))
        .route("/api/status", get(diff_server::handle_api_status_request))
        .route(
            "/api/status/file",
            get(diff_server::handle_api_status_file_request),
        )
        .route(
            "/api/branches",
            get(diff_server::handle_api_branches_request),
        )
        .route("/api/compare", get(diff_server::handle_api_compare_request))
        .route(
            "/api/compare/file",
            get(diff_server::handle_api_compare_file_request),
        )
        .with_state(diff_state);

    let github_routes = Router::new()
        .route("/{owner}/{repo}/commits", get(handle_commits_html))
        .route("/{owner}/{repo}/commits/{*branch}", get(handle_commits_html))
        .route("/{owner}/{repo}/commit/{hash}", get(handle_commit_html))
        .route("/api/tree/{*path}", get(handle_api_tree))
        .route("/api/blob/{*path}", get(handle_api_blob))
        .route("/api/commits", get(handle_api_commits))
        .route("/api/commit/{hash}", get(handle_api_commit))
        .route("/api/commit/{hash}/file", get(handle_api_commit_file))
        .with_state(github_state);

    let app = preview_routes
        .merge(diff_routes)
        .merge(github_routes)
        .layer(CorsLayer::permissive());
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

async fn handle_commits_html(State(state): State<Arc<GithubServerState>>) -> impl IntoResponse {
    let root = state.repo_root.display().to_string();
    let repo_url = github_preview_server::github_repo_url(&state.repo_root).await;
    let header = github_page_header(&root, "commits", repo_url.as_deref(), &state.url_ctx);
    let commit_prefix = github_preview_server::commit_url(&state.url_ctx, "");
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Commits</title>{css}</head><body>{header}<main class="commits-main"><section class="commits-section"><h1 class="commits-title">Commits</h1><div id="commits"><div class="loading">Loading commits...</div></div></section></main><script>
fetch('/api/commits', {{cache:'no-store'}}).then(r=>r.json()).then(data=>{{
 const list = data.commits || [];
 const root = document.getElementById('commits');
 if (!list.length) {{ root.innerHTML = '<div class="empty">No commits</div>'; return; }}
 root.innerHTML = '<ul class="commit-list">' + list.map(c => {{
   const subject = String(c.message || '').split('\n')[0];
   return `<li class="commit-item"><div class="commit-main"><a class="commit-subject" href="{commit_prefix}${{c.full_hash}}">${{escapeHtml(subject)}}</a><div class="commit-meta"><span class="commit-author">${{escapeHtml(c.author)}}</span><span class="commit-dot">·</span><span class="commit-date">${{escapeHtml(c.date)}}</span></div></div><a class="commit-hash" href="{commit_prefix}${{c.full_hash}}" title="${{escapeHtml(c.full_hash)}}"><code>${{escapeHtml(c.hash)}}</code></a></li>`;
 }}).join('') + '</ul>';
}});
function escapeHtml(s) {{ return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c])); }}
</script></body></html>"#,
        css = app_css(),
        header = header,
        commit_prefix = commit_prefix,
    ))
}

async fn handle_commit_html(
    State(state): State<Arc<GithubServerState>>,
    AxumPath((_owner, _repo, hash)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let hash = github_preview_server::html_escape(&hash);
    let root = state.repo_root.display().to_string();
    let repo_url = github_preview_server::github_repo_url(&state.repo_root).await;
    let header = github_page_header(&root, "commits", repo_url.as_deref(), &state.url_ctx);
    let commit_prefix = github_preview_server::commit_url(&state.url_ctx, "");
    let diff_styles = render_diff_styles();
    Html(format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><title>Commit {hash}</title>{css}<style>{diff_styles}</style></head><body>{header}
<section class="commit-summary section"><div id="commit-summary"><div class="loading">Loading commit...</div></div></section>
<div class="layout">
 <aside class="sidebar">
  <section class="section files-section"><h2 id="files-heading">Files</h2><div id="files-list"><div class="loading">Loading files...</div></div></section>
 </aside>
 <main class="content"><div id="files-main"><div class="loading">Loading files...</div></div></main>
</div>
<script>
const hash = "{hash}";
const summaryEl = document.getElementById('commit-summary');
const filesListEl = document.getElementById('files-list');
const filesMainEl = document.getElementById('files-main');
const filesHeadingEl = document.getElementById('files-heading');

function escapeHtml(s) {{ return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c])); }}
function statusToText(s) {{
  switch ((s || '').toUpperCase()) {{
    case 'A': return 'added';
    case 'D': return 'deleted';
    case 'R': return 'renamed';
    case 'C': return 'renamed';
    case 'M': default: return 'modified';
  }}
}}
function statusBadgeChar(s) {{
  switch (s) {{
    case 'added': return 'A';
    case 'deleted': return 'D';
    case 'renamed': return 'R';
    case 'untracked': return '?';
    default: return 'M';
  }}
}}
function fileAnchorFor(path) {{ return 'f-' + path.replace(/[^A-Za-z0-9_-]/g, '_'); }}

function renderSummary(data) {{
  const message = String(data.message || '');
  const lines = message.split('\n');
  const subject = lines[0] || hash;
  const body = lines.slice(1).join('\n').replace(/^\n+/, '');
  const author = data.author || '';
  const email = data.author_email || '';
  const date = data.date || '';
  const fullHash = data.full_hash || hash;
  summaryEl.innerHTML = `
    <h1 class="commit-title">${{escapeHtml(subject)}}</h1>
    ${{body ? `<pre class="commit-body">${{escapeHtml(body)}}</pre>` : ''}}
    <div class="commit-byline">
      <span class="commit-author"><strong>${{escapeHtml(author)}}</strong>${{email ? ` &lt;${{escapeHtml(email)}}&gt;` : ''}}</span>
      <span class="commit-dot">·</span>
      <span class="commit-date">committed ${{escapeHtml(date)}}</span>
      <span class="commit-dot">·</span>
      <a class="commit-hash" href="{commit_prefix}${{escapeHtml(fullHash)}}"><code>${{escapeHtml((fullHash || '').slice(0, 7))}}</code></a>
    </div>`;
}}

function renderSidebar(files) {{
  filesHeadingEl.textContent = `Files (${{files.length}})`;
  if (!files.length) {{ filesListEl.innerHTML = '<div class="empty">No files changed</div>'; return; }}
  filesListEl.innerHTML = '<ul class="file-list">' + files.map(f => {{
    const status = statusToText(f.status);
    const badge = statusBadgeChar(status);
    return `<li><a href="#${{fileAnchorFor(f.path)}}"><span class="file-status gr-status-${{status}}">${{badge}}</span><span class="file-path-text" title="${{escapeHtml(f.path)}}">${{escapeHtml(f.path)}}</span></a></li>`;
  }}).join('') + '</ul>';
}}

function renderMain(files) {{
  if (!files.length) {{ filesMainEl.innerHTML = '<div class="empty">No files changed</div>'; return; }}
  filesMainEl.innerHTML = files.map(f => {{
    const status = statusToText(f.status);
    const anchor = fileAnchorFor(f.path);
    return `<section class="gr-file" id="${{anchor}}"><div class="gr-file-header"><span class="gr-status-tag gr-status-${{status}}">${{status}}</span><div class="gr-file-name-wrapper"><span class="gr-file-name" title="${{escapeHtml(f.path)}}">${{escapeHtml(f.path)}}</span></div></div><div class="gr-file-body" data-path="${{escapeHtml(f.path)}}"><div class="loading">Loading diff...</div></div></section>`;
  }}).join('');
  for (const el of filesMainEl.querySelectorAll('.gr-file-body[data-path]')) {{
    fetch(`/api/commit/${{hash}}/file?path=${{encodeURIComponent(el.dataset.path)}}`, {{cache:'no-store'}})
      .then(r => r.json())
      .then(file => {{ el.innerHTML = file.html || ''; }})
      .catch(e => {{ el.innerHTML = `<div class="loading">Error: ${{escapeHtml(e.message)}}</div>`; }});
  }}
}}

fetch(`/api/commit/${{hash}}`, {{cache:'no-store'}}).then(r=>r.json()).then(data=>{{
  renderSummary(data);
  const files = data.files || [];
  renderSidebar(files);
  renderMain(files);
}}).catch(e=>{{ summaryEl.innerHTML = `<div class="loading">Error: ${{escapeHtml(e.message)}}</div>`; }});
</script></body></html>"##,
        css = app_css(),
        diff_styles = diff_styles,
        header = header,
        hash = hash,
        commit_prefix = commit_prefix,
    ))
}

async fn handle_api_tree(
    State(state): State<Arc<GithubServerState>>,
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

async fn handle_api_blob(
    State(state): State<Arc<GithubServerState>>,
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
        github_preview_server::render_markdown_with_source_lines(&text)
    } else {
        format!(
            "<div class=\"code-view\">{}</div>",
            github_preview_server::render_code_with_line_ids_for_path(&text, &path)
        )
    };
    diff_server::ok_json(serde_json::json!({ "path": path, "content": text, "html": html }))
}

async fn handle_api_commits(
    State(state): State<Arc<GithubServerState>>,
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
    let format = "%h%x00%H%x00%an%x00%ad%x00%s";
    let raw = match diff_server::git_output_in_repo(
        &state.repo_root,
        &[
            "log",
            &format!("--skip={skip}"),
            &format!("-n{count_plus}", count_plus = count + 1),
            "--date=short",
            &format!("--pretty=format:{format}"),
        ],
    )
    .await
    {
        Ok(raw) => raw,
        Err(err) => return diff_server::bad_request(err),
    };
    let mut commits = Vec::new();
    for line in raw.lines().filter(|line| !line.is_empty()) {
        let parts: Vec<_> = line.splitn(5, '\0').collect();
        if parts.len() == 5 {
            commits.push(serde_json::json!({
                "hash": parts[0],
                "full_hash": parts[1],
                "author": parts[2],
                "date": parts[3],
                "message": parts[4],
            }));
        }
    }
    let has_more = commits.len() > count;
    commits.truncate(count);
    diff_server::ok_json(serde_json::json!({ "commits": commits, "has_more": has_more }))
}

async fn handle_api_commit(
    State(state): State<Arc<GithubServerState>>,
    AxumPath(hash): AxumPath<String>,
) -> Response {
    let Some(hash) = parse_commit_hash(&hash) else {
        return diff_server::bad_request("invalid commit hash");
    };
    let meta_raw = match diff_server::git_output_in_repo(
        &state.repo_root,
        &[
            "show",
            "-s",
            "--date=short",
            "--format=%H%n%an%n%ae%n%ad%n%B",
            &hash,
        ],
    )
    .await
    {
        Ok(raw) => raw,
        Err(err) => return diff_server::bad_request(err),
    };
    let meta: Vec<_> = meta_raw.splitn(5, '\n').collect();
    let files_raw = match diff_server::git_output_in_repo(
        &state.repo_root,
        &["diff-tree", "--no-commit-id", "--name-status", "-r", &hash],
    )
    .await
    {
        Ok(raw) => raw,
        Err(err) => return diff_server::bad_request(err),
    };
    let files: Vec<_> = files_raw
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let status = parts.next()?.chars().next().unwrap_or('M');
            let path = parts.next()?.to_string();
            Some(serde_json::json!({ "path": path, "status": status.to_string() }))
        })
        .collect();
    let diff_raw = match diff_server::git_output_in_repo(
        &state.repo_root,
        &["show", "--format=", "--no-ext-diff", &hash],
    )
    .await
    {
        Ok(raw) => raw,
        Err(err) => return diff_server::bad_request(err),
    };
    let diff_files: Vec<_> = parse_unified_diff(&diff_raw)
        .iter()
        .map(diff_server::file_metadata_json)
        .collect();
    diff_server::ok_json(serde_json::json!({
        "hash": hash,
        "full_hash": meta.first().copied().unwrap_or(""),
        "author": meta.get(1).copied().unwrap_or(""),
        "author_email": meta.get(2).copied().unwrap_or(""),
        "date": meta.get(3).copied().unwrap_or(""),
        "message": meta.get(4).copied().unwrap_or(""),
        "files": files,
        "diff_files": diff_files,
    }))
}

async fn handle_api_commit_file(
    State(state): State<Arc<GithubServerState>>,
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
    let diff = match diff_server::git_output_in_repo(
        &state.repo_root,
        &["show", "--format=", "--no-ext-diff", &hash, "--", &path],
    )
    .await
    {
        Ok(raw) => raw,
        Err(err) => return diff_server::bad_request(err),
    };
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

fn github_page_header(
    root_path: &str,
    active_tab: &str,
    repo_url: Option<&str>,
    ctx: &github_preview_server::RepoUrlContext,
) -> String {
    let repo_title = github_preview_server::repo_title_html(root_path, repo_url);
    let tab = |id: &str, label: &str, href: &str| {
        let class_name = if id == active_tab {
            "repo-tab repo-tab-active"
        } else {
            "repo-tab"
        };
        format!(
            r#"<a class="{class_name}" href="{}">{}</a>"#,
            github_preview_server::html_escape(href),
            github_preview_server::html_escape(label)
        )
    };
    format!(
        r#"<header class="repo-header">
            <div class="repo-title">{}</div>
            <div class="repo-meta"><span class="context-key">Root</span><code>{}</code></div>
            <nav class="repo-tabs" aria-label="Repository views">{}{}{}{}</nav>
        </header>"#,
        repo_title,
        github_preview_server::html_escape(root_path),
        tab("code", "Code", &github_preview_server::repo_home_url(ctx)),
        tab("status", "Status", "/status"),
        tab("branches", "Branches", "/branches"),
        tab("commits", "Commits", &github_preview_server::commits_url(ctx)),
    )
}

fn parse_commit_hash(hash: &str) -> Option<String> {
    if hash.is_empty() || hash.len() > 64 {
        return None;
    }
    if hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_string())
    } else {
        None
    }
}

fn normalize_api_path(path: &str) -> String {
    let normalized = github_preview_server::normalize_rel_path_for_compare(path);
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn app_css() -> &'static str {
    r#"<style>
body { margin: 0; padding: 20px; background: #f6f8fa; color: #24292f; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
.repo-header { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; margin-bottom: 16px; overflow: hidden; }
.repo-title { display: flex; align-items: center; gap: 4px; padding: 14px 16px 8px; font-size: 20px; }
.repo-title-link { display: inline-flex; align-items: center; gap: 4px; color: inherit; text-decoration: none; }
.repo-title-link:hover strong { text-decoration: underline; }
.repo-owner, .repo-sep { color: #57606a; font-weight: 400; }
.repo-meta { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding: 0 16px 12px; font-size: 13px; color: #57606a; }
.context-key { font-weight: 600; color: #24292f; }
.repo-tabs { display: flex; gap: 4px; padding: 0 8px; border-top: 1px solid #d8dee4; }
.repo-tab { display: inline-flex; padding: 10px 12px; color: #24292f; text-decoration: none; border-bottom: 2px solid transparent; font-size: 14px; }
.repo-tab:hover { background: #f6f8fa; text-decoration: none; }
.repo-tab-active { font-weight: 600; border-bottom-color: #fd8c73; }
a { color: #0969da; text-decoration: none; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace; padding: 2px 6px; background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 4px; }
.repo-meta code { padding: 0; background: transparent; border: 0; border-radius: 0; }
.loading, .empty { padding: 16px; color: #57606a; font-size: 13px; }
.section { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; padding: 16px; margin-bottom: 16px; }
.section h2 { margin: 0 0 12px 0; font-size: 16px; }

/* Commits list */
.commits-main { max-width: 960px; }
.commits-section { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; overflow: hidden; }
.commits-title { margin: 0; padding: 12px 16px; font-size: 16px; border-bottom: 1px solid #d0d7de; background: #f6f8fa; }
.commit-list { list-style: none; padding: 0; margin: 0; }
.commit-item { display: flex; align-items: center; gap: 12px; padding: 12px 16px; border-bottom: 1px solid #d8dee4; }
.commit-item:last-child { border-bottom: 0; }
.commit-main { flex: 1 1 auto; min-width: 0; }
.commit-subject { display: block; color: #24292f; font-weight: 600; font-size: 14px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.commit-subject:hover { color: #0969da; text-decoration: underline; }
.commit-meta { display: flex; align-items: center; gap: 6px; margin-top: 4px; font-size: 12px; color: #57606a; }
.commit-author { color: #24292f; font-weight: 500; }
.commit-dot { color: #8c959f; }
.commit-hash { flex-shrink: 0; }
.commit-hash code { font-size: 12px; }

/* Commit detail summary */
.commit-summary .commit-title { margin: 0 0 8px 0; font-size: 20px; font-weight: 600; color: #24292f; }
.commit-summary .commit-body { margin: 0 0 12px 0; padding: 12px; background: #f6f8fa; border: 1px solid #d8dee4; border-radius: 6px; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 12px; white-space: pre-wrap; word-wrap: break-word; color: #1f2328; }
.commit-summary .commit-byline { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; font-size: 13px; color: #57606a; }
.commit-summary .commit-byline .commit-author strong { color: #24292f; }
.commit-summary .commit-hash code { font-size: 12px; }

/* Layout: sidebar + content */
.layout { display: flex; gap: 16px; align-items: flex-start; }
.sidebar { width: 280px; flex-shrink: 0; position: sticky; top: 16px; max-height: calc(100vh - 32px); overflow-y: auto; }
.sidebar .section { margin-bottom: 0; }
.content { flex: 1 1 auto; min-width: 0; }
@media (max-width: 900px) {
  .layout { flex-direction: column; }
  .sidebar { position: static; width: auto; max-height: none; }
}

/* File list in sidebar */
.file-list { list-style: none; margin: 0; padding: 0; }
.file-list li { margin: 2px 0; }
.file-list a { display: flex; align-items: center; gap: 6px; color: #0969da; text-decoration: none; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 12px; padding: 4px 6px; border-radius: 4px; }
.file-list a:hover { background: #f6f8fa; text-decoration: underline; }
.file-list .file-path-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; min-width: 0; }
.file-status { display: inline-block; width: 1.2em; text-align: center; font-weight: 700; flex: 0 0 1.2em; font-size: 11px; }
.file-status.gr-status-added { color: #1a7f37; }
.file-status.gr-status-modified { color: #9a6700; }
.file-status.gr-status-deleted { color: #cf222e; }
.file-status.gr-status-renamed { color: #0969da; }
.file-status.gr-status-untracked { color: #57606a; }

/* Diff file cards (compatible with .gr-diff-body styles from render_diff_styles) */
.gr-file { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; margin-bottom: 12px; overflow: hidden; scroll-margin-top: 16px; }
.gr-file-header { display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: #f6f8fa; border-bottom: 1px solid #d0d7de; }
.gr-file-name-wrapper { flex: 1 1 auto; min-width: 0; display: flex; align-items: center; gap: 8px; overflow: hidden; }
.gr-file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; }
.gr-status-tag { flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; text-transform: lowercase; }
.gr-status-tag.gr-status-modified  { background: #fff8c5; color: #9a6700; }
.gr-status-tag.gr-status-added     { background: #dafbe1; color: #1a7f37; }
.gr-status-tag.gr-status-deleted   { background: #ffebe9; color: #cf222e; }
.gr-status-tag.gr-status-renamed   { background: #ddf4ff; color: #0969da; }
.gr-status-tag.gr-status-untracked { background: #eaeef2; color: #57606a; }
.gr-file-body { background: #fff; }
.gr-file-body .loading, .gr-file-body .empty { padding: 12px; color: #57606a; font-size: 12px; }
</style>"#
}

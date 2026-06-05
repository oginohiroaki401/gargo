//! GitHub-style file preview server for browsing repository files in a browser.
//!
//! This module implements an HTTP server that serves repository files with:
//! - Directory listings with file/folder navigation
//! - Markdown rendering with GitHub Flavored Markdown support
//! - Syntax highlighting for code files
//! - Breadcrumb navigation
//!
//! It follows the async runtime pattern:
//! - Command enum for controlling the server
//! - Event enum for status updates
//! - Handle with mpsc channels for communication
//! - Worker that runs on separate thread with Tokio runtime

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Json, Redirect},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::diff_render::{hl_class_attr, render_diff_styles};
use crate::input::action::{Action, AppAction, IntegrationAction};
use crate::syntax::highlight::{HighlightSpan, highlight_text};
use crate::syntax::language::LanguageRegistry;

/// Commands that can be sent to the Gargo preview server
#[derive(Debug, Clone)]
pub enum GargoPreviewCommand {
    Start { repo_root: PathBuf },
    Stop,
    SetActivePath { rel_path: Option<String> },
    RefreshActive,
    UpdateBufferContent { content: String, cursor_line: usize },
    UpdateCursorLine { line: usize },
}

/// Events emitted by the Gargo preview server
#[derive(Debug, Clone)]
pub enum GargoPreviewEvent {
    Started { port: u16 },
    Stopped,
    Detached { requested_path: String },
    Error(String),
}

/// Handle for communicating with the Gargo preview server worker thread
pub struct GargoPreviewHandle {
    pub command_tx: mpsc::Sender<GargoPreviewCommand>,
    pub event_rx: mpsc::Receiver<GargoPreviewEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl GargoPreviewHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = GargoPreviewWorker {
            command_rx,
            event_tx,
            tokio_runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to build tokio runtime: {}", e))?,
            server_shutdown_tx: None,
            server_state: None,
            pending_active_rel_path: None,
        };

        let worker_thread = thread::Builder::new()
            .name("github-preview".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("Failed to spawn worker thread: {}", e))?;

        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }

    #[cfg(test)]
    pub(crate) fn from_channels_for_test(
        command_tx: mpsc::Sender<GargoPreviewCommand>,
        event_rx: mpsc::Receiver<GargoPreviewEvent>,
    ) -> Self {
        Self {
            command_tx,
            event_rx,
            _worker_thread: None,
        }
    }
}

/// Worker thread that manages the Tokio runtime and HTTP server
struct GargoPreviewWorker {
    command_rx: mpsc::Receiver<GargoPreviewCommand>,
    event_tx: mpsc::Sender<GargoPreviewEvent>,
    tokio_runtime: tokio::runtime::Runtime,
    server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_state: Option<Arc<Mutex<PreviewServerState>>>,
    pending_active_rel_path: Option<String>,
}

impl GargoPreviewWorker {
    fn run(mut self) {
        loop {
            match self.command_rx.recv() {
                Ok(GargoPreviewCommand::Start { repo_root }) => self.handle_start_server(repo_root),
                Ok(GargoPreviewCommand::Stop) => self.handle_stop_server(),
                Ok(GargoPreviewCommand::SetActivePath { rel_path }) => {
                    self.handle_set_active_path(rel_path);
                }
                Ok(GargoPreviewCommand::RefreshActive) => self.handle_refresh_active(),
                Ok(GargoPreviewCommand::UpdateBufferContent {
                    content,
                    cursor_line,
                }) => self.handle_update_buffer_content(content, cursor_line),
                Ok(GargoPreviewCommand::UpdateCursorLine { line }) => {
                    self.handle_update_cursor_line(line)
                }
                Err(_) => break, // Main thread exited
            }
        }
    }

    fn handle_start_server(&mut self, repo_root: PathBuf) {
        if self.server_shutdown_tx.is_some() {
            let _ = self.event_tx.send(GargoPreviewEvent::Error(
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
                let _ = self.event_tx.send(GargoPreviewEvent::Error(format!(
                    "Failed to bind Gargo preview server on localhost: {}",
                    err
                )));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(err) => {
                let _ = self.event_tx.send(GargoPreviewEvent::Error(format!(
                    "Failed to read Gargo preview server local address: {}",
                    err
                )));
                return;
            }
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.server_shutdown_tx = Some(shutdown_tx);

        let url_ctx = self
            .tokio_runtime
            .block_on(resolve_repo_url_context(&repo_root));
        let server_state = Arc::new(Mutex::new(PreviewServerState {
            repo_root,
            url_ctx: url_ctx.clone(),
            port,
            active_rel_path: self.pending_active_rel_path.clone(),
            detached: false,
            last_detach_path: None,
            version: 1,
            last_browser_event: Some(PreviewBrowserEvent {
                kind: PreviewBrowserEventKind::Navigate,
                path: self.pending_active_rel_path.clone(),
                url: Some(preview_url_for_rel_path(
                    port,
                    &url_ctx,
                    self.pending_active_rel_path.as_deref(),
                )),
                detached: false,
                version: 1,
                cursor_line: None,
            }),
            event_tx: self.event_tx.clone(),
            buffer_content: None,
            cursor_line: None,
        }));
        self.server_state = Some(server_state.clone());

        let event_tx = self.event_tx.clone();
        self.tokio_runtime.spawn(async move {
            run_server(listener, shutdown_rx, server_state).await;
        });

        let _ = event_tx.send(GargoPreviewEvent::Started { port });
    }

    fn handle_stop_server(&mut self) {
        if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
            let _ = shutdown_tx.send(());
            self.server_state = None;
            let _ = self.event_tx.send(GargoPreviewEvent::Stopped);
        } else {
            let _ = self
                .event_tx
                .send(GargoPreviewEvent::Error("Server not running".to_string()));
        }
    }

    fn handle_set_active_path(&mut self, rel_path: Option<String>) {
        let normalized = rel_path.map(|p| normalize_rel_path_for_compare(&p));
        self.pending_active_rel_path = normalized.clone();
        if let Some(state) = &self.server_state
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
                url: Some(preview_url_for_rel_path(
                    state.port,
                    &state.url_ctx,
                    state.active_rel_path.as_deref(),
                )),
                detached: state.detached,
                version: state.version,
                cursor_line: None,
            };
            broadcast_preview_event(&mut state, event);
        }
    }

    fn handle_refresh_active(&mut self) {
        let Some(state) = &self.server_state else {
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
            url: Some(preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: state.cursor_line,
        };
        broadcast_preview_event(&mut state, event);
    }

    fn handle_update_buffer_content(&mut self, content: String, cursor_line: usize) {
        let Some(state) = &self.server_state else {
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
            url: Some(preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: state.cursor_line,
        };
        broadcast_preview_event(&mut state, event);
    }

    fn handle_update_cursor_line(&mut self, line: usize) {
        let Some(state) = &self.server_state else {
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
        broadcast_preview_event(&mut state, event);
    }
}

pub(crate) struct PreviewServerState {
    pub(crate) repo_root: PathBuf,
    pub(crate) url_ctx: RepoUrlContext,
    pub(crate) port: u16,
    pub(crate) active_rel_path: Option<String>,
    pub(crate) detached: bool,
    pub(crate) last_detach_path: Option<String>,
    pub(crate) version: u64,
    pub(crate) last_browser_event: Option<PreviewBrowserEvent>,
    pub(crate) event_tx: mpsc::Sender<GargoPreviewEvent>,
    pub(crate) buffer_content: Option<String>,
    pub(crate) cursor_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewBrowserEventKind {
    Navigate,
    Refresh,
    Detached,
    ScrollTo,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PreviewBrowserEvent {
    pub(crate) kind: PreviewBrowserEventKind,
    pub(crate) path: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) detached: bool,
    pub(crate) version: u64,
    pub(crate) cursor_line: Option<usize>,
}

pub(crate) fn preview_url_for_rel_path(
    port: u16,
    ctx: &RepoUrlContext,
    rel_path: Option<&str>,
) -> String {
    let path = match rel_path {
        Some(rel_path) if rel_path != "." => blob_url(ctx, rel_path),
        _ => repo_home_url(ctx),
    };
    format!("http://127.0.0.1:{}{}", port, path)
}

pub(crate) fn broadcast_preview_event(state: &mut PreviewServerState, event: PreviewBrowserEvent) {
    state.last_browser_event = Some(event);
}

const LIVE_SYNC_SCRIPT: &str = r#"<script>
(() => {
  const POLL_INTERVAL_MS = 600;
  const VERSION_STORAGE_KEY = 'gargo_preview_last_seen_event_version';
  const CURSOR_LINE_STORAGE_KEY = 'gargo_preview_cursor_line';
  const USER_SCROLL_TIMEOUT_MS = 3000;

  const readStoredVersion = () => {
    try {
      const raw = window.sessionStorage.getItem(VERSION_STORAGE_KEY);
      const parsed = Number.parseInt(raw ?? '', 10);
      return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
    } catch (_) {
      return 0;
    }
  };
  const persistVersion = (version) => {
    try {
      window.sessionStorage.setItem(VERSION_STORAGE_KEY, String(version));
    } catch (_) {}
  };

  // Seed from the event version that was current when this page was rendered
  // (injected server-side). A freshly opened tab — e.g. via Cmd/Ctrl+click,
  // which gets an empty sessionStorage — would otherwise start at 0 and have
  // its very first poll replay the already-current navigate event, yanking it
  // away from the URL it just loaded. Taking the max keeps same-tab navigation
  // (which persists a higher version) working unchanged.
  let lastSeenVersion = Math.max(readStoredVersion(), {{INITIAL_EVENT_VERSION}});
  let pollInFlight = false;
  let userScrolled = false;
  let userScrollTimer = null;

  // Track user scroll to suppress cursor tracking while user is scrolling
  let programmaticScroll = false;
  window.addEventListener('scroll', () => {
    if (programmaticScroll) return;
    userScrolled = true;
    if (userScrollTimer) clearTimeout(userScrollTimer);
    userScrollTimer = setTimeout(() => { userScrolled = false; }, USER_SCROLL_TIMEOUT_MS);
  }, { passive: true });

  const scrollToLine = (line) => {
    if (!line || line < 1) return;
    // Try code line element first (id="L{n}")
    const lineEl = document.getElementById('L' + line);
    if (lineEl) {
      programmaticScroll = true;
      lineEl.scrollIntoView({ behavior: 'smooth', block: 'center' });
      setTimeout(() => { programmaticScroll = false; }, 100);
      return;
    }
    // For markdown: find the nearest heading with data-source-line <= line
    const headings = document.querySelectorAll('[data-source-line]');
    let best = null;
    for (const h of headings) {
      const sl = parseInt(h.getAttribute('data-source-line'), 10);
      if (sl <= line) {
        best = h;
      } else {
        break;
      }
    }
    if (best) {
      programmaticScroll = true;
      best.scrollIntoView({ behavior: 'smooth', block: 'start' });
      setTimeout(() => { programmaticScroll = false; }, 100);
    }
  };

  // On page load, restore cursor position from sessionStorage
  const storedCursorLine = (() => {
    try {
      const raw = window.sessionStorage.getItem(CURSOR_LINE_STORAGE_KEY);
      return raw ? parseInt(raw, 10) : null;
    } catch (_) { return null; }
  })();
  if (storedCursorLine) {
    // Small delay to let the page render first
    setTimeout(() => scrollToLine(storedCursorLine), 100);
    try { window.sessionStorage.removeItem(CURSOR_LINE_STORAGE_KEY); } catch (_) {}
  }

  const toRoute = (url) => {
    try {
      const parsed = new URL(url, window.location.origin);
      return `${parsed.pathname}${parsed.search}`;
    } catch (_) {
      return null;
    }
  };
  const currentRoute = () => `${window.location.pathname}${window.location.search}`;
  const navigateInPlace = (url) => {
    const route = toRoute(url);
    if (!route || route === currentRoute()) {
      return;
    }
    userScrolled = false;
    window.location.assign(route);
  };

  const applyEvent = (payload) => {
    if (!payload || !payload.kind) {
      return;
    }

    if (payload.kind === 'navigate' && payload.url) {
      userScrolled = false;
      navigateInPlace(payload.url);
      return;
    }

    if (payload.kind === 'refresh') {
      // Store cursor line so we can scroll after reload
      if (payload.cursor_line) {
        try { window.sessionStorage.setItem(CURSOR_LINE_STORAGE_KEY, String(payload.cursor_line)); } catch (_) {}
      }
      if (payload.url) {
        try {
          const target = new URL(payload.url, window.location.origin);
          // Navigate only when the file path changed; otherwise reload in
          // place so query params (e.g. ?plain=1) survive the refresh.
          if (target.pathname !== window.location.pathname) {
            window.location.assign(`${target.pathname}${target.search}`);
            return;
          }
        } catch (_) {}
      }
      window.location.reload();
      return;
    }

    if (payload.kind === 'scroll_to') {
      if (!userScrolled && payload.cursor_line) {
        scrollToLine(payload.cursor_line);
      }
      return;
    }
  };

  const pollEvents = async () => {
    if (pollInFlight) {
      return;
    }
    pollInFlight = true;
    try {
      const response = await fetch(`/events?since=${lastSeenVersion}`, { cache: 'no-store' });
      if (!response.ok) {
        return;
      }
      const body = await response.json();
      if (!body.event) {
        return;
      }
      if (typeof body.event.version === 'number') {
        lastSeenVersion = Math.max(lastSeenVersion, body.event.version);
        persistVersion(lastSeenVersion);
      }
      applyEvent(body.event);
    } catch (_) {
      // Polling should be best-effort only.
    } finally {
      pollInFlight = false;
    }
  };

  window.setInterval(pollEvents, POLL_INTERVAL_MS);
  pollEvents();
})();
</script>"#;

/// Build the live-sync `<script>` for a page, seeding the client's initial
/// `lastSeenVersion` with the event version current at render time so the page
/// never replays an already-current navigate/refresh event onto itself.
fn live_sync_script(current_version: u64) -> String {
    LIVE_SYNC_SCRIPT.replace("{{INITIAL_EVENT_VERSION}}", &current_version.to_string())
}

const PREVIEW_RELOAD_SCRIPT: &str = r#"<script>
(() => {
  const POLL_INTERVAL_MS = 600;
  let lastSeenVersion = 0;
  let pollInFlight = false;

  const pollEvents = async () => {
    if (pollInFlight) return;
    pollInFlight = true;
    try {
      const response = await fetch(`/events?since=${lastSeenVersion}`, { cache: 'no-store' });
      if (!response.ok) return;
      const body = await response.json();
      if (!body.event) return;
      if (typeof body.event.version === 'number') {
        lastSeenVersion = Math.max(lastSeenVersion, body.event.version);
      }
      if (body.event.kind === 'refresh') {
        window.location.reload();
      }
    } catch (_) {
    } finally {
      pollInFlight = false;
    }
  };

  window.setInterval(pollEvents, POLL_INTERVAL_MS);
  pollEvents();
})();
</script>"#;

const MERMAID_JS: &str = include_str!("../../assets/mermaid.min.js");

const MERMAID_INIT_SCRIPT: &str = r#"<script>
(() => {
  if (!window.mermaid) return;
  window.mermaid.initialize({ startOnLoad: false, theme: 'default' });
  window.mermaid.run({ querySelector: 'pre.mermaid' }).catch(() => {});
})();
</script>"#;

/// The mermaid runtime is 3.3MB; load it (and its init) only when the rendered
/// page actually contains a diagram. `render_mermaid_blocks` emits
/// `<pre class="mermaid">`, so its presence is the trigger. Most pages have no
/// diagram and skip the script entirely.
fn mermaid_script_block(rendered_content: &str) -> String {
    if rendered_content.contains(r#"class="mermaid""#) {
        format!(r#"<script src="/assets/mermaid.min.js"></script>{MERMAID_INIT_SCRIPT}"#)
    } else {
        String::new()
    }
}

pub(crate) async fn handle_mermaid_asset() -> impl IntoResponse {
    let mut response = (StatusCode::OK, MERMAID_JS).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

/// Serve the shared chrome stylesheet as a cacheable asset. Pages link it with
/// a `?v=<version>` stamp, so the `immutable` cache is only busted on release.
pub(crate) async fn handle_shared_css_asset() -> impl IntoResponse {
    let mut response = (StatusCode::OK, crate::command::server_shared::SHARED_CSS).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/css; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

/// Serve the shared keyboard-shortcuts module as a cacheable asset (see
/// [`handle_shared_css_asset`]).
pub(crate) async fn handle_shortcuts_js_asset() -> impl IntoResponse {
    let mut response =
        (StatusCode::OK, crate::command::server_shared::SHORTCUTS_JS).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

pub(crate) fn github_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let path = url.strip_prefix("https://github.com/")?;
    let mut parts = path.trim_matches('/').split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some((owner, repo))
    }
}

pub(crate) fn repo_name_from_root(root_path: &str) -> &str {
    root_path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(root_path)
}

/// owner/repo/branch used to build github.com-faithful URLs. The local server
/// mirrors GitHub's path layout so swapping `http://127.0.0.1:PORT/` for
/// `github.com/` yields the equivalent page.
#[derive(Debug, Clone)]
pub(crate) struct RepoUrlContext {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) branch: String,
}

/// owner/repo pair: from the GitHub remote when present, else `local`/folder name.
pub(crate) fn owner_repo_for_root(root_path: &str, repo_url: Option<&str>) -> (String, String) {
    repo_url
        .and_then(github_owner_repo_from_url)
        .unwrap_or_else(|| {
            (
                "local".to_string(),
                repo_name_from_root(root_path).to_string(),
            )
        })
}

/// Per-repo cache of the two git facts that are effectively static for a
/// session: the GitHub remote URL and the default branch name. The current
/// branch is deliberately *not* cached here — a checkout changes it. Caching
/// avoids re-spawning `git config` / `git symbolic-ref` on every page load and
/// every in-page navigation. Locks are never held across an `.await`.
static REPO_URL_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> =
    std::sync::OnceLock::new();
static DEFAULT_BRANCH_CACHE: std::sync::OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> =
    std::sync::OnceLock::new();

/// `github_repo_url`, memoized per repo root.
async fn cached_github_repo_url(repo_root: &Path) -> Option<String> {
    let cache = REPO_URL_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(repo_root).cloned() {
        return hit;
    }
    let value = github_repo_url(repo_root).await;
    cache
        .lock()
        .unwrap()
        .insert(repo_root.to_path_buf(), value.clone());
    value
}

/// `default_branch_name`, memoized per repo root.
async fn cached_default_branch_name(repo_root: &Path) -> Option<String> {
    let cache = DEFAULT_BRANCH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(repo_root).cloned() {
        return hit;
    }
    let value = default_branch_name(repo_root).await;
    cache
        .lock()
        .unwrap()
        .insert(repo_root.to_path_buf(), value.clone());
    value
}

/// Resolve the owner/repo/branch for a repository (runs git). The remote URL is
/// cached and the current-branch lookup runs concurrently with it.
pub(crate) async fn resolve_repo_url_context(repo_root: &Path) -> RepoUrlContext {
    let (repo_url, branch) = tokio::join!(
        cached_github_repo_url(repo_root),
        detect_current_branch(repo_root),
    );
    let root_path = repo_root.display().to_string();
    let (owner, repo) = owner_repo_for_root(&root_path, repo_url.as_deref());
    RepoUrlContext {
        owner,
        repo,
        branch,
    }
}

/// Resolve everything an HTML page header needs — `RepoUrlContext`, the GitHub
/// remote URL, and the default branch — spawning the underlying git lookups
/// concurrently (and reusing the per-repo caches). Replaces three sequential
/// `await`s, one of which previously re-fetched the remote URL redundantly.
pub(crate) async fn resolve_page_context(
    repo_root: &Path,
) -> (RepoUrlContext, Option<String>, Option<String>) {
    let (repo_url, branch, default_branch) = tokio::join!(
        cached_github_repo_url(repo_root),
        detect_current_branch(repo_root),
        cached_default_branch_name(repo_root),
    );
    let root_path = repo_root.display().to_string();
    let (owner, repo) = owner_repo_for_root(&root_path, repo_url.as_deref());
    (
        RepoUrlContext {
            owner,
            repo,
            branch,
        },
        repo_url,
        default_branch,
    )
}

/// Current branch name; falls back to the short commit hash when detached,
/// or `main` when git is unavailable.
async fn detect_current_branch(repo_root: &Path) -> String {
    match git_output_in_repo(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        Ok(branch) if branch != "HEAD" && !branch.is_empty() => branch,
        _ => git_output_in_repo(repo_root, &["rev-parse", "--short", "HEAD"])
            .await
            .ok()
            .filter(|hash| !hash.is_empty())
            .unwrap_or_else(|| "main".to_string()),
    }
}

/// `/{owner}/{repo}` — the repository home.
pub(crate) fn repo_home_url(ctx: &RepoUrlContext) -> String {
    format!("/{}/{}", ctx.owner, ctx.repo)
}

/// `/{owner}/{repo}/tree/{branch}/{path}` — a directory listing.
pub(crate) fn tree_url(ctx: &RepoUrlContext, rel_path: &str) -> String {
    if rel_path.is_empty() || rel_path == "." {
        format!("/{}/{}/tree/{}", ctx.owner, ctx.repo, ctx.branch)
    } else {
        format!(
            "/{}/{}/tree/{}/{}",
            ctx.owner, ctx.repo, ctx.branch, rel_path
        )
    }
}

/// `/{owner}/{repo}/blob/{branch}/{path}` — a file view.
pub(crate) fn blob_url(ctx: &RepoUrlContext, rel_path: &str) -> String {
    format!(
        "/{}/{}/blob/{}/{}",
        ctx.owner, ctx.repo, ctx.branch, rel_path
    )
}

/// `/{owner}/{repo}/commits/{branch}` — the commit history.
pub(crate) fn commits_url(ctx: &RepoUrlContext) -> String {
    format!("/{}/{}/commits/{}", ctx.owner, ctx.repo, ctx.branch)
}

/// `/{owner}/{repo}/commit/{hash}` — a single commit.
pub(crate) fn commit_url(ctx: &RepoUrlContext, hash: &str) -> String {
    format!("/{}/{}/commit/{}", ctx.owner, ctx.repo, hash)
}

/// Static pill that prefixes the breadcrumb on the code page so reviewers
/// always see which branch the file/dir is being shown from. Not a switcher
/// — the rail's "Branches" tab covers that interaction.
fn path_branch_chip_html(ctx: &RepoUrlContext) -> String {
    if ctx.branch.is_empty() {
        return String::new();
    }
    format!(
        r#"<span class="path-branch" title="{branch}">{branch}</span>"#,
        branch = html_escape(&ctx.branch),
    )
}

/// Render `repo / dir / subdir / file` with each prefix linked to its
/// tree/blob page and the final segment styled as the current location.
fn path_breadcrumb_html(ctx: &RepoUrlContext, rel_path: &str) -> String {
    let mut out = String::from(r#"<div class="breadcrumb">"#);
    let root_pill = format!(
        r#"<a class="crumb-pill" href="{}">{}</a>"#,
        repo_home_url(ctx),
        html_escape(&ctx.repo),
    );
    let segments: Vec<&str> = if rel_path == "." || rel_path.is_empty() {
        Vec::new()
    } else {
        rel_path
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    };
    if segments.is_empty() {
        // We're at the repo root: render the repo name as the current pill
        // so it visually matches the other levels.
        out.push_str(r#"<span class="crumb-pill crumb-pill-current">"#);
        out.push_str(&html_escape(&ctx.repo));
        out.push_str("</span></div>");
        return out;
    }
    out.push_str(&root_pill);
    let last = segments.len() - 1;
    let mut acc = String::new();
    for (i, seg) in segments.iter().enumerate() {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        out.push_str(r#"<span class="crumb-separator">/</span>"#);
        if i == last {
            out.push_str(r#"<span class="crumb-pill crumb-pill-current">"#);
            out.push_str(&html_escape(seg));
            out.push_str("</span>");
        } else {
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    r#"<a class="crumb-pill" href="{}">{}</a>"#,
                    tree_url(ctx, &acc),
                    html_escape(seg),
                ),
            );
        }
    }
    out.push_str("</div>");
    out
}

struct PathCommitInfo {
    short_hash: String,
    full_hash: String,
    subject: String,
    author: String,
    ago: String,
}

/// `git log -1` for a path — the commit that last touched the file/dir.
/// Used for the GitHub-style "last commit" strip above the code/dir view.
async fn path_last_commit(repo_root: &Path, rel_path: &str) -> Option<PathCommitInfo> {
    let mut args: Vec<&str> = vec!["log", "-1", "--format=%h%x00%H%x00%s%x00%an%x00%ar"];
    let path_owned = if rel_path == "." || rel_path.is_empty() {
        None
    } else {
        Some(rel_path.to_string())
    };
    if let Some(ref p) = path_owned {
        args.push("--");
        args.push(p.as_str());
    }
    let out = git_output_in_repo(repo_root, &args).await.ok()?;
    let mut parts = out.split('\0');
    let short_hash = parts.next()?.to_string();
    let full_hash = parts.next()?.to_string();
    let subject = parts.next()?.to_string();
    let author = parts.next()?.to_string();
    let ago = parts.next()?.trim_end().to_string();
    if short_hash.is_empty() {
        return None;
    }
    Some(PathCommitInfo {
        short_hash,
        full_hash,
        subject,
        author,
        ago,
    })
}

async fn path_commit_strip_html(repo_root: &Path, rel_path: &str, ctx: &RepoUrlContext) -> String {
    let Some(info) = path_last_commit(repo_root, rel_path).await else {
        return String::new();
    };
    format!(
        r#"<div class="commit-info">
  <a class="commit-info-hash" href="{href}" title="{full}"><code>{short}</code></a>
  <span class="commit-info-subject">{subject}</span>
  <span class="commit-info-meta">{author} · {ago}</span>
</div>"#,
        href = commit_url(ctx, &info.full_hash),
        full = html_escape(&info.full_hash),
        short = html_escape(&info.short_hash),
        subject = html_escape(&info.subject),
        author = html_escape(&info.author),
        ago = html_escape(&info.ago),
    )
}

/// Resolve the `{*rest}` capture of a `/tree|blob/{branch}/{path}` route into a
/// repo-relative path, stripping the known branch prefix. Branch names may
/// contain `/`, so the known branch is matched first; stale links fall back to
/// dropping the first segment as the ref.
pub(crate) fn split_branch_and_path(rest: &str, branch: &str) -> String {
    let rest = rest.trim_start_matches('/');
    if rest == branch {
        return ".".to_string();
    }
    if let Some(stripped) = rest.strip_prefix(branch)
        && let Some(path) = stripped.strip_prefix('/')
    {
        return normalize_rel_path_for_compare(path);
    }
    match rest.split_once('/') {
        Some((_branch, path)) => normalize_rel_path_for_compare(path),
        None => ".".to_string(),
    }
}

fn inject_preview_reload_script(html: &str) -> String {
    if let Some(pos) = html.rfind("</body>") {
        let mut result = String::with_capacity(html.len() + PREVIEW_RELOAD_SCRIPT.len());
        result.push_str(&html[..pos]);
        result.push_str(PREVIEW_RELOAD_SCRIPT);
        result.push_str(&html[pos..]);
        result
    } else {
        format!("{}{}", html, PREVIEW_RELOAD_SCRIPT)
    }
}

/// HTML template for directory listing
const DIRECTORY_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{{TITLE}}</title>
    {{SHARED_CSS}}
    <style>
        .header {
            background: #ffffff;
            padding: 16px 20px;
            margin-bottom: 20px;
            border-radius: 6px;
            border: 1px solid #d0d7de;
            display: flex;
            flex-direction: column;
            gap: 10px;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #57606a;
        }
        .context-row code {
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
            background: #f6f8fa;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            padding: 2px 8px;
            color: #24292f;
            word-break: break-all;
        }
        .file-list {
            background: #ffffff;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            overflow: hidden;
        }
        .file-item {
            display: grid;
            grid-template-columns: 20px 1fr auto;
            gap: 12px;
            align-items: center;
            padding: 8px 16px;
            border-bottom: 1px solid #d8dee4;
            transition: background-color 0.1s;
        }
        .file-item:last-child {
            border-bottom: none;
        }
        .file-item:hover {
            background: #f6f8fa;
        }
        /* Reveal the per-file open-actions pills only on row hover (and when a
           row is keyboard-focused, so j/k navigation shows the targets). */
        .file-item .open-actions { visibility: hidden; }
        .file-item:hover .open-actions,
        .file-item.is-focused .open-actions { visibility: visible; }
        .file-icon {
            font-size: 16px;
            text-align: center;
        }
        .file-name a {
            color: #0969da;
            text-decoration: none;
            font-size: 14px;
            font-weight: 500;
            display: block;
            width: fit-content;
        }
        .file-name a:hover {
            text-decoration: underline;
        }
        .markdown-body {
            background: #ffffff;
            padding: 20px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
        }
        .markdown-body code {
            background: rgba(175, 184, 193, 0.2);
            padding: 0.2em 0.4em;
            border-radius: 6px;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
            font-size: 85%;
        }
        .markdown-body pre {
            background: #f6f8fa;
            padding: 16px;
            border-radius: 6px;
            overflow: auto;
            line-height: 1.45;
        }
        .markdown-body pre code {
            background: transparent;
            padding: 0;
            border-radius: 0;
            font-size: 100%;
        }
        .error {
            background: #ffffff;
            color: #cf222e;
            padding: 20px;
            border-radius: 6px;
            border: 1px solid #d0d7de;
        }
        /* Mermaid diagram styling */
        .mermaid {
            background: #ffffff;
            padding: 20px;
            margin: 16px 0;
            border-radius: 6px;
            border: 1px solid #d0d7de;
            display: flex;
            justify-content: center;
            align-items: center;
        }
        .mermaid svg {
            max-width: 100%;
            height: auto;
        }
{{SYNTAX_STYLES}}
    </style>
</head>
<body data-page="code-tree">
    {{REPO_CTX_SCRIPT}}
    {{SHORTCUTS_JS}}
    <div class="app-shell">
        {{APP_RAIL}}
        <main class="app-main">
            <div class="breadcrumb-row">{{BREADCRUMB}}{{TOOLBAR}}</div>
            {{COMMIT_INFO}}
            {{CONTENT}}
        </main>
    </div>
    {{MERMAID_SCRIPT}}
    {{LIVE_SYNC_SCRIPT}}
</body>
</html>"#;

/// HTML template for file viewing
const FILE_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{{TITLE}}</title>
    {{SHARED_CSS}}
    <style>
        .header {
            background: #ffffff;
            padding: 16px 20px;
            margin-bottom: 20px;
            border-radius: 6px;
            border: 1px solid #d0d7de;
            display: flex;
            flex-direction: column;
            gap: 10px;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #57606a;
        }
        .context-row code {
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
            background: #f6f8fa;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            padding: 2px 8px;
            color: #24292f;
            word-break: break-all;
        }
        .markdown-body {
            background: #ffffff;
            padding: 20px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
        }
        .markdown-body code {
            background: rgba(175, 184, 193, 0.2);
            padding: 0.2em 0.4em;
            border-radius: 6px;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
            font-size: 85%;
        }
        .markdown-body pre {
            background: #f6f8fa;
            padding: 16px;
            border-radius: 6px;
            overflow: auto;
            line-height: 1.45;
        }
        .markdown-body pre code {
            background: transparent;
            padding: 0;
            border-radius: 0;
            font-size: 100%;
        }
        .file-content {
            background: #ffffff;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            overflow: hidden;
        }
        .file-content pre {
            margin: 0;
            padding: 16px;
            background: #f6f8fa;
        }
        .file-content code {
            background: transparent;
        }
        .error {
            background: #ffffff;
            color: #cf222e;
            padding: 20px;
            border-radius: 6px;
            border: 1px solid #d0d7de;
        }
        /* Mermaid diagram styling */
        .mermaid {
            background: #ffffff;
            padding: 20px;
            margin: 16px 0;
            border-radius: 6px;
            border: 1px solid #d0d7de;
            display: flex;
            justify-content: center;
            align-items: center;
        }
        .mermaid svg {
            max-width: 100%;
            height: auto;
        }
{{SYNTAX_STYLES}}
    </style>
</head>
<body data-page="code-blob">
    {{REPO_CTX_SCRIPT}}
    {{SHORTCUTS_JS}}
    <div class="app-shell">
        {{APP_RAIL}}
        <main class="app-main">
            <div class="breadcrumb-row">{{BREADCRUMB}}</div>
            {{COMMIT_INFO}}
            <div class="file-panel">{{TOOLBAR}}{{CONTENT}}</div>
        </main>
    </div>
    {{MERMAID_SCRIPT}}
    {{LIVE_SYNC_SCRIPT}}
</body>
</html>"#;

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<Mutex<PreviewServerState>>,
) {
    // URLs mirror github.com — see the route comment in `gargo_server::run_server`.
    let app = Router::new()
        .route("/", get(handle_bare_root))
        .route("/events", get(handle_events))
        .route("/assets/mermaid.min.js", get(handle_mermaid_asset))
        .route("/assets/server-shared.css", get(handle_shared_css_asset))
        .route(
            "/assets/server-shortcuts.js",
            get(handle_shortcuts_js_asset),
        )
        .route("/{owner}/{repo}", get(handle_root))
        .route("/{owner}/{repo}/tree/{*rest}", get(handle_tree))
        .route("/{owner}/{repo}/blob/{*rest}", get(handle_blob))
        .route("/{owner}/{repo}/preview/{*rest}", get(handle_preview))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventsQuery {
    pub(crate) since: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct EventsResponse {
    pub(crate) event: Option<PreviewBrowserEvent>,
}

pub(crate) async fn handle_events(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    Query(query): Query<EventsQuery>,
) -> impl IntoResponse {
    let since = query.since.unwrap_or(0);
    if let Ok(state) = state.lock() {
        let event = state
            .last_browser_event
            .as_ref()
            .filter(|e| e.version > since)
            .cloned();
        Json(EventsResponse { event })
    } else {
        Json(EventsResponse { event: None })
    }
}

/// Redirect the bare `/` to the github.com-style repo home `/{owner}/{repo}`.
pub(crate) async fn handle_bare_root(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
) -> impl IntoResponse {
    let url = {
        let st = state.lock().unwrap();
        repo_home_url(&st.url_ctx)
    };
    Redirect::to(&url)
}

/// Handle the repo home `/{owner}/{repo}` - show directory listing
pub(crate) async fn handle_root(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
) -> impl IntoResponse {
    maybe_emit_detached_event(&state, ".");

    let (repo_root, url_ctx, version) = {
        let st = state.lock().unwrap();
        (st.repo_root.clone(), st.url_ctx.clone(), st.version)
    };

    // Always show directory listing at root
    handle_directory_listing(&repo_root, ".", &repo_root, &url_ctx, version).await
}

/// Handle tree view (directory listing) `/{owner}/{repo}/tree/{branch}/{path}`
pub(crate) async fn handle_tree(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    AxumPath((_owner, _repo, rest)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let (repo_root, url_ctx, version) = {
        let st = state.lock().unwrap();
        (st.repo_root.clone(), st.url_ctx.clone(), st.version)
    };
    let path = split_branch_and_path(&rest, &url_ctx.branch);

    let full_path = repo_root.join(&path);

    // Security: ensure path is within repo
    if !full_path.starts_with(&repo_root) {
        return Html(render_error("Invalid path"));
    }

    maybe_emit_detached_event(&state, &path);

    handle_directory_listing(&full_path, &path, &repo_root, &url_ctx, version).await
}

/// Handle blob view (file content) `/{owner}/{repo}/blob/{branch}/{path}`
pub(crate) async fn handle_blob(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    AxumPath((_owner, _repo, rest)): AxumPath<(String, String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let (repo_root, url_ctx, path, buffer_content, version) = {
        let st = state.lock().unwrap();
        let path = split_branch_and_path(&rest, &st.url_ctx.branch);
        let bc = if st
            .active_rel_path
            .as_deref()
            .map(normalize_rel_path_for_compare)
            == Some(normalize_rel_path_for_compare(&path))
        {
            st.buffer_content.clone()
        } else {
            None
        };
        (
            st.repo_root.clone(),
            st.url_ctx.clone(),
            path,
            bc,
            st.version,
        )
    };

    let full_path = repo_root.join(&path);

    // Security: ensure path is within repo
    if !full_path.starts_with(&repo_root) {
        return Html(render_error("Invalid path"));
    }

    maybe_emit_detached_event(&state, &path);

    // GitHub's `?plain=1` convention: show the raw markdown source.
    let plain = params.get("plain").map(|v| v == "1").unwrap_or(false);
    handle_file_display(
        &full_path,
        &path,
        &repo_root,
        &url_ctx,
        buffer_content.as_deref(),
        plain,
        version,
    )
    .await
}

/// Handle HTML preview (render HTML as-is in the browser)
pub(crate) async fn handle_preview(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    AxumPath((_owner, _repo, rest)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let (repo_root, path, buffer_content) = {
        let st = state.lock().unwrap();
        let path = split_branch_and_path(&rest, &st.url_ctx.branch);
        let bc = if st
            .active_rel_path
            .as_deref()
            .map(normalize_rel_path_for_compare)
            == Some(normalize_rel_path_for_compare(&path))
        {
            st.buffer_content.clone()
        } else {
            None
        };
        (st.repo_root.clone(), path, bc)
    };

    let full_path = repo_root.join(&path);

    // Security: ensure path is within repo
    if !full_path.starts_with(&repo_root) {
        return Html(render_error("Invalid path"));
    }

    // Only allow HTML files
    let filename = full_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !filename.ends_with(".html") && !filename.ends_with(".htm") {
        return Html(render_error("Preview is only available for HTML files"));
    }

    let html_content = if let Some(text) = buffer_content {
        text
    } else {
        match tokio::fs::read(&full_path).await {
            Ok(content) => match String::from_utf8(content) {
                Ok(text) => text,
                Err(_) => return Html(render_error("File is not valid UTF-8")),
            },
            Err(e) => return Html(render_error(&format!("Failed to read file: {}", e))),
        }
    };

    Html(inject_preview_reload_script(&html_content))
}

pub(crate) fn normalize_rel_path_for_compare(path: &str) -> String {
    let normalized = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn maybe_emit_detached_event(state: &Arc<Mutex<PreviewServerState>>, requested_rel_path: &str) {
    let requested = normalize_rel_path_for_compare(requested_rel_path);
    let mut to_emit: Option<mpsc::Sender<GargoPreviewEvent>> = None;

    if let Ok(mut state) = state.lock() {
        let matches_active = match state.active_rel_path.as_deref() {
            Some(active) => active == requested,
            None => true,
        };
        if matches_active {
            return;
        }

        let should_emit = !state.detached || state.last_detach_path.as_deref() != Some(&requested);
        state.detached = true;
        state.last_detach_path = Some(requested.clone());
        if should_emit {
            state.version = state.version.wrapping_add(1);
            let browser_event = PreviewBrowserEvent {
                kind: PreviewBrowserEventKind::Detached,
                path: Some(requested.clone()),
                url: None,
                detached: state.detached,
                version: state.version,
                cursor_line: None,
            };
            broadcast_preview_event(&mut state, browser_event);
            to_emit = Some(state.event_tx.clone());
        }
    }

    if let Some(event_tx) = to_emit {
        let _ = event_tx.send(GargoPreviewEvent::Detached {
            requested_path: requested,
        });
    }
}

/// Get repository root directory
/// Render directory listing
pub(crate) async fn handle_directory_listing(
    path: &Path,
    display_path: &str,
    repo_root: &Path,
    ctx: &RepoUrlContext,
    current_version: u64,
) -> Html<String> {
    let mut entries = match tokio::fs::read_dir(path).await {
        Ok(entries) => entries,
        Err(e) => return Html(render_error(&format!("Failed to read directory: {}", e))),
    };

    let mut files: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip hidden files
        if name.starts_with('.') {
            continue;
        }

        let metadata = match entry.metadata().await {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }

    dirs.sort();
    files.sort();

    // Remote + default branch power the per-file "open on GitHub" pills below;
    // resolved once here and reused for the rail's "View on GitHub" link.
    let repo_url = github_repo_url(repo_root).await;
    let default_branch = default_branch_name(repo_root).await;

    let mut file_list = String::from("<div class=\"file-list\">");

    // Add parent directory link if not at root
    if display_path != "." {
        let parent = Path::new(display_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".");
        let parent_url = if parent == "." {
            repo_home_url(ctx)
        } else {
            tree_url(ctx, parent)
        };
        file_list.push_str(&format!(
            r#"<div class="file-item"><div class="file-icon">📁</div><div class="file-name"><a href="{}">.. (parent directory)</a></div></div>"#,
            html_escape(&parent_url)
        ));
    }

    // Add directories
    for dir in dirs {
        let link_path = if display_path == "." {
            dir.clone()
        } else {
            format!("{}/{}", display_path, dir)
        };
        file_list.push_str(&format!(
            r#"<div class="file-item"><div class="file-icon">📁</div><div class="file-name"><a href="{}">{}</a></div></div>"#,
            html_escape(&tree_url(ctx, &link_path)),
            html_escape(&dir)
        ));
    }

    // Add files
    for file in files {
        let link_path = if display_path == "." {
            file.clone()
        } else {
            format!("{}/{}", display_path, file)
        };
        file_list.push_str(&format!(
            r#"<div class="file-item"><div class="file-icon">📄</div><div class="file-name"><a href="{}">{}</a></div>{}</div>"#,
            html_escape(&blob_url(ctx, &link_path)),
            html_escape(&file),
            crate::command::app_shell::open_actions_html(
                ctx,
                &link_path,
                repo_url.as_deref(),
                default_branch.as_deref(),
            ),
        ));
    }

    file_list.push_str("</div>");

    // Check if README.md exists in this directory and add preview
    let readme_path = path.join("README.md");
    let mut content = file_list;
    if tokio::fs::metadata(&readme_path).await.is_ok()
        && let Ok(readme_content) = tokio::fs::read(&readme_path).await
        && let Ok(text) = String::from_utf8(readme_content)
    {
        let html = render_markdown(&text);
        let readme_dir = if display_path == "." {
            ""
        } else {
            display_path
        };
        let html = rewrite_markdown_links(&html, ctx, readme_dir);
        content.push_str(r#"<div style="margin-top: 24px;"><div class="markdown-body">"#);
        content.push_str(&html);
        content.push_str("</div></div>");
    }

    let title = if display_path == "." {
        "Repository Root".to_string()
    } else {
        display_path.to_string()
    };
    let root_path = repo_root.display().to_string();

    let commit_info = path_commit_strip_html(repo_root, display_path, ctx).await;
    let branch_chip = path_branch_chip_html(ctx);
    let breadcrumb = format!("{branch_chip}{}", path_breadcrumb_html(ctx, display_path));
    let github_href = repo_url.as_deref().map(|base| {
        if display_path == "." || display_path.is_empty() {
            base.to_string()
        } else {
            format!(
                "{base}/tree/{branch}/{path}",
                branch = ctx.branch,
                path = display_path,
            )
        }
    });
    let rail = crate::command::app_shell::app_rail_html(ctx, github_href.as_deref(), "code");
    let html = DIRECTORY_TEMPLATE
        .replace("{{TITLE}}", &html_escape(&title))
        .replace("{{ROOT_PATH}}", &html_escape(&root_path))
        .replace("{{CURRENT_PATH}}", &html_escape(display_path))
        .replace("{{APP_RAIL}}", &rail)
        .replace("{{TOOLBAR}}", "")
        .replace("{{BREADCRUMB}}", &breadcrumb)
        .replace("{{COMMIT_INFO}}", &commit_info)
        .replace("{{CONTENT}}", &content)
        .replace(
            "{{REPO_CTX_SCRIPT}}",
            &crate::command::server_shared::repo_ctx_script(
                &ctx.owner,
                &ctx.repo,
                &ctx.branch,
                repo_url.as_deref(),
                default_branch.as_deref(),
            ),
        )
        .replace(
            "{{SHARED_CSS}}",
            &crate::command::server_shared::shared_css_link(),
        )
        .replace(
            "{{SHORTCUTS_JS}}",
            &crate::command::server_shared::shortcuts_js_tag(),
        )
        .replace("{{SYNTAX_STYLES}}", render_diff_styles())
        .replace("{{MERMAID_SCRIPT}}", &mermaid_script_block(&content))
        .replace("{{LIVE_SYNC_SCRIPT}}", &live_sync_script(current_version));

    Html(html)
}

/// Render file content. For markdown, `plain` selects the raw source view
/// (with line numbers) over the rendered preview; a toggle links between them.
pub(crate) async fn handle_file_display(
    path: &Path,
    display_path: &str,
    repo_root: &Path,
    ctx: &RepoUrlContext,
    buffer_content: Option<&str>,
    plain: bool,
    current_version: u64,
) -> Html<String> {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let is_markdown = filename.ends_with(".md");

    // Resolve the file text: live buffer if present, otherwise disk.
    let text: Option<String> = if let Some(text) = buffer_content {
        Some(text.to_string())
    } else {
        match tokio::fs::read(path).await {
            Ok(content) => String::from_utf8(content).ok(),
            Err(e) => return Html(render_error(&format!("Failed to read file: {}", e))),
        }
    };

    let root_path = repo_root.display().to_string();
    let repo_url = github_repo_url(repo_root).await;
    let default_branch = default_branch_name(repo_root).await;

    // Preview/Code toggle goes into the breadcrumb row. View on GitHub now
    // lives in the rail, so the in-body toolbar carries view-mode chips plus the
    // open-actions pills (current/new tab, GitHub, GitHub default branch, editor).
    let editor_btn = crate::command::app_shell::open_actions_html(
        ctx,
        display_path,
        repo_url.as_deref(),
        default_branch.as_deref(),
    );
    // Copy-the-whole-file button. The click/`y` handler in server_shortcuts.js
    // fetches /api/file?path=… and writes it to the clipboard.
    let copy_btn = format!(
        r#"<button type="button" class="copy-file-btn" title="Copy file contents (y)" data-path="{path}">⧉ Copy</button>"#,
        path = html_escape(display_path),
    );
    // GitHub-style file header: view-mode tabs (markdown only) on the left, the
    // copy/open-actions pills pushed to the right. It fuses onto the top of the
    // file body below via the `.file-panel` wrapper in the template.
    let view_toggle = if is_markdown {
        let blob = blob_url(ctx, display_path);
        format!(
            r#"<div class="md-view-toggle"><a class="md-toggle-btn{preview}" href="{blob}">Preview</a><a class="md-toggle-btn{code}" href="{blob}?plain=1">Code</a></div>"#,
            preview = if plain { "" } else { " active" },
            code = if plain { " active" } else { "" },
            blob = html_escape(&blob),
        )
    } else {
        String::new()
    };
    let toolbar = format!(
        r#"<div class="file-header">{view_toggle}<div class="file-actions">{copy_btn}{editor_btn}</div></div>"#,
    );

    let rendered_content = match text {
        None => r#"<div class="error">Binary file - cannot display</div>"#.to_string(),
        Some(text) if is_markdown => {
            if plain {
                format!(
                    r#"<div class="file-content code-view">{}</div>"#,
                    render_code_with_line_ids_for_path(&text, filename)
                )
            } else {
                let rendered = render_markdown_with_source_lines(&text);
                let parent = Path::new(display_path)
                    .parent()
                    .and_then(|p| p.to_str())
                    .unwrap_or("");
                format!(
                    r#"<div class="markdown-body">{}</div>"#,
                    rewrite_markdown_links(&rendered, ctx, parent)
                )
            }
        }
        Some(text) => format!(
            r#"<div class="file-content code-view">{}</div>"#,
            render_code_with_line_ids_for_path(&text, filename)
        ),
    };

    let commit_info = path_commit_strip_html(repo_root, display_path, ctx).await;
    let branch_chip = path_branch_chip_html(ctx);
    let breadcrumb = format!("{branch_chip}{}", path_breadcrumb_html(ctx, display_path));
    let github_href = repo_url.as_deref().map(|base| {
        format!(
            "{base}/blob/{branch}/{path}",
            branch = ctx.branch,
            path = display_path,
        )
    });
    let rail = crate::command::app_shell::app_rail_html(ctx, github_href.as_deref(), "code");
    let html = FILE_TEMPLATE
        .replace("{{TITLE}}", &html_escape(filename))
        .replace("{{ROOT_PATH}}", &html_escape(&root_path))
        .replace("{{CURRENT_PATH}}", &html_escape(display_path))
        .replace("{{APP_RAIL}}", &rail)
        .replace("{{TOOLBAR}}", &toolbar)
        .replace("{{BREADCRUMB}}", &breadcrumb)
        .replace("{{COMMIT_INFO}}", &commit_info)
        .replace("{{CONTENT}}", &rendered_content)
        .replace(
            "{{REPO_CTX_SCRIPT}}",
            &crate::command::server_shared::repo_ctx_script(
                &ctx.owner,
                &ctx.repo,
                &ctx.branch,
                repo_url.as_deref(),
                default_branch.as_deref(),
            ),
        )
        .replace(
            "{{SHARED_CSS}}",
            &crate::command::server_shared::shared_css_link(),
        )
        .replace(
            "{{SHORTCUTS_JS}}",
            &crate::command::server_shared::shortcuts_js_tag(),
        )
        .replace("{{SYNTAX_STYLES}}", render_diff_styles())
        .replace(
            "{{MERMAID_SCRIPT}}",
            &mermaid_script_block(&rendered_content),
        )
        .replace("{{LIVE_SYNC_SCRIPT}}", &live_sync_script(current_version));

    Html(html)
}

/// Render markdown to HTML with GFM support
pub(crate) fn render_markdown(text: &str) -> String {
    use comrak::{ComrakOptions, markdown_to_html};

    let mut options = ComrakOptions::default();
    options.extension.strikethrough = true;
    options.extension.tagfilter = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.superscript = false;
    options.extension.header_ids = Some("".to_string());
    options.extension.footnotes = false;
    options.extension.description_lists = false;
    options.render.unsafe_ = true; // Allow HTML in markdown

    render_mermaid_blocks(&markdown_to_html(text, &options))
}

/// Rewrite relative `href` / `src` attributes in rendered markdown HTML so they
/// resolve against the server's blob route rather than relying on the browser's
/// URL-relative resolution (which depends on whether the current URL ends in a
/// file name or a directory and breaks for nested paths). Absolute URLs,
/// fragment-only links, and known non-HTTP schemes are left untouched.
///
/// `current_dir` is the directory the markdown lives in, relative to the repo
/// root (empty for the repo root, e.g. `"docs"` for `docs/README.md`).
pub(crate) fn rewrite_markdown_links(
    html: &str,
    ctx: &RepoUrlContext,
    current_dir: &str,
) -> String {
    let re = regex::Regex::new(r#"(?i)(<(?:a|img)\b[^>]*?\s(?:href|src)\s*=\s*")([^"]*)(")"#)
        .expect("valid link regex");
    re.replace_all(html, |caps: &regex::Captures<'_>| {
        let prefix = &caps[1];
        let value = &caps[2];
        let suffix = &caps[3];
        match resolve_markdown_link(ctx, current_dir, value) {
            Some(resolved) => format!("{prefix}{resolved}{suffix}"),
            None => caps[0].to_string(),
        }
    })
    .to_string()
}

fn resolve_markdown_link(ctx: &RepoUrlContext, current_dir: &str, value: &str) -> Option<String> {
    if value.is_empty() || value.starts_with('/') || value.starts_with('#') {
        return None;
    }
    if value.contains("://")
        || value.starts_with("mailto:")
        || value.starts_with("tel:")
        || value.starts_with("javascript:")
        || value.starts_with("data:")
    {
        return None;
    }

    let (path_part, suffix) = match value.find(['#', '?']) {
        Some(idx) => (&value[..idx], &value[idx..]),
        None => (value, ""),
    };
    if path_part.is_empty() {
        return None;
    }

    let mut segments: Vec<&str> = if current_dir.is_empty() || current_dir == "." {
        Vec::new()
    } else {
        current_dir
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .collect()
    };
    for seg in path_part.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let resolved = segments.join("/");
    let url = blob_url(ctx, &resolved);
    Some(format!("{url}{suffix}"))
}

/// Render markdown to HTML, then inject `data-source-line` attributes on block-level elements.
/// This enables scroll-to-line by mapping rendered elements back to source line numbers.
pub(crate) fn render_markdown_with_source_lines(text: &str) -> String {
    let html = render_markdown(text);

    // Build a map from source lines: which line does each block-level element start at?
    // We track headings (h1-h6) and paragraphs by scanning the source for heading prefixes
    // and inserting data attributes into the rendered HTML.
    //
    // A simpler approach: wrap the entire markdown body in a container and annotate
    // heading elements with their source line numbers.
    let mut heading_lines: Vec<(usize, String)> = Vec::new(); // (1-based line, heading text)
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let heading_text = trimmed.trim_start_matches('#').trim().to_string();
            if !heading_text.is_empty() {
                heading_lines.push((i + 1, heading_text));
            }
        }
    }

    // Inject data-source-line on heading tags (h1-h6) by matching heading text
    let mut result = html;
    for (line_num, _heading_text) in heading_lines.iter().rev() {
        // Match <h1>, <h2>, etc. tags and add data-source-line attribute
        for level in 1..=6 {
            let open_tag = format!("<h{}", level);
            let replacement = format!("<h{} data-source-line=\"{}\"", level, line_num);
            // Only replace the first occurrence that doesn't already have the attribute
            if let Some(pos) = result.find(&open_tag) {
                let after = &result[pos..];
                if !after[open_tag.len()..].starts_with(" data-source-line") {
                    result = format!(
                        "{}{}{}",
                        &result[..pos],
                        replacement,
                        &result[pos + open_tag.len()..]
                    );
                    break;
                }
            }
        }
    }
    result
}

/// Render code with line IDs (`<span id="L1">`, `<span id="L2">`, etc.) for scroll-to-line support.
pub(crate) fn render_code_with_line_ids_for_path(text: &str, path: &str) -> String {
    let registry = LanguageRegistry::new();
    let highlights = registry
        .detect_by_extension(path)
        .map(|lang| highlight_text(text, lang));
    render_code_with_line_ids_highlighted(text, highlights.as_ref())
}

fn render_code_with_line_ids_highlighted(
    text: &str,
    highlights: Option<&std::collections::HashMap<usize, Vec<HighlightSpan>>>,
) -> String {
    let mut result = String::from("<table class=\"code-table\"><tbody>");
    for (i, line) in text.split('\n').enumerate() {
        let line_num = i + 1;
        let line_html = highlights
            .and_then(|spans| spans.get(&i))
            .map(|spans| render_highlighted_code_line(line, spans))
            .unwrap_or_else(|| html_escape(line));
        result.push_str(&format!(
            "<tr id=\"L{n}\" class=\"code-line\"><td class=\"code-ln\" data-line-number=\"{n}\"></td><td class=\"code-text\">{content}</td></tr>",
            n = line_num,
            content = line_html
        ));
    }
    result.push_str("</tbody></table>");
    result
}

fn render_highlighted_code_line(line: &str, spans: &[HighlightSpan]) -> String {
    let mut out = String::new();
    let len = line.len();
    if len == 0 {
        return out;
    }
    let mut active: Vec<Option<&str>> = vec![None; len];
    let mut sorted: Vec<&HighlightSpan> = spans.iter().filter(|span| span.start < len).collect();
    sorted.sort_by_key(|span| std::cmp::Reverse(span.end.saturating_sub(span.start)));
    for span in sorted {
        let start = span.start;
        let end = span.end.min(len);
        if start >= end {
            continue;
        }
        for slot in active.iter_mut().take(end).skip(start) {
            *slot = Some(span.capture_name.as_str());
        }
    }
    let mut i = 0;
    while i < len {
        let current = active[i];
        let mut j = i + 1;
        while j < len && active[j] == current {
            j += 1;
        }
        if !line.is_char_boundary(i) || !line.is_char_boundary(j) {
            out.push_str(&html_escape(&line[i..]));
            return out;
        }
        let segment = &line[i..j];
        if let Some(capture) = current {
            out.push_str(&format!(
                r#"<span class="{}">{}</span>"#,
                hl_class_attr(capture),
                html_escape(segment)
            ));
        } else {
            out.push_str(&html_escape(segment));
        }
        i = j;
    }
    out
}

fn render_mermaid_blocks(html: &str) -> String {
    let re =
        regex::Regex::new(r#"(?s)<pre><code class="language-mermaid">(?P<body>.*?)</code></pre>"#)
            .expect("valid mermaid regex");
    re.replace_all(html, r#"<pre class="mermaid">$body</pre>"#)
        .to_string()
}

pub(crate) async fn github_repo_url(repo_root: &Path) -> Option<String> {
    let remote = git_output_in_repo(repo_root, &["config", "--get", "remote.origin.url"])
        .await
        .ok()?;
    remote_to_github_url(&remote)
}

/// The repository's default branch (`main` / `master` / whatever `origin/HEAD`
/// points at). Tries `origin/HEAD` first (set by clone / `git remote set-head`),
/// then falls back to a local `main` or `master`. Returns `None` when neither
/// the remote head nor a conventional branch exists — callers then hide the
/// "open on default branch" affordance.
pub(crate) async fn default_branch_name(repo_root: &Path) -> Option<String> {
    if let Ok(out) = git_output_in_repo(
        repo_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
        && let Some(rest) = out.trim().strip_prefix("origin/")
        && !rest.is_empty()
    {
        return Some(rest.to_string());
    }
    for candidate in ["main", "master"] {
        if git_output_in_repo(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{candidate}"),
            ],
        )
        .await
        .map(|s| !s.is_empty())
        .unwrap_or(false)
        {
            return Some(candidate.to_string());
        }
    }
    None
}

pub(crate) fn remote_to_github_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let url = if remote.starts_with("git@github.com:") {
        let path = remote.strip_prefix("git@github.com:")?;
        format!("https://github.com/{}", path)
    } else if remote.starts_with("https://github.com/") || remote.starts_with("http://github.com/")
    {
        remote.to_string()
    } else {
        return None;
    };

    let url = url.strip_suffix(".git").unwrap_or(&url);
    Some(url.to_string())
}

async fn git_output_in_repo(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .args(["-c", "core.optionalLocks=false"])
        .args(args)
        .current_dir(repo_root)
        .output()
        .await
        .map_err(|e| format!("Failed to execute git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git error: {}", stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Render error message
pub(crate) fn render_error(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="UTF-8"><title>Error</title></head>
<body style="background: #f6f8fa; color: #24292f; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; padding: 20px;">
<div style="background: #ffffff; color: #cf222e; padding: 20px; border-radius: 6px; border: 1px solid #d0d7de;">
<strong>Error:</strong> {}
</div>
</body>
</html>"#,
        html_escape(message)
    )
}

/// Basic HTML escaping
pub(crate) fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Register Gargo preview server commands in the command palette
pub fn register(registry: &mut CommandRegistry) {
    registry.register(CommandEntry {
        id: "server.start_gargo_preview".into(),
        label: "Start Gargo Preview Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.start_gargo_preview".to_string(),
                },
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "server.stop_gargo_preview".into(),
        label: "Stop Gargo Preview Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.stop_gargo_preview".to_string(),
                },
            )))
        }),
    });
}

#[cfg(test)]
mod link_rewriting_tests {
    use super::*;

    fn ctx() -> RepoUrlContext {
        RepoUrlContext {
            owner: "alice".into(),
            repo: "demo".into(),
            branch: "main".into(),
        }
    }

    #[test]
    fn rewrites_relative_link_from_subdir() {
        let html = r#"<p><a href="./bar.md">bar</a></p>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(
            out,
            r#"<p><a href="/alice/demo/blob/main/docs/bar.md">bar</a></p>"#
        );
    }

    #[test]
    fn rewrites_relative_link_without_leading_dot() {
        let html = r#"<a href="bar.md">bar</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(
            out,
            r#"<a href="/alice/demo/blob/main/docs/bar.md">bar</a>"#
        );
    }

    #[test]
    fn rewrites_parent_relative_link() {
        let html = r#"<a href="../README.md">readme</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs/sub");
        assert_eq!(
            out,
            r#"<a href="/alice/demo/blob/main/docs/README.md">readme</a>"#
        );
    }

    #[test]
    fn preserves_fragment_on_relative_link() {
        let html = r#"<a href="./bar.md#section">bar</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(
            out,
            r#"<a href="/alice/demo/blob/main/docs/bar.md#section">bar</a>"#
        );
    }

    #[test]
    fn leaves_fragment_only_link_untouched() {
        let html = r##"<a href="#install">install</a>"##;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(out, html);
    }

    #[test]
    fn leaves_absolute_path_untouched() {
        let html = r#"<a href="/alice/demo/blob/main/other.md">x</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(out, html);
    }

    #[test]
    fn leaves_external_http_url_untouched() {
        let html = r#"<a href="https://example.com/page">x</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(out, html);
    }

    #[test]
    fn leaves_mailto_untouched() {
        let html = r#"<a href="mailto:a@b.com">mail</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "");
        assert_eq!(out, html);
    }

    #[test]
    fn rewrites_img_src() {
        let html = r#"<img src="./img.png" alt="x" />"#;
        let out = rewrite_markdown_links(html, &ctx(), "docs");
        assert_eq!(
            out,
            r#"<img src="/alice/demo/blob/main/docs/img.png" alt="x" />"#
        );
    }

    #[test]
    fn rewrites_relative_from_repo_root() {
        let html = r#"<a href="docs/intro.md">intro</a>"#;
        let out = rewrite_markdown_links(html, &ctx(), "");
        assert_eq!(
            out,
            r#"<a href="/alice/demo/blob/main/docs/intro.md">intro</a>"#
        );
    }
}

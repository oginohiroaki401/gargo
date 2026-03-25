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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use axum::{
    Router,
    extract::{Path as AxumPath, Query, State},
    response::{Html, IntoResponse, Json},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::input::action::{Action, AppAction, IntegrationAction};

/// Commands that can be sent to the GitHub preview server
#[derive(Debug, Clone)]
pub enum GithubPreviewCommand {
    Start { repo_root: PathBuf },
    Stop,
    SetActivePath { rel_path: Option<String> },
    RefreshActive,
    UpdateBufferContent { content: String, cursor_line: usize },
    UpdateCursorLine { line: usize },
}

/// Events emitted by the GitHub preview server
#[derive(Debug, Clone)]
pub enum GithubPreviewEvent {
    Started { port: u16 },
    Stopped,
    Detached { requested_path: String },
    Error(String),
}

/// Handle for communicating with the GitHub preview server worker thread
pub struct GithubPreviewHandle {
    pub command_tx: mpsc::Sender<GithubPreviewCommand>,
    pub event_rx: mpsc::Receiver<GithubPreviewEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl GithubPreviewHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = GithubPreviewWorker {
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
        command_tx: mpsc::Sender<GithubPreviewCommand>,
        event_rx: mpsc::Receiver<GithubPreviewEvent>,
    ) -> Self {
        Self {
            command_tx,
            event_rx,
            _worker_thread: None,
        }
    }
}

/// Worker thread that manages the Tokio runtime and HTTP server
struct GithubPreviewWorker {
    command_rx: mpsc::Receiver<GithubPreviewCommand>,
    event_tx: mpsc::Sender<GithubPreviewEvent>,
    tokio_runtime: tokio::runtime::Runtime,
    server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    server_state: Option<Arc<Mutex<PreviewServerState>>>,
    pending_active_rel_path: Option<String>,
}

impl GithubPreviewWorker {
    fn run(mut self) {
        loop {
            match self.command_rx.recv() {
                Ok(GithubPreviewCommand::Start { repo_root }) => {
                    self.handle_start_server(repo_root)
                }
                Ok(GithubPreviewCommand::Stop) => self.handle_stop_server(),
                Ok(GithubPreviewCommand::SetActivePath { rel_path }) => {
                    self.handle_set_active_path(rel_path);
                }
                Ok(GithubPreviewCommand::RefreshActive) => self.handle_refresh_active(),
                Ok(GithubPreviewCommand::UpdateBufferContent {
                    content,
                    cursor_line,
                }) => self.handle_update_buffer_content(content, cursor_line),
                Ok(GithubPreviewCommand::UpdateCursorLine { line }) => {
                    self.handle_update_cursor_line(line)
                }
                Err(_) => break, // Main thread exited
            }
        }
    }

    fn handle_start_server(&mut self, repo_root: PathBuf) {
        if self.server_shutdown_tx.is_some() {
            let _ = self.event_tx.send(GithubPreviewEvent::Error(
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
                let _ = self.event_tx.send(GithubPreviewEvent::Error(format!(
                    "Failed to bind GitHub preview server on localhost: {}",
                    err
                )));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(err) => {
                let _ = self.event_tx.send(GithubPreviewEvent::Error(format!(
                    "Failed to read GitHub preview server local address: {}",
                    err
                )));
                return;
            }
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.server_shutdown_tx = Some(shutdown_tx);

        let server_state = Arc::new(Mutex::new(PreviewServerState {
            repo_root,
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

        let _ = event_tx.send(GithubPreviewEvent::Started { port });
    }

    fn handle_stop_server(&mut self) {
        if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
            let _ = shutdown_tx.send(());
            self.server_state = None;
            let _ = self.event_tx.send(GithubPreviewEvent::Stopped);
        } else {
            let _ = self
                .event_tx
                .send(GithubPreviewEvent::Error("Server not running".to_string()));
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

struct PreviewServerState {
    repo_root: PathBuf,
    port: u16,
    active_rel_path: Option<String>,
    detached: bool,
    last_detach_path: Option<String>,
    version: u64,
    last_browser_event: Option<PreviewBrowserEvent>,
    event_tx: mpsc::Sender<GithubPreviewEvent>,
    buffer_content: Option<String>,
    cursor_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreviewBrowserEventKind {
    Navigate,
    Refresh,
    Detached,
    ScrollTo,
}

#[derive(Debug, Clone, Serialize)]
struct PreviewBrowserEvent {
    kind: PreviewBrowserEventKind,
    path: Option<String>,
    url: Option<String>,
    detached: bool,
    version: u64,
    cursor_line: Option<usize>,
}

fn preview_url_for_rel_path(port: u16, rel_path: Option<&str>) -> String {
    if let Some(rel_path) = rel_path
        && rel_path != "."
    {
        format!("http://127.0.0.1:{}/blob/{}", port, rel_path)
    } else {
        format!("http://127.0.0.1:{}/", port)
    }
}

fn broadcast_preview_event(state: &mut PreviewServerState, event: PreviewBrowserEvent) {
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

  let lastSeenVersion = readStoredVersion();
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
        const route = toRoute(payload.url);
        if (route && route !== currentRoute()) {
          window.location.assign(route);
          return;
        }
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

/// HTML template for directory listing
const DIRECTORY_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{{TITLE}}</title>
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/github-markdown-css@5/github-markdown-light.css">
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/gh/highlightjs/cdn-release@11/build/styles/github.min.css">
    <script src="https://cdn.jsdelivr.net/gh/highlightjs/cdn-release@11/build/highlight.min.js"></script>
    <script type="module">
      import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs';
      mermaid.initialize({
        startOnLoad: false,
        theme: 'default'
      });

      // Wait for DOM to be ready
      document.addEventListener('DOMContentLoaded', () => {
        // Initialize highlight.js for all code blocks except mermaid
        document.querySelectorAll('pre code').forEach((block) => {
          if (!block.classList.contains('language-mermaid')) {
            hljs.highlightElement(block);
          }
        });

        // Transform mermaid code blocks to proper structure
        document.querySelectorAll('pre code.language-mermaid').forEach((block) => {
          const pre = block.parentElement;
          const mermaidDiv = document.createElement('div');
          mermaidDiv.className = 'mermaid';
          mermaidDiv.textContent = block.textContent;
          pre.replaceWith(mermaidDiv);
        });

        // Render all mermaid diagrams
        mermaid.run();
      });
    </script>
    <style>
        body {
            margin: 0;
            padding: 20px;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            background-color: #f6f8fa;
            color: #24292f;
        }
        .container {
            max-width: 1280px;
            margin: 0 auto;
        }
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
        .context-label {
            margin: 0;
            font-size: 13px;
            font-weight: 600;
            color: #57606a;
            text-transform: uppercase;
            letter-spacing: 0.04em;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #57606a;
        }
        .context-key {
            font-weight: 600;
            color: #24292f;
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
        .breadcrumb {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 6px;
        }
        .crumb-pill {
            display: inline-flex;
            align-items: center;
            padding: 3px 10px;
            border: 1px solid #d0d7de;
            border-radius: 999px;
            background: #ffffff;
            color: #0969da;
            text-decoration: none;
            font-size: 13px;
            line-height: 1.4;
        }
        a.crumb-pill:hover {
            background: #f6f8fa;
            text-decoration: none;
        }
        .crumb-pill-muted {
            color: #57606a;
        }
        .crumb-pill-current {
            color: #24292f;
            background: #f6f8fa;
        }
        .crumb-separator {
            color: #8c959f;
            font-size: 13px;
        }
        .file-list {
            background: #ffffff;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            overflow: hidden;
        }
        .file-item {
            display: grid;
            grid-template-columns: 20px 1fr;
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
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <div class="context-label">Repository browser</div>
            <div class="context-row"><span class="context-key">Root</span><code>{{ROOT_PATH}}</code></div>
            <div class="context-row"><span class="context-key">Showing</span><code>{{CURRENT_PATH}}</code></div>
            <div class="breadcrumb">{{BREADCRUMB}}</div>
        </div>
        {{CONTENT}}
    </div>
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
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/github-markdown-css@5/github-markdown-light.css">
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/gh/highlightjs/cdn-release@11/build/styles/github.min.css">
    <script src="https://cdn.jsdelivr.net/gh/highlightjs/cdn-release@11/build/highlight.min.js"></script>
    <script type="module">
      import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs';
      mermaid.initialize({
        startOnLoad: false,
        theme: 'default'
      });

      // Wait for DOM to be ready
      document.addEventListener('DOMContentLoaded', () => {
        // Initialize highlight.js for all code blocks except mermaid
        document.querySelectorAll('pre code').forEach((block) => {
          if (!block.classList.contains('language-mermaid')) {
            hljs.highlightElement(block);
          }
        });

        // Transform mermaid code blocks to proper structure
        document.querySelectorAll('pre code.language-mermaid').forEach((block) => {
          const pre = block.parentElement;
          const mermaidDiv = document.createElement('div');
          mermaidDiv.className = 'mermaid';
          mermaidDiv.textContent = block.textContent;
          pre.replaceWith(mermaidDiv);
        });

        // Render all mermaid diagrams
        mermaid.run();
      });
    </script>
    <style>
        body {
            margin: 0;
            padding: 20px;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif;
            background-color: #f6f8fa;
            color: #24292f;
        }
        .container {
            max-width: 1280px;
            margin: 0 auto;
        }
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
        .context-label {
            margin: 0;
            font-size: 13px;
            font-weight: 600;
            color: #57606a;
            text-transform: uppercase;
            letter-spacing: 0.04em;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #57606a;
        }
        .context-key {
            font-weight: 600;
            color: #24292f;
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
        .breadcrumb {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 6px;
        }
        .crumb-pill {
            display: inline-flex;
            align-items: center;
            padding: 3px 10px;
            border: 1px solid #d0d7de;
            border-radius: 999px;
            background: #ffffff;
            color: #0969da;
            text-decoration: none;
            font-size: 13px;
            line-height: 1.4;
        }
        a.crumb-pill:hover {
            background: #f6f8fa;
            text-decoration: none;
        }
        .crumb-pill-muted {
            color: #57606a;
        }
        .crumb-pill-current {
            color: #24292f;
            background: #f6f8fa;
        }
        .crumb-separator {
            color: #8c959f;
            font-size: 13px;
        }
        .markdown-body {
            background: #ffffff;
            padding: 20px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
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
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <div class="context-label">Repository browser</div>
            <div class="context-row"><span class="context-key">Root</span><code>{{ROOT_PATH}}</code></div>
            <div class="context-row"><span class="context-key">Showing</span><code>{{CURRENT_PATH}}</code></div>
            <div class="breadcrumb">{{BREADCRUMB}}</div>
        </div>
        {{CONTENT}}
    </div>
    {{LIVE_SYNC_SCRIPT}}
</body>
</html>"#;

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<Mutex<PreviewServerState>>,
) {
    let app = Router::new()
        .route("/", get(handle_root))
        .route("/events", get(handle_events))
        .route("/tree/{*path}", get(handle_tree))
        .route("/blob/{*path}", get(handle_blob))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    since: Option<u64>,
}

#[derive(Debug, Serialize)]
struct EventsResponse {
    event: Option<PreviewBrowserEvent>,
}

async fn handle_events(
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

/// Handle root path - show directory listing
async fn handle_root(State(state): State<Arc<Mutex<PreviewServerState>>>) -> impl IntoResponse {
    maybe_emit_detached_event(&state, ".");

    let repo_root = state.lock().unwrap().repo_root.clone();

    // Always show directory listing at root
    handle_directory_listing(&repo_root, ".", &repo_root).await
}

/// Handle tree view (directory listing)
async fn handle_tree(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    AxumPath(path): AxumPath<String>,
) -> impl IntoResponse {
    let repo_root = state.lock().unwrap().repo_root.clone();

    let full_path = repo_root.join(&path);

    // Security: ensure path is within repo
    if !full_path.starts_with(&repo_root) {
        return Html(render_error("Invalid path"));
    }

    maybe_emit_detached_event(&state, &path);

    handle_directory_listing(&full_path, &path, &repo_root).await
}

/// Handle blob view (file content)
async fn handle_blob(
    State(state): State<Arc<Mutex<PreviewServerState>>>,
    AxumPath(path): AxumPath<String>,
) -> impl IntoResponse {
    let (repo_root, buffer_content) = {
        let st = state.lock().unwrap();
        let bc = if st
            .active_rel_path
            .as_deref()
            .map(|a| normalize_rel_path_for_compare(a))
            == Some(normalize_rel_path_for_compare(&path))
        {
            st.buffer_content.clone()
        } else {
            None
        };
        (st.repo_root.clone(), bc)
    };

    let full_path = repo_root.join(&path);

    // Security: ensure path is within repo
    if !full_path.starts_with(&repo_root) {
        return Html(render_error("Invalid path"));
    }

    maybe_emit_detached_event(&state, &path);

    handle_file_display(&full_path, &path, &repo_root, buffer_content.as_deref()).await
}

fn normalize_rel_path_for_compare(path: &str) -> String {
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
    let mut to_emit: Option<mpsc::Sender<GithubPreviewEvent>> = None;

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
        let _ = event_tx.send(GithubPreviewEvent::Detached {
            requested_path: requested,
        });
    }
}

/// Get repository root directory
/// Render directory listing
async fn handle_directory_listing(
    path: &Path,
    display_path: &str,
    repo_root: &Path,
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

    let mut file_list = String::from("<div class=\"file-list\">");

    // Add parent directory link if not at root
    if display_path != "." {
        let parent = Path::new(display_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".");
        let parent_url = if parent == "." {
            "/".to_string()
        } else {
            format!("/tree/{}", parent)
        };
        file_list.push_str(&format!(
            r#"<div class="file-item"><div class="file-icon">📁</div><div class="file-name"><a href="{}">.. (parent directory)</a></div></div>"#,
            parent_url
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
            r#"<div class="file-item"><div class="file-icon">📁</div><div class="file-name"><a href="/tree/{}">{}</a></div></div>"#,
            html_escape(&link_path),
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
            r#"<div class="file-item"><div class="file-icon">📄</div><div class="file-name"><a href="/blob/{}">{}</a></div></div>"#,
            html_escape(&link_path),
            html_escape(&file)
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
        content.push_str(r#"<div style="margin-top: 24px;"><div class="markdown-body">"#);
        content.push_str(&html);
        content.push_str("</div></div>");
    }

    let github_url = github_url_for_path(repo_root, display_path, true).await;
    let breadcrumb = render_breadcrumb(display_path, true, github_url.as_deref());
    let title = if display_path == "." {
        "Repository Root".to_string()
    } else {
        display_path.to_string()
    };
    let root_path = repo_root.display().to_string();

    let html = DIRECTORY_TEMPLATE
        .replace("{{TITLE}}", &html_escape(&title))
        .replace("{{ROOT_PATH}}", &html_escape(&root_path))
        .replace("{{CURRENT_PATH}}", &html_escape(display_path))
        .replace("{{BREADCRUMB}}", &breadcrumb)
        .replace("{{CONTENT}}", &content)
        .replace("{{LIVE_SYNC_SCRIPT}}", LIVE_SYNC_SCRIPT);

    Html(html)
}

/// Render file content
async fn handle_file_display(
    path: &Path,
    display_path: &str,
    repo_root: &Path,
    buffer_content: Option<&str>,
) -> Html<String> {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let is_markdown = filename.ends_with(".md");

    // Use buffer content if available, otherwise read from disk
    let rendered_content = if let Some(text) = buffer_content {
        if is_markdown {
            let html = render_markdown_with_source_lines(text);
            format!(r#"<div class="markdown-body">{}</div>"#, html)
        } else {
            let language = detect_language(filename);
            format!(
                r#"<div class="file-content"><pre><code class="language-{}">{}</code></pre></div>"#,
                language,
                render_code_with_line_ids(text)
            )
        }
    } else {
        let content = match tokio::fs::read(path).await {
            Ok(content) => content,
            Err(e) => return Html(render_error(&format!("Failed to read file: {}", e))),
        };
        if is_markdown {
            let text = String::from_utf8_lossy(&content);
            let html = render_markdown_with_source_lines(&text);
            format!(r#"<div class="markdown-body">{}</div>"#, html)
        } else {
            match String::from_utf8(content) {
                Ok(text) => {
                    let language = detect_language(filename);
                    format!(
                        r#"<div class="file-content"><pre><code class="language-{}">{}</code></pre></div>"#,
                        language,
                        render_code_with_line_ids(&text)
                    )
                }
                Err(_) => r#"<div class="error">Binary file - cannot display</div>"#.to_string(),
            }
        }
    };

    let github_url = github_url_for_path(repo_root, display_path, false).await;
    let breadcrumb = render_breadcrumb(display_path, false, github_url.as_deref());
    let root_path = repo_root.display().to_string();

    let html = FILE_TEMPLATE
        .replace("{{TITLE}}", &html_escape(filename))
        .replace("{{ROOT_PATH}}", &html_escape(&root_path))
        .replace("{{CURRENT_PATH}}", &html_escape(display_path))
        .replace("{{BREADCRUMB}}", &breadcrumb)
        .replace("{{CONTENT}}", &rendered_content)
        .replace("{{LIVE_SYNC_SCRIPT}}", LIVE_SYNC_SCRIPT);

    Html(html)
}

/// Render markdown to HTML with GFM support
fn render_markdown(text: &str) -> String {
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

    markdown_to_html(text, &options)
}

/// Render markdown to HTML, then inject `data-source-line` attributes on block-level elements.
/// This enables scroll-to-line by mapping rendered elements back to source line numbers.
fn render_markdown_with_source_lines(text: &str) -> String {
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
fn render_code_with_line_ids(text: &str) -> String {
    let mut result = String::new();
    for (i, line) in text.split('\n').enumerate() {
        let line_num = i + 1;
        result.push_str(&format!(
            "<span id=\"L{}\" class=\"code-line\">{}</span>\n",
            line_num,
            html_escape(line)
        ));
    }
    result
}

/// Detect programming language from filename
fn detect_language(filename: &str) -> &str {
    if filename.ends_with(".rs") {
        "rust"
    } else if filename.ends_with(".js") {
        "javascript"
    } else if filename.ends_with(".ts") {
        "typescript"
    } else if filename.ends_with(".py") {
        "python"
    } else if filename.ends_with(".java") {
        "java"
    } else if filename.ends_with(".c") || filename.ends_with(".h") {
        "c"
    } else if filename.ends_with(".cpp") || filename.ends_with(".hpp") {
        "cpp"
    } else if filename.ends_with(".go") {
        "go"
    } else if filename.ends_with(".html") {
        "html"
    } else if filename.ends_with(".css") {
        "css"
    } else if filename.ends_with(".json") {
        "json"
    } else if filename.ends_with(".yaml") || filename.ends_with(".yml") {
        "yaml"
    } else if filename.ends_with(".toml") {
        "toml"
    } else if filename.ends_with(".sh") {
        "bash"
    } else {
        "plaintext"
    }
}

/// Render breadcrumb navigation
fn render_breadcrumb(path: &str, is_tree: bool, github_url: Option<&str>) -> String {
    let mut crumbs = vec![r#"<a class="crumb-pill" href="/">Root</a>"#.to_string()];

    if let Some(url) = github_url {
        crumbs.push(format!(
            r#"<a class="crumb-pill" href="{}" target="_blank" rel="noopener noreferrer">GitHub</a>"#,
            html_escape(url)
        ));
    } else {
        crumbs.push(r#"<span class="crumb-pill crumb-pill-muted">GitHub</span>"#.to_string());
    }

    if path != "." {
        let mut current_path = String::new();
        let segments: Vec<&str> = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();
        for (i, segment) in segments.iter().enumerate() {
            if !current_path.is_empty() {
                current_path.push('/');
            }
            current_path.push_str(segment);

            if i == segments.len() - 1 && !is_tree {
                crumbs.push(format!(
                    r#"<span class="crumb-pill crumb-pill-current">{}</span>"#,
                    html_escape(segment)
                ));
            } else {
                crumbs.push(format!(
                    r#"<a class="crumb-pill" href="/tree/{}">{}</a>"#,
                    html_escape(&current_path),
                    html_escape(segment)
                ));
            }
        }
    }

    crumbs.join(r#"<span class="crumb-separator">/</span>"#)
}

async fn github_url_for_path(repo_root: &Path, path: &str, is_tree: bool) -> Option<String> {
    let remote = git_output_in_repo(repo_root, &["config", "--get", "remote.origin.url"])
        .await
        .ok()?;
    let base = remote_to_github_url(&remote)?;
    let branch = match current_branch_in_repo(repo_root).await {
        Some(branch) => branch,
        None => default_branch_in_repo(repo_root)
            .await
            .unwrap_or_else(|| "main".to_string()),
    };

    let route = if is_tree { "tree" } else { "blob" };
    if path == "." && is_tree {
        Some(format!("{}/{}/{}", base, route, branch))
    } else {
        Some(format!("{}/{}/{}/{}", base, route, branch, path))
    }
}

async fn current_branch_in_repo(repo_root: &Path) -> Option<String> {
    let branch = git_output_in_repo(repo_root, &["branch", "--show-current"])
        .await
        .ok()?;
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

async fn default_branch_in_repo(repo_root: &Path) -> Option<String> {
    let symbolic_ref = git_output_in_repo(repo_root, &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .await
        .ok()?;
    let branch = symbolic_ref
        .strip_prefix("refs/remotes/origin/")
        .unwrap_or(&symbolic_ref);
    Some(branch.to_string())
}

fn remote_to_github_url(remote: &str) -> Option<String> {
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
fn render_error(message: &str) -> String {
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
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Register GitHub preview server commands in the command palette
pub fn register(registry: &mut CommandRegistry) {
    registry.register(CommandEntry {
        id: "server.start_github_preview".into(),
        label: "Start GitHub Preview Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.start_github_preview".to_string(),
                },
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "server.stop_github_preview".into(),
        label: "Stop GitHub Preview Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.stop_github_preview".to_string(),
                },
            )))
        }),
    });
}

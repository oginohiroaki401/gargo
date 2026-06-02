//! Diff server for viewing git status and diffs in a browser with rich formatting.
//!
//! This module implements an HTTP server for a tig-like git status page.
//! It follows the async runtime pattern:
//! - Command enum for controlling the server
//! - Event enum for status updates
//! - Handle with mpsc channels for communication
//! - Worker that runs on separate thread with Tokio runtime

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Json, Response},
    routing::{get, post},
};
use tower_http::cors::CorsLayer;

use crate::command::diff_viewed::{PAGE_COMPARE, PAGE_STATUS, ViewedStore};
use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::diff_render::{
    DiffFile, DiffHighlights, FileStatus, LineHighlights, LineKind, content_hash_of,
    content_hash_of_bytes, parse_unified_diff, render_diff_styles, render_file_body_html,
    render_file_body_html_with_highlights,
};
use crate::input::action::{Action, AppAction, IntegrationAction};
use crate::split_render::{LineHl, build_split_rows, render_split_html, render_split_styles};
use crate::syntax::highlight::highlight_text;
use crate::syntax::language::{LanguageDef, LanguageRegistry};

/// Commands that can be sent to the diff server
#[derive(Debug, Clone)]
pub enum DiffServerCommand {
    Start {
        project_root: PathBuf,
        /// Optional override for gargo's data dir. Production callers pass
        /// `None` (uses `~/.local/share/gargo`); tests pass a temp dir so the
        /// viewed-state database stays isolated.
        data_dir: Option<PathBuf>,
    },
    Stop,
}

/// Events emitted by the diff server
#[derive(Debug, Clone)]
pub enum DiffServerEvent {
    Started { port: u16 },
    Stopped,
    Error(String),
}

/// Handle for communicating with the diff server worker thread
pub struct DiffServerHandle {
    pub command_tx: mpsc::Sender<DiffServerCommand>,
    pub event_rx: mpsc::Receiver<DiffServerEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl DiffServerHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = DiffServerWorker {
            command_rx,
            event_tx,
            tokio_runtime: tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to build tokio runtime: {}", e))?,
            server_shutdown_tx: None,
        };

        let worker_thread = thread::Builder::new()
            .name("diff-server".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("Failed to spawn worker thread: {}", e))?;

        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }
}

/// Worker thread that manages the Tokio runtime and HTTP server
struct DiffServerWorker {
    command_rx: mpsc::Receiver<DiffServerCommand>,
    event_tx: mpsc::Sender<DiffServerEvent>,
    tokio_runtime: tokio::runtime::Runtime,
    server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl DiffServerWorker {
    fn run(mut self) {
        loop {
            match self.command_rx.recv() {
                Ok(DiffServerCommand::Start {
                    project_root,
                    data_dir,
                }) => {
                    self.handle_start_server(project_root, data_dir);
                }
                Ok(DiffServerCommand::Stop) => self.handle_stop_server(),
                Err(_) => break, // Main thread exited
            }
        }
    }

    fn handle_start_server(&mut self, project_root: PathBuf, data_dir: Option<PathBuf>) {
        if self.server_shutdown_tx.is_some() {
            let _ = self
                .event_tx
                .send(DiffServerEvent::Error("Server already running".to_string()));
            return;
        }

        let listener = match self
            .tokio_runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        {
            Ok(listener) => listener,
            Err(err) => {
                let _ = self.event_tx.send(DiffServerEvent::Error(format!(
                    "Failed to bind diff server on localhost: {}",
                    err
                )));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(err) => {
                let _ = self.event_tx.send(DiffServerEvent::Error(format!(
                    "Failed to read diff server local address: {}",
                    err
                )));
                return;
            }
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.server_shutdown_tx = Some(shutdown_tx);

        let viewed = match data_dir {
            Some(dir) => ViewedStore::open_in_dir(&dir),
            None => ViewedStore::open(),
        };
        let server_state = Arc::new(DiffServerState {
            project_root: std::fs::canonicalize(&project_root).unwrap_or(project_root),
            viewed,
        });
        let event_tx = self.event_tx.clone();
        self.tokio_runtime.spawn(async move {
            run_server(listener, shutdown_rx, server_state).await;
        });

        let _ = event_tx.send(DiffServerEvent::Started { port });
    }

    fn handle_stop_server(&mut self) {
        if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
            let _ = shutdown_tx.send(());
            let _ = self.event_tx.send(DiffServerEvent::Stopped);
        } else {
            let _ = self
                .event_tx
                .send(DiffServerEvent::Error("Server not running".to_string()));
        }
    }
}

pub(crate) struct DiffServerState {
    pub(crate) project_root: PathBuf,
    /// On-disk persistence for per-file "Viewed" checkboxes.
    pub(crate) viewed: ViewedStore,
}

impl DiffServerState {
    /// Stable key for this repo in the viewed-state database.
    fn repo_key(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }
}

/// HTML template with diff2html integration
const DIFF_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Git Diff</title>
    <style>
{{SHARED_CSS}}
        .repo-controls { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; padding: 10px 14px; border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; margin-bottom: 16px; }
        .repo-controls input[list] { min-width: 220px; padding: 4px 8px; border: 1px solid #d0d7de; border-radius: 6px; background: #fff; font: inherit; }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #4b5563;
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
        .controls { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
        .controls label { font-size: 14px; display: flex; align-items: center; gap: 8px; }
        .controls select, .controls button {
            padding: 6px 10px;
            border: 1px solid #ccc;
            border-radius: 6px;
            background: white;
            font-size: 14px;
        }
        .controls button { cursor: pointer; }
        #error-banner {
            display: none;
            margin: 0 0 20px 0;
            padding: 12px 16px;
            border: 1px solid #fcc;
            border-radius: 8px;
            background: #fee;
            color: #b20000;
        }
        .section {
            background: white;
            padding: 16px;
            margin-bottom: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
        }
        .section h2 { margin: 0 0 12px 0; font-size: 18px; }
        .loading, .empty { padding: 20px; color: #666; }
        .file-list { list-style: none; margin: 0; padding: 0; }
        .file-list li { margin: 2px 0; }
        .file-list a {
            display: flex;
            align-items: center;
            gap: 6px;
            color: #0a58ca;
            text-decoration: none;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            padding: 2px 4px;
            border-radius: 4px;
        }
        .file-list a:hover { background: #f6f8fa; text-decoration: underline; }
        .file-list .file-path-text {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            flex: 1 1 auto;
            min-width: 0;
        }
        .file-status { display: inline-block; width: 1.2em; text-align: center; font-weight: 700; flex: 0 0 1.2em; }
        .file-status.staged    { color: #2da44e; }
        .file-status.changed   { color: #d29922; }
        .file-status.untracked { color: #8b949e; }
        .repo-controls-spacer { flex: 1 1 auto; }
        .commit-link {
            display: inline-flex;
            align-items: center;
            padding: 6px 14px;
            border: 1px solid rgba(31,136,61,0.4);
            border-radius: 6px;
            background: #1f883d;
            color: #fff;
            font-size: 14px;
            font-weight: 500;
            text-decoration: none;
        }
        .commit-link:hover { background: #1a7f37; }
        .sidebar-group { margin-bottom: 14px; }
        .sidebar-group-header {
            display: flex;
            align-items: center;
            gap: 6px;
            padding: 4px 4px;
            margin-bottom: 2px;
            border-bottom: 1px solid #eaeef2;
            font-size: 12px;
            text-transform: uppercase;
            letter-spacing: 0.03em;
            color: #57606a;
        }
        .sidebar-group-title { font-weight: 600; flex: 1 1 auto; }
        .sidebar-group-count {
            flex-shrink: 0;
            padding: 0 6px;
            border-radius: 999px;
            background: #eaeef2;
            color: #57606a;
            font-size: 11px;
        }
        .stage-btn {
            flex-shrink: 0;
            margin-left: auto;
            padding: 2px 10px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 12px;
            cursor: pointer;
        }
        .stage-btn:hover { background: #eef2f7; border-color: #afb8c1; }
        .stage-btn:disabled { opacity: 0.5; cursor: default; }
        .file-tree { list-style: none; margin: 0; padding: 0; }
        .file-tree li { margin: 0; }
        .tree-dir { margin: 0; }
        .tree-dir-children {
            list-style: none;
            margin: 0;
            padding-left: 16px;
        }
        .tree-dir.tree-dir-collapsed > .tree-dir-children { display: none; }
        .tree-dir-header {
            display: flex;
            align-items: center;
            gap: 4px;
            padding: 2px 4px;
            border-radius: 4px;
            cursor: pointer;
            user-select: none;
        }
        .tree-dir-header:hover { background: #f6f8fa; }
        .tree-dir-toggle {
            flex-shrink: 0;
            padding: 0 4px;
            border: none;
            background: transparent;
            color: #57606a;
            font-size: 12px;
            line-height: 1;
            cursor: pointer;
        }
        .tree-dir-name {
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            font-weight: 600;
            color: #1f2328;
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            min-width: 0;
            flex: 1 1 auto;
        }
        .tree-dir-count {
            flex-shrink: 0;
            font-size: 11px;
            color: #57606a;
        }
        .tree-file a {
            display: flex;
            align-items: center;
            gap: 6px;
            color: #0a58ca;
            text-decoration: none;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            padding: 2px 4px;
            border-radius: 4px;
        }
        .tree-file a:hover { background: #f6f8fa; text-decoration: underline; }
        .tree-file .file-path-text {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            flex: 1 1 auto;
            min-width: 0;
        }
        .gr-file {
            background: white;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            margin-bottom: 12px;
            overflow: hidden;
        }
        .gr-file-header {
            display: flex;
            align-items: center;
            flex-wrap: nowrap;
            gap: 8px;
            min-width: 0;
            padding: 8px 12px;
            background: #f6f8fa;
            border-bottom: 1px solid #d0d7de;
        }
        .gr-file-collapsed .gr-file-body { display: none; }
        .gr-file-collapsed .gr-file-header { border-bottom: none; }
        .gr-large-tag { display: none; flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; background: #fff1e5; color: #bc4c00; }
        .gr-file-large.gr-file-collapsed .gr-large-tag { display: inline-flex; }
        .gr-file-viewed .gr-file-header { background: #eef2f7; opacity: 0.85; }
        .gr-file-name-wrapper {
            flex: 1 1 auto;
            min-width: 0;
            display: flex;
            align-items: center;
            gap: 8px;
            overflow: hidden;
        }
        .gr-file-name {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 13px;
        }
        a.gr-file-name { color: #0969da; text-decoration: none; }
        a.gr-file-name:hover { text-decoration: underline; }
        .gr-status-tag {
            flex-shrink: 0;
            padding: 1px 6px;
            border-radius: 4px;
            font-size: 11px;
            font-weight: 600;
        }
        .gr-status-modified  { background: #fff8c5; color: #9a6700; }
        .gr-status-added     { background: #dafbe1; color: #1a7f37; }
        .gr-status-deleted   { background: #ffebe9; color: #cf222e; }
        .gr-status-renamed   { background: #ddf4ff; color: #0969da; }
        .gr-status-untracked { background: #eaeef2; color: #57606a; }
        /* Staged files get a green left rail + a "staged" pill so they read as
         * committed-ready at a glance, distinct from unstaged working changes. */
        .gr-staged-badge {
            flex-shrink: 0;
            display: inline-flex;
            align-items: center;
            gap: 3px;
            padding: 1px 7px;
            border-radius: 999px;
            font-size: 11px;
            font-weight: 600;
            background: #1f883d;
            color: #fff;
        }
        .gr-staged-badge::before { content: "\2713"; font-size: 10px; }
        .gr-file[data-section="staged"] { border-left: 3px solid #1f883d; }
        .gr-file[data-section="staged"] > .gr-file-header { background: #f0f8f2; }
        .gr-file-stats { flex-shrink: 0; display: inline-flex; gap: 8px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
        .gr-additions { color: #1a7f37; }
        .gr-deletions { color: #cf222e; }
        .diff-toggle-btn {
            order: -1;
            flex-shrink: 0;
            padding: 0 6px;
            border: 1px solid transparent;
            border-radius: 6px;
            background: transparent;
            color: #57606a;
            font-size: 14px;
            line-height: 1.2;
            cursor: pointer;
        }
        .diff-toggle-btn:hover {
            background: #eef2f7;
            border-color: #d0d7de;
            color: #24292f;
        }
        .diff-viewed-label {
            margin-left: auto;
            flex-shrink: 0;
            display: inline-flex;
            align-items: center;
            gap: 6px;
            padding: 2px 8px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 12px;
            cursor: pointer;
            user-select: none;
        }
        .diff-viewed-label:hover { background: #eef2f7; }
        .diff-viewed-label input { margin: 0; cursor: pointer; }
        .gr-file-body { background: white; }
        .gr-file-body .loading, .gr-file-body .empty { padding: 12px; color: #57606a; font-size: 12px; }
{{DIFF_STYLES}}
        #bottom-controls {
            position: fixed;
            right: 20px;
            bottom: 20px;
            z-index: 1000;
            display: flex;
            align-items: center;
            gap: 8px;
        }
        #viewed-counter {
            padding: 8px 12px;
            border: 1px solid #ccc;
            border-radius: 8px;
            background: white;
            color: #57606a;
            font-size: 13px;
            font-variant-numeric: tabular-nums;
            white-space: nowrap;
            box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
        }
        #go-top-btn {
            padding: 8px 12px;
            border: 1px solid #ccc;
            border-radius: 8px;
            background: white;
            color: #24292f;
            font-size: 14px;
            cursor: pointer;
            opacity: 0;
            pointer-events: none;
            transform: translateY(8px);
            transition: opacity 0.15s ease, transform 0.15s ease;
        }
        #go-top-btn.visible { opacity: 1; pointer-events: auto; transform: translateY(0); }
        #go-top-btn:hover { background: #eef2f7; }
    </style>
</head>
<body data-page="status">
{{REPO_CTX_SCRIPT}}
<script>{{SHORTCUTS_JS}}</script>
<code id="root-path" hidden>{{ROOT_PATH}}</code>
<div class="app-shell">
    {{APP_RAIL}}
    <main class="app-main">
    <div class="repo-controls">
        <label>
            <input type="checkbox" id="show-untracked">
            Show untracked files
        </label>
        <button id="refresh-btn" type="button">Refresh</button>
        <span class="repo-controls-spacer"></span>
        <a id="commit-link" class="commit-link" href="/commit">Commit…</a>
    </div>

    <div id="error-banner"></div>

    <div class="layout">
        <aside class="sidebar">
            <section class="section files-section">
                <h2 id="files-heading">Files</h2>
                <div id="files-list"><div class="loading">Loading files...</div></div>
            </section>
        </aside>
        <main class="content">
            <section class="section">
                <h2>Diff</h2>
                <div id="files-main"><div class="loading">Loading files...</div></div>
            </section>
        </main>
    </div>
    <div id="bottom-controls">
        <span id="viewed-counter" title="Files marked as viewed / total files"></span>
        <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>
    </div>

    <script>
        const urlParams = new URLSearchParams(window.location.search);
        const parseBoolParam = (value, defaultValue) => value === null ? defaultValue : value === "true";

        const showUntrackedToggle = document.getElementById("show-untracked");
        const refreshButton = document.getElementById("refresh-btn");
        const errorBanner = document.getElementById("error-banner");
        const rootPathCode = document.getElementById("root-path");
        const filesHeading = document.getElementById("files-heading");
        const filesListContainer = document.getElementById("files-list");
        const filesMain = document.getElementById("files-main");
        const goTopButton = document.getElementById("go-top-btn");
        const viewedCounter = document.getElementById("viewed-counter");
        const AUTO_REFRESH_INTERVAL_MS = 2000;
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.diff.collapsed.v3:${STORAGE_ROOT}`;
        const EXPANDED_FILES_STORAGE_KEY = `gargo.diff.expanded.v1:${STORAGE_ROOT}`;
        const SIDEBAR_COLLAPSED_KEY = `gargo.diff.sidebar.collapsed.v1:${STORAGE_ROOT}`;
        // Diffs with at least this many changed lines (additions + deletions)
        // are collapsed by default so the browser stays responsive. The user
        // can still expand them, and that choice is remembered per session.
        const HUGE_DIFF_LINES = 1000;

        showUntrackedToggle.checked = parseBoolParam(urlParams.get("show_untracked"), true);

        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let expandedFileIds = loadIdSet(sessionStorage, EXPANDED_FILES_STORAGE_KEY);
        let sidebarCollapsedDirs = loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY);
        const bodyCache = new Map();
        // fileId -> optimistic viewed state while its set-viewed POST is in
        // flight; cleared once the server's reported state agrees.
        const pendingViewed = new Map();
        let isLoading = false;
        let latestStatus = null;

        const STATUS_LABELS = {
            modified: "CHANGED", added: "ADDED", deleted: "DELETED",
            renamed: "RENAMED", untracked: "UNTRACKED",
        };
        const STATUS_CSS = {
            modified: "gr-status-modified", added: "gr-status-added", deleted: "gr-status-deleted",
            renamed: "gr-status-renamed", untracked: "gr-status-untracked",
        };
        const SECTION_LIST_CSS = { staged: "staged", unstaged: "changed", untracked: "untracked" };
        const SECTION_LIST_BADGE = { staged: "S", unstaged: "M", untracked: "?" };

        function loadIdSet(storage, key) {
            try {
                const raw = storage.getItem(key);
                if (!raw) return new Set();
                const parsed = JSON.parse(raw);
                if (!Array.isArray(parsed)) return new Set();
                return new Set(parsed.filter((v) => typeof v === "string" && v.length > 0));
            } catch (_e) { return new Set(); }
        }
        const persistIdSet = (storage, key, set) => {
            try { storage.setItem(key, JSON.stringify(Array.from(set))); } catch (_e) {}
        };
        const persistCollapsedFileIds = () => persistIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY, collapsedFileIds);
        const persistExpandedFileIds = () => persistIdSet(sessionStorage, EXPANDED_FILES_STORAGE_KEY, expandedFileIds);
        const persistSidebarCollapsedDirs = () => persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY, sidebarCollapsedDirs);
        // A diff is "huge" once its changed-line count crosses the threshold.
        const isHugeDiff = (meta) => !!meta
            && ((meta.additions || 0) + (meta.deletions || 0)) >= HUGE_DIFF_LINES;
        // Whether a file should start collapsed: explicitly collapsed, viewed,
        // or a huge diff the user has not explicitly chosen to expand.
        const shouldCollapseByDefault = (fileId, meta, isViewed) =>
            isViewed
            || collapsedFileIds.has(fileId)
            || (isHugeDiff(meta) && !expandedFileIds.has(fileId));

        const fileObserver = (typeof IntersectionObserver !== "undefined")
            ? new IntersectionObserver((entries) => {
                for (const e of entries) {
                    if (e.isIntersecting) {
                        const wrapper = e.target;
                        if (!wrapper.classList.contains("gr-file-collapsed")
                            && !wrapper.classList.contains("gr-file-viewed")) {
                            ensureBodyLoaded(wrapper).catch((err) => showError(err.message));
                        }
                    }
                }
            }, { rootMargin: "600px 0px", threshold: 0 })
            : null;

        const showError = (message) => {
            errorBanner.textContent = `Error: ${message}`;
            errorBanner.style.display = "block";
        };
        const clearError = () => {
            errorBanner.textContent = "";
            errorBanner.style.display = "none";
        };
        const setLoading = (container, message) => {
            container.innerHTML = `<div class="loading">${escapeHtml(message)}</div>`;
        };
        const setEmpty = (container, message) => {
            container.innerHTML = `<div class="empty">${escapeHtml(message)}</div>`;
        };
        const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({
            "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;",
        }[c]));
        const fileIdOf = (section, path) => `${section}:${path}`;
        const fileAnchorOf = (section, path) => `file-${section}-${path.replace(/[^A-Za-z0-9_\-\.]/g, "_")}`;

        // Build a Code-page link (blob view) for a file. Returns a span when the
        // repo context is unknown so untracked-but-orphaned states still render.
        function buildFileNameLink(path) {
            const ctx = window.__GARGO_REPO_CTX__;
            const name = (ctx && ctx.owner && ctx.repo && ctx.branch)
                ? document.createElement("a") : document.createElement("span");
            name.className = "gr-file-name";
            name.textContent = path;
            if (name.tagName === "A") {
                const segs = String(path).split("/").map(encodeURIComponent).join("/");
                name.href = `/${encodeURIComponent(ctx.owner)}/${encodeURIComponent(ctx.repo)}/blob/${encodeURIComponent(ctx.branch)}/${segs}`;
            }
            return name;
        }

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) goTopButton.classList.add("visible");
            else goTopButton.classList.remove("visible");
        };
        // Reflect how many of the listed files are marked "Viewed" next to
        // the go-to-top button. Counts live DOM rows so it stays correct
        // after refreshes, viewed toggles, and base/compare changes.
        const updateViewedCounter = () => {
            const rows = filesMain.querySelectorAll(".gr-file");
            const total = rows.length;
            if (total === 0) {
                viewedCounter.style.display = "none";
                return;
            }
            let viewed = 0;
            for (const row of rows) {
                if (row.classList.contains("gr-file-viewed")) viewed += 1;
            }
            viewedCounter.style.display = "";
            viewedCounter.textContent = `viewed ${viewed} / ${total}`;
        };
        const renderDiffToggleButtonLabel = (button, collapsed) => {
            button.textContent = collapsed ? "▸" : "▾";
            button.setAttribute("aria-expanded", collapsed ? "false" : "true");
            button.setAttribute("aria-label", collapsed ? "Show diff" : "Hide diff");
            button.setAttribute("title", collapsed ? "Show diff" : "Hide diff");
        };

        function createFileRow(section, meta) {
            const fileId = fileIdOf(section, meta.path);
            const wrapper = document.createElement("div");
            wrapper.className = "gr-file";
            wrapper.dataset.diffFileId = fileId;
            wrapper.dataset.section = section;
            wrapper.dataset.path = meta.path;
            wrapper.id = fileAnchorOf(section, meta.path);

            const header = document.createElement("div");
            header.className = "gr-file-header";

            const toggleButton = document.createElement("button");
            toggleButton.type = "button";
            toggleButton.className = "diff-toggle-btn";
            toggleButton.addEventListener("click", () => {
                const wasCollapsed = wrapper.classList.contains("diff-file-collapsed");
                setFileCollapsed(wrapper, !wasCollapsed);
            });
            header.insertBefore(toggleButton, header.firstChild);

            const nameWrapper = document.createElement("span");
            nameWrapper.className = "gr-file-name-wrapper";
            const name = buildFileNameLink(meta.path);
            name.title = (meta.old_path && meta.old_path !== meta.path)
                ? `${meta.old_path} → ${meta.path}` : meta.path;
            nameWrapper.appendChild(name);
            const tag = document.createElement("span");
            const status = meta.status || "modified";
            tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
            tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            nameWrapper.appendChild(tag);
            if (section === "staged") {
                const stagedBadge = document.createElement("span");
                stagedBadge.className = "gr-staged-badge";
                stagedBadge.textContent = "staged";
                stagedBadge.title = "Staged for commit";
                nameWrapper.appendChild(stagedBadge);
            }
            const largeTag = document.createElement("span");
            largeTag.className = "gr-large-tag";
            largeTag.textContent = "large diff";
            largeTag.title = "Large diff — collapsed by default to keep the page light";
            nameWrapper.appendChild(largeTag);
            header.appendChild(nameWrapper);

            const stats = document.createElement("span");
            stats.className = "gr-file-stats";
            const adds = document.createElement("span");
            adds.className = "gr-additions";
            adds.textContent = `+${meta.additions || 0}`;
            const dels = document.createElement("span");
            dels.className = "gr-deletions";
            dels.textContent = `-${meta.deletions || 0}`;
            stats.appendChild(adds);
            stats.appendChild(dels);
            header.appendChild(stats);

            const isStaged = section === "staged";
            const stageBtn = document.createElement("button");
            stageBtn.type = "button";
            stageBtn.className = "stage-btn";
            stageBtn.textContent = isStaged ? "Unstage" : "Stage";
            stageBtn.title = isStaged ? "Unstage this file (git reset)" : "Stage this file (git add)";
            stageBtn.addEventListener("click", (e) => {
                e.stopPropagation();
                stageFile(wrapper, isStaged);
            });
            header.appendChild(stageBtn);

            const label = document.createElement("label");
            label.className = "diff-viewed-label";
            label.title = "Mark this file as viewed (saved on disk by gargo)";
            const checkbox = document.createElement("input");
            checkbox.type = "checkbox";
            checkbox.addEventListener("click", (e) => e.stopPropagation());
            checkbox.addEventListener("change", () => {
                setFileViewed(wrapper, checkbox.checked);
            });
            const labelText = document.createElement("span");
            labelText.textContent = "Viewed";
            label.appendChild(checkbox);
            label.appendChild(labelText);
            header.appendChild(label);

            wrapper.appendChild(header);

            const body = document.createElement("div");
            body.className = "gr-file-body";
            wrapper.appendChild(body);

            if (fileObserver) fileObserver.observe(wrapper);
            return wrapper;
        }

        function updateRowMeta(wrapper, meta) {
            const status = meta.status || "modified";
            const tag = wrapper.querySelector(".gr-status-tag");
            if (tag) {
                tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
                tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            }
            const adds = wrapper.querySelector(".gr-additions");
            if (adds) adds.textContent = `+${meta.additions || 0}`;
            const dels = wrapper.querySelector(".gr-deletions");
            if (dels) dels.textContent = `-${meta.deletions || 0}`;
            const name = wrapper.querySelector(".gr-file-name");
            if (name) {
                name.title = (meta.old_path && meta.old_path !== meta.path)
                    ? `${meta.old_path} → ${meta.path}` : meta.path;
            }
        }

        function setFileCollapsed(wrapper, isCollapsed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
            wrapper.classList.toggle("gr-file-collapsed", isCollapsed);
            const button = wrapper.querySelector(".diff-toggle-btn");
            if (button) renderDiffToggleButtonLabel(button, isCollapsed);
            if (isCollapsed) {
                collapsedFileIds.add(fileId);
                expandedFileIds.delete(fileId);
            } else {
                collapsedFileIds.delete(fileId);
                expandedFileIds.add(fileId);
                ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
            }
            persistCollapsedFileIds();
            persistExpandedFileIds();
        }

        // Apply (or revert) a file row's viewed appearance: viewed files start
        // collapsed but can still be re-expanded via the chevron — the body is
        // kept in the DOM so the user can peek without a network round-trip.
        function applyViewedState(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-viewed", isViewed);
            wrapper.classList.toggle("gr-file-viewed", isViewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== isViewed) checkbox.checked = isViewed;
            if (isViewed) {
                wrapper.classList.add("diff-file-collapsed");
                wrapper.classList.add("gr-file-collapsed");
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, true);
            } else {
                // Default-expand on un-viewed, unless explicitly collapsed or
                // a huge diff the user has not chosen to expand.
                const keepCollapsed = collapsedFileIds.has(fileId)
                    || (wrapper.classList.contains("gr-file-large") && !expandedFileIds.has(fileId));
                if (!keepCollapsed) {
                    wrapper.classList.remove("diff-file-collapsed");
                    wrapper.classList.remove("gr-file-collapsed");
                    const button = wrapper.querySelector(".diff-toggle-btn");
                    if (button) renderDiffToggleButtonLabel(button, false);
                    ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
                }
            }
            updateViewedCounter();
        }

        // Toggle a file's viewed state: update the UI right away, then persist
        // it on the server, which records a content hash so the checkbox only
        // survives while the diff is unchanged. Roll back if the request fails.
        function setFileViewed(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            applyViewedState(wrapper, isViewed);
            pendingViewed.set(fileId, isViewed);
            fetch("/api/status/viewed", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                    section: wrapper.dataset.section,
                    path: wrapper.dataset.path,
                    viewed: isViewed,
                }),
            }).then((response) => {
                if (!response.ok) throw new Error(`server returned ${response.status}`);
            }).catch((e) => {
                pendingViewed.delete(fileId);
                applyViewedState(wrapper, !isViewed);
                showError(`Failed to save viewed state: ${e.message}`);
            });
        }

        // Stage (git add) or unstage (git reset) one file, then refresh the
        // listing so it moves between the Staged / Changes sections.
        async function stageFile(wrapper, isStaged) {
            const path = wrapper.dataset.path;
            const endpoint = isStaged ? "/api/status/unstage" : "/api/status/stage";
            const btn = wrapper.querySelector(".stage-btn");
            if (btn) btn.disabled = true;
            try {
                const resp = await fetch(endpoint, {
                    method: "POST",
                    headers: { "Content-Type": "application/json" },
                    body: JSON.stringify({ path }),
                });
                if (!resp.ok) {
                    const data = await resp.json().catch(() => ({}));
                    throw new Error(data.error || `server returned ${resp.status}`);
                }
                await loadStatus({ showLoading: false });
            } catch (e) {
                if (btn) btn.disabled = false;
                showError(`Failed to ${isStaged ? "unstage" : "stage"}: ${e.message}`);
            }
        }

        async function ensureBodyLoaded(wrapper) {
            const fileId = wrapper.dataset.diffFileId;
            const section = wrapper.dataset.section;
            const path = wrapper.dataset.path;
            const body = wrapper.querySelector(".gr-file-body");
            if (!body || body.dataset.loaded || body.dataset.loading) return;
            const cached = bodyCache.get(fileId);
            if (cached) {
                body.innerHTML = cached.html;
                body.dataset.loaded = "1";
                return;
            }
            body.dataset.loading = "1";
            body.innerHTML = `<div class="loading">Loading diff…</div>`;
            try {
                const params = new URLSearchParams({ section, path });
                const response = await fetch(`/api/status/file?${params.toString()}`, { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                const html = typeof data.html === "string" ? data.html : "";
                bodyCache.set(fileId, { html, stats: data });
                body.innerHTML = html;
                body.dataset.loaded = "1";
                if (typeof data.additions === "number" || typeof data.deletions === "number") {
                    updateRowMeta(wrapper, {
                        path: data.path || path,
                        status: data.status,
                        additions: data.additions,
                        deletions: data.deletions,
                        binary: data.binary,
                    });
                }
            } catch (e) {
                body.innerHTML = `<div class="loading">Error: ${escapeHtml(e.message)}</div>`;
            } finally {
                delete body.dataset.loading;
            }
        }

        function buildFileTree(entries) {
            // Returns a root node { children: Map<name, node> }
            // Internal nodes have `dirPath` (absolute dir path), file leaves carry the entry.
            const root = { type: "dir", dirPath: "", children: new Map() };
            for (const entry of entries) {
                const parts = entry.meta.path.split("/").filter((p) => p.length > 0);
                let node = root;
                let dirPath = "";
                for (let i = 0; i < parts.length - 1; i++) {
                    const part = parts[i];
                    dirPath = dirPath ? `${dirPath}/${part}` : part;
                    let child = node.children.get(part);
                    if (!child) {
                        child = { type: "dir", dirPath, children: new Map() };
                        node.children.set(part, child);
                    }
                    node = child;
                }
                const leafName = parts[parts.length - 1] || entry.meta.path;
                node.children.set(`__file__${leafName}__${entry.section || ""}`, {
                    type: "file", name: leafName, entry,
                });
            }
            collapseSingleChainDirs(root, "");
            return root;
        }

        function collapseSingleChainDirs(node, parentDirPath) {
            // Merge any dir that contains exactly one sub-dir (and no files)
            // into its child, GitHub-style. Sets a `displayName` on each dir
            // child so rendering shows the merged chain (e.g. "src/main/java").
            for (const child of node.children.values()) {
                if (child.type !== "dir") continue;
                child.displayName = parentDirPath
                    ? child.dirPath.slice(parentDirPath.length + 1)
                    : child.dirPath;
                collapseSingleChainDirs(child, child.dirPath);
                while (child.children.size === 1) {
                    const only = child.children.values().next().value;
                    if (only.type !== "dir") break;
                    child.displayName = `${child.displayName}/${only.displayName}`;
                    child.dirPath = only.dirPath;
                    child.children = only.children;
                }
            }
        }

        function appendTreeNode(parentUl, node) {
            // Sort children: dirs first (alphabetical), then files (alphabetical)
            const dirs = [];
            const files = [];
            for (const [, child] of node.children) {
                if (child.type === "dir") dirs.push(child);
                else files.push(child);
            }
            dirs.sort((a, b) => a.dirPath.localeCompare(b.dirPath));
            files.sort((a, b) => a.name.localeCompare(b.name));
            for (const dir of dirs) {
                const li = document.createElement("li");
                li.className = "tree-dir";
                li.dataset.dirPath = dir.dirPath;
                const fileCount = countLeaves(dir);
                const headerEl = document.createElement("div");
                headerEl.className = "tree-dir-header";
                const toggle = document.createElement("button");
                toggle.type = "button";
                toggle.className = "tree-dir-toggle";
                toggle.setAttribute("aria-expanded", "true");
                toggle.textContent = "▾";
                const nameEl = document.createElement("span");
                nameEl.className = "tree-dir-name";
                nameEl.textContent = dir.displayName || dir.dirPath;
                nameEl.title = dir.dirPath;
                const countEl = document.createElement("span");
                countEl.className = "tree-dir-count";
                countEl.textContent = String(fileCount);
                headerEl.appendChild(toggle);
                headerEl.appendChild(nameEl);
                headerEl.appendChild(countEl);
                headerEl.addEventListener("click", (e) => {
                    e.preventDefault();
                    toggleDirCollapsed(li, dir.dirPath);
                });
                li.appendChild(headerEl);
                const childrenUl = document.createElement("ul");
                childrenUl.className = "tree-dir-children";
                appendTreeNode(childrenUl, dir);
                li.appendChild(childrenUl);
                if (sidebarCollapsedDirs.has(dir.dirPath)) {
                    li.classList.add("tree-dir-collapsed");
                    toggle.textContent = "▸";
                    toggle.setAttribute("aria-expanded", "false");
                }
                parentUl.appendChild(li);
            }
            for (const fileNode of files) {
                const entry = fileNode.entry;
                const section = entry.section;
                const meta = entry.meta;
                const li = document.createElement("li");
                li.className = "tree-file";
                const a = document.createElement("a");
                a.href = `#${fileAnchorOf(section, meta.path)}`;
                const badge = document.createElement("span");
                badge.className = `file-status ${SECTION_LIST_CSS[section] || "changed"}`;
                badge.textContent = SECTION_LIST_BADGE[section] || "M";
                const text = document.createElement("span");
                text.className = "file-path-text";
                text.textContent = fileNode.name;
                text.title = meta.path;
                a.appendChild(badge);
                a.appendChild(text);
                li.appendChild(a);
                parentUl.appendChild(li);
            }
        }

        function countLeaves(node) {
            let n = 0;
            for (const [, child] of node.children) {
                if (child.type === "file") n += 1;
                else n += countLeaves(child);
            }
            return n;
        }

        function toggleDirCollapsed(li, dirPath) {
            const collapsed = !li.classList.contains("tree-dir-collapsed");
            li.classList.toggle("tree-dir-collapsed", collapsed);
            const toggle = li.querySelector(".tree-dir-toggle");
            if (toggle) {
                toggle.textContent = collapsed ? "▸" : "▾";
                toggle.setAttribute("aria-expanded", collapsed ? "false" : "true");
            }
            if (collapsed) sidebarCollapsedDirs.add(dirPath);
            else sidebarCollapsedDirs.delete(dirPath);
            persistSidebarCollapsedDirs();
        }

        function renderSidebar(allEntries) {
            if (allEntries.length === 0) {
                filesHeading.textContent = "Files";
                setEmpty(filesListContainer, "No files changed");
                return;
            }
            const counts = {};
            for (const { section } of allEntries) counts[section] = (counts[section] || 0) + 1;
            const parts = [];
            if (counts.staged) parts.push(`${counts.staged} staged`);
            if (counts.unstaged) parts.push(`${counts.unstaged} changed`);
            if (counts.untracked) parts.push(`${counts.untracked} untracked`);
            filesHeading.textContent = parts.length > 0 ? `Files (${parts.join(", ")})` : "Files";
            filesListContainer.innerHTML = "";
            // Render each section as its own group, staged first, so staged and
            // unstaged changes to the same file read as distinct entries.
            const SECTION_GROUPS = [
                { key: "staged", title: "Staged" },
                { key: "unstaged", title: "Changes" },
                { key: "untracked", title: "Untracked" },
            ];
            for (const { key, title } of SECTION_GROUPS) {
                const entries = allEntries.filter((e) => e.section === key);
                if (entries.length === 0) continue;
                const group = document.createElement("div");
                group.className = `sidebar-group sidebar-group-${key}`;
                const heading = document.createElement("div");
                heading.className = "sidebar-group-header";
                const badge = document.createElement("span");
                badge.className = `file-status ${SECTION_LIST_CSS[key] || "changed"}`;
                badge.textContent = SECTION_LIST_BADGE[key] || "M";
                const titleEl = document.createElement("span");
                titleEl.className = "sidebar-group-title";
                titleEl.textContent = title;
                const countEl = document.createElement("span");
                countEl.className = "sidebar-group-count";
                countEl.textContent = String(entries.length);
                heading.appendChild(badge);
                heading.appendChild(titleEl);
                heading.appendChild(countEl);
                group.appendChild(heading);
                const root = buildFileTree(entries);
                const ul = document.createElement("ul");
                ul.className = "file-tree";
                appendTreeNode(ul, root);
                group.appendChild(ul);
                filesListContainer.appendChild(group);
            }
        }

        function renderMain(allEntries) {
            if (allEntries.length === 0) {
                if (fileObserver) {
                    for (const child of filesMain.children) {
                        if (child.dataset && child.dataset.diffFileId) fileObserver.unobserve(child);
                    }
                }
                filesMain.innerHTML = "";
                setEmpty(filesMain, "No files changed");
                return;
            }
            const presentIds = new Set();
            for (const { section, meta } of allEntries) presentIds.add(fileIdOf(section, meta.path));
            const existing = new Map();
            for (const child of Array.from(filesMain.children)) {
                if (child.dataset && child.dataset.diffFileId) {
                    if (presentIds.has(child.dataset.diffFileId)) {
                        existing.set(child.dataset.diffFileId, child);
                    } else {
                        if (fileObserver) fileObserver.unobserve(child);
                        child.remove();
                    }
                } else {
                    child.remove();
                }
            }
            // Remove stale cache entries
            for (const id of Array.from(bodyCache.keys())) {
                if (!presentIds.has(id)) bodyCache.delete(id);
            }
            for (const id of Array.from(pendingViewed.keys())) {
                if (!presentIds.has(id)) pendingViewed.delete(id);
            }
            for (const id of Array.from(collapsedFileIds)) {
                if (!presentIds.has(id)) collapsedFileIds.delete(id);
            }
            for (const id of Array.from(expandedFileIds)) {
                if (!presentIds.has(id)) expandedFileIds.delete(id);
            }
            persistCollapsedFileIds();
            persistExpandedFileIds();

            let anchor = null;
            for (const { section, meta } of allEntries) {
                const fileId = fileIdOf(section, meta.path);
                let wrapper = existing.get(fileId);
                if (wrapper) {
                    updateRowMeta(wrapper, meta);
                } else {
                    wrapper = createFileRow(section, meta);
                    filesMain.appendChild(wrapper);
                }
                if (anchor) {
                    if (anchor.nextSibling !== wrapper) filesMain.insertBefore(wrapper, anchor.nextSibling);
                } else if (filesMain.firstChild !== wrapper) {
                    filesMain.insertBefore(wrapper, filesMain.firstChild);
                }
                anchor = wrapper;
                // The server reports `viewed`; while a toggle's POST is still
                // in flight the optimistic value wins until the server agrees.
                let isViewed = !!meta.viewed;
                if (pendingViewed.has(fileId)) {
                    const want = pendingViewed.get(fileId);
                    if (want === isViewed) pendingViewed.delete(fileId);
                    else isViewed = want;
                }
                const isLarge = isHugeDiff(meta);
                const isCollapsed = shouldCollapseByDefault(fileId, meta, isViewed);
                wrapper.classList.toggle("gr-file-large", isLarge);
                wrapper.classList.toggle("gr-file-viewed", isViewed);
                wrapper.classList.toggle("diff-file-viewed", isViewed);
                wrapper.classList.toggle("gr-file-collapsed", isCollapsed);
                wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
                const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
                if (checkbox) checkbox.checked = isViewed;
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, isCollapsed);
                // Body fetch happens lazily via IntersectionObserver when the
                // wrapper scrolls into view. No eager fetch here.
            }
        }

        async function loadStatus({ showLoading = true } = {}) {
            if (isLoading) return;
            isLoading = true;
            clearError();
            if (showLoading) {
                setLoading(filesListContainer, "Loading files...");
                setLoading(filesMain, "Loading files...");
            }
            persistControls();
            try {
                const params = new URLSearchParams();
                params.set("show_untracked", showUntrackedToggle.checked ? "true" : "false");
                const response = await fetch(`/api/status?${params.toString()}`, { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                latestStatus = data;
                const all = [];
                for (const meta of (data.staged || [])) all.push({ section: "staged", meta });
                for (const meta of (data.unstaged || [])) all.push({ section: "unstaged", meta });
                for (const meta of (data.untracked || [])) all.push({ section: "untracked", meta });
                renderSidebar(all);
                renderMain(all);
                updateViewedCounter();
            } finally {
                isLoading = false;
            }
        }

        const persistControls = () => {
            const params = new URLSearchParams();
            params.set("show_untracked", showUntrackedToggle.checked ? "true" : "false");
            history.replaceState(null, "", `/diff?${params.toString()}`);
        };

        refreshButton.addEventListener("click", () => {
            loadStatus().catch((e) => showError(e.message));
        });
        showUntrackedToggle.addEventListener("change", () => {
            loadStatus().catch((e) => showError(e.message));
        });
        goTopButton.addEventListener("click", () => {
            window.scrollTo({ top: 0, behavior: "smooth" });
        });

        // Delegated handler for the "Show N hidden lines" buttons that sit
        // between hunks. Pulls the missing range from the corresponding ref
        // (HEAD for staged, working tree otherwise) and inserts the rows.
        document.addEventListener("click", async (e) => {
            const btn = e.target.closest && e.target.closest(".gr-expand-btn");
            if (!btn || btn.disabled) return;
            const wrapper = btn.closest(".gr-file");
            if (!wrapper) return;
            const oldStart = parseInt(btn.dataset.oldStart, 10);
            const oldEnd = parseInt(btn.dataset.oldEnd, 10);
            const newStart = parseInt(btn.dataset.newStart, 10);
            const newEnd = parseInt(btn.dataset.newEnd, 10);
            if (!Number.isFinite(newStart) || !Number.isFinite(newEnd) || newEnd < newStart) return;
            const section = wrapper.dataset.section;
            const path = wrapper.dataset.path;
            btn.disabled = true;
            try {
                const params = new URLSearchParams({
                    section,
                    path,
                    start: String(newStart),
                    end: String(newEnd),
                });
                const resp = await fetch(`/api/status/context?${params}`);
                if (!resp.ok) throw new Error(`server returned ${resp.status}`);
                const data = await resp.json();
                const lines = Array.isArray(data.lines) ? data.lines : [];
                const frag = document.createDocumentFragment();
                for (let i = 0; i < lines.length; i++) {
                    const row = document.createElement("div");
                    row.className = "gr-line gr-line-context";
                    const ln = document.createElement("span"); ln.className = "gr-ln"; ln.textContent = String(oldStart + i);
                    const lnr = document.createElement("span"); lnr.className = "gr-lnr"; lnr.textContent = String(newStart + i);
                    const sign = document.createElement("span"); sign.className = "gr-sign"; sign.textContent = " ";
                    const text = document.createElement("span"); text.className = "gr-text"; text.textContent = lines[i];
                    row.appendChild(ln); row.appendChild(lnr); row.appendChild(sign); row.appendChild(text);
                    frag.appendChild(row);
                }
                btn.closest(".gr-line-expand").replaceWith(frag);
            } catch (err) {
                btn.disabled = false;
                showError(`Failed to expand context: ${err.message}`);
            }
        });

        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });
        window.setInterval(() => {
            loadStatus({ showLoading: false }).catch((e) => showError(e.message));
        }, AUTO_REFRESH_INTERVAL_MS);

        updateGoTopButtonVisibility();
        loadStatus().catch((e) => showError(e.message));
    </script>
    </main>
</div>
</body>
</html>"#;

const COMMIT_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Commit</title>
    <style>
{{SHARED_CSS}}
        .commit-wrap { max-width: 760px; }
        #error-banner {
            display: none;
            margin: 0 0 16px 0;
            padding: 12px 16px;
            border: 1px solid #fcc;
            border-radius: 8px;
            background: #fee;
            color: #b20000;
        }
        .commit-card {
            background: #fff;
            border: 1px solid #d0d7de;
            border-radius: 8px;
            padding: 16px;
            margin-bottom: 16px;
        }
        .commit-card h2 { margin: 0 0 12px 0; font-size: 16px; }
        .commit-card h2 .count { color: #57606a; font-weight: 400; font-size: 14px; }
        .staged-list { list-style: none; margin: 0; padding: 0; }
        .staged-list li {
            display: flex;
            align-items: center;
            gap: 8px;
            padding: 5px 6px;
            border-radius: 6px;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 13px;
        }
        .staged-list li:hover { background: #f6f8fa; }
        .staged-list .s-path { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
        .staged-list .s-stats { flex-shrink: 0; display: inline-flex; gap: 8px; font-size: 12px; }
        .staged-list .gr-additions { color: #1a7f37; }
        .staged-list .gr-deletions { color: #cf222e; }
        .staged-list .unstage-btn {
            flex-shrink: 0;
            padding: 2px 8px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 12px;
            cursor: pointer;
        }
        .staged-list .unstage-btn:hover { background: #eef2f7; }
        .gr-status-tag { flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; }
        .gr-status-modified  { background: #fff8c5; color: #9a6700; }
        .gr-status-added     { background: #dafbe1; color: #1a7f37; }
        .gr-status-deleted   { background: #ffebe9; color: #cf222e; }
        .gr-status-renamed   { background: #ddf4ff; color: #0969da; }
        .gr-status-untracked { background: #eaeef2; color: #57606a; }
        #commit-message {
            width: 100%;
            box-sizing: border-box;
            min-height: 120px;
            padding: 10px 12px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 13px;
            resize: vertical;
        }
        #commit-message:focus { outline: none; border-color: #0969da; box-shadow: 0 0 0 2px #ddf4ff; }
        .commit-actions { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; margin-top: 12px; }
        .amend-label { display: inline-flex; align-items: center; gap: 6px; font-size: 13px; color: #24292f; cursor: pointer; user-select: none; }
        .amend-label input { margin: 0; cursor: pointer; }
        .commit-actions .spacer { flex: 1 1 auto; }
        .btn {
            padding: 6px 14px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 14px;
            text-decoration: none;
            cursor: pointer;
        }
        .btn:hover { background: #eef2f7; }
        .btn-primary { background: #1f883d; border-color: rgba(31,136,61,0.4); color: #fff; }
        .btn-primary:hover { background: #1a7f37; }
        .btn-primary:disabled { background: #94d3a2; border-color: transparent; cursor: not-allowed; }
        .hint { color: #57606a; font-size: 12px; margin: 8px 0 0 0; }
        .empty { padding: 16px; color: #57606a; }
    </style>
</head>
<body data-page="status">
{{REPO_CTX_SCRIPT}}
<script>{{SHORTCUTS_JS}}</script>
<div class="app-shell">
    {{APP_RAIL}}
    <main class="app-main commit-wrap">
        <h1 style="font-size:22px;margin:0 0 16px 0;">Commit</h1>
        <div id="error-banner"></div>

        <div class="commit-card">
            <h2>Staged files <span class="count" id="staged-count"></span></h2>
            <ul class="staged-list" id="staged-list"><li class="empty">Loading…</li></ul>
        </div>

        <div class="commit-card">
            <h2>Message</h2>
            <textarea id="commit-message" placeholder="Commit message" autofocus></textarea>
            <div class="commit-actions">
                <label class="amend-label" id="amend-wrap" hidden>
                    <input type="checkbox" id="amend-toggle">
                    Amend previous commit
                </label>
                <span class="spacer"></span>
                <a class="btn" href="/status">Cancel</a>
                <button class="btn btn-primary" id="commit-btn" type="button" disabled>Commit</button>
            </div>
            <p class="hint" id="commit-hint"></p>
        </div>
    </main>
</div>
<script>
    const errorBanner = document.getElementById("error-banner");
    const stagedList = document.getElementById("staged-list");
    const stagedCount = document.getElementById("staged-count");
    const messageBox = document.getElementById("commit-message");
    const amendWrap = document.getElementById("amend-wrap");
    const amendToggle = document.getElementById("amend-toggle");
    const commitBtn = document.getElementById("commit-btn");
    const commitHint = document.getElementById("commit-hint");

    const STATUS_LABELS = { modified: "CHANGED", added: "ADDED", deleted: "DELETED", renamed: "RENAMED", untracked: "UNTRACKED" };
    const STATUS_CSS = { modified: "gr-status-modified", added: "gr-status-added", deleted: "gr-status-deleted", renamed: "gr-status-renamed", untracked: "gr-status-untracked" };

    let lastMessage = "";
    let stagedFiles = [];
    let submitting = false;

    const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;" }[c]));
    const showError = (m) => { errorBanner.textContent = `Error: ${m}`; errorBanner.style.display = "block"; };
    const clearError = () => { errorBanner.textContent = ""; errorBanner.style.display = "none"; };

    function refreshCommitButton() {
        const hasMsg = messageBox.value.trim().length > 0;
        const amend = amendToggle.checked;
        const canCommit = hasMsg && (stagedFiles.length > 0 || amend) && !submitting;
        commitBtn.disabled = !canCommit;
        if (stagedFiles.length === 0 && !amend) {
            commitHint.textContent = "Nothing staged — stage files on the Status page, or enable amend to edit the previous commit.";
        } else if (!hasMsg) {
            commitHint.textContent = "Enter a commit message.";
        } else {
            commitHint.textContent = amend
                ? "Will amend (rewrite) the previous commit with the staged changes."
                : `Will commit ${stagedFiles.length} staged file${stagedFiles.length === 1 ? "" : "s"}.`;
        }
    }

    function renderStaged() {
        stagedCount.textContent = stagedFiles.length ? `(${stagedFiles.length})` : "";
        if (stagedFiles.length === 0) {
            stagedList.innerHTML = `<li class="empty">No staged changes.</li>`;
            return;
        }
        stagedList.innerHTML = "";
        for (const meta of stagedFiles) {
            const li = document.createElement("li");
            const status = meta.status || "modified";
            const tag = document.createElement("span");
            tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
            tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            const path = document.createElement("span");
            path.className = "s-path";
            path.textContent = meta.path;
            path.title = meta.path;
            const stats = document.createElement("span");
            stats.className = "s-stats";
            stats.innerHTML = `<span class="gr-additions">+${meta.additions || 0}</span><span class="gr-deletions">-${meta.deletions || 0}</span>`;
            const btn = document.createElement("button");
            btn.type = "button";
            btn.className = "unstage-btn";
            btn.textContent = "Unstage";
            btn.addEventListener("click", () => unstage(meta.path, btn));
            li.appendChild(tag);
            li.appendChild(path);
            li.appendChild(stats);
            li.appendChild(btn);
            stagedList.appendChild(li);
        }
    }

    async function unstage(path, btn) {
        btn.disabled = true;
        try {
            const resp = await fetch("/api/status/unstage", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ path }),
            });
            if (!resp.ok) {
                const data = await resp.json().catch(() => ({}));
                throw new Error(data.error || `server returned ${resp.status}`);
            }
            await load();
        } catch (e) {
            btn.disabled = false;
            showError(`Failed to unstage: ${e.message}`);
        }
    }

    async function load() {
        clearError();
        const resp = await fetch("/api/status/commit-prepare", { cache: "no-store" });
        const data = await resp.json();
        if (data.error) throw new Error(data.error);
        stagedFiles = Array.isArray(data.staged) ? data.staged : [];
        lastMessage = typeof data.last_message === "string" ? data.last_message : "";
        amendWrap.hidden = !data.has_head;
        renderStaged();
        // If amend is on, keep the prefilled message synced when the user hasn't typed.
        refreshCommitButton();
    }

    amendToggle.addEventListener("change", () => {
        if (amendToggle.checked) {
            if (!messageBox.value.trim()) messageBox.value = lastMessage;
        } else if (messageBox.value === lastMessage) {
            messageBox.value = "";
        }
        refreshCommitButton();
        messageBox.focus();
    });

    messageBox.addEventListener("input", refreshCommitButton);

    commitBtn.addEventListener("click", async () => {
        const message = messageBox.value.trim();
        if (!message) return;
        submitting = true;
        refreshCommitButton();
        commitBtn.textContent = "Committing…";
        clearError();
        try {
            const resp = await fetch("/api/status/commit", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ message, amend: amendToggle.checked }),
            });
            if (!resp.ok) {
                const data = await resp.json().catch(() => ({}));
                throw new Error(data.error || `server returned ${resp.status}`);
            }
            window.location.href = "/status";
        } catch (e) {
            submitting = false;
            commitBtn.textContent = "Commit";
            refreshCommitButton();
            showError(`Commit failed: ${e.message}`);
        }
    });

    // Cmd/Ctrl+Enter submits.
    messageBox.addEventListener("keydown", (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !commitBtn.disabled) {
            e.preventDefault();
            commitBtn.click();
        }
    });

    load().catch((e) => showError(e.message));
</script>
</body>
</html>"#;

/// HTML template for the compare-branches page.
const COMPARE_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Git Compare Branches</title>
    <style>
{{SHARED_CSS}}
        .repo-controls { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; padding: 10px 14px; border: 1px solid #d0d7de; border-radius: 6px; background: #f6f8fa; margin-bottom: 16px; }
        .repo-controls input[list] { min-width: 220px; padding: 4px 8px; border: 1px solid #d0d7de; border-radius: 6px; background: #fff; font: inherit; }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #4b5563;
        }
        .context-row code {
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            background: #f6f8fa;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            padding: 2px 8px;
            color: #24292f;
            word-break: break-all;
        }
        .controls { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; }
        .controls label { font-size: 14px; display: flex; align-items: center; gap: 8px; }
        .controls select, .controls button {
            padding: 6px 10px;
            border: 1px solid #ccc;
            border-radius: 6px;
            background: white;
            font-size: 14px;
        }
        .controls button { cursor: pointer; }
        .range-arrow { font-weight: 600; color: #6b7280; }
        #error-banner {
            display: none;
            margin: 0 0 20px 0;
            padding: 12px 16px;
            border: 1px solid #fcc;
            border-radius: 8px;
            background: #fee;
            color: #b20000;
        }
        .section {
            background: white;
            padding: 16px;
            margin-bottom: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
        }
        .section h2 { margin: 0 0 12px 0; font-size: 18px; }
        .loading, .empty { padding: 20px; color: #666; }
        .file-list { list-style: none; margin: 0; padding: 0; }
        .file-list li { margin: 2px 0; }
        .file-list a {
            display: flex;
            align-items: center;
            gap: 6px;
            color: #0a58ca;
            text-decoration: none;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            padding: 2px 4px;
            border-radius: 4px;
        }
        .file-list a:hover { background: #f6f8fa; text-decoration: underline; }
        .file-list .file-path-text {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            flex: 1 1 auto;
            min-width: 0;
        }
        .file-tree { list-style: none; margin: 0; padding: 0; }
        .file-tree li { margin: 0; }
        .tree-dir { margin: 0; }
        .tree-dir-children {
            list-style: none;
            margin: 0;
            padding-left: 16px;
        }
        .tree-dir.tree-dir-collapsed > .tree-dir-children { display: none; }
        .tree-dir-header {
            display: flex;
            align-items: center;
            gap: 4px;
            padding: 2px 4px;
            border-radius: 4px;
            cursor: pointer;
            user-select: none;
        }
        .tree-dir-header:hover { background: #f6f8fa; }
        .tree-dir-toggle {
            flex-shrink: 0;
            padding: 0 4px;
            border: none;
            background: transparent;
            color: #57606a;
            font-size: 12px;
            line-height: 1;
            cursor: pointer;
        }
        .tree-dir-name {
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            font-weight: 600;
            color: #1f2328;
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            min-width: 0;
            flex: 1 1 auto;
        }
        .tree-dir-count {
            flex-shrink: 0;
            font-size: 11px;
            color: #57606a;
        }
        .tree-file a {
            display: flex;
            align-items: center;
            gap: 6px;
            color: #0a58ca;
            text-decoration: none;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 12px;
            padding: 2px 4px;
            border-radius: 4px;
        }
        .tree-file a:hover { background: #f6f8fa; text-decoration: underline; }
        .tree-file .file-path-text {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            flex: 1 1 auto;
            min-width: 0;
        }
        .gr-file {
            background: white;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            margin-bottom: 12px;
            overflow: hidden;
        }
        .gr-file-header {
            display: flex;
            align-items: center;
            flex-wrap: nowrap;
            gap: 8px;
            min-width: 0;
            padding: 8px 12px;
            background: #f6f8fa;
            border-bottom: 1px solid #d0d7de;
        }
        .gr-file-collapsed .gr-file-body { display: none; }
        .gr-file-collapsed .gr-file-header { border-bottom: none; }
        .gr-large-tag { display: none; flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; background: #fff1e5; color: #bc4c00; }
        .gr-file-large.gr-file-collapsed .gr-large-tag { display: inline-flex; }
        .gr-file-viewed .gr-file-header { background: #eef2f7; opacity: 0.85; }
        .gr-file-name-wrapper {
            flex: 1 1 auto;
            min-width: 0;
            display: flex;
            align-items: center;
            gap: 8px;
            overflow: hidden;
        }
        .gr-file-name {
            overflow: hidden;
            text-overflow: ellipsis;
            white-space: nowrap;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
            font-size: 13px;
        }
        a.gr-file-name { color: #0969da; text-decoration: none; }
        a.gr-file-name:hover { text-decoration: underline; }
        .gr-status-tag {
            flex-shrink: 0;
            padding: 1px 6px;
            border-radius: 4px;
            font-size: 11px;
            font-weight: 600;
        }
        .gr-status-modified  { background: #fff8c5; color: #9a6700; }
        .gr-status-added     { background: #dafbe1; color: #1a7f37; }
        .gr-status-deleted   { background: #ffebe9; color: #cf222e; }
        .gr-status-renamed   { background: #ddf4ff; color: #0969da; }
        .gr-status-untracked { background: #eaeef2; color: #57606a; }
        .gr-file-stats { flex-shrink: 0; display: inline-flex; gap: 8px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
        .gr-additions { color: #1a7f37; }
        .gr-deletions { color: #cf222e; }
        .diff-toggle-btn {
            order: -1;
            flex-shrink: 0;
            padding: 0 6px;
            border: 1px solid transparent;
            border-radius: 6px;
            background: transparent;
            color: #57606a;
            font-size: 14px;
            line-height: 1.2;
            cursor: pointer;
        }
        .diff-toggle-btn:hover {
            background: #eef2f7;
            border-color: #d0d7de;
            color: #24292f;
        }
        .diff-viewed-label {
            margin-left: auto;
            flex-shrink: 0;
            display: inline-flex;
            align-items: center;
            gap: 6px;
            padding: 2px 8px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 12px;
            cursor: pointer;
            user-select: none;
        }
        .diff-viewed-label:hover { background: #eef2f7; }
        .diff-viewed-label input { margin: 0; cursor: pointer; }
        .gr-file-body { background: white; }
        .gr-file-body .loading, .gr-file-body .empty { padding: 12px; color: #57606a; font-size: 12px; }
{{DIFF_STYLES}}
        #bottom-controls {
            position: fixed;
            right: 20px;
            bottom: 20px;
            z-index: 1000;
            display: flex;
            align-items: center;
            gap: 8px;
        }
        #viewed-counter {
            padding: 8px 12px;
            border: 1px solid #ccc;
            border-radius: 8px;
            background: white;
            color: #57606a;
            font-size: 13px;
            font-variant-numeric: tabular-nums;
            white-space: nowrap;
            box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
        }
        #go-top-btn {
            padding: 8px 12px;
            border: 1px solid #ccc;
            border-radius: 8px;
            background: white;
            color: #24292f;
            font-size: 14px;
            cursor: pointer;
            opacity: 0;
            pointer-events: none;
            transform: translateY(8px);
            transition: opacity 0.15s ease, transform 0.15s ease;
        }
        #go-top-btn.visible { opacity: 1; pointer-events: auto; transform: translateY(0); }
        #go-top-btn:hover { background: #eef2f7; }
    </style>
</head>
<body data-page="compare">
{{REPO_CTX_SCRIPT}}
<script>{{SHORTCUTS_JS}}</script>
<code id="root-path" hidden>{{ROOT_PATH}}</code>
<div class="app-shell">
    {{APP_RAIL}}
    <main class="app-main">
    <div class="repo-controls">
        <label>
            Base
            <input id="base-select" list="base-list" autocomplete="off" placeholder="branch...">
            <datalist id="base-list"></datalist>
        </label>
        <span class="range-arrow">...</span>
        <label>
            Compare
            <input id="compare-select" list="compare-list" autocomplete="off" placeholder="branch...">
            <datalist id="compare-list"></datalist>
        </label>
        <button id="swap-btn" type="button" title="Swap base and compare">Swap</button>
        <button id="refresh-btn" type="button">Refresh</button>
    </div>

    <div id="error-banner"></div>

    <div class="layout">
        <aside class="sidebar">
            <section class="section files-section">
                <h2 id="files-heading">Files</h2>
                <div id="files-list"><div class="loading">Select a base and compare branch...</div></div>
            </section>
        </aside>
        <main class="content">
            <section class="section">
                <h2>Compare Diff</h2>
                <div id="files-main"><div class="loading">Select a base and compare branch...</div></div>
            </section>
        </main>
    </div>
    <div id="bottom-controls">
        <span id="viewed-counter" title="Files marked as viewed / total files"></span>
        <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>
    </div>

    <script>
        const urlParams = new URLSearchParams(window.location.search);

        const baseSelect = document.getElementById("base-select");
        const compareSelect = document.getElementById("compare-select");
        const baseList = document.getElementById("base-list");
        const compareList = document.getElementById("compare-list");
        const knownBranches = new Set();
        const swapButton = document.getElementById("swap-btn");
        const refreshButton = document.getElementById("refresh-btn");
        const errorBanner = document.getElementById("error-banner");
        const rootPathCode = document.getElementById("root-path");
        const filesHeading = document.getElementById("files-heading");
        const filesListContainer = document.getElementById("files-list");
        const filesMain = document.getElementById("files-main");
        const goTopButton = document.getElementById("go-top-btn");
        const viewedCounter = document.getElementById("viewed-counter");
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.compare.collapsed.v3:${STORAGE_ROOT}`;
        const EXPANDED_FILES_STORAGE_KEY = `gargo.compare.expanded.v1:${STORAGE_ROOT}`;
        const SIDEBAR_COLLAPSED_KEY = `gargo.compare.sidebar.collapsed.v1:${STORAGE_ROOT}`;
        // Diffs with at least this many changed lines (additions + deletions)
        // are collapsed by default so the browser stays responsive. The user
        // can still expand them, and that choice is remembered per session.
        const HUGE_DIFF_LINES = 1000;

        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let expandedFileIds = loadIdSet(sessionStorage, EXPANDED_FILES_STORAGE_KEY);
        let sidebarCollapsedDirs = loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY);
        const bodyCache = new Map();
        // fileId -> optimistic viewed state while its set-viewed POST is in
        // flight; cleared once the server's reported state agrees.
        const pendingViewed = new Map();
        let isLoadingCompare = false;
        let latestFiles = null;

        const STATUS_LABELS = {
            modified: "CHANGED", added: "ADDED", deleted: "DELETED",
            renamed: "RENAMED", untracked: "UNTRACKED",
        };
        const STATUS_CSS = {
            modified: "gr-status-modified", added: "gr-status-added", deleted: "gr-status-deleted",
            renamed: "gr-status-renamed", untracked: "gr-status-untracked",
        };

        function loadIdSet(storage, key) {
            try {
                const raw = storage.getItem(key);
                if (!raw) return new Set();
                const parsed = JSON.parse(raw);
                if (!Array.isArray(parsed)) return new Set();
                return new Set(parsed.filter((v) => typeof v === "string" && v.length > 0));
            } catch (_e) { return new Set(); }
        }
        const persistIdSet = (storage, key, set) => {
            try { storage.setItem(key, JSON.stringify(Array.from(set))); } catch (_e) {}
        };
        const persistCollapsedFileIds = () => persistIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY, collapsedFileIds);
        const persistExpandedFileIds = () => persistIdSet(sessionStorage, EXPANDED_FILES_STORAGE_KEY, expandedFileIds);
        const persistSidebarCollapsedDirs = () => persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY, sidebarCollapsedDirs);
        // A diff is "huge" once its changed-line count crosses the threshold.
        const isHugeDiff = (meta) => !!meta
            && ((meta.additions || 0) + (meta.deletions || 0)) >= HUGE_DIFF_LINES;
        // Whether a file should start collapsed: explicitly collapsed, viewed,
        // or a huge diff the user has not explicitly chosen to expand.
        const shouldCollapseByDefault = (fileId, meta, isViewed) =>
            isViewed
            || collapsedFileIds.has(fileId)
            || (isHugeDiff(meta) && !expandedFileIds.has(fileId));

        const fileObserver = (typeof IntersectionObserver !== "undefined")
            ? new IntersectionObserver((entries) => {
                for (const e of entries) {
                    if (e.isIntersecting) {
                        const wrapper = e.target;
                        if (!wrapper.classList.contains("gr-file-collapsed")
                            && !wrapper.classList.contains("gr-file-viewed")) {
                            ensureBodyLoaded(wrapper).catch((err) => showError(err.message));
                        }
                    }
                }
            }, { rootMargin: "600px 0px", threshold: 0 })
            : null;

        const showError = (m) => { errorBanner.textContent = `Error: ${m}`; errorBanner.style.display = "block"; };
        const clearError = () => { errorBanner.textContent = ""; errorBanner.style.display = "none"; };
        const setLoading = (c, m) => { c.innerHTML = `<div class="loading">${escapeHtml(m)}</div>`; };
        const setEmpty = (c, m) => { c.innerHTML = `<div class="empty">${escapeHtml(m)}</div>`; };
        const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({
            "&": "&amp;", "<": "&lt;", ">": "&gt;", "\"": "&quot;", "'": "&#39;",
        }[c]));
        const fileIdOf = (path) => `compare:${path}`;
        const fileAnchorOf = (path) => `file-compare-${path.replace(/[^A-Za-z0-9_\-\.]/g, "_")}`;

        // Build a Code-page link (blob view) for a file using the currently
        // selected compare ref so PR-style review jumps land on the right version.
        function buildFileNameLink(path) {
            const ctx = window.__GARGO_REPO_CTX__;
            const compareSelect = document.getElementById("compare-select");
            const ref = (compareSelect && compareSelect.value) || (ctx && ctx.branch);
            const ok = ctx && ctx.owner && ctx.repo && ref;
            const name = ok ? document.createElement("a") : document.createElement("span");
            name.className = "gr-file-name";
            name.textContent = path;
            if (name.tagName === "A") {
                const segs = String(path).split("/").map(encodeURIComponent).join("/");
                name.href = `/${encodeURIComponent(ctx.owner)}/${encodeURIComponent(ctx.repo)}/blob/${encodeURIComponent(ref)}/${segs}`;
            }
            return name;
        }

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) goTopButton.classList.add("visible");
            else goTopButton.classList.remove("visible");
        };
        // Reflect how many of the listed files are marked "Viewed" next to
        // the go-to-top button. Counts live DOM rows so it stays correct
        // after refreshes, viewed toggles, and base/compare changes.
        const updateViewedCounter = () => {
            const rows = filesMain.querySelectorAll(".gr-file");
            const total = rows.length;
            if (total === 0) {
                viewedCounter.style.display = "none";
                return;
            }
            let viewed = 0;
            for (const row of rows) {
                if (row.classList.contains("gr-file-viewed")) viewed += 1;
            }
            viewedCounter.style.display = "";
            viewedCounter.textContent = `viewed ${viewed} / ${total}`;
        };
        const renderDiffToggleButtonLabel = (button, collapsed) => {
            button.textContent = collapsed ? "▸" : "▾";
            button.setAttribute("aria-expanded", collapsed ? "false" : "true");
            button.setAttribute("aria-label", collapsed ? "Show diff" : "Hide diff");
            button.setAttribute("title", collapsed ? "Show diff" : "Hide diff");
        };

        function createFileRow(meta) {
            const fileId = fileIdOf(meta.path);
            const wrapper = document.createElement("div");
            wrapper.className = "gr-file";
            wrapper.dataset.diffFileId = fileId;
            wrapper.dataset.path = meta.path;
            wrapper.id = fileAnchorOf(meta.path);

            const header = document.createElement("div");
            header.className = "gr-file-header";

            const toggleButton = document.createElement("button");
            toggleButton.type = "button";
            toggleButton.className = "diff-toggle-btn";
            toggleButton.addEventListener("click", () => {
                const wasCollapsed = wrapper.classList.contains("diff-file-collapsed");
                setFileCollapsed(wrapper, !wasCollapsed);
            });
            header.insertBefore(toggleButton, header.firstChild);

            const nameWrapper = document.createElement("span");
            nameWrapper.className = "gr-file-name-wrapper";
            const name = buildFileNameLink(meta.path);
            name.title = (meta.old_path && meta.old_path !== meta.path)
                ? `${meta.old_path} → ${meta.path}` : meta.path;
            nameWrapper.appendChild(name);
            const tag = document.createElement("span");
            const status = meta.status || "modified";
            tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
            tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            nameWrapper.appendChild(tag);
            const largeTag = document.createElement("span");
            largeTag.className = "gr-large-tag";
            largeTag.textContent = "large diff";
            largeTag.title = "Large diff — collapsed by default to keep the page light";
            nameWrapper.appendChild(largeTag);
            header.appendChild(nameWrapper);

            const stats = document.createElement("span");
            stats.className = "gr-file-stats";
            const adds = document.createElement("span");
            adds.className = "gr-additions";
            adds.textContent = `+${meta.additions || 0}`;
            const dels = document.createElement("span");
            dels.className = "gr-deletions";
            dels.textContent = `-${meta.deletions || 0}`;
            stats.appendChild(adds);
            stats.appendChild(dels);
            header.appendChild(stats);

            const label = document.createElement("label");
            label.className = "diff-viewed-label";
            label.title = "Mark this file as viewed (saved on disk by gargo)";
            const checkbox = document.createElement("input");
            checkbox.type = "checkbox";
            checkbox.addEventListener("click", (e) => e.stopPropagation());
            checkbox.addEventListener("change", () => {
                setFileViewed(wrapper, checkbox.checked);
            });
            const labelText = document.createElement("span");
            labelText.textContent = "Viewed";
            label.appendChild(checkbox);
            label.appendChild(labelText);
            header.appendChild(label);

            wrapper.appendChild(header);

            const body = document.createElement("div");
            body.className = "gr-file-body";
            wrapper.appendChild(body);

            if (fileObserver) fileObserver.observe(wrapper);
            return wrapper;
        }

        function updateRowMeta(wrapper, meta) {
            const status = meta.status || "modified";
            const tag = wrapper.querySelector(".gr-status-tag");
            if (tag) {
                tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
                tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            }
            const adds = wrapper.querySelector(".gr-additions");
            if (adds) adds.textContent = `+${meta.additions || 0}`;
            const dels = wrapper.querySelector(".gr-deletions");
            if (dels) dels.textContent = `-${meta.deletions || 0}`;
        }

        function setFileCollapsed(wrapper, isCollapsed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
            wrapper.classList.toggle("gr-file-collapsed", isCollapsed);
            const button = wrapper.querySelector(".diff-toggle-btn");
            if (button) renderDiffToggleButtonLabel(button, isCollapsed);
            if (isCollapsed) {
                collapsedFileIds.add(fileId);
                expandedFileIds.delete(fileId);
            } else {
                collapsedFileIds.delete(fileId);
                expandedFileIds.add(fileId);
                ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
            }
            persistCollapsedFileIds();
            persistExpandedFileIds();
        }

        // Apply (or revert) a file row's viewed appearance: viewed files start
        // collapsed but can still be re-expanded via the chevron — the body is
        // kept in the DOM so the user can peek without a network round-trip.
        function applyViewedState(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-viewed", isViewed);
            wrapper.classList.toggle("gr-file-viewed", isViewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== isViewed) checkbox.checked = isViewed;
            if (isViewed) {
                wrapper.classList.add("diff-file-collapsed");
                wrapper.classList.add("gr-file-collapsed");
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, true);
            } else {
                // Default-expand on un-viewed, unless explicitly collapsed or
                // a huge diff the user has not chosen to expand.
                const keepCollapsed = collapsedFileIds.has(fileId)
                    || (wrapper.classList.contains("gr-file-large") && !expandedFileIds.has(fileId));
                if (!keepCollapsed) {
                    wrapper.classList.remove("diff-file-collapsed");
                    wrapper.classList.remove("gr-file-collapsed");
                    const button = wrapper.querySelector(".diff-toggle-btn");
                    if (button) renderDiffToggleButtonLabel(button, false);
                    ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
                }
            }
            updateViewedCounter();
        }

        // Toggle a file's viewed state: update the UI right away, then persist
        // it on the server for the current base/compare pair, which records a
        // content hash. Roll back if the request fails.
        function setFileViewed(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            applyViewedState(wrapper, isViewed);
            pendingViewed.set(fileId, isViewed);
            fetch("/api/compare/viewed", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({
                    base: baseSelect.value,
                    compare: compareSelect.value,
                    path: wrapper.dataset.path,
                    viewed: isViewed,
                }),
            }).then((response) => {
                if (!response.ok) throw new Error(`server returned ${response.status}`);
            }).catch((e) => {
                pendingViewed.delete(fileId);
                applyViewedState(wrapper, !isViewed);
                showError(`Failed to save viewed state: ${e.message}`);
            });
        }

        async function ensureBodyLoaded(wrapper) {
            const fileId = wrapper.dataset.diffFileId;
            const path = wrapper.dataset.path;
            const body = wrapper.querySelector(".gr-file-body");
            if (!body || body.dataset.loaded || body.dataset.loading) return;
            const cached = bodyCache.get(fileId);
            if (cached) {
                body.innerHTML = cached.html;
                body.dataset.loaded = "1";
                return;
            }
            const base = baseSelect.value;
            const compare = compareSelect.value;
            if (!base || !compare) return;
            body.dataset.loading = "1";
            body.innerHTML = `<div class="loading">Loading diff…</div>`;
            try {
                const params = new URLSearchParams({ base, compare, path });
                const response = await fetch(`/api/compare/file?${params.toString()}`, { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                const html = typeof data.html === "string" ? data.html : "";
                bodyCache.set(fileId, { html, stats: data });
                body.innerHTML = html;
                body.dataset.loaded = "1";
                if (typeof data.additions === "number" || typeof data.deletions === "number") {
                    updateRowMeta(wrapper, {
                        path: data.path || path,
                        status: data.status,
                        additions: data.additions,
                        deletions: data.deletions,
                    });
                }
            } catch (e) {
                body.innerHTML = `<div class="loading">Error: ${escapeHtml(e.message)}</div>`;
            } finally {
                delete body.dataset.loading;
            }
        }

        function buildFileTree(files) {
            const root = { type: "dir", dirPath: "", children: new Map() };
            for (const meta of files) {
                const parts = meta.path.split("/").filter((p) => p.length > 0);
                let node = root;
                let dirPath = "";
                for (let i = 0; i < parts.length - 1; i++) {
                    const part = parts[i];
                    dirPath = dirPath ? `${dirPath}/${part}` : part;
                    let child = node.children.get(part);
                    if (!child) {
                        child = { type: "dir", dirPath, children: new Map() };
                        node.children.set(part, child);
                    }
                    node = child;
                }
                const leafName = parts[parts.length - 1] || meta.path;
                node.children.set(`__file__${leafName}`, { type: "file", name: leafName, meta });
            }
            collapseSingleChainDirs(root, "");
            return root;
        }

        function collapseSingleChainDirs(node, parentDirPath) {
            for (const child of node.children.values()) {
                if (child.type !== "dir") continue;
                child.displayName = parentDirPath
                    ? child.dirPath.slice(parentDirPath.length + 1)
                    : child.dirPath;
                collapseSingleChainDirs(child, child.dirPath);
                while (child.children.size === 1) {
                    const only = child.children.values().next().value;
                    if (only.type !== "dir") break;
                    child.displayName = `${child.displayName}/${only.displayName}`;
                    child.dirPath = only.dirPath;
                    child.children = only.children;
                }
            }
        }

        function appendTreeNode(parentUl, node) {
            const dirs = [];
            const files = [];
            for (const [, child] of node.children) {
                if (child.type === "dir") dirs.push(child);
                else files.push(child);
            }
            dirs.sort((a, b) => a.dirPath.localeCompare(b.dirPath));
            files.sort((a, b) => a.name.localeCompare(b.name));
            for (const dir of dirs) {
                const li = document.createElement("li");
                li.className = "tree-dir";
                li.dataset.dirPath = dir.dirPath;
                const fileCount = countLeaves(dir);
                const headerEl = document.createElement("div");
                headerEl.className = "tree-dir-header";
                const toggle = document.createElement("button");
                toggle.type = "button";
                toggle.className = "tree-dir-toggle";
                toggle.setAttribute("aria-expanded", "true");
                toggle.textContent = "▾";
                const nameEl = document.createElement("span");
                nameEl.className = "tree-dir-name";
                nameEl.textContent = dir.displayName || dir.dirPath;
                nameEl.title = dir.dirPath;
                const countEl = document.createElement("span");
                countEl.className = "tree-dir-count";
                countEl.textContent = String(fileCount);
                headerEl.appendChild(toggle);
                headerEl.appendChild(nameEl);
                headerEl.appendChild(countEl);
                headerEl.addEventListener("click", (e) => {
                    e.preventDefault();
                    toggleDirCollapsed(li, dir.dirPath);
                });
                li.appendChild(headerEl);
                const childrenUl = document.createElement("ul");
                childrenUl.className = "tree-dir-children";
                appendTreeNode(childrenUl, dir);
                li.appendChild(childrenUl);
                if (sidebarCollapsedDirs.has(dir.dirPath)) {
                    li.classList.add("tree-dir-collapsed");
                    toggle.textContent = "▸";
                    toggle.setAttribute("aria-expanded", "false");
                }
                parentUl.appendChild(li);
            }
            for (const fileNode of files) {
                const meta = fileNode.meta;
                const li = document.createElement("li");
                li.className = "tree-file";
                const a = document.createElement("a");
                a.href = `#${fileAnchorOf(meta.path)}`;
                const text = document.createElement("span");
                text.className = "file-path-text";
                text.textContent = fileNode.name;
                text.title = meta.path;
                a.appendChild(text);
                li.appendChild(a);
                parentUl.appendChild(li);
            }
        }

        function countLeaves(node) {
            let n = 0;
            for (const [, child] of node.children) {
                if (child.type === "file") n += 1;
                else n += countLeaves(child);
            }
            return n;
        }

        function toggleDirCollapsed(li, dirPath) {
            const collapsed = !li.classList.contains("tree-dir-collapsed");
            li.classList.toggle("tree-dir-collapsed", collapsed);
            const toggle = li.querySelector(".tree-dir-toggle");
            if (toggle) {
                toggle.textContent = collapsed ? "▸" : "▾";
                toggle.setAttribute("aria-expanded", collapsed ? "false" : "true");
            }
            if (collapsed) sidebarCollapsedDirs.add(dirPath);
            else sidebarCollapsedDirs.delete(dirPath);
            persistSidebarCollapsedDirs();
        }

        function renderSidebar(files) {
            if (files.length === 0) {
                filesHeading.textContent = "Files";
                setEmpty(filesListContainer, "No changes between branches");
                return;
            }
            filesHeading.textContent = `Files (${files.length})`;
            filesListContainer.innerHTML = "";
            const root = buildFileTree(files);
            const ul = document.createElement("ul");
            ul.className = "file-tree";
            appendTreeNode(ul, root);
            filesListContainer.appendChild(ul);
        }

        function renderMain(files) {
            if (files.length === 0) {
                if (fileObserver) {
                    for (const child of filesMain.children) {
                        if (child.dataset && child.dataset.diffFileId) fileObserver.unobserve(child);
                    }
                }
                filesMain.innerHTML = "";
                setEmpty(filesMain, "No changes between branches");
                return;
            }
            const presentIds = new Set(files.map((m) => fileIdOf(m.path)));
            const existing = new Map();
            for (const child of Array.from(filesMain.children)) {
                if (child.dataset && child.dataset.diffFileId) {
                    if (presentIds.has(child.dataset.diffFileId)) {
                        existing.set(child.dataset.diffFileId, child);
                    } else {
                        if (fileObserver) fileObserver.unobserve(child);
                        child.remove();
                    }
                } else {
                    child.remove();
                }
            }
            for (const id of Array.from(bodyCache.keys())) {
                if (!presentIds.has(id)) bodyCache.delete(id);
            }
            for (const id of Array.from(pendingViewed.keys())) {
                if (!presentIds.has(id)) pendingViewed.delete(id);
            }
            for (const id of Array.from(collapsedFileIds)) {
                if (!presentIds.has(id)) collapsedFileIds.delete(id);
            }
            for (const id of Array.from(expandedFileIds)) {
                if (!presentIds.has(id)) expandedFileIds.delete(id);
            }
            persistCollapsedFileIds();
            persistExpandedFileIds();

            let anchor = null;
            for (const meta of files) {
                const fileId = fileIdOf(meta.path);
                let wrapper = existing.get(fileId);
                if (wrapper) {
                    updateRowMeta(wrapper, meta);
                } else {
                    wrapper = createFileRow(meta);
                    filesMain.appendChild(wrapper);
                }
                if (anchor) {
                    if (anchor.nextSibling !== wrapper) filesMain.insertBefore(wrapper, anchor.nextSibling);
                } else if (filesMain.firstChild !== wrapper) {
                    filesMain.insertBefore(wrapper, filesMain.firstChild);
                }
                anchor = wrapper;
                // The server reports `viewed`; while a toggle's POST is still
                // in flight the optimistic value wins until the server agrees.
                let isViewed = !!meta.viewed;
                if (pendingViewed.has(fileId)) {
                    const want = pendingViewed.get(fileId);
                    if (want === isViewed) pendingViewed.delete(fileId);
                    else isViewed = want;
                }
                const isLarge = isHugeDiff(meta);
                const isCollapsed = shouldCollapseByDefault(fileId, meta, isViewed);
                wrapper.classList.toggle("gr-file-large", isLarge);
                wrapper.classList.toggle("gr-file-viewed", isViewed);
                wrapper.classList.toggle("diff-file-viewed", isViewed);
                wrapper.classList.toggle("gr-file-collapsed", isCollapsed);
                wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
                const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
                if (checkbox) checkbox.checked = isViewed;
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, isCollapsed);
                // Body fetch happens lazily via IntersectionObserver.
            }
        }

        // Populate a base/compare combobox. The control is an <input list>
        // backed by a <datalist>, so the user can type to fuzzy-filter the
        // branch list — much friendlier than a <select> when there are many
        // refs. <datalist> has no <optgroup>, so the list is flat; remote refs
        // already carry their `origin/...` prefix, which tells the user which
        // side a ref belongs to.
        const populateOptions = (input, datalist, branches, preferred) => {
            datalist.innerHTML = "";
            if (branches.length === 0) {
                input.value = "";
                input.placeholder = "(no branches)";
                return;
            }
            const frag = document.createDocumentFragment();
            for (const name of branches) {
                const opt = document.createElement("option");
                opt.value = name;
                frag.appendChild(opt);
            }
            datalist.appendChild(frag);
            if (preferred && branches.includes(preferred)) {
                input.value = preferred;
            } else {
                input.value = branches[0];
            }
        };

        async function loadBranches() {
            try {
                const response = await fetch("/api/branches", { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                const branches = Array.isArray(data.branches) ? data.branches : [];
                knownBranches.clear();
                for (const b of branches) knownBranches.add(b);
                const current = typeof data.current === "string" ? data.current : null;
                const defaultBranch = typeof data.default === "string" ? data.default : null;

                const baseParam = urlParams.get("base");
                const compareParam = urlParams.get("compare");
                const baseFallback =
                    (defaultBranch && branches.includes(defaultBranch) && defaultBranch !== current
                        ? defaultBranch : null)
                    || (defaultBranch && branches.includes(defaultBranch) ? defaultBranch : null)
                    || branches.find((b) => b !== current)
                    || branches[0]
                    || "";
                const compareFallback = current || branches[0] || "";
                populateOptions(baseSelect, baseList, branches, baseParam || baseFallback);
                populateOptions(compareSelect, compareList, branches, compareParam || compareFallback);
            } catch (e) {
                showError(e.message);
                baseList.innerHTML = "";
                compareList.innerHTML = "";
                baseSelect.placeholder = "(error)";
                compareSelect.placeholder = "(error)";
            }
        }

        async function loadCompare({ showLoading = true } = {}) {
            const base = baseSelect.value;
            const compare = compareSelect.value;
            if (!base || !compare) {
                setEmpty(filesListContainer, "Select a base and compare branch...");
                setEmpty(filesMain, "Select a base and compare branch...");
                return;
            }
            // The combobox is a free-text <input>, so guard against typos /
            // partial input that don't name a real branch before fetching.
            if (knownBranches.size > 0 && (!knownBranches.has(base) || !knownBranches.has(compare))) {
                const bad = !knownBranches.has(base) ? base : compare;
                setEmpty(filesListContainer, `Unknown branch: ${bad}`);
                setEmpty(filesMain, `Unknown branch: ${bad}`);
                return;
            }
            if (isLoadingCompare) return;
            isLoadingCompare = true;
            clearError();
            if (showLoading) {
                setLoading(filesListContainer, "Loading files...");
                setLoading(filesMain, `Loading files for ${base}...${compare}...`);
            }
            // Drop body cache when base/compare changes
            bodyCache.clear();
            try {
                const params = new URLSearchParams({ base, compare });
                const response = await fetch(`/api/compare?${params.toString()}`, { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                latestFiles = Array.isArray(data.files) ? data.files : [];
                renderSidebar(latestFiles);
                renderMain(latestFiles);
                updateViewedCounter();
            } finally {
                isLoadingCompare = false;
            }
        }

        const persistControls = () => {
            const params = new URLSearchParams();
            if (baseSelect.value) params.set("base", baseSelect.value);
            if (compareSelect.value) params.set("compare", compareSelect.value);
            history.replaceState(null, "", `/compare?${params.toString()}`);
        };

        refreshButton.addEventListener("click", () => {
            bodyCache.clear();
            loadCompare().catch((e) => showError(e.message));
        });
        baseSelect.addEventListener("change", () => {
            persistControls();
            loadCompare().catch((e) => showError(e.message));
        });
        compareSelect.addEventListener("change", () => {
            persistControls();
            loadCompare().catch((e) => showError(e.message));
        });
        swapButton.addEventListener("click", () => {
            const a = baseSelect.value;
            const b = compareSelect.value;
            baseSelect.value = b;
            compareSelect.value = a;
            persistControls();
            loadCompare().catch((e) => showError(e.message));
        });
        goTopButton.addEventListener("click", () => {
            window.scrollTo({ top: 0, behavior: "smooth" });
        });

        // Delegated handler for hunk-spacer expand buttons on the compare page.
        // The fetched lines come from the currently-selected compare ref so PR
        // review jumps stay consistent with what the diff above/below shows.
        document.addEventListener("click", async (e) => {
            const btn = e.target.closest && e.target.closest(".gr-expand-btn");
            if (!btn || btn.disabled) return;
            const wrapper = btn.closest(".gr-file");
            if (!wrapper) return;
            const oldStart = parseInt(btn.dataset.oldStart, 10);
            const newStart = parseInt(btn.dataset.newStart, 10);
            const newEnd = parseInt(btn.dataset.newEnd, 10);
            if (!Number.isFinite(newStart) || !Number.isFinite(newEnd) || newEnd < newStart) return;
            const ref = compareSelect && compareSelect.value;
            if (!ref) { showError("Pick a compare ref before expanding context"); return; }
            const path = wrapper.dataset.path;
            btn.disabled = true;
            try {
                const params = new URLSearchParams({
                    path,
                    ref,
                    start: String(newStart),
                    end: String(newEnd),
                });
                const resp = await fetch(`/api/compare/context?${params}`);
                if (!resp.ok) throw new Error(`server returned ${resp.status}`);
                const data = await resp.json();
                const lines = Array.isArray(data.lines) ? data.lines : [];
                const frag = document.createDocumentFragment();
                for (let i = 0; i < lines.length; i++) {
                    const row = document.createElement("div");
                    row.className = "gr-line gr-line-context";
                    const ln = document.createElement("span"); ln.className = "gr-ln"; ln.textContent = String(oldStart + i);
                    const lnr = document.createElement("span"); lnr.className = "gr-lnr"; lnr.textContent = String(newStart + i);
                    const sign = document.createElement("span"); sign.className = "gr-sign"; sign.textContent = " ";
                    const text = document.createElement("span"); text.className = "gr-text"; text.textContent = lines[i];
                    row.appendChild(ln); row.appendChild(lnr); row.appendChild(sign); row.appendChild(text);
                    frag.appendChild(row);
                }
                btn.closest(".gr-line-expand").replaceWith(frag);
            } catch (err) {
                btn.disabled = false;
                showError(`Failed to expand context: ${err.message}`);
            }
        });

        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });

        (async () => {
            await loadBranches();
            persistControls();
            await loadCompare().catch((e) => showError(e.message));
        })();

        updateGoTopButtonVisibility();
    </script>
    </main>
</div>
</body>
</html>"#;

/// Side-by-side ("split view") page template. Server-rendered: the body
/// is built in Rust and embedded inline, no XHR. Keyboard handling
/// (`j`/`k`/`n`/`p`/`gg`/`G`/`q`) lives in the shared `SHORTCUTS_JS`.
const SPLIT_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Split — {{PATH}}</title>
    <style>
{{SHARED_CSS}}
{{DIFF_STYLES}}
    </style>
{{SPLIT_STYLES}}
</head>
<body data-page="split">
{{REPO_CTX_SCRIPT}}
<div class="app-shell">
{{APP_RAIL}}
<main class="app-main">
<header class="split-header">
    <a class="split-back" href="{{BACK_URL}}">← Back</a>
    <span class="split-path">{{PATH_LABEL}}</span>
    <span class="split-refs">{{REFS_LABEL}}</span>
</header>
{{NOTICE}}
<div class="split-grid">{{SPLIT_BODY}}</div>
</main>
</div>
<script>{{SHORTCUTS_JS}}</script>
<script>
document.addEventListener("DOMContentLoaded", function () {
    var target = document.getElementById("first-diff");
    if (target && typeof target.scrollIntoView === "function") {
        target.scrollIntoView({ block: "center" });
    }
});
</script>
</body>
</html>"#;

/// Origin of a split-view request. Determines which refs to read the old
/// and new file contents from, plus what the "Back" link points to.
#[derive(Debug, Clone)]
enum SplitSource {
    Status { section: String },
    Compare { base: String, compare: String },
    Commit { hash: String },
}

fn parse_split_source(params: &HashMap<String, String>) -> Result<SplitSource, String> {
    let src = params
        .get("source")
        .map(String::as_str)
        .ok_or_else(|| "missing `source` query parameter".to_string())?;
    match src {
        "status" => {
            let section = params
                .get("section")
                .ok_or_else(|| "missing `section` query parameter".to_string())?;
            match section.as_str() {
                "staged" | "unstaged" | "untracked" => Ok(SplitSource::Status {
                    section: section.clone(),
                }),
                _ => Err(format!("invalid section: {section}")),
            }
        }
        "compare" => {
            let base_raw = params
                .get("base")
                .ok_or_else(|| "missing `base` query parameter".to_string())?;
            let compare_raw = params
                .get("compare")
                .ok_or_else(|| "missing `compare` query parameter".to_string())?;
            let base = parse_branch_name(base_raw)
                .ok_or_else(|| format!("invalid branch name: {base_raw}"))?;
            let compare = parse_branch_name(compare_raw)
                .ok_or_else(|| format!("invalid branch name: {compare_raw}"))?;
            Ok(SplitSource::Compare { base, compare })
        }
        "commit" => {
            let hash_raw = params
                .get("hash")
                .ok_or_else(|| "missing `hash` query parameter".to_string())?;
            let hash = parse_commit_hash_value(hash_raw)
                .ok_or_else(|| format!("invalid commit hash: {hash_raw}"))?;
            Ok(SplitSource::Commit { hash })
        }
        other => Err(format!("invalid source: {other}")),
    }
}

/// Map a split source to `(old_ref, new_ref)`. `None` means "working tree";
/// `Some("")` means the git index (`git show :path`).
fn split_refs(source: &SplitSource) -> (Option<String>, Option<String>) {
    match source {
        SplitSource::Status { section } => match section.as_str() {
            "staged" => (Some("HEAD".into()), Some(String::new())),
            "unstaged" => (Some(String::new()), None),
            "untracked" => (None, None),
            _ => (None, None),
        },
        SplitSource::Compare { base, compare } => (Some(base.clone()), Some(compare.clone())),
        SplitSource::Commit { hash } => (Some(format!("{hash}^")), Some(hash.clone())),
    }
}

/// 64-hex limit covers SHA-256; the github_server module has a duplicate.
/// Kept private here so `/split` doesn't depend on the github router module.
fn parse_commit_hash_value(hash: &str) -> Option<String> {
    if hash.is_empty() || hash.len() > 64 {
        return None;
    }
    if hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_string())
    } else {
        None
    }
}

/// Fetch full file contents at a ref (or working tree). Returns `None` when
/// the path does not exist on that side (added on new, deleted on old, etc.)
/// so the caller can render a one-sided split.
async fn read_full_file_at_ref(
    repo_root: &Path,
    git_ref: Option<&str>,
    rel_path: &str,
) -> Result<Option<Vec<String>>, String> {
    let exists = match git_ref {
        Some(r) => {
            let spec = format!("{}:{}", r, rel_path);
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["-c", "core.quotepath=off"]);
            cmd.args(["-c", "core.optionalLocks=false"]);
            cmd.args(["cat-file", "-e", &spec]);
            cmd.current_dir(repo_root);
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            cmd.status().await.map(|s| s.success()).unwrap_or(false)
        }
        None => {
            let full = repo_root.join(rel_path);
            tokio::fs::try_exists(&full).await.unwrap_or(false)
        }
    };
    if !exists {
        return Ok(None);
    }
    let lines = read_file_range_at_ref(repo_root, git_ref, rel_path, 1, usize::MAX).await?;
    Ok(Some(lines))
}

/// Load the parsed `DiffFile` for any split source. Reuses the existing
/// per-page loaders so status/compare paths share identical git invocations
/// with their non-split counterparts.
async fn load_split_diff_file(
    repo_root: &Path,
    source: &SplitSource,
    path: &str,
) -> Result<Option<DiffFile>, String> {
    match source {
        SplitSource::Status { section } => load_status_diff_file(repo_root, section, path).await,
        SplitSource::Compare { base, compare } => {
            load_compare_diff_file(repo_root, base, compare, path).await
        }
        SplitSource::Commit { hash } => {
            let diff = git_output_in_repo(
                repo_root,
                &["show", "--format=", "--no-ext-diff", hash, "--", path],
            )
            .await?;
            Ok(parse_unified_diff(&diff).into_iter().next())
        }
    }
}

fn ref_label(r: Option<&str>) -> String {
    match r {
        None => "working tree".to_string(),
        Some("") => "index".to_string(),
        Some(name) => name.to_string(),
    }
}

/// Soft cap on combined old + new line count. The split view loads the
/// whole file into the DOM in a single pass; beyond this much content the
/// browser stalls long enough to feel broken.
const SPLIT_MAX_LINES: usize = 50_000;

/// Build per-line highlight maps (keyed by 1-based line number) from the
/// full text of one side using the language inferred from `path`.
fn build_line_highlights(lines: &[String], path: &str) -> Option<LineHl> {
    let registry = LanguageRegistry::new();
    let lang = registry.detect_by_extension(path)?;
    let joined = lines.join("\n");
    let spans_per_row = highlight_text(&joined, lang);
    let mut out: LineHl = HashMap::new();
    for (row, spans) in spans_per_row {
        if spans.is_empty() {
            continue;
        }
        let content_len = lines.get(row).map(|s| s.len()).unwrap_or(0);
        let mut packed: Vec<(usize, usize, String)> = Vec::with_capacity(spans.len());
        for s in spans {
            let start = s.start.min(content_len);
            let end = s.end.min(content_len);
            if start < end {
                packed.push((start, end, s.capture_name));
            }
        }
        if !packed.is_empty() {
            out.insert(row + 1, LineHighlights { spans: packed });
        }
    }
    Some(out)
}

fn split_back_url(
    source: &SplitSource,
    url_ctx: &crate::command::github_preview_server::RepoUrlContext,
) -> String {
    // Inputs are pre-validated (parse_branch_name, parse_commit_hash_value,
    // owner/repo from git config), so no percent-encoding needed here.
    match source {
        SplitSource::Status { .. } => "/status".to_string(),
        SplitSource::Compare { base, compare } => {
            format!("/compare?base={base}&compare={compare}")
        }
        SplitSource::Commit { hash } => {
            if url_ctx.owner.is_empty() || url_ctx.repo.is_empty() {
                "/status".to_string()
            } else {
                format!("/{}/{}/commit/{}", url_ctx.owner, url_ctx.repo, hash)
            }
        }
    }
}

/// Serve the split-view HTML page for a single file.
pub(crate) async fn handle_split_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let source = match parse_split_source(&params) {
        Ok(s) => s,
        Err(e) => return bad_request(e),
    };
    let path_raw = match params.get("path") {
        Some(v) => v,
        None => return bad_request("missing `path` query parameter"),
    };
    let path = match parse_diff_path(path_raw) {
        Some(p) => p,
        None => return bad_request(format!("invalid path: {path_raw}")),
    };

    let (old_ref, new_ref) = split_refs(&source);
    let repo_root = &state.project_root;

    // Pull the parsed DiffFile (hunks + rename info). For untracked status,
    // the diff loader may return None when the file is brand-new; in that
    // case we render right-only from the working-tree contents only.
    let diff_file = match load_split_diff_file(repo_root, &source, &path).await {
        Ok(f) => f,
        Err(e) => return bad_request(e),
    };

    // For renames the old side reads from `old_path`, not `path`.
    let old_read_path: String = diff_file
        .as_ref()
        .and_then(|f| f.old_path.clone())
        .unwrap_or_else(|| path.clone());

    // Commit on the root: parent ref doesn't exist. Catch the read error and
    // fall back to right-only rather than 400ing the page.
    let old_lines: Option<Vec<String>> =
        read_full_file_at_ref(repo_root, old_ref.as_deref(), &old_read_path)
            .await
            .unwrap_or_default();
    let new_lines = match read_full_file_at_ref(repo_root, new_ref.as_deref(), &path).await {
        Ok(v) => v,
        Err(e) => return bad_request(e),
    };

    // Synthesize a minimal DiffFile when the per-source loader had nothing
    // to say (e.g. brand-new untracked file). build_split_rows still needs
    // an instance to look at `hunks`/`old_path`/`binary`.
    let diff_file = diff_file.unwrap_or_else(|| DiffFile {
        path: path.clone(),
        old_path: None,
        status: if old_lines.is_none() {
            FileStatus::Added
        } else if new_lines.is_none() {
            FileStatus::Deleted
        } else {
            FileStatus::Modified
        },
        binary: false,
        hunks: Vec::new(),
        additions: 0,
        deletions: 0,
    });

    let ctx = crate::command::github_preview_server::resolve_repo_url_context(repo_root).await;
    let repo_url = crate::command::github_preview_server::github_repo_url(repo_root).await;
    let active_tab = match &source {
        SplitSource::Status { .. } => "status",
        SplitSource::Compare { .. } => "branches",
        SplitSource::Commit { .. } => "commits",
    };
    let rail = crate::command::app_shell::app_rail_html(&ctx, repo_url.as_deref(), active_tab);
    let ctx_script = repo_ctx_script(&ctx);
    let back_url = split_back_url(&source, &ctx);

    let path_label = match &diff_file.old_path {
        Some(op) if op != &diff_file.path => {
            format!("{} → {}", html_escape(op), html_escape(&diff_file.path))
        }
        _ => html_escape(&path),
    };
    let refs_label = format!(
        "{} → {}",
        html_escape(&ref_label(old_ref.as_deref())),
        html_escape(&ref_label(new_ref.as_deref())),
    );

    // Decide the page body. Binary, oversize, and one-sided pages get a
    // notice banner above the grid (or in place of it).
    let (notice_html, body_html) = if diff_file.binary {
        (
            r#"<div class="split-notice">Binary file — side-by-side view is not available.</div>"#
                .to_string(),
            String::new(),
        )
    } else {
        let old_len = old_lines.as_ref().map(|v| v.len()).unwrap_or(0);
        let new_len = new_lines.as_ref().map(|v| v.len()).unwrap_or(0);
        if old_len + new_len > SPLIT_MAX_LINES {
            (
                format!(
                    r#"<div class="split-notice">File too large for split view ({} + {} = {} lines, cap {}). Use the standard diff page.</div>"#,
                    old_len,
                    new_len,
                    old_len + new_len,
                    SPLIT_MAX_LINES
                ),
                String::new(),
            )
        } else {
            let rows = build_split_rows(old_lines.as_deref(), new_lines.as_deref(), &diff_file);
            let old_hl = old_lines
                .as_deref()
                .and_then(|l| build_line_highlights(l, &old_read_path));
            let new_hl = new_lines
                .as_deref()
                .and_then(|l| build_line_highlights(l, &path));
            let body = render_split_html(&rows, old_hl.as_ref(), new_hl.as_ref());
            let mut notice = String::new();
            if old_lines.is_none() {
                notice = r#"<div class="split-notice">Old version not present (added or untracked) — only the new side is shown.</div>"#.to_string();
            } else if new_lines.is_none() {
                notice = r#"<div class="split-notice">New version not present (deleted) — only the old side is shown.</div>"#.to_string();
            }
            (notice, body)
        }
    };

    let html = SPLIT_HTML_TEMPLATE
        .replace("{{PATH}}", &html_escape(&path))
        .replace("{{PATH_LABEL}}", &path_label)
        .replace("{{REFS_LABEL}}", &refs_label)
        .replace("{{APP_RAIL}}", &rail)
        .replace("{{REPO_CTX_SCRIPT}}", &ctx_script)
        .replace("{{SHARED_CSS}}", crate::command::server_shared::SHARED_CSS)
        .replace(
            "{{SHORTCUTS_JS}}",
            crate::command::server_shared::SHORTCUTS_JS,
        )
        .replace("{{DIFF_STYLES}}", render_diff_styles())
        .replace("{{SPLIT_STYLES}}", render_split_styles())
        .replace("{{BACK_URL}}", &html_escape(&back_url))
        .replace("{{NOTICE}}", &notice_html)
        .replace("{{SPLIT_BODY}}", &body_html);

    Html(html).into_response()
}

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<DiffServerState>,
) {
    let app = Router::new()
        .route("/diff", get(handle_html_request))
        .route("/compare", get(handle_compare_html_request))
        .route("/split", get(handle_split_request))
        .route("/commit", get(handle_commit_html_request))
        .route("/api/status", get(handle_api_status_request))
        .route("/api/status/file", get(handle_api_status_file_request))
        .route("/api/status/viewed", post(handle_api_status_viewed_request))
        .route("/api/status/stage", post(handle_api_status_stage_request))
        .route(
            "/api/status/unstage",
            post(handle_api_status_unstage_request),
        )
        .route(
            "/api/status/commit-prepare",
            get(handle_api_commit_prepare_request),
        )
        .route("/api/status/commit", post(handle_api_commit_request))
        .route(
            "/api/status/context",
            get(handle_api_status_context_request),
        )
        .route("/api/branches", get(handle_api_branches_request))
        .route("/api/compare", get(handle_api_compare_request))
        .route("/api/compare/file", get(handle_api_compare_file_request))
        .route(
            "/api/compare/context",
            get(handle_api_compare_context_request),
        )
        .route(
            "/api/compare/viewed",
            post(handle_api_compare_viewed_request),
        )
        .with_state(state)
        .layer(CorsLayer::permissive());

    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

/// Serve the HTML page with diff2html
pub(crate) async fn handle_html_request(
    State(state): State<Arc<DiffServerState>>,
) -> impl IntoResponse {
    use crate::command::github_preview_server as gh;
    let root_path = state.project_root.display().to_string();
    let ctx = gh::resolve_repo_url_context(&state.project_root).await;
    let repo_url = gh::github_repo_url(&state.project_root).await;
    let rail = crate::command::app_shell::app_rail_html(&ctx, repo_url.as_deref(), "status");
    let ctx_script = repo_ctx_script(&ctx);
    Html(
        DIFF_HTML_TEMPLATE
            .replace("{{ROOT_PATH}}", &html_escape(&root_path))
            .replace("{{APP_RAIL}}", &rail)
            .replace("{{REPO_CTX_SCRIPT}}", &ctx_script)
            .replace("{{SHARED_CSS}}", crate::command::server_shared::SHARED_CSS)
            .replace(
                "{{SHORTCUTS_JS}}",
                crate::command::server_shared::SHORTCUTS_JS,
            )
            .replace("{{DIFF_STYLES}}", render_diff_styles()),
    )
}

/// Serve the commit page: a focused view that lists the staged files and takes
/// a commit message + optional amend, then POSTs to `/api/status/commit`.
pub(crate) async fn handle_commit_html_request(
    State(state): State<Arc<DiffServerState>>,
) -> impl IntoResponse {
    use crate::command::github_preview_server as gh;
    let ctx = gh::resolve_repo_url_context(&state.project_root).await;
    let repo_url = gh::github_repo_url(&state.project_root).await;
    // Keep "Status" highlighted in the rail — the commit page is part of that flow.
    let rail = crate::command::app_shell::app_rail_html(&ctx, repo_url.as_deref(), "status");
    let ctx_script = repo_ctx_script(&ctx);
    Html(
        COMMIT_HTML_TEMPLATE
            .replace("{{APP_RAIL}}", &rail)
            .replace("{{REPO_CTX_SCRIPT}}", &ctx_script)
            .replace("{{SHARED_CSS}}", crate::command::server_shared::SHARED_CSS)
            .replace(
                "{{SHORTCUTS_JS}}",
                crate::command::server_shared::SHORTCUTS_JS,
            ),
    )
}

fn parse_bool_param(value: Option<&String>, default: bool) -> bool {
    match value.map(|v| v.as_str()) {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Scan an untracked file: count its lines, detect whether it is binary, and
/// fingerprint its content for the "Viewed" checkbox.
///
/// Returns `(line_count, is_binary, content_hash)`. The read is capped at
/// `MAX_SCAN_BYTES` to bound memory: a file larger than the cap is certainly
/// huge, and the newline count within the scanned prefix is already well past
/// the collapse threshold for text. A file is treated as binary when it
/// contains a NUL byte, in which case it reports a zero line count. The hash
/// folds in the file's full length so growth past the cap is still detected;
/// it is empty only when the file cannot be read.
async fn scan_untracked_file(repo_root: &Path, rel_path: &str) -> (usize, bool, String) {
    use tokio::io::AsyncReadExt;

    const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024;

    let full = repo_root.join(rel_path);
    let total_len = tokio::fs::metadata(&full)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let file = match tokio::fs::File::open(&full).await {
        Ok(f) => f,
        Err(_) => return (0, false, String::new()),
    };
    let mut buf = Vec::new();
    if file
        .take(MAX_SCAN_BYTES)
        .read_to_end(&mut buf)
        .await
        .is_err()
    {
        return (0, false, String::new());
    }
    let hash = content_hash_of_bytes(&buf, total_len);
    if buf.contains(&0) {
        return (0, true, hash);
    }
    if buf.is_empty() {
        return (0, false, hash);
    }
    let mut lines = buf.iter().filter(|&&b| b == b'\n').count();
    // A final line without a trailing newline still counts as a line.
    if buf.last() != Some(&b'\n') {
        lines += 1;
    }
    (lines, false, hash)
}

pub(crate) async fn git_output_in_repo(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["-c", "core.quotepath=off"]);
    cmd.args(["-c", "core.optionalLocks=false"]);
    cmd.args(args);
    cmd.current_dir(repo_root);
    git_output_from_command(cmd, &[], &format!("git {}", args.join(" "))).await
}

async fn git_output_from_command(
    mut cmd: tokio::process::Command,
    accepted_exit_codes: &[i32],
    display_cmd: &str,
) -> Result<String, String> {
    match cmd.output().await {
        Ok(output) if output.status.success() => Ok(String::from_utf8_lossy(&output.stdout).into()),
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            if accepted_exit_codes.contains(&code) {
                return Ok(String::from_utf8_lossy(&output.stdout).into());
            }

            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if error.is_empty() {
                Err(format!(
                    "Git command failed: {} (exit code {})",
                    display_cmd, code
                ))
            } else {
                Err(error)
            }
        }
        Err(e) => Err(format!("Failed to execute git: {}", e)),
    }
}

/// API endpoint that returns unstaged/staged diffs and untracked files.
pub(crate) async fn handle_api_status_request(
    State(state): State<Arc<DiffServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let show_untracked = parse_bool_param(params.get("show_untracked"), true);
    let repo_root = &state.project_root;

    let unstaged_raw = match git_output_in_repo(repo_root, &["diff"]).await {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };
    let staged_raw = match git_output_in_repo(repo_root, &["diff", "--cached"]).await {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    // One lookup of the persisted viewed state for the whole page; each file is
    // reported viewed only while its current content hash still matches.
    let viewed = load_viewed_map(&state, PAGE_STATUS, String::new(), String::new()).await;

    let unstaged_files: Vec<serde_json::Value> = parse_unified_diff(&unstaged_raw)
        .iter()
        .map(|f| file_metadata_json(f, diff_file_is_viewed(&viewed, "unstaged", f)))
        .collect();
    let staged_files: Vec<serde_json::Value> = parse_unified_diff(&staged_raw)
        .iter()
        .map(|f| file_metadata_json(f, diff_file_is_viewed(&viewed, "staged", f)))
        .collect();

    let untracked_files: Vec<serde_json::Value> = if show_untracked {
        let raw =
            match git_output_in_repo(repo_root, &["ls-files", "--others", "--exclude-standard"])
                .await
            {
                Ok(output) => output,
                Err(error) => return bad_request(error),
            };
        let mut entries = Vec::new();
        for path in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
            // A whole untracked file shows up as an all-additions diff, so its
            // line count drives the client's huge-diff collapse decision.
            let (additions, binary, hash) = scan_untracked_file(repo_root, path).await;
            let is_viewed = viewed
                .get(&("untracked".to_string(), path.to_string()))
                .is_some_and(|stored| !hash.is_empty() && *stored == hash);
            entries.push(serde_json::json!({
                "path": path,
                "old_path": serde_json::Value::Null,
                "status": "untracked",
                "binary": binary,
                "additions": additions,
                "deletions": 0,
                "viewed": is_viewed,
            }));
        }
        entries
    } else {
        Vec::new()
    };

    ok_json(serde_json::json!({
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

/// Fetch N lines from a file at a given git ref (or working tree).
///
/// `git_ref = Some("")` reads from the index (`git show :path`),
/// `Some("HEAD")` from HEAD, etc.; `None` reads from the working tree.
/// Lines are returned with their original content (newlines stripped).
async fn read_file_range_at_ref(
    repo_root: &Path,
    git_ref: Option<&str>,
    rel_path: &str,
    start: usize,
    end: usize,
) -> Result<Vec<String>, String> {
    let content = match git_ref {
        Some(r) => {
            let spec = format!("{}:{}", r, rel_path);
            git_output_in_repo(repo_root, &["show", &spec]).await?
        }
        None => {
            let path = repo_root.join(rel_path);
            tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("read {}: {}", path.display(), e))?
        }
    };
    let lines: Vec<&str> = content.split_terminator('\n').collect();
    let total = lines.len();
    let start_idx = start.saturating_sub(1).min(total);
    let end_idx = end.min(total);
    if start_idx >= end_idx {
        return Ok(Vec::new());
    }
    Ok(lines[start_idx..end_idx]
        .iter()
        .map(|s| s.to_string())
        .collect())
}

fn parse_usize_param(params: &HashMap<String, String>, key: &str) -> Result<usize, String> {
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

/// Run the per-file `git diff` for a status section and return the parsed
/// [`DiffFile`], if any. Shared by the file-HTML and set-viewed endpoints so
/// both hash and render over identical data.
async fn load_status_diff_file(
    repo_root: &Path,
    section: &str,
    path: &str,
) -> Result<Option<DiffFile>, String> {
    let diff_text = match section {
        "staged" => git_output_in_repo(repo_root, &["diff", "--cached", "--", path]).await?,
        "unstaged" => git_output_in_repo(repo_root, &["diff", "--", path]).await?,
        "untracked" => {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["-c", "core.quotepath=off"]);
            cmd.args(["-c", "core.optionalLocks=false"]);
            cmd.args(["diff", "--no-index", "--", "/dev/null", path]);
            cmd.current_dir(repo_root);
            git_output_from_command(
                cmd,
                &[1],
                &format!("git diff --no-index -- /dev/null {}", path),
            )
            .await?
        }
        _ => return Err(format!("invalid section: {section}")),
    };
    let mut files = parse_unified_diff(&diff_text);
    if section == "untracked" {
        for f in &mut files {
            f.status = FileStatus::Untracked;
        }
    }
    Ok(files.into_iter().next())
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
    match git_output_in_repo(&state.project_root, &["add", "--", &path]).await {
        Ok(_) => ok_json(serde_json::json!({ "ok": true })),
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
    match git_output_in_repo(&state.project_root, &["reset", "--quiet", "--", &path]).await {
        Ok(_) => ok_json(serde_json::json!({ "ok": true })),
        Err(e) => bad_request(e),
    }
}

/// GET endpoint backing the commit page: the list of staged files, the current
/// branch, and HEAD's subject+body (so the amend toggle can prefill it).
pub(crate) async fn handle_api_commit_prepare_request(
    State(state): State<Arc<DiffServerState>>,
) -> Response {
    let repo_root = &state.project_root;
    let staged_raw = match git_output_in_repo(repo_root, &["diff", "--cached"]).await {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };
    let staged: Vec<serde_json::Value> = parse_unified_diff(&staged_raw)
        .iter()
        .map(|f| file_metadata_json(f, false))
        .collect();

    // Current branch (empty on a detached HEAD or fresh repo).
    let branch = git_output_in_repo(repo_root, &["symbolic-ref", "--short", "HEAD"])
        .await
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    // HEAD's full message for the amend toggle to prefill. Empty before the
    // first commit, in which case amend is not offered.
    let last_message = git_output_in_repo(repo_root, &["log", "-1", "--pretty=%B"])
        .await
        .map(|s| s.trim_end().to_string())
        .unwrap_or_default();
    let has_head = git_output_in_repo(repo_root, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

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

pub(crate) fn file_metadata_json(file: &DiffFile, viewed: bool) -> serde_json::Value {
    serde_json::json!({
        "path": file.path,
        "old_path": file.old_path,
        "status": file.status.as_str(),
        "binary": file.binary,
        "additions": file.additions,
        "deletions": file.deletions,
        "viewed": viewed,
    })
}

/// `(section, path) -> stored content hash` for one page / branch context.
type ViewedMap = HashMap<(String, String), String>;

/// Load every viewed-file record for a page / branch context off the async
/// runtime, since a contended SQLite read can block briefly on `busy_timeout`.
async fn load_viewed_map(
    state: &Arc<DiffServerState>,
    page: &'static str,
    base_ref: String,
    compare_ref: String,
) -> ViewedMap {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        state
            .viewed
            .viewed_map(&state.repo_key(), page, &base_ref, &compare_ref)
    })
    .await
    .unwrap_or_default()
}

/// Persist (or, with `hash == None`, clear) a file's viewed record off the
/// async runtime. Best-effort: failures leave the viewed state unpersisted.
#[allow(clippy::too_many_arguments)]
async fn store_viewed(
    state: &Arc<DiffServerState>,
    page: &'static str,
    base_ref: String,
    compare_ref: String,
    section: String,
    path: String,
    hash: Option<String>,
) {
    let state = state.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let key = state.repo_key();
        match hash {
            Some(h) => state
                .viewed
                .set(&key, page, &base_ref, &compare_ref, &section, &path, &h),
            None => state
                .viewed
                .unset(&key, page, &base_ref, &compare_ref, &section, &path),
        }
    })
    .await;
}

/// Whether `file`'s current content matches its stored viewed record.
fn diff_file_is_viewed(viewed: &ViewedMap, section: &str, file: &DiffFile) -> bool {
    viewed
        .get(&(section.to_string(), file.path.clone()))
        .is_some_and(|stored| *stored == content_hash_of(file))
}

pub(crate) fn empty_diff_html() -> String {
    r#"<div class="gr-diff-body"><div class="gr-line gr-line-hunk"><span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span><span class="gr-text">(no content changes)</span></div></div>"#
        .to_string()
}

/// Render `file` to HTML, applying tree-sitter syntax highlighting when
/// the file's extension maps to a known language. Falls back to plain
/// rendering for unknown languages, binary files, or rename-only entries.
pub(crate) fn render_highlighted(file: &DiffFile) -> String {
    if file.binary || file.hunks.is_empty() {
        return render_file_body_html(file);
    }
    let registry = LanguageRegistry::new();
    let Some(lang) = registry.detect_by_extension(&file.path) else {
        return render_file_body_html(file);
    };
    let highlights = compute_diff_highlights(file, lang);
    render_file_body_html_with_highlights(file, &highlights)
}

/// Reconstruct the new- and old-side line streams for `file`, run
/// `highlight_text` over each, and translate the per-row span maps back
/// into `(hunk_idx, line_idx) → LineHighlights`.
///
/// Single-line fragments don't parse cleanly under tree-sitter (`fn foo(`
/// alone isn't valid Rust), so we feed both sides as a single body each.
/// Context lines receive their spans from the new-side pass; the old-side
/// pass only attaches to actual Remove lines.
fn compute_diff_highlights(file: &DiffFile, lang: &LanguageDef) -> DiffHighlights {
    let mut result: DiffHighlights = HashMap::new();

    // New side: Context + Add.
    let mut new_text = String::new();
    let mut new_map: Vec<(usize, usize)> = Vec::new();
    for (hi, hunk) in file.hunks.iter().enumerate() {
        for (li, line) in hunk.lines.iter().enumerate() {
            if matches!(line.kind, LineKind::Context | LineKind::Add) {
                new_map.push((hi, li));
                new_text.push_str(&line.content);
                new_text.push('\n');
            }
        }
    }
    if !new_text.is_empty() {
        let spans_per_row = highlight_text(&new_text, lang);
        for (row, key) in new_map.iter().enumerate() {
            let Some(spans) = spans_per_row.get(&row) else {
                continue;
            };
            let content_len = file.hunks[key.0].lines[key.1].content.len();
            let entry = result.entry(*key).or_default();
            for s in spans {
                let start = s.start.min(content_len);
                let end = s.end.min(content_len);
                if start < end {
                    entry.spans.push((start, end, s.capture_name.clone()));
                }
            }
        }
    }

    // Old side: Context + Remove, but only attach to Remove lines.
    let mut old_text = String::new();
    let mut old_map: Vec<(usize, usize)> = Vec::new();
    for (hi, hunk) in file.hunks.iter().enumerate() {
        for (li, line) in hunk.lines.iter().enumerate() {
            if matches!(line.kind, LineKind::Context | LineKind::Remove) {
                old_map.push((hi, li));
                old_text.push_str(&line.content);
                old_text.push('\n');
            }
        }
    }
    if !old_text.is_empty() {
        let spans_per_row = highlight_text(&old_text, lang);
        for (row, key) in old_map.iter().enumerate() {
            let (hi, li) = *key;
            if file.hunks[hi].lines[li].kind != LineKind::Remove {
                continue;
            }
            let Some(spans) = spans_per_row.get(&row) else {
                continue;
            };
            let content_len = file.hunks[hi].lines[li].content.len();
            let entry = result.entry(*key).or_default();
            for s in spans {
                let start = s.start.min(content_len);
                let end = s.end.min(content_len);
                if start < end {
                    entry.spans.push((start, end, s.capture_name.clone()));
                }
            }
        }
    }

    result
}

/// Validate a relative path used as a git diff argument.
///
/// We always pass paths after `--` so flag injection is structurally blocked,
/// but we still reject control characters, path traversal, absolute paths,
/// and unreasonably long inputs to keep the API surface tight.
pub(crate) fn parse_diff_path(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 4096 {
        return None;
    }
    if value.starts_with('-') || value.starts_with('/') {
        return None;
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return None;
    }
    for segment in value.split('/') {
        if segment == ".." {
            return None;
        }
    }
    Some(value.to_string())
}

/// Validate a git branch name to block flag injection and command injection.
///
/// Accepts the conservative subset `[A-Za-z0-9._/\-]`, rejects names that start
/// with `-` (so they can never be parsed as a git CLI flag), and caps the length
/// to bound the work git has to do on a malicious input.
fn parse_branch_name(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 256 {
        return None;
    }
    if value.starts_with('-') {
        return None;
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'));
    if !ok {
        return None;
    }
    Some(value.to_string())
}

pub(crate) fn bad_request(message: impl Into<String>) -> Response {
    let payload = serde_json::json!({ "error": message.into() });
    let mut response = (StatusCode::BAD_REQUEST, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

pub(crate) fn ok_json(payload: serde_json::Value) -> Response {
    let mut response = (StatusCode::OK, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

fn repo_ctx_script(ctx: &crate::command::github_preview_server::RepoUrlContext) -> String {
    // JSON-encode minimally — owner/repo/branch are simple strings whose escaping
    // we control with html_escape (they originate from git config and refs).
    format!(
        r#"<script>window.__GARGO_REPO_CTX__ = {{ owner: "{}", repo: "{}", branch: "{}" }};</script>"#,
        html_escape(&ctx.owner),
        html_escape(&ctx.repo),
        html_escape(&ctx.branch),
    )
}

/// Serve the compare-branches HTML page.
pub(crate) async fn handle_compare_html_request(
    State(state): State<Arc<DiffServerState>>,
) -> impl IntoResponse {
    use crate::command::github_preview_server as gh;
    let root_path = state.project_root.display().to_string();
    let ctx = gh::resolve_repo_url_context(&state.project_root).await;
    let repo_url = gh::github_repo_url(&state.project_root).await;
    let rail = crate::command::app_shell::app_rail_html(&ctx, repo_url.as_deref(), "branches");
    let ctx_script = repo_ctx_script(&ctx);
    Html(
        COMPARE_HTML_TEMPLATE
            .replace("{{ROOT_PATH}}", &html_escape(&root_path))
            .replace("{{APP_RAIL}}", &rail)
            .replace("{{REPO_CTX_SCRIPT}}", &ctx_script)
            .replace("{{SHARED_CSS}}", crate::command::server_shared::SHARED_CSS)
            .replace(
                "{{SHORTCUTS_JS}}",
                crate::command::server_shared::SHORTCUTS_JS,
            )
            .replace("{{DIFF_STYLES}}", render_diff_styles()),
    )
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
    let repo_root = &state.project_root;
    let raw = match git_output_in_repo(
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname)|%(refname:short)|%(HEAD)",
            "refs/heads/",
            "refs/remotes/",
        ],
    )
    .await
    {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    let mut branches: Vec<String> = Vec::new();
    let mut remotes: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(3, '|');
        let full = it.next().unwrap_or("").trim();
        let short = it.next().unwrap_or("").trim();
        let head = it.next().unwrap_or("").trim();
        if short.is_empty() {
            continue;
        }
        // Skip `refs/remotes/origin/HEAD` and friends — they shadow a real
        // remote branch and confuse the user when listed alongside it.
        if short.ends_with("/HEAD") {
            continue;
        }
        if head == "*" {
            current = Some(short.to_string());
        }
        if full.starts_with("refs/remotes/") {
            remotes.push(short.to_string());
        }
        branches.push(short.to_string());
    }

    let default = detect_default_branch(repo_root, &branches).await;

    ok_json(serde_json::json!({
        "current": current,
        "default": default,
        "branches": branches,
        "remotes": remotes,
    }))
}

/// Best-effort detection of the repository's default branch.
///
/// Tries `origin/HEAD` first (set by `git clone` or `git remote set-head`), then
/// falls back to the well-known `main` / `master` names if either exists
/// locally. Returns `None` only for repos without remote and without either
/// conventional name.
async fn detect_default_branch(repo_root: &Path, known: &[String]) -> Option<String> {
    if let Ok(output) = git_output_in_repo(
        repo_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    {
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

    let range = format!("{}...{}", base, compare);
    let diff = match git_output_in_repo(&state.project_root, &["diff", &range]).await {
        Ok(output) => output,
        Err(error) => return bad_request(error),
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

/// Run `git diff base...compare -- <path>` and return the parsed [`DiffFile`].
async fn load_compare_diff_file(
    repo_root: &Path,
    base: &str,
    compare: &str,
    path: &str,
) -> Result<Option<DiffFile>, String> {
    let range = format!("{}...{}", base, compare);
    let diff = git_output_in_repo(repo_root, &["diff", &range, "--", path]).await?;
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

    let file = match load_compare_diff_file(&state.project_root, &base, &compare, &path).await {
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
            "status": "modified",
            "additions": 0,
            "deletions": 0,
            "binary": false,
            "html": empty_diff_html(),
        })),
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
fn parse_compare_branches(params: &HashMap<String, String>) -> Result<(String, String), Response> {
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

/// Register diff server commands in the command palette
pub fn register(registry: &mut CommandRegistry) {
    registry.register(CommandEntry {
        id: "server.start_diff".into(),
        label: "Start Diff Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.start_diff".to_string(),
                },
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "server.stop_diff".into(),
        label: "Stop Diff Server".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.stop_diff".to_string(),
                },
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "server.open_compare".into(),
        label: "Open Compare Branches".into(),
        category: Some("Server".into()),
        action: Box::new(|_ctx: &CommandContext| {
            CommandEffect::Action(Action::App(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "server.open_compare".to_string(),
                },
            )))
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_highlighted_emits_syntax_classes_for_rust_diff() {
        let diff = "\
diff --git a/lib.rs b/lib.rs
index 1..2 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,3 +1,3 @@
 fn keep() {}
-fn old() { let x = 1; }
+fn renamed() { let y = 2; }
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        // Diff line wrappers still present.
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-remove">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-context">"#));
        // Tree-sitter Rust should classify "fn" and "let" as keywords on
        // both the added and removed lines.
        assert!(
            html.contains("gr-hl-keyword"),
            "expected gr-hl-keyword class, got:\n{}",
            html
        );
    }

    #[test]
    fn render_highlighted_falls_back_for_unknown_extension() {
        let diff = "\
diff --git a/notes.unknownext b/notes.unknownext
index 1..2 100644
--- a/notes.unknownext
+++ b/notes.unknownext
@@ -1,1 +1,1 @@
-old line
+new line
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        assert!(!html.contains("gr-hl-"), "should not highlight: {}", html);
        // Plain diff body still renders normally.
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
    }

    #[test]
    fn render_highlighted_falls_back_for_binary() {
        let diff = "\
diff --git a/img.rs b/img.rs
index abc..def
Binary files a/img.rs and b/img.rs differ
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        assert!(html.contains("(binary file changes not shown)"));
        assert!(!html.contains("gr-hl-"));
    }
}

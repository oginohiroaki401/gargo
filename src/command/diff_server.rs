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
    routing::get,
};
use tower_http::cors::CorsLayer;

use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::diff_render::{
    DiffFile, DiffHighlights, FileStatus, LineKind, parse_unified_diff, render_diff_styles,
    render_file_body_html, render_file_body_html_with_highlights,
};
use crate::input::action::{Action, AppAction, IntegrationAction};
use crate::syntax::highlight::highlight_text;
use crate::syntax::language::{LanguageDef, LanguageRegistry};

/// Commands that can be sent to the diff server
#[derive(Debug, Clone)]
pub enum DiffServerCommand {
    Start { project_root: PathBuf },
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
                Ok(DiffServerCommand::Start { project_root }) => {
                    self.handle_start_server(project_root);
                }
                Ok(DiffServerCommand::Stop) => self.handle_stop_server(),
                Err(_) => break, // Main thread exited
            }
        }
    }

    fn handle_start_server(&mut self, project_root: PathBuf) {
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

        let server_state = Arc::new(DiffServerState {
            project_root: std::fs::canonicalize(&project_root).unwrap_or(project_root),
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

#[derive(Debug)]
struct DiffServerState {
    project_root: PathBuf,
}

/// HTML template with diff2html integration
const DIFF_HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Git Diff</title>
    <style>
        body {
            margin: 0;
            padding: 20px;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
            background-color: #f5f5f5;
            color: #333;
        }
        .header {
            background: white;
            padding: 20px;
            margin-bottom: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
            display: flex;
            flex-direction: column;
            gap: 12px;
        }
        .context-label {
            margin: 0;
            font-size: 13px;
            font-weight: 600;
            color: #555;
            text-transform: uppercase;
            letter-spacing: 0.04em;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #4b5563;
        }
        .context-key { font-weight: 600; color: #1f2937; }
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
        .layout { display: flex; gap: 20px; align-items: flex-start; }
        .sidebar {
            width: 260px;
            flex-shrink: 0;
            position: sticky;
            top: 20px;
            max-height: calc(100vh - 40px);
            overflow-y: auto;
        }
        .sidebar .files-section { margin-bottom: 0; }
        .content { flex: 1 1 auto; min-width: 0; }
        @media (max-width: 900px) {
            .layout { flex-direction: column; }
            .sidebar { position: static; width: auto; max-height: none; }
        }
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
        #go-top-btn {
            position: fixed;
            right: 20px;
            bottom: 20px;
            z-index: 1000;
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
<body>
    <div class="header">
        <div class="context-label">Git diff</div>
        <div class="context-row"><span class="context-key">Root</span><code id="root-path">{{ROOT_PATH}}</code></div>
        <div class="controls">
            <label>
                <input type="checkbox" id="show-untracked">
                Show untracked files
            </label>
            <button id="refresh-btn" type="button">Refresh</button>
        </div>
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
    <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>

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
        const AUTO_REFRESH_INTERVAL_MS = 2000;
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.diff.collapsed.v3:${STORAGE_ROOT}`;
        const VIEWED_FILES_STORAGE_KEY = `gargo.diff.viewed.v2:${STORAGE_ROOT}`;
        const SIDEBAR_COLLAPSED_KEY = `gargo.diff.sidebar.collapsed.v1:${STORAGE_ROOT}`;

        showUntrackedToggle.checked = parseBoolParam(urlParams.get("show_untracked"), true);

        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let viewedFileIds = loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY);
        let sidebarCollapsedDirs = loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY);
        const bodyCache = new Map();
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
        const persistViewedFileIds = () => persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY, viewedFileIds);
        const persistSidebarCollapsedDirs = () => persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY, sidebarCollapsedDirs);

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

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) goTopButton.classList.add("visible");
            else goTopButton.classList.remove("visible");
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
            const name = document.createElement("span");
            name.className = "gr-file-name";
            name.textContent = meta.path;
            name.title = (meta.old_path && meta.old_path !== meta.path)
                ? `${meta.old_path} → ${meta.path}` : meta.path;
            nameWrapper.appendChild(name);
            const tag = document.createElement("span");
            const status = meta.status || "modified";
            tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
            tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            nameWrapper.appendChild(tag);
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
            label.title = "Mark this file as viewed (saved per browser)";
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
            } else {
                collapsedFileIds.delete(fileId);
                ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
            }
            persistCollapsedFileIds();
        }

        function setFileViewed(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-viewed", isViewed);
            wrapper.classList.toggle("gr-file-viewed", isViewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== isViewed) checkbox.checked = isViewed;
            if (isViewed) {
                viewedFileIds.add(fileId);
                wrapper.classList.add("diff-file-collapsed");
                wrapper.classList.add("gr-file-collapsed");
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, true);
                const body = wrapper.querySelector(".gr-file-body");
                if (body) { body.innerHTML = ""; delete body.dataset.loaded; }
            } else {
                viewedFileIds.delete(fileId);
                if (!collapsedFileIds.has(fileId)) {
                    // Default-expand on un-viewed
                    wrapper.classList.remove("diff-file-collapsed");
                    wrapper.classList.remove("gr-file-collapsed");
                    const button = wrapper.querySelector(".diff-toggle-btn");
                    if (button) renderDiffToggleButtonLabel(button, false);
                    ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
                }
            }
            persistViewedFileIds();
            persistCollapsedFileIds();
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
            const root = buildFileTree(allEntries);
            const ul = document.createElement("ul");
            ul.className = "file-tree";
            appendTreeNode(ul, root);
            filesListContainer.appendChild(ul);
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
            for (const id of Array.from(collapsedFileIds)) {
                if (!presentIds.has(id)) collapsedFileIds.delete(id);
            }
            persistCollapsedFileIds();

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
                const isViewed = viewedFileIds.has(fileId);
                const isCollapsed = isViewed || collapsedFileIds.has(fileId);
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
        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });
        window.setInterval(() => {
            loadStatus({ showLoading: false }).catch((e) => showError(e.message));
        }, AUTO_REFRESH_INTERVAL_MS);

        updateGoTopButtonVisibility();
        loadStatus().catch((e) => showError(e.message));
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
        body {
            margin: 0;
            padding: 20px;
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
            background-color: #f5f5f5;
            color: #333;
        }
        .header {
            background: white;
            padding: 20px;
            margin-bottom: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0, 0, 0, 0.1);
            display: flex;
            flex-direction: column;
            gap: 12px;
        }
        .context-label {
            margin: 0;
            font-size: 13px;
            font-weight: 600;
            color: #555;
            text-transform: uppercase;
            letter-spacing: 0.04em;
        }
        .context-row {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
            font-size: 14px;
            color: #4b5563;
        }
        .context-key { font-weight: 600; color: #1f2937; }
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
        .layout { display: flex; gap: 20px; align-items: flex-start; }
        .sidebar {
            width: 260px;
            flex-shrink: 0;
            position: sticky;
            top: 20px;
            max-height: calc(100vh - 40px);
            overflow-y: auto;
        }
        .sidebar .files-section { margin-bottom: 0; }
        .content { flex: 1 1 auto; min-width: 0; }
        @media (max-width: 900px) {
            .layout { flex-direction: column; }
            .sidebar { position: static; width: auto; max-height: none; }
        }
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
        #go-top-btn {
            position: fixed;
            right: 20px;
            bottom: 20px;
            z-index: 1000;
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
<body>
    <div class="header">
        <div class="context-label">Compare branches</div>
        <div class="context-row"><span class="context-key">Root</span><code id="root-path">{{ROOT_PATH}}</code></div>
        <div class="controls">
            <label>
                Base
                <select id="base-select"><option value="">(loading...)</option></select>
            </label>
            <span class="range-arrow">...</span>
            <label>
                Compare
                <select id="compare-select"><option value="">(loading...)</option></select>
            </label>
            <button id="swap-btn" type="button" title="Swap base and compare">Swap</button>
            <button id="refresh-btn" type="button">Refresh</button>
        </div>
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
    <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>

    <script>
        const urlParams = new URLSearchParams(window.location.search);

        const baseSelect = document.getElementById("base-select");
        const compareSelect = document.getElementById("compare-select");
        const swapButton = document.getElementById("swap-btn");
        const refreshButton = document.getElementById("refresh-btn");
        const errorBanner = document.getElementById("error-banner");
        const rootPathCode = document.getElementById("root-path");
        const filesHeading = document.getElementById("files-heading");
        const filesListContainer = document.getElementById("files-list");
        const filesMain = document.getElementById("files-main");
        const goTopButton = document.getElementById("go-top-btn");
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.compare.collapsed.v3:${STORAGE_ROOT}`;
        const VIEWED_FILES_STORAGE_KEY = `gargo.compare.viewed.v2:${STORAGE_ROOT}`;
        const SIDEBAR_COLLAPSED_KEY = `gargo.compare.sidebar.collapsed.v1:${STORAGE_ROOT}`;

        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let viewedFileIds = loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY);
        let sidebarCollapsedDirs = loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY);
        const bodyCache = new Map();
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
        const persistViewedFileIds = () => persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY, viewedFileIds);
        const persistSidebarCollapsedDirs = () => persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY, sidebarCollapsedDirs);

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

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) goTopButton.classList.add("visible");
            else goTopButton.classList.remove("visible");
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
            const name = document.createElement("span");
            name.className = "gr-file-name";
            name.textContent = meta.path;
            name.title = (meta.old_path && meta.old_path !== meta.path)
                ? `${meta.old_path} → ${meta.path}` : meta.path;
            nameWrapper.appendChild(name);
            const tag = document.createElement("span");
            const status = meta.status || "modified";
            tag.className = `gr-status-tag ${STATUS_CSS[status] || STATUS_CSS.modified}`;
            tag.textContent = STATUS_LABELS[status] || status.toUpperCase();
            nameWrapper.appendChild(tag);
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
            label.title = "Mark this file as viewed (saved per browser)";
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
            } else {
                collapsedFileIds.delete(fileId);
                ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
            }
            persistCollapsedFileIds();
        }

        function setFileViewed(wrapper, isViewed) {
            const fileId = wrapper.dataset.diffFileId;
            wrapper.classList.toggle("diff-file-viewed", isViewed);
            wrapper.classList.toggle("gr-file-viewed", isViewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== isViewed) checkbox.checked = isViewed;
            if (isViewed) {
                viewedFileIds.add(fileId);
                wrapper.classList.add("diff-file-collapsed");
                wrapper.classList.add("gr-file-collapsed");
                const button = wrapper.querySelector(".diff-toggle-btn");
                if (button) renderDiffToggleButtonLabel(button, true);
                const body = wrapper.querySelector(".gr-file-body");
                if (body) { body.innerHTML = ""; delete body.dataset.loaded; }
            } else {
                viewedFileIds.delete(fileId);
                if (!collapsedFileIds.has(fileId)) {
                    wrapper.classList.remove("diff-file-collapsed");
                    wrapper.classList.remove("gr-file-collapsed");
                    const button = wrapper.querySelector(".diff-toggle-btn");
                    if (button) renderDiffToggleButtonLabel(button, false);
                    ensureBodyLoaded(wrapper).catch((e) => showError(e.message));
                }
            }
            persistViewedFileIds();
            persistCollapsedFileIds();
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
            for (const id of Array.from(collapsedFileIds)) {
                if (!presentIds.has(id)) collapsedFileIds.delete(id);
            }
            persistCollapsedFileIds();

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
                const isViewed = viewedFileIds.has(fileId);
                const isCollapsed = isViewed || collapsedFileIds.has(fileId);
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

        const populateSelect = (select, branches, preferred) => {
            select.innerHTML = "";
            if (branches.length === 0) {
                const opt = document.createElement("option");
                opt.value = "";
                opt.textContent = "(no branches)";
                select.appendChild(opt);
                return;
            }
            for (const name of branches) {
                const opt = document.createElement("option");
                opt.value = name;
                opt.textContent = name;
                select.appendChild(opt);
            }
            if (preferred && branches.includes(preferred)) {
                select.value = preferred;
            } else {
                select.value = branches[0];
            }
        };

        async function loadBranches() {
            try {
                const response = await fetch("/api/branches", { cache: "no-store" });
                const data = await response.json();
                if (data.error) throw new Error(data.error);
                const branches = Array.isArray(data.branches) ? data.branches : [];
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
                populateSelect(baseSelect, branches, baseParam || baseFallback);
                populateSelect(compareSelect, branches, compareParam || compareFallback);
            } catch (e) {
                showError(e.message);
                baseSelect.innerHTML = '<option value="">(error)</option>';
                compareSelect.innerHTML = '<option value="">(error)</option>';
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
        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });

        (async () => {
            await loadBranches();
            persistControls();
            await loadCompare().catch((e) => showError(e.message));
        })();

        updateGoTopButtonVisibility();
    </script>
</body>
</html>"#;

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<DiffServerState>,
) {
    let app = Router::new()
        .route("/diff", get(handle_html_request))
        .route("/compare", get(handle_compare_html_request))
        .route("/api/status", get(handle_api_status_request))
        .route("/api/status/file", get(handle_api_status_file_request))
        .route("/api/branches", get(handle_api_branches_request))
        .route("/api/compare", get(handle_api_compare_request))
        .route("/api/compare/file", get(handle_api_compare_file_request))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

/// Serve the HTML page with diff2html
async fn handle_html_request(State(state): State<Arc<DiffServerState>>) -> impl IntoResponse {
    let root_path = state.project_root.display().to_string();
    Html(
        DIFF_HTML_TEMPLATE
            .replace("{{ROOT_PATH}}", &html_escape(&root_path))
            .replace("{{DIFF_STYLES}}", render_diff_styles()),
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

async fn git_output_in_repo(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["-c", "core.quotepath=off"]);
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
async fn handle_api_status_request(
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

    let unstaged_files: Vec<serde_json::Value> = parse_unified_diff(&unstaged_raw)
        .iter()
        .map(file_metadata_json)
        .collect();
    let staged_files: Vec<serde_json::Value> = parse_unified_diff(&staged_raw)
        .iter()
        .map(file_metadata_json)
        .collect();

    let untracked_files: Vec<serde_json::Value> = if show_untracked {
        let raw = match git_output_in_repo(
            repo_root,
            &["ls-files", "--others", "--exclude-standard"],
        )
        .await
        {
            Ok(output) => output,
            Err(error) => return bad_request(error),
        };
        raw.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|path| {
                serde_json::json!({
                    "path": path,
                    "old_path": serde_json::Value::Null,
                    "status": "untracked",
                    "binary": false,
                    "additions": 0,
                    "deletions": 0,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    ok_json(serde_json::json!({
        "unstaged": unstaged_files,
        "staged": staged_files,
        "untracked": untracked_files,
    }))
}

async fn handle_api_status_file_request(
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
    let repo_root = &state.project_root;

    let diff_text = match section {
        "staged" => match git_output_in_repo(repo_root, &["diff", "--cached", "--", &path]).await {
            Ok(o) => o,
            Err(e) => return bad_request(e),
        },
        "unstaged" => match git_output_in_repo(repo_root, &["diff", "--", &path]).await {
            Ok(o) => o,
            Err(e) => return bad_request(e),
        },
        "untracked" => {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["-c", "core.quotepath=off"]);
            cmd.args(["diff", "--no-index", "--", "/dev/null", &path]);
            cmd.current_dir(repo_root);
            match git_output_from_command(
                cmd,
                &[1],
                &format!("git diff --no-index -- /dev/null {}", path),
            )
            .await
            {
                Ok(o) => o,
                Err(e) => return bad_request(e),
            }
        }
        _ => unreachable!(),
    };

    let mut files = parse_unified_diff(&diff_text);
    if section == "untracked" {
        for f in &mut files {
            f.status = FileStatus::Untracked;
        }
    }
    match files.into_iter().next() {
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

fn file_metadata_json(file: &DiffFile) -> serde_json::Value {
    serde_json::json!({
        "path": file.path,
        "old_path": file.old_path,
        "status": file.status.as_str(),
        "binary": file.binary,
        "additions": file.additions,
        "deletions": file.deletions,
    })
}

fn empty_diff_html() -> String {
    r#"<div class="gr-diff-body"><div class="gr-line gr-line-hunk"><span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span><span class="gr-text">(no content changes)</span></div></div>"#
        .to_string()
}

/// Render `file` to HTML, applying tree-sitter syntax highlighting when
/// the file's extension maps to a known language. Falls back to plain
/// rendering for unknown languages, binary files, or rename-only entries.
fn render_highlighted(file: &DiffFile) -> String {
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
                    entry
                        .spans
                        .push((start, end, s.capture_name.clone()));
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
                    entry
                        .spans
                        .push((start, end, s.capture_name.clone()));
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
fn parse_diff_path(value: &str) -> Option<String> {
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

fn bad_request(message: impl Into<String>) -> Response {
    let payload = serde_json::json!({ "error": message.into() });
    let mut response = (StatusCode::BAD_REQUEST, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

fn ok_json(payload: serde_json::Value) -> Response {
    let mut response = (StatusCode::OK, Json(payload)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, must-revalidate"),
    );
    response
}

/// Serve the compare-branches HTML page.
async fn handle_compare_html_request(
    State(state): State<Arc<DiffServerState>>,
) -> impl IntoResponse {
    let root_path = state.project_root.display().to_string();
    Html(
        COMPARE_HTML_TEMPLATE
            .replace("{{ROOT_PATH}}", &html_escape(&root_path))
            .replace("{{DIFF_STYLES}}", render_diff_styles()),
    )
}

/// List the local branches in the repo and the current HEAD branch.
async fn handle_api_branches_request(
    State(state): State<Arc<DiffServerState>>,
) -> Response {
    let repo_root = &state.project_root;
    let raw = match git_output_in_repo(
        repo_root,
        &["branch", "--format=%(refname:short)|%(HEAD)"],
    )
    .await
    {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    let mut branches: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (name, head) = match line.rsplit_once('|') {
            Some((n, h)) => (n.trim(), h.trim()),
            None => (line.trim(), ""),
        };
        if name.is_empty() {
            continue;
        }
        if head == "*" {
            current = Some(name.to_string());
        }
        branches.push(name.to_string());
    }

    let default = detect_default_branch(repo_root, &branches).await;

    ok_json(serde_json::json!({
        "current": current,
        "default": default,
        "branches": branches,
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
async fn handle_api_compare_request(
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

    let files: Vec<serde_json::Value> = parse_unified_diff(&diff)
        .iter()
        .map(file_metadata_json)
        .collect();

    ok_json(serde_json::json!({
        "base": base,
        "compare": compare,
        "files": files,
    }))
}

async fn handle_api_compare_file_request(
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

    let range = format!("{}...{}", base, compare);
    let diff = match git_output_in_repo(&state.project_root, &["diff", &range, "--", &path]).await
    {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    match parse_unified_diff(&diff).into_iter().next() {
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

#[allow(clippy::result_large_err)]
fn parse_compare_branches(
    params: &HashMap<String, String>,
) -> Result<(String, String), Response> {
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

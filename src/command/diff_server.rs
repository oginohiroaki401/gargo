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
use crate::input::action::{Action, AppAction, IntegrationAction};

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
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/diff2html/bundles/css/diff2html.min.css" />
    <script src="https://cdn.jsdelivr.net/npm/diff2html/bundles/js/diff2html-ui.min.js"></script>
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
        .context-key {
            font-weight: 600;
            color: #1f2937;
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
        .controls {
            display: flex;
            gap: 12px;
            align-items: center;
            flex-wrap: wrap;
        }
        .controls label {
            font-size: 14px;
            display: flex;
            align-items: center;
            gap: 8px;
        }
        .controls select, .controls button {
            padding: 6px 10px;
            border: 1px solid #ccc;
            border-radius: 6px;
            background: white;
            font-size: 14px;
        }
        .controls button {
            cursor: pointer;
        }
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
        .section h2 {
            margin: 0 0 12px 0;
            font-size: 18px;
        }
        .loading, .empty {
            padding: 20px;
            color: #666;
        }
        .file-list {
            margin: 0;
            padding-left: 20px;
        }
        .file-list li {
            margin: 4px 0;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
        }
        .file-list a {
            color: #0a58ca;
            text-decoration: none;
        }
        .file-list a:hover {
            text-decoration: underline;
        }
        .file-status {
            display: inline-block;
            width: 1.2em;
            text-align: center;
            font-weight: 700;
            margin-right: 0.3em;
        }
        .file-status.staged    { color: #2da44e; }
        .file-status.changed   { color: #d29922; }
        .file-status.untracked { color: #8b949e; }
        .d2h-file-header {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
        }
        .diff-toggle-btn {
            order: -1;
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
        .diff-viewed-label:hover {
            background: #eef2f7;
        }
        .diff-viewed-label input {
            margin: 0;
            cursor: pointer;
        }
        .diff-file-viewed > .d2h-file-header {
            background: #eef2f7;
            opacity: 0.8;
        }
        .diff-file-collapsed .d2h-file-diff {
            display: none;
        }
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
        #go-top-btn.visible {
            opacity: 1;
            pointer-events: auto;
            transform: translateY(0);
        }
        #go-top-btn:hover {
            background: #eef2f7;
        }
    </style>
</head>
<body>
    <div class="header">
        <div class="context-label">Git diff</div>
        <div class="context-row"><span class="context-key">Root</span><code id="root-path">{{ROOT_PATH}}</code></div>
        <div class="controls">
            <label>
                View mode
                <select id="view-mode">
                    <option value="unified">Unified</option>
                    <option value="side-by-side">Side-by-side</option>
                </select>
            </label>
            <label>
                <input type="checkbox" id="show-untracked">
                Show untracked files
            </label>
            <button id="expand-all-btn" type="button">Expand all</button>
            <button id="collapse-all-btn" type="button">Collapse all</button>
            <button id="refresh-btn" type="button">Refresh</button>
        </div>
    </div>

    <div id="error-banner"></div>

    <section class="section">
        <h2 id="files-heading">Files</h2>
        <div id="files-list">
            <div class="loading">Loading files...</div>
        </div>
    </section>

    <section class="section">
        <h2>Changed Diff</h2>
        <div id="changed-diff" class="diff-container">
            <div class="loading">Loading changed diff...</div>
        </div>
    </section>

    <section class="section">
        <h2>Staged Diff</h2>
        <div id="staged-diff" class="diff-container">
            <div class="loading">Loading staged diff...</div>
        </div>
    </section>

    <section class="section" id="untracked-diff-section">
        <h2>Untracked Diff</h2>
        <div id="untracked-diff" class="diff-container">
            <div class="loading">Loading untracked diff...</div>
        </div>
    </section>
    <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>

    <script>
        const urlParams = new URLSearchParams(window.location.search);
        const normalizeViewMode = (value) => value === "side-by-side" ? "side-by-side" : "unified";
        const parseBoolParam = (value, defaultValue) => {
            if (value === null) {
                return defaultValue;
            }
            return value === "true";
        };

        const viewModeSelect = document.getElementById("view-mode");
        const showUntrackedToggle = document.getElementById("show-untracked");
        const expandAllButton = document.getElementById("expand-all-btn");
        const collapseAllButton = document.getElementById("collapse-all-btn");
        const refreshButton = document.getElementById("refresh-btn");
        const errorBanner = document.getElementById("error-banner");
        const rootPathCode = document.getElementById("root-path");
        const filesHeading = document.getElementById("files-heading");
        const filesListContainer = document.getElementById("files-list");
        const changedDiffContainer = document.getElementById("changed-diff");
        const stagedDiffContainer = document.getElementById("staged-diff");
        const untrackedDiffContainer = document.getElementById("untracked-diff");
        const untrackedDiffSection = document.getElementById("untracked-diff-section");
        const goTopButton = document.getElementById("go-top-btn");
        const AUTO_REFRESH_INTERVAL_MS = 2000;
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.diff.collapsed.v2:${STORAGE_ROOT}`;
        const VIEWED_FILES_STORAGE_KEY = `gargo.diff.viewed.v1:${STORAGE_ROOT}`;

        let latestStatus = null;
        let isLoading = false;
        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let viewedFileIds = loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY);
        viewModeSelect.value = normalizeViewMode(urlParams.get("view"));
        showUntrackedToggle.checked = parseBoolParam(urlParams.get("show_untracked"), true);

        function loadIdSet(storage, key) {
            try {
                const raw = storage.getItem(key);
                if (!raw) {
                    return new Set();
                }
                const parsed = JSON.parse(raw);
                if (!Array.isArray(parsed)) {
                    return new Set();
                }
                const ids = parsed.filter((value) => typeof value === "string" && value.length > 0);
                return new Set(ids);
            } catch (_error) {
                return new Set();
            }
        }

        const persistIdSet = (storage, key, set) => {
            try {
                storage.setItem(key, JSON.stringify(Array.from(set)));
            } catch (_error) {
                // Ignore storage failures and keep UI responsive.
            }
        };

        const persistCollapsedFileIds = () => {
            persistIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY, collapsedFileIds);
        };

        const persistViewedFileIds = () => {
            persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY, viewedFileIds);
        };

        const showError = (message) => {
            errorBanner.textContent = `Error: ${message}`;
            errorBanner.style.display = "block";
        };

        const clearError = () => {
            errorBanner.textContent = "";
            errorBanner.style.display = "none";
        };

        const setLoading = (container, message) => {
            container.innerHTML = `<div class="loading">${message}</div>`;
        };

        const setEmpty = (container, message) => {
            container.innerHTML = `<div class="empty">${message}</div>`;
        };

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) {
                goTopButton.classList.add("visible");
            } else {
                goTopButton.classList.remove("visible");
            }
        };

        const renderDiffToggleButtonLabel = (button, collapsed) => {
            button.textContent = collapsed ? "▸" : "▾";
            button.setAttribute("aria-expanded", collapsed ? "false" : "true");
            button.setAttribute("aria-label", collapsed ? "Show diff" : "Hide diff");
            button.setAttribute("title", collapsed ? "Show diff" : "Hide diff");
        };

        const setDiffFileCollapsed = (wrapper, fileId, collapsed) => {
            if (collapsed) {
                collapsedFileIds.add(fileId);
            } else {
                collapsedFileIds.delete(fileId);
            }
            wrapper.classList.toggle("diff-file-collapsed", collapsed);
            const button = wrapper.querySelector(".diff-toggle-btn");
            if (button) {
                renderDiffToggleButtonLabel(button, collapsed);
            }
        };

        const setDiffFileViewed = (wrapper, fileId, viewed) => {
            if (viewed) {
                viewedFileIds.add(fileId);
            } else {
                viewedFileIds.delete(fileId);
            }
            wrapper.classList.toggle("diff-file-viewed", viewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== viewed) {
                checkbox.checked = viewed;
            }
            setDiffFileCollapsed(wrapper, fileId, viewed);
        };

        const updateBulkToggleAvailability = () => {
            const wrappers = document.querySelectorAll(".diff-container .d2h-file-wrapper[data-diff-file-id]");
            const disabled = wrappers.length === 0;
            expandAllButton.disabled = disabled;
            collapseAllButton.disabled = disabled;
        };

        const setAllRenderedDiffFilesCollapsed = (collapsed) => {
            const wrappers = document.querySelectorAll(".diff-container .d2h-file-wrapper[data-diff-file-id]");
            wrappers.forEach((wrapper) => {
                const fileId = wrapper.dataset.diffFileId;
                if (!fileId) {
                    return;
                }
                setDiffFileCollapsed(wrapper, fileId, collapsed);
            });
            persistCollapsedFileIds();
            updateBulkToggleAvailability();
        };

        const pruneCollapsedFileIds = (knownFileIds) => {
            const next = new Set();
            collapsedFileIds.forEach((fileId) => {
                const isUntrackedEntry = fileId.startsWith("untracked-diff:");
                if (knownFileIds.has(fileId) || (!showUntrackedToggle.checked && isUntrackedEntry)) {
                    next.add(fileId);
                }
            });
            if (next.size !== collapsedFileIds.size) {
                collapsedFileIds = next;
                persistCollapsedFileIds();
            }
        };

        const updateUntrackedVisibility = () => {
            untrackedDiffSection.style.display = showUntrackedToggle.checked ? "" : "none";
        };

        const persistControls = () => {
            const params = new URLSearchParams();
            params.set("view", normalizeViewMode(viewModeSelect.value));
            params.set("show_untracked", showUntrackedToggle.checked ? "true" : "false");
            history.replaceState(null, "", `/diff?${params.toString()}`);
        };

        const parseDiffFiles = (diff) => {
            if (!diff || diff.trim() === "") {
                return [];
            }

            const files = [];
            for (const line of diff.split("\n")) {
                if (!line.startsWith("diff --git ")) {
                    continue;
                }
                const match = line.match(/^diff --git a\/(.+) b\/(.+)$/);
                if (!match) {
                    continue;
                }
                files.push(match[2]);
            }
            return files;
        };

        const renderUnifiedFileList = (status) => {
            const changedFiles = parseDiffFiles(status.unstaged_diff);
            const stagedFiles = parseDiffFiles(status.staged_diff);
            const showUntracked = showUntrackedToggle.checked;
            const untrackedFiles = showUntracked ? (status.untracked_files || []) : [];

            const entries = [];
            for (let i = 0; i < stagedFiles.length; i += 1) {
                entries.push({ name: stagedFiles[i], statusChar: "S", cssClass: "staged", anchor: `staged-diff-file-${i}` });
            }
            for (let i = 0; i < changedFiles.length; i += 1) {
                entries.push({ name: changedFiles[i], statusChar: "M", cssClass: "changed", anchor: `changed-diff-file-${i}` });
            }
            for (let i = 0; i < untrackedFiles.length; i += 1) {
                entries.push({ name: untrackedFiles[i], statusChar: "?", cssClass: "untracked", anchor: `untracked-diff-file-${i}` });
            }
            entries.sort((a, b) => a.name.localeCompare(b.name));

            const parts = [];
            if (stagedFiles.length > 0) parts.push(`${stagedFiles.length} staged`);
            if (changedFiles.length > 0) parts.push(`${changedFiles.length} changed`);
            if (untrackedFiles.length > 0) parts.push(`${untrackedFiles.length} untracked`);
            filesHeading.textContent = parts.length > 0 ? `Files (${parts.join(", ")})` : "Files";

            if (entries.length === 0) {
                setEmpty(filesListContainer, "No files changed");
                return;
            }

            filesListContainer.innerHTML = "";
            const list = document.createElement("ul");
            list.className = "file-list";
            for (const entry of entries) {
                const item = document.createElement("li");
                const badge = document.createElement("span");
                badge.className = `file-status ${entry.cssClass}`;
                badge.textContent = entry.statusChar;
                const link = document.createElement("a");
                link.href = `#${entry.anchor}`;
                link.textContent = entry.name;
                item.appendChild(badge);
                item.appendChild(link);
                list.appendChild(item);
            }
            filesListContainer.appendChild(list);
        };

        const decorateRenderedDiffFiles = (container, anchorPrefix, diffFiles, knownFileIds) => {
            const wrappers = container.querySelectorAll(".d2h-file-wrapper");
            wrappers.forEach((wrapper, index) => {
                wrapper.id = `${anchorPrefix}-file-${index}`;
                const filePath = diffFiles[index] || `file-${index}`;
                const fileId = `${anchorPrefix}:${filePath}`;
                wrapper.dataset.diffFileId = fileId;
                knownFileIds.add(fileId);

                const header = wrapper.querySelector(".d2h-file-header");
                if (header && !header.querySelector(".diff-toggle-btn")) {
                    const toggleButton = document.createElement("button");
                    toggleButton.type = "button";
                    toggleButton.className = "diff-toggle-btn";
                    toggleButton.addEventListener("click", () => {
                        const shouldCollapse = !wrapper.classList.contains("diff-file-collapsed");
                        setDiffFileCollapsed(wrapper, fileId, shouldCollapse);
                        persistCollapsedFileIds();
                    });
                    header.insertBefore(toggleButton, header.firstChild);
                }

                if (header && !header.querySelector(".diff-viewed-label")) {
                    const label = document.createElement("label");
                    label.className = "diff-viewed-label";
                    label.title = "Mark this file as viewed (saved per browser)";
                    const checkbox = document.createElement("input");
                    checkbox.type = "checkbox";
                    checkbox.addEventListener("click", (event) => {
                        event.stopPropagation();
                    });
                    checkbox.addEventListener("change", () => {
                        setDiffFileViewed(wrapper, fileId, checkbox.checked);
                        persistViewedFileIds();
                        persistCollapsedFileIds();
                    });
                    const text = document.createElement("span");
                    text.textContent = "Viewed";
                    label.appendChild(checkbox);
                    label.appendChild(text);
                    header.appendChild(label);
                }

                const isViewed = viewedFileIds.has(fileId);
                const isCollapsed = isViewed || collapsedFileIds.has(fileId);
                wrapper.classList.toggle("diff-file-viewed", isViewed);
                wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
                if (isViewed) {
                    collapsedFileIds.add(fileId);
                }
                const toggleButton = wrapper.querySelector(".diff-toggle-btn");
                if (toggleButton) {
                    renderDiffToggleButtonLabel(toggleButton, isCollapsed);
                }
                const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
                if (checkbox) {
                    checkbox.checked = isViewed;
                }
            });
        };

        const renderDiffSection = (container, diff, emptyMessage, anchorPrefix, knownFileIds) => {
            const viewMode = normalizeViewMode(viewModeSelect.value);
            if (!diff || diff.trim() === "") {
                setEmpty(container, emptyMessage);
                return;
            }

            container.innerHTML = "";
            const configuration = {
                drawFileList: false,
                fileListToggle: false,
                fileListStartVisible: false,
                fileContentToggle: false,
                matching: "lines",
                outputFormat: viewMode === "side-by-side" ? "side-by-side" : "line-by-line",
                synchronisedScroll: true,
                highlight: true,
                renderNothingWhenEmpty: false,
            };
            const diff2htmlUi = new Diff2HtmlUI(container, diff, configuration);
            diff2htmlUi.draw();
            diff2htmlUi.highlightCode();
            const diffFiles = parseDiffFiles(diff);
            decorateRenderedDiffFiles(container, anchorPrefix, diffFiles, knownFileIds);
        };

        const renderStatus = (status) => {
            persistControls();
            updateUntrackedVisibility();
            const knownFileIds = new Set();

            renderUnifiedFileList(status);

            renderDiffSection(
                changedDiffContainer,
                status.unstaged_diff,
                "No changed diff",
                "changed-diff",
                knownFileIds
            );
            renderDiffSection(
                stagedDiffContainer,
                status.staged_diff,
                "No staged diff",
                "staged-diff",
                knownFileIds
            );

            if (showUntrackedToggle.checked) {
                renderDiffSection(
                    untrackedDiffContainer,
                    status.untracked_diff,
                    "No untracked diff",
                    "untracked-diff",
                    knownFileIds
                );
            }
            pruneCollapsedFileIds(knownFileIds);
            updateBulkToggleAvailability();
        };

        async function loadStatus({ showLoading = true } = {}) {
            if (isLoading) {
                return;
            }
            isLoading = true;
            clearError();
            if (showLoading) {
                setLoading(filesListContainer, "Loading files...");
                setLoading(changedDiffContainer, "Loading changed diff...");
                setLoading(stagedDiffContainer, "Loading staged diff...");
            }
            updateUntrackedVisibility();
            if (showLoading && showUntrackedToggle.checked) {
                setLoading(untrackedDiffContainer, "Loading untracked diff...");
            }

            try {
                const params = new URLSearchParams();
                params.set("show_untracked", showUntrackedToggle.checked ? "true" : "false");
                const response = await fetch(`/api/status?${params.toString()}`, {
                    cache: "no-store"
                });
                const data = await response.json();
                if (data.error) {
                    throw new Error(data.error);
                }

                latestStatus = data;
                renderStatus(data);
            } finally {
                isLoading = false;
            }
        }

        refreshButton.addEventListener("click", () => {
            loadStatus().catch((error) => showError(error.message));
        });

        viewModeSelect.addEventListener("change", () => {
            if (latestStatus) {
                renderStatus(latestStatus);
            } else {
                loadStatus().catch((error) => showError(error.message));
            }
        });

        showUntrackedToggle.addEventListener("change", () => {
            loadStatus().catch((error) => showError(error.message));
        });

        expandAllButton.addEventListener("click", () => {
            setAllRenderedDiffFilesCollapsed(false);
        });

        collapseAllButton.addEventListener("click", () => {
            setAllRenderedDiffFilesCollapsed(true);
        });

        goTopButton.addEventListener("click", () => {
            window.scrollTo({ top: 0, behavior: "smooth" });
        });

        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });

        window.setInterval(() => {
            loadStatus({ showLoading: false }).catch((error) => showError(error.message));
        }, AUTO_REFRESH_INTERVAL_MS);

        updateGoTopButtonVisibility();
        loadStatus().catch((error) => showError(error.message));
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
    <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/diff2html/bundles/css/diff2html.min.css" />
    <script src="https://cdn.jsdelivr.net/npm/diff2html/bundles/js/diff2html-ui.min.js"></script>
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
        .context-key {
            font-weight: 600;
            color: #1f2937;
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
        .controls {
            display: flex;
            gap: 12px;
            align-items: center;
            flex-wrap: wrap;
        }
        .controls label {
            font-size: 14px;
            display: flex;
            align-items: center;
            gap: 8px;
        }
        .controls select, .controls button {
            padding: 6px 10px;
            border: 1px solid #ccc;
            border-radius: 6px;
            background: white;
            font-size: 14px;
        }
        .controls button {
            cursor: pointer;
        }
        .range-arrow {
            font-weight: 600;
            color: #6b7280;
        }
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
        .section h2 {
            margin: 0 0 12px 0;
            font-size: 18px;
        }
        .loading, .empty {
            padding: 20px;
            color: #666;
        }
        .file-list {
            margin: 0;
            padding-left: 20px;
        }
        .file-list li {
            margin: 4px 0;
            font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
        }
        .file-list a {
            color: #0a58ca;
            text-decoration: none;
        }
        .file-list a:hover {
            text-decoration: underline;
        }
        .d2h-file-header {
            display: flex;
            align-items: center;
            flex-wrap: wrap;
            gap: 8px;
        }
        .diff-toggle-btn {
            order: -1;
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
        .diff-viewed-label:hover {
            background: #eef2f7;
        }
        .diff-viewed-label input {
            margin: 0;
            cursor: pointer;
        }
        .diff-file-viewed > .d2h-file-header {
            background: #eef2f7;
            opacity: 0.8;
        }
        .diff-file-collapsed .d2h-file-diff {
            display: none;
        }
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
        #go-top-btn.visible {
            opacity: 1;
            pointer-events: auto;
            transform: translateY(0);
        }
        #go-top-btn:hover {
            background: #eef2f7;
        }
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
            <label>
                View mode
                <select id="view-mode">
                    <option value="unified">Unified</option>
                    <option value="side-by-side">Side-by-side</option>
                </select>
            </label>
            <button id="expand-all-btn" type="button">Expand all</button>
            <button id="collapse-all-btn" type="button">Collapse all</button>
            <button id="refresh-btn" type="button">Refresh</button>
        </div>
    </div>

    <div id="error-banner"></div>

    <section class="section">
        <h2 id="files-heading">Files</h2>
        <div id="files-list">
            <div class="loading">Select a base and compare branch...</div>
        </div>
    </section>

    <section class="section">
        <h2>Compare Diff</h2>
        <div id="compare-diff" class="diff-container">
            <div class="loading">Select a base and compare branch...</div>
        </div>
    </section>
    <button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>

    <script>
        const urlParams = new URLSearchParams(window.location.search);
        const normalizeViewMode = (value) => value === "side-by-side" ? "side-by-side" : "unified";

        const baseSelect = document.getElementById("base-select");
        const compareSelect = document.getElementById("compare-select");
        const swapButton = document.getElementById("swap-btn");
        const viewModeSelect = document.getElementById("view-mode");
        const expandAllButton = document.getElementById("expand-all-btn");
        const collapseAllButton = document.getElementById("collapse-all-btn");
        const refreshButton = document.getElementById("refresh-btn");
        const errorBanner = document.getElementById("error-banner");
        const rootPathCode = document.getElementById("root-path");
        const filesHeading = document.getElementById("files-heading");
        const filesListContainer = document.getElementById("files-list");
        const compareDiffContainer = document.getElementById("compare-diff");
        const goTopButton = document.getElementById("go-top-btn");
        const GO_TOP_SHOW_SCROLL_Y = 240;
        const STORAGE_ROOT = rootPathCode ? rootPathCode.textContent : "unknown-root";
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.compare.collapsed.v2:${STORAGE_ROOT}`;
        const VIEWED_FILES_STORAGE_KEY = `gargo.compare.viewed.v1:${STORAGE_ROOT}`;

        let latestDiff = null;
        let isLoadingCompare = false;
        let collapsedFileIds = loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY);
        let viewedFileIds = loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY);
        viewModeSelect.value = normalizeViewMode(urlParams.get("view"));

        function loadIdSet(storage, key) {
            try {
                const raw = storage.getItem(key);
                if (!raw) {
                    return new Set();
                }
                const parsed = JSON.parse(raw);
                if (!Array.isArray(parsed)) {
                    return new Set();
                }
                const ids = parsed.filter((value) => typeof value === "string" && value.length > 0);
                return new Set(ids);
            } catch (_error) {
                return new Set();
            }
        }

        const persistIdSet = (storage, key, set) => {
            try {
                storage.setItem(key, JSON.stringify(Array.from(set)));
            } catch (_error) {
                // Ignore storage failures.
            }
        };

        const persistCollapsedFileIds = () => {
            persistIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY, collapsedFileIds);
        };

        const persistViewedFileIds = () => {
            persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY, viewedFileIds);
        };

        const showError = (message) => {
            errorBanner.textContent = `Error: ${message}`;
            errorBanner.style.display = "block";
        };

        const clearError = () => {
            errorBanner.textContent = "";
            errorBanner.style.display = "none";
        };

        const setLoading = (container, message) => {
            container.innerHTML = `<div class="loading">${message}</div>`;
        };

        const setEmpty = (container, message) => {
            container.innerHTML = `<div class="empty">${message}</div>`;
        };

        const updateGoTopButtonVisibility = () => {
            if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) {
                goTopButton.classList.add("visible");
            } else {
                goTopButton.classList.remove("visible");
            }
        };

        const renderDiffToggleButtonLabel = (button, collapsed) => {
            button.textContent = collapsed ? "▸" : "▾";
            button.setAttribute("aria-expanded", collapsed ? "false" : "true");
            button.setAttribute("aria-label", collapsed ? "Show diff" : "Hide diff");
            button.setAttribute("title", collapsed ? "Show diff" : "Hide diff");
        };

        const setDiffFileCollapsed = (wrapper, fileId, collapsed) => {
            if (collapsed) {
                collapsedFileIds.add(fileId);
            } else {
                collapsedFileIds.delete(fileId);
            }
            wrapper.classList.toggle("diff-file-collapsed", collapsed);
            const button = wrapper.querySelector(".diff-toggle-btn");
            if (button) {
                renderDiffToggleButtonLabel(button, collapsed);
            }
        };

        const setDiffFileViewed = (wrapper, fileId, viewed) => {
            if (viewed) {
                viewedFileIds.add(fileId);
            } else {
                viewedFileIds.delete(fileId);
            }
            wrapper.classList.toggle("diff-file-viewed", viewed);
            const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
            if (checkbox && checkbox.checked !== viewed) {
                checkbox.checked = viewed;
            }
            setDiffFileCollapsed(wrapper, fileId, viewed);
        };

        const updateBulkToggleAvailability = () => {
            const wrappers = document.querySelectorAll(".diff-container .d2h-file-wrapper[data-diff-file-id]");
            const disabled = wrappers.length === 0;
            expandAllButton.disabled = disabled;
            collapseAllButton.disabled = disabled;
        };

        const setAllRenderedDiffFilesCollapsed = (collapsed) => {
            const wrappers = document.querySelectorAll(".diff-container .d2h-file-wrapper[data-diff-file-id]");
            wrappers.forEach((wrapper) => {
                const fileId = wrapper.dataset.diffFileId;
                if (!fileId) {
                    return;
                }
                setDiffFileCollapsed(wrapper, fileId, collapsed);
            });
            persistCollapsedFileIds();
            updateBulkToggleAvailability();
        };

        const pruneCollapsedFileIds = (knownFileIds) => {
            const next = new Set();
            collapsedFileIds.forEach((fileId) => {
                if (knownFileIds.has(fileId)) {
                    next.add(fileId);
                }
            });
            if (next.size !== collapsedFileIds.size) {
                collapsedFileIds = next;
                persistCollapsedFileIds();
            }
        };

        const parseDiffFiles = (diff) => {
            if (!diff || diff.trim() === "") {
                return [];
            }
            const files = [];
            for (const line of diff.split("\n")) {
                if (!line.startsWith("diff --git ")) {
                    continue;
                }
                const match = line.match(/^diff --git a\/(.+) b\/(.+)$/);
                if (!match) {
                    continue;
                }
                files.push(match[2]);
            }
            return files;
        };

        const renderFileList = (diff) => {
            const files = parseDiffFiles(diff);
            filesHeading.textContent = files.length > 0 ? `Files (${files.length})` : "Files";
            if (files.length === 0) {
                setEmpty(filesListContainer, "No changes between branches");
                return;
            }
            filesListContainer.innerHTML = "";
            const list = document.createElement("ul");
            list.className = "file-list";
            for (let i = 0; i < files.length; i += 1) {
                const item = document.createElement("li");
                const link = document.createElement("a");
                link.href = `#compare-diff-file-${i}`;
                link.textContent = files[i];
                item.appendChild(link);
                list.appendChild(item);
            }
            filesListContainer.appendChild(list);
        };

        const decorateRenderedDiffFiles = (container, anchorPrefix, diffFiles, knownFileIds) => {
            const wrappers = container.querySelectorAll(".d2h-file-wrapper");
            wrappers.forEach((wrapper, index) => {
                wrapper.id = `${anchorPrefix}-file-${index}`;
                const filePath = diffFiles[index] || `file-${index}`;
                const fileId = `${anchorPrefix}:${filePath}`;
                wrapper.dataset.diffFileId = fileId;
                knownFileIds.add(fileId);

                const header = wrapper.querySelector(".d2h-file-header");
                if (header && !header.querySelector(".diff-toggle-btn")) {
                    const toggleButton = document.createElement("button");
                    toggleButton.type = "button";
                    toggleButton.className = "diff-toggle-btn";
                    toggleButton.addEventListener("click", () => {
                        const shouldCollapse = !wrapper.classList.contains("diff-file-collapsed");
                        setDiffFileCollapsed(wrapper, fileId, shouldCollapse);
                        persistCollapsedFileIds();
                    });
                    header.insertBefore(toggleButton, header.firstChild);
                }

                if (header && !header.querySelector(".diff-viewed-label")) {
                    const label = document.createElement("label");
                    label.className = "diff-viewed-label";
                    label.title = "Mark this file as viewed (saved per browser)";
                    const checkbox = document.createElement("input");
                    checkbox.type = "checkbox";
                    checkbox.addEventListener("click", (event) => {
                        event.stopPropagation();
                    });
                    checkbox.addEventListener("change", () => {
                        setDiffFileViewed(wrapper, fileId, checkbox.checked);
                        persistViewedFileIds();
                        persistCollapsedFileIds();
                    });
                    const text = document.createElement("span");
                    text.textContent = "Viewed";
                    label.appendChild(checkbox);
                    label.appendChild(text);
                    header.appendChild(label);
                }

                const isViewed = viewedFileIds.has(fileId);
                const isCollapsed = isViewed || collapsedFileIds.has(fileId);
                wrapper.classList.toggle("diff-file-viewed", isViewed);
                wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
                if (isViewed) {
                    collapsedFileIds.add(fileId);
                }
                const toggleButton = wrapper.querySelector(".diff-toggle-btn");
                if (toggleButton) {
                    renderDiffToggleButtonLabel(toggleButton, isCollapsed);
                }
                const checkbox = wrapper.querySelector(".diff-viewed-label input[type=checkbox]");
                if (checkbox) {
                    checkbox.checked = isViewed;
                }
            });
        };

        const renderDiff = (diff) => {
            const viewMode = normalizeViewMode(viewModeSelect.value);
            const knownFileIds = new Set();
            renderFileList(diff);
            if (!diff || diff.trim() === "") {
                setEmpty(compareDiffContainer, "No changes between branches");
                pruneCollapsedFileIds(knownFileIds);
                updateBulkToggleAvailability();
                return;
            }
            compareDiffContainer.innerHTML = "";
            const configuration = {
                drawFileList: false,
                fileListToggle: false,
                fileListStartVisible: false,
                fileContentToggle: false,
                matching: "lines",
                outputFormat: viewMode === "side-by-side" ? "side-by-side" : "line-by-line",
                synchronisedScroll: true,
                highlight: true,
                renderNothingWhenEmpty: false,
            };
            const diff2htmlUi = new Diff2HtmlUI(compareDiffContainer, diff, configuration);
            diff2htmlUi.draw();
            diff2htmlUi.highlightCode();
            const diffFiles = parseDiffFiles(diff);
            decorateRenderedDiffFiles(compareDiffContainer, "compare-diff", diffFiles, knownFileIds);
            pruneCollapsedFileIds(knownFileIds);
            updateBulkToggleAvailability();
        };

        const persistControls = () => {
            const params = new URLSearchParams();
            if (baseSelect.value) params.set("base", baseSelect.value);
            if (compareSelect.value) params.set("compare", compareSelect.value);
            params.set("view", normalizeViewMode(viewModeSelect.value));
            history.replaceState(null, "", `/compare?${params.toString()}`);
        };

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
                if (data.error) {
                    throw new Error(data.error);
                }
                const branches = Array.isArray(data.branches) ? data.branches : [];
                const current = typeof data.current === "string" ? data.current : null;
                const defaultBranch = typeof data.default === "string" ? data.default : null;

                const baseParam = urlParams.get("base");
                const compareParam = urlParams.get("compare");
                const baseFallback =
                    (defaultBranch && branches.includes(defaultBranch) && defaultBranch !== current
                        ? defaultBranch
                        : null)
                    || (defaultBranch && branches.includes(defaultBranch) ? defaultBranch : null)
                    || branches.find((b) => b !== current)
                    || branches[0]
                    || "";
                const compareFallback = current || branches[0] || "";
                populateSelect(baseSelect, branches, baseParam || baseFallback);
                populateSelect(compareSelect, branches, compareParam || compareFallback);
            } catch (error) {
                showError(error.message);
                baseSelect.innerHTML = '<option value="">(error)</option>';
                compareSelect.innerHTML = '<option value="">(error)</option>';
            }
        }

        async function loadCompare({ showLoading = true } = {}) {
            const base = baseSelect.value;
            const compare = compareSelect.value;
            if (!base || !compare) {
                setEmpty(filesListContainer, "Select a base and compare branch...");
                setEmpty(compareDiffContainer, "Select a base and compare branch...");
                updateBulkToggleAvailability();
                return;
            }
            if (isLoadingCompare) {
                return;
            }
            isLoadingCompare = true;
            clearError();
            if (showLoading) {
                setLoading(filesListContainer, "Loading files...");
                setLoading(compareDiffContainer, `Loading diff for ${base}...${compare}...`);
            }
            try {
                const params = new URLSearchParams();
                params.set("base", base);
                params.set("compare", compare);
                const response = await fetch(`/api/compare?${params.toString()}`, {
                    cache: "no-store"
                });
                const data = await response.json();
                if (data.error) {
                    throw new Error(data.error);
                }
                latestDiff = typeof data.diff === "string" ? data.diff : "";
                renderDiff(latestDiff);
            } finally {
                isLoadingCompare = false;
            }
        }

        refreshButton.addEventListener("click", () => {
            loadCompare().catch((error) => showError(error.message));
        });

        viewModeSelect.addEventListener("change", () => {
            persistControls();
            if (latestDiff !== null) {
                renderDiff(latestDiff);
            }
        });

        baseSelect.addEventListener("change", () => {
            persistControls();
            loadCompare().catch((error) => showError(error.message));
        });

        compareSelect.addEventListener("change", () => {
            persistControls();
            loadCompare().catch((error) => showError(error.message));
        });

        swapButton.addEventListener("click", () => {
            const a = baseSelect.value;
            const b = compareSelect.value;
            baseSelect.value = b;
            compareSelect.value = a;
            persistControls();
            loadCompare().catch((error) => showError(error.message));
        });

        expandAllButton.addEventListener("click", () => {
            setAllRenderedDiffFilesCollapsed(false);
        });

        collapseAllButton.addEventListener("click", () => {
            setAllRenderedDiffFilesCollapsed(true);
        });

        goTopButton.addEventListener("click", () => {
            window.scrollTo({ top: 0, behavior: "smooth" });
        });

        window.addEventListener("scroll", updateGoTopButtonVisibility, { passive: true });

        (async () => {
            await loadBranches();
            persistControls();
            await loadCompare().catch((error) => showError(error.message));
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
        .route("/api/branches", get(handle_api_branches_request))
        .route("/api/compare", get(handle_api_compare_request))
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

    Html(DIFF_HTML_TEMPLATE.replace("{{ROOT_PATH}}", &html_escape(&root_path)))
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
    fn status_response(payload: serde_json::Value) -> Response {
        let mut response = (StatusCode::OK, Json(payload)).into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, no-cache, must-revalidate"),
        );
        response
    }

    let show_untracked = parse_bool_param(params.get("show_untracked"), true);
    let repo_root = &state.project_root;

    let unstaged_diff = match git_output_in_repo(repo_root, &["diff"]).await {
        Ok(output) => output,
        Err(error) => {
            return status_response(serde_json::json!({
                "error": error
            }));
        }
    };

    let staged_diff = match git_output_in_repo(repo_root, &["diff", "--cached"]).await {
        Ok(output) => output,
        Err(error) => {
            return status_response(serde_json::json!({
                "error": error
            }));
        }
    };

    let (untracked_files, untracked_diff) = if show_untracked {
        let untracked_raw =
            match git_output_in_repo(repo_root, &["ls-files", "--others", "--exclude-standard"])
                .await
            {
                Ok(output) => output,
                Err(error) => {
                    return status_response(serde_json::json!({
                        "error": error
                    }));
                }
            };
        let untracked_files: Vec<String> = untracked_raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();

        let mut untracked_diffs = Vec::with_capacity(untracked_files.len());
        for path in &untracked_files {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["diff", "--no-index", "--", "/dev/null"]);
            cmd.arg(path);
            cmd.current_dir(repo_root);

            let patch = match git_output_from_command(
                cmd,
                &[1],
                &format!("git diff --no-index -- /dev/null {}", path),
            )
            .await
            {
                Ok(output) => output,
                Err(error) => {
                    return status_response(serde_json::json!({
                        "error": error
                    }));
                }
            };
            if !patch.trim().is_empty() {
                untracked_diffs.push(patch);
            }
        }

        (untracked_files, untracked_diffs.join("\n"))
    } else {
        (Vec::new(), String::new())
    };

    status_response(serde_json::json!({
        "unstaged_diff": unstaged_diff,
        "staged_diff": staged_diff,
        "untracked_files": untracked_files,
        "untracked_diff": untracked_diff,
    }))
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
    Html(COMPARE_HTML_TEMPLATE.replace("{{ROOT_PATH}}", &html_escape(&root_path)))
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
        if let Some(rest) = trimmed.strip_prefix("origin/") {
            if !rest.is_empty() && known.iter().any(|b| b == rest) {
                return Some(rest.to_string());
            }
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
    let base_raw = match params.get("base") {
        Some(v) => v,
        None => return bad_request("missing `base` query parameter"),
    };
    let compare_raw = match params.get("compare") {
        Some(v) => v,
        None => return bad_request("missing `compare` query parameter"),
    };
    let base = match parse_branch_name(base_raw) {
        Some(name) => name,
        None => return bad_request(format!("invalid branch name: {}", base_raw)),
    };
    let compare = match parse_branch_name(compare_raw) {
        Some(name) => name,
        None => return bad_request(format!("invalid branch name: {}", compare_raw)),
    };

    let range = format!("{}...{}", base, compare);
    let diff = match git_output_in_repo(&state.project_root, &["diff", &range]).await {
        Ok(output) => output,
        Err(error) => return bad_request(error),
    };

    ok_json(serde_json::json!({
        "base": base,
        "compare": compare,
        "diff": diff,
    }))
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

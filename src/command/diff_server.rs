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
        .diff-toggle-btn {
            float: right;
            margin-left: 8px;
            padding: 2px 8px;
            border: 1px solid #d0d7de;
            border-radius: 6px;
            background: #f6f8fa;
            color: #24292f;
            font-size: 12px;
            cursor: pointer;
        }
        .diff-toggle-btn:hover {
            background: #eef2f7;
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
        const COLLAPSED_FILES_STORAGE_KEY = `gargo.diff.collapsed.v1:${rootPathCode ? rootPathCode.textContent : "unknown-root"}`;

        let latestStatus = null;
        let isLoading = false;
        let collapsedFileIds = loadCollapsedFileIds();
        viewModeSelect.value = normalizeViewMode(urlParams.get("view"));
        showUntrackedToggle.checked = parseBoolParam(urlParams.get("show_untracked"), true);

        function loadCollapsedFileIds() {
            try {
                const raw = sessionStorage.getItem(COLLAPSED_FILES_STORAGE_KEY);
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

        const persistCollapsedFileIds = () => {
            try {
                sessionStorage.setItem(
                    COLLAPSED_FILES_STORAGE_KEY,
                    JSON.stringify(Array.from(collapsedFileIds))
                );
            } catch (_error) {
                // Ignore storage failures and keep UI responsive.
            }
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
            button.textContent = collapsed ? "Show diff" : "Hide diff";
            button.setAttribute("aria-expanded", collapsed ? "false" : "true");
            button.setAttribute("aria-label", collapsed ? "Show diff" : "Hide diff");
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
                const fileId = `${anchorPrefix}:${filePath}:${index}`;
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
                    header.appendChild(toggleButton);
                }

                const isCollapsed = collapsedFileIds.has(fileId);
                wrapper.classList.toggle("diff-file-collapsed", isCollapsed);
                const toggleButton = wrapper.querySelector(".diff-toggle-btn");
                if (toggleButton) {
                    renderDiffToggleButtonLabel(toggleButton, isCollapsed);
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

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<DiffServerState>,
) {
    let app = Router::new()
        .route("/diff", get(handle_html_request))
        .route("/api/status", get(handle_api_status_request))
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
}

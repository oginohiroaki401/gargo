//! Unified local server for the keyboard-driven gargo code and Git browser.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use axum::{
    Router,
    routing::{get, post},
};
use tower_http::cors::CorsLayer;

use crate::command::diff_server::{self, DiffServerState};
use crate::command::diff_viewed::ViewedStore;
use crate::command::gargo_preview_server::{
    self, GargoPreviewEvent, PreviewBrowserEvent, PreviewBrowserEventKind, PreviewServerState,
};

mod repo_api;
mod util;

pub(crate) use repo_api::*;
pub(crate) use util::*;

#[derive(Debug, Clone)]
pub enum GargoServerRoute {
    Root,
    Tree { path: String },
    Blob { path: String },
    Changes,
    Compare,
    Commits,
    Commit { hash: String },
}

#[derive(Debug, Clone)]
pub enum GargoServerCommand {
    Start { repo_root: PathBuf },
    Stop,
    OpenRoute { route: GargoServerRoute },
    SetActivePath { rel_path: Option<String> },
    RefreshActive,
    UpdateBufferContent { content: String, cursor_line: usize },
    UpdateCursorLine { line: usize },
}

#[derive(Debug, Clone)]
pub enum GargoServerEvent {
    Started { port: u16, root_url: String },
    Stopped,
    Detached { requested_path: String },
    Opened { url: String },
    Error(String),
}

pub struct GargoServerHandle {
    pub command_tx: mpsc::Sender<GargoServerCommand>,
    pub event_rx: mpsc::Receiver<GargoServerEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl GargoServerHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = GargoServerWorker {
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
            .name("gargo-server".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("Failed to spawn worker thread: {}", e))?;
        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }
}

struct GargoServerWorker {
    command_rx: mpsc::Receiver<GargoServerCommand>,
    event_tx: mpsc::Sender<GargoServerEvent>,
    tokio_runtime: tokio::runtime::Runtime,
    server_shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    preview_state: Option<Arc<Mutex<PreviewServerState>>>,
    pending_active_rel_path: Option<String>,
    port: Option<u16>,
}

impl GargoServerWorker {
    fn run(mut self) {
        loop {
            match self.command_rx.recv() {
                Ok(GargoServerCommand::Start { repo_root }) => self.handle_start(repo_root),
                Ok(GargoServerCommand::Stop) => self.handle_stop(),
                Ok(GargoServerCommand::OpenRoute { route }) => self.handle_open_route(route),
                Ok(GargoServerCommand::SetActivePath { rel_path }) => {
                    self.handle_set_active_path(rel_path)
                }
                Ok(GargoServerCommand::RefreshActive) => self.handle_refresh_active(),
                Ok(GargoServerCommand::UpdateBufferContent {
                    content,
                    cursor_line,
                }) => self.handle_update_buffer_content(content, cursor_line),
                Ok(GargoServerCommand::UpdateCursorLine { line }) => {
                    self.handle_update_cursor_line(line)
                }
                Err(_) => break,
            }
        }
    }

    fn handle_start(&mut self, repo_root: PathBuf) {
        if self.server_shutdown_tx.is_some() {
            let _ = self.event_tx.send(GargoServerEvent::Error(
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
                let _ = self.event_tx.send(GargoServerEvent::Error(format!(
                    "Failed to bind Gargo server on localhost: {}",
                    err
                )));
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(err) => {
                let _ = self.event_tx.send(GargoServerEvent::Error(format!(
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
            .block_on(gargo_preview_server::resolve_repo_url_context(&repo_root));
        let root_url = format!("http://127.0.0.1:{port}/");

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
            viewed: ViewedStore::open(),
        });
        let github_state = Arc::new(GargoServerState {
            repo_root,
            files_cache: std::sync::Mutex::new(None),
            fs_generation: std::sync::atomic::AtomicU64::new(0),
        });

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
            .send(GargoServerEvent::Started { port, root_url });
    }

    fn handle_stop(&mut self) {
        if let Some(shutdown_tx) = self.server_shutdown_tx.take() {
            let _ = shutdown_tx.send(());
            self.preview_state = None;
            self.port = None;
            let _ = self.event_tx.send(GargoServerEvent::Stopped);
        } else {
            let _ = self
                .event_tx
                .send(GargoServerEvent::Error("Server not running".to_string()));
        }
    }

    fn handle_open_route(&self, route: GargoServerRoute) {
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
        let _ = self.event_tx.send(GargoServerEvent::Opened {
            url: format!("http://127.0.0.1:{}{}", port, path),
        });
    }

    fn handle_set_active_path(&mut self, rel_path: Option<String>) {
        let normalized = rel_path.map(|p| gargo_preview_server::normalize_rel_path_for_compare(&p));
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
                url: Some(gargo_preview_server::preview_url_for_rel_path(
                    state.port,
                    &state.url_ctx,
                    state.active_rel_path.as_deref(),
                )),
                detached: state.detached,
                version: state.version,
                cursor_line: None,
            };
            gargo_preview_server::broadcast_preview_event(&mut state, event);
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
            url: Some(gargo_preview_server::preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: state.cursor_line,
        };
        gargo_preview_server::broadcast_preview_event(&mut state, event);
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
            url: Some(gargo_preview_server::preview_url_for_rel_path(
                state.port,
                &state.url_ctx,
                state.active_rel_path.as_deref(),
            )),
            detached: state.detached,
            version: state.version,
            cursor_line: Some(cursor_line),
        };
        gargo_preview_server::broadcast_preview_event(&mut state, event);
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
        gargo_preview_server::broadcast_preview_event(&mut state, event);
    }
}

impl GargoServerRoute {
    fn path(&self, _ctx: &gargo_preview_server::RepoUrlContext) -> String {
        match self {
            Self::Root | Self::Tree { .. } | Self::Blob { .. } => "/#explorer".to_string(),
            Self::Changes => "/#status".to_string(),
            Self::Compare => "/#compare".to_string(),
            Self::Commits | Self::Commit { .. } => "/#history".to_string(),
        }
    }
}

fn bridge_preview_events(
    event_tx: mpsc::Sender<GargoServerEvent>,
) -> mpsc::Sender<GargoPreviewEvent> {
    let (tx, rx) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("gargo-server-preview-events".to_string())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    GargoPreviewEvent::Detached { requested_path } => {
                        let _ = event_tx.send(GargoServerEvent::Detached { requested_path });
                    }
                    GargoPreviewEvent::Error(msg) => {
                        let _ = event_tx.send(GargoServerEvent::Error(msg));
                    }
                    GargoPreviewEvent::Started { .. } | GargoPreviewEvent::Stopped => {}
                }
            }
        });
    tx
}

#[derive(Debug)]
pub(crate) struct GargoServerState {
    pub(crate) repo_root: PathBuf,
    /// Short-lived cache for the `/api/files` listing (`git ls-files`), which the
    /// editor hits on every Cmd+P open. Holds `(generation, cached_at, files)`;
    /// reused while the generation matches and the entry is within the TTL.
    pub(crate) files_cache: std::sync::Mutex<Option<(u64, std::time::Instant, Vec<String>)>>,
    /// Bumped by filesystem-mutating editor handlers (create/rename/delete/save)
    /// so `files_cache` is invalidated immediately rather than waiting for the TTL.
    pub(crate) fs_generation: std::sync::atomic::AtomicU64,
}

async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    preview_state: Arc<Mutex<PreviewServerState>>,
    diff_state: Arc<DiffServerState>,
    github_state: Arc<GargoServerState>,
) {
    // URLs mirror github.com: `/{owner}/{repo}/blob/{branch}/{path}` etc., so
    // swapping `http://127.0.0.1:PORT/` for `github.com/` yields the same page.
    // The dynamic `/{owner}/{repo}` pattern is 2 segments; static prefixes
    // (`/events`, `/assets`, `/status`, `/api`, ...) rank above it in axum's
    // router, so do not add new top-level 2-segment static routes.
    let preview_routes = Router::new()
        .route("/events", get(gargo_preview_server::handle_events))
        .route(
            "/assets/mermaid.min.js",
            get(gargo_preview_server::handle_mermaid_asset),
        )
        .route(
            "/assets/server-shared.css",
            get(gargo_preview_server::handle_shared_css_asset),
        )
        .route(
            "/assets/server-shortcuts.js",
            get(gargo_preview_server::handle_shortcuts_js_asset),
        )
        .route(
            "/{owner}/{repo}/tree/{*rest}",
            get(gargo_preview_server::handle_tree),
        )
        .route(
            "/{owner}/{repo}/blob/{*rest}",
            get(gargo_preview_server::handle_blob),
        )
        .route(
            "/{owner}/{repo}/preview/{*rest}",
            get(gargo_preview_server::handle_preview),
        )
        .with_state(preview_state);

    let diff_routes = Router::new()
        .route(
            "/diff",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/changes",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/status",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/commit",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/compare",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/branches",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route("/api/status", get(diff_server::handle_api_status_request))
        .route(
            "/api/status/file",
            get(diff_server::handle_api_status_file_request),
        )
        .route(
            "/api/status/viewed",
            post(diff_server::handle_api_status_viewed_request),
        )
        .route(
            "/api/status/context",
            get(diff_server::handle_api_status_context_request),
        )
        .route(
            "/api/status/stage",
            post(diff_server::handle_api_status_stage_request),
        )
        .route(
            "/api/status/unstage",
            post(diff_server::handle_api_status_unstage_request),
        )
        .route(
            "/api/status/commit-prepare",
            get(diff_server::handle_api_commit_prepare_request),
        )
        .route(
            "/api/status/commit",
            post(diff_server::handle_api_commit_request),
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
        .route(
            "/api/compare/viewed",
            post(diff_server::handle_api_compare_viewed_request),
        )
        .route(
            "/api/compare/context",
            get(diff_server::handle_api_compare_context_request),
        )
        .route("/split", get(diff_server::handle_split_request))
        .with_state(diff_state);

    let github_routes = Router::new()
        .route(
            "/",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/{owner}/{repo}",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/{owner}/{repo}/commits",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/{owner}/{repo}/commits/{*branch}",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route(
            "/{owner}/{repo}/commit/{hash}",
            get(crate::command::web_editor_server::handle_editor_page),
        )
        .route("/api/tree/{*path}", get(handle_api_tree))
        .route("/api/blob/{*path}", get(handle_api_blob))
        .route("/api/commits", get(handle_api_commits))
        .route("/api/commit/{hash}", get(handle_api_commit))
        .route("/api/commit/{hash}/file", get(handle_api_commit_file))
        .with_state(github_state.clone());

    use crate::command::web_editor_server as editor;
    let editor_routes = Router::new()
        .route("/editor", get(editor::handle_editor_page))
        .route("/editor/{*path}", get(editor::handle_editor_page))
        .route("/assets/gargo_wasm.js", get(editor::handle_wasm_js))
        .route(
            "/assets/gargo_wasm_bg.wasm",
            get(editor::handle_wasm_binary),
        )
        .route("/api/file", get(editor::handle_api_file))
        .route("/api/files", get(editor::handle_api_files))
        .route(
            "/api/last-file",
            get(editor::handle_api_last_file).post(editor::handle_api_last_file_set),
        )
        .route("/api/git-status", get(editor::handle_api_git_status))
        .route("/api/search", get(editor::handle_api_search))
        .route("/api/save", post(editor::handle_api_save))
        .route("/api/fs/create", post(editor::handle_api_fs_create))
        .route("/api/fs/rename", post(editor::handle_api_fs_rename))
        .route("/api/fs/delete", post(editor::handle_api_fs_delete))
        .route("/api/fs/reveal", post(editor::handle_api_fs_reveal))
        .route("/api/highlight", post(editor::handle_api_highlight))
        .route("/api/symbols", post(editor::handle_api_symbols))
        .route("/api/git-gutter", post(editor::handle_api_git_gutter))
        .route("/api/preview", post(editor::handle_api_preview))
        .with_state(github_state);

    let app = preview_routes
        .merge(diff_routes)
        .merge(github_routes)
        .merge(editor_routes)
        .layer(CorsLayer::permissive());
    let _ = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .await;
}

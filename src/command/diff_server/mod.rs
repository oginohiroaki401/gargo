//! Diff server for viewing git status and diffs in a browser with rich formatting.
//!
//! This module implements an HTTP server for a tig-like git status page.
//! It follows the async runtime pattern:
//! - Command enum for controlling the server
//! - Event enum for status updates
//! - Handle with mpsc channels for communication
//! - Worker that runs on separate thread with Tokio runtime

use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;

use axum::{
    Router,
    routing::{get, post},
};
use tower_http::cors::CorsLayer;

use crate::command::diff_viewed::ViewedStore;
use crate::command::registry::{CommandContext, CommandEffect, CommandEntry, CommandRegistry};
use crate::input::action::{Action, AppAction, IntegrationAction};

mod compare_api;
mod git_ops;
mod html_pages;
mod render;
mod split;
mod status_api;
mod templates;
mod validation;

pub(crate) use compare_api::*;
pub(crate) use git_ops::*;
pub(crate) use html_pages::*;
pub(crate) use render::*;
pub(crate) use split::*;
pub(crate) use status_api::*;
pub(crate) use templates::*;
pub(crate) use validation::*;

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
            diff_cache: Arc::new(DiffRenderCache::new()),
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
    /// In-memory cache of rendered immutable (compare/commit) file diffs.
    pub(crate) diff_cache: Arc<DiffRenderCache>,
}

impl DiffServerState {
    /// Stable key for this repo in the viewed-state database.
    fn repo_key(&self) -> String {
        self.project_root.to_string_lossy().to_string()
    }
}

/// Run the HTTP server
async fn run_server(
    listener: tokio::net::TcpListener,
    shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    state: Arc<DiffServerState>,
) {
    let app = Router::new()
        .route(
            "/assets/server-shared.css",
            get(crate::command::gargo_preview_server::handle_shared_css_asset),
        )
        .route(
            "/assets/server-shortcuts.js",
            get(crate::command::gargo_preview_server::handle_shortcuts_js_asset),
        )
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

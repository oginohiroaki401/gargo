use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::command::github_server::{GithubServerCommand, GithubServerEvent, GithubServerHandle};
use crate::config::Config;
use crate::core::document::DocumentId;
use crate::plugin::types::{Plugin, PluginCommandSpec, PluginContext, PluginEvent, PluginOutput};

pub struct GithubServerPlugin {
    commands: Vec<PluginCommandSpec>,
    handle: Option<GithubServerHandle>,
    project_root: PathBuf,
    auto_open_browser: bool,
    server_port: Option<u16>,
    is_running: bool,
    is_detached: bool,
    last_active_rel_path: Option<String>,
    last_observed_file_sig: Option<ActiveFileSignature>,
    last_file_probe_at: Option<Instant>,
    last_pushed_content_hash: u64,
    last_pushed_cursor_line: usize,
    last_content_push_at: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveFileSignature {
    len: u64,
    modified_unix_ns: Option<u128>,
}

const EXTERNAL_CHANGE_POLL_INTERVAL: Duration = Duration::from_millis(600);
const BUFFER_PUSH_INTERVAL: Duration = Duration::from_millis(300);

impl GithubServerPlugin {
    pub fn new(config: &Config, project_root: &Path) -> Self {
        let handle = GithubServerHandle::new().ok();
        Self {
            commands: vec![
                PluginCommandSpec {
                    id: "server.start_github".to_string(),
                    label: "Start gargo server".to_string(),
                    category: Some("Server".to_string()),
                },
                PluginCommandSpec {
                    id: "server.stop_github".to_string(),
                    label: "Stop gargo server".to_string(),
                    category: Some("Server".to_string()),
                },
            ],
            handle,
            project_root: project_root.to_path_buf(),
            auto_open_browser: config.plugin.github_server.auto_open_browser,
            server_port: None,
            is_running: false,
            is_detached: false,
            last_active_rel_path: None,
            last_observed_file_sig: None,
            last_file_probe_at: None,
            last_pushed_content_hash: 0,
            last_pushed_cursor_line: 0,
            last_content_push_at: None,
        }
    }

    fn active_rel_path(&self, ctx: &PluginContext) -> Option<String> {
        let fp = ctx.editor().active_buffer().file_path.as_ref()?;
        let rel_path = fp.strip_prefix(&self.project_root).ok()?;
        Some(rel_path.to_string_lossy().replace('\\', "/"))
    }

    fn send(&self, command: GithubServerCommand) {
        if let Some(handle) = &self.handle {
            let _ = handle.command_tx.send(command);
        }
    }

    fn clear_external_change_probe_state(&mut self) {
        self.last_observed_file_sig = None;
        self.last_file_probe_at = None;
    }

    fn snapshot_active_file_signature(&self, ctx: &PluginContext) -> Option<ActiveFileSignature> {
        let file_path = ctx.editor().active_buffer().file_path.as_ref()?;
        file_path.strip_prefix(&self.project_root).ok()?;
        let metadata = std::fs::metadata(file_path).ok()?;
        let modified_unix_ns = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        Some(ActiveFileSignature {
            len: metadata.len(),
            modified_unix_ns,
        })
    }

    fn refresh_file_signature_baseline(&mut self, ctx: &PluginContext) {
        self.last_observed_file_sig = self.snapshot_active_file_signature(ctx);
        self.last_file_probe_at = Some(Instant::now());
    }

    fn maybe_push_buffer_content(&mut self, ctx: &PluginContext) {
        if !self.is_running || self.is_detached || self.active_rel_path(ctx).is_none() {
            return;
        }
        if let Some(last_push) = self.last_content_push_at
            && last_push.elapsed() < BUFFER_PUSH_INTERVAL
        {
            return;
        }
        if self.last_content_push_at.is_none() {
            self.last_content_push_at = Some(Instant::now());
            return;
        }

        let doc = ctx.editor().active_buffer();
        let content = doc.rope.to_string();
        let cursor_line = doc.cursor_line() + 1;

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let content_hash = hasher.finish();

        if content_hash != self.last_pushed_content_hash {
            self.last_pushed_content_hash = content_hash;
            self.last_pushed_cursor_line = cursor_line;
            self.last_content_push_at = Some(Instant::now());
            self.send(GithubServerCommand::UpdateBufferContent {
                content,
                cursor_line,
            });
        } else if cursor_line != self.last_pushed_cursor_line {
            self.last_pushed_cursor_line = cursor_line;
            self.last_content_push_at = Some(Instant::now());
            self.send(GithubServerCommand::UpdateCursorLine { line: cursor_line });
        }
    }

    fn clear_buffer_push_state(&mut self) {
        self.last_pushed_content_hash = 0;
        self.last_pushed_cursor_line = 0;
        self.last_content_push_at = None;
    }

    fn maybe_refresh_on_external_change(&mut self, ctx: &PluginContext) {
        if !self.is_running || self.is_detached {
            return;
        }
        if self.active_rel_path(ctx).is_none() {
            self.clear_external_change_probe_state();
            return;
        }
        if let Some(last_probe_at) = self.last_file_probe_at
            && last_probe_at.elapsed() < EXTERNAL_CHANGE_POLL_INTERVAL
        {
            return;
        }
        let current_sig = self.snapshot_active_file_signature(ctx);
        self.last_file_probe_at = Some(Instant::now());
        match (&self.last_observed_file_sig, &current_sig) {
            (Some(previous), Some(current)) if previous != current => {
                self.send(GithubServerCommand::RefreshActive);
                self.last_observed_file_sig = current_sig;
            }
            (None, sig) => self.last_observed_file_sig = sig.clone(),
            _ => {}
        }
    }

    fn on_active_buffer_activated(
        &mut self,
        doc_id: DocumentId,
        ctx: &PluginContext,
    ) -> Vec<PluginOutput> {
        if doc_id != ctx.editor().active_buffer().id {
            return Vec::new();
        }
        let rel_path = self.active_rel_path(ctx);
        if rel_path == self.last_active_rel_path {
            return Vec::new();
        }
        self.last_active_rel_path = rel_path.clone();
        self.is_detached = false;
        self.clear_external_change_probe_state();
        self.clear_buffer_push_state();
        self.send(GithubServerCommand::SetActivePath { rel_path });
        Vec::new()
    }

    fn on_active_buffer_saved(
        &mut self,
        doc_id: DocumentId,
        ctx: &PluginContext,
    ) -> Vec<PluginOutput> {
        if doc_id != ctx.editor().active_buffer().id || self.is_detached || !self.is_running {
            return Vec::new();
        }
        let rel_path = self.active_rel_path(ctx);
        if rel_path != self.last_active_rel_path {
            self.last_active_rel_path = rel_path.clone();
            self.send(GithubServerCommand::SetActivePath { rel_path });
        }
        self.send(GithubServerCommand::RefreshActive);
        self.refresh_file_signature_baseline(ctx);
        Vec::new()
    }
}

impl Plugin for GithubServerPlugin {
    fn id(&self) -> &str {
        "github_server"
    }

    fn commands(&self) -> &[PluginCommandSpec] {
        &self.commands
    }

    fn on_command(&mut self, command_id: &str, _ctx: &PluginContext) -> Vec<PluginOutput> {
        let Some(handle) = &self.handle else {
            return vec![PluginOutput::Message(
                "Gargo server plugin unavailable".to_string(),
            )];
        };
        let result = match command_id {
            "server.start_github" => handle.command_tx.send(GithubServerCommand::Start {
                repo_root: self.project_root.clone(),
            }),
            "server.stop_github" => handle.command_tx.send(GithubServerCommand::Stop),
            _ => return Vec::new(),
        };
        if result.is_err() {
            vec![PluginOutput::Message(
                "Failed to send Gargo server command".to_string(),
            )]
        } else {
            Vec::new()
        }
    }

    fn on_event(&mut self, event: &PluginEvent, ctx: &PluginContext) -> Vec<PluginOutput> {
        match event {
            PluginEvent::BufferActivated { doc_id } => {
                self.on_active_buffer_activated(*doc_id, ctx)
            }
            PluginEvent::BufferSaved { doc_id } => self.on_active_buffer_saved(*doc_id, ctx),
            PluginEvent::Tick
            | PluginEvent::BufferChanged { .. }
            | PluginEvent::BufferClosed { .. } => Vec::new(),
        }
    }

    fn poll(&mut self, ctx: &PluginContext) -> Vec<PluginOutput> {
        let Some(handle) = &self.handle else {
            return Vec::new();
        };
        let mut drained = Vec::new();
        while let Ok(event) = handle.event_rx.try_recv() {
            drained.push(event);
        }

        let mut out = Vec::new();
        for event in drained {
            match event {
                GithubServerEvent::Started { port, root_url } => {
                    self.server_port = Some(port);
                    self.is_running = true;
                    self.is_detached = false;
                    self.last_active_rel_path = None;
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    self.refresh_file_signature_baseline(ctx);
                    out.push(PluginOutput::Message(format!(
                        "Gargo server: {}",
                        root_url
                    )));
                    if self.auto_open_browser {
                        out.push(PluginOutput::OpenUrl(root_url));
                    }
                }
                GithubServerEvent::Stopped => {
                    self.server_port = None;
                    self.is_running = false;
                    self.is_detached = false;
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    out.push(PluginOutput::Message("Gargo server stopped".to_string()));
                }
                GithubServerEvent::Detached { requested_path } => {
                    self.is_detached = true;
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    out.push(PluginOutput::Message(format!(
                        "Gargo server detached: {}",
                        requested_path
                    )));
                }
                GithubServerEvent::Opened { url } => {
                    out.push(PluginOutput::Message(format!("Gargo server: {}", url)));
                    if self.auto_open_browser {
                        out.push(PluginOutput::OpenUrl(url));
                    }
                }
                GithubServerEvent::Error(msg) => {
                    out.push(PluginOutput::Message(format!("Server error: {}", msg)));
                }
            }
        }
        self.maybe_refresh_on_external_change(ctx);
        self.maybe_push_buffer_content(ctx);
        out
    }
}

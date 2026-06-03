use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::command::gargo_preview_server::{
    GargoPreviewCommand, GargoPreviewEvent, GargoPreviewHandle,
};
use crate::config::Config;
use crate::core::document::DocumentId;
use crate::plugin::types::{Plugin, PluginCommandSpec, PluginContext, PluginEvent, PluginOutput};

pub struct GargoPreviewPlugin {
    commands: Vec<PluginCommandSpec>,
    handle: Option<GargoPreviewHandle>,
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
    exists: bool,
    len: u64,
    modified_unix_ns: Option<u128>,
}

const EXTERNAL_CHANGE_POLL_INTERVAL: Duration = Duration::from_millis(600);
const BUFFER_PUSH_INTERVAL: Duration = Duration::from_millis(300);

impl GargoPreviewPlugin {
    pub fn new(config: &Config, project_root: &Path) -> Self {
        let handle = GargoPreviewHandle::new().ok();
        Self {
            commands: vec![
                PluginCommandSpec {
                    id: "server.start_gargo_preview".to_string(),
                    label: "Start Gargo Preview Server".to_string(),
                    category: Some("Server".to_string()),
                },
                PluginCommandSpec {
                    id: "server.stop_gargo_preview".to_string(),
                    label: "Stop Gargo Preview Server".to_string(),
                    category: Some("Server".to_string()),
                },
            ],
            handle,
            project_root: project_root.to_path_buf(),
            auto_open_browser: config.plugin.gargo_preview.auto_open_browser,
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

    fn preview_url(port: u16, rel_path: Option<&str>) -> String {
        if let Some(rel_path) = rel_path {
            format!("http://127.0.0.1:{}/blob/{}", port, rel_path)
        } else {
            format!("http://127.0.0.1:{}/", port)
        }
    }

    fn sync_active_path(&self, rel_path: Option<String>) {
        if let Some(handle) = &self.handle {
            let _ = handle
                .command_tx
                .send(GargoPreviewCommand::SetActivePath { rel_path });
        }
    }

    fn refresh_active_preview(&self) {
        if let Some(handle) = &self.handle {
            let _ = handle.command_tx.send(GargoPreviewCommand::RefreshActive);
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
            exists: true,
            len: metadata.len(),
            modified_unix_ns,
        })
    }

    fn refresh_file_signature_baseline(&mut self, ctx: &PluginContext) {
        self.last_observed_file_sig = self.snapshot_active_file_signature(ctx);
        self.last_file_probe_at = Some(Instant::now());
    }

    fn maybe_push_buffer_content(&mut self, ctx: &PluginContext) {
        if !self.is_running || self.is_detached {
            return;
        }
        if self.active_rel_path(ctx).is_none() {
            return;
        }
        match self.last_content_push_at {
            None => {
                // First call — initialize the timer but don't push yet
                self.last_content_push_at = Some(Instant::now());
                return;
            }
            Some(last_push) if last_push.elapsed() < BUFFER_PUSH_INTERVAL => {
                return;
            }
            _ => {}
        }

        let doc = ctx.editor().active_buffer();
        let content = doc.rope.to_string();
        let cursor_line = doc.cursor_line() + 1; // 1-based

        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        let content_hash = hasher.finish();

        if content_hash != self.last_pushed_content_hash {
            // Content changed — push full content + cursor
            self.last_pushed_content_hash = content_hash;
            self.last_pushed_cursor_line = cursor_line;
            self.last_content_push_at = Some(Instant::now());
            if let Some(handle) = &self.handle {
                let _ = handle
                    .command_tx
                    .send(GargoPreviewCommand::UpdateBufferContent {
                        content,
                        cursor_line,
                    });
            }
        } else if cursor_line != self.last_pushed_cursor_line {
            // Only cursor moved — send lightweight scroll event
            self.last_pushed_cursor_line = cursor_line;
            self.last_content_push_at = Some(Instant::now());
            if let Some(handle) = &self.handle {
                let _ = handle
                    .command_tx
                    .send(GargoPreviewCommand::UpdateCursorLine { line: cursor_line });
            }
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
                self.refresh_active_preview();
                self.last_observed_file_sig = current_sig;
            }
            (None, sig) => {
                self.last_observed_file_sig = sig.clone();
            }
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
        self.sync_active_path(rel_path.clone());

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
            self.sync_active_path(rel_path.clone());
        }

        self.refresh_active_preview();
        self.refresh_file_signature_baseline(ctx);
        Vec::new()
    }
}

impl Plugin for GargoPreviewPlugin {
    fn id(&self) -> &str {
        "gargo_preview"
    }

    fn commands(&self) -> &[PluginCommandSpec] {
        &self.commands
    }

    fn on_command(&mut self, command_id: &str, _ctx: &PluginContext) -> Vec<PluginOutput> {
        let Some(handle) = &self.handle else {
            return vec![PluginOutput::Message(
                "Gargo preview plugin unavailable".to_string(),
            )];
        };
        // `server.*_github_preview` are pre-rename aliases for old keybindings.
        let result = match command_id {
            "server.start_gargo_preview" | "server.start_github_preview" => {
                handle.command_tx.send(GargoPreviewCommand::Start {
                    repo_root: self.project_root.clone(),
                })
            }
            "server.stop_gargo_preview" | "server.stop_github_preview" => {
                handle.command_tx.send(GargoPreviewCommand::Stop)
            }
            _ => return Vec::new(),
        };
        if result.is_err() {
            vec![PluginOutput::Message(
                "Failed to send Gargo preview command".to_string(),
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

        let mut drained_events = Vec::new();
        while let Ok(event) = handle.event_rx.try_recv() {
            drained_events.push(event);
        }

        let mut out = Vec::new();
        for event in drained_events {
            match event {
                GargoPreviewEvent::Started { port } => {
                    self.server_port = Some(port);
                    self.is_running = true;
                    self.is_detached = false;
                    let rel_path = self.active_rel_path(ctx);
                    self.last_active_rel_path = rel_path.clone();
                    self.sync_active_path(rel_path.clone());
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    self.refresh_file_signature_baseline(ctx);
                    let url = Self::preview_url(port, rel_path.as_deref());
                    out.push(PluginOutput::Message(format!("Gargo preview: {}", url)));
                    if self.auto_open_browser {
                        out.push(PluginOutput::OpenUrl(url));
                    }
                }
                GargoPreviewEvent::Stopped => {
                    self.server_port = None;
                    self.is_running = false;
                    self.is_detached = false;
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    out.push(PluginOutput::Message(
                        "Gargo preview server stopped".to_string(),
                    ));
                }
                GargoPreviewEvent::Detached { requested_path } => {
                    self.is_detached = true;
                    self.clear_external_change_probe_state();
                    self.clear_buffer_push_state();
                    out.push(PluginOutput::Message(format!(
                        "Gargo preview detached: {}",
                        requested_path
                    )));
                }
                GargoPreviewEvent::Error(msg) => {
                    out.push(PluginOutput::Message(format!("Server error: {}", msg)));
                }
            }
        }
        self.maybe_refresh_on_external_change(ctx);
        self.maybe_push_buffer_content(ctx);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Editor;
    use std::fs;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;
    use tempfile::tempdir;

    fn create_plugin_with_channels(
        project_root: &Path,
    ) -> (
        GargoPreviewPlugin,
        mpsc::Receiver<GargoPreviewCommand>,
        mpsc::Sender<GargoPreviewEvent>,
    ) {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let plugin = GargoPreviewPlugin {
            commands: vec![],
            handle: Some(GargoPreviewHandle::from_channels_for_test(
                command_tx, event_rx,
            )),
            project_root: project_root.to_path_buf(),
            auto_open_browser: true,
            server_port: None,
            is_running: false,
            is_detached: false,
            last_active_rel_path: None,
            last_observed_file_sig: None,
            last_file_probe_at: None,
            last_pushed_content_hash: 0,
            last_pushed_cursor_line: 0,
            last_content_push_at: None,
        };
        (plugin, command_rx, event_tx)
    }

    #[test]
    fn started_event_opens_active_document_and_syncs_active_path() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let file = root.join("note.md");
        fs::write(&file, "# note\n").expect("write file");

        let editor = Editor::open(&file.to_string_lossy());
        let config = Config::default();
        let ctx = PluginContext::new(&editor, root, &config);
        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);

        event_tx
            .send(GargoPreviewEvent::Started { port: 3101 })
            .expect("send started event");
        let outputs = plugin.poll(&ctx);

        assert!(outputs.iter().any(
            |output| matches!(output, PluginOutput::OpenUrl(url) if url == "http://127.0.0.1:3101/blob/note.md")
        ));
        assert!(
            matches!(
                command_rx.recv().expect("expected command"),
                GargoPreviewCommand::SetActivePath { rel_path } if rel_path == Some("note.md".to_string())
            ),
            "expected SetActivePath command for active file"
        );
    }

    #[test]
    fn detached_preview_reattaches_on_active_buffer_change_and_not_on_save() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let first = root.join("first.md");
        let second = root.join("second.md");
        fs::write(&first, "# first\n").expect("write first");
        fs::write(&second, "# second\n").expect("write second");

        let config = Config::default();

        let editor_first = Editor::open(&first.to_string_lossy());
        let ctx_first = PluginContext::new(&editor_first, root, &config);

        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);
        event_tx
            .send(GargoPreviewEvent::Started { port: 3102 })
            .expect("send started event");
        let _ = plugin.poll(&ctx_first);
        let _ = command_rx.recv();

        event_tx
            .send(GargoPreviewEvent::Detached {
                requested_path: "other.md".to_string(),
            })
            .expect("send detached event");
        let _ = plugin.poll(&ctx_first);

        let save_outputs = plugin.on_event(
            &PluginEvent::BufferSaved {
                doc_id: editor_first.active_buffer().id,
            },
            &ctx_first,
        );
        assert!(
            save_outputs.is_empty(),
            "save should not reopen while detached"
        );

        let editor_second = Editor::open(&second.to_string_lossy());
        let ctx_second = PluginContext::new(&editor_second, root, &config);
        let activate_outputs = plugin.on_event(
            &PluginEvent::BufferActivated {
                doc_id: editor_second.active_buffer().id,
            },
            &ctx_second,
        );
        assert!(activate_outputs.is_empty());
        assert!(matches!(
            command_rx.recv().expect("expected SetActivePath command"),
            GargoPreviewCommand::SetActivePath { rel_path }
                if rel_path == Some("second.md".to_string())
        ));
    }

    #[test]
    fn save_event_refreshes_active_preview_when_attached() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let file = root.join("doc.md");
        fs::write(&file, "# doc\n").expect("write file");

        let editor = Editor::open(&file.to_string_lossy());
        let config = Config::default();
        let ctx = PluginContext::new(&editor, root, &config);
        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);

        event_tx
            .send(GargoPreviewEvent::Started { port: 3103 })
            .expect("send started event");
        let _ = plugin.poll(&ctx);
        let _ = command_rx.recv();

        let outputs = plugin.on_event(
            &PluginEvent::BufferSaved {
                doc_id: editor.active_buffer().id,
            },
            &ctx,
        );
        assert!(outputs.is_empty());
        assert!(matches!(
            command_rx.recv().expect("expected refresh command"),
            GargoPreviewCommand::RefreshActive
        ));
    }

    #[test]
    fn external_file_change_refreshes_active_preview_when_attached() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let file = root.join("doc.md");
        fs::write(&file, "# doc\n").expect("write file");

        let editor = Editor::open(&file.to_string_lossy());
        let config = Config::default();
        let ctx = PluginContext::new(&editor, root, &config);
        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);

        event_tx
            .send(GargoPreviewEvent::Started { port: 3104 })
            .expect("send started event");
        let _ = plugin.poll(&ctx);
        let _ = command_rx.recv().expect("expected initial SetActivePath");

        fs::write(&file, "# doc\nupdated from external process\n").expect("external write");
        plugin.last_file_probe_at = None;
        let outputs = plugin.poll(&ctx);
        assert!(outputs.is_empty());

        assert!(matches!(
            command_rx
                .recv_timeout(Duration::from_millis(200))
                .expect("expected refresh command"),
            GargoPreviewCommand::RefreshActive
        ));
    }

    #[test]
    fn external_file_change_does_not_refresh_when_detached() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let file = root.join("doc.md");
        fs::write(&file, "# doc\n").expect("write file");

        let editor = Editor::open(&file.to_string_lossy());
        let config = Config::default();
        let ctx = PluginContext::new(&editor, root, &config);
        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);

        event_tx
            .send(GargoPreviewEvent::Started { port: 3105 })
            .expect("send started event");
        let _ = plugin.poll(&ctx);
        let _ = command_rx.recv().expect("expected initial SetActivePath");

        event_tx
            .send(GargoPreviewEvent::Detached {
                requested_path: "other.md".to_string(),
            })
            .expect("send detached event");
        let _ = plugin.poll(&ctx);

        fs::write(&file, "# doc\nupdated from external process\n").expect("external write");
        plugin.last_file_probe_at = None;
        let _ = plugin.poll(&ctx);

        assert!(
            matches!(
                command_rx.recv_timeout(Duration::from_millis(200)),
                Err(RecvTimeoutError::Timeout)
            ),
            "expected no refresh command while detached"
        );
    }

    #[test]
    fn save_event_updates_external_change_baseline() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let file = root.join("doc.md");
        fs::write(&file, "# doc\n").expect("write file");

        let editor = Editor::open(&file.to_string_lossy());
        let config = Config::default();
        let ctx = PluginContext::new(&editor, root, &config);
        let (mut plugin, command_rx, event_tx) = create_plugin_with_channels(root);

        event_tx
            .send(GargoPreviewEvent::Started { port: 3106 })
            .expect("send started event");
        let _ = plugin.poll(&ctx);
        let _ = command_rx.recv().expect("expected initial SetActivePath");

        let _ = plugin.on_event(
            &PluginEvent::BufferSaved {
                doc_id: editor.active_buffer().id,
            },
            &ctx,
        );
        let _ = command_rx
            .recv_timeout(Duration::from_millis(200))
            .expect("expected refresh command from save");

        plugin.last_file_probe_at = None;
        let _ = plugin.poll(&ctx);
        assert!(
            matches!(
                command_rx.recv_timeout(Duration::from_millis(200)),
                Err(RecvTimeoutError::Timeout)
            ),
            "expected no duplicate refresh command without file changes"
        );
    }
}

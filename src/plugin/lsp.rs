use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::command::lsp::{
    LspClientCommand, LspClientEvent, LspClientHandle, file_uri_to_path, path_to_file_uri,
};
use crate::config::{Config, LspServerConfig, LspStartMode};
use crate::core::document::DocumentId;
use crate::core::lsp_types::LspLocation;
use crate::core::markdown_link::{
    ResolvedTarget, bare_url_at_cursor, complete_link_target_at_cursor, resolve_link_target,
};
use crate::log::debug_log;
use crate::plugin::types::{
    LspPickerLocation, Plugin, PluginCommandSpec, PluginContext, PluginEvent, PluginOutput,
};

struct LspServerRuntime {
    command: String,
    args: Vec<String>,
    languages: HashSet<String>,
    handle: Option<LspClientHandle>,
    started: bool,
}

impl LspServerRuntime {
    fn new(cfg: &LspServerConfig) -> Self {
        Self {
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            languages: cfg
                .languages
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            handle: LspClientHandle::new().ok(),
            started: false,
        }
    }

    fn start(&mut self, project_root: &Path) -> bool {
        if self.started {
            return true;
        }
        let Some(handle) = &self.handle else {
            return false;
        };
        if handle
            .command_tx
            .send(LspClientCommand::Start {
                project_root: project_root.to_path_buf(),
                command: self.command.clone(),
                args: self.args.clone(),
            })
            .is_err()
        {
            return false;
        }
        self.started = true;
        true
    }

    fn restart(&mut self, project_root: &Path) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        if self.started {
            let _ = handle.command_tx.send(LspClientCommand::Stop);
        }
        if handle
            .command_tx
            .send(LspClientCommand::Start {
                project_root: project_root.to_path_buf(),
                command: self.command.clone(),
                args: self.args.clone(),
            })
            .is_err()
        {
            self.started = false;
            return false;
        }
        self.started = true;
        true
    }

    fn supports_language(&self, lang: &str) -> bool {
        self.languages.contains(&lang.to_ascii_lowercase())
    }
}

pub struct LspPlugin {
    commands: Vec<PluginCommandSpec>,
    servers: HashMap<String, LspServerRuntime>,
    doc_server: HashMap<DocumentId, String>,
    doc_versions: HashMap<DocumentId, i32>,
    dirty_docs: HashSet<DocumentId>,
}

fn is_rust_analyzer_command(command: &str) -> bool {
    let Some(name) = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    name.eq_ignore_ascii_case("rust-analyzer") || name.eq_ignore_ascii_case("rust-analyzer.exe")
}

fn command_uses_explicit_path(command: &str) -> bool {
    Path::new(command).components().count() > 1
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|meta| meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn command_exists_in_path(command: &str, path_env: Option<&OsStr>) -> bool {
    if command_uses_explicit_path(command) {
        return is_executable_file(Path::new(command));
    }
    let Some(path_env) = path_env else {
        return false;
    };

    for dir in std::env::split_paths(path_env) {
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            let candidate_exe = dir.join(format!("{command}.exe"));
            if is_executable_file(&candidate_exe) {
                return true;
            }
        }
    }
    false
}

fn has_rust_project_marker(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() || dir.join("rust-project.json").is_file()
}

fn is_rust_project_path(path: &Path, project_root: &Path) -> bool {
    if !path.starts_with(project_root) {
        return false;
    }

    let Some(parent) = path.parent() else {
        return false;
    };
    let mut current = parent.to_path_buf();
    loop {
        if has_rust_project_marker(&current) {
            return true;
        }
        if current == project_root {
            break;
        }
        let Some(next) = current.parent() else {
            break;
        };
        current = next.to_path_buf();
    }
    false
}

fn should_enable_server(server_cfg: &LspServerConfig, path_env: Option<&OsStr>) -> bool {
    if is_rust_analyzer_command(&server_cfg.command)
        && !command_uses_explicit_path(&server_cfg.command)
    {
        return command_exists_in_path(&server_cfg.command, path_env);
    }
    true
}

fn server_supports_rust(server_cfg: &LspServerConfig) -> bool {
    server_cfg
        .languages
        .iter()
        .any(|lang| lang.eq_ignore_ascii_case("rust"))
}

fn default_rust_analyzer_server() -> LspServerConfig {
    LspServerConfig {
        id: "rust-analyzer".to_string(),
        command: "rust-analyzer".to_string(),
        args: Vec::new(),
        languages: vec!["rust".to_string()],
        root: "project".to_string(),
    }
}

fn effective_server_configs(config: &Config) -> Vec<LspServerConfig> {
    let mut servers = config.lsp.servers.clone();
    if !servers.iter().any(server_supports_rust) {
        servers.push(default_rust_analyzer_server());
    }
    servers
}

impl LspPlugin {
    pub fn new(config: &Config, project_root: &Path) -> Self {
        let mut servers = HashMap::new();
        let start_mode = config.performance.lsp.start_mode;
        let path_env = std::env::var_os("PATH");
        let mut server_cfgs = effective_server_configs(config);
        if server_cfgs.len() != config.lsp.servers.len() {
            debug_log!(
                config,
                "lsp: injected server id=rust-analyzer command=rust-analyzer reason=no-rust-server-configured"
            );
        }
        let configured_servers: Vec<String> = server_cfgs
            .iter()
            .map(|s| format!("{}:{}", s.id, s.command))
            .collect();
        debug_log!(
            config,
            "lsp: configured-servers count={} entries={:?}",
            configured_servers.len(),
            configured_servers
        );
        for server_cfg in server_cfgs.drain(..) {
            if !should_enable_server(&server_cfg, path_env.as_deref()) {
                debug_log!(
                    config,
                    "lsp: skipping server id={} command={} reason=rust-analyzer-not-in-path",
                    server_cfg.id,
                    server_cfg.command
                );
                continue;
            }
            let mut runtime = LspServerRuntime::new(&server_cfg);
            if runtime.handle.is_none() {
                debug_log!(
                    config,
                    "lsp: start unavailable id={} command={} reason=worker-init-failed",
                    server_cfg.id,
                    server_cfg.command
                );
            } else if start_mode == LspStartMode::Eager {
                if runtime.start(project_root) {
                    debug_log!(
                        config,
                        "lsp: eager start requested id={} command={} args={:?}",
                        server_cfg.id,
                        server_cfg.command,
                        server_cfg.args
                    );
                } else {
                    debug_log!(
                        config,
                        "lsp: eager start request failed id={} command={}",
                        server_cfg.id,
                        server_cfg.command
                    );
                }
            } else {
                debug_log!(
                    config,
                    "lsp: deferred start id={} command={} mode=on_demand",
                    server_cfg.id,
                    server_cfg.command
                );
            }
            servers.insert(server_cfg.id.clone(), runtime);
        }
        let enabled_servers: Vec<String> = servers
            .iter()
            .map(|(id, s)| format!("{id}:{}", s.command))
            .collect();
        debug_log!(
            config,
            "lsp: enabled-servers count={} entries={:?}",
            enabled_servers.len(),
            enabled_servers
        );
        if !servers.values().any(|s| s.supports_language("rust")) {
            debug_log!(
                config,
                "lsp: rust-server-unavailable after enable filtering"
            );
        }
        Self {
            commands: vec![
                PluginCommandSpec {
                    id: "lsp.hover".to_string(),
                    label: "LSP: Hover".to_string(),
                    category: Some("LSP".to_string()),
                },
                PluginCommandSpec {
                    id: "lsp.goto_definition".to_string(),
                    label: "LSP: Go to Definition".to_string(),
                    category: Some("LSP".to_string()),
                },
                PluginCommandSpec {
                    id: "lsp.find_references".to_string(),
                    label: "LSP: Find References".to_string(),
                    category: Some("LSP".to_string()),
                },
                PluginCommandSpec {
                    id: "lsp.restart".to_string(),
                    label: "LSP: Restart".to_string(),
                    category: Some("LSP".to_string()),
                },
            ],
            servers,
            doc_server: HashMap::new(),
            doc_versions: HashMap::new(),
            dirty_docs: HashSet::new(),
        }
    }

    fn detect_language_id(path: &Path, ctx: &PluginContext) -> Option<String> {
        let path_str = path.to_string_lossy();
        let lang = ctx
            .editor()
            .language_registry
            .detect_by_extension(&path_str)?;
        Some(lang.name.to_ascii_lowercase().replace(' ', ""))
    }

    fn select_server_for_doc(&self, doc_id: DocumentId, ctx: &PluginContext) -> Option<String> {
        let doc = ctx.document(doc_id)?;
        let path = doc.file_path.as_ref()?;
        let language_id = Self::detect_language_id(path, ctx)?;
        self.servers
            .iter()
            .find(|(_, server)| {
                server.supports_language(&language_id)
                    && (!is_rust_analyzer_command(&server.command)
                        || is_rust_project_path(path, ctx.project_root()))
            })
            .map(|(id, _)| id.clone())
    }

    fn ensure_server_started(&mut self, server_id: &str, ctx: &PluginContext) -> bool {
        let Some(server) = self.servers.get_mut(server_id) else {
            return false;
        };
        if server.started {
            return true;
        }
        if server.start(ctx.project_root()) {
            debug_log!(
                ctx.config(),
                "lsp: on-demand start requested id={} command={} args={:?}",
                server_id,
                server.command,
                server.args
            );
            true
        } else {
            debug_log!(
                ctx.config(),
                "lsp: on-demand start unavailable id={} command={}",
                server_id,
                server.command
            );
            false
        }
    }

    fn doc_debug_info(doc_id: DocumentId, ctx: &PluginContext) -> (String, String, bool) {
        let Some(doc) = ctx.document(doc_id) else {
            return ("<missing-doc>".to_string(), "<unknown>".to_string(), false);
        };
        let Some(path) = doc.file_path.as_ref() else {
            return ("<scratch>".to_string(), "<unknown>".to_string(), false);
        };
        let language =
            Self::detect_language_id(path, ctx).unwrap_or_else(|| "<unknown>".to_string());
        let rust_project = language == "rust" && is_rust_project_path(path, ctx.project_root());
        (path.display().to_string(), language, rust_project)
    }

    fn sync_doc(&mut self, doc_id: DocumentId, ctx: &PluginContext) {
        let Some(doc) = ctx.document(doc_id) else {
            return;
        };
        let Some(path) = doc.file_path.as_ref() else {
            return;
        };
        let Some(uri) = path_to_file_uri(path) else {
            return;
        };
        let Some(language_id) = Self::detect_language_id(path, ctx) else {
            return;
        };
        let Some(server_id) = self.doc_server.get(&doc_id).cloned() else {
            return;
        };
        if !self.ensure_server_started(&server_id, ctx) {
            return;
        }
        let Some(server) = self.servers.get(&server_id) else {
            return;
        };
        let Some(handle) = &server.handle else {
            return;
        };
        let next_version = self.doc_versions.get(&doc_id).copied().unwrap_or(0) + 1;
        self.doc_versions.insert(doc_id, next_version);
        let _ = handle.command_tx.send(LspClientCommand::SyncFull {
            uri,
            language_id,
            version: next_version,
            text: doc.rope.to_string(),
        });
    }

    fn builtin_markdown_definition(
        &self,
        active_doc: &crate::core::document::Document,
        project_root: &Path,
    ) -> Option<Vec<PluginOutput>> {
        let path = active_doc.file_path.as_ref()?;
        let is_markdown = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
            });
        if !is_markdown {
            return None;
        }

        let resolved = if let Some(target) = complete_link_target_at_cursor(active_doc) {
            resolve_link_target(&target.target, path, project_root)?
        } else if let Some(url) = bare_url_at_cursor(active_doc) {
            ResolvedTarget::Url(url)
        } else {
            return None;
        };
        let outputs = match resolved {
            ResolvedTarget::Url(url) => vec![PluginOutput::OpenUrl(url)],
            // Open the link target as a buffer whether it exists on disk yet
            // or not — saving the buffer will mkdir -p its parent and create
            // the file, which lets the user follow a stub link into a fresh
            // note without having to create the path by hand first.
            ResolvedTarget::LocalPath(path) => vec![PluginOutput::OpenFileAtLsp {
                path,
                line: 0,
                character_utf16: 0,
            }],
        };
        Some(outputs)
    }

    fn references_outputs(locations: &[LspLocation]) -> Vec<PluginOutput> {
        let mut deduped = Vec::new();
        let mut seen: HashSet<(PathBuf, usize, usize)> = HashSet::new();

        for location in locations {
            let Some(path) = file_uri_to_path(&location.uri) else {
                continue;
            };
            if seen.insert((path.clone(), location.line, location.character_utf16)) {
                deduped.push(LspPickerLocation {
                    path,
                    line: location.line,
                    character_utf16: location.character_utf16,
                });
            }
        }

        match deduped.len() {
            0 => vec![PluginOutput::Message("No references found".to_string())],
            1 => {
                let location = &deduped[0];
                vec![PluginOutput::OpenFileAtLsp {
                    path: location.path.clone(),
                    line: location.line,
                    character_utf16: location.character_utf16,
                }]
            }
            _ => vec![PluginOutput::OpenLspReferencesPicker {
                caller_label: "LSP: Find References".to_string(),
                locations: deduped,
            }],
        }
    }
}

impl Plugin for LspPlugin {
    fn id(&self) -> &str {
        "lsp"
    }

    fn commands(&self) -> &[PluginCommandSpec] {
        &self.commands
    }

    fn on_command(&mut self, command_id: &str, ctx: &PluginContext) -> Vec<PluginOutput> {
        let active_doc = ctx.editor().active_buffer();
        let active_doc_id = active_doc.id;
        let (file_label, language_id, rust_project) = Self::doc_debug_info(active_doc_id, ctx);
        debug_log!(
            ctx.config(),
            "lsp: command={} file={} language={} rust_project={}",
            command_id,
            file_label,
            language_id,
            rust_project
        );

        if command_id == "lsp.goto_definition"
            && let Some(outputs) = self.builtin_markdown_definition(active_doc, ctx.project_root())
        {
            debug_log!(
                ctx.config(),
                "lsp: command={} resolved by builtin markdown handler",
                command_id
            );
            return outputs;
        }

        let Some(server_id) = self.doc_server.get(&active_doc_id).cloned() else {
            debug_log!(
                ctx.config(),
                "lsp: command={} no-server file={} language={} rust_project={}",
                command_id,
                file_label,
                language_id,
                rust_project
            );
            return vec![PluginOutput::Message(
                "LSP is not configured for this file".to_string(),
            )];
        };
        if command_id == "lsp.restart" {
            let Some(server) = self.servers.get_mut(&server_id) else {
                debug_log!(
                    ctx.config(),
                    "lsp: command={} server-missing id={}",
                    command_id,
                    server_id
                );
                return vec![PluginOutput::Message("LSP server not found".to_string())];
            };
            debug_log!(
                ctx.config(),
                "lsp: restart requested server_id={} command={}",
                server_id,
                server.command
            );
            if !server.restart(ctx.project_root()) {
                debug_log!(
                    ctx.config(),
                    "lsp: restart failed server_id={} command={}",
                    server_id,
                    server.command
                );
                return vec![PluginOutput::Message("LSP server unavailable".to_string())];
            }
            return Vec::new();
        }

        if !self.ensure_server_started(&server_id, ctx) {
            let command = self
                .servers
                .get(&server_id)
                .map(|s| s.command.as_str())
                .unwrap_or("<unknown>");
            debug_log!(
                ctx.config(),
                "lsp: command={} server-unavailable id={} command={}",
                command_id,
                server_id,
                command
            );
            return vec![PluginOutput::Message("LSP server unavailable".to_string())];
        }
        let Some(server) = self.servers.get(&server_id) else {
            debug_log!(
                ctx.config(),
                "lsp: command={} server-missing id={}",
                command_id,
                server_id
            );
            return vec![PluginOutput::Message("LSP server not found".to_string())];
        };
        let Some(handle) = &server.handle else {
            debug_log!(
                ctx.config(),
                "lsp: command={} server-unavailable id={} command={}",
                command_id,
                server_id,
                server.command
            );
            return vec![PluginOutput::Message("LSP server unavailable".to_string())];
        };
        let Some(path) = active_doc.file_path.as_ref() else {
            debug_log!(ctx.config(), "lsp: command={} no-file-path", command_id);
            return vec![PluginOutput::Message("No file path".to_string())];
        };
        let Some(uri) = path_to_file_uri(path) else {
            debug_log!(
                ctx.config(),
                "lsp: command={} failed-to-build-uri path={}",
                command_id,
                path.display()
            );
            return vec![PluginOutput::Message(
                "Failed to create file URI".to_string(),
            )];
        };
        let (line, character_utf16) = active_doc.cursor_position_utf16();
        debug_log!(
            ctx.config(),
            "lsp: dispatch command={} server_id={} server_command={} language={} line={} char_utf16={}",
            command_id,
            server_id,
            server.command,
            language_id,
            line,
            character_utf16
        );

        let send_result = match command_id {
            "lsp.hover" => handle.command_tx.send(LspClientCommand::RequestHover {
                uri,
                line,
                character_utf16,
            }),
            "lsp.goto_definition" => handle.command_tx.send(LspClientCommand::RequestDefinition {
                uri,
                line,
                character_utf16,
            }),
            "lsp.find_references" => handle.command_tx.send(LspClientCommand::RequestReferences {
                uri,
                line,
                character_utf16,
            }),
            _ => return Vec::new(),
        };
        if send_result.is_err() {
            debug_log!(
                ctx.config(),
                "lsp: send failed command={} server_id={} server_command={}",
                command_id,
                server_id,
                server.command
            );
            vec![PluginOutput::Message(
                "Failed to send LSP command".to_string(),
            )]
        } else {
            debug_log!(
                ctx.config(),
                "lsp: send ok command={} server_id={}",
                command_id,
                server_id
            );
            Vec::new()
        }
    }

    fn on_event(&mut self, event: &PluginEvent, ctx: &PluginContext) -> Vec<PluginOutput> {
        match event {
            PluginEvent::BufferActivated { doc_id } => {
                if let Some(server_id) = self.select_server_for_doc(*doc_id, ctx) {
                    self.doc_server.insert(*doc_id, server_id.clone());
                    let _ = self.ensure_server_started(&server_id, ctx);
                    self.dirty_docs.insert(*doc_id);
                    let (file, language, rust_project) = Self::doc_debug_info(*doc_id, ctx);
                    if let Some(selected_server) = self.doc_server.get(doc_id)
                        && let Some(runtime) = self.servers.get(selected_server)
                    {
                        debug_log!(
                            ctx.config(),
                            "lsp: buffer-activated file={} language={} rust_project={} server_id={} server_command={}",
                            file,
                            language,
                            rust_project,
                            selected_server,
                            runtime.command
                        );
                    }
                } else {
                    self.doc_server.remove(doc_id);
                    let (file, language, rust_project) = Self::doc_debug_info(*doc_id, ctx);
                    debug_log!(
                        ctx.config(),
                        "lsp: buffer-activated file={} language={} rust_project={} server_id=<none>",
                        file,
                        language,
                        rust_project
                    );
                }
            }
            PluginEvent::BufferChanged { doc_id } => {
                if !self.doc_server.contains_key(doc_id)
                    && let Some(server_id) = self.select_server_for_doc(*doc_id, ctx)
                {
                    self.doc_server.insert(*doc_id, server_id);
                    let (file, language, rust_project) = Self::doc_debug_info(*doc_id, ctx);
                    if let Some(selected_server) = self.doc_server.get(doc_id)
                        && let Some(runtime) = self.servers.get(selected_server)
                    {
                        debug_log!(
                            ctx.config(),
                            "lsp: buffer-changed file={} language={} rust_project={} server_id={} server_command={}",
                            file,
                            language,
                            rust_project,
                            selected_server,
                            runtime.command
                        );
                    }
                }
                if let Some(server_id) = self.doc_server.get(doc_id).cloned() {
                    let _ = self.ensure_server_started(&server_id, ctx);
                    self.dirty_docs.insert(*doc_id);
                }
            }
            PluginEvent::BufferSaved { doc_id } => {
                if let (Some(doc), Some(server_id)) =
                    (ctx.document(*doc_id), self.doc_server.get(doc_id))
                    && let (Some(path), Some(server)) =
                        (doc.file_path.as_ref(), self.servers.get(server_id))
                    && let (Some(uri), Some(handle)) =
                        (path_to_file_uri(path), server.handle.as_ref())
                    && server.started
                {
                    let _ = handle.command_tx.send(LspClientCommand::DidSave {
                        uri,
                        text: doc.rope.to_string(),
                    });
                }
            }
            PluginEvent::BufferClosed { doc_id, path } => {
                if let Some(server_id) = self.doc_server.remove(doc_id)
                    && let (Some(path), Some(server)) = (path, self.servers.get(&server_id))
                    && let (Some(uri), Some(handle)) =
                        (path_to_file_uri(path), server.handle.as_ref())
                    && server.started
                {
                    let _ = handle.command_tx.send(LspClientCommand::DidClose { uri });
                }
                self.dirty_docs.remove(doc_id);
                self.doc_versions.remove(doc_id);
            }
            PluginEvent::Tick => {}
        }
        Vec::new()
    }

    fn poll(&mut self, ctx: &PluginContext) -> Vec<PluginOutput> {
        let mut out = Vec::new();
        let dirty: Vec<DocumentId> = self.dirty_docs.iter().copied().collect();
        for doc_id in dirty {
            self.sync_doc(doc_id, ctx);
            self.dirty_docs.remove(&doc_id);
        }

        for (server_id, server) in self.servers.iter_mut() {
            let Some(handle) = &server.handle else {
                continue;
            };
            while let Ok(event) = handle.event_rx.try_recv() {
                match event {
                    LspClientEvent::Started => {
                        server.started = true;
                        debug_log!(
                            ctx.config(),
                            "lsp: started server_id={} command={} args={:?}",
                            server_id,
                            server.command,
                            server.args
                        );
                        out.push(PluginOutput::Message(format!(
                            "LSP started ({})",
                            server_id
                        )));
                    }
                    LspClientEvent::Stopped => {
                        server.started = false;
                        debug_log!(
                            ctx.config(),
                            "lsp: stopped server_id={} command={}",
                            server_id,
                            server.command
                        );
                        out.push(PluginOutput::Message(format!(
                            "LSP stopped ({})",
                            server_id
                        )));
                    }
                    LspClientEvent::PublishDiagnostics { uri, diagnostics } => {
                        debug_log!(
                            ctx.config(),
                            "lsp: diagnostics server_id={} count={} uri={}",
                            server_id,
                            diagnostics.len(),
                            uri
                        );
                        if let Some(path) = file_uri_to_path(&uri) {
                            out.push(PluginOutput::SetDiagnostics { path, diagnostics });
                        }
                    }
                    LspClientEvent::HoverResult { contents } => {
                        let trimmed = contents.lines().next().unwrap_or("").trim().to_string();
                        if trimmed.is_empty() {
                            out.push(PluginOutput::Message("No hover information".to_string()));
                        } else {
                            out.push(PluginOutput::Message(trimmed));
                        }
                    }
                    LspClientEvent::DefinitionResult { locations } => {
                        debug_log!(
                            ctx.config(),
                            "lsp: definition-result server_id={} count={}",
                            server_id,
                            locations.len()
                        );
                        if let Some(first) = locations.first() {
                            if let Some(path) = file_uri_to_path(&first.uri) {
                                out.push(PluginOutput::OpenFileAtLsp {
                                    path,
                                    line: first.line,
                                    character_utf16: first.character_utf16,
                                });
                                if locations.len() > 1 {
                                    out.push(PluginOutput::Message(format!(
                                        "{} locations found; jumped to first",
                                        locations.len()
                                    )));
                                }
                            }
                        } else {
                            out.push(PluginOutput::Message("Definition not found".to_string()));
                        }
                    }
                    LspClientEvent::ReferencesResult { locations } => {
                        debug_log!(
                            ctx.config(),
                            "lsp: references-result server_id={} count={}",
                            server_id,
                            locations.len()
                        );
                        out.extend(Self::references_outputs(&locations));
                    }
                    LspClientEvent::Error(msg) => {
                        if msg.contains("Failed to start LSP command")
                            || msg.contains("LSP process is not running")
                        {
                            server.started = false;
                        }
                        debug_log!(
                            ctx.config(),
                            "lsp: error server_id={} command={} message={}",
                            server_id,
                            server.command,
                            msg
                        );
                        out.push(PluginOutput::Message(format!(
                            "LSP error ({}): {}",
                            server_id, msg
                        )));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Editor;
    use crate::plugin::types::Plugin;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn config_without_lsp_servers() -> Config {
        let mut config = Config::default();
        config.lsp.servers.clear();
        config
    }

    fn char_idx(text: &str, needle: &str) -> usize {
        let byte = text.find(needle).expect("needle must exist");
        text[..byte].chars().count()
    }

    #[test]
    fn goto_definition_opens_local_markdown_link_without_external_lsp() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        let target_path = root.join("target.md");
        let content = "[x](target.md)\n";
        fs::write(&doc_path, content).expect("write doc");
        fs::write(&target_path, "target").expect("write target");

        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.active_buffer_mut().cursors[0] = char_idx(content, "target");

        let config = config_without_lsp_servers();
        let mut plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);
        let outputs = plugin.on_command("lsp.goto_definition", &ctx);

        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenFileAtLsp { path, .. }) if path == &target_path
        ));
    }

    #[test]
    fn goto_definition_opens_missing_markdown_target_as_new_buffer() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        let missing_path = root.join("notes/draft.md");
        let content = "[x](notes/draft.md)\n";
        fs::write(&doc_path, content).expect("write doc");

        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.active_buffer_mut().cursors[0] = char_idx(content, "draft");

        let config = config_without_lsp_servers();
        let mut plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);
        let outputs = plugin.on_command("lsp.goto_definition", &ctx);

        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenFileAtLsp { path, .. }) if path == &missing_path
        ));
    }

    #[test]
    fn goto_definition_opens_web_markdown_link_without_external_lsp() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        let content = "[x](https://example.com/docs)\n";
        fs::write(&doc_path, content).expect("write doc");

        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.active_buffer_mut().cursors[0] = char_idx(content, "https");

        let config = config_without_lsp_servers();
        let mut plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);
        let outputs = plugin.on_command("lsp.goto_definition", &ctx);

        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenUrl(url)) if url == "https://example.com/docs"
        ));
    }

    #[test]
    fn goto_definition_opens_bare_url_in_markdown() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        let content = "- https://www.google.com/\n";
        fs::write(&doc_path, content).expect("write doc");

        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.active_buffer_mut().cursors[0] = char_idx(content, "google");

        let config = config_without_lsp_servers();
        let mut plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);
        let outputs = plugin.on_command("lsp.goto_definition", &ctx);

        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenUrl(url)) if url == "https://www.google.com/"
        ));
    }

    #[test]
    fn goto_definition_outside_markdown_link_falls_back_to_external_lsp_path() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        let content = "prefix [x](target.md)\n";
        fs::write(&doc_path, content).expect("write doc");

        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.active_buffer_mut().cursors[0] = char_idx(content, "prefix");

        let config = config_without_lsp_servers();
        let mut plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);
        let outputs = plugin.on_command("lsp.goto_definition", &ctx);

        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::Message(msg)) if msg == "LSP is not configured for this file"
        ));
    }

    #[test]
    fn default_config_routes_rust_to_rust_analyzer_in_rust_project() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"demo\"\nversion=\"0.1.0\"\n",
        )
        .expect("write cargo toml");
        let doc_path = root.join("main.rs");
        fs::write(&doc_path, "fn main() {}\n").expect("write doc");

        let editor = Editor::open(&doc_path.to_string_lossy());
        let mut config = Config::default();
        config.lsp.servers = vec![LspServerConfig {
            id: "rust-analyzer".to_string(),
            command: root.join("rust-analyzer").to_string_lossy().to_string(),
            args: Vec::new(),
            languages: vec!["rust".to_string()],
            root: "project".to_string(),
        }];
        let plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);

        let server = plugin.select_server_for_doc(editor.active_buffer().id, &ctx);
        assert_eq!(server.as_deref(), Some("rust-analyzer"));
    }

    #[test]
    fn rust_analyzer_not_selected_for_rust_file_outside_rust_project() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("main.rs");
        fs::write(&doc_path, "fn main() {}\n").expect("write doc");

        let editor = Editor::open(&doc_path.to_string_lossy());
        let mut config = Config::default();
        config.lsp.servers = vec![LspServerConfig {
            id: "rust-analyzer".to_string(),
            command: root.join("rust-analyzer").to_string_lossy().to_string(),
            args: Vec::new(),
            languages: vec!["rust".to_string()],
            root: "project".to_string(),
        }];
        let plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);

        let server = plugin.select_server_for_doc(editor.active_buffer().id, &ctx);
        assert_eq!(server, None);
    }

    #[test]
    fn command_exists_in_path_detects_bare_binary_on_path() {
        let tmp = tempdir().expect("temp dir");
        let dir = tmp.path().join("bin");
        fs::create_dir_all(&dir).expect("create bin dir");
        let bin = dir.join("rust-analyzer");
        fs::write(&bin, "#!/bin/sh\n").expect("write binary");
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&bin).expect("stat").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&bin, perms).expect("chmod");
        }

        let found = command_exists_in_path("rust-analyzer", Some(dir.as_os_str()));
        assert!(found);
    }

    #[test]
    fn command_exists_in_path_is_false_when_missing() {
        let tmp = tempdir().expect("temp dir");
        let found = command_exists_in_path("rust-analyzer", Some(tmp.path().as_os_str()));
        assert!(!found);
    }

    #[test]
    fn should_enable_server_disables_rust_analyzer_when_not_on_path() {
        let server = LspServerConfig {
            id: "rust-analyzer".to_string(),
            command: "rust-analyzer".to_string(),
            args: Vec::new(),
            languages: vec!["rust".to_string()],
            root: "project".to_string(),
        };
        assert!(!should_enable_server(&server, None));
    }

    #[test]
    fn should_enable_server_keeps_non_rust_server_without_path() {
        let server = LspServerConfig {
            id: "marksman".to_string(),
            command: "marksman".to_string(),
            args: vec!["server".to_string()],
            languages: vec!["markdown".to_string()],
            root: "project".to_string(),
        };
        assert!(should_enable_server(&server, None));
    }

    #[test]
    fn effective_server_configs_injects_rust_analyzer_when_missing() {
        let mut config = Config::default();
        config.lsp.servers = vec![LspServerConfig {
            id: "marksman".to_string(),
            command: "marksman".to_string(),
            args: vec!["server".to_string()],
            languages: vec!["markdown".to_string()],
            root: "project".to_string(),
        }];

        let effective = effective_server_configs(&config);
        assert!(effective.iter().any(|s| s.id == "marksman"));
        assert!(
            effective
                .iter()
                .any(|s| s.id == "rust-analyzer" && s.command == "rust-analyzer")
        );
    }

    #[test]
    fn effective_server_configs_does_not_duplicate_existing_rust_server() {
        let config = Config::default();
        let effective = effective_server_configs(&config);
        let rust_count = effective.iter().filter(|s| s.id == "rust-analyzer").count();
        assert_eq!(rust_count, 1);
    }

    #[test]
    fn default_config_routes_markdown_to_marksman() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        fs::write(&doc_path, "# hello\n").expect("write doc");

        let editor = Editor::open(&doc_path.to_string_lossy());
        let config = Config::default();
        let plugin = LspPlugin::new(&config, root);
        let ctx = PluginContext::new(&editor, root, &config);

        let server = plugin.select_server_for_doc(editor.active_buffer().id, &ctx);
        assert_eq!(server.as_deref(), Some("marksman"));
    }

    #[test]
    fn on_demand_mode_keeps_servers_stopped_until_needed() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();
        let doc_path = root.join("note.md");
        fs::write(&doc_path, "# hello\n").expect("write doc");

        let mut config = Config::default();
        config.performance.lsp.start_mode = LspStartMode::OnDemand;
        config.lsp.servers = vec![LspServerConfig {
            id: "md".to_string(),
            command: "marksman".to_string(),
            args: vec!["server".to_string()],
            languages: vec!["markdown".to_string()],
            root: "project".to_string(),
        }];

        let editor = Editor::open(&doc_path.to_string_lossy());
        let mut plugin = LspPlugin::new(&config, root);
        assert!(
            plugin.servers.values().all(|runtime| !runtime.started),
            "on_demand should not start any server in constructor"
        );

        let ctx = PluginContext::new(&editor, root, &config);
        let _ = plugin.on_event(
            &PluginEvent::BufferActivated {
                doc_id: editor.active_buffer().id,
            },
            &ctx,
        );

        let started = plugin
            .servers
            .get("md")
            .expect("markdown server is configured")
            .started;
        assert!(
            started,
            "server should start when matching buffer activates"
        );
    }

    #[test]
    fn eager_mode_requests_server_start_in_constructor() {
        let tmp = tempdir().expect("temp dir");
        let root = tmp.path();

        let mut config = Config::default();
        config.performance.lsp.start_mode = LspStartMode::Eager;
        config.lsp.servers = vec![LspServerConfig {
            id: "dummy".to_string(),
            command: "this-command-should-not-exist-gargo".to_string(),
            args: Vec::new(),
            languages: vec!["markdown".to_string()],
            root: "project".to_string(),
        }];

        let plugin = LspPlugin::new(&config, root);
        let started = plugin
            .servers
            .get("dummy")
            .expect("dummy server should be registered")
            .started;
        assert!(
            started,
            "eager mode should request start during constructor"
        );
    }

    #[test]
    fn references_with_no_locations_show_no_references_message() {
        let outputs = LspPlugin::references_outputs(&[]);
        assert_eq!(outputs.len(), 1);
        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::Message(msg)) if msg == "No references found"
        ));
    }

    #[test]
    fn references_with_single_location_open_file_directly() {
        let tmp = tempdir().expect("temp dir");
        let path = tmp.path().join("main.rs");
        fs::write(&path, "fn main() {}\n").expect("write file");
        let expected_path = fs::canonicalize(&path).expect("canonical path");
        let uri = path_to_file_uri(&path).expect("file uri");

        let outputs = LspPlugin::references_outputs(&[LspLocation {
            uri,
            line: 4,
            character_utf16: 9,
        }]);

        assert_eq!(outputs.len(), 1);
        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenFileAtLsp {
                path: output_path,
                line: 4,
                character_utf16: 9
            }) if output_path == &expected_path
        ));
    }

    #[test]
    fn references_with_multiple_locations_open_picker() {
        let tmp = tempdir().expect("temp dir");
        let path_a = tmp.path().join("a.rs");
        let path_b = tmp.path().join("b.rs");
        fs::write(&path_a, "fn a() {}\n").expect("write file");
        fs::write(&path_b, "fn b() {}\n").expect("write file");
        let expected_a = fs::canonicalize(&path_a).expect("canonical path");
        let expected_b = fs::canonicalize(&path_b).expect("canonical path");

        let outputs = LspPlugin::references_outputs(&[
            LspLocation {
                uri: path_to_file_uri(&path_a).expect("file uri"),
                line: 1,
                character_utf16: 2,
            },
            LspLocation {
                uri: path_to_file_uri(&path_b).expect("file uri"),
                line: 3,
                character_utf16: 4,
            },
        ]);

        assert_eq!(outputs.len(), 1);
        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenLspReferencesPicker {
                caller_label,
                locations
            }) if caller_label == "LSP: Find References"
                && locations.len() == 2
                && locations[0].path == expected_a
                && locations[0].line == 1
                && locations[0].character_utf16 == 2
                && locations[1].path == expected_b
                && locations[1].line == 3
                && locations[1].character_utf16 == 4
        ));
    }

    #[test]
    fn references_filter_invalid_uris_and_stably_dedupe() {
        let tmp = tempdir().expect("temp dir");
        let path = tmp.path().join("same.rs");
        fs::write(&path, "fn same() {}\n").expect("write file");
        let expected_path = fs::canonicalize(&path).expect("canonical path");
        let uri = path_to_file_uri(&path).expect("file uri");

        let outputs = LspPlugin::references_outputs(&[
            LspLocation {
                uri: "https://example.com/not-a-file.rs".to_string(),
                line: 0,
                character_utf16: 0,
            },
            LspLocation {
                uri: uri.clone(),
                line: 2,
                character_utf16: 6,
            },
            LspLocation {
                uri,
                line: 2,
                character_utf16: 6,
            },
        ]);

        assert_eq!(outputs.len(), 1);
        assert!(matches!(
            outputs.first(),
            Some(PluginOutput::OpenFileAtLsp {
                path: output_path,
                line: 2,
                character_utf16: 6
            }) if output_path == &expected_path
        ));
    }
}

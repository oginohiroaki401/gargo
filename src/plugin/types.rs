use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::core::document::{Document, DocumentId};
use crate::core::editor::Editor;
use crate::core::lsp_types::LspDiagnostic;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommandSpec {
    pub id: String,
    pub label: String,
    pub category: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspPickerLocation {
    pub path: PathBuf,
    pub line: usize,
    pub character_utf16: usize,
}

#[derive(Debug, Clone)]
pub enum PluginOutput {
    Message(String),
    OpenUrl(String),
    OpenFileAtLsp {
        path: PathBuf,
        line: usize,
        character_utf16: usize,
    },
    OpenLspReferencesPicker {
        caller_label: String,
        locations: Vec<LspPickerLocation>,
    },
    SetDiagnostics {
        path: PathBuf,
        diagnostics: Vec<LspDiagnostic>,
    },
    ClearDiagnostics {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginEvent {
    Tick,
    BufferActivated {
        doc_id: DocumentId,
    },
    BufferChanged {
        doc_id: DocumentId,
    },
    BufferSaved {
        doc_id: DocumentId,
    },
    BufferClosed {
        doc_id: DocumentId,
        path: Option<PathBuf>,
    },
}

pub struct PluginContext<'a> {
    editor: &'a Editor,
    project_root: &'a Path,
    config: &'a Config,
}

impl<'a> PluginContext<'a> {
    pub fn new(editor: &'a Editor, project_root: &'a Path, config: &'a Config) -> Self {
        Self {
            editor,
            project_root,
            config,
        }
    }

    pub fn editor(&self) -> &Editor {
        self.editor
    }

    pub fn project_root(&self) -> &Path {
        self.project_root
    }

    pub fn config(&self) -> &Config {
        self.config
    }

    pub fn document(&self, doc_id: DocumentId) -> Option<&Document> {
        self.editor.buffers().iter().find(|d| d.id == doc_id)
    }
}

pub trait Plugin: Send {
    fn id(&self) -> &str;
    fn commands(&self) -> &[PluginCommandSpec];
    /// Ids of commands that should be hidden from the command palette given the
    /// plugin's current runtime state. Registered commands stay runnable (so old
    /// keybindings keep working); they are merely filtered out of the picker.
    fn hidden_command_ids(&self) -> Vec<String> {
        Vec::new()
    }
    fn on_command(&mut self, command_id: &str, ctx: &PluginContext) -> Vec<PluginOutput>;
    fn on_event(&mut self, event: &PluginEvent, ctx: &PluginContext) -> Vec<PluginOutput>;
    fn poll(&mut self, ctx: &PluginContext) -> Vec<PluginOutput>;
}

mod buffer_manager;
mod diagnostics;
mod jump_list;
pub mod search;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use crate::command::git::{GitLineStatus, git_diff_line_status};
use crate::core::buffer::BufferId;
use crate::core::document::{Document, DocumentId};
use crate::core::dot_rec::DotRecorder;
use crate::core::lsp_types::{LspDiagnostic, LspSeverity};
use crate::core::macro_rec::MacroRecorder;
use crate::core::mode::Mode;
use crate::syntax::highlight::HighlightManager;
use crate::syntax::indent;
use crate::syntax::language::LanguageRegistry;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpLocation {
    pub doc_id: DocumentId,
    pub file_path: Option<PathBuf>,
    pub cursor: usize,
    pub line: usize,
    pub char_col: usize,
}

pub struct SearchState {
    pub pattern: String,
    /// Lowercased copy of `pattern`, cached so render-time match scanning
    /// doesn't lowercase it per-frame.
    pub pattern_lower: String,
    /// Primary cursor at the moment `/` was opened. Subsequent keystrokes
    /// search forward from here so adding/removing characters doesn't drift
    /// the result through the buffer (matches vim/Helix semantics).
    pub anchor: usize,
    /// Set when the most recent `search_update` found no match. Drives the
    /// "Pattern not found" status line on confirm.
    pub last_search_found: bool,
    /// All confirmed search patterns (oldest first).
    history: Vec<String>,
    /// Current position when browsing history (`None` = not browsing).
    history_index: Option<usize>,
    /// Saves what the user typed before starting to browse history.
    input_before_history: String,
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            pattern: String::new(),
            pattern_lower: String::new(),
            anchor: 0,
            last_search_found: false,
            history: Vec::new(),
            history_index: None,
            input_before_history: String::new(),
        }
    }

    pub fn clear(&mut self) {
        self.pattern.clear();
        self.pattern_lower.clear();
        self.anchor = 0;
        self.last_search_found = false;
    }

    /// Record the cursor position from which subsequent forward searches
    /// should originate (called when `/` opens the search bar).
    pub fn set_anchor(&mut self, cursor: usize) {
        self.anchor = cursor;
    }

    /// Push a non-empty pattern into history, deduplicating consecutive entries.
    pub fn push_history(&mut self, pattern: &str) {
        if pattern.is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) == Some(pattern) {
            return;
        }
        self.history.push(pattern.to_string());
    }

    /// Move to an older history entry. On the first call, saves `current_input`.
    /// Returns the history entry to display, or `None` if already at the oldest.
    pub fn history_prev(&mut self, current_input: &str) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        match self.history_index {
            None => {
                // Start browsing from the newest entry
                self.input_before_history = current_input.to_string();
                let idx = self.history.len() - 1;
                self.history_index = Some(idx);
                Some(self.history[idx].clone())
            }
            Some(0) => {
                // Already at the oldest entry
                None
            }
            Some(idx) => {
                let new_idx = idx - 1;
                self.history_index = Some(new_idx);
                Some(self.history[new_idx].clone())
            }
        }
    }

    /// Move to a newer history entry. Returns the entry to display, or the
    /// saved input when moving past the newest entry back to the user's text.
    /// Returns `None` if not currently browsing history.
    pub fn history_next(&mut self) -> Option<String> {
        let idx = self.history_index?;
        if idx + 1 < self.history.len() {
            let new_idx = idx + 1;
            self.history_index = Some(new_idx);
            Some(self.history[new_idx].clone())
        } else {
            // Past newest → restore user's original input
            self.history_index = None;
            Some(self.input_before_history.clone())
        }
    }

    /// Reset history browsing state (call when opening a new search session).
    pub fn reset_history_browse(&mut self) {
        self.history_index = None;
        self.input_before_history.clear();
    }
}

pub struct Editor {
    documents: Vec<Document>,
    active_index: usize,
    /// MRU file-buffer history (oldest -> newest), excludes scratch buffers.
    buffer_history: Vec<DocumentId>,
    /// Current index for history navigation (`g p` / `g n`).
    buffer_history_index: Option<usize>,
    next_id: DocumentId,
    pub mode: Mode,
    pub message: Option<String>,
    pub highlight_manager: HighlightManager,
    pub language_registry: LanguageRegistry,
    /// Language name per document id (for status bar display).
    language_names: std::collections::HashMap<DocumentId, &'static str>,
    pub search: SearchState,
    /// Single yank register (clipboard-like).
    pub register: Option<String>,
    /// Set when edits occur; cleared after highlights are updated.
    pub highlights_dirty: bool,
    /// Vim-style macro recorder (q/@ commands).
    pub macro_recorder: MacroRecorder,
    /// Dot-repeat recorder (. command).
    pub dot_recorder: DotRecorder,
    diagnostics_by_path: std::collections::HashMap<String, FileDiagnostics>,
    jump_list: Vec<JumpLocation>,
    jump_list_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct FileDiagnostics {
    line_severity: std::collections::HashMap<usize, LspSeverity>,
    line_message: std::collections::HashMap<usize, String>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub(crate) const MAX_JUMP_LIST_LEN: usize = 512;

    pub fn new() -> Self {
        let first = Document::new_scratch(1);
        Self {
            documents: vec![first],
            active_index: 0,
            buffer_history: Vec::new(),
            buffer_history_index: None,
            next_id: 2,
            mode: Mode::Normal,
            message: None,
            highlight_manager: HighlightManager::new(),
            language_registry: LanguageRegistry::new(),
            language_names: std::collections::HashMap::new(),
            search: SearchState::new(),
            register: None,
            highlights_dirty: false,
            macro_recorder: MacroRecorder::new(),
            dot_recorder: DotRecorder::new(),
            diagnostics_by_path: std::collections::HashMap::new(),
            jump_list: Vec::new(),
            jump_list_index: None,
        }
    }

    pub fn open(path: &str) -> Self {
        let language_registry = LanguageRegistry::new();
        let mut highlight_manager = HighlightManager::new();
        let mut language_names = std::collections::HashMap::new();
        let mut first = Document::from_file(1, path);
        first.git_gutter = git_diff_line_status(path);

        if let Some(lang_def) = language_registry.detect_by_extension(path) {
            highlight_manager.register_buffer(first.id, &first.rope, lang_def);
            language_names.insert(first.id, lang_def.name);
        }

        let first_id = first.id;
        Self {
            documents: vec![first],
            active_index: 0,
            buffer_history: vec![first_id],
            buffer_history_index: Some(0),
            next_id: 2,
            mode: Mode::Normal,
            message: None,
            highlight_manager,
            language_registry,
            language_names,
            search: SearchState::new(),
            register: None,
            highlights_dirty: false,
            macro_recorder: MacroRecorder::new(),
            dot_recorder: DotRecorder::new(),
            diagnostics_by_path: std::collections::HashMap::new(),
            jump_list: Vec::new(),
            jump_list_index: None,
        }
    }

    /// Mark that highlights need updating (deferred to next render).
    ///
    /// The document version counter (bumped per-edit) is the authoritative
    /// signal for invalidating the async search worker's lowercased-text
    /// cache, so this no longer touches search state.
    pub fn mark_highlights_dirty(&mut self) {
        self.highlights_dirty = true;
    }

    /// Drain pending edits from the active document and update highlighting.
    pub fn update_highlights(&mut self) {
        self.highlights_dirty = false;
        let doc = &mut self.documents[self.active_index];
        if doc.pending_edits.is_empty() {
            return;
        }
        let edits: Vec<_> = doc.pending_edits.drain(..).collect();
        let doc_id = doc.id;
        let rope = &doc.rope;
        self.highlight_manager.update(doc_id, rope, &edits);
    }

    /// Update highlights only if marked dirty. Call before rendering.
    pub fn update_highlights_if_dirty(&mut self) {
        if self.highlights_dirty {
            self.update_highlights();
        }
    }

    /// Register highlights for the active buffer by file extension.
    /// Useful from external code (e.g. benchmarks) where splitting borrows is awkward.
    pub fn register_highlights_for_extension(&mut self, ext: &str) {
        let doc = &self.documents[self.active_index];
        let doc_id = doc.id;
        if let Some(lang_def) = self.language_registry.detect_by_extension(ext) {
            self.highlight_manager.register_buffer(
                doc_id,
                &self.documents[self.active_index].rope,
                lang_def,
            );
            self.language_names.insert(doc_id, lang_def.name);
        }
    }

    /// Reload the active buffer from disk and rebuild syntax state from scratch.
    /// `reload_from_disk` replaces the rope wholesale and clears `pending_edits`,
    /// so the tree-sitter tree and the visible-span cache must be reset rather
    /// than incrementally updated.
    pub fn reload_active_buffer_from_disk(&mut self) -> Result<String, String> {
        let result = self.documents[self.active_index].reload_from_disk();
        if result.is_ok() {
            self.refresh_active_buffer_language();
            self.mark_highlights_dirty();
        }
        result
    }

    /// Refresh highlight registration and language name for the active buffer's current file path.
    pub fn refresh_active_buffer_language(&mut self) {
        let doc_id = self.documents[self.active_index].id;
        let path = self.documents[self.active_index]
            .file_path
            .as_ref()
            .and_then(|p| p.to_str())
            .map(str::to_owned);

        match path
            .as_deref()
            .and_then(|p| self.language_registry.detect_by_extension(p))
        {
            Some(lang_def) => {
                let rope = &self.documents[self.active_index].rope;
                self.highlight_manager
                    .register_buffer(doc_id, rope, lang_def);
                self.language_names.insert(doc_id, lang_def.name);
            }
            None => {
                self.highlight_manager.unregister_buffer(doc_id);
                self.language_names.remove(&doc_id);
            }
        }
    }

    /// Get the detected language name for the active document.
    pub fn active_language_name(&self) -> Option<&'static str> {
        let doc_id = self.documents[self.active_index].id;
        self.language_names.get(&doc_id).copied()
    }

    /// Get the detected language name for any document by id.
    pub fn language_name_for(&self, doc_id: DocumentId) -> Option<&'static str> {
        self.language_names.get(&doc_id).copied()
    }

    /// Insert a newline with tree-sitter-based auto-indent.
    /// Falls back to copying the current line's indent when no tree/indent query is available.
    pub fn insert_newline_with_indent(&mut self, tab_width: usize) {
        // Ensure tree is up-to-date before reading it for indent calculation
        self.update_highlights_if_dirty();
        let doc = &self.documents[self.active_index];
        let doc_id = doc.id;
        let cursor = doc.cursors[0].min(doc.rope.len_chars());
        let cursor_byte = doc.rope.char_to_byte(cursor);
        let current_line = doc.cursor_line();

        // Try tree-sitter indent calculation
        let indent_str = if let (Some(tree), Some(iq)) = (
            self.highlight_manager.tree(doc_id),
            self.highlight_manager.indent_query(doc_id),
        ) {
            let source = doc.rope.to_string();
            let level = indent::calculate_indent_level(tree, iq, source.as_bytes(), cursor_byte);
            indent::indent_string(level, tab_width)
        } else {
            indent::copy_line_indent(&doc.rope, current_line)
        };

        let text = format!("\n{}", indent_str);
        self.documents[self.active_index].insert_text(&text);
        self.mark_highlights_dirty();
    }

    /// Returns true when there is exactly one buffer that is a clean scratch buffer.
    pub fn is_single_clean_scratch(&self) -> bool {
        self.documents.len() == 1
            && self.documents[0].file_path.is_none()
            && !self.documents[0].dirty
    }
}

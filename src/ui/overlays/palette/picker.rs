use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crossterm::style::Color;

use crate::command::git::GitFileStatus;
use crate::command::history::CommandHistory;
use crate::command::registry::CommandRegistry;
use crate::config::Config;
use crate::core::buffer::BufferId;
use crate::input::action::{
    Action, AppAction, BufferAction, IntegrationAction, NavigationAction, ProjectAction, UiAction,
    WorkspaceAction,
};
use crate::log::debug_log;
use crate::syntax::highlight::{HighlightSpan, highlight_text};
use crate::syntax::language::LanguageRegistry;
use crate::syntax::theme::Theme;
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::EventResult;
use crate::ui::framework::surface::Surface;
use crate::ui::shared::filtering::{fuzzy_match, fzf_style_match};
use crate::ui::text::{display_width, truncate_to_width};
use crate::ui::text_input::TextInput;
use crate::ui::views::text_view::render_highlighted_line_windowed;

#[path = "workers.rs"]
mod workers;

#[path = "types.rs"]
mod types;
pub use types::*;

#[path = "constructors.rs"]
mod constructors;

#[path = "filtering.rs"]
mod filtering;

#[path = "preview.rs"]
mod preview;

#[path = "rendering.rs"]
mod rendering;

#[path = "input.rs"]
mod input;

#[path = "search.rs"]
mod search;

#[cfg(test)]
use rendering::shorten_path;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

type PreviewCache = HashMap<String, (Vec<String>, HashMap<usize, Vec<HighlightSpan>>)>;

pub struct Palette {
    pub input: TextInput,
    pub mode: PaletteMode,
    pub candidates: Vec<ScoredCandidate>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub preview_lines: Vec<String>,
    pub preview_spans: HashMap<usize, Vec<HighlightSpan>>,
    preview_cache: PreviewCache,
    buffer_entries: Vec<(BufferId, String, Vec<String>)>,
    jump_entries: Vec<JumpEntry>,
    reference_entries: Vec<ReferenceEntry>,
    git_branch_entries: Vec<GitBranchEntry>,
    symbol_entries: Vec<SymbolEntry>,
    symbol_submit_behavior: SymbolSubmitBehavior,
    file_entries: Vec<String>,
    project_root: PathBuf,
    request_tx: Option<mpsc::Sender<PreviewRequest>>,
    result_rx: Option<mpsc::Receiver<PreviewResult>>,
    _worker: Option<thread::JoinHandle<()>>,
    requested_paths: HashSet<String>,
    git_status_map: HashMap<String, GitFileStatus>,
    last_previewed_buffer: Option<BufferId>,
    last_previewed_jump_index: Option<usize>,
    last_previewed_reference_index: Option<usize>,
    last_previewed_git_branch_index: Option<usize>,
    last_previewed_symbol_index: Option<usize>,
    last_previewed_search_index: Option<usize>,
    jump_target_preview_line: Option<usize>,
    jump_target_char_col: Option<usize>,
    buffer_highlight_cache: HashMap<BufferId, HashMap<usize, Vec<HighlightSpan>>>,
    reference_highlight_cache: HashMap<usize, HashMap<usize, Vec<HighlightSpan>>>,
    lang_registry_owned: Option<LanguageRegistry>,
    command_history: Option<Rc<CommandHistory>>,
    global_search_unsaved_buffers: Vec<GlobalSearchBufferSource>,
    global_search_entries: Vec<GlobalSearchResultEntry>,
    global_search_request_tx: Option<mpsc::Sender<GlobalSearchRequest>>,
    global_search_result_rx: Option<mpsc::Receiver<GlobalSearchBatch>>,
    _global_search_worker: Option<thread::JoinHandle<()>>,
    global_search_generation: u64,
    global_search_latest_applied: u64,
    global_search_dirty: bool,
    global_search_changed_at: Option<Instant>,
    active_doc_lines: Vec<String>,
    is_unified: bool,
    caller_label: Option<String>,
}

impl Palette {
    pub fn set_git_status_map(&mut self, git_status_map: &HashMap<String, GitFileStatus>) {
        self.git_status_map = git_status_map.clone();
    }

    pub fn set_file_entries(&mut self, files: Vec<String>) {
        self.file_entries = files;
        self.requested_paths.clear();
        self.preview_cache.clear();
        self.restart_global_search_worker();
    }

    pub fn set_git_branch_entries(&mut self, entries: Vec<GitBranchPickerEntry>) {
        self.git_branch_entries = entries
            .into_iter()
            .map(|entry| GitBranchEntry {
                branch_name: entry.branch_name,
                label: entry.label,
                preview_lines: entry.preview_lines,
            })
            .collect();
        self.last_previewed_git_branch_index = None;

        if self.mode == PaletteMode::GitBranchPicker
            || self.mode == PaletteMode::GitBranchComparePicker
        {
            self.filter_git_branch_candidates();
            self.update_git_branch_preview();
        }
    }

    pub fn refresh_after_file_entries_update(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        if self.is_unified {
            self.update_candidates(registry, lang_registry, config);
            return;
        }

        if self.mode == PaletteMode::GlobalSearch {
            self.mark_global_search_dirty();
            self.pump_global_search();
        }
    }
}

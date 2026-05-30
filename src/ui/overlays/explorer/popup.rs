use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};

use crate::command::git::{GitFileStatus, dir_git_status};
use crate::input::action::{
    Action, AppAction, BufferAction, IntegrationAction, ProjectAction, UiAction,
};
use crate::syntax::highlight::{HighlightSpan, highlight_text};
use crate::syntax::language::LanguageRegistry;
use crate::syntax::theme::Theme;
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::EventResult;
use crate::ui::framework::surface::Surface;
use crate::ui::shared::file_browser::{
    is_valid_relative_subpath, is_valid_single_name, sort_by_name_case_insensitive,
};
use crate::ui::shared::filtering::fuzzy_match;
use crate::ui::text::{display_width, slice_display_window, truncate_to_width};
use crate::ui::text_input::delete_prev_word_input;
use crate::ui::views::text_view::render_highlighted_line_windowed;

type PreviewCache = HashMap<PathBuf, (Vec<String>, HashMap<usize, Vec<HighlightSpan>>)>;

struct PreviewRequest {
    path: PathBuf,
}

struct PreviewResult {
    path: PathBuf,
    lines: Vec<String>,
    spans: HashMap<usize, Vec<HighlightSpan>>,
}

fn preview_worker(rx: mpsc::Receiver<PreviewRequest>, tx: mpsc::Sender<PreviewResult>) {
    let lang_registry = LanguageRegistry::new();
    while let Ok(req) = rx.recv() {
        let content = match std::fs::read_to_string(&req.path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let lines: Vec<String> = content.lines().take(200).map(|l| l.to_string()).collect();
        let path_str = req.path.to_string_lossy();
        let spans = if let Some(lang_def) = lang_registry.detect_by_extension(&path_str) {
            let preview_text: String = lines.join("\n");
            highlight_text(&preview_text, lang_def)
        } else {
            HashMap::new()
        };

        if tx
            .send(PreviewResult {
                path: req.path,
                lines,
                spans,
            })
            .is_err()
        {
            break;
        }
    }
}

#[derive(Clone)]
struct TreeEntry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    depth: usize,
    git_status: Option<GitFileStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExplorerPopupMode {
    OpenFiles,
    SelectProjectRoot,
}

pub struct ExplorerPopup {
    root: PathBuf,
    mode: ExplorerPopupMode,
    expanded_dirs: HashSet<PathBuf>,
    entries: Vec<TreeEntry>,
    selected: usize,
    scroll_offset: usize,
    find_active: bool,
    find_input: String,
    copy_menu_active: bool,
    rename_active: bool,
    rename_input: String,
    add_active: bool,
    add_input: String,
    delete_confirm_active: bool,
    // preview
    preview_lines: Vec<String>,
    preview_scroll: usize,
    preview_horizontal_scroll: usize,
    preview_spans: HashMap<usize, Vec<HighlightSpan>>,
    preview_cache: PreviewCache,
    request_tx: Option<mpsc::Sender<PreviewRequest>>,
    result_rx: Option<mpsc::Receiver<PreviewResult>>,
    _worker: Option<thread::JoinHandle<()>>,
    requested_paths: HashSet<PathBuf>,
    current_preview_path: Option<PathBuf>,
    git_status_map: HashMap<String, GitFileStatus>,
}

const PREVIEW_SPLIT_THRESHOLD: usize = 60;
const MOUSE_SCROLL_LINES: usize = 3;
const HORIZONTAL_SCROLL_COLS: usize = 8;
const TREE_INDENT_STEP: usize = 2;
const TREE_MIN_LABEL_COLUMNS: usize = 6;

impl ExplorerPopup {
    pub fn new(
        root: PathBuf,
        git_status_map: &HashMap<String, GitFileStatus>,
        reveal: Option<&Path>,
    ) -> Self {
        Self::new_with_mode(root, git_status_map, ExplorerPopupMode::OpenFiles, reveal)
    }

    pub fn new_for_project_root(
        root: PathBuf,
        git_status_map: &HashMap<String, GitFileStatus>,
    ) -> Self {
        Self::new_with_mode(
            root,
            git_status_map,
            ExplorerPopupMode::SelectProjectRoot,
            None,
        )
    }

    fn new_with_mode(
        root: PathBuf,
        git_status_map: &HashMap<String, GitFileStatus>,
        mode: ExplorerPopupMode,
        reveal: Option<&Path>,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PreviewRequest>();
        let (res_tx, res_rx) = mpsc::channel::<PreviewResult>();
        let handle = thread::spawn(move || {
            preview_worker(req_rx, res_tx);
        });

        let mut popup = Self {
            root,
            mode,
            expanded_dirs: HashSet::new(),
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            find_active: false,
            find_input: String::new(),
            copy_menu_active: false,
            rename_active: false,
            rename_input: String::new(),
            add_active: false,
            add_input: String::new(),
            delete_confirm_active: false,
            preview_lines: Vec::new(),
            preview_scroll: 0,
            preview_horizontal_scroll: 0,
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            request_tx: Some(req_tx),
            result_rx: Some(res_rx),
            _worker: Some(handle),
            requested_paths: HashSet::new(),
            current_preview_path: None,
            git_status_map: git_status_map.clone(),
        };
        if let Some(target) = reveal {
            popup.expand_ancestors_of(target);
        }
        popup.rebuild_entries();
        if let Some(target) = reveal {
            popup.select_by_path(target);
        }
        popup.update_preview();
        popup
    }

    /// Mark every ancestor directory of `target` (up to but not including the
    /// popup root) as expanded so the subsequent `rebuild_entries` walk emits
    /// the path down to `target`. A no-op when `target` lives outside the root.
    fn expand_ancestors_of(&mut self, target: &Path) {
        let Ok(rel) = target.strip_prefix(&self.root) else {
            return;
        };
        let mut current = self.root.clone();
        for component in rel.components() {
            current = current.join(component);
            if current == *target {
                break;
            }
            self.expanded_dirs.insert(current.clone());
        }
    }

    fn rebuild_entries(&mut self) {
        self.entries.clear();
        self.build_tree(&self.root.clone(), 0);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    fn build_tree(&mut self, dir: &Path, depth: usize) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let path = entry.path();
                let rel_path = path
                    .strip_prefix(&self.root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                let git_status = if is_dir {
                    let prefix = if rel_path.ends_with('/') {
                        rel_path
                    } else {
                        format!("{}/", rel_path)
                    };
                    dir_git_status(&self.git_status_map, &prefix)
                } else {
                    self.git_status_map.get(&rel_path).copied()
                };

                if is_dir {
                    dirs.push(TreeEntry {
                        name,
                        path,
                        is_dir: true,
                        depth,
                        git_status,
                    });
                } else {
                    files.push(TreeEntry {
                        name,
                        path,
                        is_dir: false,
                        depth,
                        git_status,
                    });
                }
            }
        }

        sort_by_name_case_insensitive(&mut dirs, |entry| &entry.name);
        sort_by_name_case_insensitive(&mut files, |entry| &entry.name);

        for d in dirs {
            let expanded = self.expanded_dirs.contains(&d.path);
            let child_path = d.path.clone();
            self.entries.push(d);
            if expanded {
                self.build_tree(&child_path, depth + 1);
            }
        }

        for f in files {
            self.entries.push(f);
        }
    }

    fn filtered_entries(&self) -> Vec<usize> {
        (0..self.entries.len()).collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return EventResult::Action(Action::Ui(UiAction::CloseExplorerPopup));
        }

        if self.copy_menu_active {
            return self.handle_copy_menu_key(key);
        }

        if self.rename_active {
            return self.handle_rename_key(key);
        }

        if self.add_active {
            return self.handle_add_key(key);
        }

        if self.delete_confirm_active {
            return self.handle_delete_confirm_key(key);
        }

        if self.find_active {
            return self.handle_find_key(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            return self.change_project_root_from_selection();
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('n') => {
                    self.move_down();
                    EventResult::Consumed
                }
                KeyCode::Char('p') => {
                    self.move_up();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }

        if key.modifiers.contains(KeyModifiers::SHIFT) {
            return match key.code {
                KeyCode::Char('J') | KeyCode::Down => {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                    EventResult::Consumed
                }
                KeyCode::Char('K') | KeyCode::Up => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                    EventResult::Consumed
                }
                KeyCode::Char('L') | KeyCode::Right => {
                    self.scroll_preview_right();
                    EventResult::Consumed
                }
                KeyCode::Char('H') | KeyCode::Left => {
                    self.scroll_preview_left();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_down();
                EventResult::Consumed
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_up();
                EventResult::Consumed
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if self.mode == ExplorerPopupMode::SelectProjectRoot {
                    self.toggle_selected_dir()
                } else {
                    self.enter_selected()
                }
            }
            KeyCode::Enter => self.enter_selected(),
            KeyCode::Char('h') | KeyCode::Left => self.handle_h(),
            KeyCode::Char(' ') if self.mode == ExplorerPopupMode::SelectProjectRoot => {
                self.toggle_selected_dir()
            }
            KeyCode::Char('/') => {
                self.find_active = true;
                self.find_input.clear();
                EventResult::Consumed
            }
            KeyCode::Char('c') => {
                self.copy_menu_active = true;
                EventResult::Consumed
            }
            KeyCode::Char('r') => self.start_rename_prompt(),
            KeyCode::Char('a') => self.start_add_prompt(),
            KeyCode::Char('d') => self.start_delete_confirm(),
            KeyCode::Esc => EventResult::Action(Action::Ui(UiAction::CloseExplorerPopup)),
            _ => EventResult::Consumed,
        }
    }

    fn popup_size(cols: usize, rows: usize) -> (usize, usize) {
        crate::ui::popup_layout::popup_size(cols, rows)
    }

    fn preview_content_height_for_surface(cols: usize, rows: usize) -> Option<usize> {
        let (popup_w, popup_h) = Self::popup_size(cols, rows);
        if popup_w >= PREVIEW_SPLIT_THRESHOLD {
            Some(popup_h.saturating_sub(2))
        } else {
            None
        }
    }

    fn preview_max_scroll(&self, content_h: usize) -> usize {
        if content_h == 0 {
            0
        } else {
            self.preview_lines.len().saturating_sub(content_h)
        }
    }

    fn preview_max_horizontal_scroll(&self, content_w: usize) -> usize {
        if content_w == 0 {
            return 0;
        }
        self.preview_lines
            .iter()
            .map(|line| display_width(line).saturating_sub(content_w))
            .max()
            .unwrap_or(0)
    }

    fn clamp_preview_scroll(&mut self, content_h: usize) {
        let max_scroll = self.preview_max_scroll(content_h);
        if self.preview_scroll > max_scroll {
            self.preview_scroll = max_scroll;
        }
    }

    fn clamp_preview_horizontal_scroll(&mut self, content_w: usize) {
        let max_scroll = self.preview_max_horizontal_scroll(content_w);
        if self.preview_horizontal_scroll > max_scroll {
            self.preview_horizontal_scroll = max_scroll;
        }
    }

    fn scroll_preview_down_lines(&mut self, lines: usize, content_h: usize) {
        let max_scroll = self.preview_max_scroll(content_h);
        self.preview_scroll = self.preview_scroll.saturating_add(lines).min(max_scroll);
    }

    fn scroll_preview_up_lines(&mut self, lines: usize) {
        self.preview_scroll = self.preview_scroll.saturating_sub(lines);
    }

    fn scroll_preview_right(&mut self) {
        self.preview_horizontal_scroll = self
            .preview_horizontal_scroll
            .saturating_add(HORIZONTAL_SCROLL_COLS);
    }

    fn scroll_preview_left(&mut self) {
        self.preview_horizontal_scroll = self
            .preview_horizontal_scroll
            .saturating_sub(HORIZONTAL_SCROLL_COLS);
    }

    pub fn handle_mouse_scroll(
        &mut self,
        kind: MouseEventKind,
        cols: usize,
        rows: usize,
    ) -> EventResult {
        let Some(content_h) = Self::preview_content_height_for_surface(cols, rows) else {
            return EventResult::Ignored;
        };
        match kind {
            MouseEventKind::ScrollDown => {
                self.scroll_preview_down_lines(MOUSE_SCROLL_LINES, content_h);
                EventResult::Consumed
            }
            MouseEventKind::ScrollUp => {
                self.scroll_preview_up_lines(MOUSE_SCROLL_LINES);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    pub fn set_git_status_map(&mut self, git_status_map: &HashMap<String, GitFileStatus>) {
        self.git_status_map = git_status_map.clone();

        let statuses: Vec<Option<GitFileStatus>> = self
            .entries
            .iter()
            .map(|entry| self.entry_git_status(&entry.path, entry.is_dir))
            .collect();
        for (entry, status) in self.entries.iter_mut().zip(statuses) {
            entry.git_status = status;
        }
    }

    fn entry_git_status(&self, path: &Path, is_dir: bool) -> Option<GitFileStatus> {
        let rel_path = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        if is_dir {
            let prefix = if rel_path.ends_with('/') {
                rel_path
            } else {
                format!("{}/", rel_path)
            };
            dir_git_status(&self.git_status_map, &prefix)
        } else {
            self.git_status_map.get(&rel_path).copied()
        }
    }

    fn handle_copy_menu_key(&mut self, key: KeyEvent) -> EventResult {
        self.copy_menu_active = false;
        match key.code {
            KeyCode::Char('c') => self.copy_selected_full_path(),
            KeyCode::Char('d') => self.copy_selected_dir_path(),
            KeyCode::Char('f') => self.copy_selected_name(),
            _ => EventResult::Consumed,
        }
    }

    fn handle_find_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('n') => {
                    self.move_down();
                    EventResult::Consumed
                }
                KeyCode::Char('p') => {
                    self.move_up();
                    EventResult::Consumed
                }
                KeyCode::Char('f') => self.enter_selected(),
                KeyCode::Char('b') => self.handle_h(),
                KeyCode::Char('w') => {
                    self.delete_prev_word();
                    self.jump_to_best_match();
                    EventResult::Consumed
                }
                KeyCode::Char('k') => {
                    self.find_input.clear();
                    EventResult::Consumed
                }
                KeyCode::Char('u') => {
                    self.find_input.clear();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }

        match key.code {
            KeyCode::Esc => {
                self.find_active = false;
                self.find_input.clear();
                EventResult::Consumed
            }
            KeyCode::Enter => {
                self.find_active = false;
                EventResult::Consumed
            }
            KeyCode::Backspace => {
                self.find_input.pop();
                self.jump_to_best_match();
                EventResult::Consumed
            }
            KeyCode::Up => {
                self.move_up();
                EventResult::Consumed
            }
            KeyCode::Down => {
                self.move_down();
                EventResult::Consumed
            }
            KeyCode::Left => self.handle_h(),
            KeyCode::Right => self.enter_selected(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.find_input.push(c);
                self.jump_to_best_match();
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn handle_rename_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('w') => {
                    delete_prev_word_input(&mut self.rename_input);
                    EventResult::Consumed
                }
                KeyCode::Char('u') | KeyCode::Char('k') => {
                    self.rename_input.clear();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }
        match key.code {
            KeyCode::Esc => {
                self.rename_active = false;
                self.rename_input.clear();
                EventResult::Consumed
            }
            KeyCode::Enter => {
                self.rename_active = false;
                self.apply_rename()
            }
            KeyCode::Backspace => {
                self.rename_input.pop();
                EventResult::Consumed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.rename_input.push(c);
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn handle_add_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('w') => {
                    delete_prev_word_input(&mut self.add_input);
                    EventResult::Consumed
                }
                KeyCode::Char('u') | KeyCode::Char('k') => {
                    self.add_input.clear();
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }
        match key.code {
            KeyCode::Esc => {
                self.add_active = false;
                self.add_input.clear();
                EventResult::Consumed
            }
            KeyCode::Enter => {
                self.add_active = false;
                self.apply_add()
            }
            KeyCode::Backspace => {
                self.add_input.pop();
                EventResult::Consumed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.add_input.push(c);
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn handle_delete_confirm_key(&mut self, key: KeyEvent) -> EventResult {
        self.delete_confirm_active = false;
        match key.code {
            KeyCode::Char('y') => self.apply_delete(),
            _ => self.show_message("Delete aborted".to_string()),
        }
    }

    fn jump_to_best_match(&mut self) {
        if self.find_input.is_empty() {
            return;
        }
        let mut best: Option<(i32, usize)> = None;
        for (idx, entry) in self.entries.iter().enumerate() {
            if let Some((score, _)) = fuzzy_match(&entry.name, &self.find_input)
                && best.is_none_or(|(best_score, _)| score > best_score)
            {
                best = Some((score, idx));
            }
        }
        if let Some((_, idx)) = best {
            self.selected = idx;
            self.update_preview();
        }
    }

    fn delete_prev_word(&mut self) {
        delete_prev_word_input(&mut self.find_input);
    }

    fn show_message(&self, message: String) -> EventResult {
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::ShowMessage(message),
        )))
    }

    fn start_rename_prompt(&mut self) -> EventResult {
        let Some(name) = self
            .entries
            .get(self.selected)
            .map(|entry| entry.name.clone())
        else {
            return EventResult::Consumed;
        };
        self.rename_active = true;
        self.rename_input = name;
        EventResult::Consumed
    }

    fn start_add_prompt(&mut self) -> EventResult {
        self.add_active = true;
        self.add_input.clear();
        EventResult::Consumed
    }

    fn start_delete_confirm(&mut self) -> EventResult {
        if self.entries.get(self.selected).is_none() {
            return EventResult::Consumed;
        }
        self.delete_confirm_active = true;
        EventResult::Consumed
    }

    fn selected_add_parent_dir(&self) -> PathBuf {
        let Some(entry) = self.entries.get(self.selected) else {
            return self.root.clone();
        };
        if entry.is_dir {
            entry.path.clone()
        } else if let Some(parent) = entry.path.parent() {
            parent.to_path_buf()
        } else {
            self.root.clone()
        }
    }

    fn select_by_path(&mut self, target: &Path) {
        if let Some((idx, _)) = self
            .entries
            .iter()
            .enumerate()
            .find(|(_, e)| e.path == target)
        {
            self.selected = idx;
        }
    }

    fn remap_expanded_dir_paths(&mut self, old: &Path, new: &Path) {
        let mut remapped = HashSet::new();
        for expanded in &self.expanded_dirs {
            if expanded == old || expanded.starts_with(old) {
                if let Ok(rest) = expanded.strip_prefix(old) {
                    remapped.insert(new.join(rest));
                }
            } else {
                remapped.insert(expanded.clone());
            }
        }
        self.expanded_dirs = remapped;
    }

    fn apply_rename(&mut self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return self.show_message("Rename failed: no selection".to_string());
        };
        let new_name = self.rename_input.trim().to_string();
        if !is_valid_single_name(&new_name) {
            return self.show_message("Rename failed: invalid name".to_string());
        }
        if new_name == entry.name {
            return self.show_message("Rename skipped: unchanged".to_string());
        }
        let Some(parent) = entry.path.parent() else {
            return self.show_message("Rename failed: invalid parent path".to_string());
        };
        let dest_path = parent.join(&new_name);
        if dest_path.exists() {
            return self.show_message(format!("Rename failed: '{}' already exists", new_name));
        }
        match std::fs::rename(&entry.path, &dest_path) {
            Ok(()) => {
                if entry.is_dir {
                    self.remap_expanded_dir_paths(&entry.path, &dest_path);
                }
                self.rebuild_entries();
                self.select_by_path(&dest_path);
                self.update_preview();
                self.show_message(format!("Renamed to {}", new_name))
            }
            Err(e) => self.show_message(format!("Rename failed: {}", e)),
        }
    }

    fn apply_add(&mut self) -> EventResult {
        let raw = self.add_input.trim().to_string();
        let is_dir = raw.ends_with('/');
        let rel = raw.trim_end_matches('/');
        if !is_valid_relative_subpath(rel) {
            return self.show_message("Add failed: invalid path".to_string());
        }
        let parent_dir = self.selected_add_parent_dir();
        let rel_path = std::path::PathBuf::from(rel);
        let target = parent_dir.join(&rel_path);
        if target.exists() {
            return self.show_message(format!("Add failed: '{}' already exists", rel));
        }

        let result = if is_dir {
            std::fs::create_dir_all(&target)
        } else {
            let mkdir = match target.parent() {
                Some(parent) if parent != parent_dir.as_path() => std::fs::create_dir_all(parent),
                _ => Ok(()),
            };
            mkdir.and_then(|()| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map(|_| ())
            })
        };

        match result {
            Ok(()) => {
                // Expand each intermediate dir so the tree view reveals the new entry.
                self.expanded_dirs.insert(parent_dir.clone());
                let mut walk = parent_dir.clone();
                for component in rel_path.components() {
                    walk = walk.join(component);
                    if walk == target && !is_dir {
                        break;
                    }
                    self.expanded_dirs.insert(walk.clone());
                }
                self.rebuild_entries();
                self.select_by_path(&target);
                self.update_preview();
                let kind = if is_dir { "directory" } else { "file" };
                self.show_message(format!("Created {} {}", kind, rel))
            }
            Err(e) => self.show_message(format!("Add failed: {}", e)),
        }
    }

    fn apply_delete(&mut self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return self.show_message("Delete failed: no selection".to_string());
        };
        let old_selected = self.selected;
        let result = if entry.is_dir {
            std::fs::remove_dir_all(&entry.path)
        } else {
            std::fs::remove_file(&entry.path)
        };
        match result {
            Ok(()) => {
                if entry.is_dir {
                    self.expanded_dirs
                        .retain(|p| !(p == &entry.path || p.starts_with(&entry.path)));
                }
                self.rebuild_entries();
                if !self.entries.is_empty() {
                    self.selected = old_selected.min(self.entries.len() - 1);
                }
                self.update_preview();
                self.show_message(format!("Deleted {}", entry.name))
            }
            Err(e) => self.show_message(format!("Delete failed: {}", e)),
        }
    }

    fn move_down(&mut self) {
        let visible = self.filtered_entries();
        if visible.is_empty() {
            return;
        }
        if let Some(pos) = visible.iter().position(|&i| i == self.selected)
            && pos + 1 < visible.len()
        {
            self.selected = visible[pos + 1];
            self.update_preview();
        }
    }

    fn move_up(&mut self) {
        let visible = self.filtered_entries();
        if visible.is_empty() {
            return;
        }
        if let Some(pos) = visible.iter().position(|&i| i == self.selected)
            && pos > 0
        {
            self.selected = visible[pos - 1];
            self.update_preview();
        }
    }

    fn toggle_selected_dir(&mut self) -> EventResult {
        if self.selected >= self.entries.len() {
            return EventResult::Consumed;
        }
        let entry = &self.entries[self.selected];
        if !entry.is_dir {
            return EventResult::Consumed;
        }
        let path = entry.path.clone();
        if self.expanded_dirs.contains(&path) {
            self.expanded_dirs.remove(&path);
        } else {
            self.expanded_dirs.insert(path.clone());
        }
        self.rebuild_entries();
        self.select_by_path(&path);
        self.update_preview();
        EventResult::Consumed
    }

    fn change_project_root_from_selection(&self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected) else {
            return EventResult::Consumed;
        };
        let target = if entry.is_dir {
            entry.path.clone()
        } else if let Some(parent) = entry.path.parent() {
            parent.to_path_buf()
        } else {
            self.root.clone()
        };
        EventResult::Action(Action::App(AppAction::Project(
            ProjectAction::ChangeProjectRoot(target.to_string_lossy().to_string()),
        )))
    }

    fn enter_selected(&mut self) -> EventResult {
        if self.selected >= self.entries.len() {
            return EventResult::Consumed;
        }
        if self.mode == ExplorerPopupMode::SelectProjectRoot {
            return self.change_project_root_from_selection();
        }

        let entry = &self.entries[self.selected];
        if entry.is_dir {
            self.toggle_selected_dir()
        } else {
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorerPopup(entry.path.to_string_lossy().to_string()),
            )))
        }
    }

    fn copy_selected_full_path(&self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected) else {
            return EventResult::Consumed;
        };
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: entry.path.to_string_lossy().to_string(),
                description: "path".to_string(),
            },
        )))
    }

    fn copy_selected_dir_path(&self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected) else {
            return EventResult::Consumed;
        };
        let dir_path = if entry.is_dir {
            entry.path.clone()
        } else if let Some(parent) = entry.path.parent() {
            parent.to_path_buf()
        } else {
            self.root.clone()
        };
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: dir_path.to_string_lossy().to_string(),
                description: "dir path".to_string(),
            },
        )))
    }

    fn copy_selected_name(&self) -> EventResult {
        let Some(entry) = self.entries.get(self.selected) else {
            return EventResult::Consumed;
        };
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: entry.name.clone(),
                description: "file name".to_string(),
            },
        )))
    }

    fn handle_h(&mut self) -> EventResult {
        if self.selected >= self.entries.len() {
            return EventResult::Consumed;
        }
        let entry = &self.entries[self.selected];
        if entry.is_dir && self.expanded_dirs.contains(&entry.path) {
            // Collapse expanded dir
            let path = entry.path.clone();
            self.expanded_dirs.remove(&path);
            self.rebuild_entries();
            self.update_preview();
            return EventResult::Consumed;
        }

        // Jump to parent entry
        let entry_depth = self.entries[self.selected].depth;
        if entry_depth > 0 {
            // Find the nearest directory entry above with depth = entry_depth - 1
            for i in (0..self.selected).rev() {
                if self.entries[i].is_dir && self.entries[i].depth == entry_depth - 1 {
                    self.selected = i;
                    self.update_preview();
                    break;
                }
            }
            return EventResult::Consumed;
        }

        if self.mode == ExplorerPopupMode::SelectProjectRoot {
            let old_root = self.root.clone();
            let Some(parent) = old_root.parent() else {
                return EventResult::Consumed;
            };
            self.root = parent.to_path_buf();
            self.rebuild_entries();
            if let Some(name) = old_root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
            {
                for (idx, candidate) in self.entries.iter().enumerate() {
                    if candidate.depth == 0 && candidate.name == name {
                        self.selected = idx;
                        break;
                    }
                }
            }
            self.update_preview();
        }
        EventResult::Consumed
    }

    fn update_preview(&mut self) {
        self.preview_lines.clear();
        self.preview_scroll = 0;
        self.preview_horizontal_scroll = 0;
        self.preview_spans.clear();
        self.current_preview_path = None;

        if self.selected >= self.entries.len() {
            return;
        }
        let entry = &self.entries[self.selected];
        if entry.is_dir {
            return;
        }

        let path = entry.path.clone();
        self.current_preview_path = Some(path.clone());

        // Drain background results
        self.drain_preview_results();

        if let Some(cached) = self.preview_cache.get(&path) {
            self.preview_lines = cached.0.clone();
            self.preview_spans = cached.1.clone();
            self.schedule_nearby_previews();
            return;
        }

        // Sync fallback
        let lang_registry = LanguageRegistry::new();
        if let Ok(content) = std::fs::read_to_string(&path) {
            self.preview_lines = content.lines().take(200).map(|l| l.to_string()).collect();
            let path_str = path.to_string_lossy();
            if let Some(lang_def) = lang_registry.detect_by_extension(&path_str) {
                let preview_text: String = self.preview_lines.join("\n");
                self.preview_spans = highlight_text(&preview_text, lang_def);
            }
            self.preview_cache.insert(
                path,
                (self.preview_lines.clone(), self.preview_spans.clone()),
            );
        }

        self.schedule_nearby_previews();
    }

    fn drain_preview_results(&mut self) {
        let Some(ref rx) = self.result_rx else {
            return;
        };
        while let Ok(result) = rx.try_recv() {
            self.preview_cache
                .insert(result.path, (result.lines, result.spans));
        }
    }

    fn schedule_nearby_previews(&mut self) {
        let Some(ref tx) = self.request_tx else {
            return;
        };
        if self.entries.is_empty() {
            return;
        }

        let start = self.selected.saturating_sub(5);
        let end = (self.selected + 15).min(self.entries.len());

        for idx in start..end {
            let entry = &self.entries[idx];
            if entry.is_dir {
                continue;
            }
            let path = entry.path.clone();
            if self.preview_cache.contains_key(&path) || self.requested_paths.contains(&path) {
                continue;
            }
            self.requested_paths.insert(path.clone());
            if tx.send(PreviewRequest { path }).is_err() {
                break;
            }
        }
    }

    fn bottom_prompt(&self) -> String {
        if self.find_active {
            format!("/{}", self.find_input)
        } else if self.copy_menu_active {
            "copy: [c] path [d] dir [f] name".to_string()
        } else if self.rename_active {
            format!("rename: {}", self.rename_input)
        } else if self.add_active {
            format!("add: {} (end with / for dir)", self.add_input)
        } else if self.delete_confirm_active {
            if let Some(entry) = self.entries.get(self.selected) {
                format!("delete {}? [y/N]", entry.name)
            } else {
                "delete item? [y/N]".to_string()
            }
        } else {
            String::new()
        }
    }

    pub fn render_overlay(&mut self, surface: &mut Surface, theme: &Theme) -> Option<(u16, u16)> {
        let cols = surface.width;
        let rows = surface.height;
        let (popup_w, popup_h) = Self::popup_size(cols, rows);
        let offset_x = (cols.saturating_sub(popup_w)) / 2;
        let offset_y = (rows.saturating_sub(popup_h)) / 2;

        let left_w;

        if popup_w >= PREVIEW_SPLIT_THRESHOLD {
            let gap = 2;
            left_w = (popup_w - gap) / 2;
            let right_w = popup_w - gap - left_w;
            let right_x = offset_x + left_w + gap;

            self.render_tree_panel(surface, offset_x, offset_y, left_w, popup_h);
            self.render_preview_panel(surface, right_x, offset_y, right_w, popup_h, theme);
        } else {
            left_w = popup_w;
            self.render_tree_panel(surface, offset_x, offset_y, left_w, popup_h);
        }

        // Return cursor position for input prompts
        let prompt = if self.find_active {
            format!("/{}", self.find_input)
        } else if self.rename_active {
            format!("rename: {}", self.rename_input)
        } else if self.add_active {
            format!("add: {} (end with / for dir)", self.add_input)
        } else {
            String::new()
        };
        if !prompt.is_empty() {
            let find_row = offset_y + popup_h - 2; // inside bottom border
            let cursor_x = (offset_x + 1 + crate::ui::text::display_width(&prompt)) as u16;
            let cursor_y = find_row as u16;
            Some((cursor_x, cursor_y))
        } else {
            None
        }
    }

    fn render_tree_panel(&mut self, surface: &mut Surface, x: usize, y: usize, w: usize, h: usize) {
        let inner_w = w.saturating_sub(2);
        let default_style = CellStyle::default();

        let visible = self.filtered_entries();

        // Content area: rows between top border and bottom border
        // Row 0: top border
        // Row h-1: bottom border
        // If prompt is active, row h-2 is used for prompt
        let bottom_prompt_active = self.find_active
            || self.copy_menu_active
            || self.rename_active
            || self.add_active
            || self.delete_confirm_active;
        let content_h = if bottom_prompt_active {
            h.saturating_sub(3) // top border + find row + bottom border
        } else {
            h.saturating_sub(2) // top border + bottom border
        };

        // Adjust scroll
        // Find position of selected in visible list
        let sel_vis_pos = visible
            .iter()
            .position(|&i| i == self.selected)
            .unwrap_or(0);
        if sel_vis_pos < self.scroll_offset {
            self.scroll_offset = sel_vis_pos;
        }
        if sel_vis_pos >= self.scroll_offset + content_h {
            self.scroll_offset = sel_vis_pos.saturating_sub(content_h.saturating_sub(1));
        }

        for row in 0..h {
            if row == 0 {
                // Top border
                surface.put_str(x, y + row, "\u{250c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2510}", &default_style);
            } else if row == h - 1 {
                // Bottom border
                surface.put_str(x, y + row, "\u{2514}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2518}", &default_style);
            } else if bottom_prompt_active && row == h - 2 {
                // Bottom prompt row
                let find_style = CellStyle {
                    reverse: true,
                    ..CellStyle::default()
                };
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                let prompt = self.bottom_prompt();
                let (truncated, used) = truncate_to_width(&prompt, inner_w);
                surface.put_str(x + 1, y + row, truncated, &find_style);
                if used < inner_w {
                    surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &find_style);
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else {
                // Content row
                let content_row = row - 1;
                let vis_idx = self.scroll_offset + content_row;

                surface.put_str(x, y + row, "\u{2502}", &default_style);

                if vis_idx < visible.len() {
                    let entry_idx = visible[vis_idx];
                    let entry = &self.entries[entry_idx];
                    let is_selected = entry_idx == self.selected;

                    let display = tree_entry_display(entry, inner_w);

                    let status_fg = entry.git_status.map(|s| s.color());
                    let style = if is_selected {
                        CellStyle {
                            reverse: true,
                            fg: status_fg,
                            ..CellStyle::default()
                        }
                    } else {
                        CellStyle {
                            fg: status_fg,
                            ..CellStyle::default()
                        }
                    };

                    let (truncated, used) = truncate_to_width(&display, inner_w);
                    surface.put_str(x + 1, y + row, truncated, &style);
                    if used < inner_w {
                        surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &style);
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }

                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            }
        }
    }

    #[cfg(test)]
    fn new_without_worker(root: PathBuf) -> Self {
        Self::new_without_worker_with_mode(root, ExplorerPopupMode::OpenFiles)
    }

    #[cfg(test)]
    fn new_without_worker_for_project_root(root: PathBuf) -> Self {
        Self::new_without_worker_with_mode(root, ExplorerPopupMode::SelectProjectRoot)
    }

    #[cfg(test)]
    fn new_without_worker_with_mode(root: PathBuf, mode: ExplorerPopupMode) -> Self {
        let mut popup = Self {
            root,
            mode,
            expanded_dirs: HashSet::new(),
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            find_active: false,
            find_input: String::new(),
            copy_menu_active: false,
            rename_active: false,
            rename_input: String::new(),
            add_active: false,
            add_input: String::new(),
            delete_confirm_active: false,
            preview_lines: Vec::new(),
            preview_scroll: 0,
            preview_horizontal_scroll: 0,
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            current_preview_path: None,
            git_status_map: HashMap::new(),
        };
        popup.rebuild_entries();
        popup
    }

    fn render_preview_panel(
        &mut self,
        surface: &mut Surface,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        theme: &Theme,
    ) {
        let inner_w = w.saturating_sub(2);
        let content_h = h.saturating_sub(2);
        let has_preview = !self.preview_lines.is_empty();
        let default_style = CellStyle::default();
        let dim_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };
        self.clamp_preview_scroll(content_h);
        self.clamp_preview_horizontal_scroll(inner_w);

        for row in 0..h {
            if row == 0 {
                surface.put_str(x, y + row, "\u{250c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2510}", &default_style);
            } else if row == h - 1 {
                surface.put_str(x, y + row, "\u{2514}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2518}", &default_style);
            } else if has_preview {
                let line_idx = self.preview_scroll + (row - 1);
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                if line_idx < self.preview_lines.len() && (row - 1) < content_h {
                    let line = &self.preview_lines[line_idx];
                    let window =
                        slice_display_window(line, self.preview_horizontal_scroll, inner_w);
                    if let Some(spans) = self.preview_spans.get(&line_idx) {
                        render_highlighted_line_windowed(
                            surface,
                            (y + row, x + 1),
                            window.visible,
                            spans,
                            window.start_byte..window.end_byte,
                            inner_w,
                            theme,
                        );
                    } else {
                        surface.put_str(x + 1, y + row, window.visible, &dim_style);
                        let pad = inner_w.saturating_sub(window.used_width);
                        if pad > 0 {
                            surface.fill_region(
                                x + 1 + window.used_width,
                                y + row,
                                pad,
                                ' ',
                                &default_style,
                            );
                        }
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else {
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            }
        }
    }
}

fn tree_entry_display(entry: &TreeEntry, inner_w: usize) -> String {
    if inner_w == 0 {
        return String::new();
    }

    let mut label = entry.name.clone();
    if entry.is_dir {
        label.push('/');
    }

    let label_w = display_width(&label);
    let reserved_label_w = label_w.min(TREE_MIN_LABEL_COLUMNS).min(inner_w);
    let max_prefix_w = inner_w.saturating_sub(reserved_label_w);
    let raw_indent_w = entry.depth * TREE_INDENT_STEP;

    // Preserve full indent when it still leaves enough room for a readable label.
    if raw_indent_w <= max_prefix_w {
        return format!("{}{}", " ".repeat(raw_indent_w), label);
    }

    let collapse_prefix = collapse_indent_prefix(max_prefix_w);
    let collapse_prefix_w = display_width(collapse_prefix);
    let visible_indent_w = max_prefix_w.saturating_sub(collapse_prefix_w);

    format!(
        "{}{}{}",
        collapse_prefix,
        " ".repeat(visible_indent_w),
        label
    )
}

fn collapse_indent_prefix(max_prefix_w: usize) -> &'static str {
    match max_prefix_w {
        0 => "",
        1 => ".",
        2 => "..",
        _ => ".. ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Creates a temp directory with a known structure:
    ///   aaa_dir/        (directory)
    ///     inner.txt     (file)
    ///   bbb.txt         (file)
    ///   ccc.rs          (file)
    fn setup(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kaguya_test_ep_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join("aaa_dir")).unwrap();
        fs::write(dir.join("aaa_dir").join("inner.txt"), "inner content").unwrap();
        fs::write(dir.join("bbb.txt"), "bbb content").unwrap();
        fs::write(dir.join("ccc.rs"), "fn main() {}").unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // Entries (depth 0): aaa_dir (dir), bbb.txt (file), ccc.rs (file)
    // Dirs sorted first, then files, all alphabetical.

    #[test]
    fn initial_entries_ordered_dirs_first() {
        let dir = setup("init");
        let popup = ExplorerPopup::new_without_worker(dir.clone());

        assert_eq!(popup.entries.len(), 3);
        assert!(popup.entries[0].is_dir);
        assert_eq!(popup.entries[0].name, "aaa_dir");
        assert!(!popup.entries[1].is_dir);
        assert_eq!(popup.entries[1].name, "bbb.txt");
        assert!(!popup.entries[2].is_dir);
        assert_eq!(popup.entries[2].name, "ccc.rs");

        cleanup(&dir);
    }

    #[test]
    fn reveal_expands_ancestors_and_selects_file() {
        let dir = setup("reveal");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        let target = dir.join("aaa_dir").join("inner.txt");

        popup.expand_ancestors_of(&target);
        popup.rebuild_entries();
        popup.select_by_path(&target);

        // aaa_dir is now expanded, so inner.txt appears between aaa_dir and bbb.txt.
        assert_eq!(popup.entries.len(), 4);
        assert_eq!(popup.entries[popup.selected].name, "inner.txt");
        assert!(
            popup.entries[popup.selected].path.ends_with("inner.txt"),
            "selected path should be the revealed file, got {:?}",
            popup.entries[popup.selected].path
        );

        cleanup(&dir);
    }

    #[test]
    fn enter_on_file_returns_open_command() {
        let dir = setup("enter_file");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        // selected=0 → aaa_dir.  Move down to bbb.txt.
        popup.handle_key(key(KeyCode::Char('j')));
        assert_eq!(popup.selected, 1);

        let result = popup.handle_key(key(KeyCode::Enter));

        match result {
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorerPopup(ref path),
            ))) => {
                assert!(
                    path.ends_with("bbb.txt"),
                    "should reference bbb.txt, got: {}",
                    path
                );
            }
            _ => panic!("Expected OpenFileFromExplorerPopup for file entry"),
        }

        cleanup(&dir);
    }

    #[test]
    fn enter_on_dir_expands_not_opens() {
        let dir = setup("enter_dir");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        // selected=0 → aaa_dir
        let result = popup.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(result, EventResult::Consumed),
            "Dir entry should Consume, not emit Action"
        );

        // Directory should now be expanded with child entries inserted
        assert!(popup.expanded_dirs.contains(&dir.join("aaa_dir")));
        assert_eq!(popup.entries.len(), 4); // aaa_dir, inner.txt, bbb.txt, ccc.rs
        assert_eq!(popup.entries[1].name, "inner.txt");
        assert_eq!(popup.entries[1].depth, 1);

        cleanup(&dir);
    }

    #[test]
    fn command_prefix_matches_app_handler() {
        let dir = setup("cmd_prefix");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('j'))); // → bbb.txt
        let result = popup.handle_key(key(KeyCode::Enter));

        if let EventResult::Action(Action::App(AppAction::Buffer(
            BufferAction::OpenFileFromExplorerPopup(path),
        ))) = result
        {
            assert!(!path.is_empty());
            assert!(
                PathBuf::from(&path).exists(),
                "Extracted path should exist on disk: {}",
                path
            );
        } else {
            panic!("Expected OpenFileFromExplorerPopup");
        }

        cleanup(&dir);
    }

    #[test]
    fn right_arrow_opens_file_like_enter() {
        let dir = setup("right_arrow");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('j'))); // → bbb.txt
        let result = popup.handle_key(key(KeyCode::Right));

        match result {
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorerPopup(_),
            ))) => {}
            _ => panic!("Right arrow should open file"),
        }

        cleanup(&dir);
    }

    #[test]
    fn left_arrow_collapses_expanded_dir() {
        let dir = setup("left_arrow");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        // Expand aaa_dir with Enter
        popup.handle_key(key(KeyCode::Enter));
        assert!(!popup.expanded_dirs.is_empty());

        // Collapse with Left
        popup.handle_key(key(KeyCode::Left));
        assert!(popup.expanded_dirs.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn esc_returns_close_action() {
        let dir = setup("esc");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        let result = popup.handle_key(key(KeyCode::Esc));
        assert!(matches!(
            result,
            EventResult::Action(Action::Ui(UiAction::CloseExplorerPopup))
        ));

        cleanup(&dir);
    }

    #[test]
    fn find_mode_enter_exits_without_opening() {
        let dir = setup("find_enter");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        // Enter find mode, type "bbb"
        popup.handle_key(key(KeyCode::Char('/')));
        assert!(popup.find_active);
        popup.handle_key(key(KeyCode::Char('b')));
        popup.handle_key(key(KeyCode::Char('b')));
        popup.handle_key(key(KeyCode::Char('b')));

        // Enter should only exit find mode, not open anything
        let result = popup.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, EventResult::Consumed));
        assert!(!popup.find_active);
    }

    #[test]
    fn enter_after_find_opens_correct_file() {
        let dir = setup("find_open");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        // Find "ccc" — should filter to just ccc.rs and select it
        popup.handle_key(key(KeyCode::Char('/')));
        popup.handle_key(key(KeyCode::Char('c')));
        popup.handle_key(key(KeyCode::Char('c')));
        popup.handle_key(key(KeyCode::Char('c')));
        // Confirm find
        popup.handle_key(key(KeyCode::Enter));
        assert!(!popup.find_active);

        // Now selected should point to ccc.rs (the entry that matched)
        assert!(popup.selected < popup.entries.len());
        assert_eq!(popup.entries[popup.selected].name, "ccc.rs");

        // Press Enter to open
        let result = popup.handle_key(key(KeyCode::Enter));
        match result {
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorerPopup(ref path),
            ))) => {
                assert!(
                    path.ends_with("ccc.rs"),
                    "Should open ccc.rs, got: {}",
                    path
                );
            }
            _ => panic!("Expected OpenFileFromExplorerPopup for ccc.rs"),
        }

        cleanup(&dir);
    }

    #[test]
    fn find_mode_keeps_full_list_and_jumps_selection() {
        let dir = setup("find_jump");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('/')));
        popup.handle_key(key(KeyCode::Char('c')));
        popup.handle_key(key(KeyCode::Char('c')));
        popup.handle_key(key(KeyCode::Char('c')));

        assert_eq!(popup.entries.len(), 3);
        assert_eq!(popup.entries[popup.selected].name, "ccc.rs");

        cleanup(&dir);
    }

    #[test]
    fn find_mode_ctrl_and_arrow_navigation_work() {
        let dir = setup("find_nav");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('/')));
        popup.handle_key(ctrl_key('n'));
        assert_eq!(popup.selected, 1);
        popup.handle_key(ctrl_key('p'));
        assert_eq!(popup.selected, 0);
        popup.handle_key(key(KeyCode::Down));
        assert_eq!(popup.selected, 1);
        popup.handle_key(key(KeyCode::Up));
        assert_eq!(popup.selected, 0);

        cleanup(&dir);
    }

    #[test]
    fn find_mode_ctrl_w_ctrl_u_and_ctrl_k_edit_query() {
        let dir = setup("find_ctrl_edit");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('/')));
        for c in "src ui ccc".chars() {
            popup.handle_key(key(KeyCode::Char(c)));
        }
        popup.handle_key(ctrl_key('w'));
        assert_eq!(popup.find_input, "src ui ");
        popup.handle_key(ctrl_key('u'));
        assert!(popup.find_input.is_empty());
        for c in "tmp new".chars() {
            popup.handle_key(key(KeyCode::Char(c)));
        }
        popup.handle_key(ctrl_key('k'));
        assert!(popup.find_input.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn copy_menu_cc_copies_selected_full_path() {
        let dir = setup("copy_path");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let _ = popup.handle_key(key(KeyCode::Char('c')));
        let result = popup.handle_key(key(KeyCode::Char('c')));

        match result {
            EventResult::Action(Action::App(AppAction::Integration(
                IntegrationAction::CopyToClipboard { text, description },
            ))) => {
                assert!(text.ends_with("bbb.txt"));
                assert_eq!(description, "path");
            }
            _ => panic!("Expected CopyToClipboard path action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn copy_menu_cd_copies_directory_path() {
        let dir = setup("copy_dir");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let _ = popup.handle_key(key(KeyCode::Char('c')));
        let result = popup.handle_key(key(KeyCode::Char('d')));

        match result {
            EventResult::Action(Action::App(AppAction::Integration(
                IntegrationAction::CopyToClipboard { text, description },
            ))) => {
                assert_eq!(PathBuf::from(text), dir);
                assert_eq!(description, "dir path");
            }
            _ => panic!("Expected CopyToClipboard dir path action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn copy_menu_cf_copies_file_name() {
        let dir = setup("copy_name");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let _ = popup.handle_key(key(KeyCode::Char('c')));
        let result = popup.handle_key(key(KeyCode::Char('f')));

        match result {
            EventResult::Action(Action::App(AppAction::Integration(
                IntegrationAction::CopyToClipboard { text, description },
            ))) => {
                assert_eq!(text, "bbb.txt");
                assert_eq!(description, "file name");
            }
            _ => panic!("Expected CopyToClipboard file name action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn copy_menu_invalid_second_key_is_consumed_and_closes_menu() {
        let dir = setup("copy_invalid");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        let _ = popup.handle_key(key(KeyCode::Char('c')));
        let first = popup.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(first, EventResult::Consumed));

        let second = popup.handle_key(key(KeyCode::Char('j')));
        assert!(matches!(second, EventResult::Consumed));
        assert_eq!(popup.entries[popup.selected].name, "bbb.txt");

        cleanup(&dir);
    }

    #[test]
    fn rename_selected_file_with_r() {
        let dir = setup("rename_file");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let _ = popup.handle_key(key(KeyCode::Char('r')));
        let _ = popup.handle_key(ctrl_key('u'));
        for c in "renamed.txt".chars() {
            let _ = popup.handle_key(key(KeyCode::Char(c)));
        }
        let result = popup.handle_key(key(KeyCode::Enter));

        assert!(dir.join("renamed.txt").exists());
        assert!(!dir.join("bbb.txt").exists());
        assert_eq!(popup.entries[popup.selected].name, "renamed.txt");
        assert!(matches!(
            result,
            EventResult::Action(Action::App(AppAction::Integration(
                IntegrationAction::ShowMessage(_),
            )))
        ));

        cleanup(&dir);
    }

    #[test]
    fn add_file_and_dir_with_a() {
        let dir = setup("add_entries");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        let _ = popup.handle_key(key(KeyCode::Char('a')));
        for c in "new.txt".chars() {
            let _ = popup.handle_key(key(KeyCode::Char(c)));
        }
        let _ = popup.handle_key(key(KeyCode::Enter));
        assert!(dir.join("aaa_dir").join("new.txt").exists());
        assert_eq!(popup.entries[popup.selected].name, "new.txt");

        popup.handle_key(key(KeyCode::Char('h'))); // back to aaa_dir
        assert_eq!(popup.entries[popup.selected].name, "aaa_dir");
        let _ = popup.handle_key(key(KeyCode::Char('a')));
        for c in "inner_dir/".chars() {
            let _ = popup.handle_key(key(KeyCode::Char(c)));
        }
        let _ = popup.handle_key(key(KeyCode::Enter));
        assert!(dir.join("aaa_dir").join("inner_dir").is_dir());
        assert_eq!(popup.entries[popup.selected].name, "inner_dir");

        cleanup(&dir);
    }

    #[test]
    fn delete_confirmation_requires_y() {
        let dir = setup("delete_confirm");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let _ = popup.handle_key(key(KeyCode::Char('d')));
        let _ = popup.handle_key(key(KeyCode::Char('n')));
        assert!(dir.join("bbb.txt").exists());

        let _ = popup.handle_key(key(KeyCode::Char('d')));
        let _ = popup.handle_key(key(KeyCode::Char('y')));
        assert!(!dir.join("bbb.txt").exists());

        cleanup(&dir);
    }

    #[test]
    fn ctrl_r_in_open_files_mode_emits_change_root_for_selected_directory() {
        let dir = setup("ctrl_r_dir");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        let result = popup.handle_key(ctrl_key('r'));
        match result {
            EventResult::Action(Action::App(AppAction::Project(
                ProjectAction::ChangeProjectRoot(path),
            ))) => {
                assert_eq!(PathBuf::from(path), dir.join("aaa_dir"));
            }
            _ => panic!("Expected ChangeProjectRoot action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn ctrl_r_on_file_emits_parent_directory_as_new_root() {
        let dir = setup("ctrl_r_file");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        let result = popup.handle_key(ctrl_key('r'));
        match result {
            EventResult::Action(Action::App(AppAction::Project(
                ProjectAction::ChangeProjectRoot(path),
            ))) => {
                assert_eq!(PathBuf::from(path), dir);
            }
            _ => panic!("Expected ChangeProjectRoot action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn project_root_mode_enter_changes_root_and_space_toggles_directory() {
        let dir = setup("project_root_mode");
        let mut popup = ExplorerPopup::new_without_worker_for_project_root(dir.clone());

        let toggle = popup.handle_key(key(KeyCode::Char(' ')));
        assert!(matches!(toggle, EventResult::Consumed));
        assert!(popup.expanded_dirs.contains(&dir.join("aaa_dir")));

        let result = popup.handle_key(key(KeyCode::Enter));
        match result {
            EventResult::Action(Action::App(AppAction::Project(
                ProjectAction::ChangeProjectRoot(path),
            ))) => {
                assert_eq!(PathBuf::from(path), dir.join("aaa_dir"));
            }
            _ => panic!("Expected ChangeProjectRoot action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn project_root_mode_left_at_top_level_moves_popup_root_to_parent() {
        let dir = setup("project_root_parent_nav");
        let mut popup = ExplorerPopup::new_without_worker_for_project_root(dir.clone());
        let old_root_name = dir.file_name().unwrap().to_string_lossy().to_string();

        let result = popup.handle_key(key(KeyCode::Left));
        assert!(matches!(result, EventResult::Consumed));
        assert_eq!(popup.root, dir.parent().unwrap().to_path_buf());
        assert_eq!(popup.entries[popup.selected].name, old_root_name);
        assert_eq!(popup.entries[popup.selected].depth, 0);

        cleanup(&dir);
    }

    #[test]
    fn project_root_mode_left_on_nested_entry_stays_within_tree() {
        let dir = setup("project_root_nested_parent_entry");
        let mut popup = ExplorerPopup::new_without_worker_for_project_root(dir.clone());

        let _ = popup.handle_key(key(KeyCode::Char(' '))); // expand aaa_dir
        let _ = popup.handle_key(key(KeyCode::Char('j'))); // inner.txt
        assert_eq!(popup.entries[popup.selected].name, "inner.txt");
        assert_eq!(popup.entries[popup.selected].depth, 1);

        let result = popup.handle_key(key(KeyCode::Left));
        assert!(matches!(result, EventResult::Consumed));
        assert_eq!(popup.root, dir);
        assert_eq!(popup.entries[popup.selected].name, "aaa_dir");
        assert_eq!(popup.entries[popup.selected].depth, 0);

        cleanup(&dir);
    }

    #[test]
    fn mouse_scroll_clamps_preview_and_resets_on_selection_change() {
        let dir = setup("mouse_preview_scroll");
        let long_body: String = (0..120).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.join("bbb.txt"), long_body).unwrap();
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());

        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt
        assert_eq!(popup.entries[popup.selected].name, "bbb.txt");
        assert_eq!(popup.preview_scroll, 0);

        let cols = 100;
        let rows = 20;
        let content_h = ExplorerPopup::preview_content_height_for_surface(cols, rows).unwrap();
        let max_scroll = popup.preview_max_scroll(content_h);

        for _ in 0..200 {
            let result = popup.handle_mouse_scroll(MouseEventKind::ScrollDown, cols, rows);
            assert!(matches!(result, EventResult::Consumed));
        }
        assert_eq!(popup.preview_scroll, max_scroll);

        popup.handle_key(key(KeyCode::Char('j'))); // ccc.rs
        assert_eq!(popup.preview_scroll, 0);

        cleanup(&dir);
    }

    #[test]
    fn mouse_scroll_ignored_without_split_preview_panel() {
        let dir = setup("mouse_preview_ignored");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        let result = popup.handle_mouse_scroll(MouseEventKind::ScrollDown, 40, 20);
        assert!(matches!(result, EventResult::Ignored));
        cleanup(&dir);
    }

    #[test]
    fn shift_h_l_scrolls_preview_horizontally() {
        let dir = setup("shift_h_l_preview");
        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        popup.handle_key(key(KeyCode::Char('j'))); // bbb.txt

        assert_eq!(popup.preview_horizontal_scroll, 0);
        let _ = popup.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert_eq!(popup.preview_horizontal_scroll, HORIZONTAL_SCROLL_COLS);
        let _ = popup.handle_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(popup.preview_horizontal_scroll, 0);

        cleanup(&dir);
    }

    /// Empty directory: all keys except Esc should be Consumed.
    #[test]
    fn empty_dir_enter_consumed() {
        let dir = std::env::temp_dir().join("kaguya_test_ep_empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut popup = ExplorerPopup::new_without_worker(dir.clone());
        assert!(popup.entries.is_empty());

        // Enter on empty → Consumed (stuck in popup)
        let result = popup.handle_key(key(KeyCode::Enter));
        assert!(matches!(result, EventResult::Consumed));

        // Only Esc works
        let result = popup.handle_key(key(KeyCode::Esc));
        assert!(matches!(
            result,
            EventResult::Action(Action::Ui(UiAction::CloseExplorerPopup))
        ));

        cleanup(&dir);
    }

    fn row_text(surface: &Surface, y: usize, x: usize, w: usize) -> String {
        let mut out = String::new();
        for col in x..(x + w) {
            let symbol = &surface.get(col, y).symbol;
            if symbol.is_empty() {
                continue;
            }
            out.push_str(symbol);
        }
        out.trim_end().to_string()
    }

    fn test_entry(name: &str, depth: usize, is_dir: bool) -> TreeEntry {
        TreeEntry {
            name: name.to_string(),
            path: PathBuf::from(format!("/tmp/{name}")),
            is_dir,
            depth,
            git_status: None,
        }
    }

    #[test]
    fn tree_entry_display_collapses_deep_indent_and_keeps_filename_visible() {
        let mut popup = ExplorerPopup::new_without_worker(PathBuf::from("/tmp"));
        popup.entries = vec![test_entry("very_long_filename.rs", 10, false)];
        popup.selected = 0;
        popup.scroll_offset = 0;

        let mut surface = Surface::new(16, 6);
        popup.render_tree_panel(&mut surface, 0, 0, 16, 6);

        // Inside border: x=1..14, first content row at y=1.
        let line = row_text(&surface, 1, 1, 14);
        assert!(
            line.starts_with(".."),
            "expected collapsed indent marker: {line}"
        );
        assert!(
            line.contains("very") || line.contains("long") || line.contains("file"),
            "expected visible filename segment: {line}"
        );
    }

    #[test]
    fn tree_entry_display_keeps_shallow_indent_unchanged_when_space_allows() {
        let mut popup = ExplorerPopup::new_without_worker(PathBuf::from("/tmp"));
        popup.entries = vec![test_entry("alpha.txt", 1, false)];
        popup.selected = 0;
        popup.scroll_offset = 0;

        let mut surface = Surface::new(32, 6);
        popup.render_tree_panel(&mut surface, 0, 0, 32, 6);

        let line = row_text(&surface, 1, 1, 30);
        assert!(line.starts_with("  alpha.txt"), "unexpected line: {line}");
    }

    #[test]
    fn tree_entry_display_handles_very_narrow_width_without_hiding_basename() {
        let mut popup = ExplorerPopup::new_without_worker(PathBuf::from("/tmp"));
        popup.entries = vec![test_entry("abcdef", 12, false)];
        popup.selected = 0;
        popup.scroll_offset = 0;

        let mut surface = Surface::new(5, 6);
        popup.render_tree_panel(&mut surface, 0, 0, 5, 6);

        // Inner width is 3, so row should prefer basename over indent.
        let line = row_text(&surface, 1, 1, 3);
        assert_eq!(line, "abc");
    }
}

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::command::git::{GitFileEntry, GitFileStatus, dir_git_status};
use crate::input::action::{Action, AppAction, BufferAction, IntegrationAction, WorkspaceAction};
use crate::input::chord::KeyState;
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
use crate::ui::text::{slice_display_window, truncate_to_width};
use crate::ui::text_input::delete_prev_word_input;
use crate::ui::views::text_view::render_highlighted_line_windowed;

const PREVIEW_MAX_LINES: usize = 500;
const PREVIEW_HSCROLL_STEP: usize = 8;

struct DirEntry {
    name: String,
    is_dir: bool,
    git_status: Option<GitFileStatus>,
    is_repo_header: bool,
    diff_stats: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExplorerMode {
    AllFiles,
    ChangedOnly,
    /// Files that differ between a base branch and HEAD. The branch name
    /// and file list are stored separately on `Explorer`.
    BranchCompare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewKind {
    None,
    File,
    Dir,
}

pub struct Explorer {
    mode: ExplorerMode,
    current_dir: PathBuf,
    entries: Vec<DirEntry>,
    visible_entries: Vec<usize>,
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
    project_root: PathBuf,
    git_status_map: HashMap<String, GitFileStatus>,
    // Populated only when mode == BranchCompare.
    branch_compare_base: Option<String>,
    branch_compare_files: Vec<GitFileEntry>,
    // preview
    preview_mode: bool,
    preview_lines: Vec<String>,
    preview_spans: HashMap<usize, Vec<HighlightSpan>>,
    preview_path: Option<PathBuf>,
    preview_kind: PreviewKind,
    preview_scroll: usize,
    preview_horizontal_scroll: usize,
    preview_image: Option<(PathBuf, std::sync::Arc<crate::ui::image::EncodedImage>)>,
    preview_image_cache: HashMap<PathBuf, std::sync::Arc<crate::ui::image::EncodedImage>>,
    pending_image_request: Option<crate::ui::image::ImageRenderRequest>,
}

impl Explorer {
    pub fn new(
        dir: PathBuf,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
    ) -> Self {
        Self::new_with_mode(dir, project_root, git_status_map, ExplorerMode::AllFiles)
    }

    pub fn new_changed_only(
        dir: PathBuf,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
    ) -> Self {
        Self::new_with_mode(dir, project_root, git_status_map, ExplorerMode::ChangedOnly)
    }

    fn new_with_mode(
        dir: PathBuf,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
        mode: ExplorerMode,
    ) -> Self {
        let mut explorer = Self {
            mode,
            current_dir: dir,
            entries: Vec::new(),
            visible_entries: Vec::new(),
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
            project_root: project_root.to_path_buf(),
            git_status_map: git_status_map.clone(),
            branch_compare_base: None,
            branch_compare_files: Vec::new(),
            preview_mode: false,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_path: None,
            preview_kind: PreviewKind::None,
            preview_scroll: 0,
            preview_horizontal_scroll: 0,
            preview_image: None,
            preview_image_cache: HashMap::new(),
            pending_image_request: None,
        };
        explorer.read_directory();
        explorer
    }

    /// Sidebar showing files that differ between `base_branch` and HEAD.
    /// The file list is supplied directly; refresh later via
    /// [`apply_branch_diff_files`] to update without rebuilding.
    pub fn new_branch_compare(
        project_root: PathBuf,
        base_branch: String,
        files: Vec<GitFileEntry>,
    ) -> Self {
        let mut explorer = Self {
            mode: ExplorerMode::BranchCompare,
            current_dir: project_root.clone(),
            entries: Vec::new(),
            visible_entries: Vec::new(),
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
            project_root,
            git_status_map: HashMap::new(),
            branch_compare_base: Some(base_branch),
            branch_compare_files: files,
            preview_mode: false,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_path: None,
            preview_kind: PreviewKind::None,
            preview_scroll: 0,
            preview_horizontal_scroll: 0,
            preview_image: None,
            preview_image_cache: HashMap::new(),
            pending_image_request: None,
        };
        explorer.read_directory();
        explorer
    }

    pub fn is_branch_compare(&self) -> bool {
        self.mode == ExplorerMode::BranchCompare
    }

    pub fn branch_compare_base(&self) -> Option<&str> {
        self.branch_compare_base.as_deref()
    }

    /// Replace the branch-compare file list and reread entries. Preserves
    /// the selected file by path when possible.
    pub fn apply_branch_diff_files(&mut self, files: Vec<GitFileEntry>) {
        if !self.is_branch_compare() {
            return;
        }
        let selected_name = self.selected_name().map(|s| s.to_string());
        self.branch_compare_files = files;
        self.read_directory();
        if let Some(name) = selected_name {
            self.select_by_name(&name);
        }
    }

    /// Enable or disable the preview pane. When enabled, the editor area shows
    /// the file/dir under the cursor instead of the active buffer. The actual
    /// open buffer is never modified.
    pub fn set_preview_mode(&mut self, on: bool) {
        if self.preview_mode == on {
            return;
        }
        self.preview_mode = on;
        if on {
            self.update_preview();
        } else {
            self.clear_preview();
        }
    }

    pub fn preview_mode_active(&self) -> bool {
        self.preview_mode
    }

    fn clear_preview(&mut self) {
        self.preview_lines.clear();
        self.preview_spans.clear();
        self.preview_path = None;
        self.preview_kind = PreviewKind::None;
        self.preview_scroll = 0;
        self.preview_horizontal_scroll = 0;
        self.preview_image = None;
    }

    pub fn take_pending_image_request(&mut self) -> Option<crate::ui::image::ImageRenderRequest> {
        self.pending_image_request.take()
    }

    fn try_load_preview_image(&mut self, path: &Path) -> bool {
        if !crate::ui::image::is_image_path(path) {
            return false;
        }
        let supported = crate::ui::image::supports_kitty_graphics();
        crate::ui::image::debug_log(&format!(
            "explorer: try_load_preview_image path={:?} kitty_supported={}",
            path, supported
        ));
        if !supported {
            return false;
        }
        let key = path.to_path_buf();
        if let Some(cached) = self.preview_image_cache.get(&key) {
            self.preview_image = Some((key, cached.clone()));
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.preview_kind = PreviewKind::File;
            return true;
        }
        match crate::ui::image::load_and_encode(path, 1024) {
            Some(img) => {
                let arc = std::sync::Arc::new(img);
                self.preview_image_cache.insert(key.clone(), arc.clone());
                self.preview_image = Some((key, arc));
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.preview_kind = PreviewKind::File;
                true
            }
            None => false,
        }
    }

    fn update_preview(&mut self) {
        if !self.preview_mode {
            return;
        }
        let Some(&entry_idx) = self.visible_entries.get(self.selected) else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.preview_path = None;
            self.preview_kind = PreviewKind::None;
            self.preview_image = None;
            return;
        };
        let entry = &self.entries[entry_idx];
        if entry.is_repo_header {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.preview_path = None;
            self.preview_kind = PreviewKind::None;
            self.preview_image = None;
            return;
        }
        let path = self.current_dir.join(&entry.name);
        if self.preview_path.as_ref() == Some(&path) {
            return;
        }

        self.preview_scroll = 0;
        self.preview_horizontal_scroll = 0;
        self.preview_spans.clear();
        self.preview_image = None;

        if entry.is_dir {
            self.preview_lines = build_dir_listing(&path);
            self.preview_kind = PreviewKind::Dir;
        } else if self.try_load_preview_image(&path) {
            // Image preview state set above.
        } else {
            let (lines, spans) = read_file_preview(&path);
            self.preview_lines = lines;
            self.preview_spans = spans;
            self.preview_kind = PreviewKind::File;
        }
        self.preview_path = Some(path);
    }

    pub fn render_preview(
        &mut self,
        surface: &mut Surface,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        theme: &Theme,
    ) {
        self.pending_image_request = None;
        if width == 0 || height == 0 {
            return;
        }

        let default_style = CellStyle::default();
        let dim_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };

        // Title row.
        let title = match (&self.preview_kind, self.preview_path.as_ref()) {
            (PreviewKind::File, Some(p)) => {
                let rel = p.strip_prefix(&self.project_root).unwrap_or(p);
                format!("PREVIEW: {}", rel.to_string_lossy())
            }
            (PreviewKind::Dir, Some(p)) => {
                let rel = p.strip_prefix(&self.project_root).unwrap_or(p);
                format!("PREVIEW: {}/", rel.to_string_lossy())
            }
            _ => "PREVIEW".to_string(),
        };
        let (truncated_title, used) = truncate_to_width(&title, width);
        surface.put_str(x, y, truncated_title, &dim_style);
        if used < width {
            surface.fill_region(x + used, y, width - used, ' ', &dim_style);
        }

        let body_h = height.saturating_sub(1);
        if body_h == 0 {
            return;
        }
        let body_y = y + 1;

        if let Some((path, data)) = self.preview_image.clone() {
            for row in 0..body_h {
                surface.fill_region(x, body_y + row, width, ' ', &default_style);
            }
            let (cell_cols, cell_rows) =
                crate::ui::image::fit_cells(data.width, data.height, width as u16, body_h as u16);
            self.pending_image_request = Some(crate::ui::image::ImageRenderRequest {
                key: path,
                col: x as u16,
                row: body_y as u16,
                cell_cols,
                cell_rows,
                data,
            });
            return;
        }

        // Clamp vertical scroll.
        let max_vscroll = self.preview_lines.len().saturating_sub(body_h);
        if self.preview_scroll > max_vscroll {
            self.preview_scroll = max_vscroll;
        }

        let highlight_enabled = self.preview_kind == PreviewKind::File;

        for row in 0..body_h {
            let line_idx = self.preview_scroll + row;
            let screen_row = body_y + row;
            if line_idx < self.preview_lines.len() {
                let line = &self.preview_lines[line_idx];
                let window = slice_display_window(line, self.preview_horizontal_scroll, width);
                if highlight_enabled && let Some(spans) = self.preview_spans.get(&line_idx) {
                    render_highlighted_line_windowed(
                        surface,
                        (screen_row, x),
                        window.visible,
                        spans,
                        window.start_byte..window.end_byte,
                        width,
                        theme,
                    );
                    let pad = width.saturating_sub(window.used_width);
                    if pad > 0 {
                        surface.fill_region(
                            x + window.used_width,
                            screen_row,
                            pad,
                            ' ',
                            &default_style,
                        );
                    }
                } else {
                    let style = if self.preview_kind == PreviewKind::Dir && line_idx == 0 {
                        dim_style
                    } else {
                        default_style
                    };
                    surface.put_str(x, screen_row, window.visible, &style);
                    let pad = width.saturating_sub(window.used_width);
                    if pad > 0 {
                        surface.fill_region(
                            x + window.used_width,
                            screen_row,
                            pad,
                            ' ',
                            &default_style,
                        );
                    }
                }
            } else {
                surface.fill_region(x, screen_row, width, ' ', &default_style);
            }
        }
    }

    fn read_directory(&mut self) {
        self.entries.clear();
        self.visible_entries.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.find_active = false;
        self.find_input.clear();
        self.copy_menu_active = false;
        self.rename_active = false;
        self.rename_input.clear();
        self.add_active = false;
        self.add_input.clear();
        self.delete_confirm_active = false;

        if self.mode == ExplorerMode::ChangedOnly {
            self.read_changed_entries();
            return;
        }

        if self.mode == ExplorerMode::BranchCompare {
            self.read_branch_compare_entries();
            return;
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(&self.current_dir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip dotfiles
                if name.starts_with('.') {
                    continue;
                }
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let full_path = entry.path();
                let rel_path = full_path
                    .strip_prefix(&self.project_root)
                    .unwrap_or(&full_path)
                    .to_string_lossy()
                    .to_string();

                let git_status = if is_dir {
                    let prefix = if rel_path.ends_with('/') {
                        rel_path.clone()
                    } else {
                        format!("{}/", rel_path)
                    };
                    dir_git_status(&self.git_status_map, &prefix)
                } else {
                    self.git_status_map.get(&rel_path).copied()
                };

                if is_dir {
                    dirs.push(DirEntry {
                        name,
                        is_dir: true,
                        git_status,
                        is_repo_header: false,
                        diff_stats: None,
                    });
                } else {
                    files.push(DirEntry {
                        name,
                        is_dir: false,
                        git_status,
                        is_repo_header: false,
                        diff_stats: None,
                    });
                }
            }
        }

        sort_by_name_case_insensitive(&mut dirs, |entry| &entry.name);
        sort_by_name_case_insensitive(&mut files, |entry| &entry.name);

        self.entries.extend(dirs);
        self.entries.extend(files);

        self.visible_entries = (0..self.entries.len()).collect();
    }

    fn read_changed_entries(&mut self) {
        // Detect multi-repo: check if paths contain a "/" and group by first component
        // Only treat as a repo group if the subdirectory contains a .git marker
        let mut repo_groups: std::collections::BTreeMap<String, Vec<(String, GitFileStatus)>> =
            std::collections::BTreeMap::new();
        let mut ungrouped: Vec<(String, GitFileStatus)> = Vec::new();

        for (path, status) in &self.git_status_map {
            if let Some(slash_idx) = path.find('/') {
                let repo_name = &path[..slash_idx];
                let repo_dir = self.project_root.join(repo_name);
                let dot_git = repo_dir.join(".git");
                if dot_git.is_dir() || dot_git.is_file() {
                    repo_groups
                        .entry(repo_name.to_string())
                        .or_default()
                        .push((path.clone(), *status));
                    continue;
                }
            }
            ungrouped.push((path.clone(), *status));
        }

        let is_multi_repo = !repo_groups.is_empty() && ungrouped.is_empty();

        // Build a (path → (additions, deletions)) lookup for the project root.
        // Aggregates staged + unstaged numstat for each path.
        let stats_lookup = collect_diff_stats(&self.project_root);

        if is_multi_repo {
            for (repo_name, mut files) in repo_groups {
                // Insert repo header
                self.entries.push(DirEntry {
                    name: repo_name.clone(),
                    is_dir: true,
                    git_status: None,
                    is_repo_header: true,
                    diff_stats: None,
                });
                let repo_root = self.project_root.join(&repo_name);
                let repo_stats = collect_diff_stats(&repo_root);
                sort_by_name_case_insensitive(&mut files, |(path, _)| path);
                for (path, status) in files {
                    let sub_path = path
                        .strip_prefix(&format!("{}/", repo_name))
                        .unwrap_or(&path)
                        .to_string();
                    let stats = repo_stats.get(&sub_path).copied().or_else(|| {
                        if status == GitFileStatus::Untracked {
                            Some((count_file_lines(&repo_root.join(&sub_path)), 0))
                        } else {
                            None
                        }
                    });
                    self.entries.push(DirEntry {
                        name: path,
                        is_dir: false,
                        git_status: Some(status),
                        is_repo_header: false,
                        diff_stats: stats,
                    });
                }
            }
        } else {
            // Single repo or mixed: flat list
            let mut all_files: Vec<(String, GitFileStatus)> = ungrouped;
            for (_, files) in repo_groups {
                all_files.extend(files);
            }
            sort_by_name_case_insensitive(&mut all_files, |(path, _)| path);
            for (path, status) in all_files {
                let stats = stats_lookup.get(&path).copied().or_else(|| {
                    if status == GitFileStatus::Untracked {
                        Some((count_file_lines(&self.project_root.join(&path)), 0))
                    } else {
                        None
                    }
                });
                self.entries.push(DirEntry {
                    name: path,
                    is_dir: false,
                    git_status: Some(status),
                    is_repo_header: false,
                    diff_stats: stats,
                });
            }
        }
        self.visible_entries = (0..self.entries.len()).collect();
    }

    fn read_branch_compare_entries(&mut self) {
        let mut files: Vec<&GitFileEntry> = self.branch_compare_files.iter().collect();
        sort_by_name_case_insensitive(&mut files, |entry| entry.path.as_str());
        for entry in files {
            let status = branch_diff_status_char_to_file_status(entry.status_char);
            self.entries.push(DirEntry {
                name: entry.path.clone(),
                is_dir: false,
                git_status: Some(status),
                is_repo_header: false,
                diff_stats: Some((entry.additions, entry.deletions)),
            });
        }
        self.visible_entries = (0..self.entries.len()).collect();
    }

    pub fn handle_key(&mut self, key: KeyEvent, key_state: &KeyState) -> EventResult {
        // When a chord is in progress, yield so the chord resolves
        if *key_state != KeyState::Normal {
            return EventResult::Ignored;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return EventResult::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::ToggleExplorer,
            )));
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

        // Preview-pane scroll: only intercept Shift+J/K/H/L while preview is on,
        // so other Shift-keys keep their default fallthrough behavior.
        if self.preview_mode && key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::Char('J') | KeyCode::Down => {
                    self.preview_scroll = self.preview_scroll.saturating_add(1);
                    return EventResult::Consumed;
                }
                KeyCode::Char('K') | KeyCode::Up => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1);
                    return EventResult::Consumed;
                }
                KeyCode::Char('L') | KeyCode::Right => {
                    self.preview_horizontal_scroll = self
                        .preview_horizontal_scroll
                        .saturating_add(PREVIEW_HSCROLL_STEP);
                    return EventResult::Consumed;
                }
                KeyCode::Char('H') | KeyCode::Left => {
                    self.preview_horizontal_scroll = self
                        .preview_horizontal_scroll
                        .saturating_sub(PREVIEW_HSCROLL_STEP);
                    return EventResult::Consumed;
                }
                _ => {}
            }
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
                KeyCode::Char('f') => {
                    return self.enter_selected();
                }
                KeyCode::Char('b') => {
                    self.go_parent();
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
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
            KeyCode::Char('h') | KeyCode::Left => {
                self.go_parent();
                EventResult::Consumed
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => self.enter_selected(),
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
            KeyCode::Char('p') => {
                self.set_preview_mode(!self.preview_mode);
                EventResult::Consumed
            }
            KeyCode::Esc => EventResult::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::ToggleExplorer,
            ))),
            KeyCode::Char(' ') => EventResult::Ignored, // let Space chord start
            _ => EventResult::Consumed,
        }
    }

    pub fn set_git_status_map(&mut self, git_status_map: &HashMap<String, GitFileStatus>) {
        self.git_status_map = git_status_map.clone();

        if self.mode == ExplorerMode::ChangedOnly {
            let selected_name = self.selected_name().map(ToString::to_string);
            self.read_directory();
            if let Some(name) = selected_name {
                self.select_by_name(&name);
            }
            return;
        }

        let statuses: Vec<Option<GitFileStatus>> = self
            .entries
            .iter()
            .map(|entry| self.entry_git_status(&entry.name, entry.is_dir))
            .collect();
        for (entry, status) in self.entries.iter_mut().zip(statuses) {
            entry.git_status = status;
        }
    }

    fn entry_git_status(&self, entry_name: &str, is_dir: bool) -> Option<GitFileStatus> {
        let full_path = self.current_dir.join(entry_name);
        let rel_path = full_path
            .strip_prefix(&self.project_root)
            .unwrap_or(&full_path)
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
                KeyCode::Char('b') => {
                    self.go_parent();
                    EventResult::Consumed
                }
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
            KeyCode::Left => {
                self.go_parent();
                EventResult::Consumed
            }
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
        for (visible_idx, &entry_idx) in self.visible_entries.iter().enumerate() {
            if let Some((score, _)) = fuzzy_match(&self.entries[entry_idx].name, &self.find_input)
                && best.is_none_or(|(best_score, _)| score > best_score)
            {
                best = Some((score, visible_idx));
            }
        }
        if let Some((_, visible_idx)) = best {
            self.selected = visible_idx;
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
        let Some(name) = self.selected_entry().map(|entry| entry.name.clone()) else {
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
        if self.selected_entry().is_none() {
            return EventResult::Consumed;
        }
        self.delete_confirm_active = true;
        EventResult::Consumed
    }

    fn apply_rename(&mut self) -> EventResult {
        let Some(entry) = self.selected_entry() else {
            return self.show_message("Rename failed: no selection".to_string());
        };
        let source_name = entry.name.clone();
        let source_path = self.current_dir.join(&source_name);
        let new_name = self.rename_input.trim().to_string();
        if !is_valid_single_name(&new_name) {
            return self.show_message("Rename failed: invalid name".to_string());
        }
        if new_name == source_name {
            return self.show_message("Rename skipped: unchanged".to_string());
        }
        let dest_path = self.current_dir.join(&new_name);
        if dest_path.exists() {
            return self.show_message(format!("Rename failed: '{}' already exists", new_name));
        }
        match std::fs::rename(&source_path, &dest_path) {
            Ok(()) => {
                self.read_directory();
                self.select_by_name(&new_name);
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

        let rel_path = std::path::PathBuf::from(rel);
        let target = self.current_dir.join(&rel_path);
        if target.exists() {
            return self.show_message(format!("Add failed: '{}' already exists", rel));
        }

        let result = if is_dir {
            std::fs::create_dir_all(&target)
        } else {
            let mkdir = match target.parent() {
                Some(parent) if parent != self.current_dir.as_path() => {
                    std::fs::create_dir_all(parent)
                }
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
                // Navigate into the deepest parent dir of the new entry, then select the leaf.
                let leaf = rel_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string());
                if let Some(parent) = rel_path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    self.current_dir = self.current_dir.join(parent);
                }
                self.read_directory();
                if let Some(name) = leaf.as_deref() {
                    self.select_by_name(name);
                }
                let kind = if is_dir { "directory" } else { "file" };
                self.show_message(format!("Created {} {}", kind, rel))
            }
            Err(e) => self.show_message(format!("Add failed: {}", e)),
        }
    }

    fn apply_delete(&mut self) -> EventResult {
        if self.visible_entries.is_empty() {
            return self.show_message("Delete failed: no selection".to_string());
        }
        let entry_idx = self.visible_entries[self.selected];
        let entry_name = self.entries[entry_idx].name.clone();
        let entry_is_dir = self.entries[entry_idx].is_dir;
        let target = self.current_dir.join(&entry_name);
        let old_selected = self.selected;

        let result = if entry_is_dir {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };

        match result {
            Ok(()) => {
                self.read_directory();
                if !self.visible_entries.is_empty() {
                    self.selected = old_selected.min(self.visible_entries.len() - 1);
                }
                self.show_message(format!("Deleted {}", entry_name))
            }
            Err(e) => self.show_message(format!("Delete failed: {}", e)),
        }
    }

    fn move_down(&mut self) {
        if !self.visible_entries.is_empty() && self.selected + 1 < self.visible_entries.len() {
            self.selected += 1;
            self.update_preview();
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.update_preview();
        }
    }

    fn go_parent(&mut self) {
        if self.mode == ExplorerMode::ChangedOnly || self.mode == ExplorerMode::BranchCompare {
            return;
        }
        if let Some(parent) = self.current_dir.parent() {
            let old_name = self
                .current_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.current_dir = parent.to_path_buf();
            self.read_directory();
            if let Some(name) = old_name {
                self.select_by_name(&name);
            }
            self.update_preview();
        }
    }

    fn enter_selected(&mut self) -> EventResult {
        if self.visible_entries.is_empty() {
            return EventResult::Consumed;
        }
        let entry_idx = self.visible_entries[self.selected];
        let entry = &self.entries[entry_idx];
        if entry.is_repo_header {
            return EventResult::Consumed;
        }
        if entry.is_dir {
            let new_dir = self.current_dir.join(&entry.name);
            self.current_dir = new_dir;
            self.read_directory();
            self.update_preview();
            EventResult::Consumed
        } else {
            let path = self.current_dir.join(&entry.name);
            let path_str = path.to_string_lossy().to_string();
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorer(path_str),
            )))
        }
    }

    fn selected_entry(&self) -> Option<&DirEntry> {
        if self.visible_entries.is_empty() {
            return None;
        }
        let idx = self.visible_entries[self.selected];
        self.entries.get(idx)
    }

    fn copy_selected_full_path(&self) -> EventResult {
        let Some(entry) = self.selected_entry() else {
            return EventResult::Consumed;
        };
        let path = self.current_dir.join(&entry.name);
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: path.to_string_lossy().to_string(),
                description: "path".to_string(),
            },
        )))
    }

    fn copy_selected_dir_path(&self) -> EventResult {
        let Some(entry) = self.selected_entry() else {
            return EventResult::Consumed;
        };
        let path = if entry.is_dir {
            self.current_dir.join(&entry.name)
        } else {
            self.current_dir.clone()
        };
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: path.to_string_lossy().to_string(),
                description: "dir path".to_string(),
            },
        )))
    }

    fn copy_selected_name(&self) -> EventResult {
        let Some(entry) = self.selected_entry() else {
            return EventResult::Consumed;
        };
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: entry.name.clone(),
                description: "file name".to_string(),
            },
        )))
    }

    pub fn select_by_name(&mut self, name: &str) {
        for (i, &idx) in self.visible_entries.iter().enumerate() {
            if self.entries[idx].name == name {
                self.selected = i;
                self.update_preview();
                return;
            }
        }
    }

    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    pub fn is_changed_only(&self) -> bool {
        self.mode == ExplorerMode::ChangedOnly
    }

    pub fn selected_name(&self) -> Option<&str> {
        if self.visible_entries.is_empty() {
            return None;
        }
        let idx = self.visible_entries[self.selected];
        Some(&self.entries[idx].name)
    }

    pub fn render(&mut self, surface: &mut Surface, x: usize, width: usize, height: usize) {
        if width == 0 || height == 0 {
            return;
        }

        let default_style = CellStyle::default();
        let dim_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };

        // Header: show current directory path. Prefix "[P] " when preview is on.
        let prefix = if self.preview_mode { "[P] " } else { "" };
        let prefix_w = crate::ui::text::display_width(prefix);
        let header_budget = width.saturating_sub(prefix_w);
        let header_body = self.truncated_path_header(header_budget);
        let header = format!("{}{}", prefix, header_body);
        surface.put_str(x, 0, &header, &dim_style);
        let header_w = crate::ui::text::display_width(&header);
        if header_w < width {
            surface.fill_region(x + header_w, 0, width - header_w, ' ', &dim_style);
        }

        // Compute content area: rows 1..height (reserve row 0 for header)
        // If prompt is active, reserve the last row for prompt
        let content_start_row = 1;
        let bottom_prompt_active = self.find_active
            || self.copy_menu_active
            || self.rename_active
            || self.add_active
            || self.delete_confirm_active;
        let content_height = if bottom_prompt_active {
            height.saturating_sub(2) // header + find prompt
        } else {
            height.saturating_sub(1) // header only
        };

        // Build a render plan. In changed-only mode, each file expands into
        // two display rows (the entry itself + a "+adds -dels" stats row).
        enum RenderRow {
            Entry {
                vis_idx: usize,
            },
            Stats {
                vis_idx: usize,
                additions: usize,
                deletions: usize,
            },
        }
        let mut plan: Vec<RenderRow> = Vec::new();
        let mut entry_to_primary: Vec<usize> = Vec::with_capacity(self.visible_entries.len());
        for (vis_idx, &entry_idx) in self.visible_entries.iter().enumerate() {
            entry_to_primary.push(plan.len());
            plan.push(RenderRow::Entry { vis_idx });
            if self.mode == ExplorerMode::ChangedOnly || self.mode == ExplorerMode::BranchCompare {
                let entry = &self.entries[entry_idx];
                if !entry.is_repo_header && !entry.is_dir {
                    let (adds, dels) = entry.diff_stats.unwrap_or((0, 0));
                    plan.push(RenderRow::Stats {
                        vis_idx,
                        additions: adds,
                        deletions: dels,
                    });
                }
            }
        }

        // Adjust scroll offset (which counts display rows) to keep the selected
        // entry — and its trailing stats row — visible.
        let sel_primary = entry_to_primary.get(self.selected).copied().unwrap_or(0);
        let sel_end = entry_to_primary
            .get(self.selected + 1)
            .copied()
            .map(|next| next.saturating_sub(1))
            .unwrap_or_else(|| plan.len().saturating_sub(1));
        if sel_primary < self.scroll_offset {
            self.scroll_offset = sel_primary;
        }
        if sel_end >= self.scroll_offset + content_height {
            self.scroll_offset = sel_end.saturating_sub(content_height.saturating_sub(1));
        }

        // Draw entries
        for row in 0..content_height {
            let plan_idx = self.scroll_offset + row;
            let screen_row = content_start_row + row;
            if screen_row >= height {
                break;
            }

            let Some(render_row) = plan.get(plan_idx) else {
                surface.fill_region(x, screen_row, width, ' ', &default_style);
                continue;
            };

            match render_row {
                RenderRow::Entry { vis_idx } => {
                    let entry_idx = self.visible_entries[*vis_idx];
                    let entry = &self.entries[entry_idx];
                    let is_selected = *vis_idx == self.selected;

                    let prefix = if is_selected { "> " } else { "  " };
                    let display = if entry.is_repo_header {
                        format!("{}\u{e0a0} {}/", prefix, entry.name)
                    } else if self.mode == ExplorerMode::ChangedOnly
                        || self.mode == ExplorerMode::BranchCompare
                    {
                        let status = entry.git_status.map_or(' ', |s| s.indicator());
                        format!("{}[{}] {}", prefix, status, entry.name)
                    } else {
                        let suffix = if entry.is_dir { "/" } else { "" };
                        format!("{}{}{}", prefix, entry.name, suffix)
                    };

                    let style = if entry.is_repo_header {
                        if is_selected {
                            CellStyle {
                                bold: true,
                                reverse: true,
                                fg: Some(crossterm::style::Color::Cyan),
                                ..CellStyle::default()
                            }
                        } else {
                            CellStyle {
                                bold: true,
                                fg: Some(crossterm::style::Color::Cyan),
                                ..CellStyle::default()
                            }
                        }
                    } else if is_selected {
                        CellStyle {
                            reverse: true,
                            fg: entry.git_status.map(|s| s.color()),
                            ..CellStyle::default()
                        }
                    } else {
                        CellStyle {
                            fg: entry.git_status.map(|s| s.color()),
                            ..CellStyle::default()
                        }
                    };
                    let (truncated, used) = truncate_to_width(&display, width);
                    surface.put_str(x, screen_row, truncated, &style);
                    if used < width {
                        surface.fill_region(x + used, screen_row, width - used, ' ', &style);
                    }
                }
                RenderRow::Stats {
                    vis_idx,
                    additions,
                    deletions,
                } => {
                    let is_selected = *vis_idx == self.selected;
                    let base = if is_selected {
                        CellStyle {
                            reverse: true,
                            ..CellStyle::default()
                        }
                    } else {
                        CellStyle::default()
                    };
                    let add_style = CellStyle {
                        fg: Some(crossterm::style::Color::Green),
                        ..base
                    };
                    let del_style = CellStyle {
                        fg: Some(crossterm::style::Color::Red),
                        ..base
                    };
                    surface.fill_region(x, screen_row, width, ' ', &base);
                    // Indent under the "  [X] " prefix (6 columns) when not selected,
                    // "> [X] " (6 columns) when selected — same width either way.
                    let indent = "      ";
                    let mut col = x;
                    let indent_w = crate::ui::text::display_width(indent);
                    if indent_w <= width {
                        surface.put_str(col, screen_row, indent, &base);
                        col += indent_w;
                    }
                    let adds_str = format!("+{}", additions);
                    let dels_str = format!("-{}", deletions);
                    let adds_w = crate::ui::text::display_width(&adds_str);
                    if col + adds_w <= x + width {
                        surface.put_str(col, screen_row, &adds_str, &add_style);
                        col += adds_w;
                    }
                    if col < x + width {
                        surface.put_str(col, screen_row, " ", &base);
                        col += 1;
                    }
                    let dels_w = crate::ui::text::display_width(&dels_str);
                    if col + dels_w <= x + width {
                        surface.put_str(col, screen_row, &dels_str, &del_style);
                    }
                }
            }
        }

        // Bottom prompt
        if bottom_prompt_active {
            let find_row = height.saturating_sub(1);
            let prompt = self.bottom_prompt();
            let find_style = CellStyle {
                reverse: true,
                ..CellStyle::default()
            };
            let (truncated, used) = truncate_to_width(&prompt, width);
            surface.put_str(x, find_row, truncated, &find_style);
            if used < width {
                surface.fill_region(x + used, find_row, width - used, ' ', &find_style);
            }
        }
    }

    fn truncated_path_header(&self, max_width: usize) -> String {
        let path = &self.current_dir;
        let components: Vec<_> = path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();

        if components.is_empty() {
            return "/".to_string();
        }

        // Try building from the last component backwards
        // Start with just the last dir name + /
        let last = &components[components.len() - 1];
        let mut result = format!("{}/", last);

        if crate::ui::text::display_width(&result) <= max_width {
            // Try adding more parent components
            for i in (0..components.len() - 1).rev() {
                let candidate = format!("{}/{}", components[i], result);
                if crate::ui::text::display_width(&candidate) <= max_width {
                    result = candidate;
                } else {
                    break;
                }
            }
        }

        // If even the last component doesn't fit, truncate it
        if crate::ui::text::display_width(&result) > max_width {
            let (truncated, _) = truncate_to_width(&result, max_width);
            return truncated.to_string();
        }

        result
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
            let label = self.selected_name().unwrap_or("item");
            format!("delete {}? [y/N]", label)
        } else {
            String::new()
        }
    }

    /// Returns cursor position (x, y) for the find prompt, if find is active
    pub fn find_cursor(&self, x: usize, height: usize) -> Option<(u16, u16)> {
        let prompt = if self.find_active {
            format!("/{}", self.find_input)
        } else if self.rename_active {
            format!("rename: {}", self.rename_input)
        } else if self.add_active {
            format!("add: {} (end with / for dir)", self.add_input)
        } else {
            String::new()
        };

        if prompt.is_empty() {
            return None;
        }
        let find_row = height.saturating_sub(1);
        let cursor_x = x + crate::ui::text::display_width(&prompt);
        Some((cursor_x as u16, find_row as u16))
    }
}

fn read_file_preview(path: &Path) -> (Vec<String>, HashMap<usize, Vec<HighlightSpan>>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (vec!["<binary or unreadable>".to_string()], HashMap::new()),
    };
    let lines: Vec<String> = content
        .lines()
        .take(PREVIEW_MAX_LINES)
        .map(|l| l.to_string())
        .collect();
    let lang_registry = LanguageRegistry::new();
    let path_str = path.to_string_lossy();
    let spans = if let Some(lang_def) = lang_registry.detect_by_extension(&path_str) {
        let preview_text: String = lines.join("\n");
        highlight_text(&preview_text, lang_def)
    } else {
        HashMap::new()
    };
    (lines, spans)
}

fn build_dir_listing(path: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return vec![format!("<cannot read {}>", path.display())];
    };

    struct Row {
        name: String,
        is_dir: bool,
        size: u64,
        mtime: Option<SystemTime>,
    }

    let mut dirs: Vec<Row> = Vec::new();
    let mut files: Vec<Row> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.metadata().ok();
        let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = metadata.as_ref().and_then(|m| m.modified().ok());
        let row = Row {
            name,
            is_dir,
            size,
            mtime,
        };
        if is_dir {
            dirs.push(row);
        } else {
            files.push(row);
        }
    }
    sort_by_name_case_insensitive(&mut dirs, |r| &r.name);
    sort_by_name_case_insensitive(&mut files, |r| &r.name);

    let mut lines: Vec<String> = Vec::new();
    let total = dirs.len() + files.len();
    lines.push(format!("total: {} entries", total));
    lines.push(String::new());
    let mut emit = |row: &Row| {
        let display_name = if row.is_dir {
            format!("{}/", row.name)
        } else {
            row.name.clone()
        };
        let size = if row.is_dir {
            "-".to_string()
        } else {
            format_size(row.size)
        };
        let mtime = row
            .mtime
            .map(format_mtime)
            .unwrap_or_else(|| "-".to_string());
        // size col 8, mtime col 17, gap, then name
        lines.push(format!("{:>8}  {:<17}  {}", size, mtime, display_name));
    };
    for r in &dirs {
        emit(r);
    }
    for r in &files {
        emit(r);
    }
    lines
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if bytes < KB {
        format!("{}B", bytes)
    } else if bytes < MB {
        format!("{:.1}K", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1}M", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else {
        format!("{:.1}T", bytes as f64 / TB as f64)
    }
}

/// Format a SystemTime as "YYYY-MM-DD HH:MM" in UTC. No tz crate; uses
/// Howard Hinnant's civil-from-days algorithm for the date split.
fn format_mtime(t: SystemTime) -> String {
    let secs = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;

    // Howard Hinnant: civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02} {:02}:{:02}", year, m, d, hour, minute)
}

/// Map the single-char status from `git diff --name-status` to the
/// `GitFileStatus` enum used by the sidebar's display layer. Anything we
/// don't recognise falls back to `Modified` (the explorer's most common
/// rendering style).
fn branch_diff_status_char_to_file_status(status_char: char) -> GitFileStatus {
    match status_char.to_ascii_uppercase() {
        'A' => GitFileStatus::Added,
        'D' => GitFileStatus::Deleted,
        'U' => GitFileStatus::Conflict,
        _ => GitFileStatus::Modified,
    }
}

fn collect_diff_stats(project_root: &Path) -> HashMap<String, (usize, usize)> {
    let mut map = HashMap::new();
    let (changed, staged) = match crate::command::git::git_status_files_in(project_root) {
        Ok(v) => v,
        Err(_) => return map,
    };
    for entry in staged.into_iter().chain(changed.into_iter()) {
        if entry.additions == 0 && entry.deletions == 0 {
            map.entry(entry.path).or_insert((0, 0));
            continue;
        }
        let slot = map.entry(entry.path).or_insert((0, 0));
        slot.0 += entry.additions;
        slot.1 += entry.deletions;
    }
    map
}

fn count_file_lines(path: &Path) -> usize {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    if bytes.is_empty() {
        return 0;
    }
    let nl = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.last() == Some(&b'\n') {
        nl
    } else {
        nl + 1
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

    fn setup(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gargo_test_explorer_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join("aaa_dir")).unwrap();
        fs::write(dir.join("bbb.txt"), "bbb").unwrap();
        fs::write(dir.join("ccc.rs"), "ccc").unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn changed_only_mode_shows_only_changed_entries() {
        let dir = setup("changed_only");
        let mut git_status_map = HashMap::new();
        git_status_map.insert("bbb.txt".to_string(), GitFileStatus::Modified);
        git_status_map.insert("aaa_dir/nested.txt".to_string(), GitFileStatus::Added);

        let explorer = Explorer::new_changed_only(dir.clone(), &dir, &git_status_map);

        assert!(explorer.is_changed_only());
        assert_eq!(explorer.visible_entries.len(), 2);
        let names: Vec<String> = explorer
            .visible_entries
            .iter()
            .map(|&idx| explorer.entries[idx].name.clone())
            .collect();
        assert_eq!(
            names,
            vec!["aaa_dir/nested.txt".to_string(), "bbb.txt".to_string()]
        );
        assert!(explorer.entries.iter().all(|entry| !entry.is_dir));

        cleanup(&dir);
    }

    #[test]
    fn changed_only_mode_enter_opens_nested_path_as_file() {
        let dir = setup("changed_open_nested");
        fs::write(dir.join("aaa_dir").join("nested.txt"), "nested").unwrap();
        let mut git_status_map = HashMap::new();
        git_status_map.insert("aaa_dir/nested.txt".to_string(), GitFileStatus::Modified);
        let mut explorer = Explorer::new_changed_only(dir.clone(), &dir, &git_status_map);

        let result = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);
        match result {
            EventResult::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenFileFromExplorer(path),
            ))) => {
                assert_eq!(PathBuf::from(path), dir.join("aaa_dir").join("nested.txt"));
            }
            _ => panic!("Expected OpenFileFromExplorer action"),
        }

        cleanup(&dir);
    }

    #[test]
    fn changed_only_mode_renders_status_badge() {
        let dir = setup("changed_badge");
        let mut git_status_map = HashMap::new();
        git_status_map.insert("bbb.txt".to_string(), GitFileStatus::Modified);
        let mut explorer = Explorer::new_changed_only(dir.clone(), &dir, &git_status_map);
        let mut surface = Surface::new(40, 6);

        explorer.render(&mut surface, 0, 40, 6);

        let row: String = (0..40)
            .map(|x| {
                let symbol = &surface.get(x, 1).symbol;
                if symbol.is_empty() {
                    ' '
                } else {
                    symbol.chars().next().unwrap_or(' ')
                }
            })
            .collect();
        assert!(
            row.contains("[M] bbb.txt"),
            "row did not contain status badge: {}",
            row
        );

        cleanup(&dir);
    }

    #[test]
    fn find_mode_jumps_selection_without_filtering() {
        let dir = setup("find_jump");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        explorer.handle_key(key(KeyCode::Char('/')), &KeyState::Normal);
        explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);

        assert_eq!(explorer.visible_entries.len(), 3);
        assert_eq!(explorer.selected_name(), Some("ccc.rs"));

        cleanup(&dir);
    }

    #[test]
    fn find_mode_ctrl_and_arrow_navigation_work() {
        let dir = setup("find_nav");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        explorer.handle_key(key(KeyCode::Char('/')), &KeyState::Normal);
        explorer.handle_key(ctrl_key('n'), &KeyState::Normal);
        assert_eq!(explorer.selected_name(), Some("bbb.txt"));
        explorer.handle_key(ctrl_key('p'), &KeyState::Normal);
        assert_eq!(explorer.selected_name(), Some("aaa_dir"));
        explorer.handle_key(key(KeyCode::Down), &KeyState::Normal);
        assert_eq!(explorer.selected_name(), Some("bbb.txt"));
        explorer.handle_key(key(KeyCode::Up), &KeyState::Normal);
        assert_eq!(explorer.selected_name(), Some("aaa_dir"));

        cleanup(&dir);
    }

    #[test]
    fn find_mode_ctrl_w_ctrl_u_and_ctrl_k_edit_query() {
        let dir = setup("find_ctrl_edit");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        explorer.handle_key(key(KeyCode::Char('/')), &KeyState::Normal);
        for c in "src ui ccc".chars() {
            explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        explorer.handle_key(ctrl_key('w'), &KeyState::Normal);
        assert_eq!(explorer.find_input, "src ui ");
        explorer.handle_key(ctrl_key('u'), &KeyState::Normal);
        assert!(explorer.find_input.is_empty());
        for c in "tmp new".chars() {
            explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        explorer.handle_key(ctrl_key('k'), &KeyState::Normal);
        assert!(explorer.find_input.is_empty());

        cleanup(&dir);
    }

    #[test]
    fn copy_menu_cc_copies_selected_full_path() {
        let dir = setup("copy_path");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());
        explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal); // bbb.txt

        let _ = explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        let result = explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);

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
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());
        explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal); // bbb.txt

        let _ = explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        let result = explorer.handle_key(key(KeyCode::Char('d')), &KeyState::Normal);

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
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());
        explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal); // bbb.txt

        let _ = explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        let result = explorer.handle_key(key(KeyCode::Char('f')), &KeyState::Normal);

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
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        let _ = explorer.handle_key(key(KeyCode::Char('c')), &KeyState::Normal);
        let first = explorer.handle_key(key(KeyCode::Char('x')), &KeyState::Normal);
        assert!(matches!(first, EventResult::Consumed));

        let second = explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal);
        assert!(matches!(second, EventResult::Consumed));
        assert_eq!(explorer.selected_name(), Some("bbb.txt"));

        cleanup(&dir);
    }

    #[test]
    fn rename_selected_file_with_r() {
        let dir = setup("rename_file");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());
        explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal); // bbb.txt

        let _ = explorer.handle_key(key(KeyCode::Char('r')), &KeyState::Normal);
        let _ = explorer.handle_key(ctrl_key('u'), &KeyState::Normal);
        for c in "renamed.txt".chars() {
            let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        let result = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);

        assert!(dir.join("renamed.txt").exists());
        assert!(!dir.join("bbb.txt").exists());
        assert_eq!(explorer.selected_name(), Some("renamed.txt"));
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
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        let _ = explorer.handle_key(key(KeyCode::Char('a')), &KeyState::Normal);
        for c in "new.txt".chars() {
            let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        let _ = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);
        assert!(dir.join("new.txt").exists());
        assert_eq!(explorer.selected_name(), Some("new.txt"));

        let _ = explorer.handle_key(key(KeyCode::Char('a')), &KeyState::Normal);
        for c in "new_dir/".chars() {
            let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        let _ = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);
        assert!(dir.join("new_dir").is_dir());
        assert_eq!(explorer.selected_name(), Some("new_dir"));

        cleanup(&dir);
    }

    #[test]
    fn add_nested_file_creates_intermediate_dirs() {
        let dir = setup("add_nested_file");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        let _ = explorer.handle_key(key(KeyCode::Char('a')), &KeyState::Normal);
        for c in "dirA/dirB/README.md".chars() {
            let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        let _ = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);

        assert!(dir.join("dirA/dirB/README.md").is_file());
        assert!(dir.join("dirA/dirB").is_dir());
        assert_eq!(explorer.current_dir(), dir.join("dirA/dirB").as_path());
        assert_eq!(explorer.selected_name(), Some("README.md"));

        cleanup(&dir);
    }

    #[test]
    fn add_nested_dir_creates_intermediate_dirs() {
        let dir = setup("add_nested_dir");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        let _ = explorer.handle_key(key(KeyCode::Char('a')), &KeyState::Normal);
        for c in "x/y/z/".chars() {
            let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
        }
        let _ = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);

        assert!(dir.join("x/y/z").is_dir());
        assert_eq!(explorer.current_dir(), dir.join("x/y").as_path());
        assert_eq!(explorer.selected_name(), Some("z"));

        cleanup(&dir);
    }

    #[test]
    fn add_rejects_absolute_and_parent_components() {
        let dir = setup("add_invalid_path");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());

        for raw in ["/etc/passwd", "../foo", "foo/../bar"] {
            let _ = explorer.handle_key(key(KeyCode::Char('a')), &KeyState::Normal);
            for c in raw.chars() {
                let _ = explorer.handle_key(key(KeyCode::Char(c)), &KeyState::Normal);
            }
            let _ = explorer.handle_key(key(KeyCode::Enter), &KeyState::Normal);
            assert!(!dir.join(raw.trim_start_matches('/')).exists());
        }

        cleanup(&dir);
    }

    #[test]
    fn delete_confirmation_requires_y() {
        let dir = setup("delete_confirm");
        let mut explorer = Explorer::new(dir.clone(), &dir, &HashMap::new());
        explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal); // bbb.txt

        let _ = explorer.handle_key(key(KeyCode::Char('d')), &KeyState::Normal);
        let _ = explorer.handle_key(key(KeyCode::Char('n')), &KeyState::Normal);
        assert!(dir.join("bbb.txt").exists());

        let _ = explorer.handle_key(key(KeyCode::Char('d')), &KeyState::Normal);
        let _ = explorer.handle_key(key(KeyCode::Char('y')), &KeyState::Normal);
        assert!(!dir.join("bbb.txt").exists());

        cleanup(&dir);
    }

    #[test]
    fn branch_compare_mode_lists_supplied_files_with_stats() {
        let dir = setup("branch_compare_list");
        let files = vec![
            GitFileEntry {
                path: "src/a.rs".to_string(),
                status_char: 'M',
                staged: false,
                additions: 3,
                deletions: 1,
            },
            GitFileEntry {
                path: "README.md".to_string(),
                status_char: 'A',
                staged: false,
                additions: 5,
                deletions: 0,
            },
        ];
        let explorer = Explorer::new_branch_compare(dir.clone(), "main".to_string(), files);
        assert!(explorer.is_branch_compare());
        assert_eq!(explorer.branch_compare_base(), Some("main"));
        // Files appear sorted case-insensitively.
        let names: Vec<&str> = explorer.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["README.md", "src/a.rs"]);
        // diff_stats are populated.
        assert_eq!(
            explorer
                .entries
                .iter()
                .find(|e| e.name == "src/a.rs")
                .and_then(|e| e.diff_stats),
            Some((3, 1)),
        );
        cleanup(&dir);
    }

    #[test]
    fn branch_compare_refresh_keeps_selection_by_name() {
        let dir = setup("branch_compare_refresh");
        let initial = vec![
            GitFileEntry {
                path: "a.rs".to_string(),
                status_char: 'M',
                staged: false,
                additions: 0,
                deletions: 0,
            },
            GitFileEntry {
                path: "b.rs".to_string(),
                status_char: 'M',
                staged: false,
                additions: 0,
                deletions: 0,
            },
        ];
        let mut explorer = Explorer::new_branch_compare(dir.clone(), "main".to_string(), initial);
        // Move selection to b.rs.
        let _ = explorer.handle_key(key(KeyCode::Char('j')), &KeyState::Normal);
        assert_eq!(explorer.selected_name(), Some("b.rs"));

        // Refresh: a.rs removed, c.rs added, b.rs still present.
        let refreshed = vec![
            GitFileEntry {
                path: "b.rs".to_string(),
                status_char: 'M',
                staged: false,
                additions: 0,
                deletions: 0,
            },
            GitFileEntry {
                path: "c.rs".to_string(),
                status_char: 'A',
                staged: false,
                additions: 0,
                deletions: 0,
            },
        ];
        explorer.apply_branch_diff_files(refreshed);
        assert_eq!(explorer.selected_name(), Some("b.rs"));

        cleanup(&dir);
    }
}

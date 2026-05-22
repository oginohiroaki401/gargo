use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use crossterm::style::Color;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;

use crate::command::git::{self, GitFileEntry};
use crate::command::git_view_diff_runtime::{DiffCacheKey, GitViewDiffCommand, GitViewDiffEvent};
use crate::input::action::{Action, AppAction, BufferAction, UiAction, WorkspaceAction};
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::EventResult;
use crate::ui::framework::surface::Surface;
use crate::ui::shared::filtering::fuzzy_match;
use crate::ui::text::{display_width, slice_display_window, truncate_to_width};
use crate::ui::text_input::delete_prev_word_input;

#[derive(Debug, Clone, Default)]
pub struct GitViewIndexSnapshot {
    pub branch: String,
    pub changed: Vec<GitFileEntry>,
    pub staged: Vec<GitFileEntry>,
}

pub struct RepoSection {
    pub project_root: PathBuf,
    pub display_name: String,
    pub branch: String,
    pub changed: Vec<GitFileEntry>,
    pub staged: Vec<GitFileEntry>,
}

pub struct GitView {
    /// Primary project root (for single-repo compat and the public accessor).
    project_root: PathBuf,
    /// Per-repo data. Single-repo: len=1, multi-repo: len>1.
    repos: Vec<RepoSection>,
    selected: usize,
    scroll_offset: usize,
    find_active: bool,
    find_input: String,
    diff_state: DiffDisplayState,
    diff_scroll: usize,
    diff_horizontal_scroll: usize,
    message: Option<String>,
    diff_runtime_tx: Option<mpsc::Sender<GitViewDiffCommand>>,
    allow_sync_diff_fallback: bool,
    diff_cache: HashMap<DiffCacheKey, CachedDiffEntry>,
    diff_cache_order: VecDeque<DiffCacheKey>,
    diff_cache_max_entries: usize,
    diff_prefetch_radius: usize,
    pending_requests: HashMap<DiffCacheKey, u64>,
    next_request_id: u64,
    selected_diff_key: Option<DiffCacheKey>,
    selected_request_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Changed,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionTarget {
    RepoHeader(usize),
    ChangedHeader(usize),
    ChangedFile(usize, usize),
    StagedHeader(usize),
    StagedFile(usize, usize),
}

impl SelectionTarget {
    fn repo_idx(&self) -> usize {
        match self {
            Self::RepoHeader(ri)
            | Self::ChangedHeader(ri)
            | Self::ChangedFile(ri, _)
            | Self::StagedHeader(ri)
            | Self::StagedFile(ri, _) => *ri,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectionAnchor {
    Header(usize, SectionKind),
    File {
        repo_idx: usize,
        path: String,
        staged: bool,
    },
}

#[derive(Debug, Clone)]
enum CachedDiffEntry {
    Ready(Vec<String>),
    Error(String),
}

impl CachedDiffEntry {
    fn into_display_state(self) -> DiffDisplayState {
        match self {
            CachedDiffEntry::Ready(lines) => DiffDisplayState::Ready(lines),
            CachedDiffEntry::Error(message) => DiffDisplayState::Error(message),
        }
    }
}

#[derive(Debug, Clone)]
enum DiffDisplayState {
    Idle,
    Loading,
    Ready(Vec<String>),
    Error(String),
}

const DEFAULT_DIFF_CACHE_MAX_ENTRIES: usize = 64;
const DEFAULT_DIFF_PREFETCH_RADIUS: usize = 1;
const DIFF_SPLIT_THRESHOLD: usize = 60;
const MOUSE_SCROLL_LINES: usize = 3;
const HORIZONTAL_SCROLL_COLS: usize = 8;
const DIFF_LOADING_LABEL: &str = "Loading diff...";
const DIFF_IDLE_LABEL: &str = "No diff selected";
const DIFF_RUNTIME_UNAVAILABLE_LABEL: &str = "Diff runtime unavailable";
const GIT_INDEX_LOADING_LABEL: &str = "Loading git index...";

impl Default for GitView {
    fn default() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::new(project_root)
    }
}

impl GitView {
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn is_multi_repo(&self) -> bool {
        self.repos.len() > 1
    }

    /// Returns the repo root for the currently selected entry.
    pub fn active_repo_root(&self) -> &Path {
        if let Some(target) = self.selected_target() {
            let repo_idx = target.repo_idx();
            if let Some(section) = self.repos.get(repo_idx) {
                return &section.project_root;
            }
        }
        // Fallback: first repo or project_root
        self.repos
            .first()
            .map(|s| s.project_root.as_path())
            .unwrap_or(&self.project_root)
    }

    fn repo_root_for_target(&self, target: &SelectionTarget) -> &Path {
        self.repos
            .get(target.repo_idx())
            .map(|s| s.project_root.as_path())
            .unwrap_or(&self.project_root)
    }

    pub fn new(project_root: PathBuf) -> Self {
        Self::new_with_runtime_prefetched(
            project_root,
            None,
            DEFAULT_DIFF_CACHE_MAX_ENTRIES,
            DEFAULT_DIFF_PREFETCH_RADIUS,
            None,
            true,
        )
    }

    pub fn new_with_runtime(
        project_root: PathBuf,
        diff_runtime_tx: Option<mpsc::Sender<GitViewDiffCommand>>,
        diff_cache_max_entries: usize,
        diff_prefetch_radius: usize,
    ) -> Self {
        Self::new_with_runtime_prefetched(
            project_root,
            diff_runtime_tx,
            diff_cache_max_entries,
            diff_prefetch_radius,
            None,
            true,
        )
    }

    pub fn new_with_runtime_prefetched(
        project_root: PathBuf,
        diff_runtime_tx: Option<mpsc::Sender<GitViewDiffCommand>>,
        diff_cache_max_entries: usize,
        diff_prefetch_radius: usize,
        preloaded_index: Option<GitViewIndexSnapshot>,
        allow_sync_index_fallback: bool,
    ) -> Self {
        let (branch, changed, staged, message) = if let Some(snapshot) = preloaded_index {
            (snapshot.branch, snapshot.changed, snapshot.staged, None)
        } else if allow_sync_index_fallback {
            let branch = git::git_branch_in(&project_root).unwrap_or_else(|_| "???".to_string());
            let (changed, staged) = git::git_status_files_in(&project_root).unwrap_or_default();
            (branch, changed, staged, None)
        } else {
            (
                "???".to_string(),
                Vec::new(),
                Vec::new(),
                Some(GIT_INDEX_LOADING_LABEL.to_string()),
            )
        };
        let display_name = project_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let repos = vec![RepoSection {
            project_root: project_root.clone(),
            display_name,
            branch,
            changed,
            staged,
        }];
        let allow_sync_diff_fallback = diff_runtime_tx.is_none();
        let mut view = Self {
            project_root,
            repos,
            selected: 0,
            scroll_offset: 0,
            find_active: false,
            find_input: String::new(),
            diff_state: DiffDisplayState::Idle,
            diff_scroll: 0,
            diff_horizontal_scroll: 0,
            message,
            diff_runtime_tx,
            allow_sync_diff_fallback,
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_max_entries,
            diff_prefetch_radius,
            pending_requests: HashMap::new(),
            next_request_id: 1,
            selected_diff_key: None,
            selected_request_id: None,
        };
        view.selected = view.first_file_row().unwrap_or(0);
        view.request_selected_diff();
        view
    }

    pub fn new_multi_repo(
        project_root: PathBuf,
        repo_sections: Vec<RepoSection>,
        diff_runtime_tx: Option<mpsc::Sender<GitViewDiffCommand>>,
        diff_cache_max_entries: usize,
        diff_prefetch_radius: usize,
    ) -> Self {
        let allow_sync_diff_fallback = diff_runtime_tx.is_none();
        let mut view = Self {
            project_root,
            repos: repo_sections,
            selected: 0,
            scroll_offset: 0,
            find_active: false,
            find_input: String::new(),
            diff_state: DiffDisplayState::Idle,
            diff_scroll: 0,
            diff_horizontal_scroll: 0,
            message: None,
            diff_runtime_tx,
            allow_sync_diff_fallback,
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_max_entries,
            diff_prefetch_radius,
            pending_requests: HashMap::new(),
            next_request_id: 1,
            selected_diff_key: None,
            selected_request_id: None,
        };
        view.selected = view.first_file_row().unwrap_or(0);
        view.request_selected_diff();
        view
    }

    pub fn apply_index_snapshot(&mut self, snapshot: GitViewIndexSnapshot) {
        if let Some(section) = self.repos.first_mut() {
            section.branch = snapshot.branch;
        }
        self.apply_file_entries(0, snapshot.changed, snapshot.staged);
        if self.message.as_deref() == Some(GIT_INDEX_LOADING_LABEL) {
            self.message = None;
        }
    }

    pub fn apply_multi_index_snapshots(&mut self, snapshots: Vec<(PathBuf, GitViewIndexSnapshot)>) {
        for (repo_root, snapshot) in snapshots {
            if let Some(section) = self.repos.iter_mut().find(|s| s.project_root == repo_root) {
                section.branch = snapshot.branch;
                section.changed = snapshot.changed;
                section.staged = snapshot.staged;
            }
        }
        if self.message.as_deref() == Some(GIT_INDEX_LOADING_LABEL) {
            self.message = None;
        }
        // Recalculate selection
        let total_rows = self.selectable_row_count();
        if total_rows == 0 {
            self.selected = 0;
        } else if self.selected >= total_rows {
            self.selected = total_rows.saturating_sub(1);
        }
        self.request_selected_diff();
    }

    #[cfg(test)]
    pub fn branch_name(&self) -> &str {
        self.repos
            .first()
            .map(|s| s.branch.as_str())
            .unwrap_or("???")
    }

    #[cfg(test)]
    pub fn changed_entries(&self) -> &[GitFileEntry] {
        self.repos
            .first()
            .map(|s| s.changed.as_slice())
            .unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn staged_entries(&self) -> &[GitFileEntry] {
        self.repos
            .first()
            .map(|s| s.staged.as_slice())
            .unwrap_or(&[])
    }

    #[cfg(test)]
    pub fn message_text(&self) -> Option<&str> {
        self.message.as_deref()
    }

    fn selectable_row_count(&self) -> usize {
        let multi = self.is_multi_repo();
        let mut count: usize = 0;
        for section in &self.repos {
            let has_entries = !section.changed.is_empty() || !section.staged.is_empty();
            if !has_entries {
                continue;
            }
            if multi {
                count = count.saturating_add(1); // repo header
            }
            if !section.changed.is_empty() {
                count = count.saturating_add(1 + section.changed.len());
            }
            if !section.staged.is_empty() {
                count = count.saturating_add(1 + section.staged.len());
            }
        }
        count
    }

    fn first_file_row(&self) -> Option<usize> {
        let multi = self.is_multi_repo();
        for section in &self.repos {
            if !section.changed.is_empty() || !section.staged.is_empty() {
                // First file row: after repo header (if multi) + section header
                return Some(if multi { 2 } else { 1 });
            }
        }
        None
    }

    fn selection_target_at(&self, row: usize) -> Option<SelectionTarget> {
        let multi = self.is_multi_repo();
        let mut offset = row;

        for (repo_idx, section) in self.repos.iter().enumerate() {
            let has_entries = !section.changed.is_empty() || !section.staged.is_empty();
            if !has_entries {
                continue;
            }

            if multi {
                if offset == 0 {
                    return Some(SelectionTarget::RepoHeader(repo_idx));
                }
                offset = offset.saturating_sub(1);
            }

            if !section.changed.is_empty() {
                if offset == 0 {
                    return Some(SelectionTarget::ChangedHeader(repo_idx));
                }
                offset = offset.saturating_sub(1);
                if offset < section.changed.len() {
                    return Some(SelectionTarget::ChangedFile(repo_idx, offset));
                }
                offset = offset.saturating_sub(section.changed.len());
            }

            if !section.staged.is_empty() {
                if offset == 0 {
                    return Some(SelectionTarget::StagedHeader(repo_idx));
                }
                offset = offset.saturating_sub(1);
                if offset < section.staged.len() {
                    return Some(SelectionTarget::StagedFile(repo_idx, offset));
                }
                offset = offset.saturating_sub(section.staged.len());
            }
        }

        None
    }

    fn selected_target(&self) -> Option<SelectionTarget> {
        let total_rows = self.selectable_row_count();
        if total_rows == 0 || self.selected >= total_rows {
            return None;
        }
        self.selection_target_at(self.selected)
    }

    fn row_for_anchor(&self, anchor: &SelectionAnchor) -> Option<usize> {
        let multi = self.is_multi_repo();
        let mut base = 0usize;

        for (repo_idx, section) in self.repos.iter().enumerate() {
            let has_entries = !section.changed.is_empty() || !section.staged.is_empty();
            if !has_entries {
                continue;
            }

            if multi {
                base += 1; // repo header
            }

            match anchor {
                SelectionAnchor::Header(ri, SectionKind::Changed) if *ri == repo_idx => {
                    return (!section.changed.is_empty()).then_some(base);
                }
                SelectionAnchor::Header(ri, SectionKind::Staged) if *ri == repo_idx => {
                    if section.staged.is_empty() {
                        return None;
                    }
                    let staged_offset = if section.changed.is_empty() {
                        0
                    } else {
                        1 + section.changed.len()
                    };
                    return Some(base + staged_offset);
                }
                SelectionAnchor::File {
                    repo_idx: ri,
                    path,
                    staged,
                } if *ri == repo_idx => {
                    if *staged {
                        let file_idx = section.staged.iter().position(|e| e.path == *path)?;
                        let staged_offset = if section.changed.is_empty() {
                            0
                        } else {
                            1 + section.changed.len()
                        };
                        return Some(base + staged_offset + 1 + file_idx);
                    } else {
                        let file_idx = section.changed.iter().position(|e| e.path == *path)?;
                        return Some(base + 1 + file_idx);
                    }
                }
                _ => {}
            }

            if !section.changed.is_empty() {
                base += 1 + section.changed.len();
            }
            if !section.staged.is_empty() {
                base += 1 + section.staged.len();
            }
        }
        None
    }

    fn selected_anchor(&self) -> Option<SelectionAnchor> {
        match self.selected_target()? {
            SelectionTarget::RepoHeader(_) => None,
            SelectionTarget::ChangedHeader(ri) => {
                Some(SelectionAnchor::Header(ri, SectionKind::Changed))
            }
            SelectionTarget::StagedHeader(ri) => {
                Some(SelectionAnchor::Header(ri, SectionKind::Staged))
            }
            SelectionTarget::ChangedFile(ri, idx) => self
                .repos
                .get(ri)
                .and_then(|s| s.changed.get(idx))
                .map(|entry| SelectionAnchor::File {
                    repo_idx: ri,
                    path: entry.path.clone(),
                    staged: false,
                }),
            SelectionTarget::StagedFile(ri, idx) => self
                .repos
                .get(ri)
                .and_then(|s| s.staged.get(idx))
                .map(|entry| SelectionAnchor::File {
                    repo_idx: ri,
                    path: entry.path.clone(),
                    staged: true,
                }),
        }
    }

    fn entry_for_target(&self, target: SelectionTarget) -> Option<&GitFileEntry> {
        match target {
            SelectionTarget::ChangedFile(ri, idx) => {
                self.repos.get(ri).and_then(|s| s.changed.get(idx))
            }
            SelectionTarget::StagedFile(ri, idx) => {
                self.repos.get(ri).and_then(|s| s.staged.get(idx))
            }
            SelectionTarget::RepoHeader(_)
            | SelectionTarget::ChangedHeader(_)
            | SelectionTarget::StagedHeader(_) => None,
        }
    }

    fn selected_entry(&self) -> Option<&GitFileEntry> {
        self.selected_target()
            .and_then(|target| self.entry_for_target(target))
    }

    fn selected_entry_key(&self) -> Option<DiffCacheKey> {
        self.selected_entry().map(|entry| DiffCacheKey {
            path: entry.path.clone(),
            staged: entry.staged,
        })
    }

    fn path_at_row(&self, row: usize) -> Option<&str> {
        self.selection_target_at(row)
            .and_then(|target| self.entry_for_target(target))
            .map(|entry| entry.path.as_str())
    }

    fn has_entry_for_key(&self, key: &DiffCacheKey) -> bool {
        self.repos.iter().any(|section| {
            section
                .changed
                .iter()
                .any(|entry| entry.path == key.path && !key.staged)
                || section
                    .staged
                    .iter()
                    .any(|entry| entry.path == key.path && key.staged)
        })
    }

    fn current_diff_lines(&self) -> Option<&[String]> {
        match &self.diff_state {
            DiffDisplayState::Ready(lines) => Some(lines.as_slice()),
            _ => None,
        }
    }

    fn first_changed_line_in_diff(&self) -> Option<usize> {
        first_changed_line_in_lines(self.current_diff_lines()?)
    }

    /// First changed line (0-based, new-file coordinates) for the current
    /// selection. Falls back to a synchronous `git diff` when the async diff
    /// has not arrived yet, so opening a file always lands on its first hunk.
    fn first_changed_line_for_selected(&self) -> Option<usize> {
        if let Some(line) = self.first_changed_line_in_diff() {
            return Some(line);
        }

        let key = self.selected_entry_key()?;

        if let Some(CachedDiffEntry::Ready(lines)) = self.diff_cache.get(&key) {
            return first_changed_line_in_lines(lines);
        }

        let repo_root = self.repo_root_for_entry_path(&key.path, key.staged);
        let diff = git::git_diff_in(&repo_root, &key.path, key.staged).ok()?;
        let lines: Vec<String> = diff.lines().map(|line| line.to_string()).collect();
        first_changed_line_in_lines(&lines)
    }

    fn jump_to_best_match(&mut self) -> bool {
        if self.find_input.is_empty() {
            return false;
        }
        let mut best: Option<(i32, usize)> = None;
        for row in 0..self.selectable_row_count() {
            let Some(path) = self.path_at_row(row) else {
                continue;
            };
            if let Some((score, _)) = fuzzy_match(path, &self.find_input)
                && best.is_none_or(|(best_score, _)| score > best_score)
            {
                best = Some((score, row));
            }
        }
        if let Some((_, row)) = best
            && self.selected != row
        {
            self.selected = row;
            self.request_selected_diff();
            return true;
        }
        false
    }

    pub fn refresh(&mut self) {
        for section in &mut self.repos {
            let (changed, staged) =
                git::git_status_files_in(&section.project_root).unwrap_or_default();
            section.changed = changed;
            section.staged = staged;
        }
        self.after_file_entries_changed();
    }

    fn apply_file_entries(
        &mut self,
        repo_idx: usize,
        changed: Vec<GitFileEntry>,
        staged: Vec<GitFileEntry>,
    ) {
        if let Some(section) = self.repos.get_mut(repo_idx) {
            section.changed = changed;
            section.staged = staged;
        }
        self.after_file_entries_changed();
    }

    fn after_file_entries_changed(&mut self) {
        let prev_selected = self.selected_anchor();
        self.retain_cache_for_current_entries();

        let total_rows = self.selectable_row_count();
        if total_rows == 0 {
            self.selected = 0;
        } else if let Some(anchor) = prev_selected {
            if let Some(row) = self.row_for_anchor(&anchor) {
                self.selected = row;
            } else if self.selected >= total_rows {
                self.selected = total_rows - 1;
            }
        } else {
            self.selected = self.first_file_row().unwrap_or(0);
        }

        let moved = self.jump_to_best_match();
        if !moved {
            self.request_selected_diff();
        }
    }

    fn move_down(&mut self) {
        let total_rows = self.selectable_row_count();
        if total_rows == 0 {
            return;
        }
        if self.selected + 1 < total_rows {
            self.selected += 1;
            self.request_selected_diff();
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.request_selected_diff();
        }
    }

    fn reset_diff_scroll_state(&mut self) {
        self.diff_scroll = 0;
        self.diff_horizontal_scroll = 0;
    }

    fn request_selected_diff(&mut self) {
        let Some(key) = self.selected_entry_key() else {
            self.selected_diff_key = None;
            self.selected_request_id = None;
            self.diff_state = DiffDisplayState::Idle;
            self.reset_diff_scroll_state();
            return;
        };

        let selection_changed = self.selected_diff_key.as_ref() != Some(&key);
        self.selected_diff_key = Some(key.clone());

        if let Some(cached) = self.cache_get(&key) {
            self.selected_request_id = None;
            self.diff_state = cached.into_display_state();
            if selection_changed {
                self.reset_diff_scroll_state();
            }
            self.prefetch_neighbors();
            return;
        }

        self.diff_state = DiffDisplayState::Loading;
        if selection_changed {
            self.reset_diff_scroll_state();
        }

        if let Some(request_id) = self.enqueue_diff_request(key.clone(), true) {
            self.selected_request_id = Some(request_id);
        } else if self.allow_sync_diff_fallback {
            self.selected_request_id = None;
            self.load_diff_sync(&key);
        } else {
            self.selected_request_id = None;
            self.diff_state = DiffDisplayState::Error(DIFF_RUNTIME_UNAVAILABLE_LABEL.to_string());
        }

        self.prefetch_neighbors();
    }

    fn repo_root_for_entry_path(&self, path: &str, staged: bool) -> PathBuf {
        for section in &self.repos {
            let list = if staged {
                &section.staged
            } else {
                &section.changed
            };
            if list.iter().any(|e| e.path == path) {
                return section.project_root.clone();
            }
        }
        self.project_root.clone()
    }

    fn enqueue_diff_request(&mut self, key: DiffCacheKey, high_priority: bool) -> Option<u64> {
        if let Some(existing) = self.pending_requests.get(&key) {
            return Some(*existing);
        }

        let tx = self.diff_runtime_tx.as_ref()?;
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);

        let project_root = self.repo_root_for_entry_path(&key.path, key.staged);
        let sent = tx.send(GitViewDiffCommand::RequestDiff {
            request_id,
            project_root,
            key: key.clone(),
            high_priority,
        });
        if sent.is_err() {
            return None;
        }

        self.pending_requests.insert(key, request_id);
        Some(request_id)
    }

    fn prefetch_neighbors(&mut self) {
        if self.diff_runtime_tx.is_none() || self.diff_prefetch_radius == 0 {
            return;
        }

        let total_rows = self.selectable_row_count();
        if total_rows == 0 {
            return;
        }

        for offset in 1..=self.diff_prefetch_radius {
            if let Some(row) = self.selected.checked_sub(offset) {
                self.prefetch_row(row);
            }
            let row = self.selected.saturating_add(offset);
            if row < total_rows {
                self.prefetch_row(row);
            }
        }
    }

    fn prefetch_row(&mut self, row: usize) {
        let Some(target) = self.selection_target_at(row) else {
            return;
        };
        let Some(entry) = self.entry_for_target(target) else {
            return;
        };
        let key = DiffCacheKey {
            path: entry.path.clone(),
            staged: entry.staged,
        };
        if self.diff_cache.contains_key(&key) || self.pending_requests.contains_key(&key) {
            return;
        }
        let _ = self.enqueue_diff_request(key, false);
    }

    fn load_diff_sync(&mut self, key: &DiffCacheKey) {
        let repo_root = self.repo_root_for_entry_path(&key.path, key.staged);
        match git::git_diff_in(&repo_root, &key.path, key.staged) {
            Ok(diff) => {
                let lines: Vec<String> = diff.lines().map(|line| line.to_string()).collect();
                self.cache_insert(key.clone(), CachedDiffEntry::Ready(lines.clone()));
                self.diff_state = DiffDisplayState::Ready(lines);
            }
            Err(message) => {
                self.cache_insert(key.clone(), CachedDiffEntry::Error(message.clone()));
                self.diff_state = DiffDisplayState::Error(message);
            }
        }
    }

    fn retain_cache_for_current_entries(&mut self) {
        let valid: HashSet<DiffCacheKey> = self
            .repos
            .iter()
            .flat_map(|section| {
                section
                    .changed
                    .iter()
                    .map(|entry| DiffCacheKey {
                        path: entry.path.clone(),
                        staged: false,
                    })
                    .chain(section.staged.iter().map(|entry| DiffCacheKey {
                        path: entry.path.clone(),
                        staged: true,
                    }))
            })
            .collect();

        self.diff_cache.retain(|key, _| valid.contains(key));
        self.diff_cache_order.retain(|key| valid.contains(key));
        self.pending_requests.retain(|key, _| valid.contains(key));

        if let Some(selected_key) = &self.selected_diff_key
            && !valid.contains(selected_key)
        {
            self.selected_diff_key = None;
            self.selected_request_id = None;
            self.diff_state = DiffDisplayState::Idle;
            self.reset_diff_scroll_state();
        }
    }

    fn cache_get(&mut self, key: &DiffCacheKey) -> Option<CachedDiffEntry> {
        let value = self.diff_cache.get(key).cloned();
        if value.is_some() {
            self.cache_touch(key);
        }
        value
    }

    fn cache_insert(&mut self, key: DiffCacheKey, value: CachedDiffEntry) {
        if self.diff_cache_max_entries == 0 {
            return;
        }

        self.diff_cache.insert(key.clone(), value);
        self.cache_touch(&key);

        while self.diff_cache.len() > self.diff_cache_max_entries {
            let Some(evicted) = self.diff_cache_order.pop_front() else {
                break;
            };
            self.diff_cache.remove(&evicted);
        }
    }

    fn cache_touch(&mut self, key: &DiffCacheKey) {
        if let Some(pos) = self
            .diff_cache_order
            .iter()
            .position(|existing| existing == key)
        {
            self.diff_cache_order.remove(pos);
        }
        self.diff_cache_order.push_back(key.clone());
    }

    fn cache_remove(&mut self, key: &DiffCacheKey) {
        self.diff_cache.remove(key);
        if let Some(pos) = self
            .diff_cache_order
            .iter()
            .position(|existing| existing == key)
        {
            self.diff_cache_order.remove(pos);
        }
    }

    fn invalidate_cache_for_path(&mut self, path: &str) {
        for staged in [false, true] {
            let key = DiffCacheKey {
                path: path.to_string(),
                staged,
            };
            self.cache_remove(&key);
            self.pending_requests.remove(&key);
        }
    }

    fn invalidate_cache_for_paths(&mut self, paths: &[String]) {
        for path in paths {
            self.invalidate_cache_for_path(path);
        }
    }

    pub fn on_diff_event(&mut self, event: GitViewDiffEvent) {
        let (request_id, key, cached_entry) = match event {
            GitViewDiffEvent::DiffReady {
                request_id,
                key,
                lines,
            } => (request_id, key, CachedDiffEntry::Ready(lines)),
            GitViewDiffEvent::DiffError {
                request_id,
                key,
                message,
            } => (request_id, key, CachedDiffEntry::Error(message)),
        };

        if !self.has_entry_for_key(&key) {
            self.pending_requests.remove(&key);
            return;
        }

        let Some(expected_request_id) = self.pending_requests.get(&key).copied() else {
            return;
        };
        if expected_request_id != request_id {
            return;
        }
        self.pending_requests.remove(&key);

        self.cache_insert(key.clone(), cached_entry.clone());

        if self.selected_diff_key.as_ref() == Some(&key)
            && self.selected_request_id.is_none_or(|id| id == request_id)
        {
            self.selected_request_id = None;
            self.diff_state = cached_entry.into_display_state();
            self.reset_diff_scroll_state();
        }
    }

    fn diff_max_scroll(&self, content_h: usize) -> usize {
        if content_h == 0 {
            return 0;
        }

        let line_count = self
            .current_diff_lines()
            .map(|lines| lines.len())
            .unwrap_or(0);
        line_count.saturating_sub(content_h)
    }

    fn clamp_diff_scroll(&mut self, content_h: usize) {
        let max_scroll = self.diff_max_scroll(content_h);
        if self.diff_scroll > max_scroll {
            self.diff_scroll = max_scroll;
        }
    }

    fn scroll_diff_down_lines(&mut self, lines: usize, content_h: usize) {
        let max_scroll = self.diff_max_scroll(content_h);
        self.diff_scroll = self.diff_scroll.saturating_add(lines).min(max_scroll);
    }

    fn scroll_diff_up_lines(&mut self, lines: usize) {
        self.diff_scroll = self.diff_scroll.saturating_sub(lines);
    }

    fn scroll_diff_down(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_add(1);
    }

    fn scroll_diff_up(&mut self) {
        self.scroll_diff_up_lines(1);
    }

    fn diff_max_horizontal_scroll(&self, content_w: usize) -> usize {
        if content_w == 0 {
            return 0;
        }
        self.current_diff_lines()
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| display_width(line).saturating_sub(content_w))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    fn clamp_diff_horizontal_scroll(&mut self, content_w: usize) {
        let max_scroll = self.diff_max_horizontal_scroll(content_w);
        if self.diff_horizontal_scroll > max_scroll {
            self.diff_horizontal_scroll = max_scroll;
        }
    }

    fn scroll_diff_right(&mut self) {
        self.diff_horizontal_scroll = self
            .diff_horizontal_scroll
            .saturating_add(HORIZONTAL_SCROLL_COLS);
    }

    fn scroll_diff_left(&mut self) {
        self.diff_horizontal_scroll = self
            .diff_horizontal_scroll
            .saturating_sub(HORIZONTAL_SCROLL_COLS);
    }

    fn popup_size(cols: usize, rows: usize) -> (usize, usize) {
        crate::ui::popup_layout::popup_size(cols, rows)
    }

    fn diff_content_height_for_surface(cols: usize, rows: usize) -> Option<usize> {
        let (popup_w, popup_h) = Self::popup_size(cols, rows);
        if popup_w >= DIFF_SPLIT_THRESHOLD {
            Some(popup_h.saturating_sub(2))
        } else {
            None
        }
    }

    pub fn handle_mouse_scroll(
        &mut self,
        kind: MouseEventKind,
        cols: usize,
        rows: usize,
    ) -> EventResult {
        let Some(content_h) = Self::diff_content_height_for_surface(cols, rows) else {
            return EventResult::Ignored;
        };
        match kind {
            MouseEventKind::ScrollDown => {
                self.scroll_diff_down_lines(MOUSE_SCROLL_LINES, content_h);
                EventResult::Consumed
            }
            MouseEventKind::ScrollUp => {
                self.scroll_diff_up_lines(MOUSE_SCROLL_LINES);
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }

    fn single_file_stage_or_unstage(&mut self, repo_idx: usize, path: String, staged: bool) {
        let repo_root = self
            .repos
            .get(repo_idx)
            .map(|s| s.project_root.clone())
            .unwrap_or_else(|| self.project_root.clone());
        if staged {
            match git::git_unstage_in(&repo_root, &path) {
                Ok(()) => self.message = Some(format!("Unstaged: {}", path)),
                Err(e) => {
                    self.message = Some(e);
                    return;
                }
            }
        } else {
            match git::git_stage_in(&repo_root, &path) {
                Ok(()) => self.message = Some(format!("Staged: {}", path)),
                Err(e) => {
                    self.message = Some(e);
                    return;
                }
            }
        }
        self.invalidate_cache_for_path(&path);
        self.refresh();
    }

    fn bulk_result_message(action: &str, result: &git::GitBatchOperationResult) -> String {
        let total = result.total();
        if result.failures.is_empty() {
            format!("{} {}/{}", action, result.successes, total)
        } else if let Some((path, err)) = result.failures.first() {
            format!(
                "{} {}/{} ({} failed): {}: {}",
                action,
                result.successes,
                total,
                result.failures.len(),
                path,
                err
            )
        } else {
            format!(
                "{} {}/{} ({} failed)",
                action,
                result.successes,
                total,
                result.failures.len()
            )
        }
    }

    fn stage_all_changed(&mut self, repo_idx: usize) {
        let Some(section) = self.repos.get(repo_idx) else {
            return;
        };
        let paths: Vec<String> = section
            .changed
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        if paths.is_empty() {
            self.message = Some("No changed files to stage".to_string());
            return;
        }
        let result = git::git_stage_many_in(&section.project_root, &paths);
        self.invalidate_cache_for_paths(&paths);
        self.refresh();
        self.message = Some(Self::bulk_result_message("Staged", &result));
    }

    fn unstage_all_staged(&mut self, repo_idx: usize) {
        let Some(section) = self.repos.get(repo_idx) else {
            return;
        };
        let paths: Vec<String> = section
            .staged
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        if paths.is_empty() {
            self.message = Some("No staged files to unstage".to_string());
            return;
        }
        let result = git::git_unstage_many_in(&section.project_root, &paths);
        self.invalidate_cache_for_paths(&paths);
        self.refresh();
        self.message = Some(Self::bulk_result_message("Unstaged", &result));
    }

    fn stage_or_unstage(&mut self) {
        match self.selected_target() {
            Some(SelectionTarget::ChangedHeader(ri)) => self.stage_all_changed(ri),
            Some(SelectionTarget::StagedHeader(ri)) => self.unstage_all_staged(ri),
            Some(SelectionTarget::ChangedFile(ri, idx)) => {
                if let Some(entry) = self.repos.get(ri).and_then(|s| s.changed.get(idx)) {
                    self.single_file_stage_or_unstage(ri, entry.path.clone(), false);
                }
            }
            Some(SelectionTarget::StagedFile(ri, idx)) => {
                if let Some(entry) = self.repos.get(ri).and_then(|s| s.staged.get(idx)) {
                    self.single_file_stage_or_unstage(ri, entry.path.clone(), true);
                }
            }
            Some(SelectionTarget::RepoHeader(_)) | None => {}
        }
    }

    fn handle_find_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('q') => EventResult::Action(Action::Ui(UiAction::CloseGitView)),
                KeyCode::Char('n') => {
                    self.move_down();
                    EventResult::Consumed
                }
                KeyCode::Char('p') => {
                    self.move_up();
                    EventResult::Consumed
                }
                KeyCode::Char('f') => {
                    self.scroll_diff_down();
                    EventResult::Consumed
                }
                KeyCode::Char('b') => {
                    self.scroll_diff_up();
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
                self.scroll_diff_up();
                EventResult::Consumed
            }
            KeyCode::Right => {
                self.scroll_diff_down();
                EventResult::Consumed
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.find_input.push(c);
                self.jump_to_best_match();
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }

    fn delete_prev_word(&mut self) {
        delete_prev_word_input(&mut self.find_input);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            return EventResult::Action(Action::Ui(UiAction::CloseGitView));
        }

        // Always let the global Ctrl+0..9 chords punch through.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('0'..='9'))
        {
            return EventResult::Ignored;
        }

        if self.find_active {
            return self.handle_find_key(key);
        }

        let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('n') => {
                    if has_shift {
                        self.scroll_diff_down();
                    } else {
                        self.move_down();
                    }
                    EventResult::Consumed
                }
                KeyCode::Char('p') => {
                    if has_shift {
                        self.scroll_diff_up();
                    } else {
                        self.move_up();
                    }
                    EventResult::Consumed
                }
                _ => EventResult::Consumed,
            };
        }

        if has_shift {
            return match key.code {
                KeyCode::Char('J') | KeyCode::Down => {
                    self.scroll_diff_down();
                    EventResult::Consumed
                }
                KeyCode::Char('K') | KeyCode::Up => {
                    self.scroll_diff_up();
                    EventResult::Consumed
                }
                KeyCode::Char('L') | KeyCode::Right => {
                    self.scroll_diff_right();
                    EventResult::Consumed
                }
                KeyCode::Char('H') | KeyCode::Left => {
                    self.scroll_diff_left();
                    EventResult::Consumed
                }
                KeyCode::Char('C') => EventResult::Action(Action::App(AppAction::Workspace(
                    WorkspaceAction::OpenGitCommitMessageBuffer,
                ))),
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
            KeyCode::Char('/') => {
                self.find_active = true;
                self.find_input.clear();
                EventResult::Consumed
            }
            KeyCode::Char('u') => {
                self.stage_or_unstage();
                EventResult::Consumed
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(target) = self.selected_target() {
                    if let Some(entry) = self.entry_for_target(target) {
                        let repo_root = self.repo_root_for_target(&target);
                        let path = if self.is_multi_repo() {
                            // In multi-repo mode, emit path relative to project_root
                            // by joining repo display name + file path
                            let repo_name = self
                                .repos
                                .get(target.repo_idx())
                                .map(|s| s.display_name.as_str())
                                .unwrap_or("");
                            format!("{}/{}", repo_name, entry.path)
                        } else {
                            entry.path.clone()
                        };
                        let _ = repo_root; // used for context; path resolves via project_root
                        let line = self.first_changed_line_for_selected();
                        EventResult::Action(Action::App(AppAction::Buffer(
                            BufferAction::OpenFileFromGitView { path, line },
                        )))
                    } else {
                        EventResult::Consumed
                    }
                } else {
                    EventResult::Consumed
                }
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.message = Some("Refreshed".to_string());
                EventResult::Consumed
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                EventResult::Action(Action::Ui(UiAction::CloseGitView))
            }
            _ => EventResult::Consumed,
        }
    }

    pub fn render_overlay(&mut self, surface: &mut Surface) -> Option<(u16, u16)> {
        let cols = surface.width;
        let rows = surface.height;
        let (popup_w, popup_h) = Self::popup_size(cols, rows);
        let offset_x = (cols.saturating_sub(popup_w)) / 2;
        let offset_y = (rows.saturating_sub(popup_h)) / 2;

        if popup_w >= DIFF_SPLIT_THRESHOLD {
            let gap = 2;
            let left_w = (popup_w - gap) / 2;
            let right_w = popup_w - gap - left_w;
            let right_x = offset_x + left_w + gap;

            let cursor = self.render_file_panel(surface, offset_x, offset_y, left_w, popup_h);
            self.render_diff_panel(surface, right_x, offset_y, right_w, popup_h);
            cursor
        } else {
            self.render_file_panel(surface, offset_x, offset_y, popup_w, popup_h)
        }
    }

    fn render_file_panel(
        &mut self,
        surface: &mut Surface,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
    ) -> Option<(u16, u16)> {
        let inner_w = w.saturating_sub(2);
        let default_style = CellStyle::default();
        let multi = self.is_multi_repo();

        // Layout:
        // row 0: top border
        // row 1: branch name (single-repo) or title (multi-repo)
        // row 2..h-2: file list content (with headers)
        // row h-2: message row
        // row h-1: bottom border

        let content_start = 2;
        let content_end = h.saturating_sub(2);
        let content_h = content_end.saturating_sub(content_start);

        // Build flat display list: selectable headers + files (each file is two rows)
        let mut display_items: Vec<DisplayItem> = Vec::new();
        let mut selectable_idx: usize = 0;
        for section in &self.repos {
            let has_entries = !section.changed.is_empty() || !section.staged.is_empty();
            if !has_entries {
                continue;
            }

            if multi {
                let row_idx = selectable_idx;
                selectable_idx += 1;
                display_items.push(DisplayItem::RepoHeader(
                    format!("\u{e0a0} {} ({})", section.display_name, section.branch),
                    row_idx == self.selected,
                ));
            }

            if !section.changed.is_empty() {
                let header_idx = selectable_idx;
                selectable_idx += 1;
                display_items.push(DisplayItem::Header(
                    format!("Changed ({})", section.changed.len()),
                    header_idx == self.selected,
                ));
                for entry in &section.changed {
                    let row_idx = selectable_idx;
                    selectable_idx += 1;
                    let is_sel = row_idx == self.selected;
                    display_items.push(DisplayItem::File {
                        label: format!(" [{}] {}", entry.status_char, entry.path),
                        selected: is_sel,
                    });
                    display_items.push(DisplayItem::FileStats {
                        additions: entry.additions,
                        deletions: entry.deletions,
                        selected: is_sel,
                    });
                }
            }
            if !section.staged.is_empty() {
                let header_idx = selectable_idx;
                selectable_idx += 1;
                display_items.push(DisplayItem::Header(
                    format!("Staged ({})", section.staged.len()),
                    header_idx == self.selected,
                ));
                for entry in &section.staged {
                    let row_idx = selectable_idx;
                    selectable_idx += 1;
                    let is_sel = row_idx == self.selected;
                    display_items.push(DisplayItem::File {
                        label: format!(" [{}] {}", entry.status_char, entry.path),
                        selected: is_sel,
                    });
                    display_items.push(DisplayItem::FileStats {
                        additions: entry.additions,
                        deletions: entry.deletions,
                        selected: is_sel,
                    });
                }
            }
        }

        // Find the position of the selected item in display_items
        let sel_display_pos = display_items
            .iter()
            .position(DisplayItem::is_selected)
            .unwrap_or(0);
        // If the selected item is a File, also try to keep its FileStats row visible.
        let sel_display_end = match display_items.get(sel_display_pos) {
            Some(DisplayItem::File { .. })
                if matches!(
                    display_items.get(sel_display_pos + 1),
                    Some(DisplayItem::FileStats { .. })
                ) =>
            {
                sel_display_pos + 1
            }
            _ => sel_display_pos,
        };

        // Adjust scroll_offset
        if sel_display_pos < self.scroll_offset {
            self.scroll_offset = sel_display_pos;
        }
        if sel_display_end >= self.scroll_offset + content_h {
            self.scroll_offset = sel_display_end.saturating_sub(content_h.saturating_sub(1));
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
            } else if row == 1 {
                // Branch row (single-repo) or title row (multi-repo)
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                let branch_label = if multi {
                    format!(" Git Status ({} repos)", self.repos.len())
                } else {
                    let branch = self
                        .repos
                        .first()
                        .map(|s| s.branch.as_str())
                        .unwrap_or("???");
                    format!(" \u{e0a0} {}", branch)
                };
                let branch_style = CellStyle {
                    bold: true,
                    fg: Some(Color::Cyan),
                    ..CellStyle::default()
                };
                let (truncated, used) = truncate_to_width(&branch_label, inner_w);
                surface.put_str(x + 1, y + row, truncated, &branch_style);
                if used < inner_w {
                    surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &default_style);
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else if row == h - 2 {
                // Message row
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                let row_style = CellStyle {
                    reverse: true,
                    ..CellStyle::default()
                };
                if self.find_active {
                    let prompt = format!("/{}", self.find_input);
                    let (truncated, used) = truncate_to_width(&prompt, inner_w);
                    surface.put_str(x + 1, y + row, truncated, &row_style);
                    if used < inner_w {
                        surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &row_style);
                    }
                } else if let Some(ref msg) = self.message {
                    let (truncated, used) = truncate_to_width(msg, inner_w);
                    surface.put_str(x + 1, y + row, truncated, &row_style);
                    if used < inner_w {
                        surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &row_style);
                    }
                } else {
                    let hint = "u:toggle/header=all C:commit o:open r:refresh q:close";
                    let (truncated, used) = truncate_to_width(hint, inner_w);
                    let dim_style = CellStyle {
                        dim: true,
                        reverse: true,
                        ..CellStyle::default()
                    };
                    surface.put_str(x + 1, y + row, truncated, &dim_style);
                    if used < inner_w {
                        surface.fill_region(x + 1 + used, y + row, inner_w - used, ' ', &dim_style);
                    }
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else {
                // Content rows
                let content_row = row - content_start;
                let display_idx = self.scroll_offset + content_row;

                surface.put_str(x, y + row, "\u{2502}", &default_style);

                if display_idx < display_items.len() {
                    match &display_items[display_idx] {
                        DisplayItem::RepoHeader(label, selected) => {
                            let header_style = if *selected {
                                CellStyle {
                                    bold: true,
                                    reverse: true,
                                    fg: Some(Color::Cyan),
                                    ..CellStyle::default()
                                }
                            } else {
                                CellStyle {
                                    bold: true,
                                    fg: Some(Color::Cyan),
                                    ..CellStyle::default()
                                }
                            };
                            let (truncated, used) = truncate_to_width(label, inner_w);
                            surface.put_str(x + 1, y + row, truncated, &header_style);
                            if used < inner_w {
                                surface.fill_region(
                                    x + 1 + used,
                                    y + row,
                                    inner_w - used,
                                    ' ',
                                    &header_style,
                                );
                            }
                        }
                        DisplayItem::Header(label, selected) => {
                            let header_style = if *selected {
                                CellStyle {
                                    bold: true,
                                    reverse: true,
                                    ..CellStyle::default()
                                }
                            } else {
                                CellStyle {
                                    bold: true,
                                    dim: true,
                                    ..CellStyle::default()
                                }
                            };
                            let (truncated, used) = truncate_to_width(label, inner_w);
                            surface.put_str(x + 1, y + row, truncated, &header_style);
                            if used < inner_w {
                                surface.fill_region(
                                    x + 1 + used,
                                    y + row,
                                    inner_w - used,
                                    ' ',
                                    &header_style,
                                );
                            }
                        }
                        DisplayItem::File {
                            label, selected, ..
                        } => {
                            let style = if *selected {
                                CellStyle {
                                    reverse: true,
                                    ..CellStyle::default()
                                }
                            } else {
                                CellStyle::default()
                            };
                            let (truncated, used) = truncate_to_width(label, inner_w);
                            surface.put_str(x + 1, y + row, truncated, &style);
                            if used < inner_w {
                                surface.fill_region(
                                    x + 1 + used,
                                    y + row,
                                    inner_w - used,
                                    ' ',
                                    &style,
                                );
                            }
                        }
                        DisplayItem::FileStats {
                            additions,
                            deletions,
                            selected,
                        } => {
                            let base = if *selected {
                                CellStyle {
                                    reverse: true,
                                    ..CellStyle::default()
                                }
                            } else {
                                CellStyle::default()
                            };
                            let add_style = CellStyle {
                                fg: Some(Color::Green),
                                ..base
                            };
                            let del_style = CellStyle {
                                fg: Some(Color::Red),
                                ..base
                            };
                            // Indent under the [X] file label: 5 spaces lines up past " [M] ".
                            let indent = "     ";
                            let adds_str = format!("+{}", additions);
                            let dels_str = format!("-{}", deletions);
                            surface.fill_region(x + 1, y + row, inner_w, ' ', &base);
                            let mut col = x + 1;
                            let indent_w = crate::ui::text::display_width(indent);
                            if indent_w <= inner_w {
                                surface.put_str(col, y + row, indent, &base);
                                col += indent_w;
                            }
                            let adds_w = crate::ui::text::display_width(&adds_str);
                            if col + adds_w <= x + 1 + inner_w {
                                surface.put_str(col, y + row, &adds_str, &add_style);
                                col += adds_w;
                            }
                            if col < x + 1 + inner_w {
                                surface.put_str(col, y + row, " ", &base);
                                col += 1;
                            }
                            let dels_w = crate::ui::text::display_width(&dels_str);
                            if col + dels_w <= x + 1 + inner_w {
                                surface.put_str(col, y + row, &dels_str, &del_style);
                            }
                        }
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }

                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            }
        }

        if self.find_active {
            let find_row = y + h.saturating_sub(2);
            let prompt = format!("/{}", self.find_input);
            let (_, used) = truncate_to_width(&prompt, inner_w);
            let cursor_x = (x + 1 + used) as u16;
            let cursor_y = find_row as u16;
            Some((cursor_x, cursor_y))
        } else {
            None
        }
    }

    fn render_diff_panel(&mut self, surface: &mut Surface, x: usize, y: usize, w: usize, h: usize) {
        let inner_w = w.saturating_sub(2);
        let content_h = h.saturating_sub(2);
        let default_style = CellStyle::default();
        self.clamp_diff_scroll(content_h);
        self.clamp_diff_horizontal_scroll(inner_w);

        let placeholder = match &self.diff_state {
            DiffDisplayState::Loading => Some((
                DIFF_LOADING_LABEL,
                CellStyle {
                    dim: true,
                    ..CellStyle::default()
                },
            )),
            DiffDisplayState::Error(message) => Some((
                message.as_str(),
                CellStyle {
                    fg: Some(Color::Red),
                    ..CellStyle::default()
                },
            )),
            DiffDisplayState::Idle => Some((
                DIFF_IDLE_LABEL,
                CellStyle {
                    dim: true,
                    ..CellStyle::default()
                },
            )),
            DiffDisplayState::Ready(_) => None,
        };

        for row in 0..h {
            if row == 0 {
                surface.put_str(x, y + row, "\u{250c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2510}", &default_style);
                continue;
            }

            if row == h - 1 {
                surface.put_str(x, y + row, "\u{2514}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2518}", &default_style);
                continue;
            }

            surface.put_str(x, y + row, "\u{2502}", &default_style);

            if let Some((text, style)) = placeholder {
                let placeholder_row = 1 + content_h / 2;
                if row == placeholder_row {
                    let (truncated, used) = truncate_to_width(text, inner_w);
                    surface.put_str(x + 1, y + row, truncated, &style);
                    if used < inner_w {
                        surface.fill_region(
                            x + 1 + used,
                            y + row,
                            inner_w - used,
                            ' ',
                            &default_style,
                        );
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }
            } else if let Some(lines) = self.current_diff_lines() {
                let line_idx = self.diff_scroll + (row - 1);
                if line_idx < lines.len() && (row - 1) < content_h {
                    let line = &lines[line_idx];
                    let style = diff_line_style(line);
                    let window = slice_display_window(line, self.diff_horizontal_scroll, inner_w);
                    surface.put_str(x + 1, y + row, window.visible, &style);
                    if window.used_width < inner_w {
                        surface.fill_region(
                            x + 1 + window.used_width,
                            y + row,
                            inner_w - window.used_width,
                            ' ',
                            &default_style,
                        );
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }
            } else {
                surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
            }

            surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
        }
    }
}

enum DisplayItem {
    RepoHeader(String, bool),
    Header(String, bool),
    File {
        label: String,
        selected: bool,
    },
    FileStats {
        additions: usize,
        deletions: usize,
        selected: bool,
    },
}

impl DisplayItem {
    fn is_selected(&self) -> bool {
        match self {
            DisplayItem::RepoHeader(_, selected) => *selected,
            DisplayItem::Header(_, selected) => *selected,
            DisplayItem::File { selected, .. } => *selected,
            DisplayItem::FileStats { .. } => false,
        }
    }
}

/// Scan unified-diff `lines` for the first added/removed line and return its
/// 0-based position in the new file.
fn first_changed_line_in_lines(lines: &[String]) -> Option<usize> {
    let mut new_line: Option<usize> = None;
    for line in lines {
        if line.starts_with("@@") {
            new_line = parse_hunk_new_start(line).map(|start| start.saturating_sub(1));
            continue;
        }

        let Some(current_line) = new_line else {
            continue;
        };

        if line.starts_with(' ') {
            new_line = Some(current_line.saturating_add(1));
            continue;
        }

        if line.starts_with('+') && !line.starts_with("+++") {
            return Some(current_line);
        }

        if line.starts_with('-') && !line.starts_with("---") {
            return Some(current_line);
        }
    }
    None
}

fn parse_hunk_new_start(line: &str) -> Option<usize> {
    let plus_idx = line.find('+')?;
    let after_plus = &line[plus_idx + 1..];
    let digits: String = after_plus
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<usize>().ok()
}

fn diff_line_style(line: &str) -> CellStyle {
    if line.starts_with('+') {
        CellStyle {
            fg: Some(Color::Green),
            ..CellStyle::default()
        }
    } else if line.starts_with('-') {
        CellStyle {
            fg: Some(Color::Red),
            ..CellStyle::default()
        }
    } else if line.starts_with("@@") {
        CellStyle {
            fg: Some(Color::Cyan),
            ..CellStyle::default()
        }
    } else {
        CellStyle {
            dim: true,
            ..CellStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn test_view() -> GitView {
        GitView {
            project_root: PathBuf::from("."),
            repos: vec![RepoSection {
                project_root: PathBuf::from("."),
                display_name: "test".to_string(),
                branch: "main".to_string(),
                changed: vec![
                    GitFileEntry {
                        path: "src/lib.rs".to_string(),
                        status_char: 'M',
                        staged: false,
                        additions: 0,
                        deletions: 0,
                    },
                    GitFileEntry {
                        path: "README.md".to_string(),
                        status_char: 'M',
                        staged: false,
                        additions: 0,
                        deletions: 0,
                    },
                ],
                staged: vec![GitFileEntry {
                    path: "src/ui/explorer.rs".to_string(),
                    status_char: 'A',
                    staged: true,
                    additions: 0,
                    deletions: 0,
                }],
            }],
            selected: 0,
            scroll_offset: 0,
            find_active: false,
            find_input: String::new(),
            diff_state: DiffDisplayState::Idle,
            diff_scroll: 0,
            diff_horizontal_scroll: 0,
            message: None,
            diff_runtime_tx: None,
            allow_sync_diff_fallback: false,
            diff_cache: HashMap::new(),
            diff_cache_order: VecDeque::new(),
            diff_cache_max_entries: DEFAULT_DIFF_CACHE_MAX_ENTRIES,
            diff_prefetch_radius: 0,
            pending_requests: HashMap::new(),
            next_request_id: 1,
            selected_diff_key: None,
            selected_request_id: None,
        }
    }

    #[test]
    fn slash_search_jumps_to_best_match() {
        let mut view = test_view();
        view.handle_key(key(KeyCode::Char('/')));
        view.handle_key(key(KeyCode::Char('e')));
        view.handle_key(key(KeyCode::Char('x')));
        view.handle_key(key(KeyCode::Char('p')));

        assert!(view.find_active);
        assert_eq!(
            view.selected_entry().map(|entry| entry.path.as_str()),
            Some("src/ui/explorer.rs")
        );
    }

    #[test]
    fn search_mode_navigation_keys_work() {
        let mut view = test_view();
        view.handle_key(key(KeyCode::Char('/')));
        view.handle_key(ctrl_key('n'));
        assert_eq!(view.selected, 1);
        view.handle_key(ctrl_key('p'));
        assert_eq!(view.selected, 0);
        view.handle_key(key(KeyCode::Down));
        assert_eq!(view.selected, 1);
        view.handle_key(key(KeyCode::Up));
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn navigation_moves_across_headers_and_files() {
        let mut view = test_view();
        assert_eq!(
            view.selected_target(),
            Some(SelectionTarget::ChangedHeader(0))
        );
        view.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            view.selected_target(),
            Some(SelectionTarget::ChangedFile(0, 0))
        );
        view.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            view.selected_target(),
            Some(SelectionTarget::ChangedFile(0, 1))
        );
        view.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            view.selected_target(),
            Some(SelectionTarget::StagedHeader(0))
        );
    }

    #[test]
    fn request_diff_from_header_sets_idle_state() {
        let mut view = test_view();
        view.diff_state = DiffDisplayState::Error("stale".to_string());
        view.selected = 0; // Changed header
        view.request_selected_diff();
        assert!(matches!(view.diff_state, DiffDisplayState::Idle));
    }

    #[test]
    fn apply_file_entries_preserves_header_selection() {
        let mut view = test_view();
        view.selected = 3; // Staged header with current test data
        let changed = view.repos[0].changed.clone();
        let staged = view.repos[0].staged.clone();
        view.apply_file_entries(0, changed, staged);
        assert_eq!(
            view.selected_target(),
            Some(SelectionTarget::StagedHeader(0))
        );
    }

    #[test]
    fn search_mode_ctrl_w_ctrl_u_and_ctrl_k_edit_query() {
        let mut view = test_view();
        view.handle_key(key(KeyCode::Char('/')));
        for c in "src ui explorer".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        view.handle_key(ctrl_key('w'));
        assert_eq!(view.find_input, "src ui ");
        view.handle_key(ctrl_key('u'));
        assert!(view.find_input.is_empty());
        for c in "tmp new".chars() {
            view.handle_key(key(KeyCode::Char(c)));
        }
        view.handle_key(ctrl_key('k'));
        assert!(view.find_input.is_empty());
    }

    #[test]
    fn first_changed_line_skips_hunk_context() {
        let mut view = test_view();
        view.diff_state = DiffDisplayState::Ready(vec![
            "diff --git a/src/lib.rs b/src/lib.rs".to_string(),
            "index 1111111..2222222 100644".to_string(),
            "--- a/src/lib.rs".to_string(),
            "+++ b/src/lib.rs".to_string(),
            "@@ -10,4 +10,5 @@".to_string(),
            " context".to_string(),
            "-old value".to_string(),
            "+new value".to_string(),
            " tail".to_string(),
        ]);

        assert_eq!(view.first_changed_line_in_diff(), Some(10));
    }

    #[test]
    fn first_changed_line_for_selected_falls_back_to_cached_diff() {
        let mut view = test_view();
        view.selected = 1; // first changed file: src/lib.rs
        // Async diff has not arrived: diff_state stays Idle.
        view.diff_cache.insert(
            DiffCacheKey {
                path: "src/lib.rs".to_string(),
                staged: false,
            },
            CachedDiffEntry::Ready(vec![
                "@@ -22,3 +22,4 @@".to_string(),
                " context".to_string(),
                " context".to_string(),
                "+added line".to_string(),
            ]),
        );

        assert!(view.first_changed_line_in_diff().is_none());
        assert_eq!(view.first_changed_line_for_selected(), Some(23));
    }

    #[test]
    fn mouse_scroll_clamps_diff_to_visible_content() {
        let mut view = test_view();
        view.diff_state = DiffDisplayState::Ready((0..100).map(|i| format!("line {i}")).collect());

        let cols = 100;
        let rows = 20;
        let content_h = GitView::diff_content_height_for_surface(cols, rows).unwrap();
        let max_scroll = view.diff_max_scroll(content_h);

        for _ in 0..200 {
            let result = view.handle_mouse_scroll(MouseEventKind::ScrollDown, cols, rows);
            assert!(matches!(result, EventResult::Consumed));
        }

        assert_eq!(view.diff_scroll, max_scroll);
    }

    #[test]
    fn mouse_scroll_ignored_without_split_diff_panel() {
        let mut view = test_view();
        let result = view.handle_mouse_scroll(MouseEventKind::ScrollDown, 40, 20);
        assert!(matches!(result, EventResult::Ignored));
    }

    #[test]
    fn shift_h_l_scrolls_diff_horizontally() {
        let mut view = test_view();
        assert_eq!(view.diff_horizontal_scroll, 0);
        view.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert_eq!(view.diff_horizontal_scroll, HORIZONTAL_SCROLL_COLS);
        view.handle_key(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT));
        assert_eq!(view.diff_horizontal_scroll, 0);
    }

    #[test]
    fn shift_c_opens_git_commit_message_buffer() {
        let mut view = test_view();
        let result = view.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT));
        assert!(matches!(
            result,
            EventResult::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenGitCommitMessageBuffer
            )))
        ));
    }

    #[test]
    fn diff_event_updates_selected_diff_state() {
        let mut view = test_view();
        let key = DiffCacheKey {
            path: "src/lib.rs".to_string(),
            staged: false,
        };
        view.selected_diff_key = Some(key.clone());
        view.selected_request_id = Some(7);
        view.pending_requests.insert(key.clone(), 7);

        view.on_diff_event(GitViewDiffEvent::DiffReady {
            request_id: 7,
            key,
            lines: vec!["@@ -1 +1 @@".to_string(), "+line".to_string()],
        });

        assert!(matches!(view.diff_state, DiffDisplayState::Ready(_)));
        assert_eq!(view.selected_request_id, None);
    }

    #[test]
    fn stale_diff_event_is_ignored() {
        let mut view = test_view();
        let key = DiffCacheKey {
            path: "src/lib.rs".to_string(),
            staged: false,
        };
        view.selected_diff_key = Some(key.clone());
        view.selected_request_id = Some(8);
        view.pending_requests.insert(key.clone(), 8);

        view.on_diff_event(GitViewDiffEvent::DiffReady {
            request_id: 6,
            key,
            lines: vec!["stale".to_string()],
        });

        assert!(matches!(view.diff_state, DiffDisplayState::Idle));
        assert_eq!(view.selected_request_id, Some(8));
    }
}

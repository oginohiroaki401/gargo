use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use std::rc::Rc;

use crossterm::{
    event::{Event, KeyCode, KeyModifiers, MouseEventKind},
    terminal,
};
use event_loop::{FRAME_DURATION_60_FPS, poll_event_until};

use crate::command::commit_log_runtime::{CommitLogEvent, CommitLogRuntimeHandle};
use crate::command::file_index_runtime::{
    FileIndexRuntimeCommand, FileIndexRuntimeEvent, FileIndexRuntimeHandle,
};
use crate::command::git::GitFileStatus;
use crate::command::git_index_runtime::{
    GitIndexRuntimeCommand, GitIndexRuntimeEvent, GitIndexRuntimeHandle, GitIndexSnapshot,
    collect_git_index_snapshot,
};
use crate::command::git_runtime::{
    GitRuntimeCommand, GitRuntimeDebounceConfig, GitRuntimeEvent, GitRuntimeHandle,
};
use crate::command::git_view_diff_runtime::GitViewDiffRuntimeHandle;
use crate::command::history::CommandHistory;
use crate::command::in_editor_diff::{DiffJumpTarget, InEditorDiffView, build_in_editor_diff_view};
use crate::command::recent_projects::RecentProjectsStore;
use crate::command::registry::{CommandContext, CommandEffect, CommandRegistry, copy_to_clipboard};
use crate::command::update_check_runtime::{UpdateCheckRuntimeEvent, UpdateCheckRuntimeHandle};
use crate::config::{Config, FileIndexMode, app_data_dir};
use crate::core::document::Selection;
use crate::core::editor::{Editor, JumpLocation};
use crate::core::markdown_link::link_edit_context_at_cursor;
use crate::core::mode;
use crate::input::action::{
    Action, AppAction, BufferAction, CoreAction, IntegrationAction, LifecycleAction,
    NavigationAction, ProjectAction, UiAction, WindowAction, WorkspaceAction,
};
use crate::input::chord::KeyState;
use crate::log::debug_log;
use crate::plugin::host::PluginHost;
use crate::plugin::registry::build_plugin_host;
use crate::plugin::types::{LspPickerLocation, PluginContext, PluginEvent, PluginOutput};
use crate::syntax::symbol::{extract_definition_sections, extract_symbols};
use crate::syntax::theme::Theme;
use crate::ui::framework::component::{EventResult, RenderContext};
use crate::ui::framework::compositor::Compositor;
use crate::ui::overlays::explorer::popup::ExplorerPopup;
use crate::ui::overlays::explorer::sidebar::Explorer;
use crate::ui::overlays::git::view::{GitView, GitViewIndexSnapshot, RepoSection};
use crate::ui::overlays::palette::Palette;
use crate::ui::overlays::palette::{
    GitBranchPickerEntry, JumpPickerEntry, ReferencePickerEntry, SmartCopyPickerEntry,
};
use crate::ui::overlays::project::root_picker::ProjectRootPopup;
use crate::ui::overlays::project::save_as_popup::SaveAsPopup;
use crate::ui::shared::filtering::fuzzy_match;
use crate::ui::views::text_view::reserved_left_gutter_width;
use crate::upgrade::UpgradeCheckStatus;
use serde::{Deserialize, Serialize};

#[path = "app/dispatch_app.rs"]
mod dispatch_app;
#[path = "app/dispatch_core.rs"]
mod dispatch_core;
#[path = "app/event_loop.rs"]
mod event_loop;

const DIRTY_CLOSE_WARNING: &str = "buffer is dirty. ctrl c to force close.";
const CLOSE_ABORTED_MESSAGE: &str = "Close aborted";
const UPDATE_CHECK_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

struct ClosedBufferInfo {
    doc_id: usize,
    path: Option<PathBuf>,
}

struct InEditorDiffBufferState {
    line_targets: HashMap<usize, DiffJumpTarget>,
}

struct GitCommitBufferState {
    project_root: PathBuf,
    commit_editmsg_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountBehavior {
    Repeat,
    LineSelect,
    AbsoluteLineJump,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UpdateCheckCache {
    checked_at_unix_secs: u64,
    current_version: String,
    latest_version: String,
    has_update: bool,
}

impl UpdateCheckCache {
    fn from_status(status: &UpgradeCheckStatus, checked_at_unix_secs: u64) -> Self {
        Self {
            checked_at_unix_secs,
            current_version: status.current_version().to_string(),
            latest_version: status.latest_version().to_string(),
            has_update: status.has_update(),
        }
    }
}

pub struct App {
    editor: Editor,
    compositor: Compositor,
    registry: CommandRegistry,
    config: Config,
    theme: Theme,
    project_root: PathBuf,
    file_list: Vec<String>,
    file_index_loading: bool,
    file_index_requested_for_root: bool,
    key_state: KeyState,
    last_explorer_dir: Option<PathBuf>,
    last_explorer_selected: Option<String>,
    close_confirm: bool,
    git_status_cache: HashMap<String, GitFileStatus>,
    discovered_repos: Vec<PathBuf>,
    git_index_snapshot: GitIndexSnapshot,
    git_index_snapshot_root: Option<PathBuf>,
    git_multi_index_snapshots: Vec<(PathBuf, GitIndexSnapshot)>,
    git_index_loading: bool,
    git_index_loading_root: Option<PathBuf>,
    git_index_requested_for_root: bool,
    git_runtime: Option<GitRuntimeHandle>,
    git_index_runtime: Option<GitIndexRuntimeHandle>,
    git_view_diff_runtime: Option<GitViewDiffRuntimeHandle>,
    commit_log_runtime: Option<CommitLogRuntimeHandle>,
    file_index_runtime: Option<FileIndexRuntimeHandle>,
    update_check_runtime: Option<UpdateCheckRuntimeHandle>,
    command_history: Rc<CommandHistory>,
    recent_projects: RecentProjectsStore,
    plugin_host: PluginHost,
    pending_count: Option<usize>,
    pending_edit_jump_locations: Vec<JumpLocation>,
    suspend_jump_recording: bool,
    in_editor_diff_buffers: HashMap<usize, InEditorDiffBufferState>,
    git_commit_buffers: HashMap<usize, GitCommitBufferState>,
    home_screen_active: bool,
    home_screen_update_notice: Option<String>,
    home_screen_update_check_requested: bool,
    last_term_cols: usize,
    last_term_rows: usize,
}

impl App {
    pub fn new(editor: Editor, config: Config, start_path: Option<&Path>) -> Self {
        let project_root = crate::project::find_project_root(start_path);
        let file_index_runtime = Self::build_file_index_runtime().ok();
        let git_index_runtime = Self::build_git_index_runtime().ok();
        let (file_list, file_index_loading, file_index_requested_for_root) =
            match config.performance.file_index.mode {
                FileIndexMode::Eager => (crate::project::collect_files(&project_root), false, true),
                FileIndexMode::Lazy => (Vec::new(), false, false),
            };
        let home_screen_active = should_show_home_screen(start_path, &editor);
        let fresh_update_cache =
            load_fresh_update_check_cache(&app_data_dir(), unix_timestamp_secs());
        let home_screen_update_notice = fresh_update_cache
            .as_ref()
            .and_then(home_screen_notice_from_cache);

        debug_log!(
            &config,
            "startup: debug={}, log_path={:?}",
            config.debug,
            config.debug_log_path
        );

        let git_status_cache = HashMap::new();
        let discovered_repos = crate::project::discover_sub_repos(&project_root);
        let git_runtime = Self::build_git_runtime(&config).ok();
        let git_view_diff_runtime = Self::build_git_view_diff_runtime().ok();
        let commit_log_runtime = Self::build_commit_log_runtime().ok();

        let command_history = Rc::new(CommandHistory::new(&project_root));
        let recent_projects = RecentProjectsStore::new();

        let mut app = Self {
            editor,
            compositor: Compositor::new(),
            registry: CommandRegistry::new(),
            theme: Theme::from_config(&config.theme),
            config,
            project_root,
            file_list,
            file_index_loading,
            file_index_requested_for_root,
            key_state: KeyState::Normal,
            last_explorer_dir: None,
            last_explorer_selected: None,
            close_confirm: false,
            git_status_cache,
            discovered_repos,
            git_index_snapshot: GitIndexSnapshot::default(),
            git_multi_index_snapshots: Vec::new(),
            git_index_snapshot_root: None,
            git_index_loading: false,
            git_index_loading_root: None,
            git_index_requested_for_root: false,
            git_runtime,
            git_index_runtime,
            git_view_diff_runtime,
            commit_log_runtime,
            file_index_runtime,
            update_check_runtime: None,
            command_history,
            recent_projects,
            plugin_host: PluginHost::new(Vec::new()),
            pending_count: None,
            pending_edit_jump_locations: Vec::new(),
            suspend_jump_recording: false,
            in_editor_diff_buffers: HashMap::new(),
            git_commit_buffers: HashMap::new(),
            home_screen_active,
            home_screen_update_notice,
            home_screen_update_check_requested: !home_screen_active || fresh_update_cache.is_some(),
            last_term_cols: 120,
            last_term_rows: 40,
        };
        app.start_lazy_file_index_prefetch_if_possible();
        app.start_git_index_prefetch_if_possible();
        app.rebuild_registry_and_plugin_host();
        app.emit_plugin_event(PluginEvent::BufferActivated {
            doc_id: app.editor.active_buffer().id,
        });
        app.queue_git_status_refresh(true);
        app.queue_active_doc_git_refresh(true);
        app
    }

    fn count_behavior(action: &CoreAction) -> Option<CountBehavior> {
        match action {
            CoreAction::MoveRight
            | CoreAction::MoveLeft
            | CoreAction::MoveDown
            | CoreAction::MoveUp
            | CoreAction::MoveWordForward
            | CoreAction::MoveWordForwardEnd
            | CoreAction::MoveWordBackward
            | CoreAction::MoveLongWordForward
            | CoreAction::MoveLongWordForwardEnd
            | CoreAction::MoveLongWordBackward
            | CoreAction::ExtendLineSelection
            | CoreAction::SearchNext
            | CoreAction::SearchPrev => Some(CountBehavior::Repeat),
            CoreAction::SelectLine => Some(CountBehavior::LineSelect),
            CoreAction::MoveToFileEnd => Some(CountBehavior::AbsoluteLineJump),
            _ => None,
        }
    }

    fn build_git_runtime(config: &Config) -> Result<GitRuntimeHandle, String> {
        GitRuntimeHandle::new(GitRuntimeDebounceConfig {
            gutter_high_priority_ms: config.git.gutter_debounce_high_priority_ms,
            gutter_normal_ms: config.git.gutter_debounce_normal_ms,
        })
    }

    fn build_git_view_diff_runtime() -> Result<GitViewDiffRuntimeHandle, String> {
        GitViewDiffRuntimeHandle::new()
    }

    fn build_commit_log_runtime() -> Result<CommitLogRuntimeHandle, String> {
        CommitLogRuntimeHandle::new()
    }

    fn build_git_index_runtime() -> Result<GitIndexRuntimeHandle, String> {
        GitIndexRuntimeHandle::new()
    }

    fn build_file_index_runtime() -> Result<FileIndexRuntimeHandle, String> {
        FileIndexRuntimeHandle::new()
    }

    fn build_update_check_runtime() -> Result<UpdateCheckRuntimeHandle, String> {
        UpdateCheckRuntimeHandle::new()
    }

    #[cfg(test)]
    fn is_home_screen_active(&self) -> bool {
        self.home_screen_active
    }

    fn materialize_scratch_from_home_if_needed(&mut self) {
        if self.home_screen_active {
            self.home_screen_active = false;
        }
    }

    /// Returns the git repo root for the active buffer, falling back to
    /// `self.project_root` when the buffer has no file path or isn't in a repo.
    fn active_buffer_repo_root(&self) -> PathBuf {
        self.editor
            .active_buffer()
            .file_path
            .as_ref()
            .and_then(|p| crate::command::git::repo_root_for_path(p).ok())
            .unwrap_or_else(|| self.project_root.clone())
    }

    fn active_buffer_should_refresh_project_scoped_state(&self) -> bool {
        let Some(path) = self.editor.active_buffer().file_path.as_deref() else {
            return true;
        };
        path_is_within_project_root(&self.project_root, path)
    }

    fn is_multi_repo(&self) -> bool {
        self.discovered_repos.len() > 1
    }

    fn queue_git_status_refresh(&self, high_priority: bool) {
        let Some(runtime) = &self.git_runtime else {
            return;
        };
        if self.is_multi_repo() {
            let _ = runtime
                .command_tx
                .send(GitRuntimeCommand::RefreshMultiStatus {
                    repos: self.discovered_repos.clone(),
                    high_priority,
                });
        } else {
            let _ = runtime.command_tx.send(GitRuntimeCommand::RefreshStatus {
                project_root: self.active_buffer_repo_root(),
                high_priority,
            });
        }
    }

    fn queue_active_doc_git_refresh(&self, high_priority: bool) {
        let Some(runtime) = &self.git_runtime else {
            return;
        };
        let doc = self.editor.active_buffer();
        let Some(path) = doc.file_path.clone() else {
            let _ = runtime
                .command_tx
                .send(GitRuntimeCommand::ClearDocument { doc_id: doc.id });
            return;
        };
        let _ = runtime.command_tx.send(GitRuntimeCommand::UpdateDocument {
            doc_id: doc.id,
            path,
            content: doc.rope.to_string(),
            high_priority,
        });
    }

    fn git_view_index_snapshot(&self) -> GitViewIndexSnapshot {
        GitViewIndexSnapshot {
            branch: self.git_index_snapshot.branch.clone(),
            changed: self.git_index_snapshot.changed.clone(),
            staged: self.git_index_snapshot.staged.clone(),
        }
    }

    fn git_index_matches_root(&self, repo_root: &Path) -> bool {
        self.git_index_snapshot_root.as_deref() == Some(repo_root)
    }

    fn git_index_loading_for_root(&self, repo_root: &Path) -> bool {
        self.git_index_loading_root.as_deref() == Some(repo_root)
    }

    fn git_view_index_snapshot_for_root(&self, repo_root: &Path) -> Option<GitViewIndexSnapshot> {
        self.git_index_matches_root(repo_root)
            .then(|| self.git_view_index_snapshot())
    }

    fn git_branch_picker_entries(&self) -> Vec<GitBranchPickerEntry> {
        self.git_index_snapshot
            .branches
            .iter()
            .map(|entry| GitBranchPickerEntry {
                branch_name: entry.name.clone(),
                label: if entry.is_current {
                    format!("* {}", entry.name)
                } else {
                    format!("  {}", entry.name)
                },
                preview_lines: entry.preview_lines.clone(),
            })
            .collect()
    }

    fn git_branch_picker_entries_for_root(&self, repo_root: &Path) -> Vec<GitBranchPickerEntry> {
        if self.git_index_matches_root(repo_root) {
            self.git_branch_picker_entries()
        } else {
            Vec::new()
        }
    }

    fn queue_git_index_refresh(&mut self) {
        self.git_index_requested_for_root = true;

        if self.is_multi_repo() {
            let Some(runtime) = &self.git_index_runtime else {
                // Synchronous fallback for multi-repo
                self.git_multi_index_snapshots = self
                    .discovered_repos
                    .iter()
                    .map(|r| (r.clone(), collect_git_index_snapshot(r)))
                    .collect();
                self.git_index_loading = false;
                self.git_index_loading_root = None;
                self.refresh_git_index_consumers();
                return;
            };
            self.git_index_loading = true;
            self.git_index_loading_root = Some(self.project_root.clone());
            if runtime
                .command_tx
                .send(GitIndexRuntimeCommand::RefreshMulti {
                    repos: self.discovered_repos.clone(),
                })
                .is_err()
            {
                self.git_index_loading = false;
                self.git_index_loading_root = None;
                self.git_index_requested_for_root = false;
            }
            return;
        }

        let repo_root = self.active_buffer_repo_root();
        let Some(runtime) = &self.git_index_runtime else {
            self.git_index_snapshot = collect_git_index_snapshot(&repo_root);
            self.git_index_snapshot_root = Some(repo_root);
            self.git_index_loading = false;
            self.git_index_loading_root = None;
            self.refresh_git_index_consumers();
            return;
        };
        self.git_index_loading = true;
        self.git_index_loading_root = Some(repo_root.clone());
        if runtime
            .command_tx
            .send(GitIndexRuntimeCommand::Refresh {
                project_root: repo_root,
            })
            .is_err()
        {
            self.git_index_loading = false;
            self.git_index_loading_root = None;
            self.git_index_requested_for_root = false;
        }
    }

    fn start_git_index_prefetch_if_possible(&mut self) {
        if self.git_index_loading || self.git_index_requested_for_root {
            return;
        }

        let Some(command_tx) = self
            .git_index_runtime
            .as_ref()
            .map(|runtime| runtime.command_tx.clone())
        else {
            return;
        };

        self.git_index_requested_for_root = true;
        self.git_index_loading = true;
        self.git_index_loading_root = Some(self.project_root.clone());

        let cmd = if self.is_multi_repo() {
            GitIndexRuntimeCommand::RefreshMulti {
                repos: self.discovered_repos.clone(),
            }
        } else {
            GitIndexRuntimeCommand::Refresh {
                project_root: self.project_root.clone(),
            }
        };
        if command_tx.send(cmd).is_err() {
            self.git_index_loading = false;
            self.git_index_loading_root = None;
            self.git_index_requested_for_root = false;
        }
    }

    fn ensure_git_index_started_if_needed(&mut self) {
        if self.is_multi_repo() {
            // For multi-repo, check if we already have snapshots or are loading
            if !self.git_multi_index_snapshots.is_empty() || self.git_index_loading {
                return;
            }
            self.queue_git_index_refresh();
            return;
        }
        let repo_root = self.active_buffer_repo_root();
        if self.git_index_matches_root(&repo_root) || self.git_index_loading_for_root(&repo_root) {
            return;
        }
        self.queue_git_index_refresh();
    }

    fn queue_git_index_refresh_if_idle(&mut self) {
        if self.git_index_runtime.is_none() {
            return;
        }
        let repo_root = self.active_buffer_repo_root();
        if self.git_index_loading_for_root(&repo_root) {
            return;
        }
        self.queue_git_index_refresh();
    }

    fn refresh_git_index_consumers(&mut self) {
        // Prepare data before borrowing compositor mutably
        let snapshot = self.git_view_index_snapshot();
        let snapshot_root = self.git_index_snapshot_root.clone();
        let multi_snapshots: Vec<(PathBuf, GitViewIndexSnapshot)> = self
            .git_multi_index_snapshots
            .iter()
            .map(|(root, snap)| {
                (
                    root.clone(),
                    GitViewIndexSnapshot {
                        branch: snap.branch.clone(),
                        changed: snap.changed.clone(),
                        staged: snap.staged.clone(),
                    },
                )
            })
            .collect();

        if let Some(git_view) = self.compositor.git_view_mut() {
            if git_view.is_multi_repo() && !multi_snapshots.is_empty() {
                git_view.apply_multi_index_snapshots(multi_snapshots);
            } else if snapshot_root.as_deref() == Some(git_view.project_root()) {
                git_view.apply_index_snapshot(snapshot);
            }
        }
        let active_repo_root = self.active_buffer_repo_root();
        let branch_entries = self.git_branch_picker_entries_for_root(&active_repo_root);
        if let Some(palette) = self.compositor.palette_mut() {
            palette.set_git_branch_entries(branch_entries);
        }
    }

    fn poll_git_index_runtime(&mut self) {
        let mut drained_events = Vec::new();
        if let Some(runtime) = &self.git_index_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                drained_events.push(event);
            }
        }

        let mut updated = false;
        for event in drained_events {
            match event {
                GitIndexRuntimeEvent::Ready {
                    project_root,
                    snapshot,
                } => {
                    let branches_ready = snapshot.branches_ready;
                    self.git_index_snapshot = snapshot;
                    self.git_index_snapshot_root = Some(project_root.clone());
                    if branches_ready {
                        if self.git_index_loading_root.as_deref() == Some(project_root.as_path()) {
                            self.git_index_loading_root = None;
                        }
                    } else if self.git_index_loading_root.is_none() {
                        self.git_index_loading_root = Some(project_root.clone());
                    }
                    self.git_index_loading = self.git_index_loading_root.is_some();
                    self.git_index_requested_for_root = true;
                    updated = true;
                }
                GitIndexRuntimeEvent::MultiReady { snapshots } => {
                    self.git_multi_index_snapshots = snapshots;
                    self.git_index_loading = false;
                    self.git_index_loading_root = None;
                    self.git_index_requested_for_root = true;
                    updated = true;
                }
            }
        }

        if updated {
            self.refresh_git_index_consumers();
        }
    }

    fn refresh_git_index_for_current_root(&mut self) {
        self.git_index_snapshot = GitIndexSnapshot::default();
        self.git_index_snapshot_root = None;
        self.git_index_loading = false;
        self.git_index_loading_root = None;
        self.git_index_requested_for_root = false;
        self.refresh_git_index_consumers();
        self.start_git_index_prefetch_if_possible();
    }

    fn queue_file_index_refresh(&mut self) {
        self.file_index_requested_for_root = true;
        let Some(runtime) = &self.file_index_runtime else {
            self.file_list = crate::project::collect_files(&self.project_root);
            self.file_index_loading = false;
            return;
        };
        self.file_index_loading = true;
        if runtime
            .command_tx
            .send(FileIndexRuntimeCommand::Refresh {
                project_root: self.project_root.clone(),
            })
            .is_err()
        {
            self.file_index_loading = false;
            self.file_index_requested_for_root = false;
        }
    }

    fn start_lazy_file_index_prefetch_if_possible(&mut self) {
        if self.config.performance.file_index.mode != FileIndexMode::Lazy {
            return;
        }
        if self.file_index_loading || self.file_index_requested_for_root {
            return;
        }

        let Some(command_tx) = self
            .file_index_runtime
            .as_ref()
            .map(|runtime| runtime.command_tx.clone())
        else {
            return;
        };

        self.file_index_requested_for_root = true;
        self.file_index_loading = true;
        if command_tx
            .send(FileIndexRuntimeCommand::Refresh {
                project_root: self.project_root.clone(),
            })
            .is_err()
        {
            self.file_index_loading = false;
            self.file_index_requested_for_root = false;
        }
    }

    fn ensure_file_index_started_if_needed(&mut self) {
        if self.config.performance.file_index.mode != FileIndexMode::Lazy {
            return;
        }
        if self.file_index_loading || self.file_index_requested_for_root {
            return;
        }
        self.queue_file_index_refresh();
    }

    fn refresh_file_index_consumers(&mut self) {
        if let Some(palette) = self.compositor.palette_mut() {
            palette.set_file_entries(self.file_list.clone());
            palette.refresh_after_file_entries_update(
                &self.registry,
                &self.editor.language_registry,
                &self.config,
            );
        }
        self.refresh_markdown_link_hover();
    }

    fn poll_file_index_runtime(&mut self) {
        let mut drained_events = Vec::new();
        if let Some(runtime) = &self.file_index_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                drained_events.push(event);
            }
        }

        let mut updated = false;
        for event in drained_events {
            match event {
                FileIndexRuntimeEvent::Ready {
                    project_root,
                    files,
                } => {
                    if project_root != self.project_root {
                        continue;
                    }
                    self.file_list = files;
                    self.file_index_loading = false;
                    self.file_index_requested_for_root = true;
                    updated = true;
                }
            }
        }

        if updated {
            self.refresh_file_index_consumers();
        }
    }

    fn refresh_file_index_for_current_root(&mut self) {
        match self.config.performance.file_index.mode {
            FileIndexMode::Eager => {
                self.file_list = crate::project::collect_files(&self.project_root);
                self.file_index_loading = false;
                self.file_index_requested_for_root = true;
                self.refresh_file_index_consumers();
            }
            FileIndexMode::Lazy => {
                self.file_list.clear();
                self.file_index_loading = false;
                self.file_index_requested_for_root = false;
                self.refresh_file_index_consumers();
            }
        }
        self.start_lazy_file_index_prefetch_if_possible();
    }

    fn poll_git_runtime(&mut self) {
        let mut drained_events = Vec::new();
        if let Some(runtime) = &self.git_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                drained_events.push(event);
            }
        }

        let mut should_refresh_git_index = false;
        for event in drained_events {
            match event {
                GitRuntimeEvent::FileStatusMapUpdated(map) => {
                    self.git_status_cache = map;
                    if let Some(explorer) = self.compositor.explorer_mut() {
                        explorer.set_git_status_map(&self.git_status_cache);
                    }
                    if let Some(popup) = self.compositor.explorer_popup_mut() {
                        popup.set_git_status_map(&self.git_status_cache);
                    }
                    if let Some(palette) = self.compositor.palette_mut() {
                        palette.set_git_status_map(&self.git_status_cache);
                    }
                    should_refresh_git_index = true;
                }
                GitRuntimeEvent::MultiFileStatusMapUpdated(repo_maps) => {
                    self.git_status_cache.clear();
                    for (repo_root, map) in repo_maps {
                        let repo_name = repo_root
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        for (path, status) in map {
                            let key = format!("{}/{}", repo_name, path);
                            self.git_status_cache.insert(key, status);
                        }
                    }
                    if let Some(explorer) = self.compositor.explorer_mut() {
                        explorer.set_git_status_map(&self.git_status_cache);
                    }
                    if let Some(popup) = self.compositor.explorer_popup_mut() {
                        popup.set_git_status_map(&self.git_status_cache);
                    }
                    if let Some(palette) = self.compositor.palette_mut() {
                        palette.set_git_status_map(&self.git_status_cache);
                    }
                    should_refresh_git_index = true;
                }
                GitRuntimeEvent::DocumentGutterUpdated { doc_id, gutter } => {
                    self.editor.set_git_gutter_for_doc(doc_id, gutter);
                }
            }
        }

        if should_refresh_git_index {
            self.queue_git_index_refresh_if_idle();
        }
    }

    fn poll_git_view_diff_runtime(&mut self) {
        let mut drained_events = Vec::new();
        if let Some(runtime) = &self.git_view_diff_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                drained_events.push(event);
            }
        }

        if let Some(git_view) = self.compositor.git_view_mut() {
            for event in drained_events {
                git_view.on_diff_event(event);
            }
        }
    }

    fn poll_commit_log_runtime(&mut self) {
        let mut drained_events: Vec<CommitLogEvent> = Vec::new();
        if let Some(runtime) = &self.commit_log_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                drained_events.push(event);
            }
        }

        if let Some(commit_log) = self.compositor.commit_log_mut() {
            for event in drained_events {
                commit_log.apply_event(event);
            }
        }
    }

    fn start_home_screen_update_check_if_needed(&mut self) {
        if !self.home_screen_active || self.home_screen_update_check_requested {
            return;
        }

        self.home_screen_update_check_requested = true;
        match Self::build_update_check_runtime() {
            Ok(runtime) => {
                self.update_check_runtime = Some(runtime);
            }
            Err(err) => {
                debug_log!(
                    &self.config,
                    "home screen update check runtime failed to start: {}",
                    err
                );
            }
        }
    }

    fn poll_update_check_runtime(&mut self) {
        let mut latest_result = None;
        if let Some(runtime) = &self.update_check_runtime {
            while let Ok(event) = runtime.event_rx.try_recv() {
                match event {
                    UpdateCheckRuntimeEvent::Ready(result) => latest_result = Some(result),
                }
            }
        }

        let Some(result) = latest_result else {
            return;
        };
        self.update_check_runtime = None;

        match result {
            Ok(status) => {
                self.home_screen_update_notice = home_screen_update_notice_from_status(&status);
                let cache = UpdateCheckCache::from_status(&status, unix_timestamp_secs());
                if let Err(err) = write_update_check_cache(&app_data_dir(), &cache) {
                    debug_log!(
                        &self.config,
                        "failed to persist update check cache: {}",
                        err
                    );
                }
            }
            Err(err) => {
                debug_log!(&self.config, "home screen update check failed: {}", err);
            }
        }
    }

    fn rebuild_registry_and_plugin_host(&mut self) -> Option<String> {
        let mut registry = CommandRegistry::new();
        crate::command::registry::register_builtins(&mut registry);

        let mut plugin_error = None;
        self.plugin_host = match build_plugin_host(&self.config, &self.project_root) {
            Ok(host) => host,
            Err(e) => {
                debug_log!(&self.config, "plugin host init failed: {}", e);
                plugin_error = Some(e);
                PluginHost::new(Vec::new())
            }
        };

        registry.register_plugin_commands(self.plugin_host.command_specs());
        self.registry = registry;
        plugin_error
    }

    fn can_collect_count_prefix(&self) -> bool {
        matches!(self.editor.mode, mode::Mode::Normal | mode::Mode::Visual)
            && self.key_state == KeyState::Normal
    }

    fn collect_count_prefix(&mut self, key_event: crossterm::event::KeyEvent) -> bool {
        if !self.can_collect_count_prefix() || !key_event.modifiers.is_empty() {
            self.pending_count = None;
            return false;
        }

        match key_event.code {
            KeyCode::Char(c @ '1'..='9') => {
                let digit = (c as u8 - b'0') as usize;
                let next = self
                    .pending_count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit);
                self.pending_count = Some(next);
                true
            }
            KeyCode::Char('0') if self.pending_count.is_some() => {
                let next = self.pending_count.unwrap_or(0).saturating_mul(10);
                self.pending_count = Some(next);
                true
            }
            _ => false,
        }
    }

    fn command_display(&self) -> String {
        match self.pending_count {
            Some(count) => format!("{count}{}", self.key_state.display_prefix()),
            None => self.key_state.display_prefix().to_string(),
        }
    }

    fn dispatch_resolved_key_action(&mut self, action: Action) -> bool {
        let Some(count) = self.pending_count.take() else {
            return self.dispatch_action(action);
        };

        match action {
            Action::Core(core_action) => match Self::count_behavior(&core_action) {
                Some(CountBehavior::Repeat) => {
                    for _ in 0..count {
                        if self.dispatch_action(Action::Core(core_action.clone())) {
                            return true;
                        }
                    }
                    false
                }
                Some(CountBehavior::LineSelect) => {
                    if self.dispatch_action(Action::Core(CoreAction::SelectLine)) {
                        return true;
                    }
                    for _ in 1..count {
                        if self.dispatch_action(Action::Core(CoreAction::ExtendLineSelection)) {
                            return true;
                        }
                    }
                    false
                }
                Some(CountBehavior::AbsoluteLineJump) => self.dispatch_action(Action::Core(
                    CoreAction::MoveToLineNumber(count.saturating_sub(1)),
                )),
                None => self.dispatch_action(Action::Core(core_action)),
            },
            _ => self.dispatch_action(action),
        }
    }

    fn resolve_contextual_key_action(
        &mut self,
        key_event: crossterm::event::KeyEvent,
    ) -> Option<Action> {
        if self.editor.mode != mode::Mode::Normal
            || self.key_state != KeyState::Normal
            || self.pending_count.is_some()
        {
            return None;
        }
        if !key_event.modifiers.is_empty() {
            return None;
        }

        match key_event.code {
            KeyCode::Char('r') if self.is_active_in_editor_diff_buffer() => Some(Action::App(
                AppAction::Workspace(WorkspaceAction::RefreshInEditorDiffView),
            )),
            _ => None,
        }
    }

    fn layout_dims(&self) -> (usize, usize) {
        (self.last_term_cols, self.last_term_rows)
    }

    fn sync_focused_window_to_active_buffer(&mut self) {
        let active_id = self.editor.active_buffer().id;
        self.compositor.set_focused_buffer(active_id);
    }

    fn sync_active_buffer_to_focused_window(&mut self) {
        let focused_id = self.compositor.focused_buffer_id();
        if focused_id != 0 {
            let _ = self.editor.switch_to_buffer(focused_id);
        }
    }

    fn close_active_buffer_with_reconciliation(
        &mut self,
        force: bool,
    ) -> Result<ClosedBufferInfo, String> {
        let closing_doc_id = self.editor.active_buffer().id;
        let closing_path = self.editor.active_buffer().file_path.clone();
        let commit_message = self.finalize_git_commit_buffer_on_close(closing_doc_id);
        let force_close = force || commit_message.is_some();
        if !force_close {
            self.editor.close_active_buffer()?;
        } else {
            self.editor.force_close_active_buffer();
        }
        if let Some(message) = commit_message {
            self.editor.message = Some(message);
        }
        let replacement_id = self.editor.active_buffer().id;
        self.compositor
            .replace_window_buffer_refs(closing_doc_id, replacement_id);
        self.compositor.set_focused_buffer(replacement_id);
        Ok(ClosedBufferInfo {
            doc_id: closing_doc_id,
            path: closing_path,
        })
    }

    fn prune_in_editor_diff_buffers(&mut self) {
        self.in_editor_diff_buffers
            .retain(|buffer_id, _| self.editor.buffer_by_id(*buffer_id).is_some());
    }

    fn prune_git_commit_buffers(&mut self) {
        self.git_commit_buffers
            .retain(|buffer_id, _| self.editor.buffer_by_id(*buffer_id).is_some());
    }

    fn finalize_git_commit_buffer_on_close(&mut self, doc_id: usize) -> Option<String> {
        let state = self.git_commit_buffers.remove(&doc_id)?;
        let raw_message = self
            .editor
            .buffer_by_id(doc_id)
            .map(|doc| doc.rope.to_string())
            .unwrap_or_default();

        let cleaned = match crate::command::git::git_strip_commit_message_in(
            &state.project_root,
            &raw_message,
        ) {
            Ok(message) => message,
            Err(err) => {
                let _ = std::fs::remove_file(&state.commit_editmsg_path);
                return Some(format!("Commit aborted: {}", err));
            }
        };

        if cleaned.trim().is_empty() {
            let _ = std::fs::remove_file(&state.commit_editmsg_path);
            return Some("Commit aborted: empty commit message".to_string());
        }

        let payload = format!("{}\n", cleaned);
        if let Err(err) = std::fs::write(&state.commit_editmsg_path, payload) {
            let _ = std::fs::remove_file(&state.commit_editmsg_path);
            return Some(format!("Commit failed: {}", err));
        }

        let result = crate::command::git::git_commit_with_message_file_in(
            &state.project_root,
            &state.commit_editmsg_path,
        );
        let _ = std::fs::remove_file(&state.commit_editmsg_path);
        match result {
            Ok(summary) => Some(summary),
            Err(err) => Some(format!("Commit failed: {}", err)),
        }
    }

    fn resolve_close_confirmation(&mut self, key_event: crossterm::event::KeyEvent) {
        self.close_confirm = false;
        let is_ctrl_c = key_event.modifiers.contains(KeyModifiers::CONTROL)
            && key_event.code == KeyCode::Char('c');
        if is_ctrl_c {
            if let Ok(closed) = self.close_active_buffer_with_reconciliation(true) {
                self.emit_plugin_event(PluginEvent::BufferClosed {
                    doc_id: closed.doc_id,
                    path: closed.path,
                });
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
                self.prune_in_editor_diff_buffers();
                self.prune_git_commit_buffers();
            }
            self.editor.message = None;
        } else {
            self.editor.message = Some(CLOSE_ABORTED_MESSAGE.to_string());
        }
    }

    fn queue_insert_edit_jump_line(&mut self) {
        if self.editor.mode != mode::Mode::Insert || self.suspend_jump_recording {
            return;
        }
        let location = self.editor.current_jump_location();
        let exists = self
            .pending_edit_jump_locations
            .iter()
            .any(|loc| loc.doc_id == location.doc_id && loc.line == location.line);
        if !exists {
            self.pending_edit_jump_locations.push(location);
        }
    }

    fn flush_pending_edit_jump_lines(&mut self) {
        if self.suspend_jump_recording || self.pending_edit_jump_locations.is_empty() {
            self.pending_edit_jump_locations.clear();
            return;
        }
        for location in self.pending_edit_jump_locations.drain(..) {
            self.editor.push_jump_location(location);
        }
    }

    fn flush_insert_transaction_if_active(&mut self) {
        if self.editor.mode == mode::Mode::Insert {
            self.editor.active_buffer_mut().flush_transaction();
            self.flush_pending_edit_jump_lines();
        }
    }

    fn record_jump_transition_if_needed(&mut self, before: JumpLocation, after: JumpLocation) {
        if self.suspend_jump_recording {
            return;
        }
        self.editor.record_jump_transition(before, after);
    }

    fn jump_location_label_base(&self, location: &JumpLocation) -> String {
        match &location.file_path {
            Some(path) => {
                let rel = path
                    .strip_prefix(&self.project_root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                format!("{}:{}:{}", rel, location.line + 1, location.char_col + 1)
            }
            None => format!(
                "[scratch#{}]:{}:{}",
                location.doc_id,
                location.line + 1,
                location.char_col + 1
            ),
        }
    }

    fn is_jump_word_char(ch: char) -> bool {
        ch.is_ascii_alphanumeric() || ch == '_'
    }

    fn jump_word_under_char(line: &str, char_col: usize) -> Option<String> {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            return None;
        }
        let idx = char_col.min(chars.len().saturating_sub(1));
        if !Self::is_jump_word_char(chars[idx]) {
            return None;
        }
        let mut start = idx;
        while start > 0 && Self::is_jump_word_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = idx + 1;
        while end < chars.len() && Self::is_jump_word_char(chars[end]) {
            end += 1;
        }
        Some(chars[start..end].iter().collect())
    }

    fn jump_line_text_from_preview_line(preview_line: &str) -> Option<&str> {
        let (_, right) = preview_line.split_once('|')?;
        Some(right.strip_prefix(' ').unwrap_or(right))
    }

    fn jump_label_for_location(
        &self,
        location: &JumpLocation,
        target_line_text: Option<&str>,
    ) -> String {
        let base = self.jump_location_label_base(location);
        let Some(line) = target_line_text else {
            return base;
        };
        let Some(word) = Self::jump_word_under_char(line, location.char_col) else {
            return base;
        };
        format!("{}  {}", base, word)
    }

    fn jump_preview_lines_for_location(
        &self,
        location: &JumpLocation,
    ) -> (Vec<String>, Option<usize>) {
        let mut lines = Vec::new();
        lines.push(self.jump_location_label_base(location));

        if let Some(doc) = self
            .editor
            .buffers()
            .iter()
            .find(|d| d.id == location.doc_id)
        {
            let total = doc.rope.len_lines();
            if total == 0 {
                return (lines, None);
            }
            let target_line = location.line.min(total.saturating_sub(1));
            let start = target_line.saturating_sub(3);
            let end = (target_line + 4).min(total);
            let mut target_preview_line = None;
            for line_idx in start..end {
                let text = doc
                    .rope
                    .line(line_idx)
                    .to_string()
                    .trim_end_matches('\n')
                    .to_string();
                if line_idx == target_line {
                    target_preview_line = Some(lines.len());
                }
                lines.push(format!("{:>5} | {}", line_idx + 1, text));
            }
            return (lines, target_preview_line);
        }

        if let Some(path) = &location.file_path
            && let Ok(content) = std::fs::read_to_string(path)
        {
            let file_lines: Vec<&str> = content.lines().collect();
            if !file_lines.is_empty() {
                let target_line = location.line.min(file_lines.len().saturating_sub(1));
                let start = target_line.saturating_sub(3);
                let end = (target_line + 4).min(file_lines.len());
                let mut target_preview_line = None;
                for (line_idx, text) in file_lines.iter().enumerate().take(end).skip(start) {
                    if line_idx == target_line {
                        target_preview_line = Some(lines.len());
                    }
                    lines.push(format!("{:>5} | {}", line_idx + 1, text));
                }
                return (lines, target_preview_line);
            }
        }
        (lines, None)
    }

    fn lsp_location_label_base(&self, path: &Path, line: usize, char_col: usize) -> String {
        let rel = path
            .strip_prefix(&self.project_root)
            .unwrap_or(path)
            .display()
            .to_string();
        format!("{}:{}:{}", rel, line + 1, char_col + 1)
    }

    fn char_col_from_utf16(line: &str, character_utf16: usize) -> usize {
        let mut utf16_col = 0usize;
        for (char_col, ch) in line.chars().enumerate() {
            if utf16_col >= character_utf16 {
                return char_col;
            }
            let next = utf16_col + ch.len_utf16();
            if next > character_utf16 {
                return char_col;
            }
            utf16_col = next;
        }
        line.chars().count()
    }

    fn reference_preview_lines_for_location(
        &self,
        path: &Path,
        line: usize,
        character_utf16: usize,
    ) -> (Vec<String>, Option<usize>, usize) {
        let mut target_char_col = character_utf16;
        let mut lines = Vec::new();

        if let Ok(content) = std::fs::read_to_string(path) {
            let file_lines: Vec<&str> = content.lines().collect();
            if !file_lines.is_empty() {
                let target_line = line.min(file_lines.len().saturating_sub(1));
                target_char_col =
                    Self::char_col_from_utf16(file_lines[target_line], character_utf16);
                lines.push(self.lsp_location_label_base(path, line, target_char_col));

                let start = target_line.saturating_sub(3);
                let end = (target_line + 4).min(file_lines.len());
                let mut target_preview_line = None;
                for (line_idx, text) in file_lines.iter().enumerate().take(end).skip(start) {
                    if line_idx == target_line {
                        target_preview_line = Some(lines.len());
                    }
                    lines.push(format!("{:>5} | {}", line_idx + 1, text));
                }
                return (lines, target_preview_line, target_char_col);
            }
        }

        lines.push(self.lsp_location_label_base(path, line, target_char_col));
        (lines, None, target_char_col)
    }

    fn reference_label_for_location(
        &self,
        path: &Path,
        line: usize,
        char_col: usize,
        target_line_text: Option<&str>,
    ) -> String {
        let base = self.lsp_location_label_base(path, line, char_col);
        let Some(line_text) = target_line_text else {
            return base;
        };
        let Some(word) = Self::jump_word_under_char(line_text, char_col) else {
            return base;
        };
        format!("{}  {}", base, word)
    }

    fn open_lsp_references_picker(
        &mut self,
        caller_label: String,
        locations: Vec<LspPickerLocation>,
    ) {
        let entries: Vec<ReferencePickerEntry> = locations
            .into_iter()
            .map(|location| {
                let (preview_lines, target_preview_line, target_char_col) = self
                    .reference_preview_lines_for_location(
                        &location.path,
                        location.line,
                        location.character_utf16,
                    );
                let target_line_text = target_preview_line
                    .and_then(|line_idx| preview_lines.get(line_idx))
                    .and_then(|line| Self::jump_line_text_from_preview_line(line));
                let label = self.reference_label_for_location(
                    &location.path,
                    location.line,
                    target_char_col,
                    target_line_text,
                );

                ReferencePickerEntry {
                    label,
                    path: location.path.clone(),
                    line: location.line,
                    character_utf16: location.character_utf16,
                    preview_lines,
                    source_path: Some(location.path.to_string_lossy().to_string()),
                    target_preview_line,
                    target_char_col,
                }
            })
            .collect();

        if entries.is_empty() {
            self.editor.message = Some("No references found".to_string());
            return;
        }

        let palette = Palette::new_reference_picker(caller_label, entries);
        self.compositor.push_palette(palette);
    }

    fn refresh_markdown_link_hover(&mut self) {
        if self.editor.mode != mode::Mode::Insert || !self.compositor.can_show_markdown_link_hover()
        {
            self.compositor.close_markdown_link_hover();
            return;
        }

        let (doc_path, typed_fragment) = {
            let active_doc = self.editor.active_buffer();
            let Some(doc_path) = active_doc.file_path.as_ref() else {
                self.compositor.close_markdown_link_hover();
                return;
            };
            if !is_markdown_file(&self.editor, doc_path) {
                self.compositor.close_markdown_link_hover();
                return;
            }

            let Some(edit_ctx) = link_edit_context_at_cursor(active_doc) else {
                self.compositor.close_markdown_link_hover();
                return;
            };
            (doc_path.clone(), edit_ctx.typed_fragment)
        };

        self.ensure_file_index_started_if_needed();
        let candidates = ranked_markdown_link_candidates(
            &self.file_list,
            &self.project_root,
            &doc_path,
            &typed_fragment,
            50,
        );
        if candidates.is_empty() {
            self.compositor.close_markdown_link_hover();
        } else {
            self.compositor
                .set_markdown_link_hover_candidates(candidates);
        }
    }

    fn apply_markdown_link_completion(&mut self, candidate: &str) {
        let Some(edit_ctx) = link_edit_context_at_cursor(self.editor.active_buffer()) else {
            self.compositor.close_markdown_link_hover();
            return;
        };

        let replace_start = edit_ctx.path_start_char;
        let replace_end = edit_ctx.cursor_char;
        if replace_start > replace_end {
            self.compositor.close_markdown_link_hover();
            return;
        }

        let doc = self.editor.active_buffer_mut();
        doc.delete_range(replace_start, replace_end);
        doc.insert_text_at(replace_start, candidate);
        doc.cursors[0] = replace_start + candidate.chars().count();
        doc.clear_anchor();

        self.editor.mark_highlights_dirty();
        self.emit_plugin_event(PluginEvent::BufferChanged {
            doc_id: self.editor.active_buffer().id,
        });
        self.queue_active_doc_git_refresh(false);
    }

    fn open_jump_list_picker(&mut self) {
        let jump_entries = self.editor.jump_list_entries();
        let entries: Vec<JumpPickerEntry> = jump_entries
            .iter()
            .enumerate()
            .rev()
            .map(|(idx, location)| {
                let (preview_lines, target_preview_line) =
                    self.jump_preview_lines_for_location(location);
                let target_line_text = target_preview_line
                    .and_then(|line_idx| preview_lines.get(line_idx))
                    .and_then(|line| Self::jump_line_text_from_preview_line(line));
                let label = self.jump_label_for_location(location, target_line_text);
                JumpPickerEntry {
                    jump_index: idx,
                    label,
                    preview_lines,
                    source_path: location
                        .file_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    target_preview_line,
                    target_char_col: location.char_col,
                }
            })
            .collect();
        let palette = Palette::new_jump_picker(entries);
        self.compositor.push_palette(palette);
    }

    fn symbol_preview_lines_for_line(&self, line: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let doc = self.editor.active_buffer();
        let total = doc.rope.len_lines();
        if total == 0 {
            return lines;
        }

        let line = line.min(total.saturating_sub(1));
        let start = line.saturating_sub(3);
        let end = (line + 4).min(total);
        for line_idx in start..end {
            let text = doc
                .rope
                .line(line_idx)
                .to_string()
                .trim_end_matches('\n')
                .to_string();
            lines.push(format!("{:>5} | {}", line_idx + 1, text));
        }
        lines
    }

    fn extract_symbol_entries(&self) -> Vec<(String, usize, usize, Vec<String>)> {
        let buf = self.editor.active_buffer();
        let Some(path) = &buf.file_path else {
            return Vec::new();
        };
        let path_str = path.to_string_lossy();
        let Some(lang_def) = self.editor.language_registry.detect_by_extension(&path_str) else {
            return Vec::new();
        };
        if lang_def.tags_query.is_none() {
            return Vec::new();
        }
        let text = buf.rope.to_string();
        let symbols = extract_symbols(&text, lang_def);
        symbols
            .iter()
            .map(|symbol| {
                (
                    format!(
                        "{} [{}]  {}:{}",
                        symbol.name,
                        symbol.kind,
                        symbol.line + 1,
                        symbol.char_col + 1
                    ),
                    symbol.line,
                    symbol.char_col,
                    self.symbol_preview_lines_for_line(symbol.line),
                )
            })
            .collect()
    }

    fn normalize_smart_copy_text(raw: &str) -> String {
        let mut lines: Vec<String> = raw.lines().map(|line| line.to_string()).collect();
        if lines.is_empty() {
            return String::new();
        }

        lines[0] = lines[0].trim_start_matches([' ', '\t']).to_string();

        let min_indent = lines
            .iter()
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            .min()
            .unwrap_or(0);

        for line in lines.iter_mut().skip(1) {
            if line.trim().is_empty() {
                continue;
            }
            *line = line.chars().skip(min_indent).collect();
        }

        lines.join("\n")
    }

    fn extract_smart_copy_entries(&self) -> Vec<SmartCopyPickerEntry> {
        let buf = self.editor.active_buffer();
        let Some(path) = &buf.file_path else {
            return Vec::new();
        };
        let path_str = path.to_string_lossy();
        let Some(lang_def) = self.editor.language_registry.detect_by_extension(&path_str) else {
            return Vec::new();
        };
        if lang_def.tags_query.is_none() {
            return Vec::new();
        }

        let text = buf.rope.to_string();
        extract_definition_sections(&text, lang_def)
            .into_iter()
            .filter(|section| {
                matches!(
                    section.kind.as_str(),
                    "class" | "function" | "method" | "code_block"
                )
            })
            .filter_map(|section| {
                let raw_copy_text = text.get(section.start_byte..section.end_byte)?;
                let copy_text = Self::normalize_smart_copy_text(raw_copy_text);
                let preview_lines: Vec<String> = copy_text
                    .lines()
                    .take(200)
                    .enumerate()
                    .map(|(idx, line_text)| {
                        format!("{:>5} | {}", section.start_line + idx + 1, line_text)
                    })
                    .collect();

                Some(SmartCopyPickerEntry {
                    label: format!(
                        "{} [{}]  {}:{}",
                        section.name,
                        section.kind,
                        section.line + 1,
                        section.char_col + 1
                    ),
                    line: section.line,
                    char_col: section.char_col,
                    preview_lines,
                    copy_text,
                })
            })
            .collect()
    }

    fn extract_active_doc_lines(&self) -> Vec<String> {
        let doc = self.editor.active_buffer();
        (0..doc.rope.len_lines().min(200))
            .map(|i| {
                doc.rope
                    .line(i)
                    .to_string()
                    .trim_end_matches('\n')
                    .to_string()
            })
            .collect()
    }

    pub fn run(
        &mut self,
        stdout: &mut impl std::io::Write,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.start_home_screen_update_check_if_needed();
        loop {
            let frame_start = Instant::now();
            let frame_deadline = frame_start + FRAME_DURATION_60_FPS;

            // Flush any deferred highlight updates before rendering
            self.editor.update_highlights_if_dirty();

            // Update find/replace popup preview if active
            if let Some(popup) = self.compositor.find_replace_popup_mut() {
                let document_rope = &self.editor.active_buffer().rope;
                popup.update_preview(document_rope);
            }

            // Render
            let (term_cols, term_rows) = terminal::size()?;
            let cols = term_cols as usize;
            let rows = term_rows as usize;
            self.last_term_cols = cols;
            self.last_term_rows = rows;
            let default_view_height = if rows > 2 { rows - 2 } else { 1 };
            let default_editor_width = self
                .compositor
                .explorer_layout(cols)
                .map(|(_, _, _, editor_w)| editor_w)
                .unwrap_or(cols);
            let focused_pane = self.compositor.focused_pane_rect(cols, rows);
            let view_height = focused_pane
                .map(|rect| rect.height)
                .unwrap_or(default_view_height);
            let editor_area_width = focused_pane
                .map(|rect| rect.width)
                .unwrap_or(default_editor_width);
            let total_lines = self.editor.active_buffer().rope.len_lines();
            let gutter_w = reserved_left_gutter_width(
                total_lines,
                self.config.show_line_number,
                self.config.line_number_width,
            );
            let text_width = editor_area_width.saturating_sub(gutter_w);
            self.editor
                .active_buffer_mut()
                .ensure_cursor_visible_with_horizontal(
                    view_height.max(1),
                    text_width,
                    self.config.horizontal_scroll_margin,
                );

            let command_display = self.command_display();
            let mut ctx = RenderContext::new_with_chord_display(
                cols,
                rows,
                &self.editor,
                &self.theme,
                &command_display,
                &self.config,
                &self.project_root,
                self.close_confirm,
                self.home_screen_active,
            );
            ctx.home_screen_notice = self.home_screen_update_notice.as_deref();
            if let Some((_ew, _border, editor_x, editor_w)) = self.compositor.explorer_layout(cols)
            {
                ctx.editor_area_x = editor_x;
                ctx.editor_area_width = editor_w;
            }
            self.compositor.render(&ctx, stdout)?;

            // Process events within frame budget
            loop {
                let Some(event) = poll_event_until(frame_deadline)? else {
                    break;
                };
                match event {
                    Event::Key(key_event) => {
                        debug_log!(
                            &self.config,
                            "KeyEvent: code={:?}, modifiers={:?}, kind={:?}",
                            key_event.code,
                            key_event.modifiers,
                            key_event.kind
                        );
                        // Intercept keys when close confirmation is active
                        if self.close_confirm {
                            self.resolve_close_confirmation(key_event);
                            continue;
                        }

                        let event_result = self.compositor.handle_key(
                            key_event,
                            &self.registry,
                            &self.editor.language_registry,
                            &self.config,
                            &self.key_state,
                        );

                        match event_result {
                            EventResult::Consumed => {}
                            EventResult::Action(action) => {
                                if self.dispatch_action(action) {
                                    return Ok(());
                                }
                            }
                            EventResult::Ignored => {
                                self.editor.message = None;
                                if self.collect_count_prefix(key_event) {
                                    continue;
                                }
                                if let Some(action) = self.resolve_contextual_key_action(key_event)
                                {
                                    if self.dispatch_action(action) {
                                        return Ok(());
                                    }
                                    continue;
                                }
                                let action = crate::input::keymap::resolve(
                                    key_event,
                                    &mut self.key_state,
                                    &self.editor.mode,
                                    self.editor.macro_recorder.is_recording(),
                                );
                                debug_log!(&self.config, "action: {:?}", action);
                                // Update command helper based on current key_state
                                self.compositor.update_command_helper(&self.key_state);
                                if self.dispatch_resolved_key_action(action) {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    Event::Resize(_, _) => break, // Resize → re-render immediately
                    Event::Paste(text) => {
                        debug_log!(&self.config, "Paste event: {:?}", text);
                        // Forward paste to palette if it's open
                        if let Some(palette) = self.compositor.palette_mut() {
                            debug_log!(&self.config, "Paste: forwarding to palette");
                            palette.insert_text(
                                &text,
                                &self.registry,
                                &self.editor.language_registry,
                                &self.config,
                            );
                        } else if let Some(search_bar) = self.compositor.search_bar_mut() {
                            // Forward paste to search bar (for IME input)
                            debug_log!(&self.config, "Paste: forwarding to search_bar");
                            search_bar.insert_text(&text);
                            // Trigger search update with the new input
                            let query = search_bar.input.text.clone();
                            self.dispatch_action(Action::Core(CoreAction::SearchUpdate(query)));
                        } else if self.editor.mode == mode::Mode::Insert {
                            let action = Action::Core(CoreAction::InsertText(text));
                            if self.dispatch_action(action) {
                                return Ok(());
                            }
                        }
                    }
                    Event::Mouse(mouse_event) => {
                        let event_result = self.compositor.handle_mouse(&mouse_event);
                        match event_result {
                            EventResult::Consumed => {}
                            EventResult::Action(action) => {
                                if self.dispatch_action(action) {
                                    return Ok(());
                                }
                            }
                            EventResult::Ignored => match mouse_event.kind {
                                MouseEventKind::ScrollUp => {
                                    self.editor
                                        .active_buffer_mut()
                                        .scroll_viewport(-3, view_height);
                                }
                                MouseEventKind::ScrollDown => {
                                    self.editor
                                        .active_buffer_mut()
                                        .scroll_viewport(3, view_height);
                                }
                                _ => {}
                            },
                        }
                    }
                    _ => {}
                }
            }

            self.emit_plugin_event(PluginEvent::Tick);
            self.poll_plugins();
            self.poll_git_runtime();
            self.poll_git_index_runtime();
            self.poll_git_view_diff_runtime();
            self.poll_commit_log_runtime();
            self.poll_file_index_runtime();
            self.poll_update_check_runtime();
        }
    }

    pub fn dispatch_action(&mut self, action: Action) -> bool {
        self.dispatch(action)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Dispatch an action. Returns true if the editor should quit.
    fn dispatch(&mut self, action: Action) -> bool {
        let should_quit = match action {
            Action::Core(action) => self.dispatch_core(action),
            Action::Ui(action) => {
                self.compositor.apply(action);
                false
            }
            Action::App(action) => self.dispatch_app(action),
            Action::Noop => false,
        };

        if self.home_screen_active && !self.editor.is_single_clean_scratch() {
            self.materialize_scratch_from_home_if_needed();
        }

        if !should_quit {
            self.sync_focused_window_to_active_buffer();
            self.refresh_markdown_link_hover();
            self.poll_git_index_runtime();
            self.poll_git_view_diff_runtime();
            self.poll_commit_log_runtime();
            self.poll_file_index_runtime();
            self.poll_update_check_runtime();
        }

        should_quit
    }

    fn reload_config_runtime(&mut self) -> Option<String> {
        let config = Config::load();
        self.apply_config_runtime(config)
    }

    fn create_default_config_at_path(&mut self, path: &Path) -> Result<String, String> {
        let merged = if path.exists() {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| format!("Config read failed ({}): {}", path.display(), e))?;
            toml::from_str::<Config>(&contents)
                .map_err(|e| format!("Config parse failed: {}", e))?
        } else {
            Config::default()
        };

        let contents = toml::to_string_pretty(&merged)
            .map_err(|e| format!("Config serialize failed: {}", e))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Config directory create failed ({}): {}",
                    parent.display(),
                    e
                )
            })?;
        }

        std::fs::write(path, contents)
            .map_err(|e| format!("Config write failed ({}): {}", path.display(), e))?;

        let plugin_error = self.apply_config_runtime(merged);
        let msg = match plugin_error {
            Some(e) => format!(
                "Config written: {} (plugin init failed: {})",
                path.display(),
                e
            ),
            None => format!("Config written: {}", path.display()),
        };
        Ok(msg)
    }

    fn apply_config_runtime(&mut self, config: Config) -> Option<String> {
        let theme = Theme::from_config(&config.theme);
        self.config = config;
        self.theme = theme;
        self.git_runtime = Self::build_git_runtime(&self.config).ok();
        self.git_index_runtime = Self::build_git_index_runtime().ok();
        self.git_view_diff_runtime = Self::build_git_view_diff_runtime().ok();
        self.commit_log_runtime = Self::build_commit_log_runtime().ok();
        self.file_index_runtime = Self::build_file_index_runtime().ok();
        self.refresh_file_index_for_current_root();
        self.refresh_git_index_for_current_root();
        let plugin_error = self.rebuild_registry_and_plugin_host();
        self.editor.mark_highlights_dirty();
        self.emit_plugin_event(PluginEvent::BufferActivated {
            doc_id: self.editor.active_buffer().id,
        });
        self.queue_git_status_refresh(true);
        self.queue_active_doc_git_refresh(true);
        plugin_error
    }

    fn record_recent_project_open_for_active_buffer(&self) {
        let active_file = self.editor.active_buffer().file_path.as_deref();
        let _ = self
            .recent_projects
            .record_project_open(&self.project_root, active_file);
    }

    fn record_recent_project_edit_for_active_buffer(&self) {
        let Some(active_file) = self.editor.active_buffer().file_path.as_deref() else {
            return;
        };
        let _ = self
            .recent_projects
            .record_file_edit(&self.project_root, active_file);
    }

    fn emit_plugin_event(&mut self, event: PluginEvent) {
        if matches!(event, PluginEvent::BufferActivated { .. }) {
            self.record_recent_project_open_for_active_buffer();
        }
        let ctx = PluginContext::new(&self.editor, &self.project_root, &self.config);
        let outputs = self.plugin_host.on_event(&event, &ctx);
        self.apply_plugin_outputs(outputs);
    }

    fn poll_plugins(&mut self) {
        let ctx = PluginContext::new(&self.editor, &self.project_root, &self.config);
        let outputs = self.plugin_host.poll(&ctx);
        self.apply_plugin_outputs(outputs);
    }

    fn apply_plugin_outputs(&mut self, outputs: Vec<PluginOutput>) {
        for output in outputs {
            match output {
                PluginOutput::Message(msg) => {
                    self.editor.message = Some(msg);
                }
                PluginOutput::OpenUrl(url) => {
                    self.open_url_in_browser(&url);
                }
                PluginOutput::OpenFileAtLsp {
                    path,
                    line,
                    character_utf16,
                } => {
                    self.open_file_at_lsp_location(&path, line, character_utf16);
                }
                PluginOutput::OpenLspReferencesPicker {
                    caller_label,
                    locations,
                } => {
                    self.open_lsp_references_picker(caller_label, locations);
                }
                PluginOutput::SetDiagnostics { path, diagnostics } => {
                    self.editor.set_lsp_diagnostics_for_path(&path, diagnostics);
                }
                PluginOutput::ClearDiagnostics { path } => {
                    self.editor.clear_lsp_diagnostics_for_path(&path);
                }
            }
        }

        if self.home_screen_active && !self.editor.is_single_clean_scratch() {
            self.materialize_scratch_from_home_if_needed();
        }
    }

    fn open_url_in_browser(&mut self, url: &str) {
        if let Err(err) = spawn_open_url(url) {
            self.editor.message = Some(format!("Failed to open URL: {err}"));
        }
    }

    fn is_active_in_editor_diff_buffer(&self) -> bool {
        self.in_editor_diff_buffers
            .contains_key(&self.editor.active_buffer().id)
    }

    fn is_active_git_commit_buffer(&self) -> bool {
        self.git_commit_buffers
            .contains_key(&self.editor.active_buffer().id)
    }

    fn apply_in_editor_diff_view_to_active_buffer(&mut self, view: InEditorDiffView) {
        let active_id = self.editor.active_buffer().id;
        {
            let doc = self.editor.active_buffer_mut();
            doc.rope = ropey::Rope::from_str(&view.text);
            doc.cursors = vec![0];
            doc.scroll_offset = 0;
            doc.horizontal_scroll_offset = 0;
            doc.selection = None;
            doc.dirty = false;
            doc.pending_edits.clear();
            doc.history = crate::core::history::History::new();
            doc.git_gutter.clear();
        }
        self.in_editor_diff_buffers.insert(
            active_id,
            InEditorDiffBufferState {
                line_targets: view.line_targets,
            },
        );
        self.editor.register_highlights_for_extension("diff");
        self.editor.mark_highlights_dirty();
    }

    fn open_in_editor_diff_view(&mut self) -> Result<(), String> {
        let view = build_in_editor_diff_view(&self.project_root)?;
        let previous_buffer_id = self.editor.active_buffer().id;
        if self.editor.active_buffer().file_path.is_some() {
            self.editor.new_buffer();
        }
        self.materialize_scratch_from_home_if_needed();
        self.apply_in_editor_diff_view_to_active_buffer(view);
        if self.editor.active_buffer().id != previous_buffer_id {
            self.emit_plugin_event(PluginEvent::BufferActivated {
                doc_id: self.editor.active_buffer().id,
            });
        } else {
            self.emit_plugin_event(PluginEvent::BufferChanged {
                doc_id: self.editor.active_buffer().id,
            });
        }
        Ok(())
    }

    fn refresh_active_in_editor_diff_view(&mut self) -> Result<bool, String> {
        if !self.is_active_in_editor_diff_buffer() {
            return Ok(false);
        }
        let view = build_in_editor_diff_view(&self.project_root)?;
        self.apply_in_editor_diff_view_to_active_buffer(view);
        self.emit_plugin_event(PluginEvent::BufferChanged {
            doc_id: self.editor.active_buffer().id,
        });
        Ok(true)
    }

    fn open_git_commit_message_buffer(&mut self) -> Result<(), String> {
        let repo_root = self.active_buffer_repo_root();
        if !crate::command::git::git_has_staged_changes_in(&repo_root)? {
            return Err("No staged changes to commit".to_string());
        }
        let commit_editmsg_path = crate::command::git::git_commit_editmsg_path_in(&repo_root)?;
        crate::command::git::git_prepare_commit_editmsg_template_in(
            &repo_root,
            &commit_editmsg_path,
        )?;
        let path_str = commit_editmsg_path.to_string_lossy().to_string();
        self.flush_insert_transaction_if_active();
        self.editor.open_file(&path_str);
        let _ = self.editor.active_buffer_mut().reload_from_disk();
        self.editor.active_buffer_mut().set_cursor_line_char(0, 0);
        let active_id = self.editor.active_buffer().id;
        self.git_commit_buffers.insert(
            active_id,
            GitCommitBufferState {
                project_root: repo_root,
                commit_editmsg_path,
            },
        );
        self.emit_plugin_event(PluginEvent::BufferActivated { doc_id: active_id });
        Ok(())
    }

    fn open_git_branch_picker(&mut self) -> Result<(), String> {
        self.ensure_git_index_started_if_needed();
        let repo_root = self.active_buffer_repo_root();
        let entries = self.git_branch_picker_entries_for_root(&repo_root);
        if entries.is_empty() {
            if self.git_index_loading_for_root(&repo_root)
                || (self.git_index_matches_root(&repo_root)
                    && !self.git_index_snapshot.branches_ready)
            {
                let palette = Palette::new_git_branch_picker(Vec::new());
                self.compositor.push_palette(palette);
                self.editor.message = Some("Indexing git branches...".to_string());
                return Ok(());
            }
            return Err("No local branches found".to_string());
        }
        let palette = Palette::new_git_branch_picker(entries);
        self.compositor.push_palette(palette);
        Ok(())
    }

    fn open_git_branch_compare_picker(&mut self) -> Result<(), String> {
        self.ensure_git_index_started_if_needed();
        let repo_root = self.active_buffer_repo_root();
        let entries = self.git_branch_picker_entries_for_root(&repo_root);
        if entries.is_empty() {
            if self.git_index_loading_for_root(&repo_root)
                || (self.git_index_matches_root(&repo_root)
                    && !self.git_index_snapshot.branches_ready)
            {
                let palette = Palette::new_git_branch_compare_picker(Vec::new());
                self.compositor.push_palette(palette);
                self.editor.message = Some("Indexing git branches...".to_string());
                return Ok(());
            }
            return Err("No local branches found".to_string());
        }
        let palette = Palette::new_git_branch_compare_picker(entries);
        self.compositor.push_palette(palette);
        Ok(())
    }

    fn open_branch_compare_view(&mut self, other_branch: &str) -> Result<(), String> {
        let repo_root = self.active_buffer_repo_root();
        let view = crate::command::in_editor_diff::build_branch_compare_diff_view(
            &repo_root,
            other_branch,
        )?;
        let previous_buffer_id = self.editor.active_buffer().id;
        if self.editor.active_buffer().file_path.is_some() {
            self.editor.new_buffer();
        }
        self.materialize_scratch_from_home_if_needed();
        self.apply_in_editor_diff_view_to_active_buffer(view);
        if self.editor.active_buffer().id != previous_buffer_id {
            self.emit_plugin_event(PluginEvent::BufferActivated {
                doc_id: self.editor.active_buffer().id,
            });
        } else {
            self.emit_plugin_event(PluginEvent::BufferChanged {
                doc_id: self.editor.active_buffer().id,
            });
        }
        Ok(())
    }

    fn open_commit_diff_view(&mut self, hash: &str) -> Result<(), String> {
        let view =
            crate::command::in_editor_diff::build_commit_diff_view(&self.project_root, hash)?;
        let previous_buffer_id = self.editor.active_buffer().id;
        if self.editor.active_buffer().file_path.is_some() {
            self.editor.new_buffer();
        }
        self.materialize_scratch_from_home_if_needed();
        self.apply_in_editor_diff_view_to_active_buffer(view);
        if self.editor.active_buffer().id != previous_buffer_id {
            self.emit_plugin_event(PluginEvent::BufferActivated {
                doc_id: self.editor.active_buffer().id,
            });
        } else {
            self.emit_plugin_event(PluginEvent::BufferChanged {
                doc_id: self.editor.active_buffer().id,
            });
        }
        Ok(())
    }

    fn open_file_at_char_location(&mut self, path: &Path, line: usize, char_col: usize) {
        self.flush_insert_transaction_if_active();
        let jump_before = self.editor.current_jump_location();
        let path_str = path.to_string_lossy().to_string();
        self.editor.open_file(&path_str);
        self.editor
            .active_buffer_mut()
            .set_cursor_line_char(line, char_col);
        let jump_after = self.editor.current_jump_location();
        self.record_jump_transition_if_needed(jump_before, jump_after);
        self.emit_plugin_event(PluginEvent::BufferActivated {
            doc_id: self.editor.active_buffer().id,
        });
        self.queue_active_doc_git_refresh(true);
    }

    fn open_in_editor_diff_target_under_cursor(&mut self) -> bool {
        let buffer_id = self.editor.active_buffer().id;
        let cursor_line = self.editor.active_buffer().cursor_line();
        let target = self
            .in_editor_diff_buffers
            .get(&buffer_id)
            .and_then(|state| state.line_targets.get(&cursor_line))
            .cloned();
        let Some(target) = target else {
            return false;
        };
        self.open_file_at_char_location(&target.path, target.line, target.char_col);
        true
    }

    fn open_file_at_lsp_location(&mut self, path: &Path, line: usize, character_utf16: usize) {
        self.flush_insert_transaction_if_active();
        let jump_before = self.editor.current_jump_location();
        let path_str = path.to_string_lossy().to_string();
        self.editor.open_file(&path_str);
        let char_col = self
            .editor
            .active_buffer()
            .utf16_to_char_col(line, character_utf16);
        self.editor
            .active_buffer_mut()
            .set_cursor_line_char(line, char_col);
        let jump_after = self.editor.current_jump_location();
        self.record_jump_transition_if_needed(jump_before, jump_after);
        self.emit_plugin_event(PluginEvent::BufferActivated {
            doc_id: self.editor.active_buffer().id,
        });
        self.queue_active_doc_git_refresh(true);
    }

    /// Determine which directory to open the explorer in.
    /// Priority: saved state > active buffer's parent > project_root
    fn resolve_explorer_open_target(&self) -> (PathBuf, Option<String>) {
        if let Some(ref dir) = self.last_explorer_dir {
            return (dir.clone(), self.last_explorer_selected.clone());
        }
        if let Some(ref fp) = self.editor.active_buffer().file_path
            && let Some(parent) = fp.parent()
        {
            let name = fp.file_name().map(|n| n.to_string_lossy().to_string());
            return (parent.to_path_buf(), name);
        }
        (self.project_root.clone(), None)
    }

    /// Determine which directory to reveal the current file in.
    /// Always uses active buffer's parent or falls back to project_root.
    fn resolve_reveal_target(&self) -> (PathBuf, Option<String>) {
        if let Some(ref fp) = self.editor.active_buffer().file_path
            && let Some(parent) = fp.parent()
        {
            let name = fp.file_name().map(|n| n.to_string_lossy().to_string());
            return (parent.to_path_buf(), name);
        }
        (self.project_root.clone(), None)
    }

    fn resolve_selected_project_root(&self, selected: &Path) -> Result<PathBuf, String> {
        let selected_abs = if selected.is_absolute() {
            selected.to_path_buf()
        } else {
            self.project_root.join(selected)
        };

        let candidate = if selected_abs.is_dir() {
            selected_abs
        } else if selected_abs.is_file() {
            selected_abs
                .parent()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| "selected file has no parent directory".to_string())?
        } else {
            return Err(format!(
                "path does not exist: {}",
                selected_abs.to_string_lossy()
            ));
        };

        if !candidate.is_dir() {
            return Err(format!(
                "selected path is not a directory: {}",
                candidate.to_string_lossy()
            ));
        }

        Ok(std::fs::canonicalize(&candidate).unwrap_or(candidate))
    }

    fn apply_project_root_change(&mut self, new_root: PathBuf) {
        self.compositor.apply(UiAction::CloseProjectRootPopup);
        self.compositor.apply(UiAction::CloseRecentProjectPopup);
        self.compositor.apply(UiAction::CloseExplorerPopup);
        self.compositor.close_explorer();
        self.last_explorer_dir = None;
        self.last_explorer_selected = None;

        if self.project_root == new_root {
            self.editor.message = Some(format!(
                "Project root unchanged: {}",
                self.project_root.display()
            ));
            return;
        }

        self.project_root = new_root;
        self.discovered_repos = crate::project::discover_sub_repos(&self.project_root);
        self.git_multi_index_snapshots.clear();
        self.command_history = Rc::new(CommandHistory::new(&self.project_root));
        self.refresh_file_index_for_current_root();
        self.refresh_git_index_for_current_root();
        self.git_status_cache.clear();

        let plugin_error = self.rebuild_registry_and_plugin_host();
        self.emit_plugin_event(PluginEvent::BufferActivated {
            doc_id: self.editor.active_buffer().id,
        });

        self.editor.message = Some(match plugin_error {
            Some(err) => format!(
                "Project root changed to {} (plugin init failed: {})",
                self.project_root.display(),
                err
            ),
            None => format!("Project root changed to {}", self.project_root.display()),
        });
    }

    fn fetch_pr_list(
        project_root: &std::path::Path,
    ) -> Result<Vec<crate::ui::overlays::github::pr_picker::PrEntry>, String> {
        let output = std::process::Command::new("gh")
            .args([
                "pr",
                "list",
                "--json",
                "number,title,body,url,state,author,headRefName,createdAt,labels",
                "--limit",
                "100",
            ])
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("Failed to run gh: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("gh error: {}", stderr));
        }

        let json = String::from_utf8_lossy(&output.stdout);
        crate::ui::overlays::github::pr_picker::parse_gh_pr_json(&json)
    }

    fn fetch_issue_list(
        project_root: &std::path::Path,
    ) -> Result<Vec<crate::ui::overlays::github::issue_picker::IssueEntry>, String> {
        let output = std::process::Command::new("gh")
            .args([
                "issue",
                "list",
                "--state",
                "open",
                "--json",
                "number,title,body,url,state,author,createdAt,labels,comments",
                "--limit",
                "100",
            ])
            .current_dir(project_root)
            .output()
            .map_err(|e| format!("Failed to run gh: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("gh error: {}", stderr));
        }

        let json = String::from_utf8_lossy(&output.stdout);
        crate::ui::overlays::github::issue_picker::parse_gh_issue_json(&json)
    }
}

fn is_markdown_file(editor: &Editor, path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    editor
        .language_registry
        .detect_by_extension(&path_str)
        .is_some_and(|lang| lang.name.eq_ignore_ascii_case("markdown"))
}

fn ranked_markdown_link_candidates(
    file_list: &[String],
    project_root: &Path,
    doc_path: &Path,
    typed_fragment: &str,
    max_candidates: usize,
) -> Vec<String> {
    let query = normalize_link_query(typed_fragment);
    let doc_rel_dir = doc_path
        .strip_prefix(project_root)
        .ok()
        .and_then(|p| p.parent().map(|parent| parent.to_path_buf()))
        .unwrap_or_default();

    let mut all_candidates = file_list
        .iter()
        .map(|rel| {
            let rel_path = relative_path_from(&doc_rel_dir, Path::new(rel));
            path_to_slash_string(&rel_path)
        })
        .collect::<Vec<_>>();
    all_candidates.sort();
    all_candidates.dedup();

    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut rest = Vec::new();
    for candidate in all_candidates {
        if query.is_empty() || candidate == query {
            exact.push(candidate);
        } else if candidate.starts_with(&query) {
            prefix.push(candidate);
        } else {
            rest.push(candidate);
        }
    }

    if !exact.is_empty() || !prefix.is_empty() {
        exact.sort();
        prefix.sort();
        rest.sort();
        exact.extend(prefix);
        exact.extend(rest);
        exact.truncate(max_candidates);
        return exact;
    }

    let mut fuzzy = Vec::new();
    let mut remainder = Vec::new();
    for candidate in rest {
        if let Some((score, _)) = fuzzy_match(&candidate, &query) {
            fuzzy.push((score, candidate));
        } else {
            remainder.push(candidate);
        }
    }

    fuzzy.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    remainder.sort();

    let mut ranked = fuzzy.into_iter().map(|(_, c)| c).collect::<Vec<_>>();
    ranked.extend(remainder);
    ranked.truncate(max_candidates);
    ranked
}

fn normalize_link_query(query: &str) -> String {
    if let Some(stripped) = query.strip_prefix("./") {
        stripped.to_string()
    } else {
        query.to_string()
    }
}

fn path_to_slash_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_path_from(base: &Path, target: &Path) -> PathBuf {
    let base_components = normalize_relative_components(base);
    let target_components = normalize_relative_components(target);

    let mut common_len = 0usize;
    while common_len < base_components.len()
        && common_len < target_components.len()
        && base_components[common_len] == target_components[common_len]
    {
        common_len += 1;
    }

    let mut rel = PathBuf::new();
    for _ in common_len..base_components.len() {
        rel.push("..");
    }
    for comp in &target_components[common_len..] {
        rel.push(comp);
    }

    if rel.as_os_str().is_empty() {
        rel.push(".");
    }
    rel
}

fn normalize_relative_components(path: &Path) -> Vec<String> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if components.last().is_some_and(|c| c != "..") {
                    components.pop();
                } else {
                    components.push("..".to_string());
                }
            }
            Component::Prefix(_) | Component::RootDir => {}
        }
    }
    components
}

fn browser_open_command(url: &str) -> (&'static str, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        (
            "cmd",
            vec![
                "/C".to_string(),
                "start".to_string(),
                "".to_string(),
                format!("\"{url}\""),
            ],
        )
    }
    #[cfg(target_os = "macos")]
    {
        ("open", vec![url.to_string()])
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        ("xdg-open", vec![url.to_string()])
    }
}

fn spawn_open_url(url: &str) -> std::io::Result<()> {
    let (program, args) = browser_open_command(url);
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
}

fn should_show_home_screen(start_path: Option<&Path>, editor: &Editor) -> bool {
    if !editor.is_single_clean_scratch() {
        return false;
    }
    match start_path {
        None => true,
        Some(path) => path.is_dir(),
    }
}

fn home_screen_update_notice_from_status(status: &UpgradeCheckStatus) -> Option<String> {
    match status {
        UpgradeCheckStatus::UpdateAvailable { latest, .. } => {
            Some(format!("Update available: v{} (gargo --update)", latest))
        }
        UpgradeCheckStatus::UpToDate { .. } => None,
    }
}

fn home_screen_notice_from_cache(cache: &UpdateCheckCache) -> Option<String> {
    if cache.has_update {
        Some(format!(
            "Update available: v{} (gargo --update)",
            cache.latest_version
        ))
    } else {
        None
    }
}

fn update_check_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("update_check.toml")
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_update_check_cache(data_dir: &Path) -> Option<UpdateCheckCache> {
    let path = update_check_cache_path(data_dir);
    let content = fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

fn load_fresh_update_check_cache(data_dir: &Path, now_secs: u64) -> Option<UpdateCheckCache> {
    let cache = load_update_check_cache(data_dir)?;
    if cache.current_version != env!("CARGO_PKG_VERSION") {
        return None;
    }
    if now_secs.saturating_sub(cache.checked_at_unix_secs) > UPDATE_CHECK_CACHE_TTL_SECS {
        return None;
    }
    Some(cache)
}

fn write_update_check_cache(data_dir: &Path, cache: &UpdateCheckCache) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|e| format!("failed to create data dir: {e}"))?;
    let content =
        toml::to_string(cache).map_err(|e| format!("failed to serialize update cache: {e}"))?;
    fs::write(update_check_cache_path(data_dir), content)
        .map_err(|e| format!("failed to write update cache: {e}"))
}

fn is_home_screen_insert_entry(action: &CoreAction) -> bool {
    matches!(
        action,
        CoreAction::ChangeMode(mode::Mode::Insert)
            | CoreAction::InsertAfterCursor
            | CoreAction::InsertAtLineStart
            | CoreAction::InsertAtLineEnd
            | CoreAction::OpenLineBelow
    )
}

fn core_action_updates_git_gutter(action: &CoreAction) -> bool {
    matches!(
        action,
        CoreAction::InsertChar(_)
            | CoreAction::InsertText(_)
            | CoreAction::InsertNewline
            | CoreAction::DeleteForward
            | CoreAction::DeleteBackward
            | CoreAction::KillLine
            | CoreAction::DeleteSelection
            | CoreAction::Paste
            | CoreAction::Indent
            | CoreAction::Dedent
            | CoreAction::WrapSelection { .. }
            | CoreAction::Undo
            | CoreAction::Redo
            | CoreAction::OpenLineBelow
            | CoreAction::InsertAfterCursor
            | CoreAction::InsertAtLineStart
            | CoreAction::InsertAtLineEnd
    )
}

fn core_action_records_recent_edit(action: &CoreAction) -> bool {
    matches!(
        action,
        CoreAction::InsertChar(_)
            | CoreAction::InsertText(_)
            | CoreAction::InsertNewline
            | CoreAction::DeleteForward
            | CoreAction::DeleteBackward
            | CoreAction::KillLine
            | CoreAction::DeleteSelection
            | CoreAction::Paste
            | CoreAction::Indent
            | CoreAction::Dedent
            | CoreAction::WrapSelection { .. }
            | CoreAction::Undo
            | CoreAction::Redo
            | CoreAction::OpenLineBelow
            | CoreAction::InsertAfterCursor
            | CoreAction::InsertAtLineStart
            | CoreAction::InsertAtLineEnd
    )
}

fn app_action_refreshes_git_status(action: &AppAction) -> bool {
    matches!(
        action,
        AppAction::Buffer(BufferAction::Save)
            | AppAction::Buffer(BufferAction::SaveBufferAs(_))
            | AppAction::Buffer(BufferAction::RefreshBuffer)
            | AppAction::Buffer(BufferAction::CloseBuffer)
            | AppAction::Workspace(WorkspaceAction::OpenExplorerPopup)
            | AppAction::Project(ProjectAction::OpenProjectRootPicker)
            | AppAction::Project(ProjectAction::OpenRecentProjectPicker)
            | AppAction::Workspace(WorkspaceAction::ToggleExplorer)
            | AppAction::Workspace(WorkspaceAction::ToggleChangedFilesSidebar)
            | AppAction::Workspace(WorkspaceAction::RevealInExplorer)
            | AppAction::Project(ProjectAction::ChangeProjectRoot(_))
            | AppAction::Project(ProjectAction::SwitchToRecentProject(_))
            | AppAction::Workspace(WorkspaceAction::OpenCommandPalette)
            | AppAction::Workspace(WorkspaceAction::OpenFilePicker)
            | AppAction::Workspace(WorkspaceAction::OpenGlobalSearch)
            | AppAction::Workspace(WorkspaceAction::OpenGitView)
            | AppAction::Workspace(WorkspaceAction::OpenGitBranchPicker)
            | AppAction::Project(ProjectAction::SwitchGitBranch(_))
            | AppAction::Buffer(BufferAction::OpenFileFromGitView { .. })
    )
}

fn app_action_refreshes_active_doc(action: &AppAction) -> bool {
    matches!(
        action,
        AppAction::Buffer(BufferAction::Save)
            | AppAction::Buffer(BufferAction::SaveBufferAs(_))
            | AppAction::Buffer(BufferAction::RefreshBuffer)
            | AppAction::Lifecycle(LifecycleAction::OpenConfigFile)
            | AppAction::Window(WindowAction::WindowSplit(_))
            | AppAction::Window(WindowAction::WindowFocus(_))
            | AppAction::Window(WindowAction::WindowFocusNext)
            | AppAction::Window(WindowAction::WindowCloseCurrent)
            | AppAction::Window(WindowAction::WindowCloseOthers)
            | AppAction::Window(WindowAction::WindowSwap(_))
            | AppAction::Buffer(BufferAction::OpenFileFromGitView { .. })
            | AppAction::Buffer(BufferAction::OpenFileFromExplorerPopup(_))
            | AppAction::Buffer(BufferAction::OpenFileFromExplorer(_))
            | AppAction::Buffer(BufferAction::OpenProjectFile(_))
            | AppAction::Project(ProjectAction::ChangeProjectRoot(_))
            | AppAction::Project(ProjectAction::SwitchToRecentProject(_))
            | AppAction::Buffer(BufferAction::OpenProjectFileAt { .. })
            | AppAction::Workspace(WorkspaceAction::OpenInEditorDiffView)
            | AppAction::Workspace(WorkspaceAction::OpenGitCommitMessageBuffer)
            | AppAction::Buffer(BufferAction::SwitchBufferById(_))
            | AppAction::Navigation(NavigationAction::JumpOlder)
            | AppAction::Navigation(NavigationAction::JumpNewer)
            | AppAction::Navigation(NavigationAction::JumpToListIndex(_))
    )
}

fn path_is_within_project_root(project_root: &Path, file_path: &Path) -> bool {
    if file_path.strip_prefix(project_root).is_ok() {
        return true;
    }

    let Ok(root) = std::fs::canonicalize(project_root) else {
        return false;
    };
    let Ok(file) = std::fs::canonicalize(file_path) else {
        return false;
    };

    file.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use crossterm::style::Color;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use tempfile::tempdir;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct WorkingDirGuard {
        original: PathBuf,
    }

    impl WorkingDirGuard {
        fn set(path: &Path) -> Self {
            let original = std::env::current_dir().expect("read current dir");
            std::env::set_current_dir(path).expect("switch current dir");
            Self { original }
        }
    }

    impl Drop for WorkingDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn test_app_with_text(text: &str) -> App {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text(text);
        editor.active_buffer_mut().cursors[0] = 0;
        App::new(editor, Config::default(), Some(Path::new(".")))
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git command");
        assert!(
            output.status.success(),
            "git command failed: git {}\nstdout={}\nstderr={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_git_output(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git command");
        assert!(
            output.status.success(),
            "git command failed: git {}\nstdout={}\nstderr={}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn browser_open_command_uses_open_on_macos() {
        let (program, args) = browser_open_command("https://example.com");
        assert_eq!(program, "open");
        assert_eq!(args, vec!["https://example.com".to_string()]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn browser_open_command_uses_cmd_start_on_windows() {
        let (program, args) = browser_open_command("https://example.com");
        assert_eq!(program, "cmd");
        assert_eq!(
            args,
            vec![
                "/C".to_string(),
                "start".to_string(),
                "".to_string(),
                "\"https://example.com\"".to_string()
            ]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn browser_open_command_quotes_windows_url_with_query_delimiters() {
        let (_, args) = browser_open_command("https://example.com/?a=1&b=2");
        assert_eq!(
            args,
            vec![
                "/C".to_string(),
                "start".to_string(),
                "".to_string(),
                "\"https://example.com/?a=1&b=2\"".to_string()
            ]
        );
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    #[test]
    fn browser_open_command_uses_xdg_open_on_unix() {
        let (program, args) = browser_open_command("https://example.com");
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec!["https://example.com".to_string()]);
    }

    fn repo_with_modified_file() -> (tempfile::TempDir, PathBuf) {
        let temp = tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );

        let file = temp.path().join("sample.txt");
        fs::write(&file, "line1\n").expect("write initial file");
        run_git(temp.path(), &["add", "sample.txt"]);
        run_git(temp.path(), &["commit", "-m", "init"]);

        fs::write(&file, "line1\nline2\n").expect("write modified file");
        (temp, file)
    }

    fn drain_git_runtime_events(app: &App) -> Vec<GitRuntimeEvent> {
        let Some(runtime) = &app.git_runtime else {
            return Vec::new();
        };

        let mut events = Vec::new();
        while let Ok(event) = runtime.event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[test]
    fn home_screen_active_on_no_arg_startup() {
        let editor = Editor::new();
        let app = App::new(editor, Config::default(), None);
        assert!(app.is_home_screen_active());
    }

    #[test]
    fn path_within_project_root_accepts_direct_and_canonicalized_matches() {
        let temp = tempdir().expect("create temp dir");
        let root = temp.path().join("repo");
        let nested = root.join("src");
        fs::create_dir_all(&nested).expect("create nested dir");
        let file = nested.join("main.rs");
        fs::write(&file, "fn main() {}\n").expect("write file");

        assert!(path_is_within_project_root(&root, &file));

        let relative_dot = root.join("src/../src/main.rs");
        assert!(path_is_within_project_root(&root, &relative_dot));

        let outside = temp.path().join("outside.rs");
        fs::write(&outside, "fn outside() {}\n").expect("write outside file");
        assert!(!path_is_within_project_root(&root, &outside));
    }

    #[test]
    fn open_config_file_outside_project_keeps_editor_responsive() {
        let _guard = env_lock();
        let temp = tempdir().expect("create temp dir");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.name", "gargo-test"]);
        run_git(&repo, &["config", "user.email", "gargo-test@example.com"]);

        let repo_file = repo.join("README.md");
        fs::write(&repo_file, "# repo\n").expect("write repo file");
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-m", "init"]);

        let config_home = temp.path().join("config-home");
        let gargo_dir = config_home.join("gargo");
        fs::create_dir_all(&gargo_dir).expect("create config dir");
        let config_path = gargo_dir.join("config.toml");
        fs::write(
            &config_path,
            "debug = true\nshow_line_number = true\nhorizontal_scroll_margin = 5\n",
        )
        .expect("write config file");

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config_home);
        }

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(repo_file.to_str().expect("repo path utf-8"));
        let mut app = App::new(editor, config, Some(repo_file.as_path()));

        drain_git_runtime_events(&app);

        let action = Action::App(AppAction::Lifecycle(LifecycleAction::OpenConfigFile));
        assert!(!app.dispatch_action(action));

        assert_eq!(
            app.editor.active_buffer().file_path.as_deref(),
            Some(config_path.as_path())
        );
        assert!(!app.active_buffer_should_refresh_project_scoped_state());

        std::thread::sleep(Duration::from_millis(20));
        let runtime_events = drain_git_runtime_events(&app);
        assert!(
            !runtime_events.iter().any(|event| matches!(
                event,
                GitRuntimeEvent::DocumentGutterUpdated { doc_id, .. }
                    if *doc_id == app.editor.active_buffer().id
            )),
            "external config buffer should not queue project-scoped git refresh events"
        );

        let initial_line = app.editor.active_buffer().cursor_line();
        assert!(!app.dispatch_action(Action::Core(CoreAction::MoveDown)));
        assert_eq!(app.editor.active_buffer().cursor_line(), initial_line + 1);

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    #[test]
    fn home_screen_inactive_when_starting_from_file_path() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("startup.txt");
        fs::write(&file_path, "hello").unwrap();

        let editor = Editor::open(&file_path.to_string_lossy());
        let app = App::new(editor, Config::default(), Some(file_path.as_path()));
        assert!(!app.is_home_screen_active());
    }

    #[test]
    fn startup_relative_directory_uses_explicit_path_chain_for_project_root() {
        let _guard = env_lock();
        let temp = tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let cwd_repo = workspace.join("cwd-repo");
        let target_dir = workspace.join("outside").join("nested");
        fs::create_dir_all(cwd_repo.join(".git")).unwrap();
        fs::create_dir_all(&target_dir).unwrap();

        let _wd = WorkingDirGuard::set(&cwd_repo);
        let app = App::new(
            Editor::new(),
            Config::default(),
            Some(Path::new("../outside/nested")),
        );

        assert_eq!(app.project_root, fs::canonicalize(&target_dir).unwrap());
    }

    #[test]
    fn home_screen_insert_mode_activation_materializes_scratch() {
        let editor = Editor::new();
        let mut app = App::new(editor, Config::default(), None);
        assert!(app.is_home_screen_active());

        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));

        assert!(!app.is_home_screen_active());
        assert_eq!(app.editor.mode, mode::Mode::Insert);
    }

    #[test]
    fn home_screen_insert_entry_actions_materialize_scratch() {
        let actions = [
            CoreAction::InsertAfterCursor,
            CoreAction::InsertAtLineStart,
            CoreAction::InsertAtLineEnd,
            CoreAction::OpenLineBelow,
        ];

        for action in actions {
            let editor = Editor::new();
            let mut app = App::new(editor, Config::default(), None);
            assert!(app.is_home_screen_active());

            app.dispatch(Action::Core(action));

            assert!(!app.is_home_screen_active());
            assert_eq!(app.editor.mode, mode::Mode::Insert);
        }
    }

    #[test]
    fn home_screen_non_insert_action_keeps_home_active() {
        let editor = Editor::new();
        let mut app = App::new(editor, Config::default(), None);
        assert!(app.is_home_screen_active());

        app.dispatch(Action::Core(CoreAction::MoveRight));

        assert!(app.is_home_screen_active());
        assert_eq!(app.editor.mode, mode::Mode::Normal);
    }

    #[test]
    fn update_check_cache_round_trip_and_ttl() {
        let temp = tempdir().unwrap();
        let data_dir = temp.path().join("gargo");
        let now = 1_700_000_000u64;
        let cache = UpdateCheckCache {
            checked_at_unix_secs: now,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: "9.9.9".to_string(),
            has_update: true,
        };

        write_update_check_cache(&data_dir, &cache).expect("write cache");

        assert_eq!(load_update_check_cache(&data_dir), Some(cache.clone()));
        assert_eq!(
            load_fresh_update_check_cache(&data_dir, now + UPDATE_CHECK_CACHE_TTL_SECS - 1),
            Some(cache.clone())
        );
        assert_eq!(
            load_fresh_update_check_cache(&data_dir, now + UPDATE_CHECK_CACHE_TTL_SECS + 1),
            None
        );
        assert_eq!(
            home_screen_notice_from_cache(&cache),
            Some("Update available: v9.9.9 (gargo --update)".to_string())
        );
    }

    #[test]
    fn stale_or_mismatched_update_cache_is_ignored() {
        let temp = tempdir().unwrap();
        let data_dir = temp.path().join("gargo");
        let now = 1_700_000_000u64;
        let stale = UpdateCheckCache {
            checked_at_unix_secs: now.saturating_sub(UPDATE_CHECK_CACHE_TTL_SECS + 10),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            latest_version: "9.9.9".to_string(),
            has_update: true,
        };
        write_update_check_cache(&data_dir, &stale).expect("write stale cache");
        assert_eq!(load_fresh_update_check_cache(&data_dir, now), None);

        let mismatched = UpdateCheckCache {
            checked_at_unix_secs: now,
            current_version: "0.0.1".to_string(),
            latest_version: "9.9.9".to_string(),
            has_update: true,
        };
        write_update_check_cache(&data_dir, &mismatched).expect("write mismatched cache");
        assert_eq!(load_fresh_update_check_cache(&data_dir, now), None);
    }

    #[test]
    fn home_screen_open_project_file_materializes_scratch() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let file_path = temp.path().join("picked.txt");
        fs::write(&file_path, "hello").unwrap();

        let editor = Editor::new();
        let mut app = App::new(editor, Config::default(), Some(temp.path()));
        assert!(app.is_home_screen_active());

        app.dispatch(Action::App(AppAction::Buffer(
            BufferAction::OpenProjectFile("picked.txt".to_string()),
        )));

        assert!(!app.is_home_screen_active());
        assert_eq!(
            app.editor.active_buffer().file_path.as_ref(),
            Some(&file_path)
        );
    }

    #[test]
    fn lazy_file_index_populates_async_after_startup() {
        let temp = tempdir().expect("create temp dir");
        fs::create_dir(temp.path().join(".git")).expect("create git dir");
        fs::write(temp.path().join("first.txt"), "hello").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        config.performance.file_index.mode = FileIndexMode::Lazy;
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        assert!(app.file_index_loading);
        assert!(app.file_index_requested_for_root);
        assert!(app.file_list.is_empty());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.file_index_loading && std::time::Instant::now() < deadline {
            app.poll_file_index_runtime();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(!app.file_index_loading);
        assert!(app.file_index_requested_for_root);
        assert!(app.file_list.contains(&"first.txt".to_string()));
    }

    #[test]
    fn lazy_file_index_global_search_uses_startup_prefetch() {
        let temp = tempdir().expect("create temp dir");
        fs::create_dir(temp.path().join(".git")).expect("create git dir");
        fs::write(temp.path().join("first.txt"), "hello").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        config.performance.file_index.mode = FileIndexMode::Lazy;
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        assert!(app.file_index_requested_for_root);

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenGlobalSearch,
        )));
        assert!(app.file_index_requested_for_root);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.file_index_loading && std::time::Instant::now() < deadline {
            app.poll_file_index_runtime();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(!app.file_index_loading);
        assert!(app.file_list.contains(&"first.txt".to_string()));
    }

    #[test]
    fn lazy_prefetch_skips_when_runtime_unavailable_and_on_demand_still_collects() {
        let temp = tempdir().expect("create temp dir");
        fs::create_dir(temp.path().join(".git")).expect("create git dir");
        fs::write(temp.path().join("first.txt"), "hello").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        config.performance.file_index.mode = FileIndexMode::Lazy;
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        app.file_index_runtime = None;
        app.file_index_loading = false;
        app.file_index_requested_for_root = false;
        app.file_list.clear();

        app.start_lazy_file_index_prefetch_if_possible();
        assert!(!app.file_index_loading);
        assert!(!app.file_index_requested_for_root);
        assert!(app.file_list.is_empty());

        app.ensure_file_index_started_if_needed();
        assert!(!app.file_index_loading);
        assert!(app.file_index_requested_for_root);
        assert!(app.file_list.contains(&"first.txt".to_string()));
    }

    #[test]
    fn git_index_prefetch_populates_async_after_startup() {
        let temp = tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        fs::write(temp.path().join("main.txt"), "hello\n").expect("write file");
        run_git(temp.path(), &["add", "main.txt"]);
        run_git(temp.path(), &["commit", "-m", "init"]);
        run_git(temp.path(), &["branch", "feature/ui"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        assert!(app.git_index_loading);
        assert!(app.git_index_requested_for_root);

        // Wait for the full snapshot (including branch previews) to arrive.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_git_index_runtime();
            if app
                .git_index_snapshot
                .branches
                .iter()
                .any(|entry| entry.name == "feature/ui")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for branch previews"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(!app.git_index_loading);
        assert!(app.git_index_requested_for_root);
    }

    #[test]
    fn open_git_view_uses_active_buffer_repo_root() {
        let temp = tempdir().expect("create temp dir");
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        fs::create_dir_all(&repo_a).expect("create repo_a");
        fs::create_dir_all(&repo_b).expect("create repo_b");

        fs::write(repo_a.join("a.txt"), "a\n").expect("write repo_a file");
        run_git(repo_a.as_path(), &["init"]);
        run_git(repo_a.as_path(), &["config", "user.name", "gargo-test"]);
        run_git(
            repo_a.as_path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        run_git(repo_a.as_path(), &["add", "a.txt"]);
        run_git(repo_a.as_path(), &["commit", "-m", "init-a"]);

        fs::write(repo_b.join("b.txt"), "b\n").expect("write repo_b file");
        run_git(repo_b.as_path(), &["init"]);
        run_git(repo_b.as_path(), &["config", "user.name", "gargo-test"]);
        run_git(
            repo_b.as_path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        run_git(repo_b.as_path(), &["add", "b.txt"]);
        run_git(repo_b.as_path(), &["commit", "-m", "init-b"]);
        run_git(repo_b.as_path(), &["switch", "-c", "feature/b"]);
        fs::write(repo_b.join("b.txt"), "b\nchanged\n").expect("modify repo_b file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo_a.as_path()));

        let repo_b_file = repo_b.join("b.txt");
        app.editor.open_file(&repo_b_file.to_string_lossy());

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenGitView,
        )));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_git_index_runtime();
            if let Some(git_view) = app.compositor.git_view_mut()
                && git_view.branch_name() == "feature/b"
                && git_view
                    .changed_entries()
                    .iter()
                    .any(|entry| entry.path == "b.txt")
                && git_view.message_text() != Some("Loading git index...")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for repo_b git view snapshot"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn open_file_from_git_view_resolves_path_from_project_root() {
        let temp = tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );

        let nested = temp.path().join("nested");
        fs::create_dir_all(&nested).expect("create nested dir");
        let root_file = temp.path().join("root.txt");
        fs::write(&root_file, "root-content\n").expect("write root file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(nested.as_path()));

        app.dispatch(Action::App(AppAction::Buffer(
            BufferAction::OpenFileFromGitView {
                path: "root.txt".to_string(),
                line: None,
            },
        )));

        assert_eq!(
            app.editor.active_buffer().file_path.as_ref(),
            Some(&root_file)
        );
        assert_eq!(
            app.editor.active_buffer().rope.to_string(),
            "root-content\n"
        );
    }

    #[test]
    fn open_file_from_git_view_sets_cursor_to_first_changed_line() {
        let (repo, file) = repo_with_modified_file();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        let mut git_view = GitView::new(repo.path().to_path_buf());
        let action = match git_view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            EventResult::Action(action) => action,
            _ => panic!("expected git view enter to emit an action"),
        };

        app.dispatch(action);

        assert_eq!(app.editor.active_buffer().file_path.as_ref(), Some(&file));
        assert_eq!(app.editor.active_buffer().cursor_line(), 1);
    }

    #[test]
    fn close_git_commit_message_buffer_commits_with_stripped_message() {
        let (repo, _) = repo_with_modified_file();
        run_git(repo.path(), &["add", "sample.txt"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenGitCommitMessageBuffer,
        )));

        assert!(app.is_active_git_commit_buffer());
        {
            let doc = app.editor.active_buffer_mut();
            doc.rope = ropey::Rope::from_str("feat: add line2\n\n# comment\n\nmore detail\n");
            doc.dirty = true;
            doc.cursors = vec![0];
        }

        app.dispatch(Action::App(AppAction::Buffer(BufferAction::CloseBuffer)));

        let subject = run_git_output(repo.path(), &["log", "-1", "--pretty=%s"]);
        let body = run_git_output(repo.path(), &["log", "-1", "--pretty=%b"]);
        assert_eq!(subject, "feat: add line2");
        assert_eq!(body, "more detail");
    }

    #[test]
    fn close_git_commit_message_buffer_with_empty_message_aborts_commit() {
        let (repo, _) = repo_with_modified_file();
        run_git(repo.path(), &["add", "sample.txt"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenGitCommitMessageBuffer,
        )));
        {
            let doc = app.editor.active_buffer_mut();
            doc.rope = ropey::Rope::from_str("# comment only\n\n");
            doc.dirty = true;
            doc.cursors = vec![0];
        }

        app.dispatch(Action::App(AppAction::Buffer(BufferAction::CloseBuffer)));

        let commit_count = run_git_output(repo.path(), &["rev-list", "--count", "HEAD"]);
        assert_eq!(commit_count, "1");
        assert!(
            app.editor
                .message
                .as_deref()
                .unwrap_or("")
                .contains("Commit aborted")
        );
    }

    #[test]
    fn open_git_branch_picker_shows_local_branches() {
        let temp = tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        fs::write(temp.path().join("main.txt"), "hello\n").expect("write file");
        run_git(temp.path(), &["add", "main.txt"]);
        run_git(temp.path(), &["commit", "-m", "init"]);
        run_git(temp.path(), &["branch", "feature/ui"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenGitBranchPicker,
        )));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_git_index_runtime();
            if let Some(palette) = app.compositor.palette_mut()
                && palette
                    .candidates
                    .iter()
                    .any(|entry| entry.label.contains("feature/ui"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for branch picker entries"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        {
            let palette = app
                .compositor
                .palette_mut()
                .expect("branch picker should be opened");
            assert_eq!(
                palette.mode,
                crate::ui::overlays::palette::PaletteMode::GitBranchPicker
            );
        }
        assert!(app.git_index_requested_for_root);
    }

    #[test]
    fn switch_git_branch_action_changes_current_branch() {
        let temp = tempdir().expect("create temp dir");
        run_git(temp.path(), &["init"]);
        run_git(temp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            temp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        fs::write(temp.path().join("main.txt"), "hello\n").expect("write file");
        run_git(temp.path(), &["add", "main.txt"]);
        run_git(temp.path(), &["commit", "-m", "init"]);
        run_git(temp.path(), &["branch", "feature/ui"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::SwitchGitBranch("feature/ui".to_string()),
        )));

        let current = run_git_output(temp.path(), &["branch", "--show-current"]);
        assert_eq!(current, "feature/ui");
    }

    #[test]
    fn open_in_editor_diff_view_creates_new_scratch_when_active_buffer_is_file() {
        let (repo, file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(&file.to_string_lossy());
        let mut app = App::new(editor, config, Some(repo.path()));
        let before_id = app.editor.active_buffer().id;

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));

        assert_ne!(app.editor.active_buffer().id, before_id);
        assert!(app.editor.active_buffer().file_path.is_none());
        assert!(
            app.editor
                .active_buffer()
                .rope
                .to_string()
                .contains("diff --git")
        );
        assert!(
            app.in_editor_diff_buffers
                .contains_key(&app.editor.active_buffer().id)
        );
    }

    #[test]
    fn open_in_editor_diff_view_reuses_active_scratch_buffer() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));
        let scratch_id = app.editor.active_buffer().id;

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));

        assert_eq!(app.editor.active_buffer().id, scratch_id);
        assert!(app.editor.active_buffer().file_path.is_none());
        assert!(
            app.editor
                .active_buffer()
                .rope
                .to_string()
                .contains("IN-EDITOR DIFF VIEW")
        );
        assert!(app.in_editor_diff_buffers.contains_key(&scratch_id));
    }

    #[test]
    fn open_in_editor_diff_view_sets_diff_language_for_active_buffer() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));

        assert_eq!(app.editor.active_language_name(), Some("Diff"));
    }

    #[test]
    fn home_screen_open_in_editor_diff_view_materializes_immediately() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));
        assert!(app.is_home_screen_active());

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));

        assert!(!app.is_home_screen_active());
        assert!(app.editor.active_buffer().file_path.is_none());
        assert!(
            app.editor
                .active_buffer()
                .rope
                .to_string()
                .contains("IN-EDITOR DIFF VIEW")
        );
    }

    #[test]
    fn gd_on_in_editor_diff_line_opens_file_at_target() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));
        let diff_text = app.editor.active_buffer().rope.to_string();
        let target_line = diff_text
            .lines()
            .position(|line| line == "+line2")
            .expect("added line should exist in diff buffer");
        app.editor
            .active_buffer_mut()
            .set_cursor_line_char(target_line, 0);

        app.dispatch(Action::App(AppAction::Integration(
            IntegrationAction::RunPluginCommand {
                id: "lsp.goto_definition".to_string(),
            },
        )));

        let opened_path = app
            .editor
            .active_buffer()
            .file_path
            .as_ref()
            .expect("gd should open file from diff target");
        assert!(
            opened_path.ends_with("sample.txt"),
            "expected sample.txt target, got {}",
            opened_path.display()
        );
        assert_eq!(app.editor.active_buffer().cursor_line(), 1);
    }

    #[test]
    fn gd_on_non_target_diff_line_falls_back_to_plugin_command() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));
        app.editor.active_buffer_mut().set_cursor_line_char(0, 0);

        app.dispatch(Action::App(AppAction::Integration(
            IntegrationAction::RunPluginCommand {
                id: "lsp.goto_definition".to_string(),
            },
        )));

        assert!(app.editor.active_buffer().file_path.is_none());
        assert_eq!(
            app.editor.message.as_deref(),
            Some("Unknown plugin command: lsp.goto_definition")
        );
    }

    #[test]
    fn plugin_output_opens_lsp_references_picker_for_multiple_locations() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir(temp.path().join(".git")).expect("git dir");
        let file = temp.path().join("main.rs");
        fs::write(&file, "fn helper() {}\nfn main() { helper(); }\n").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        app.apply_plugin_outputs(vec![PluginOutput::OpenLspReferencesPicker {
            caller_label: "LSP: Find References".to_string(),
            locations: vec![
                LspPickerLocation {
                    path: file.clone(),
                    line: 0,
                    character_utf16: 3,
                },
                LspPickerLocation {
                    path: file.clone(),
                    line: 1,
                    character_utf16: 12,
                },
            ],
        }]);

        let palette = app
            .compositor
            .palette_mut()
            .expect("palette should be opened");
        assert_eq!(
            palette.mode,
            crate::ui::overlays::palette::PaletteMode::ReferencePicker
        );
        assert_eq!(palette.candidates.len(), 2);
        assert!(palette.candidates[0].label.contains("main.rs:1:4"));
    }

    #[test]
    fn open_smart_copy_opens_symbol_picker_with_candidates() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir(temp.path().join(".git")).expect("git dir");
        let file = temp.path().join("main.rs");
        fs::write(&file, "struct User {}\n\nfn helper() {}\n").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(&file.to_string_lossy());
        let mut app = App::new(editor, config, Some(temp.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenSmartCopy,
        )));

        let palette = app
            .compositor
            .palette_mut()
            .expect("palette should be opened");
        assert_eq!(
            palette.mode,
            crate::ui::overlays::palette::PaletteMode::SymbolPicker
        );
        assert!(palette.candidates.iter().any(|c| c.label.contains("User")));
        assert!(
            palette
                .candidates
                .iter()
                .any(|c| c.label.contains("helper"))
        );
        assert!(!palette.preview_lines.is_empty());
    }

    #[test]
    fn open_smart_copy_without_supported_sections_sets_empty_message() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir(temp.path().join(".git")).expect("git dir");
        let file = temp.path().join("data.json");
        fs::write(&file, "{\"a\":1}\n").expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(&file.to_string_lossy());
        let mut app = App::new(editor, config, Some(temp.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenSmartCopy,
        )));

        let palette = app
            .compositor
            .palette_mut()
            .expect("palette should be opened");
        assert_eq!(
            palette.mode,
            crate::ui::overlays::palette::PaletteMode::SymbolPicker
        );
        assert_eq!(palette.candidates.len(), 0);
        assert_eq!(
            app.editor.message.as_deref(),
            Some("No class/function/code block sections found in active document")
        );
    }

    #[test]
    fn normalize_smart_copy_text_deindents_to_first_line_baseline() {
        let raw = "    fn dispatch_app_buffer(&mut self, action: BufferAction) -> bool {\n        self.dispatch_app_flat(AppAction::Buffer(action))\n    }";
        let normalized = App::normalize_smart_copy_text(raw);
        assert_eq!(
            normalized,
            "fn dispatch_app_buffer(&mut self, action: BufferAction) -> bool {\n    self.dispatch_app_flat(AppAction::Buffer(action))\n}"
        );
    }

    #[test]
    fn smart_copy_extracts_markdown_fenced_code_block_payload() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir(temp.path().join(".git")).expect("git dir");
        let file = temp.path().join("README.md");
        fs::write(
            &file,
            "# Notes\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n",
        )
        .expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(&file.to_string_lossy());
        let app = App::new(editor, config, Some(temp.path()));

        let entries = app.extract_smart_copy_entries();
        let code_block = entries
            .iter()
            .find(|entry| entry.label.contains("code block (rust) [code_block]"))
            .expect("markdown fenced code block candidate should be present");

        assert_eq!(
            code_block.copy_text,
            "```rust\nfn main() {\n    println!(\"hi\");\n}\n```"
        );
        assert!(
            code_block
                .preview_lines
                .first()
                .is_some_and(|line| line.contains("```rust"))
        );
        assert!(
            code_block
                .preview_lines
                .last()
                .is_some_and(|line| line.trim_end().ends_with("```"))
        );
    }

    #[test]
    fn navigation_open_file_at_lsp_location_uses_utf16_column() {
        let temp = tempdir().expect("temp dir");
        fs::create_dir(temp.path().join(".git")).expect("git dir");
        let file = temp.path().join("emoji.rs");
        let line = "let value = \"a😀b\";";
        fs::write(&file, format!("{line}\n")).expect("write file");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        let byte_to_b = line.find('b').expect("contains b");
        let character_utf16 = line[..byte_to_b].encode_utf16().count();
        let expected_char_col = line[..byte_to_b].chars().count();

        app.dispatch(Action::App(AppAction::Navigation(
            NavigationAction::OpenFileAtLspLocation {
                path: file.clone(),
                line: 0,
                character_utf16,
            },
        )));

        let active_path = app
            .editor
            .active_buffer()
            .file_path
            .as_ref()
            .expect("opened file path");
        let expected_path = fs::canonicalize(&file).expect("canonical path");
        let active_path_canonical =
            fs::canonicalize(active_path).unwrap_or_else(|_| active_path.clone());
        assert_eq!(active_path_canonical, expected_path);
        assert_eq!(app.editor.active_buffer().cursor_line(), 0);
        assert_eq!(app.editor.active_buffer().cursor_col(), expected_char_col);
    }

    #[test]
    fn refresh_in_editor_diff_view_updates_content_after_file_change() {
        let (repo, file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));
        assert!(
            !app.editor
                .active_buffer()
                .rope
                .to_string()
                .contains("+line3"),
            "precondition: line3 should not be present before refresh"
        );

        fs::write(&file, "line1\nline2\nline3\n").expect("write second change");
        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::RefreshInEditorDiffView,
        )));

        assert!(
            app.editor
                .active_buffer()
                .rope
                .to_string()
                .contains("+line3"),
            "expected refreshed diff to include +line3"
        );
    }

    #[test]
    fn contextual_r_refresh_shortcut_is_only_available_in_diff_buffer() {
        let (repo, _file) = repo_with_modified_file();
        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.path()));

        let no_action = app
            .resolve_contextual_key_action(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(no_action, None);

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenInEditorDiffView,
        )));
        let action = app
            .resolve_contextual_key_action(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(
            action,
            Some(Action::App(AppAction::Workspace(
                WorkspaceAction::RefreshInEditorDiffView,
            )))
        );
    }

    #[test]
    fn markdown_link_candidates_rank_prefix_and_fuzzy_matches() {
        let file_list = vec!["123.md".to_string(), "345.md".to_string()];
        let project_root = Path::new("/tmp/repo");
        let doc_path = project_root.join("note.md");

        let all = ranked_markdown_link_candidates(&file_list, project_root, &doc_path, "./", 50);
        assert_eq!(all[0], "123.md");
        assert_eq!(all[1], "345.md");

        let fuzzy = ranked_markdown_link_candidates(&file_list, project_root, &doc_path, "./5", 50);
        assert_eq!(fuzzy[0], "345.md");
        assert_eq!(fuzzy[1], "123.md");
    }

    #[test]
    fn open_project_root_picker_opens_project_root_popup() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        assert!(!app.compositor.has_project_root_popup());

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::OpenProjectRootPicker,
        )));

        assert!(app.compositor.has_project_root_popup());
    }

    #[test]
    fn open_recent_project_picker_opens_recent_project_popup() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        assert!(!app.compositor.has_recent_project_popup());

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::OpenRecentProjectPicker,
        )));

        assert!(app.compositor.has_recent_project_popup());
    }

    #[test]
    fn open_save_as_popup_opens_overlay() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        assert!(!app.compositor.has_save_as_popup());

        app.dispatch(Action::App(AppAction::Buffer(
            BufferAction::OpenSaveBufferAsPopup,
        )));

        assert!(app.compositor.has_save_as_popup());
    }

    #[test]
    fn save_as_popup_default_for_scratch_is_project_root() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        app.dispatch(Action::App(AppAction::Buffer(
            BufferAction::OpenSaveBufferAsPopup,
        )));
        let expected = app.project_root.to_string_lossy().to_string();
        assert_eq!(
            app.compositor.save_as_popup_input(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn save_as_popup_default_for_file_is_current_file_path() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let file = temp.path().join("note.txt");
        fs::write(&file, "hello").unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let editor = Editor::open(&file.to_string_lossy());
        let mut app = App::new(editor, config, Some(file.as_path()));
        app.dispatch(Action::App(AppAction::Buffer(
            BufferAction::OpenSaveBufferAsPopup,
        )));

        let result = app.compositor.handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &app.registry,
            &app.editor.language_registry,
            &app.config,
            &app.key_state,
        );

        match result {
            EventResult::Action(Action::App(AppAction::Buffer(BufferAction::SaveBufferAs(
                path,
            )))) => {
                assert_eq!(path, file.to_string_lossy().to_string());
            }
            _ => panic!("expected SaveBufferAs action"),
        }
    }

    #[test]
    fn save_buffer_as_relative_path_resolves_from_project_root() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        app.editor.active_buffer_mut().insert_text("saved text");
        app.editor.active_buffer_mut().dirty = true;

        app.dispatch(Action::App(AppAction::Buffer(BufferAction::SaveBufferAs(
            "notes/today.md".to_string(),
        ))));

        let expected = temp.path().join("notes").join("today.md");
        let expected_canonical = fs::canonicalize(&expected).unwrap();
        assert_eq!(fs::read_to_string(&expected).unwrap(), "saved text");
        assert_eq!(
            app.editor.active_buffer().file_path.as_ref(),
            Some(&expected_canonical)
        );
        assert!(!app.editor.active_buffer().dirty);
    }

    #[test]
    fn change_project_root_updates_root_scoped_state_and_closes_explorers() {
        let temp = tempdir().unwrap();
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        fs::create_dir_all(repo_a.join(".git")).unwrap();
        fs::create_dir_all(repo_b.join(".git")).unwrap();
        fs::write(repo_a.join("a.txt"), "a").unwrap();
        fs::write(repo_b.join("b.txt"), "b").unwrap();
        let repo_b_file = repo_b.join("b.txt");
        let expected_root = fs::canonicalize(&repo_b).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        config.performance.file_index.mode = FileIndexMode::Eager;
        let mut app = App::new(Editor::new(), config, Some(repo_a.as_path()));
        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::OpenProjectRootPicker,
        )));
        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::ToggleExplorer,
        )));
        assert!(app.compositor.has_project_root_popup());
        assert!(app.compositor.has_explorer());

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::ChangeProjectRoot(repo_b_file.to_string_lossy().to_string()),
        )));

        assert_eq!(app.project_root, expected_root);
        assert!(!app.compositor.has_project_root_popup());
        assert!(!app.compositor.has_explorer_popup());
        assert!(!app.compositor.has_explorer());
        assert!(app.file_list.contains(&"b.txt".to_string()));
        assert!(!app.file_list.contains(&"a.txt".to_string()));
        assert!(
            app.editor
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("Project root changed to"))
        );
    }

    #[test]
    fn toggle_changed_files_sidebar_opens_and_closes_same_mode() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::write(temp.path().join("changed.txt"), "content").unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));
        app.git_status_cache
            .insert("changed.txt".to_string(), GitFileStatus::Modified);

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::ToggleChangedFilesSidebar,
        )));
        assert!(app.compositor.has_explorer());
        {
            let explorer = app
                .compositor
                .explorer_mut()
                .expect("changed-files sidebar should open");
            assert!(explorer.is_changed_only());
            assert_eq!(explorer.current_dir(), app.project_root.as_path());
        }

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::ToggleChangedFilesSidebar,
        )));
        assert!(!app.compositor.has_explorer());
    }

    #[test]
    fn toggle_changed_files_sidebar_replaces_normal_explorer() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(temp.path()));

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::ToggleExplorer,
        )));
        assert!(app.compositor.has_explorer());
        {
            let explorer = app
                .compositor
                .explorer_mut()
                .expect("regular explorer should open");
            assert!(!explorer.is_changed_only());
        }

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::ToggleChangedFilesSidebar,
        )));
        assert!(app.compositor.has_explorer());
        {
            let explorer = app
                .compositor
                .explorer_mut()
                .expect("changed-files sidebar should replace explorer");
            assert!(explorer.is_changed_only());
        }
    }

    #[test]
    fn lazy_root_change_restarts_file_index_prefetch() {
        let temp = tempdir().unwrap();
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        fs::create_dir_all(repo_a.join(".git")).unwrap();
        fs::create_dir_all(repo_b.join(".git")).unwrap();
        fs::write(repo_a.join("a.txt"), "a").unwrap();
        fs::write(repo_b.join("b.txt"), "b").unwrap();
        let repo_b_file = repo_b.join("b.txt");
        let expected_root = fs::canonicalize(&repo_b).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        config.performance.file_index.mode = FileIndexMode::Lazy;
        let mut app = App::new(Editor::new(), config, Some(repo_a.as_path()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.file_index_loading && std::time::Instant::now() < deadline {
            app.poll_file_index_runtime();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.file_index_requested_for_root);
        assert!(app.file_list.contains(&"a.txt".to_string()));

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::ChangeProjectRoot(repo_b_file.to_string_lossy().to_string()),
        )));

        assert_eq!(app.project_root, expected_root);
        assert!(app.file_index_requested_for_root);
        assert!(!app.file_list.contains(&"a.txt".to_string()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app.file_index_loading && std::time::Instant::now() < deadline {
            app.poll_file_index_runtime();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!app.file_index_loading);
        assert!(app.file_list.contains(&"b.txt".to_string()));
    }

    #[test]
    fn git_index_root_change_restarts_prefetch() {
        let temp = tempdir().unwrap();
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        fs::create_dir_all(&repo_a).unwrap();
        fs::create_dir_all(&repo_b).unwrap();
        fs::write(repo_a.join("a.txt"), "a").unwrap();
        fs::write(repo_b.join("b.txt"), "b").unwrap();
        let repo_b_file = repo_b.join("b.txt");
        let expected_root = fs::canonicalize(&repo_b).unwrap();

        run_git(repo_a.as_path(), &["init"]);
        run_git(repo_a.as_path(), &["config", "user.name", "gargo-test"]);
        run_git(
            repo_a.as_path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        run_git(repo_a.as_path(), &["add", "a.txt"]);
        run_git(repo_a.as_path(), &["commit", "-m", "init-a"]);
        run_git(repo_a.as_path(), &["branch", "feature/a"]);

        run_git(repo_b.as_path(), &["init"]);
        run_git(repo_b.as_path(), &["config", "user.name", "gargo-test"]);
        run_git(
            repo_b.as_path(),
            &["config", "user.email", "gargo-test@example.com"],
        );
        run_git(repo_b.as_path(), &["add", "b.txt"]);
        run_git(repo_b.as_path(), &["commit", "-m", "init-b"]);
        run_git(repo_b.as_path(), &["branch", "feature/b"]);

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo_a.as_path()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_git_index_runtime();
            if app
                .git_index_snapshot
                .branches
                .iter()
                .any(|entry| entry.name == "feature/a")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for feature/a branch"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::ChangeProjectRoot(repo_b_file.to_string_lossy().to_string()),
        )));

        assert_eq!(app.project_root, expected_root);
        assert!(app.git_index_requested_for_root);
        assert!(
            !app.git_index_snapshot
                .branches
                .iter()
                .any(|entry| entry.name == "feature/a")
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.poll_git_index_runtime();
            if app
                .git_index_snapshot
                .branches
                .iter()
                .any(|entry| entry.name == "feature/b")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for feature/b branch"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn switch_to_recent_project_changes_root_and_closes_recent_popup() {
        let temp = tempdir().unwrap();
        let repo_a = temp.path().join("repo_a");
        let repo_b = temp.path().join("repo_b");
        fs::create_dir_all(repo_a.join(".git")).unwrap();
        fs::create_dir_all(repo_b.join(".git")).unwrap();
        let expected_root = fs::canonicalize(&repo_b).unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo_a.as_path()));

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::OpenRecentProjectPicker,
        )));
        assert!(app.compositor.has_recent_project_popup());

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::SwitchToRecentProject(repo_b.to_string_lossy().to_string()),
        )));

        assert_eq!(app.project_root, expected_root);
        assert!(!app.compositor.has_recent_project_popup());
        assert!(
            app.editor
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("Project root changed to"))
        );
    }

    #[test]
    fn change_project_root_invalid_path_keeps_existing_root() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        let missing = temp.path().join("missing").join("nope");

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut app = App::new(Editor::new(), config, Some(repo.as_path()));
        let original_root = app.project_root.clone();

        app.dispatch(Action::App(AppAction::Project(
            ProjectAction::ChangeProjectRoot(missing.to_string_lossy().to_string()),
        )));

        assert_eq!(app.project_root, original_root);
        assert!(
            app.editor
                .message
                .as_deref()
                .is_some_and(|msg| msg.contains("Change project root failed"))
        );
    }

    #[test]
    fn markdown_link_candidates_are_relative_to_current_document_directory() {
        let file_list = vec!["README.md".to_string(), "docs/guide.md".to_string()];
        let project_root = Path::new("/tmp/repo");
        let doc_path = project_root.join("docs").join("notes").join("current.md");

        let ranked = ranked_markdown_link_candidates(&file_list, project_root, &doc_path, "", 50);
        assert!(ranked.contains(&"../../README.md".to_string()));
        assert!(ranked.contains(&"../guide.md".to_string()));
    }

    #[test]
    fn apply_markdown_link_completion_replaces_only_path_fragment_and_keeps_insert_mode() {
        let temp = tempdir().unwrap();
        let doc_path = temp.path().join("note.md");
        fs::write(&doc_path, "[x](./5").unwrap();

        let mut config = Config::default();
        config.plugins.enabled.clear();
        let mut editor = Editor::open(&doc_path.to_string_lossy());
        editor.mode = mode::Mode::Insert;
        let end = editor.active_buffer().rope.len_chars();
        editor.active_buffer_mut().cursors[0] = end;

        let mut app = App::new(editor, config, Some(temp.path()));
        app.dispatch(Action::App(AppAction::Integration(
            IntegrationAction::ApplyMarkdownLinkCompletion {
                candidate: "345.md".to_string(),
            },
        )));

        assert_eq!(app.editor.active_buffer().rope.to_string(), "[x](345.md");
        assert_eq!(app.editor.mode, mode::Mode::Insert);
        assert_eq!(
            app.editor.active_buffer().cursors[0],
            "[x](345.md".chars().count()
        );
    }

    #[test]
    fn dirty_close_request_sets_confirmation_and_message() {
        let mut app = test_app_with_text("");
        app.editor.active_buffer_mut().dirty = true;

        assert!(!app.dispatch(Action::App(AppAction::Buffer(BufferAction::CloseBuffer))));
        assert!(app.close_confirm);
        assert_eq!(app.editor.message.as_deref(), Some(DIRTY_CLOSE_WARNING));
        assert_eq!(app.editor.buffer_count(), 1);
    }

    #[test]
    fn close_confirmation_ctrl_c_force_closes_buffer() {
        let mut app = test_app_with_text("");
        app.editor.new_buffer();
        app.editor.active_buffer_mut().dirty = true;
        app.close_confirm = true;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        app.resolve_close_confirmation(key);

        assert!(!app.close_confirm);
        assert_eq!(app.editor.buffer_count(), 1);
        assert_eq!(app.editor.message, None);
    }

    #[test]
    fn close_confirmation_other_key_aborts() {
        let mut app = test_app_with_text("");
        app.editor.new_buffer();
        app.editor.active_buffer_mut().dirty = true;
        app.close_confirm = true;
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);

        app.resolve_close_confirmation(key);

        assert!(!app.close_confirm);
        assert_eq!(app.editor.buffer_count(), 2);
        assert_eq!(app.editor.message.as_deref(), Some(CLOSE_ABORTED_MESSAGE));
    }

    #[test]
    fn window_split_creates_scratch_and_focuses_new_window() {
        let mut app = test_app_with_text("alpha\n");
        assert_eq!(app.compositor.window_count(), 1);

        app.dispatch(Action::App(AppAction::Window(WindowAction::WindowSplit(
            crate::input::action::WindowSplitAxis::Vertical,
        ))));

        assert_eq!(app.compositor.window_count(), 2);
        assert_eq!(app.editor.buffer_count(), 2);
        assert_eq!(app.editor.active_buffer().id, 2);
        assert!(app.editor.active_buffer().file_path.is_none());
    }

    #[test]
    fn window_close_current_with_multiple_windows_closes_only_window() {
        let mut app = test_app_with_text("alpha\n");
        app.dispatch(Action::App(AppAction::Window(WindowAction::WindowSplit(
            crate::input::action::WindowSplitAxis::Vertical,
        ))));
        assert_eq!(app.compositor.window_count(), 2);
        assert_eq!(app.editor.buffer_count(), 2);

        app.dispatch(Action::App(AppAction::Window(
            WindowAction::WindowCloseCurrent,
        )));

        assert_eq!(app.compositor.window_count(), 1);
        assert_eq!(app.editor.buffer_count(), 2);
    }

    #[test]
    fn window_close_current_single_window_routes_to_close_buffer() {
        let mut editor = Editor::new();
        editor.new_buffer();
        let mut app = App::new(editor, Config::default(), Some(Path::new(".")));
        assert_eq!(app.compositor.window_count(), 1);
        assert_eq!(app.editor.buffer_count(), 2);

        assert!(!app.dispatch(Action::App(AppAction::Window(
            WindowAction::WindowCloseCurrent
        ))));

        assert_eq!(app.compositor.window_count(), 1);
        assert_eq!(app.editor.buffer_count(), 1);
    }

    #[test]
    fn force_close_reconciles_window_buffer_refs() {
        let mut app = test_app_with_text("alpha\n");
        app.dispatch(Action::App(AppAction::Window(WindowAction::WindowSplit(
            crate::input::action::WindowSplitAxis::Vertical,
        ))));
        assert_eq!(app.compositor.window_count(), 2);

        app.editor.active_buffer_mut().dirty = true;
        app.dispatch(Action::App(AppAction::Buffer(BufferAction::CloseBuffer)));
        assert!(app.close_confirm);

        app.resolve_close_confirmation(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.close_confirm);
        assert_eq!(app.editor.buffer_count(), 1);
        assert_eq!(app.compositor.window_count(), 2);

        let (cols, rows) = app.layout_dims();
        let next = app
            .compositor
            .focus_next_window(cols, rows)
            .expect("focus next");
        assert_eq!(next, app.editor.active_buffer().id);
    }

    #[test]
    fn search_cancel_restores_horizontal_scroll() {
        let mut app = test_app_with_text("0123456789abcdefghijklmnopqrstuvwxyz\n");
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 8;
            buf.scroll_offset = 2;
            buf.horizontal_scroll_offset = 4;
        }

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::SearchCancel {
                saved_cursor: 1,
                saved_scroll: 0,
                saved_horizontal_scroll: 2,
            },
        )));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.cursors[0], 1);
        assert_eq!(buf.scroll_offset, 0);
        assert_eq!(buf.horizontal_scroll_offset, 2);
    }

    #[test]
    fn search_cancel_clamps_saved_cursor_to_buffer_len() {
        let mut app = test_app_with_text("short\n");

        app.dispatch(Action::App(AppAction::Workspace(
            WorkspaceAction::SearchCancel {
                saved_cursor: 3303,
                saved_scroll: 0,
                saved_horizontal_scroll: 0,
            },
        )));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.cursors[0], buf.rope.len_chars());
    }

    #[test]
    fn normal_word_move_sets_anchor() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForward));

        assert_eq!(app.editor.active_buffer().selection_anchor(), Some(0));
    }

    #[test]
    fn normal_word_move_no_select_keeps_anchor_cleared() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForwardNoSelect));
        assert!(!app.editor.active_buffer().has_selection());

        app.dispatch(Action::Core(CoreAction::MoveWordBackwardNoSelect));
        assert!(!app.editor.active_buffer().has_selection());
    }

    #[test]
    fn normal_cursor_move_clears_anchor() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        assert!(app.editor.active_buffer().has_selection());

        app.dispatch(Action::Core(CoreAction::MoveRight));
        assert!(!app.editor.active_buffer().has_selection());
    }

    #[test]
    fn visual_to_insert_clears_anchor() {
        let mut app = test_app_with_text("abcd");
        app.editor.mode = mode::Mode::Visual;
        app.editor.active_buffer_mut().set_anchor();
        app.dispatch(Action::Core(CoreAction::MoveRight));
        assert!(app.editor.active_buffer().has_selection());

        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));
        assert_eq!(app.editor.mode, mode::Mode::Insert);
        assert!(!app.editor.active_buffer().has_selection());
    }

    #[test]
    fn insert_char_lf_uses_plain_newline_without_autoindent() {
        let mut app = test_app_with_text("    alpha");
        let end = app.editor.active_buffer().rope.len_chars();
        app.editor.active_buffer_mut().cursors[0] = end;
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        assert_eq!(
            action,
            Action::Core(CoreAction::InsertText("\n".to_string()))
        );
        app.dispatch(action);
        assert_eq!(app.editor.active_buffer().rope.to_string(), "    alpha\n");
    }

    #[test]
    fn insert_enter_keeps_autoindent_behavior() {
        let mut app = test_app_with_text("    alpha");
        let end = app.editor.active_buffer().rope.len_chars();
        app.editor.active_buffer_mut().cursors[0] = end;
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        assert_eq!(action, Action::Core(CoreAction::InsertNewline));
        app.dispatch(action);
        assert_eq!(
            app.editor.active_buffer().rope.to_string(),
            "    alpha\n    "
        );
    }

    #[test]
    fn numeric_g_jumps_to_line_number() {
        let mut app = test_app_with_text("line1\nline2\nline3\n");
        assert!(app.collect_count_prefix(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE,)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        app.dispatch_resolved_key_action(action);
        assert_eq!(app.editor.active_buffer().cursor_line(), 1);
    }

    #[test]
    fn count_prefix_repeats_line_selection_until_eof() {
        let mut app = test_app_with_text("one\ntwo\nthree\nfour\n");
        app.editor.active_buffer_mut().move_down();
        app.editor.mode = mode::Mode::Normal;

        assert!(app.collect_count_prefix(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE,)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        app.dispatch_resolved_key_action(action);

        assert_eq!(app.editor.mode, mode::Mode::Visual);
        assert_eq!(
            app.editor.active_buffer().selection_text(),
            Some("two\nthree\nfour\n".to_string())
        );
        assert_eq!(app.pending_count, None);
    }

    #[test]
    fn count_prefix_repeats_direct_motions() {
        let mut app = test_app_with_text("a\nb\nc\nd\n");
        app.editor.mode = mode::Mode::Normal;

        assert!(app.collect_count_prefix(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE,)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        app.dispatch_resolved_key_action(action);

        assert_eq!(app.editor.active_buffer().cursor_line(), 3);
        assert_eq!(app.pending_count, None);
    }

    #[test]
    fn bare_zero_keeps_line_start_behavior() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;
        app.editor.active_buffer_mut().move_right();
        app.editor.active_buffer_mut().move_right();
        assert_eq!(app.editor.active_buffer().cursors[0], 2);

        assert!(!app.collect_count_prefix(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE,)));

        let action = crate::input::keymap::resolve(
            KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE),
            &mut app.key_state,
            &app.editor.mode,
            app.editor.macro_recorder.is_recording(),
        );

        app.dispatch_resolved_key_action(action);

        assert_eq!(app.editor.active_buffer().cursors[0], 0);
        assert_eq!(app.pending_count, None);
    }

    #[test]
    fn small_moves_do_not_create_jumps() {
        let mut app = test_app_with_text("a\nb\n");
        app.dispatch(Action::Core(CoreAction::MoveDown));
        app.dispatch(Action::Core(CoreAction::MoveUp));
        assert!(app.editor.jump_list_entries().is_empty());

        app.dispatch(Action::Core(CoreAction::MoveToFileEnd));
        assert_eq!(app.editor.jump_list_entries().len(), 2);
    }

    #[test]
    fn jump_label_includes_word_under_cursor() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let app = App::new(Editor::new(), Config::default(), Some(temp.path()));
        let location = JumpLocation {
            doc_id: 1,
            file_path: Some(temp.path().join("src/main.rs")),
            cursor: 5,
            line: 0,
            char_col: 5,
        };

        let label = app.jump_label_for_location(&location, Some("let answer = 42;"));
        assert_eq!(label, "src/main.rs:1:6  answer");
    }

    #[test]
    fn jump_label_omits_word_when_cursor_not_on_word_char() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let app = App::new(Editor::new(), Config::default(), Some(temp.path()));
        let location = JumpLocation {
            doc_id: 1,
            file_path: Some(temp.path().join("src/main.rs")),
            cursor: 3,
            line: 0,
            char_col: 3,
        };

        let label = app.jump_label_for_location(&location, Some("let answer = 42;"));
        assert_eq!(label, "src/main.rs:1:4");
        assert!(!label.ends_with("  "));
    }

    #[test]
    fn insert_transaction_marks_each_edited_line_once() {
        let mut app = test_app_with_text("abc\ndef\n");
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));
        app.dispatch(Action::Core(CoreAction::InsertChar('X')));
        app.dispatch(Action::Core(CoreAction::InsertChar('Y')));
        app.dispatch(Action::Core(CoreAction::MoveDown));
        app.dispatch(Action::Core(CoreAction::InsertChar('Z')));
        assert!(app.editor.jump_list_entries().is_empty());

        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Normal)));
        let lines: Vec<usize> = app
            .editor
            .jump_list_entries()
            .iter()
            .map(|loc| loc.line)
            .collect();
        assert_eq!(lines, vec![0, 1]);
    }

    #[test]
    fn normal_word_then_delete_deletes_selection_not_single_char() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        let (start, end) = app
            .editor
            .active_buffer()
            .selection_range()
            .expect("word motion should set selection");
        let selected_len = end - start;
        assert!(selected_len > 1);
        let before_len = app.editor.active_buffer().rope.len_chars();

        app.dispatch(Action::Core(CoreAction::DeleteSelection));

        assert!(!app.editor.active_buffer().has_selection());
        assert_eq!(
            app.editor.active_buffer().rope.len_chars(),
            before_len - selected_len
        );
    }

    #[test]
    fn normal_e_from_word_start_selects_full_word_and_lands_on_word_end() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForwardEnd));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.char(buf.display_cursor()), 'o');
        assert_eq!(buf.selection_text(), Some("hello".to_string()));
    }

    #[test]
    fn normal_yank_prefers_selection_over_line() {
        let mut app = test_app_with_text("hello world\nnext\n");
        app.editor.mode = mode::Mode::Normal;

        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        assert!(app.editor.active_buffer().has_selection());

        app.dispatch(Action::Core(CoreAction::Yank));

        assert_eq!(app.editor.register.as_deref(), Some("hello "));
        assert!(app.editor.active_buffer().has_selection());
        assert_eq!(app.editor.message.as_deref(), Some("Yanked 6 chars"));
    }

    #[test]
    fn visual_yank_keeps_selection() {
        let mut app = test_app_with_text("hello");
        app.editor.mode = mode::Mode::Visual;
        {
            let buf = app.editor.active_buffer_mut();
            buf.set_anchor();
            buf.move_right();
            buf.move_right();
        }
        assert!(app.editor.active_buffer().has_selection());

        app.dispatch(Action::Core(CoreAction::YankSelection));

        assert_eq!(app.editor.register.as_deref(), Some("he"));
        assert!(app.editor.active_buffer().has_selection());
        assert_eq!(app.editor.mode, mode::Mode::Normal);
        assert_eq!(app.editor.message.as_deref(), Some("Yanked 2 chars"));
    }

    #[test]
    fn add_cursor_to_next_match_uses_visual_selection_and_exits_visual() {
        let mut app = test_app_with_text("foo bar foo baz foo");
        app.editor.mode = mode::Mode::Visual;
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 0;
            buf.set_anchor();
            buf.move_right();
            buf.move_right();
            buf.move_right();
        }

        app.dispatch(Action::Core(CoreAction::AddCursorToNextMatch));

        assert_eq!(app.editor.mode, mode::Mode::Normal);
        assert!(!app.editor.active_buffer().has_selection());
        assert_eq!(app.editor.search.pattern, "foo");
        assert_eq!(app.editor.active_buffer().cursor_count(), 2);
        assert!(app.editor.active_buffer().cursors.contains(&8));
    }

    #[test]
    fn add_cursor_to_next_match_from_visual_does_not_record_jump_entries() {
        let mut app = test_app_with_text("foo bar foo baz foo");
        app.editor.mode = mode::Mode::Visual;
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 0;
            buf.set_anchor();
            buf.move_right();
            buf.move_right();
            buf.move_right();
        }

        app.dispatch(Action::Core(CoreAction::AddCursorToNextMatch));

        assert!(
            app.editor.jump_list_entries().is_empty(),
            "add-cursor should not add synthetic jump entries"
        );
    }

    #[test]
    fn add_cursor_to_prev_match_uses_existing_search_pattern() {
        let mut app = test_app_with_text("foo bar foo baz foo");
        app.editor.mode = mode::Mode::Normal;
        app.editor.search_update("foo");
        app.editor.active_buffer_mut().cursors[0] = 8;

        app.dispatch(Action::Core(CoreAction::AddCursorToPrevMatch));

        assert_eq!(app.editor.active_buffer().cursor_count(), 2);
        assert!(app.editor.active_buffer().cursors.contains(&0));
    }

    #[test]
    fn add_cursor_to_next_match_without_selection_or_search_shows_message() {
        let mut app = test_app_with_text("hello world");
        app.editor.mode = mode::Mode::Normal;
        app.editor.search.clear();

        app.dispatch(Action::Core(CoreAction::AddCursorToNextMatch));

        assert_eq!(
            app.editor.message.as_deref(),
            Some("No selection or active search pattern")
        );
        assert_eq!(app.editor.active_buffer().cursor_count(), 1);
    }

    #[test]
    fn normal_word_motion_matches_flowchart_tb_example() {
        let mut app = test_app_with_text("flowchart TB\n    Start[hourly_flow start]\n");
        app.editor.mode = mode::Mode::Normal;

        // 1) w on 'f' lands display cursor on space between 't' and 'T'
        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.char(buf.display_cursor()), ' ');

        // 2) w on that space selects "TB" and display cursor lands on 'B'
        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.char(buf.display_cursor()), 'B');
        assert_eq!(buf.selection_text(), Some("TB".to_string()));

        // 3) w on displayed 'B' crosses to next line and stops at indent before 'S'
        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.char(buf.display_cursor()), ' ');
        assert_eq!(buf.selection_text(), Some("\n    ".to_string()));

        // 4) w on 'S' moves to 't' while selecting "Start"
        app.dispatch(Action::Core(CoreAction::MoveWordForward));
        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.char(buf.display_cursor()), 't');
        assert_eq!(buf.selection_text(), Some("Start".to_string()));
    }

    #[test]
    fn wrap_selection_in_normal_mode_with_brackets() {
        let mut app = test_app_with_text("hello");
        app.editor.mode = mode::Mode::Normal;
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 1;
            buf.set_anchor();
            buf.move_right();
            buf.move_right();
            buf.move_right();
        }

        app.dispatch(Action::Core(CoreAction::WrapSelection {
            open: '[',
            close: ']',
        }));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.to_string(), "h[ell]o");
        assert_eq!(buf.cursors[0], 2);
        assert!(!buf.has_selection());
        assert_eq!(app.editor.mode, mode::Mode::Normal);
    }

    #[test]
    fn wrap_selection_in_visual_mode_with_parentheses_exits_visual() {
        let mut app = test_app_with_text("hello");
        app.editor.mode = mode::Mode::Visual;
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 1;
            buf.set_anchor();
            buf.move_right();
            buf.move_right();
            buf.move_right();
        }

        app.dispatch(Action::Core(CoreAction::WrapSelection {
            open: '(',
            close: ')',
        }));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.to_string(), "h(ell)o");
        assert_eq!(buf.cursors[0], 2);
        assert!(!buf.has_selection());
        assert_eq!(app.editor.mode, mode::Mode::Normal);
    }

    #[test]
    fn wrap_selection_noops_without_or_with_empty_selection() {
        let mut app = test_app_with_text("hello");
        app.editor.mode = mode::Mode::Normal;
        let before = app.editor.active_buffer().rope.to_string();
        let before_cursor = app.editor.active_buffer().cursors[0];

        app.dispatch(Action::Core(CoreAction::WrapSelection {
            open: '[',
            close: ']',
        }));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.to_string(), before);
        assert_eq!(buf.cursors[0], before_cursor);
        assert!(!buf.has_selection());

        app.editor.mode = mode::Mode::Visual;
        {
            let buf = app.editor.active_buffer_mut();
            buf.cursors[0] = 2;
            buf.set_anchor();
        }
        let before = app.editor.active_buffer().rope.to_string();

        app.dispatch(Action::Core(CoreAction::WrapSelection {
            open: '(',
            close: ')',
        }));

        let buf = app.editor.active_buffer();
        assert_eq!(buf.rope.to_string(), before);
        assert_eq!(app.editor.mode, mode::Mode::Visual);
        assert!(buf.has_selection());
    }

    #[test]
    fn create_default_config_writes_missing_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("gargo").join("config.toml");
        let mut app = test_app_with_text("");

        let msg = app.create_default_config_at_path(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let cfg: Config = toml::from_str(&written).unwrap();

        assert!(msg.contains("Config written:"));
        assert_eq!(cfg.tab_width, 4);
        assert_eq!(cfg.horizontal_scroll_margin, 5);
        assert!(cfg.show_line_number);
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.theme.preset, "ansi_dark");
        assert!(cfg.theme.captures.is_empty());
        assert_eq!(app.config.tab_width, 4);
        assert_eq!(app.config.horizontal_scroll_margin, 5);
        assert_eq!(app.config.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(app.config.git.gutter_debounce_normal_ms, 96);
        assert_eq!(app.config.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(app.config.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(app.config.theme.preset, "ansi_dark");
    }

    #[test]
    fn create_default_config_preserves_existing_values_and_fills_missing() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("gargo").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"
tab_width = 2
[plugins]
enabled = ["diff_ui"]
"#,
        )
        .unwrap();

        let mut app = test_app_with_text("");
        let _ = app.create_default_config_at_path(&path).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let cfg: Config = toml::from_str(&written).unwrap();

        assert_eq!(cfg.tab_width, 2);
        assert_eq!(cfg.horizontal_scroll_margin, 5);
        assert_eq!(cfg.plugins.enabled, vec!["diff_ui"]);
        assert!(cfg.show_line_number);
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.theme.preset, "ansi_dark");
        assert_eq!(app.config.tab_width, 2);
        assert_eq!(app.config.horizontal_scroll_margin, 5);
        assert_eq!(app.config.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(app.config.git.gutter_debounce_normal_ms, 96);
        assert_eq!(app.config.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(app.config.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(app.config.theme.preset, "ansi_dark");
    }

    #[test]
    fn create_default_config_keeps_invalid_existing_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("gargo").join("config.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let invalid = "debug = [";
        fs::write(&path, invalid).unwrap();
        let mut app = test_app_with_text("");

        let err = app.create_default_config_at_path(&path).unwrap_err();
        let after = fs::read_to_string(&path).unwrap();

        assert!(err.contains("Config parse failed:"));
        assert_eq!(after, invalid);
    }

    #[test]
    fn apply_config_runtime_updates_theme() {
        let mut app = test_app_with_text("");
        assert_eq!(
            app.theme.style_for_capture("keyword").unwrap().fg,
            Some(Color::Magenta)
        );

        let mut cfg = Config::default();
        cfg.theme.captures.insert(
            "keyword".to_string(),
            crate::config::ThemeCaptureConfig {
                fg: Some("dark_blue".to_string()),
                bold: Some(false),
                italic: None,
            },
        );

        let _ = app.apply_config_runtime(cfg);
        assert_eq!(
            app.theme.style_for_capture("keyword").unwrap().fg,
            Some(Color::DarkBlue)
        );
    }

    #[test]
    fn toggle_debug_updates_runtime_config_and_message() {
        let mut app = test_app_with_text("");
        assert!(!app.config.debug);

        assert!(!app.dispatch_action(Action::App(AppAction::Lifecycle(
            LifecycleAction::ToggleDebug
        ))));
        assert!(app.config.debug);
        assert_eq!(app.editor.message.as_deref(), Some("Debug: ON"));

        assert!(!app.dispatch_action(Action::App(AppAction::Lifecycle(
            LifecycleAction::ToggleDebug
        ))));
        assert!(!app.config.debug);
        assert_eq!(app.editor.message.as_deref(), Some("Debug: OFF"));
    }

    #[test]
    fn toggle_line_number_updates_runtime_config_and_message() {
        let mut app = test_app_with_text("");
        assert!(app.config.show_line_number);

        assert!(!app.dispatch_action(Action::App(AppAction::Lifecycle(
            LifecycleAction::ToggleLineNumber
        ))));
        assert!(!app.config.show_line_number);
        assert_eq!(app.editor.message.as_deref(), Some("Line numbers: OFF"));

        assert!(!app.dispatch_action(Action::App(AppAction::Lifecycle(
            LifecycleAction::ToggleLineNumber
        ))));
        assert!(app.config.show_line_number);
        assert_eq!(app.editor.message.as_deref(), Some("Line numbers: ON"));
    }

    // -------------------------------------------------------
    // Dot-repeat integration tests
    // -------------------------------------------------------

    #[test]
    fn dot_repeat_insert_session() {
        let mut app = test_app_with_text("");
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));
        app.dispatch(Action::Core(CoreAction::InsertChar('h')));
        app.dispatch(Action::Core(CoreAction::InsertChar('i')));
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Normal)));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "hi");

        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "hihi");
    }

    #[test]
    fn dot_repeat_single_shot_delete() {
        let mut app = test_app_with_text("abcdef");
        app.editor.mode = mode::Mode::Normal;
        app.editor.active_buffer_mut().cursors[0] = 0;
        app.editor.active_buffer_mut().set_anchor();
        app.editor.active_buffer_mut().move_right();
        app.dispatch(Action::Core(CoreAction::DeleteSelection));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "bcdef");

        app.editor.active_buffer_mut().cursors[0] = 0;
        app.editor.active_buffer_mut().set_anchor();
        app.editor.active_buffer_mut().move_right();
        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "cdef");
    }

    #[test]
    fn dot_repeat_paste() {
        let mut app = test_app_with_text("");
        app.editor.register = Some("xy".to_string());
        app.dispatch(Action::Core(CoreAction::Paste));
        assert!(app.editor.active_buffer().rope.to_string().contains("xy"));

        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        let text = app.editor.active_buffer().rope.to_string();
        // Should contain "xy" twice
        assert_eq!(text.matches("xy").count(), 2);
    }

    #[test]
    fn dot_repeat_no_prior_edit_shows_message() {
        let mut app = test_app_with_text("");
        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert_eq!(app.editor.message.as_deref(), Some("No edit to repeat"));
    }

    #[test]
    fn dot_repeat_preserves_last_edit_across_replays() {
        let mut app = test_app_with_text("");
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Insert)));
        app.dispatch(Action::Core(CoreAction::InsertChar('a')));
        app.dispatch(Action::Core(CoreAction::InsertChar('b')));
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Normal)));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "ab");

        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "abab");

        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "ababab");
    }

    #[test]
    fn dot_repeat_insert_after_cursor() {
        let mut app = test_app_with_text("x");
        app.editor.mode = mode::Mode::Normal;
        app.editor.active_buffer_mut().cursors[0] = 0;
        app.dispatch(Action::Core(CoreAction::InsertAfterCursor));
        app.dispatch(Action::Core(CoreAction::InsertChar('y')));
        app.dispatch(Action::Core(CoreAction::ChangeMode(mode::Mode::Normal)));
        assert_eq!(app.editor.active_buffer().rope.to_string(), "xy");

        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));
        assert!(
            app.editor.active_buffer().rope.to_string().contains("yy")
                || app.editor.active_buffer().rope.len_chars() == 3
        );
    }

    #[test]
    fn dot_repeat_with_macro_recording() {
        let mut app = test_app_with_text("abcdef");
        app.editor.mode = mode::Mode::Normal;
        app.editor.active_buffer_mut().cursors[0] = 0;
        app.editor.active_buffer_mut().set_anchor();
        app.editor.active_buffer_mut().move_right();

        // Record a delete as single-shot dot edit
        app.dispatch(Action::Core(CoreAction::DeleteSelection));
        let after_first_delete = app.editor.active_buffer().rope.to_string();
        assert_eq!(after_first_delete, "bcdef");

        // Start macro recording
        app.dispatch(Action::Core(CoreAction::MacroRecord('a')));
        assert!(app.editor.macro_recorder.is_recording());

        // Use dot repeat inside macro
        app.editor.active_buffer_mut().cursors[0] = 0;
        app.editor.active_buffer_mut().set_anchor();
        app.editor.active_buffer_mut().move_right();
        app.dispatch(Action::Core(CoreAction::RepeatLastEdit));

        // Stop macro
        app.dispatch(Action::Core(CoreAction::MacroStop));

        // The macro should have recorded RepeatLastEdit (not the expanded actions)
        let macro_actions = app.editor.macro_recorder.get('a').unwrap();
        assert!(macro_actions.contains(&CoreAction::RepeatLastEdit));
    }
}

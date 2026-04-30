use super::*;

impl App {
    pub(super) fn dispatch_app(&mut self, action: AppAction) -> bool {
        match action {
            AppAction::Buffer(action) => self.dispatch_app_buffer(action),
            AppAction::Project(action) => self.dispatch_app_project(action),
            AppAction::Workspace(action) => self.dispatch_app_workspace(action),
            AppAction::Window(action) => self.dispatch_app_window(action),
            AppAction::Integration(action) => self.dispatch_app_integration(action),
            AppAction::Lifecycle(action) => self.dispatch_app_lifecycle(action),
            AppAction::Navigation(action) => self.dispatch_app_navigation(action),
        }
    }

    fn dispatch_app_buffer(&mut self, action: BufferAction) -> bool {
        self.dispatch_app_flat(AppAction::Buffer(action))
    }

    fn dispatch_app_project(&mut self, action: ProjectAction) -> bool {
        self.dispatch_app_flat(AppAction::Project(action))
    }

    fn dispatch_app_workspace(&mut self, action: WorkspaceAction) -> bool {
        self.dispatch_app_flat(AppAction::Workspace(action))
    }

    fn dispatch_app_window(&mut self, action: WindowAction) -> bool {
        self.dispatch_app_flat(AppAction::Window(action))
    }

    fn dispatch_app_integration(&mut self, action: IntegrationAction) -> bool {
        self.dispatch_app_flat(AppAction::Integration(action))
    }

    fn dispatch_app_lifecycle(&mut self, action: LifecycleAction) -> bool {
        self.dispatch_app_flat(AppAction::Lifecycle(action))
    }

    fn dispatch_app_navigation(&mut self, action: NavigationAction) -> bool {
        self.dispatch_app_flat(AppAction::Navigation(action))
    }

    fn dispatch_app_flat(&mut self, action: AppAction) -> bool {
        let action_for_jump = action.clone();
        let jump_before = self.editor.current_jump_location();
        let should_record_jump = matches!(
            action_for_jump,
            AppAction::Buffer(BufferAction::CloseBuffer)
                | AppAction::Lifecycle(LifecycleAction::OpenConfigFile)
                | AppAction::Buffer(BufferAction::OpenFileFromGitView { .. })
                | AppAction::Buffer(BufferAction::OpenFileFromExplorerPopup(_))
                | AppAction::Buffer(BufferAction::OpenFileFromExplorer(_))
                | AppAction::Buffer(BufferAction::SwitchBufferById(_))
                | AppAction::Buffer(BufferAction::OpenProjectFile(_))
                | AppAction::Buffer(BufferAction::OpenProjectFileAt { .. })
                | AppAction::Workspace(WorkspaceAction::OpenInEditorDiffView)
                | AppAction::Navigation(NavigationAction::JumpToLineChar { .. })
        );
        match action {
            AppAction::Buffer(BufferAction::Save) => match self.editor.active_buffer_mut().save() {
                Ok(msg) => {
                    debug_log!(&self.config, "save: ok");
                    self.editor.message = Some(msg);
                    self.emit_plugin_event(PluginEvent::BufferSaved {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
                Err(e) => {
                    debug_log!(&self.config, "save: failed: {}", e);
                    self.editor.message = Some(format!("Save failed: {}", e));
                }
            },
            AppAction::Buffer(BufferAction::OpenSaveBufferAsPopup) => {
                let default_path = self
                    .editor
                    .active_buffer()
                    .file_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.project_root.to_string_lossy().to_string());
                let popup = SaveAsPopup::new(default_path, self.project_root.clone());
                self.compositor.open_save_as_popup(popup);
            }
            AppAction::Buffer(BufferAction::SaveBufferAs(path)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::CloseSaveAsPopup);

                let trimmed = path.trim();
                if trimmed.is_empty() {
                    self.editor.message = Some("Save as failed: path is empty".to_string());
                    return false;
                }

                let input_path = PathBuf::from(trimmed);
                let target_path = if input_path.is_absolute() {
                    input_path
                } else {
                    self.project_root.join(input_path)
                };

                match self.editor.active_buffer_mut().save_as(&target_path) {
                    Ok(msg) => {
                        self.editor.refresh_active_buffer_language();
                        self.editor.mark_highlights_dirty();
                        self.editor.message = Some(msg);
                        self.emit_plugin_event(PluginEvent::BufferSaved {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(e) => {
                        self.editor.message = Some(format!("Save as failed: {}", e));
                    }
                }
            }
            AppAction::Buffer(BufferAction::OpenRenameFilePopup) => {
                let default_path = self
                    .editor
                    .active_buffer()
                    .file_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.project_root.to_string_lossy().to_string());
                let popup = SaveAsPopup::new_rename(default_path, self.project_root.clone());
                self.compositor.open_save_as_popup(popup);
            }
            AppAction::Buffer(BufferAction::RenameBufferFile(path)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::CloseSaveAsPopup);

                let trimmed = path.trim();
                if trimmed.is_empty() {
                    self.editor.message = Some("Rename failed: path is empty".to_string());
                    return false;
                }

                let input_path = PathBuf::from(trimmed);
                let target_path = if input_path.is_absolute() {
                    input_path
                } else {
                    self.project_root.join(input_path)
                };

                match self.editor.active_buffer_mut().rename_file(&target_path) {
                    Ok(msg) => {
                        self.editor.refresh_active_buffer_language();
                        self.editor.mark_highlights_dirty();
                        self.editor.message = Some(msg);
                        self.emit_plugin_event(PluginEvent::BufferSaved {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(e) => {
                        self.editor.message = Some(format!("Rename failed: {}", e));
                    }
                }
            }
            AppAction::Lifecycle(LifecycleAction::ReloadConfig) => {
                let plugin_error = self.reload_config_runtime();
                self.editor.message = match plugin_error {
                    Some(e) => Some(format!("Config reloaded (plugin init failed: {})", e)),
                    None => Some("Config reloaded".to_string()),
                };
            }
            AppAction::Lifecycle(LifecycleAction::OpenConfigFile) => match Config::path() {
                Some(path) => {
                    self.flush_insert_transaction_if_active();
                    let path_str = path.to_string_lossy().to_string();
                    self.editor.open_file(&path_str);
                    self.editor.message = Some(format!("Opened config: {}", path.display()));
                    self.emit_plugin_event(PluginEvent::BufferActivated {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
                None => {
                    self.editor.message = Some("Could not resolve config path".to_string());
                }
            },
            AppAction::Lifecycle(LifecycleAction::CreateDefaultConfig) => match Config::path() {
                Some(path) => {
                    self.editor.message = Some(match self.create_default_config_at_path(&path) {
                        Ok(msg) => msg,
                        Err(e) => e,
                    });
                }
                None => {
                    self.editor.message = Some("Could not resolve config path".to_string());
                }
            },
            AppAction::Lifecycle(LifecycleAction::ToggleDebug) => {
                self.config.debug = !self.config.debug;
                self.editor.message = Some(format!(
                    "Debug: {}",
                    if self.config.debug { "ON" } else { "OFF" }
                ));
            }
            AppAction::Lifecycle(LifecycleAction::ToggleLineNumber) => {
                self.config.show_line_number = !self.config.show_line_number;
                self.editor.message = Some(format!(
                    "Line numbers: {}",
                    if self.config.show_line_number {
                        "ON"
                    } else {
                        "OFF"
                    }
                ));
            }
            AppAction::Buffer(BufferAction::RefreshBuffer) => {
                match self.editor.reload_active_buffer_from_disk() {
                    Ok(msg) => {
                        debug_log!(&self.config, "refresh: ok");
                        self.editor.message = Some(msg);
                        self.emit_plugin_event(PluginEvent::BufferChanged {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(e) => {
                        debug_log!(&self.config, "refresh: failed: {}", e);
                        self.editor.message = Some(format!("Refresh failed: {}", e));
                    }
                }
            }
            AppAction::Buffer(BufferAction::CloseBuffer) => {
                self.flush_insert_transaction_if_active();
                if self.editor.is_single_clean_scratch() {
                    if self.compositor.window_count() > 1 {
                        match self.compositor.close_focused_window() {
                            Ok(buffer_id) => {
                                if self.editor.switch_to_buffer(buffer_id) {
                                    self.emit_plugin_event(PluginEvent::BufferActivated {
                                        doc_id: self.editor.active_buffer().id,
                                    });
                                }
                            }
                            Err(msg) => {
                                self.editor.message = Some(msg);
                            }
                        }
                        return false;
                    }
                    return true;
                }
                if self.is_active_git_commit_buffer() {
                    match self.close_active_buffer_with_reconciliation(true) {
                        Ok(closed) => {
                            self.emit_plugin_event(PluginEvent::BufferClosed {
                                doc_id: closed.doc_id,
                                path: closed.path,
                            });
                            self.emit_plugin_event(PluginEvent::BufferActivated {
                                doc_id: self.editor.active_buffer().id,
                            });
                        }
                        Err(msg) => {
                            self.editor.message = Some(msg);
                        }
                    }
                } else if self.editor.active_buffer().dirty {
                    self.editor.message = Some(DIRTY_CLOSE_WARNING.to_string());
                    self.close_confirm = true;
                } else if let Ok(closed) = self.close_active_buffer_with_reconciliation(false) {
                    self.emit_plugin_event(PluginEvent::BufferClosed {
                        doc_id: closed.doc_id,
                        path: closed.path,
                    });
                    self.emit_plugin_event(PluginEvent::BufferActivated {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
            }
            AppAction::Lifecycle(LifecycleAction::Quit) => return true,
            AppAction::Lifecycle(LifecycleAction::ForceQuit) => return true,
            AppAction::Lifecycle(LifecycleAction::Cancel) => {
                self.editor.message = Some("Quit".to_string());
            }
            AppAction::Workspace(WorkspaceAction::OpenCommandPalette) => {
                debug_log!(&self.config, "palette: opened (command)");
                self.ensure_file_index_started_if_needed();
                self.queue_git_status_refresh(true);
                let symbols = self.extract_symbol_entries();
                let doc_lines = self.extract_active_doc_lines();
                let mut palette = Palette::new(
                    self.file_list.clone(),
                    &self.project_root,
                    &self.git_status_cache,
                    Some(Rc::clone(&self.command_history)),
                    symbols,
                    doc_lines,
                );
                palette.update_candidates(
                    &self.registry,
                    &self.editor.language_registry,
                    &self.config,
                );
                self.compositor.push_palette(palette);
                if self.file_index_loading {
                    self.editor.message = Some("Indexing project files...".to_string());
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenFilePicker) => {
                debug_log!(&self.config, "palette: opened (file picker)");
                self.ensure_file_index_started_if_needed();
                self.queue_git_status_refresh(true);
                let symbols = self.extract_symbol_entries();
                let doc_lines = self.extract_active_doc_lines();
                let mut palette = Palette::new(
                    self.file_list.clone(),
                    &self.project_root,
                    &self.git_status_cache,
                    None,
                    symbols,
                    doc_lines,
                );
                palette.set_input(String::new());
                palette.update_candidates(
                    &self.registry,
                    &self.editor.language_registry,
                    &self.config,
                );
                self.compositor.push_palette(palette);
                if self.file_index_loading {
                    self.editor.message = Some("Indexing project files...".to_string());
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenBufferPicker) => {
                debug_log!(&self.config, "palette: opened (buffer picker)");
                let entries: Vec<_> = self
                    .editor
                    .buffers()
                    .iter()
                    .map(|b| {
                        let lines: Vec<String> = (0..b.rope.len_lines().min(200))
                            .map(|i| {
                                let line = b.rope.line(i).to_string();
                                line.trim_end_matches('\n').to_string()
                            })
                            .collect();
                        let name = match &b.file_path {
                            Some(p) => p
                                .strip_prefix(&self.project_root)
                                .unwrap_or(p)
                                .display()
                                .to_string(),
                            None => "[scratch]".to_string(),
                        };
                        (b.id, name, lines)
                    })
                    .collect();
                let palette = Palette::new_buffer_picker(entries);
                self.compositor.push_palette(palette);
            }
            AppAction::Workspace(WorkspaceAction::OpenJumpListPicker) => {
                debug_log!(&self.config, "palette: opened (jumplist picker)");
                self.open_jump_list_picker();
            }
            AppAction::Workspace(WorkspaceAction::OpenSymbolPicker) => {
                debug_log!(&self.config, "palette: opened (symbol picker)");
                self.ensure_file_index_started_if_needed();
                self.queue_git_status_refresh(true);
                let symbols = self.extract_symbol_entries();
                let has_symbols = !symbols.is_empty();
                let doc_lines = self.extract_active_doc_lines();
                let mut palette = Palette::new(
                    self.file_list.clone(),
                    &self.project_root,
                    &self.git_status_cache,
                    None,
                    symbols,
                    doc_lines,
                );
                palette.set_input("@".to_string());
                palette.update_candidates(
                    &self.registry,
                    &self.editor.language_registry,
                    &self.config,
                );
                self.compositor.push_palette(palette);
                if !has_symbols {
                    self.editor.message = Some("No symbols found in active document".to_string());
                } else if self.file_index_loading {
                    self.editor.message = Some("Indexing project files...".to_string());
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenSmartCopy) => {
                debug_log!(&self.config, "palette: opened (smart copy)");
                self.queue_git_status_refresh(true);
                let entries = self.extract_smart_copy_entries();
                let has_entries = !entries.is_empty();
                let palette = Palette::new_smart_copy_picker(entries);
                self.compositor.push_palette(palette);
                if !has_entries {
                    self.editor.message = Some(
                        "No class/function/code block sections found in active document"
                            .to_string(),
                    );
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenGlobalSearch) => {
                debug_log!(&self.config, "palette: opened (global search)");
                self.ensure_file_index_started_if_needed();
                self.queue_git_status_refresh(true);
                let palette = Palette::new_global_search(
                    self.file_list.clone(),
                    &self.project_root,
                    &self.git_status_cache,
                );
                self.compositor.push_palette(palette);
                if self.file_index_loading {
                    self.editor.message = Some("Indexing project files...".to_string());
                }
            }
            AppAction::Project(ProjectAction::OpenProjectRootPicker) => {
                self.queue_git_status_refresh(true);
                let popup = ProjectRootPopup::new(self.project_root.clone());
                self.compositor.open_project_root_popup(popup);
            }
            AppAction::Project(ProjectAction::OpenRecentProjectPicker) => {
                self.queue_git_status_refresh(true);
                let entries: Vec<_> = self
                    .recent_projects
                    .get_recent_projects(200)
                    .into_iter()
                    .filter(|entry| std::path::Path::new(&entry.project_path).is_dir())
                    .collect();
                let popup =
                    crate::ui::overlays::project::recent_picker::RecentProjectPopup::new(entries);
                self.compositor.open_recent_project_popup(popup);
            }
            AppAction::Workspace(WorkspaceAction::ToggleExplorer) => {
                if self.compositor.has_explorer() {
                    // Close explorer, save state
                    if let Some(explorer) = self.compositor.close_explorer() {
                        self.last_explorer_dir = Some(explorer.current_dir().to_path_buf());
                        self.last_explorer_selected =
                            explorer.selected_name().map(|s| s.to_string());
                    }
                } else {
                    // Open explorer
                    self.queue_git_status_refresh(true);
                    let initial_preview = self.active_buffer_is_blank();
                    let (dir, select) = self.resolve_explorer_open_target();
                    let mut explorer =
                        Explorer::new(dir, &self.project_root, &self.git_status_cache);
                    if let Some(name) = select {
                        explorer.select_by_name(&name);
                    }
                    explorer.set_preview_mode(initial_preview);
                    self.compositor.open_explorer(explorer);
                }
            }
            AppAction::Workspace(WorkspaceAction::ToggleChangedFilesSidebar) => {
                self.queue_git_status_refresh(true);
                let initial_preview = self.active_buffer_is_blank();
                if let Some(explorer) = self.compositor.close_explorer() {
                    if !explorer.is_changed_only() {
                        self.last_explorer_dir = Some(explorer.current_dir().to_path_buf());
                        self.last_explorer_selected =
                            explorer.selected_name().map(|s| s.to_string());
                        let mut explorer = Explorer::new_changed_only(
                            self.project_root.clone(),
                            &self.project_root,
                            &self.git_status_cache,
                        );
                        explorer.set_preview_mode(initial_preview);
                        self.compositor.open_explorer(explorer);
                    }
                } else {
                    let mut explorer = Explorer::new_changed_only(
                        self.project_root.clone(),
                        &self.project_root,
                        &self.git_status_cache,
                    );
                    explorer.set_preview_mode(initial_preview);
                    self.compositor.open_explorer(explorer);
                }
            }
            AppAction::Workspace(WorkspaceAction::RevealInExplorer) => {
                self.queue_git_status_refresh(true);
                let initial_preview = self.active_buffer_is_blank();
                let (dir, select) = self.resolve_reveal_target();
                let mut explorer = Explorer::new(dir, &self.project_root, &self.git_status_cache);
                if let Some(name) = select {
                    explorer.select_by_name(&name);
                }
                explorer.set_preview_mode(initial_preview);
                self.compositor.open_explorer(explorer);
            }
            AppAction::Workspace(WorkspaceAction::OpenExplorerPopup) => {
                self.queue_git_status_refresh(true);
                let reveal = self.editor.active_buffer().file_path.clone();
                let popup = ExplorerPopup::new(
                    self.project_root.clone(),
                    &self.git_status_cache,
                    reveal.as_deref(),
                );
                self.compositor.open_explorer_popup(popup);
            }
            AppAction::Workspace(WorkspaceAction::OpenCommitLog) => {
                let runtime_tx = self
                    .commit_log_runtime
                    .as_ref()
                    .map(|rt| rt.command_tx.clone());
                let repo_root = self.active_buffer_repo_root();
                let view = crate::ui::overlays::git::CommitLogView::new(repo_root, runtime_tx);
                self.compositor.open_commit_log(view);
            }
            AppAction::Workspace(WorkspaceAction::OpenGitView) => {
                self.queue_git_status_refresh(true);
                self.ensure_git_index_started_if_needed();
                let diff_runtime_tx = self
                    .git_view_diff_runtime
                    .as_ref()
                    .map(|runtime| runtime.command_tx.clone());

                if self.is_multi_repo() {
                    // Build a multi-repo git view
                    let sections: Vec<RepoSection> = if !self.git_multi_index_snapshots.is_empty() {
                        self.git_multi_index_snapshots
                            .iter()
                            .map(|(repo_root, snapshot)| {
                                let display_name = repo_root
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                RepoSection {
                                    project_root: repo_root.clone(),
                                    display_name,
                                    branch: snapshot.branch.clone(),
                                    changed: snapshot.changed.clone(),
                                    staged: snapshot.staged.clone(),
                                }
                            })
                            .collect()
                    } else {
                        // No preloaded snapshots; collect synchronously
                        self.discovered_repos
                            .iter()
                            .map(|repo_root| {
                                let display_name = repo_root
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                let branch = crate::command::git::git_branch_in(repo_root)
                                    .unwrap_or_else(|_| "???".to_string());
                                let (changed, staged) =
                                    crate::command::git::git_status_files_in(repo_root)
                                        .unwrap_or_default();
                                RepoSection {
                                    project_root: repo_root.clone(),
                                    display_name,
                                    branch,
                                    changed,
                                    staged,
                                }
                            })
                            .collect()
                    };
                    let git_view = GitView::new_multi_repo(
                        self.project_root.clone(),
                        sections,
                        diff_runtime_tx,
                        self.config.git.git_view_diff_cache_max_entries,
                        self.config.git.git_view_diff_prefetch_radius,
                    );
                    self.compositor.open_git_view(git_view);
                } else {
                    let repo_root = self.active_buffer_repo_root();
                    let preloaded_index = (!self.git_index_loading
                        || self.git_index_matches_root(&repo_root))
                    .then(|| self.git_view_index_snapshot_for_root(&repo_root))
                    .flatten();
                    let git_view = GitView::new_with_runtime_prefetched(
                        repo_root,
                        diff_runtime_tx,
                        self.config.git.git_view_diff_cache_max_entries,
                        self.config.git.git_view_diff_prefetch_radius,
                        preloaded_index,
                        false,
                    );
                    self.compositor.open_git_view(git_view);
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenGitCommitMessageBuffer) => {
                self.compositor.apply(UiAction::CloseGitView);
                match self.open_git_commit_message_buffer() {
                    Ok(()) => {
                        self.editor.message =
                            Some("Edit commit message, then close buffer to commit".to_string());
                    }
                    Err(err) => {
                        self.editor.message =
                            Some(format!("Failed to open commit message: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenGitBranchPicker) => {
                self.queue_git_status_refresh(true);
                match self.open_git_branch_picker() {
                    Ok(()) => {}
                    Err(err) => {
                        self.editor.message =
                            Some(format!("Failed to open branch picker: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenBranchComparePicker) => {
                self.queue_git_status_refresh(true);
                match self.open_git_branch_compare_picker() {
                    Ok(()) => {}
                    Err(err) => {
                        self.editor.message =
                            Some(format!("Failed to open branch compare picker: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenBranchCompareView(branch)) => {
                self.compositor.apply(UiAction::ClosePalette);
                match self.open_branch_compare_view(&branch) {
                    Ok(()) => {
                        self.editor.message =
                            Some(format!("Opened branch compare: {}...HEAD", branch));
                    }
                    Err(err) => {
                        self.editor.message =
                            Some(format!("Failed to open branch compare: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenCommitDiffView(hash)) => {
                self.compositor.apply(UiAction::CloseCommitLog);
                match self.open_commit_diff_view(&hash) {
                    Ok(()) => {
                        self.editor.message = Some(format!(
                            "Opened commit diff: {}",
                            &hash[..hash.len().min(8)]
                        ));
                    }
                    Err(err) => {
                        self.editor.message = Some(format!("Failed to open commit diff: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenInEditorDiffView) => {
                match self.open_in_editor_diff_view() {
                    Ok(()) => {
                        self.editor.message = Some("Opened in-editor diff view".to_string());
                    }
                    Err(err) => {
                        self.editor.message = Some(format!("Failed to open diff view: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::RefreshInEditorDiffView) => {
                match self.refresh_active_in_editor_diff_view() {
                    Ok(true) => {
                        self.editor.message = Some("Refreshed in-editor diff view".to_string());
                    }
                    Ok(false) => {
                        self.editor.message =
                            Some("Active buffer is not an in-editor diff view".to_string());
                    }
                    Err(err) => {
                        self.editor.message = Some(format!("Failed to refresh diff view: {}", err));
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::OpenPrList) => {
                match Self::fetch_pr_list(&self.active_buffer_repo_root()) {
                    Ok(entries) => {
                        let picker =
                            crate::ui::overlays::github::pr_picker::PrListPicker::new(entries);
                        self.compositor.open_pr_list_picker(picker);
                    }
                    Err(e) => {
                        self.editor.message = Some(e);
                    }
                }
            }
            AppAction::Integration(IntegrationAction::OpenPrUrl(url)) => {
                self.open_url_in_browser(&url);
            }
            AppAction::Workspace(WorkspaceAction::OpenIssueList) => {
                match Self::fetch_issue_list(&self.active_buffer_repo_root()) {
                    Ok(entries) => {
                        let picker =
                            crate::ui::overlays::github::issue_picker::IssueListPicker::new(
                                entries,
                            );
                        self.compositor.open_issue_list_picker(picker);
                    }
                    Err(e) => {
                        self.editor.message = Some(e);
                    }
                }
            }
            AppAction::Integration(IntegrationAction::OpenIssueUrl(url)) => {
                self.open_url_in_browser(&url);
            }
            AppAction::Workspace(WorkspaceAction::OpenFindReplace) => {
                let document_cursor = self.editor.active_buffer().cursors[0];
                let popup = crate::ui::overlays::editor::find_replace::FindReplacePopup::new(
                    document_cursor,
                );
                self.compositor.open_find_replace_popup(popup);
            }
            AppAction::Workspace(WorkspaceAction::ExecuteFindReplace {
                find: _,
                replace,
                use_regex: _,
                replace_all: _,
            }) => {
                // Get matches from the popup before closing it
                let matches = if let Some(popup) = self.compositor.find_replace_popup_mut() {
                    popup.matches().to_vec()
                } else {
                    Vec::new()
                };

                // Close popup
                self.compositor.apply(UiAction::CloseFindReplacePopup);

                if matches.is_empty() {
                    self.editor.message = Some("No matches found".to_string());
                    return false;
                }

                // Execute replacement
                let doc = self.editor.active_buffer_mut();
                doc.begin_transaction();

                // Apply replacements in reverse order to preserve indices
                for (start_char, end_char) in matches.iter().rev() {
                    doc.delete_range(*start_char, *end_char);
                    doc.insert_text_at(*start_char, &replace);
                }

                doc.commit_transaction();

                // Show success message
                let count = matches.len();
                self.editor.message = Some(format!(
                    "Replaced {} match{}",
                    count,
                    if count == 1 { "" } else { "es" }
                ));
                self.editor.mark_highlights_dirty();
                self.emit_plugin_event(PluginEvent::BufferChanged {
                    doc_id: self.editor.active_buffer().id,
                });
                self.queue_active_doc_git_refresh(false);
            }
            AppAction::Window(WindowAction::WindowSplit(axis)) => {
                self.flush_insert_transaction_if_active();
                let previous_buffer = self.editor.active_buffer().id;
                let new_buffer = self.editor.new_buffer();
                let (cols, rows) = self.layout_dims();
                match self
                    .compositor
                    .split_focused_window(axis, new_buffer, cols, rows)
                {
                    Ok(()) => {
                        self.sync_active_buffer_to_focused_window();
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(msg) => {
                        self.editor.force_close_active_buffer();
                        let _ = self.editor.switch_to_buffer(previous_buffer);
                        self.compositor.set_focused_buffer(previous_buffer);
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Window(WindowAction::WindowFocus(direction)) => {
                self.flush_insert_transaction_if_active();
                let (cols, rows) = self.layout_dims();
                match self
                    .compositor
                    .focus_window_direction(direction, cols, rows)
                {
                    Ok(buffer_id) => {
                        if self.editor.switch_to_buffer(buffer_id) {
                            self.emit_plugin_event(PluginEvent::BufferActivated {
                                doc_id: self.editor.active_buffer().id,
                            });
                        }
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Window(WindowAction::WindowFocusNext) => {
                self.flush_insert_transaction_if_active();
                let (cols, rows) = self.layout_dims();
                match self.compositor.focus_next_window(cols, rows) {
                    Ok(buffer_id) => {
                        if self.editor.switch_to_buffer(buffer_id) {
                            self.emit_plugin_event(PluginEvent::BufferActivated {
                                doc_id: self.editor.active_buffer().id,
                            });
                        }
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Window(WindowAction::WindowCloseCurrent) => {
                if self.compositor.window_count() <= 1 {
                    return self
                        .dispatch(Action::App(AppAction::Buffer(BufferAction::CloseBuffer)));
                }
                self.flush_insert_transaction_if_active();
                match self.compositor.close_focused_window() {
                    Ok(buffer_id) => {
                        if self.editor.switch_to_buffer(buffer_id) {
                            self.emit_plugin_event(PluginEvent::BufferActivated {
                                doc_id: self.editor.active_buffer().id,
                            });
                        }
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Window(WindowAction::WindowCloseOthers) => {
                if self.compositor.window_count() <= 1 {
                    self.editor.message = Some("No other windows to close".to_string());
                } else {
                    self.flush_insert_transaction_if_active();
                    let buffer_id = self.compositor.close_other_windows();
                    if self.editor.switch_to_buffer(buffer_id) {
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                }
            }
            AppAction::Window(WindowAction::WindowSwap(direction)) => {
                let (cols, rows) = self.layout_dims();
                match self.compositor.swap_window_direction(direction, cols, rows) {
                    Ok(()) => {
                        self.sync_active_buffer_to_focused_window();
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Buffer(BufferAction::OpenFileFromGitView { path, line }) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::CloseGitView);
                let full_path = self.project_root.join(&path);
                self.editor.open_file(&full_path.to_string_lossy());
                if let Some(line) = line {
                    self.editor
                        .active_buffer_mut()
                        .set_cursor_line_char(line, 0);
                }
                debug_log!(
                    &self.config,
                    "git_view: opened file {}",
                    full_path.display()
                );
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Buffer(BufferAction::OpenFileFromExplorerPopup(path)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::CloseExplorerPopup);
                self.editor.open_file(&path);
                debug_log!(&self.config, "explorer_popup: opened file {}", path);
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Buffer(BufferAction::OpenFileFromExplorer(path)) => {
                self.flush_insert_transaction_if_active();
                // Save explorer state before closing
                if let Some(explorer) = self.compositor.close_explorer() {
                    self.last_explorer_dir = Some(explorer.current_dir().to_path_buf());
                    self.last_explorer_selected = explorer.selected_name().map(|s| s.to_string());
                }
                self.editor.open_file(&path);
                debug_log!(&self.config, "explorer: opened file {}", path);
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Buffer(BufferAction::SwitchBufferById(buf_id)) => {
                self.compositor.apply(UiAction::ClosePalette);
                self.flush_insert_transaction_if_active();
                debug_log!(&self.config, "palette: switch to buffer id={}", buf_id);
                self.editor.switch_to_buffer(buf_id);
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Buffer(BufferAction::OpenProjectFile(rel_path)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::ClosePalette);
                let full_path = self.project_root.join(&rel_path);
                self.editor.open_file(&full_path.to_string_lossy());
                debug_log!(&self.config, "palette: opened file {}", rel_path);
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Project(ProjectAction::ChangeProjectRoot(path)) => {
                self.flush_insert_transaction_if_active();
                let selected = PathBuf::from(path);
                match self.resolve_selected_project_root(&selected) {
                    Ok(new_root) => {
                        self.apply_project_root_change(new_root);
                    }
                    Err(err) => {
                        self.editor.message = Some(format!("Change project root failed: {}", err));
                    }
                }
            }
            AppAction::Project(ProjectAction::SwitchToRecentProject(path)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::CloseRecentProjectPopup);
                let selected = PathBuf::from(path);
                match self.resolve_selected_project_root(&selected) {
                    Ok(new_root) => {
                        let reopen_rel = self.recent_projects.get_last_open_file(&new_root);
                        self.apply_project_root_change(new_root.clone());
                        if let Some(rel) = reopen_rel {
                            let full_path = new_root.join(&rel);
                            if full_path.is_file() {
                                self.editor.open_file(&full_path.to_string_lossy());
                                self.emit_plugin_event(PluginEvent::BufferActivated {
                                    doc_id: self.editor.active_buffer().id,
                                });
                                self.queue_active_doc_git_refresh(true);
                                self.editor.message = Some(format!(
                                    "Switched to {} and opened {}",
                                    new_root.display(),
                                    rel
                                ));
                            }
                        }
                    }
                    Err(err) => {
                        self.editor.message =
                            Some(format!("Switch recent project failed: {}", err));
                    }
                }
            }
            AppAction::Project(ProjectAction::SwitchGitBranch(branch)) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::ClosePalette);
                match crate::command::git::git_switch_branch_in(
                    &self.active_buffer_repo_root(),
                    &branch,
                ) {
                    Ok(()) => {
                        self.queue_file_index_refresh();
                        self.queue_git_index_refresh();
                        self.queue_git_status_refresh(true);
                        self.editor.message = Some(format!("Switched to branch: {}", branch));
                    }
                    Err(err) => {
                        self.editor.message = Some(format!("Branch switch failed: {}", err));
                    }
                }
            }
            AppAction::Buffer(BufferAction::OpenProjectFileAt {
                rel_path,
                line,
                char_col,
            }) => {
                self.flush_insert_transaction_if_active();
                self.compositor.apply(UiAction::ClosePalette);
                let full_path = self.project_root.join(&rel_path);
                self.editor.open_file(&full_path.to_string_lossy());
                self.editor
                    .active_buffer_mut()
                    .set_cursor_line_char(line, char_col);
                debug_log!(
                    &self.config,
                    "palette: opened file {} at {}:{}",
                    rel_path,
                    line + 1,
                    char_col + 1
                );
                self.emit_plugin_event(PluginEvent::BufferActivated {
                    doc_id: self.editor.active_buffer().id,
                });
            }
            AppAction::Navigation(NavigationAction::ExecutePaletteCommand(idx)) => {
                self.compositor.apply(UiAction::ClosePalette);
                let cmd = &self.registry.commands()[idx];
                debug_log!(
                    &self.config,
                    "palette: execute command id={} label={}",
                    cmd.id,
                    cmd.label
                );

                // Record in history
                if let Err(e) = self.command_history.record_execution(&cmd.id) {
                    debug_log!(&self.config, "history: record failed: {}", e);
                }

                let ctx = CommandContext::new(&self.editor);
                let effect = (cmd.action)(&ctx);
                match effect {
                    CommandEffect::None => {}
                    CommandEffect::Message(msg) => {
                        self.editor.message = Some(msg);
                    }
                    CommandEffect::Action(action) => {
                        return self.dispatch(action);
                    }
                }
            }
            AppAction::Workspace(WorkspaceAction::SearchForward) => {
                let cursor = self.editor.active_buffer().cursors[0];
                let scroll = self.editor.active_buffer().scroll_offset;
                let horizontal_scroll = self.editor.active_buffer().horizontal_scroll_offset;
                self.compositor.apply(UiAction::OpenSearchBar {
                    saved_cursor: cursor,
                    saved_scroll: scroll,
                    saved_horizontal_scroll: horizontal_scroll,
                });
                self.editor.search.reset_history_browse();
            }
            AppAction::Workspace(WorkspaceAction::SearchConfirm) => {
                let pattern = self.editor.search.pattern.clone();
                self.editor.search.push_history(&pattern);
                self.compositor.apply(UiAction::CloseSearchBar);
                let count = self.editor.search.matches.len();
                if count > 0 {
                    let idx = self.editor.search.current_match.map(|i| i + 1).unwrap_or(0);
                    self.editor.message = Some(format!("[{}/{}]", idx, count));
                } else {
                    self.editor.message = Some("Pattern not found".to_string());
                }
            }
            AppAction::Workspace(WorkspaceAction::SearchCancel {
                saved_cursor,
                saved_scroll,
                saved_horizontal_scroll,
            }) => {
                self.compositor.apply(UiAction::CloseSearchBar);
                let len = self.editor.active_buffer().rope.len_chars();
                self.editor.active_buffer_mut().cursors = vec![saved_cursor.min(len)];
                self.editor.active_buffer_mut().scroll_offset = saved_scroll;
                self.editor.active_buffer_mut().horizontal_scroll_offset = saved_horizontal_scroll;
                self.editor.search.clear();
            }
            AppAction::Workspace(WorkspaceAction::SearchHistoryPrev) => {
                let current_input = self.compositor.search_bar_input().unwrap_or("").to_string();
                if let Some(pattern) = self.editor.search.history_prev(&current_input) {
                    self.compositor
                        .apply(UiAction::SetSearchBarInput(pattern.clone()));
                    self.editor.search_update(&pattern);
                    self.editor.search_next();
                }
            }
            AppAction::Workspace(WorkspaceAction::SearchHistoryNext) => {
                if let Some(pattern) = self.editor.search.history_next() {
                    self.compositor
                        .apply(UiAction::SetSearchBarInput(pattern.clone()));
                    self.editor.search_update(&pattern);
                    self.editor.search_next();
                }
            }
            AppAction::Integration(IntegrationAction::RunPluginCommand { id }) => {
                if id == "lsp.goto_definition"
                    && self.is_active_in_editor_diff_buffer()
                    && self.open_in_editor_diff_target_under_cursor()
                {
                    return false;
                }
                self.flush_insert_transaction_if_active();
                let ctx = PluginContext::new(&self.editor, &self.project_root, &self.config);
                let outputs = self.plugin_host.run_command(&id, &ctx);
                self.apply_plugin_outputs(outputs);
            }
            AppAction::Integration(IntegrationAction::ApplyMarkdownLinkCompletion {
                candidate,
            }) => {
                self.apply_markdown_link_completion(&candidate);
            }
            AppAction::Integration(IntegrationAction::ShowMessage(message)) => {
                self.editor.message = Some(message);
            }
            AppAction::Integration(IntegrationAction::CopyToClipboard { text, description }) => {
                self.compositor.apply(UiAction::ClosePalette);
                match copy_to_clipboard(&text) {
                    Ok(()) => {
                        self.editor.message = Some(format!("Copied {}: {}", description, text));
                    }
                    Err(e) => {
                        self.editor.message = Some(format!("Copy failed ({}): {}", description, e));
                    }
                }
            }
            AppAction::Navigation(NavigationAction::JumpOlder) => {
                self.suspend_jump_recording = true;
                let result = self.editor.jump_older();
                self.suspend_jump_recording = false;
                match result {
                    Ok(()) => {
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Navigation(NavigationAction::JumpNewer) => {
                self.suspend_jump_recording = true;
                let result = self.editor.jump_newer();
                self.suspend_jump_recording = false;
                match result {
                    Ok(()) => {
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Navigation(NavigationAction::JumpToListIndex(index)) => {
                self.compositor.apply(UiAction::ClosePalette);
                self.suspend_jump_recording = true;
                let result = self.editor.jump_to_list_index(index);
                self.suspend_jump_recording = false;
                match result {
                    Ok(()) => {
                        self.emit_plugin_event(PluginEvent::BufferActivated {
                            doc_id: self.editor.active_buffer().id,
                        });
                    }
                    Err(msg) => {
                        self.editor.message = Some(msg);
                    }
                }
            }
            AppAction::Navigation(NavigationAction::JumpToLineChar { line, char_col }) => {
                self.compositor.apply(UiAction::ClosePalette);
                self.editor
                    .active_buffer_mut()
                    .set_cursor_line_char(line, char_col);
            }
            AppAction::Navigation(NavigationAction::OpenFileAtLspLocation {
                path,
                line,
                character_utf16,
            }) => {
                self.compositor.apply(UiAction::ClosePalette);
                self.open_file_at_lsp_location(&path, line, character_utf16);
            }
        }
        if should_record_jump {
            let jump_after = self.editor.current_jump_location();
            self.record_jump_transition_if_needed(jump_before, jump_after);
        }
        if app_action_refreshes_git_status(&action_for_jump) {
            self.queue_git_status_refresh(true);
        }
        if app_action_refreshes_active_doc(&action_for_jump) {
            if self.active_buffer_should_refresh_project_scoped_state() {
                self.queue_active_doc_git_refresh(true);
            } else {
                debug_log!(
                    &self.config,
                    "git: skipped active-doc refresh for external buffer path={} project_root={}",
                    self.editor
                        .active_buffer()
                        .file_path
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "<scratch>".to_string()),
                    self.project_root.display()
                );
            }
        }
        self.prune_in_editor_diff_buffers();
        self.prune_git_commit_buffers();
        false
    }
}

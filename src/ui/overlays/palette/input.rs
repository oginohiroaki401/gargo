use super::*;

impl Palette {
    pub(super) fn min_input_cursor(&self) -> usize {
        if self.is_unified {
            return 0;
        }
        match self.input.text.chars().next() {
            Some('>') | Some('@') | Some(':') => 1,
            _ => 0,
        }
    }

    pub(super) fn clamp_input_cursor(&mut self) {
        self.input.min_cursor = self.min_input_cursor();
        self.input.clamp();
    }

    pub(super) fn refresh_after_input_edit(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        if self.is_unified {
            self.update_candidates(registry, lang_registry, config);
            return;
        }
        match self.mode {
            PaletteMode::BufferPicker => {
                self.filter_buffer_candidates();
                self.update_buffer_preview();
            }
            PaletteMode::JumpPicker => {
                self.filter_jump_candidates();
                self.update_jump_preview();
            }
            PaletteMode::ReferencePicker => {
                self.filter_reference_candidates();
                self.update_reference_preview();
            }
            PaletteMode::GitBranchPicker | PaletteMode::GitBranchComparePicker => {
                self.filter_git_branch_candidates();
                self.update_git_branch_preview();
            }
            PaletteMode::SymbolPicker => {
                self.filter_symbol_candidates();
                self.update_symbol_preview();
            }
            PaletteMode::GlobalSearch => {
                self.mark_global_search_dirty();
                self.pump_global_search();
            }
            _ => self.update_candidates(registry, lang_registry, config),
        }
    }

    pub fn on_char(
        &mut self,
        c: char,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        self.input.insert_char(c);
        self.refresh_after_input_edit(registry, lang_registry, config);
    }

    /// Insert a string (e.g., from a Paste event or IME composition) into the input.
    pub fn insert_text(
        &mut self,
        text: &str,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        for c in text.chars() {
            self.input.insert_char(c);
        }
        self.refresh_after_input_edit(registry, lang_registry, config);
    }

    pub fn on_backspace(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        if self.input.backspace() {
            self.refresh_after_input_edit(registry, lang_registry, config);
        }
    }

    pub fn on_char_buffer(&mut self, c: char) {
        self.clamp_input_cursor();
        self.input.insert_char(c);
        self.filter_buffer_candidates();
        self.update_buffer_preview();
    }

    pub fn on_backspace_buffer(&mut self) {
        self.clamp_input_cursor();
        if self.input.backspace() {
            self.filter_buffer_candidates();
            self.update_buffer_preview();
        }
    }

    pub fn on_char_jump(&mut self, c: char) {
        self.clamp_input_cursor();
        self.input.insert_char(c);
        self.filter_jump_candidates();
        self.update_jump_preview();
    }

    pub fn on_backspace_jump(&mut self) {
        self.clamp_input_cursor();
        if self.input.backspace() {
            self.filter_jump_candidates();
            self.update_jump_preview();
        }
    }

    pub fn on_delete_prev_word(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        if self.input.delete_prev_word() {
            self.refresh_after_input_edit(registry, lang_registry, config);
        }
    }

    pub fn on_delete_to_end(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        self.clamp_input_cursor();
        if self.input.delete_to_end() {
            self.refresh_after_input_edit(registry, lang_registry, config);
        }
    }

    pub fn select_next(&mut self, lang_registry: &LanguageRegistry, config: &Config) {
        if !self.candidates.is_empty() {
            let prev = self.selected;
            self.selected = if self.selected + 1 >= self.candidates.len() {
                0
            } else {
                self.selected + 1
            };
            if self.selected != prev {
                self.update_preview(lang_registry, config);
            }
        }
    }

    pub fn select_prev(&mut self, lang_registry: &LanguageRegistry, config: &Config) {
        if !self.candidates.is_empty() {
            let prev = self.selected;
            self.selected = if self.selected == 0 {
                self.candidates.len() - 1
            } else {
                self.selected - 1
            };
            if self.selected != prev {
                self.update_preview(lang_registry, config);
            }
        }
    }

    pub fn ensure_selection_visible(&mut self, visible_count: usize) {
        if visible_count == 0 {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + visible_count {
            self.scroll_offset = self.selected - visible_count + 1;
        }
    }

    pub fn selected_command_index(&self) -> Option<usize> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Command(idx) => Some(idx),
                _ => None,
            })
    }

    pub fn selected_buffer_id(&self) -> Option<BufferId> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Buffer(id) => Some(id),
                _ => None,
            })
    }

    pub fn selected_jump_index(&self) -> Option<usize> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Jump(idx) => Some(idx),
                _ => None,
            })
    }

    pub fn selected_reference_location(&self) -> Option<(PathBuf, usize, usize)> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Reference(idx) => self
                    .reference_entries
                    .get(idx)
                    .map(|entry| (entry.path.clone(), entry.line, entry.character_utf16)),
                _ => None,
            })
    }

    pub fn selected_git_branch(&self) -> Option<String> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::GitBranch(idx) => self
                    .git_branch_entries
                    .get(idx)
                    .map(|entry| entry.branch_name.clone()),
                _ => None,
            })
    }

    pub fn selected_symbol_location(&self) -> Option<(usize, usize)> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Symbol(idx) => self
                    .symbol_entries
                    .get(idx)
                    .map(|entry| (entry.line, entry.char_col)),
                _ => None,
            })
    }

    fn selected_symbol_copy_text(&self) -> Option<String> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::Symbol(idx) => self
                    .symbol_entries
                    .get(idx)
                    .and_then(|entry| entry.copy_text.clone()),
                _ => None,
            })
    }

    pub(super) fn selected_search_result(&self) -> Option<&GlobalSearchResultEntry> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::SearchResult(idx) => self.global_search_entries.get(idx),
                _ => None,
            })
    }

    pub fn selected_file_path(&self) -> Option<&str> {
        self.candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::File(idx) => Some(self.file_entries[idx].as_str()),
                _ => None,
            })
    }

    /// Parse `:LINE` or `:LINE:CHAR` input into 0-based (line, char_col).
    pub(super) fn parse_goto_line(input: &str) -> Option<(usize, usize)> {
        let text = input.strip_prefix(':')?.trim();
        if text.is_empty() {
            return None;
        }
        let parts: Vec<&str> = text.splitn(2, ':').collect();
        let line = parts[0].trim().parse::<usize>().ok()?;
        let char_col = if parts.len() > 1 {
            parts[1].trim().parse::<usize>().unwrap_or(1)
        } else {
            1
        };
        Some((line.saturating_sub(1), char_col.saturating_sub(1)))
    }

    /// Handle a key event and return an EventResult.
    /// The registry is needed for command/file filtering.
    pub fn handle_key_event(
        &mut self,
        key: KeyEvent,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) -> EventResult {
        self.pump_global_search();

        debug_log!(
            config,
            "palette: key={:?}, kind={:?}, input_before={:?}",
            key.code,
            key.kind,
            self.input.text
        );

        // Ignore non-Press events (e.g., Release). This is critical for IME input:
        // when the user presses Enter to confirm IME composition, we must ignore
        // the Release event to avoid clearing the input prematurely.
        if key.kind != KeyEventKind::Press {
            debug_log!(config, "palette: ignoring non-Press event");
            return EventResult::Consumed;
        }

        // Control-key bindings for picker navigation and query editing
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('j') => {
                    self.select_next(lang_registry, config);
                    return EventResult::Consumed;
                }
                KeyCode::Char('p') => {
                    self.select_prev(lang_registry, config);
                    return EventResult::Consumed;
                }
                KeyCode::Left => {
                    self.clamp_input_cursor();
                    self.input.move_word_left();
                    return EventResult::Consumed;
                }
                KeyCode::Right => {
                    self.clamp_input_cursor();
                    self.input.move_word_right();
                    return EventResult::Consumed;
                }
                KeyCode::Char('f') => {
                    self.clamp_input_cursor();
                    self.input.move_right();
                    return EventResult::Consumed;
                }
                KeyCode::Char('b') => {
                    self.clamp_input_cursor();
                    self.input.move_left();
                    return EventResult::Consumed;
                }
                KeyCode::Char('a') => {
                    self.clamp_input_cursor();
                    self.input.move_start();
                    return EventResult::Consumed;
                }
                KeyCode::Char('e') => {
                    self.clamp_input_cursor();
                    self.input.move_end();
                    return EventResult::Consumed;
                }
                KeyCode::Char('w') => {
                    self.on_delete_prev_word(registry, lang_registry, config);
                    return EventResult::Consumed;
                }
                KeyCode::Char('k') => {
                    self.on_delete_to_end(registry, lang_registry, config);
                    return EventResult::Consumed;
                }
                KeyCode::Char('c') | KeyCode::Char('q') => {
                    return EventResult::Action(Action::Ui(UiAction::ClosePalette));
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => EventResult::Action(Action::Ui(UiAction::ClosePalette)),
            KeyCode::Down => {
                self.select_next(lang_registry, config);
                EventResult::Consumed
            }
            KeyCode::Up => {
                self.select_prev(lang_registry, config);
                EventResult::Consumed
            }
            KeyCode::Left => {
                self.clamp_input_cursor();
                self.input.move_left();
                EventResult::Consumed
            }
            KeyCode::Right => {
                self.clamp_input_cursor();
                self.input.move_right();
                EventResult::Consumed
            }
            KeyCode::Backspace => {
                self.on_backspace(registry, lang_registry, config);
                EventResult::Consumed
            }
            KeyCode::Enter => {
                debug_log!(
                    config,
                    "palette: Enter pressed, mode={:?}, input={:?}, candidates={}",
                    self.mode,
                    self.input.text,
                    self.candidates.len()
                );
                // Determine action based on mode and selection
                match self.mode {
                    PaletteMode::BufferPicker => {
                        if let Some(buf_id) = self.selected_buffer_id() {
                            EventResult::Action(Action::App(AppAction::Buffer(
                                BufferAction::SwitchBufferById(buf_id),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::JumpPicker => {
                        if let Some(idx) = self.selected_jump_index() {
                            EventResult::Action(Action::App(AppAction::Navigation(
                                NavigationAction::JumpToListIndex(idx),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::ReferencePicker => {
                        if let Some((path, line, character_utf16)) =
                            self.selected_reference_location()
                        {
                            EventResult::Action(Action::App(AppAction::Navigation(
                                NavigationAction::OpenFileAtLspLocation {
                                    path,
                                    line,
                                    character_utf16,
                                },
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::GitBranchPicker => {
                        if let Some(branch) = self.selected_git_branch() {
                            EventResult::Action(Action::App(AppAction::Project(
                                ProjectAction::SwitchGitBranch(branch),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::GitBranchComparePicker => {
                        if let Some(branch) = self.selected_git_branch() {
                            EventResult::Action(Action::App(AppAction::Workspace(
                                WorkspaceAction::OpenBranchCompareView(branch),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::SymbolPicker => match self.symbol_submit_behavior {
                        SymbolSubmitBehavior::JumpToLocation => {
                            if let Some((line, char_col)) = self.selected_symbol_location() {
                                EventResult::Action(Action::App(AppAction::Navigation(
                                    NavigationAction::JumpToLineChar { line, char_col },
                                )))
                            } else {
                                EventResult::Action(Action::Ui(UiAction::ClosePalette))
                            }
                        }
                        SymbolSubmitBehavior::CopyToClipboard => {
                            if let Some(text) = self.selected_symbol_copy_text() {
                                EventResult::Action(Action::App(AppAction::Integration(
                                    IntegrationAction::CopyToClipboard {
                                        text,
                                        description: "smart copy section".to_string(),
                                    },
                                )))
                            } else {
                                EventResult::Action(Action::Ui(UiAction::ClosePalette))
                            }
                        }
                    },
                    PaletteMode::FileFinder => {
                        if let Some(path) = self.selected_file_path() {
                            EventResult::Action(Action::App(AppAction::Buffer(
                                BufferAction::OpenProjectFile(path.to_string()),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::Command => {
                        if let Some(idx) = self.selected_command_index() {
                            EventResult::Action(Action::App(AppAction::Navigation(
                                NavigationAction::ExecutePaletteCommand(idx),
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::GlobalSearch => {
                        if let Some(entry) = self.selected_search_result() {
                            EventResult::Action(Action::App(AppAction::Buffer(
                                BufferAction::OpenProjectFileAt {
                                    rel_path: entry.rel_path.clone(),
                                    line: entry.line,
                                    char_col: entry.char_col,
                                },
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                    PaletteMode::GotoLine => {
                        if let Some((line, char_col)) = Self::parse_goto_line(&self.input.text) {
                            EventResult::Action(Action::App(AppAction::Navigation(
                                NavigationAction::JumpToLineChar { line, char_col },
                            )))
                        } else {
                            EventResult::Action(Action::Ui(UiAction::ClosePalette))
                        }
                    }
                }
            }
            KeyCode::Char(c) => {
                self.on_char(c, registry, lang_registry, config);
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        }
    }
}

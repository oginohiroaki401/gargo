use super::*;

impl App {
    pub(super) fn dispatch_core(&mut self, action: CoreAction) -> bool {
        if self.home_screen_active && is_home_screen_insert_entry(&action) {
            self.materialize_scratch_from_home_if_needed();
        }

        let action_for_plugin = action.clone();
        let jump_before = self.editor.current_jump_location();
        let should_record_jump = matches!(
            action_for_plugin,
            CoreAction::MoveToFileStart
                | CoreAction::MoveToFileEnd
                | CoreAction::MoveToLineNumber(_)
                | CoreAction::SearchUpdate(_)
                | CoreAction::SearchNext
                | CoreAction::SearchPrev
                | CoreAction::NextBuffer
                | CoreAction::PrevBuffer
                | CoreAction::SwitchBufferByIndex(_)
        );
        // Record actions into macro register if recording
        if self.editor.macro_recorder.is_recording() && !self.editor.dot_recorder.is_replaying() {
            match &action {
                CoreAction::MacroRecord(_)
                | CoreAction::MacroStop
                | CoreAction::MacroPlay(_)
                | CoreAction::MacroPlayLast
                | CoreAction::InsertText(_)
                | CoreAction::Noop => {}
                _ => {
                    self.editor.macro_recorder.record(&action);
                }
            }
        }

        // Dot-repeat recording
        if !self.editor.dot_recorder.is_replaying() {
            match &action {
                CoreAction::ChangeMode(mode::Mode::Insert)
                | CoreAction::InsertAfterCursor
                | CoreAction::InsertAtLineStart
                | CoreAction::InsertAtLineEnd
                | CoreAction::OpenLineBelow => {
                    self.editor
                        .dot_recorder
                        .begin_insert_session(action.clone());
                }
                CoreAction::ChangeMode(mode::Mode::Normal)
                    if self.editor.dot_recorder.is_recording_insert() =>
                {
                    self.editor
                        .dot_recorder
                        .finalize_insert_session(action.clone());
                }
                CoreAction::DeleteSelection
                | CoreAction::Paste
                | CoreAction::Indent
                | CoreAction::Dedent
                | CoreAction::WrapSelection { .. } => {
                    if !self.editor.dot_recorder.is_recording_insert() {
                        self.editor.dot_recorder.record_single_shot(action.clone());
                    } else {
                        self.editor.dot_recorder.record(&action);
                    }
                }
                _ => {
                    if self.editor.dot_recorder.is_recording_insert() {
                        self.editor.dot_recorder.record(&action);
                    }
                }
            }
        }

        match action {
            CoreAction::MoveRight => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_right();
            }
            CoreAction::MoveLeft => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_left();
            }
            CoreAction::MoveDown => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_down();
            }
            CoreAction::MoveUp => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_up();
            }
            CoreAction::MoveToLineStart => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_to_line_start();
            }
            CoreAction::MoveToLineEnd => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_to_line_end();
            }
            CoreAction::MoveWordForward => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_word_forward();
            }
            CoreAction::MoveWordForwardEnd => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_word_forward_end()
            }
            CoreAction::MoveWordBackward => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_word_backward();
            }
            CoreAction::MoveWordForwardNoSelect => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_word_forward();
            }
            CoreAction::MoveWordBackwardNoSelect => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_word_backward();
            }
            CoreAction::MoveLongWordForward => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_long_word_forward()
            }
            CoreAction::MoveLongWordForwardEnd => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_long_word_forward_end()
            }
            CoreAction::MoveLongWordBackward => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().set_anchor();
                }
                self.editor.active_buffer_mut().move_long_word_backward()
            }
            CoreAction::MoveToLineNumber(line) => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor
                    .active_buffer_mut()
                    .set_cursor_line_char(line, 0);
            }
            CoreAction::MoveToFileStart => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_to_file_start();
            }
            CoreAction::MoveToFileEnd => {
                if self.editor.mode == mode::Mode::Normal {
                    self.editor.active_buffer_mut().clear_anchor();
                }
                self.editor.active_buffer_mut().move_to_file_end();
            }
            CoreAction::DeleteForward => {
                self.queue_insert_edit_jump_line();
                self.editor.active_buffer_mut().delete_forward();
                self.editor.mark_highlights_dirty();
            }
            CoreAction::DeleteBackward => {
                self.queue_insert_edit_jump_line();
                self.editor.active_buffer_mut().delete_backward();
                self.editor.mark_highlights_dirty();
            }
            CoreAction::KillLine => {
                self.queue_insert_edit_jump_line();
                self.editor.active_buffer_mut().kill_line();
                self.editor.mark_highlights_dirty();
            }
            CoreAction::InsertNewline => {
                self.queue_insert_edit_jump_line();
                self.editor
                    .insert_newline_with_indent(self.config.tab_width);
            }
            CoreAction::InsertChar(c) => {
                self.queue_insert_edit_jump_line();
                self.editor.active_buffer_mut().insert_char(c);
                self.editor.mark_highlights_dirty();
            }
            CoreAction::InsertText(text) => {
                self.queue_insert_edit_jump_line();
                let t0 = std::time::Instant::now();
                self.editor.active_buffer_mut().insert_text(&text);
                let insert_elapsed = t0.elapsed();
                self.editor.mark_highlights_dirty();
                if insert_elapsed.as_millis() > 10 {
                    debug_log!(
                        &self.config,
                        "insert_text: {} bytes, took {:?}",
                        text.len(),
                        insert_elapsed
                    );
                }
            }
            CoreAction::ChangeMode(m) => {
                let old_mode = self.editor.mode;
                self.editor.mode = m;
                if old_mode == mode::Mode::Insert && m != mode::Mode::Insert {
                    self.editor.active_buffer_mut().commit_transaction();
                    self.flush_pending_edit_jump_lines();
                }
                if m == mode::Mode::Insert && old_mode != mode::Mode::Insert {
                    // Insert mode should never inherit an active visual/normal selection anchor.
                    self.editor.active_buffer_mut().clear_anchor();
                    self.editor.active_buffer_mut().begin_transaction();
                    self.pending_edit_jump_locations.clear();
                }
                // Entering Visual: set anchor at current cursor
                if m == mode::Mode::Visual && old_mode != mode::Mode::Visual {
                    self.editor.active_buffer_mut().set_anchor();
                }
                // Leaving Visual: clear anchor
                if old_mode == mode::Mode::Visual && m != mode::Mode::Visual {
                    self.editor.active_buffer_mut().clear_anchor();
                }
            }
            CoreAction::InsertAfterCursor => {
                // Helix-like append-at-EOF behavior: if there is a non-empty selection
                // ending at EOF, insert one line ending first.
                let append_newline = {
                    let buf = self.editor.active_buffer();
                    matches!(buf.selection_range(), Some((_, end)) if end == buf.rope.len_chars())
                };
                if append_newline {
                    self.editor.active_buffer_mut().append_newline_at_eof();
                    self.editor.mark_highlights_dirty();
                }
                self.editor.active_buffer_mut().clear_anchor();
                self.editor.active_buffer_mut().move_right();
                self.editor.mode = mode::Mode::Insert;
                self.editor.active_buffer_mut().begin_transaction();
            }
            CoreAction::InsertAtLineStart => {
                let is_empty = self.editor.active_buffer().current_line_is_empty();
                self.editor.active_buffer_mut().clear_anchor();
                self.editor
                    .active_buffer_mut()
                    .move_to_line_first_non_whitespace();
                self.editor.mode = mode::Mode::Insert;
                self.editor.active_buffer_mut().begin_transaction();
                if is_empty {
                    let indent = self.editor.active_buffer().indent_for_empty_line();
                    if !indent.is_empty() {
                        self.editor.active_buffer_mut().insert_text(&indent);
                        self.editor.mark_highlights_dirty();
                    }
                }
            }
            CoreAction::InsertAtLineEnd => {
                self.editor.active_buffer_mut().clear_anchor();
                self.editor.active_buffer_mut().move_to_line_end();
                let is_empty = self.editor.active_buffer().current_line_is_empty();
                self.editor.mode = mode::Mode::Insert;
                self.editor.active_buffer_mut().begin_transaction();
                if is_empty {
                    let indent = self.editor.active_buffer().indent_for_empty_line();
                    if !indent.is_empty() {
                        self.editor.active_buffer_mut().insert_text(&indent);
                        self.editor.mark_highlights_dirty();
                    }
                }
            }
            CoreAction::OpenLineBelow => {
                self.editor.active_buffer_mut().clear_anchor();
                self.editor.active_buffer_mut().move_to_line_end();
                self.editor.active_buffer_mut().begin_transaction();
                self.editor
                    .insert_newline_with_indent(self.config.tab_width);
                self.editor.mode = mode::Mode::Insert;
            }
            CoreAction::Yank => {
                // Normal mode: yank every cursor's selection (joined with `\n`)
                // when any cursor has a selection; otherwise yank current line.
                if let Some(text) = self.editor.active_buffer().selection_text_combined() {
                    let len = text.chars().count();
                    let _ = copy_to_clipboard(&text);
                    self.editor.register = Some(text);
                    self.editor.message = Some(format!("Yanked {} chars", len));
                } else {
                    let buf = self.editor.active_buffer();
                    let line = buf.cursor_line();
                    let line_text = buf.rope.line(line).to_string();
                    let _ = copy_to_clipboard(&line_text);
                    self.editor.register = Some(line_text);
                    self.editor.message = Some("Yanked line".to_string());
                }
            }
            CoreAction::SelectLine => {
                // Normal mode: select current line, enter visual
                self.editor.active_buffer_mut().select_line();
                self.editor.mode = mode::Mode::Visual;
            }
            CoreAction::ExtendLineSelection => {
                // Visual mode: extend line selection down
                self.editor.active_buffer_mut().extend_line_selection_down();
            }
            CoreAction::ExtendRight => {
                self.editor.active_buffer_mut().extend_right();
            }
            CoreAction::ExtendLeft => {
                self.editor.active_buffer_mut().extend_left();
            }
            CoreAction::ExtendUp => {
                self.editor.active_buffer_mut().extend_up();
            }
            CoreAction::ExtendDown => {
                self.editor.active_buffer_mut().extend_down();
            }
            CoreAction::ExtendToLineStart => {
                self.editor.active_buffer_mut().extend_to_line_start();
            }
            CoreAction::ExtendToLineEnd => {
                self.editor.active_buffer_mut().extend_to_line_end();
            }
            CoreAction::ExtendWordForwardShift => {
                self.editor.active_buffer_mut().extend_word_forward_shift();
            }
            CoreAction::ExtendWordBackwardShift => {
                self.editor.active_buffer_mut().extend_word_backward_shift();
            }
            CoreAction::ExtendWordForward => {
                self.editor.active_buffer_mut().extend_word_forward();
            }
            CoreAction::ExtendWordForwardEnd => {
                self.editor.active_buffer_mut().extend_word_forward_end();
            }
            CoreAction::ExtendWordBackward => {
                self.editor.active_buffer_mut().extend_word_backward();
            }
            CoreAction::ExtendLongWordForward => {
                self.editor.active_buffer_mut().extend_long_word_forward();
            }
            CoreAction::ExtendLongWordForwardEnd => {
                self.editor
                    .active_buffer_mut()
                    .extend_long_word_forward_end();
            }
            CoreAction::ExtendLongWordBackward => {
                self.editor.active_buffer_mut().extend_long_word_backward();
            }
            CoreAction::DeleteSelection => {
                // Treat overlapping/touching selections as one big selection so
                // we don't over-delete cells that two cursors both cover.
                let buf = self.editor.active_buffer();
                let ranges: Vec<(usize, usize)> = buf
                    .merged_selection_ranges()
                    .into_iter()
                    .filter(|&(s, e)| s < e)
                    .collect();

                if !ranges.is_empty() {
                    let buf = self.editor.active_buffer_mut();
                    let deleted = buf.delete_ranges(&ranges);
                    buf.clear_anchor();
                    let _ = copy_to_clipboard(&deleted);
                    self.editor.register = Some(deleted);
                    self.editor.mode = mode::Mode::Normal;
                    self.editor.mark_highlights_dirty();
                } else if self.editor.mode == mode::Mode::Visual {
                    // Visual mode with empty selections: clear and exit.
                    self.editor.active_buffer_mut().clear_anchor();
                    self.editor.mode = mode::Mode::Normal;
                } else {
                    // Normal mode: delete char under cursor (like old 'x')
                    self.editor.active_buffer_mut().delete_forward();
                    self.editor.mark_highlights_dirty();
                }
            }
            CoreAction::YankSelection => {
                if self.editor.mode == mode::Mode::Visual {
                    let buf = self.editor.active_buffer();
                    if let Some(text) = buf.selection_text_combined() {
                        let len = text.chars().count();
                        let _ = copy_to_clipboard(&text);
                        self.editor.register = Some(text);
                        self.editor.message = Some(format!("Yanked {} chars", len));
                    }
                    // Preserve selection after yank so follow-up actions can reuse it.
                    self.editor.mode = mode::Mode::Normal;
                }
            }
            CoreAction::Paste => {
                if let Some(text) = self.editor.register.clone() {
                    let buf = self.editor.active_buffer_mut();
                    let pos = buf.cursors[0];
                    buf.insert_text_at(pos, &text);
                    self.editor.mark_highlights_dirty();
                } else {
                    self.editor.message = Some("Nothing to paste".to_string());
                }
            }
            CoreAction::CollapseSelection => {
                // Drop selection, stay at cursor, back to Normal
                self.editor.active_buffer_mut().clear_anchor();
                self.editor.mode = mode::Mode::Normal;
            }
            CoreAction::WrapSelection { open, close } => {
                if let Some((start, end)) = self.editor.active_buffer().selection_range()
                    && start < end
                {
                    let was_visual = self.editor.mode == mode::Mode::Visual;
                    let open_text = open.to_string();
                    let close_text = close.to_string();
                    let buf = self.editor.active_buffer_mut();
                    buf.clear_anchor();
                    buf.begin_transaction();
                    buf.insert_text_at(end, &close_text);
                    buf.insert_text_at(start, &open_text);
                    buf.cursors[0] = start + 1;
                    buf.commit_transaction();
                    if was_visual {
                        self.editor.mode = mode::Mode::Normal;
                    }
                    self.editor.mark_highlights_dirty();
                }
            }
            CoreAction::NewBuffer => {
                self.editor.new_buffer();
            }
            CoreAction::NextBuffer => {
                self.flush_insert_transaction_if_active();
                if self.editor.next_buffer_history() {
                    self.emit_plugin_event(PluginEvent::BufferActivated {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
            }
            CoreAction::PrevBuffer => {
                self.flush_insert_transaction_if_active();
                if self.editor.prev_buffer_history() {
                    self.emit_plugin_event(PluginEvent::BufferActivated {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
            }
            CoreAction::SwitchBufferByIndex(idx) => {
                self.flush_insert_transaction_if_active();
                if !self.editor.switch_to_index(idx) {
                    self.editor.message = Some(format!("No buffer at F{}", idx + 1));
                } else {
                    self.emit_plugin_event(PluginEvent::BufferActivated {
                        doc_id: self.editor.active_buffer().id,
                    });
                }
            }
            CoreAction::Undo => {
                if self.editor.active_buffer_mut().undo() {
                    self.editor.mark_highlights_dirty();
                    if self.editor.mode == mode::Mode::Insert {
                        self.editor.active_buffer_mut().begin_transaction();
                    }
                } else {
                    self.editor.message = Some("Nothing to undo".to_string());
                }
            }
            CoreAction::Redo => {
                if self.editor.active_buffer_mut().redo() {
                    self.editor.mark_highlights_dirty();
                    if self.editor.mode == mode::Mode::Insert {
                        self.editor.active_buffer_mut().begin_transaction();
                    }
                } else {
                    self.editor.message = Some("Nothing to redo".to_string());
                }
            }
            CoreAction::SearchUpdate(pattern) => {
                self.editor.search_update(&pattern);
                self.editor.search_next();
            }
            CoreAction::SearchNext => {
                if self.editor.search.matches.is_empty() {
                    self.editor.message = Some("No search pattern".to_string());
                } else {
                    self.editor.search_next();
                    let count = self.editor.search.matches.len();
                    let idx = self.editor.search.current_match.map(|i| i + 1).unwrap_or(0);
                    self.editor.message = Some(format!("[{}/{}]", idx, count));
                }
            }
            CoreAction::SearchPrev => {
                if self.editor.search.matches.is_empty() {
                    self.editor.message = Some("No search pattern".to_string());
                } else {
                    self.editor.search_prev();
                    let count = self.editor.search.matches.len();
                    let idx = self.editor.search.current_match.map(|i| i + 1).unwrap_or(0);
                    self.editor.message = Some(format!("[{}/{}]", idx, count));
                }
            }
            CoreAction::AddCursorToNextMatch => {
                self.add_cursor_to_search_match(true);
            }
            CoreAction::AddCursorToPrevMatch => {
                self.add_cursor_to_search_match(false);
            }
            CoreAction::AddCursorToAllMatches => {
                self.add_cursor_to_all_search_matches();
            }
            CoreAction::MacroRecord(reg) => {
                self.editor.macro_recorder.start_recording(reg);
                self.editor.message = Some(format!("Recording @{}", reg));
            }
            CoreAction::MacroStop => {
                let reg = self.editor.macro_recorder.recording_register();
                self.editor.macro_recorder.stop_recording();
                if let Some(r) = reg {
                    self.editor.message = Some(format!("Recorded @{}", r));
                }
            }
            CoreAction::MacroPlay(reg) => {
                if !self.editor.macro_recorder.enter_playback() {
                    self.editor.message = Some("Macro recursion limit reached".to_string());
                } else {
                    if let Some(actions) = self.editor.macro_recorder.get(reg).cloned() {
                        self.editor.macro_recorder.set_last_played(reg);
                        for a in actions {
                            if self.dispatch(Action::Core(a)) {
                                self.editor.macro_recorder.exit_playback();
                                return true;
                            }
                        }
                    } else {
                        self.editor.message = Some(format!("Register @{} is empty", reg));
                    }
                    self.editor.macro_recorder.exit_playback();
                }
            }
            CoreAction::MacroPlayLast => {
                if let Some(reg) = self.editor.macro_recorder.last_played() {
                    return self.dispatch(Action::Core(CoreAction::MacroPlay(reg)));
                } else {
                    self.editor.message = Some("No macro has been played yet".to_string());
                }
            }
            CoreAction::RepeatLastEdit => {
                if !self.editor.dot_recorder.enter_replay() {
                    self.editor.message = Some("No edit to repeat".to_string());
                } else {
                    if let Some(actions) = self.editor.dot_recorder.last_edit() {
                        for a in actions {
                            if self.dispatch(Action::Core(a)) {
                                self.editor.dot_recorder.exit_replay();
                                return true;
                            }
                        }
                    }
                    self.editor.dot_recorder.exit_replay();
                }
            }
            CoreAction::Indent => {
                let tab_width = self.config.tab_width;
                let indent_str = " ".repeat(tab_width);
                if self.editor.mode == mode::Mode::Visual {
                    let buf = self.editor.active_buffer();
                    if let Some((sel_start, sel_end)) = buf.selection_range() {
                        let first_line = buf.rope.char_to_line(sel_start);
                        let last_line =
                            buf.rope
                                .char_to_line(if sel_end > 0 { sel_end - 1 } else { 0 });
                        let anchor = buf.selection_anchor().unwrap_or(0);
                        let cursor = buf.cursors[0];
                        let anchor_line = buf.rope.char_to_line(anchor).min(last_line);
                        let cursor_line = buf.rope.char_to_line(cursor).min(last_line);

                        let buf = self.editor.active_buffer_mut();
                        buf.begin_transaction();
                        for line in first_line..=last_line {
                            let line_start = buf.rope.line_to_char(line);
                            buf.insert_text_at(line_start, &indent_str);
                        }
                        let anchor_shift = tab_width * (anchor_line - first_line + 1);
                        let cursor_shift = tab_width * (cursor_line - first_line + 1);
                        let buf = self.editor.active_buffer_mut();
                        let new_anchor = anchor + anchor_shift;
                        let new_cursor = cursor + cursor_shift;
                        // Indent in Visual mode operates on the primary selection only.
                        buf.selections[0] =
                            Some(Selection::tail_on_forward(new_anchor, new_cursor));
                        buf.cursors[0] = new_cursor;
                        buf.commit_transaction();
                    }
                    self.editor.active_buffer_mut().clear_anchor();
                    self.editor.mode = mode::Mode::Normal;
                } else {
                    let buf = self.editor.active_buffer();
                    let cursor = buf.cursors[0];
                    let line = buf.cursor_line();
                    let line_start = buf.rope.line_to_char(line);
                    let buf = self.editor.active_buffer_mut();
                    buf.insert_text_at(line_start, &indent_str);
                    buf.cursors[0] = cursor + tab_width;
                }
                self.editor.mark_highlights_dirty();
            }
            CoreAction::Dedent => {
                let tab_width = self.config.tab_width;
                if self.editor.mode == mode::Mode::Visual {
                    let buf = self.editor.active_buffer();
                    if let Some((sel_start, sel_end)) = buf.selection_range() {
                        let first_line = buf.rope.char_to_line(sel_start);
                        let last_line =
                            buf.rope
                                .char_to_line(if sel_end > 0 { sel_end - 1 } else { 0 });
                        let anchor = buf.selection_anchor().unwrap_or(0);
                        let cursor = buf.cursors[0];
                        let anchor_line = buf.rope.char_to_line(anchor).min(last_line);
                        let cursor_line = buf.rope.char_to_line(cursor).min(last_line);

                        let mut per_line_removed = Vec::with_capacity(last_line - first_line + 1);
                        for line in first_line..=last_line {
                            let line_text = buf.rope.line(line).to_string();
                            let leading = line_text.chars().take_while(|c| *c == ' ').count();
                            per_line_removed.push(leading.min(tab_width));
                        }

                        let buf = self.editor.active_buffer_mut();
                        buf.begin_transaction();
                        for line in (first_line..=last_line).rev() {
                            let remove_count = per_line_removed[line - first_line];
                            if remove_count > 0 {
                                let line_start = buf.rope.line_to_char(line);
                                buf.delete_range(line_start, line_start + remove_count);
                            }
                        }
                        let anchor_shift: usize =
                            per_line_removed[..=(anchor_line - first_line)].iter().sum();
                        let cursor_shift: usize =
                            per_line_removed[..=(cursor_line - first_line)].iter().sum();
                        let buf = self.editor.active_buffer_mut();
                        let new_anchor = anchor.saturating_sub(anchor_shift);
                        let new_cursor = cursor.saturating_sub(cursor_shift);
                        // Dedent in Visual mode operates on the primary selection only.
                        buf.selections[0] =
                            Some(Selection::tail_on_forward(new_anchor, new_cursor));
                        buf.cursors[0] = new_cursor;
                        buf.commit_transaction();
                    }
                    self.editor.active_buffer_mut().clear_anchor();
                    self.editor.mode = mode::Mode::Normal;
                } else {
                    let buf = self.editor.active_buffer();
                    let line = buf.cursor_line();
                    let line_start = buf.rope.line_to_char(line);
                    let line_text = buf.rope.line(line).to_string();
                    let leading_spaces = line_text.chars().take_while(|c| *c == ' ').count();
                    let remove_count = leading_spaces.min(tab_width);
                    if remove_count > 0 {
                        let cursor = buf.cursors[0];
                        let buf = self.editor.active_buffer_mut();
                        buf.delete_range(line_start, line_start + remove_count);
                        buf.cursors[0] = cursor.saturating_sub(remove_count).max(line_start);
                    }
                }
                self.editor.mark_highlights_dirty();
            }
            CoreAction::AddCursorAbove => {
                self.editor.active_buffer_mut().add_cursor_above();
            }
            CoreAction::AddCursorBelow => {
                self.editor.active_buffer_mut().add_cursor_below();
            }
            CoreAction::AddCursorsToTop => {
                self.editor.active_buffer_mut().add_cursors_to_top();
            }
            CoreAction::AddCursorsToBottom => {
                self.editor.active_buffer_mut().add_cursors_to_bottom();
            }
            CoreAction::RemoveSecondaryCursors => {
                self.editor.active_buffer_mut().remove_secondary_cursors();
            }
            CoreAction::VisualExpand => {
                self.handle_visual_expand();
            }
            CoreAction::Noop => {}
        }

        if should_record_jump {
            let jump_after = self.editor.current_jump_location();
            self.record_jump_transition_if_needed(jump_before, jump_after);
        }

        if !matches!(
            action_for_plugin,
            CoreAction::MoveRight
                | CoreAction::MoveLeft
                | CoreAction::MoveDown
                | CoreAction::MoveUp
                | CoreAction::MoveToLineStart
                | CoreAction::MoveToLineEnd
                | CoreAction::MoveWordForward
                | CoreAction::MoveWordForwardEnd
                | CoreAction::MoveWordBackward
                | CoreAction::MoveWordForwardNoSelect
                | CoreAction::MoveWordBackwardNoSelect
                | CoreAction::MoveLongWordForward
                | CoreAction::MoveLongWordForwardEnd
                | CoreAction::MoveLongWordBackward
                | CoreAction::MoveToLineNumber(_)
                | CoreAction::MoveToFileStart
                | CoreAction::MoveToFileEnd
                | CoreAction::ExtendLineSelection
                | CoreAction::ExtendRight
                | CoreAction::ExtendLeft
                | CoreAction::ExtendWordForwardShift
                | CoreAction::ExtendWordBackwardShift
                | CoreAction::ExtendWordForward
                | CoreAction::ExtendWordForwardEnd
                | CoreAction::ExtendWordBackward
                | CoreAction::ExtendLongWordForward
                | CoreAction::ExtendLongWordForwardEnd
                | CoreAction::ExtendLongWordBackward
                | CoreAction::SearchNext
                | CoreAction::SearchPrev
                | CoreAction::AddCursorToNextMatch
                | CoreAction::AddCursorToPrevMatch
                | CoreAction::AddCursorToAllMatches
                | CoreAction::Noop
        ) {
            self.emit_plugin_event(PluginEvent::BufferChanged {
                doc_id: self.editor.active_buffer().id,
            });
        }
        if core_action_records_recent_edit(&action_for_plugin) {
            self.record_recent_project_edit_for_active_buffer();
        }
        if core_action_updates_git_gutter(&action_for_plugin) {
            self.queue_active_doc_git_refresh(false);
        } else if matches!(
            action_for_plugin,
            CoreAction::NewBuffer
                | CoreAction::NextBuffer
                | CoreAction::PrevBuffer
                | CoreAction::SwitchBufferByIndex(_)
        ) {
            self.queue_active_doc_git_refresh(true);
        }
        false
    }

    fn add_cursor_to_search_match(&mut self, next: bool) {
        let selection_pattern = self
            .editor
            .active_buffer()
            .selection_text()
            .filter(|text| !text.is_empty());

        if let Some(pattern) = selection_pattern {
            self.editor.search_update(&pattern);
        } else if self.editor.search.pattern.is_empty() {
            self.editor.message = Some("No selection or active search pattern".to_string());
            return;
        } else {
            let pattern = self.editor.search.pattern.clone();
            self.editor.search_update(&pattern);
        }

        if self.editor.mode == mode::Mode::Visual {
            self.editor.active_buffer_mut().clear_anchor();
            self.editor.mode = mode::Mode::Normal;
        }

        if self.editor.search.matches.is_empty() {
            self.editor.message = Some("No matches found".to_string());
            return;
        }

        let added = if next {
            self.editor.add_cursor_to_next_search_match()
        } else {
            self.editor.add_cursor_to_prev_search_match()
        };
        if !added {
            self.editor.message = Some("No more unmatched occurrences".to_string());
        }
    }

    fn add_cursor_to_all_search_matches(&mut self) {
        let selection_pattern = self
            .editor
            .active_buffer()
            .selection_text()
            .filter(|text| !text.is_empty());

        if let Some(pattern) = selection_pattern {
            self.editor.search_update(&pattern);
        } else if self.editor.search.pattern.is_empty() {
            self.editor.message = Some("No selection or active search pattern".to_string());
            return;
        } else {
            let pattern = self.editor.search.pattern.clone();
            self.editor.search_update(&pattern);
        }

        if self.editor.mode == mode::Mode::Visual {
            self.editor.active_buffer_mut().clear_anchor();
            self.editor.mode = mode::Mode::Normal;
        }

        if self.editor.search.matches.is_empty() {
            self.editor.message = Some("No matches found".to_string());
            return;
        }

        let added = self.editor.add_cursor_to_all_search_matches();
        self.editor.message = Some(if added == 0 {
            "All matches already have cursors".to_string()
        } else {
            format!("Added {} cursors", added)
        });
    }
}

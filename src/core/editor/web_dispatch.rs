//! Minimal `CoreAction` dispatcher for the browser (WASM) editor.
//!
//! The terminal app dispatches `CoreAction`s through `App::dispatch_core`
//! (`src/app/dispatch_core.rs`), which is deeply entangled with terminal-only
//! orchestration: jump-list tracking, plugin events, git-gutter refresh, macro
//! and dot-repeat recording, and a recursion path back through the full
//! `Action` pipeline. None of that is needed (or available) in the browser, so
//! the web editor uses this focused dispatcher instead.
//!
//! The actual editing primitives (rope mutation, cursor/selection movement,
//! undo) live on [`Document`]/[`Editor`] and are shared with the terminal path
//! — only the thin mode-aware glue is re-expressed here. Yank/paste use the
//! in-editor [`Editor::register`] (the browser handles the system clipboard in
//! JS); jump-list, macros, plugin events and search are intentionally omitted.

use crate::core::document::Selection;
use crate::core::mode::Mode;
use crate::input::action::CoreAction;

use super::Editor;

impl Editor {
    /// Apply a `CoreAction` to the active buffer for the browser editor.
    ///
    /// `tab_width` mirrors the terminal config value used by indent/newline
    /// logic. Returns `false` always (there is no "quit" concept in the web
    /// editor); the return type matches `App::dispatch_core` for parity.
    pub fn dispatch_core(&mut self, action: CoreAction, tab_width: usize) -> bool {
        // The browser editor lives in Insert mode permanently, so a *plain*
        // motion (Ctrl+f/b/n/p/a/e, arrows) must collapse any active selection —
        // the anchor only "sticks" when the motion is an explicit Extend* action
        // (Ctrl+Shift+f/b/n/p, Shift+arrows). The terminal keeps the old
        // Normal-mode-only behavior via its own `App::dispatch_core`.
        match action {
            CoreAction::MoveRight => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_right();
            }
            CoreAction::MoveLeft => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_left();
            }
            CoreAction::MoveDown => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_down();
            }
            CoreAction::MoveUp => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_up();
            }
            CoreAction::MoveToLineStart => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_line_start();
            }
            CoreAction::MoveToLineEnd => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_line_end();
            }
            CoreAction::MoveWordForward => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_word_forward();
            }
            CoreAction::MoveWordForwardEnd => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_word_forward_end();
            }
            CoreAction::MoveWordBackward => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_word_backward();
            }
            CoreAction::MoveWordForwardNoSelect => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_word_forward();
            }
            CoreAction::MoveWordBackwardNoSelect => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_word_backward();
            }
            CoreAction::MoveLongWordForward => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_long_word_forward();
            }
            CoreAction::MoveLongWordForwardEnd => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_long_word_forward_end();
            }
            CoreAction::MoveLongWordBackward => {
                if self.mode == Mode::Normal {
                    self.active_buffer_mut().set_anchor();
                }
                self.active_buffer_mut().move_long_word_backward();
            }
            CoreAction::MoveToLineNumber(line) => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().set_cursor_line_char(line, 0);
            }
            CoreAction::MoveToFileStart => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_file_start();
            }
            CoreAction::MoveToFileEnd => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_file_end();
            }
            CoreAction::DeleteForward => {
                self.active_buffer_mut().delete_forward();
                self.mark_highlights_dirty();
            }
            CoreAction::DeleteBackward => {
                self.active_buffer_mut().delete_backward();
                self.mark_highlights_dirty();
            }
            CoreAction::KillLine => {
                self.active_buffer_mut().kill_line();
                self.mark_highlights_dirty();
            }
            CoreAction::InsertNewline => {
                self.insert_newline_with_indent(tab_width);
            }
            CoreAction::InsertChar(c) => {
                self.active_buffer_mut().insert_char(c);
                self.mark_highlights_dirty();
            }
            CoreAction::InsertText(text) => {
                self.active_buffer_mut().paste(&text);
                self.mark_highlights_dirty();
            }
            CoreAction::ChangeMode(m) => {
                let old_mode = self.mode;
                self.mode = m;
                if old_mode == Mode::Insert && m != Mode::Insert {
                    self.active_buffer_mut().commit_transaction();
                }
                if m == Mode::Insert && old_mode != Mode::Insert {
                    self.active_buffer_mut().clear_anchor();
                    self.active_buffer_mut().begin_transaction();
                }
                if m == Mode::Visual && old_mode != Mode::Visual {
                    self.active_buffer_mut().set_anchor();
                }
                if old_mode == Mode::Visual && m != Mode::Visual {
                    self.active_buffer_mut().clear_anchor();
                }
            }
            CoreAction::InsertAfterCursor => {
                let append_newline = {
                    let buf = self.active_buffer();
                    matches!(buf.selection_range(), Some((_, end)) if end == buf.rope.len_chars())
                };
                if append_newline {
                    self.active_buffer_mut().append_newline_at_eof();
                    self.mark_highlights_dirty();
                }
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_right();
                self.mode = Mode::Insert;
                self.active_buffer_mut().begin_transaction();
            }
            CoreAction::InsertAtLineStart => {
                let is_empty = self.active_buffer().current_line_is_empty();
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_line_first_non_whitespace();
                self.mode = Mode::Insert;
                self.active_buffer_mut().begin_transaction();
                if is_empty {
                    let indent = self.active_buffer().indent_for_empty_line();
                    if !indent.is_empty() {
                        self.active_buffer_mut().insert_text(&indent);
                        self.mark_highlights_dirty();
                    }
                }
            }
            CoreAction::InsertAtLineEnd => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_line_end();
                let is_empty = self.active_buffer().current_line_is_empty();
                self.mode = Mode::Insert;
                self.active_buffer_mut().begin_transaction();
                if is_empty {
                    let indent = self.active_buffer().indent_for_empty_line();
                    if !indent.is_empty() {
                        self.active_buffer_mut().insert_text(&indent);
                        self.mark_highlights_dirty();
                    }
                }
            }
            CoreAction::OpenLineBelow => {
                self.active_buffer_mut().clear_anchor();
                self.active_buffer_mut().move_to_line_end();
                self.active_buffer_mut().begin_transaction();
                self.insert_newline_with_indent(tab_width);
                self.mode = Mode::Insert;
            }
            CoreAction::Yank => {
                if let Some(text) = self.active_buffer().selection_text_combined() {
                    let len = text.chars().count();
                    self.register = Some(text);
                    self.message = Some(format!("Yanked {} chars", len));
                } else {
                    let buf = self.active_buffer();
                    let line = buf.cursor_line();
                    let line_text = buf.rope.line(line).to_string();
                    self.register = Some(line_text);
                    self.message = Some("Yanked line".to_string());
                }
            }
            CoreAction::SelectLine => {
                self.active_buffer_mut().select_line();
                self.mode = Mode::Visual;
            }
            CoreAction::ExtendLineSelection => {
                self.active_buffer_mut().extend_line_selection_down();
            }
            CoreAction::ExtendRight => self.active_buffer_mut().extend_right(),
            CoreAction::ExtendLeft => self.active_buffer_mut().extend_left(),
            CoreAction::ExtendUp => self.active_buffer_mut().extend_up(),
            CoreAction::ExtendDown => self.active_buffer_mut().extend_down(),
            CoreAction::ExtendToLineStart => self.active_buffer_mut().extend_to_line_start(),
            CoreAction::ExtendToLineEnd => self.active_buffer_mut().extend_to_line_end(),
            CoreAction::ExtendWordForwardShift => {
                self.active_buffer_mut().extend_word_forward_shift()
            }
            CoreAction::ExtendWordBackwardShift => {
                self.active_buffer_mut().extend_word_backward_shift()
            }
            CoreAction::ExtendWordForward => self.active_buffer_mut().extend_word_forward(),
            CoreAction::ExtendWordForwardEnd => self.active_buffer_mut().extend_word_forward_end(),
            CoreAction::ExtendWordBackward => self.active_buffer_mut().extend_word_backward(),
            CoreAction::ExtendLongWordForward => {
                self.active_buffer_mut().extend_long_word_forward()
            }
            CoreAction::ExtendLongWordForwardEnd => {
                self.active_buffer_mut().extend_long_word_forward_end()
            }
            CoreAction::ExtendLongWordBackward => {
                self.active_buffer_mut().extend_long_word_backward()
            }
            CoreAction::DeleteSelection => {
                let buf = self.active_buffer();
                let ranges: Vec<(usize, usize)> = buf
                    .merged_selection_ranges()
                    .into_iter()
                    .filter(|&(s, e)| s < e)
                    .collect();
                if !ranges.is_empty() {
                    let buf = self.active_buffer_mut();
                    let deleted = buf.delete_ranges(&ranges);
                    buf.clear_anchor();
                    self.register = Some(deleted);
                    self.mode = Mode::Normal;
                    self.mark_highlights_dirty();
                } else if self.mode == Mode::Visual {
                    self.active_buffer_mut().clear_anchor();
                    self.mode = Mode::Normal;
                } else {
                    self.active_buffer_mut().delete_forward();
                    self.mark_highlights_dirty();
                }
            }
            CoreAction::YankSelection => {
                if self.mode == Mode::Visual {
                    if let Some(text) = self.active_buffer().selection_text_combined() {
                        let len = text.chars().count();
                        self.register = Some(text);
                        self.message = Some(format!("Yanked {} chars", len));
                    }
                    self.mode = Mode::Normal;
                }
            }
            CoreAction::Paste => {
                if let Some(text) = self.register.clone() {
                    self.active_buffer_mut().paste(&text);
                    self.mark_highlights_dirty();
                } else {
                    self.message = Some("Nothing to paste".to_string());
                }
            }
            CoreAction::CollapseSelection => {
                self.active_buffer_mut().clear_anchor();
                self.mode = Mode::Normal;
            }
            CoreAction::WrapSelection { open, close } => {
                if let Some((start, end)) = self.active_buffer().selection_range()
                    && start < end
                {
                    let was_visual = self.mode == Mode::Visual;
                    let open_text = open.to_string();
                    let close_text = close.to_string();
                    let buf = self.active_buffer_mut();
                    buf.clear_anchor();
                    buf.begin_transaction();
                    buf.insert_text_at(end, &close_text);
                    buf.insert_text_at(start, &open_text);
                    buf.cursors[0] = start + 1;
                    buf.commit_transaction();
                    if was_visual {
                        self.mode = Mode::Normal;
                    }
                    self.mark_highlights_dirty();
                }
            }
            CoreAction::Undo => {
                if self.active_buffer_mut().undo() {
                    self.mark_highlights_dirty();
                    if self.mode == Mode::Insert {
                        self.active_buffer_mut().begin_transaction();
                    }
                } else {
                    self.message = Some("Nothing to undo".to_string());
                }
            }
            CoreAction::Redo => {
                if self.active_buffer_mut().redo() {
                    self.mark_highlights_dirty();
                    if self.mode == Mode::Insert {
                        self.active_buffer_mut().begin_transaction();
                    }
                } else {
                    self.message = Some("Nothing to redo".to_string());
                }
            }
            CoreAction::Indent => {
                let indent_str = " ".repeat(tab_width);
                if self.mode == Mode::Visual {
                    let buf = self.active_buffer();
                    if let Some((sel_start, sel_end)) = buf.selection_range() {
                        let first_line = buf.rope.char_to_line(sel_start);
                        let last_line =
                            buf.rope
                                .char_to_line(if sel_end > 0 { sel_end - 1 } else { 0 });
                        let anchor = buf.selection_anchor().unwrap_or(0);
                        let cursor = buf.cursors[0];
                        let anchor_line = buf.rope.char_to_line(anchor).min(last_line);
                        let cursor_line = buf.rope.char_to_line(cursor).min(last_line);

                        let buf = self.active_buffer_mut();
                        buf.begin_transaction();
                        for line in first_line..=last_line {
                            let line_start = buf.rope.line_to_char(line);
                            buf.insert_text_at(line_start, &indent_str);
                        }
                        let anchor_shift = tab_width * (anchor_line - first_line + 1);
                        let cursor_shift = tab_width * (cursor_line - first_line + 1);
                        let new_anchor = anchor + anchor_shift;
                        let new_cursor = cursor + cursor_shift;
                        buf.selections[0] =
                            Some(Selection::tail_on_forward(new_anchor, new_cursor));
                        buf.cursors[0] = new_cursor;
                        buf.commit_transaction();
                    }
                    self.active_buffer_mut().clear_anchor();
                    self.mode = Mode::Normal;
                } else {
                    let buf = self.active_buffer();
                    let cursor = buf.cursors[0];
                    let line = buf.cursor_line();
                    let line_start = buf.rope.line_to_char(line);
                    let buf = self.active_buffer_mut();
                    buf.insert_text_at(line_start, &indent_str);
                    buf.cursors[0] = cursor + tab_width;
                }
                self.mark_highlights_dirty();
            }
            CoreAction::Dedent => {
                if self.mode == Mode::Visual {
                    let buf = self.active_buffer();
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

                        let buf = self.active_buffer_mut();
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
                        let new_anchor = anchor.saturating_sub(anchor_shift);
                        let new_cursor = cursor.saturating_sub(cursor_shift);
                        buf.selections[0] =
                            Some(Selection::tail_on_forward(new_anchor, new_cursor));
                        buf.cursors[0] = new_cursor;
                        buf.commit_transaction();
                    }
                    self.active_buffer_mut().clear_anchor();
                    self.mode = Mode::Normal;
                } else {
                    let buf = self.active_buffer();
                    let line = buf.cursor_line();
                    let line_start = buf.rope.line_to_char(line);
                    let line_text = buf.rope.line(line).to_string();
                    let leading_spaces = line_text.chars().take_while(|c| *c == ' ').count();
                    let remove_count = leading_spaces.min(tab_width);
                    if remove_count > 0 {
                        let cursor = buf.cursors[0];
                        let buf = self.active_buffer_mut();
                        buf.delete_range(line_start, line_start + remove_count);
                        buf.cursors[0] = cursor.saturating_sub(remove_count).max(line_start);
                    }
                }
                self.mark_highlights_dirty();
            }
            CoreAction::AddCursorAbove => {
                self.active_buffer_mut().add_cursor_above();
            }
            CoreAction::AddCursorBelow => {
                self.active_buffer_mut().add_cursor_below();
            }
            CoreAction::AddCursorsToTop => {
                self.active_buffer_mut().add_cursors_to_top();
            }
            CoreAction::AddCursorsToBottom => {
                self.active_buffer_mut().add_cursors_to_bottom();
            }
            CoreAction::RemoveSecondaryCursors => {
                self.active_buffer_mut().remove_secondary_cursors();
            }
            // Actions that depend on terminal-only orchestration (macros, dot
            // repeat, search-driven cursors, plugin/buffer switching, visual
            // expand) are not supported in the browser MVP.
            CoreAction::NewBuffer
            | CoreAction::NextBuffer
            | CoreAction::PrevBuffer
            | CoreAction::SearchUpdate(_)
            | CoreAction::SearchNext
            | CoreAction::SearchPrev
            | CoreAction::AddCursorToNextMatch
            | CoreAction::AddCursorToPrevMatch
            | CoreAction::AddCursorToAllMatches
            | CoreAction::MacroRecord(_)
            | CoreAction::MacroStop
            | CoreAction::MacroPlay(_)
            | CoreAction::MacroPlayLast
            | CoreAction::RepeatLastEdit
            | CoreAction::VisualExpand
            | CoreAction::Noop => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn editor_with(text: &str) -> Editor {
        let mut ed = Editor::new();
        ed.active_buffer_mut().rope = Rope::from_str(text);
        ed.active_buffer_mut().cursors = vec![0];
        ed
    }

    fn content(ed: &Editor) -> String {
        ed.active_buffer().rope.to_string()
    }

    #[test]
    fn plain_motion_collapses_selection_in_insert() {
        let mut ed = editor_with("hello world\nsecond line\n");
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Insert), 4);
        // Build a selection (Shift+Right style) then issue a *plain* motion.
        ed.dispatch_core(CoreAction::ExtendRight, 4);
        ed.dispatch_core(CoreAction::ExtendRight, 4);
        assert!(
            ed.active_buffer().selection_range().is_some(),
            "selection should exist after extending"
        );
        ed.dispatch_core(CoreAction::MoveDown, 4);
        assert!(
            ed.active_buffer().selection_range().is_none(),
            "plain motion must collapse the selection"
        );
        // And it must not have touched the buffer contents.
        assert_eq!(content(&ed), "hello world\nsecond line\n");
        assert!(!ed.active_buffer().dirty);
    }

    #[test]
    fn insert_char_in_insert_mode_edits_buffer() {
        let mut ed = editor_with("");
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Insert), 4);
        ed.dispatch_core(CoreAction::InsertChar('h'), 4);
        ed.dispatch_core(CoreAction::InsertChar('i'), 4);
        assert_eq!(content(&ed), "hi");
        assert_eq!(ed.mode, Mode::Insert);
    }

    #[test]
    fn delete_backward_removes_previous_char() {
        let mut ed = editor_with("ab");
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Insert), 4);
        ed.active_buffer_mut().cursors = vec![2];
        ed.dispatch_core(CoreAction::DeleteBackward, 4);
        assert_eq!(content(&ed), "a");
    }

    #[test]
    fn undo_after_insert_restores_buffer() {
        let mut ed = editor_with("");
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Insert), 4);
        ed.dispatch_core(CoreAction::InsertChar('x'), 4);
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Normal), 4);
        assert_eq!(content(&ed), "x");
        ed.dispatch_core(CoreAction::Undo, 4);
        assert_eq!(content(&ed), "");
    }

    #[test]
    fn select_line_then_yank_fills_register() {
        let mut ed = editor_with("hello\nworld\n");
        ed.dispatch_core(CoreAction::SelectLine, 4);
        assert_eq!(ed.mode, Mode::Visual);
        ed.dispatch_core(CoreAction::YankSelection, 4);
        assert_eq!(ed.mode, Mode::Normal);
        assert!(ed.register.as_deref().unwrap_or("").contains("hello"));
    }

    #[test]
    fn indent_in_normal_mode_adds_leading_spaces() {
        let mut ed = editor_with("code\n");
        ed.dispatch_core(CoreAction::Indent, 2);
        assert!(content(&ed).starts_with("  code"));
    }

    #[test]
    fn paste_inserts_register_contents() {
        let mut ed = editor_with("");
        ed.register = Some("abc".to_string());
        ed.dispatch_core(CoreAction::Paste, 4);
        assert_eq!(content(&ed), "abc");
    }

    #[test]
    fn wrap_selection_surrounds_in_insert_mode_without_leaving_it() {
        let mut ed = editor_with("foo bar\n");
        ed.dispatch_core(CoreAction::ChangeMode(Mode::Insert), 4);
        // Select "foo" (offsets 0..3) the way JS does for auto-surround.
        ed.active_buffer_mut().selections = vec![Some(Selection::tail_on_forward(0, 3))];
        ed.active_buffer_mut().cursors = vec![3];
        ed.dispatch_core(
            CoreAction::WrapSelection {
                open: '(',
                close: ')',
            },
            4,
        );
        assert_eq!(content(&ed), "(foo) bar\n");
        // Auto-surround must keep the editor in Insert mode.
        assert_eq!(ed.mode, Mode::Insert);
    }
}

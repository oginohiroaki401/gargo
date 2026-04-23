use super::*;

impl Document {
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    pub fn selection_anchor(&self) -> Option<usize> {
        self.selection.map(|s| s.anchor)
    }

    fn set_anchor_with_display(&mut self, cursor_display: SelectionCursorDisplay) {
        self.selection = Some(Selection {
            anchor: self.cursors[0],
            head: self.cursors[0],
            cursor_display,
        });
    }

    pub fn set_anchor(&mut self) {
        self.set_anchor_with_display(SelectionCursorDisplay::TailOnForward);
    }

    pub fn set_anchor_for_shift_extend(&mut self) {
        self.set_anchor_with_display(SelectionCursorDisplay::Head);
    }

    pub fn clear_anchor(&mut self) {
        if let Some(selection) = self.selection
            && matches!(
                selection.cursor_display,
                SelectionCursorDisplay::TailOnForward
            )
            && selection.head > selection.anchor
        {
            // Forward selection: all cursors are "one past" their display
            // position (exclusive end).  Adjust every cursor back by one so
            // that subsequent motions start from the displayed position.
            for cursor in &mut self.cursors {
                *cursor = cursor.saturating_sub(1);
            }
        }
        self.selection = None;
        self.sort_and_dedup_cursors();
    }

    /// Returns the half-open selection range `[start, end)`.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let start = selection.anchor.min(selection.head);
        let end = selection
            .anchor
            .max(selection.head)
            .min(self.rope.len_chars());
        Some((start, end))
    }

    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        Some(self.rope.slice(start..end).to_string())
    }

    /// Select the word (or whitespace/punctuation run) at `pos`.
    /// Newlines are hard boundaries so a click on EOL whitespace stays on its line.
    /// No-op when the document is empty or `pos` lands on a newline.
    pub fn select_word_at(&mut self, pos: usize) {
        let len = self.rope.len_chars();
        if len == 0 {
            return;
        }
        let pos = pos.min(len.saturating_sub(1));
        let pivot = self.rope.char(pos);
        if pivot == '\n' {
            return;
        }
        let cls = char_class(pivot);
        let mut start = pos;
        while start > 0 {
            let c = self.rope.char(start - 1);
            if c == '\n' || char_class(c) != cls {
                break;
            }
            start -= 1;
        }
        let mut end = pos;
        while end < len {
            let c = self.rope.char(end);
            if c == '\n' || char_class(c) != cls {
                break;
            }
            end += 1;
        }
        if start == end {
            return;
        }
        self.selection = Some(Selection::tail_on_forward(start, end));
        self.cursors[0] = end;
    }

    /// Select the current line as a linewise span:
    /// includes trailing newline when present.
    pub fn select_line(&mut self) {
        let line = self.cursor_line();
        let line_start = self.rope.line_to_char(line);
        let line_end = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        };
        let head = line_end;
        self.selection = Some(Selection::tail_on_forward(line_start, head));
        // Head is one-past-the-end; display cursor shows the last selected char.
        self.cursors[0] = head;
    }

    /// Extend line selection down by one line. Keeps anchor, moves cursor to
    /// end of next line.
    pub fn extend_line_selection_down(&mut self) {
        let line = self.display_cursor_line();
        if line + 1 < self.rope.len_lines() {
            let next_line = line + 1;
            let next_end = if next_line + 1 < self.rope.len_lines() {
                self.rope.line_to_char(next_line + 1)
            } else {
                self.rope.len_chars()
            };
            self.cursors[0] = next_end;
            self.sync_selection_head();
        }
    }

    fn ensure_anchor_for_extend(&mut self) {
        if self.selection.is_none() {
            self.set_anchor();
        }
    }

    fn ensure_anchor_for_shift_extend(&mut self) {
        if self.selection.is_none() {
            self.set_anchor_for_shift_extend();
            return;
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.cursor_display = SelectionCursorDisplay::Head;
        }
    }

    pub fn extend_right(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_right();
    }

    pub fn extend_left(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_left();
    }

    pub fn extend_word_forward(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_forward_impl(false);
    }

    pub fn extend_word_forward_shift(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_word_forward_impl(false);
    }

    pub fn extend_word_backward_shift(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_word_backward_impl(false);
    }

    pub fn extend_word_forward_end(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_forward_end_impl(false);
    }

    pub fn extend_word_backward(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_backward_impl(false);
    }

    pub fn extend_long_word_forward(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_forward_impl(true);
    }

    pub fn extend_long_word_forward_end(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_forward_end_impl(true);
    }

    pub fn extend_long_word_backward(&mut self) {
        self.ensure_anchor_for_extend();
        self.move_word_backward_impl(true);
    }
}

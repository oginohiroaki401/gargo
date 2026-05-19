use super::*;

impl Document {
    /// True if any cursor has an active selection.
    pub fn has_selection(&self) -> bool {
        self.selections.iter().any(|s| s.is_some())
    }

    /// True if the primary cursor (index 0) has an active selection.
    pub fn primary_has_selection(&self) -> bool {
        self.selections[0].is_some()
    }

    /// The primary cursor's selection, if any.
    pub fn primary_selection(&self) -> Option<Selection> {
        self.selections[0]
    }

    /// Primary cursor's selection anchor, if any.
    pub fn selection_anchor(&self) -> Option<usize> {
        self.selections[0].map(|s| s.anchor)
    }

    fn set_anchor_with_display(&mut self, cursor_display: SelectionCursorDisplay) {
        for i in 0..self.cursors.len() {
            self.selections[i] = Some(Selection {
                anchor: self.cursors[i],
                head: self.cursors[i],
                cursor_display,
            });
        }
    }

    pub fn set_anchor(&mut self) {
        self.set_anchor_with_display(SelectionCursorDisplay::TailOnForward);
    }

    pub fn set_anchor_for_shift_extend(&mut self) {
        self.set_anchor_with_display(SelectionCursorDisplay::Head);
    }

    pub fn clear_anchor(&mut self) {
        // Per-cursor: forward TailOnForward selections render with the head one
        // past the displayed cursor, so step each such cursor back by one when
        // collapsing so subsequent motions begin at the displayed position.
        for i in 0..self.cursors.len() {
            if let Some(sel) = self.selections[i]
                && matches!(sel.cursor_display, SelectionCursorDisplay::TailOnForward)
                && sel.head > sel.anchor
            {
                self.cursors[i] = self.cursors[i].saturating_sub(1);
            }
            self.selections[i] = None;
        }
        self.sort_and_dedup_cursors();
    }

    /// Returns the half-open selection range `[start, end)` for the primary cursor.
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let selection = self.selections[0]?;
        let start = selection.anchor.min(selection.head);
        let end = selection
            .anchor
            .max(selection.head)
            .min(self.rope.len_chars());
        Some((start, end))
    }

    /// Returns the primary cursor's selection text, if any.
    pub fn selection_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        Some(self.rope.slice(start..end).to_string())
    }

    /// Returns every cursor's selection range, in cursor order. Cursors with no
    /// active selection are skipped. Zero-width ranges (`anchor == head`) are
    /// included so callers can distinguish "anchor set but empty" from "no
    /// selection".
    pub fn selection_ranges(&self) -> Vec<(usize, usize)> {
        let len = self.rope.len_chars();
        self.selections
            .iter()
            .filter_map(|sel| {
                sel.map(|s| {
                    let start = s.anchor.min(s.head);
                    let end = s.anchor.max(s.head).min(len);
                    (start, end)
                })
            })
            .collect()
    }

    /// Returns text for every cursor's selection (non-empty ranges only),
    /// in cursor order.
    pub fn selection_texts(&self) -> Vec<String> {
        self.selection_ranges()
            .into_iter()
            .filter(|(s, e)| s < e)
            .map(|(s, e)| self.rope.slice(s..e).to_string())
            .collect()
    }

    /// Returns selection ranges with overlapping and touching ranges merged
    /// into single unions. The underlying per-cursor selections are NOT
    /// modified — callers that need each cursor's individual anchor (e.g.
    /// the `extend_*` motions) continue to read `self.selections` directly.
    pub fn merged_selection_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = self.selection_ranges();
        if ranges.len() <= 1 {
            return ranges;
        }
        ranges.sort_by_key(|&(s, _)| s);
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for (s, e) in ranges {
            match merged.last_mut() {
                // Touching counts as overlap (`s <= prev_end`).
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        merged
    }

    /// Builds the register/clipboard text for a yank.
    ///
    /// A single (or fully merged) selection is returned verbatim, so a plain
    /// visual yank is unchanged. With multiple disjoint selections — a
    /// multi-cursor copy — each selection becomes its own line: one trailing
    /// newline is trimmed from each segment and the segments are joined with
    /// `\n`. That keeps each cursor's text on a distinct line so a later
    /// multi-cursor paste can distribute one line back to each cursor.
    /// Overlapping ranges still contribute their union exactly once.
    pub fn selection_text_combined(&self) -> Option<String> {
        let parts: Vec<String> = self
            .merged_selection_ranges()
            .into_iter()
            .filter(|&(s, e)| s < e)
            .map(|(s, e)| self.rope.slice(s..e).to_string())
            .collect();
        match parts.len() {
            0 => None,
            1 => parts.into_iter().next(),
            _ => Some(
                parts
                    .iter()
                    .map(|p| p.strip_suffix('\n').unwrap_or(p))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    /// Select the word (or whitespace/punctuation run) at `pos`.
    /// Newlines are hard boundaries so a click on EOL whitespace stays on its line.
    /// No-op when the document is empty or `pos` lands on a newline.
    /// Collapses to a single cursor (mouse semantics).
    pub fn select_word_at(&mut self, pos: usize) {
        if let Some((start, end)) = super::expand::word_range_at(&self.rope, pos) {
            self.cursors = vec![end];
            self.selections = vec![Some(Selection::tail_on_forward(start, end))];
        }
    }

    /// Select the current line as a linewise span:
    /// includes trailing newline when present. Operates on every cursor.
    pub fn select_line(&mut self) {
        let total_lines = self.rope.len_lines();
        let total_chars = self.rope.len_chars();
        for i in 0..self.cursors.len() {
            let line = self.rope.char_to_line(self.cursors[i]);
            let line_start = self.rope.line_to_char(line);
            let line_end = if line + 1 < total_lines {
                self.rope.line_to_char(line + 1)
            } else {
                total_chars
            };
            // Head is one-past-the-end; display cursor shows the last selected char.
            self.selections[i] = Some(Selection::tail_on_forward(line_start, line_end));
            self.cursors[i] = line_end;
        }
        self.sort_and_dedup_cursors();
    }

    /// Extend line selection down by one line for every cursor. Keeps each
    /// anchor, moves each cursor to end of the next line.
    pub fn extend_line_selection_down(&mut self) {
        let total_lines = self.rope.len_lines();
        let total_chars = self.rope.len_chars();
        // Use each cursor's *display* line (one back when forward TailOnForward).
        let new_positions: Vec<usize> = (0..self.cursors.len())
            .map(|i| {
                let raw = self.cursors[i];
                let display = if let Some(sel) = self.selections[i] {
                    if matches!(sel.cursor_display, SelectionCursorDisplay::TailOnForward)
                        && sel.head > sel.anchor
                    {
                        raw.saturating_sub(1)
                    } else {
                        raw
                    }
                } else {
                    raw
                };
                let line = self.rope.char_to_line(display);
                if line + 1 < total_lines {
                    let next_line = line + 1;
                    if next_line + 1 < total_lines {
                        self.rope.line_to_char(next_line + 1)
                    } else {
                        total_chars
                    }
                } else {
                    raw
                }
            })
            .collect();
        for (i, pos) in new_positions.into_iter().enumerate() {
            self.cursors[i] = pos;
        }
        self.sync_selection_head();
    }

    fn ensure_anchor_for_extend(&mut self) {
        for i in 0..self.cursors.len() {
            if self.selections[i].is_none() {
                self.selections[i] = Some(Selection {
                    anchor: self.cursors[i],
                    head: self.cursors[i],
                    cursor_display: SelectionCursorDisplay::TailOnForward,
                });
            }
        }
    }

    fn ensure_anchor_for_shift_extend(&mut self) {
        for i in 0..self.cursors.len() {
            match self.selections[i].as_mut() {
                None => {
                    self.selections[i] = Some(Selection {
                        anchor: self.cursors[i],
                        head: self.cursors[i],
                        cursor_display: SelectionCursorDisplay::Head,
                    });
                }
                Some(sel) => {
                    sel.cursor_display = SelectionCursorDisplay::Head;
                }
            }
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

    pub fn extend_up(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_up();
    }

    pub fn extend_down(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_down();
    }

    pub fn extend_to_line_start(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_to_line_start();
    }

    pub fn extend_to_line_end(&mut self) {
        self.ensure_anchor_for_shift_extend();
        self.move_to_line_end();
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

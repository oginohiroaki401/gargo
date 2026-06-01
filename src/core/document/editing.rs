use std::rc::Rc;

use super::*;

impl Document {
    pub fn insert_char(&mut self, c: char) {
        // Multi-cursor insert: process from highest to lowest position
        let mut positions: Vec<usize> = self.cursors.clone();
        positions.sort_by(|a, b| b.cmp(a)); // Descending order

        let cursors_before = self.cursors.clone();
        let char_len = c.len_utf8();

        // Begin transaction to group all multi-cursor edits together
        // Only commit if we started the transaction (not if one was already open externally)
        let started_transaction = self.history.begin_transaction(&cursors_before);

        for pos in &positions {
            let byte_pos = self.rope.char_to_byte(*pos);
            let line = self.rope.char_to_line(*pos);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;

            self.rope.insert_char(*pos, c);

            let new_line = if c == '\n' { line + 1 } else { line };
            let new_col_byte = if c == '\n' { 0 } else { col_byte + char_len };
            let edit_event = EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos,
                new_end_byte: byte_pos + char_len,
                start_position: (line, col_byte),
                old_end_position: (line, col_byte),
                new_end_position: (new_line, new_col_byte),
            };
            self.pending_edits.push(edit_event.clone());

            // Record history for undo - record ALL cursor edits
            self.history.record(
                EditRecord {
                    char_offset: *pos,
                    old_text: Rc::from(""),
                    new_text: Rc::from(c.to_string()),
                    edit_event,
                },
                &cursors_before,
                &cursors_before, // Will be updated after cursor adjustment
            );
        }

        // Adjust all cursor positions: each cursor moves forward by 1
        // plus the number of cursors that were before it
        let original_positions: Vec<usize> = self.cursors.clone();
        for (i, cursor) in self.cursors.iter_mut().enumerate() {
            let cursors_before_count = original_positions
                .iter()
                .filter(|&&p| p < original_positions[i])
                .count();
            *cursor = original_positions[i] + 1 + cursors_before_count;
        }

        // Update the cursors_after and commit only if we started the transaction
        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        self.sort_and_dedup_cursors();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let text = Self::normalize_newlines_for_insert(text);
        let text = text.as_ref();
        let char_count = if text.is_ascii() {
            text.len()
        } else {
            text.chars().count()
        };

        // Multi-cursor insert: process from highest to lowest position
        let mut positions: Vec<usize> = self.cursors.clone();
        positions.sort_by(|a, b| b.cmp(a)); // Descending order

        let cursors_before = self.cursors.clone();

        // Begin transaction to group all multi-cursor edits together
        // Only commit if we started the transaction (not if one was already open externally)
        let started_transaction = self.history.begin_transaction(&cursors_before);

        let empty_rc: Rc<str> = Rc::from("");
        let text_rc: Rc<str> = Rc::from(text);

        for pos in &positions {
            let byte_pos = self.rope.char_to_byte(*pos);
            let line = self.rope.char_to_line(*pos);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;

            self.rope.insert(*pos, text);

            let new_end_pos = self.compute_end_position(line, col_byte, text);
            let edit_event = EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos,
                new_end_byte: byte_pos + text.len(),
                start_position: (line, col_byte),
                old_end_position: (line, col_byte),
                new_end_position: new_end_pos,
            };
            self.pending_edits.push(edit_event.clone());

            // Record history for undo - record ALL cursor edits
            self.history.record(
                EditRecord {
                    char_offset: *pos,
                    old_text: empty_rc.clone(),
                    new_text: text_rc.clone(),
                    edit_event,
                },
                &cursors_before,
                &cursors_before, // Will be updated after cursor adjustment
            );
        }

        // Adjust all cursor positions
        let original_positions: Vec<usize> = self.cursors.clone();
        for (i, cursor) in self.cursors.iter_mut().enumerate() {
            let cursors_before_count = original_positions
                .iter()
                .filter(|&&p| p < original_positions[i])
                .count();
            *cursor = original_positions[i] + char_count + (cursors_before_count * char_count);
        }

        // Update the cursors_after and commit only if we started the transaction
        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        self.sort_and_dedup_cursors();
    }

    /// Paste `text` honoring multi-cursor semantics.
    ///
    /// With multiple cursors, if the number of lines in `text` matches the
    /// cursor count, each line is inserted at the corresponding cursor
    /// (first line into the top-most cursor, and so on). Otherwise the whole
    /// `text` is inserted at every cursor (same as [`insert_text`]).
    ///
    /// [`insert_text`]: Document::insert_text
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = Self::normalize_newlines_for_insert(text);
        let normalized = normalized.as_ref();
        let lines: Vec<&str> = normalized.split('\n').collect();
        if self.cursors.len() > 1 && lines.len() == self.cursors.len() {
            self.insert_lines_per_cursor(&lines);
        } else {
            self.insert_text(normalized);
        }
    }

    /// Insert `lines[i]` at `cursors[i]`. `lines.len()` must equal the cursor
    /// count. Cursors are kept sorted/unique, so `lines[0]` lands at the
    /// top-most cursor.
    fn insert_lines_per_cursor(&mut self, lines: &[&str]) {
        debug_assert_eq!(lines.len(), self.cursors.len());

        let cursors_before = self.cursors.clone();
        let started_transaction = self.history.begin_transaction(&cursors_before);
        let empty_rc: Rc<str> = Rc::from("");

        // Insert from the highest position down so earlier offsets stay valid.
        let mut order: Vec<usize> = (0..self.cursors.len()).collect();
        order.sort_by(|&a, &b| self.cursors[b].cmp(&self.cursors[a]));

        for &i in &order {
            let seg = lines[i];
            if seg.is_empty() {
                continue;
            }
            let pos = self.cursors[i];
            let byte_pos = self.rope.char_to_byte(pos);
            let line = self.rope.char_to_line(pos);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;

            self.rope.insert(pos, seg);

            let new_end_pos = self.compute_end_position(line, col_byte, seg);
            let edit_event = EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos,
                new_end_byte: byte_pos + seg.len(),
                start_position: (line, col_byte),
                old_end_position: (line, col_byte),
                new_end_position: new_end_pos,
            };
            self.pending_edits.push(edit_event.clone());

            self.history.record(
                EditRecord {
                    char_offset: pos,
                    old_text: empty_rc.clone(),
                    new_text: Rc::from(seg),
                    edit_event,
                },
                &cursors_before,
                &cursors_before, // Updated after cursor adjustment below.
            );
        }

        // Each cursor moves to the end of its own inserted segment, shifted by
        // the total length of segments inserted at earlier (lower) positions.
        // Cursors are sorted ascending and unique, so that is simply a prefix
        // sum over the segment lengths.
        let original: Vec<usize> = self.cursors.clone();
        let mut prefix = 0usize;
        for i in 0..self.cursors.len() {
            let seg_len = lines[i].chars().count();
            self.cursors[i] = original[i] + prefix + seg_len;
            prefix += seg_len;
        }

        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        self.sort_and_dedup_cursors();
    }

    #[cfg(test)]
    pub fn insert_newline(&mut self) {
        // Use insert_char for newline to get multi-cursor support
        self.insert_char('\n');
    }

    pub fn delete_forward(&mut self) {
        let len = self.rope.len_chars();
        // Multi-cursor delete: process from highest to lowest position
        let mut positions: Vec<usize> =
            self.cursors.iter().filter(|&&p| p < len).cloned().collect();
        if positions.is_empty() {
            return;
        }
        positions.sort_by(|a, b| b.cmp(a)); // Descending order

        let cursors_before = self.cursors.clone();

        // Begin transaction to group all multi-cursor edits together
        // Only commit if we started the transaction (not if one was already open externally)
        let started_transaction = self.history.begin_transaction(&cursors_before);

        for pos in &positions {
            let byte_pos = self.rope.char_to_byte(*pos);
            let line = self.rope.char_to_line(*pos);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;
            let ch = self.rope.char(*pos);
            let char_len = ch.len_utf8();

            let old_end_line = if ch == '\n' { line + 1 } else { line };
            let old_end_col = if ch == '\n' { 0 } else { col_byte + char_len };

            self.rope.remove(*pos..*pos + 1);

            let edit_event = EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos + char_len,
                new_end_byte: byte_pos,
                start_position: (line, col_byte),
                old_end_position: (old_end_line, old_end_col),
                new_end_position: (line, col_byte),
            };
            self.pending_edits.push(edit_event.clone());

            // Record history for undo - record ALL cursor edits
            self.history.record(
                EditRecord {
                    char_offset: *pos,
                    old_text: Rc::from(ch.to_string()),
                    new_text: Rc::from(""),
                    edit_event,
                },
                &cursors_before,
                &cursors_before, // Will be updated after cursor adjustment
            );
        }

        // Adjust cursor positions: cursors after deleted positions shift back
        let original_positions: Vec<usize> = self.cursors.clone();
        for (i, cursor) in self.cursors.iter_mut().enumerate() {
            let deleted_before = positions
                .iter()
                .filter(|&&p| p < original_positions[i])
                .count();
            *cursor = original_positions[i].saturating_sub(deleted_before);
        }

        // Update the cursors_after and commit only if we started the transaction
        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        self.sort_and_dedup_cursors();
    }

    pub fn delete_backward(&mut self) {
        // Multi-cursor delete: process from highest to lowest position
        // Each cursor deletes the character before it
        let mut positions: Vec<usize> = self.cursors.iter().filter(|&&p| p > 0).cloned().collect();
        if positions.is_empty() {
            return;
        }
        positions.sort_by(|a, b| b.cmp(a)); // Descending order

        let cursors_before = self.cursors.clone();

        // Begin transaction to group all multi-cursor edits together
        // Only commit if we started the transaction (not if one was already open externally)
        let started_transaction = self.history.begin_transaction(&cursors_before);

        for pos in &positions {
            let delete_pos = *pos - 1;
            let byte_pos = self.rope.char_to_byte(delete_pos);
            let line = self.rope.char_to_line(delete_pos);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;
            let ch = self.rope.char(delete_pos);
            let char_len = ch.len_utf8();

            let old_end_line = if ch == '\n' { line + 1 } else { line };
            let old_end_col = if ch == '\n' { 0 } else { col_byte + char_len };

            self.rope.remove(delete_pos..delete_pos + 1);

            let edit_event = EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos + char_len,
                new_end_byte: byte_pos,
                start_position: (line, col_byte),
                old_end_position: (old_end_line, old_end_col),
                new_end_position: (line, col_byte),
            };
            self.pending_edits.push(edit_event.clone());

            // Record history for undo - record ALL cursor edits
            self.history.record(
                EditRecord {
                    char_offset: delete_pos,
                    old_text: Rc::from(ch.to_string()),
                    new_text: Rc::from(""),
                    edit_event,
                },
                &cursors_before,
                &cursors_before, // Will be updated after cursor adjustment
            );
        }

        // Adjust cursor positions: each cursor moves back by 1 plus the number of
        // deletions that happened before it
        let original_positions: Vec<usize> = self.cursors.clone();
        for (i, cursor) in self.cursors.iter_mut().enumerate() {
            if original_positions[i] > 0 {
                let deletions_before = positions
                    .iter()
                    .filter(|&&p| p <= original_positions[i])
                    .count();
                *cursor = original_positions[i].saturating_sub(deletions_before);
            }
        }

        // Update the cursors_after and commit only if we started the transaction
        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        self.sort_and_dedup_cursors();
    }

    pub fn kill_line(&mut self) {
        // kill_line operates on primary cursor only for now
        let cursors_before = self.cursors.clone();
        let line = self.cursor_line();
        let line_start = self.rope.line_to_char(line);
        let line_len = self.line_len(line);
        let line_end = line_start + line_len;

        if self.cursors[0] == line_end {
            if self.cursors[0] < self.rope.len_chars() {
                // Deleting the newline character
                let byte_pos = self.rope.char_to_byte(self.cursors[0]);
                let line_byte_start = self.rope.line_to_byte(line);
                let col_byte = byte_pos - line_byte_start;

                self.rope.remove(self.cursors[0]..self.cursors[0] + 1);
                self.dirty = true;
                self.bump_version();

                let edit_event = EditEvent {
                    start_byte: byte_pos,
                    old_end_byte: byte_pos + 1,
                    new_end_byte: byte_pos,
                    start_position: (line, col_byte),
                    old_end_position: (line + 1, 0),
                    new_end_position: (line, col_byte),
                };
                self.pending_edits.push(edit_event.clone());

                self.history.record(
                    EditRecord {
                        char_offset: self.cursors[0],
                        old_text: Rc::from("\n"),
                        new_text: Rc::from(""),
                        edit_event,
                    },
                    &cursors_before,
                    &self.cursors,
                );
            }
        } else {
            // Delete from cursor to end of line
            let deleted: String = self.rope.slice(self.cursors[0]..line_end).to_string();
            let start_byte = self.rope.char_to_byte(self.cursors[0]);
            let end_byte = self.rope.char_to_byte(line_end);
            let line_byte_start = self.rope.line_to_byte(line);
            let start_col_byte = start_byte - line_byte_start;
            let end_col_byte = end_byte - line_byte_start;

            self.rope.remove(self.cursors[0]..line_end);
            self.dirty = true;
            self.bump_version();

            let edit_event = EditEvent {
                start_byte,
                old_end_byte: end_byte,
                new_end_byte: start_byte,
                start_position: (line, start_col_byte),
                old_end_position: (line, end_col_byte),
                new_end_position: (line, start_col_byte),
            };
            self.pending_edits.push(edit_event.clone());

            self.history.record(
                EditRecord {
                    char_offset: self.cursors[0],
                    old_text: Rc::from(deleted),
                    new_text: Rc::from(""),
                    edit_event,
                },
                &cursors_before,
                &self.cursors,
            );
        }
    }

    // -------------------------------------------------------
    // Transaction delegation
    // -------------------------------------------------------

    pub fn begin_transaction(&mut self) {
        self.history.begin_transaction(&self.cursors);
    }

    pub fn commit_transaction(&mut self) {
        self.history.commit_transaction();
    }

    pub fn flush_transaction(&mut self) {
        self.history.flush_transaction();
    }

    // -------------------------------------------------------
    // Undo / Redo
    // -------------------------------------------------------

    /// Undo the most recent edit transaction. Returns true if something was undone.
    pub fn undo(&mut self) -> bool {
        self.history.flush_transaction();
        let tx = match self.history.pop_undo() {
            Some(tx) => tx,
            None => return false,
        };

        // Apply records in reverse order
        for record in tx.records.iter().rev() {
            if !record.new_text.is_empty() {
                // This was an insertion -- remove it
                let char_count = record.new_text.chars().count();
                self.rope
                    .remove(record.char_offset..record.char_offset + char_count);
            }
            if !record.old_text.is_empty() {
                // This was a deletion -- re-insert the old text
                self.rope.insert(record.char_offset, &record.old_text);
            }

            // Generate EditEvent for tree-sitter (reversed direction)
            let byte_pos = self.rope.char_to_byte(record.char_offset);
            let line = self.rope.char_to_line(record.char_offset);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;

            let old_text_bytes = record.new_text.len(); // what was new is now old
            let new_text_bytes = record.old_text.len(); // what was old is now new

            // Compute end positions for old (what was inserted, now being removed)
            let old_end_pos = self.compute_end_position(line, col_byte, &record.new_text);
            // Compute end positions for new (what was deleted, now being re-inserted)
            let new_end_pos = self.compute_end_position(line, col_byte, &record.old_text);

            self.pending_edits.push(EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos + old_text_bytes,
                new_end_byte: byte_pos + new_text_bytes,
                start_position: (line, col_byte),
                old_end_position: old_end_pos,
                new_end_position: new_end_pos,
            });
        }

        // Undo restores all cursor positions from before the edit.
        // History does not track selections; reset to a clean (None) state for
        // every restored cursor.
        self.cursors = tx.cursors_before.clone();
        self.selections = vec![None; self.cursors.len()];
        self.sync_selection_head();
        self.dirty = true;
        self.bump_version();

        // Push to redo stack
        self.history.push_redo(tx);
        true
    }

    /// Redo the most recently undone transaction. Returns true if something was redone.
    pub fn redo(&mut self) -> bool {
        self.history.flush_transaction();
        let tx = match self.history.pop_redo() {
            Some(tx) => tx,
            None => return false,
        };

        // Re-apply records in forward order
        for record in tx.records.iter() {
            if !record.old_text.is_empty() {
                // Remove the old text that was re-inserted by undo
                let char_count = record.old_text.chars().count();
                self.rope
                    .remove(record.char_offset..record.char_offset + char_count);
            }
            if !record.new_text.is_empty() {
                // Re-insert the new text
                self.rope.insert(record.char_offset, &record.new_text);
            }

            // Generate EditEvent for tree-sitter
            let byte_pos = self.rope.char_to_byte(record.char_offset);
            let line = self.rope.char_to_line(record.char_offset);
            let line_byte_start = self.rope.line_to_byte(line);
            let col_byte = byte_pos - line_byte_start;

            let old_text_bytes = record.old_text.len();
            let new_text_bytes = record.new_text.len();

            let old_end_pos = self.compute_end_position(line, col_byte, &record.old_text);
            let new_end_pos = self.compute_end_position(line, col_byte, &record.new_text);

            self.pending_edits.push(EditEvent {
                start_byte: byte_pos,
                old_end_byte: byte_pos + old_text_bytes,
                new_end_byte: byte_pos + new_text_bytes,
                start_position: (line, col_byte),
                old_end_position: old_end_pos,
                new_end_position: new_end_pos,
            });
        }

        // Redo restores all cursor positions from after the edit. History
        // does not track selections; reset to a clean (None) state.
        self.cursors = tx.cursors_after.clone();
        self.selections = vec![None; self.cursors.len()];
        self.sync_selection_head();
        self.dirty = true;
        self.bump_version();

        // Push back to undo stack
        self.history.push_undo(tx);
        true
    }

    /// Delete a char range `[start, end)` with full undo/redo recording.
    /// Returns the deleted text.
    pub fn delete_range(&mut self, start: usize, end: usize) -> String {
        if start >= end || start >= self.rope.len_chars() {
            return String::new();
        }
        let end = end.min(self.rope.len_chars());
        let cursors_before = self.cursors.clone();
        let deleted: String = self.rope.slice(start..end).to_string();

        let start_byte = self.rope.char_to_byte(start);
        let end_byte = self.rope.char_to_byte(end);
        let start_line = self.rope.char_to_line(start);
        let start_line_byte = self.rope.line_to_byte(start_line);
        let start_col_byte = start_byte - start_line_byte;

        let end_line = self.rope.char_to_line(end);
        let end_line_byte = self.rope.line_to_byte(end_line);
        let end_col_byte = end_byte - end_line_byte;

        self.rope.remove(start..end);
        self.dirty = true;
        self.bump_version();

        // Place cursor at start of deleted range (single cursor after delete_range)
        self.cursors = vec![start.min(self.rope.len_chars())];
        self.selections = vec![None];

        let edit_event = EditEvent {
            start_byte,
            old_end_byte: end_byte,
            new_end_byte: start_byte,
            start_position: (start_line, start_col_byte),
            old_end_position: (end_line, end_col_byte),
            new_end_position: (start_line, start_col_byte),
        };
        self.pending_edits.push(edit_event.clone());

        self.history.record(
            EditRecord {
                char_offset: start,
                old_text: Rc::from(deleted.as_str()),
                new_text: Rc::from(""),
                edit_event,
            },
            &cursors_before,
            &self.cursors,
        );

        deleted
    }

    /// Delete multiple char ranges in one transaction, returning each deleted
    /// segment joined with `\n` (Helix-style). Cursor count is preserved:
    /// every cursor is placed at the start of its corresponding deleted range,
    /// or shifted to account for earlier deletions. `ranges` is assumed to be
    /// non-overlapping and aligned with `self.cursors` (one range per cursor
    /// that has a selection). Cursors without a corresponding range are
    /// shifted to account for earlier deletions only.
    pub fn delete_ranges(&mut self, ranges: &[(usize, usize)]) -> String {
        if ranges.is_empty() {
            return String::new();
        }

        // Sort ranges by start descending so deletions don't invalidate later
        // offsets. Collect deleted text in original (ascending) order for the
        // returned clipboard string.
        let mut indexed: Vec<(usize, (usize, usize))> =
            ranges.iter().copied().enumerate().collect();
        indexed.sort_by(|a, b| b.1.0.cmp(&a.1.0));

        let cursors_before = self.cursors.clone();
        let started_transaction = self.history.begin_transaction(&cursors_before);

        let mut deleted_segments: Vec<(usize, String)> = Vec::with_capacity(ranges.len());

        for &(orig_idx, (start, end)) in &indexed {
            if start >= end || start >= self.rope.len_chars() {
                deleted_segments.push((orig_idx, String::new()));
                continue;
            }
            let end = end.min(self.rope.len_chars());
            let deleted: String = self.rope.slice(start..end).to_string();

            let start_byte = self.rope.char_to_byte(start);
            let end_byte = self.rope.char_to_byte(end);
            let start_line = self.rope.char_to_line(start);
            let start_line_byte = self.rope.line_to_byte(start_line);
            let start_col_byte = start_byte - start_line_byte;

            let end_line = self.rope.char_to_line(end);
            let end_line_byte = self.rope.line_to_byte(end_line);
            let end_col_byte = end_byte - end_line_byte;

            self.rope.remove(start..end);

            let edit_event = EditEvent {
                start_byte,
                old_end_byte: end_byte,
                new_end_byte: start_byte,
                start_position: (start_line, start_col_byte),
                old_end_position: (end_line, end_col_byte),
                new_end_position: (start_line, start_col_byte),
            };
            self.pending_edits.push(edit_event.clone());

            self.history.record(
                EditRecord {
                    char_offset: start,
                    old_text: Rc::from(deleted.as_str()),
                    new_text: Rc::from(""),
                    edit_event,
                },
                &cursors_before,
                &cursors_before,
            );

            deleted_segments.push((orig_idx, deleted));
        }

        // Re-sort segments to original cursor order for the returned text.
        deleted_segments.sort_by_key(|&(idx, _)| idx);
        // Concatenate the deleted segments (no separator) — each range carries
        // its own trailing characters, so concat is faithful to the source.
        let combined: String = deleted_segments
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(_, s)| s.as_str())
            .collect();

        // One cursor per deleted range, parked at the post-shift start of that
        // range. Ranges to the left shift later ranges back by the cumulative
        // deleted length.
        let new_len = self.rope.len_chars();
        let mut sorted_ranges: Vec<(usize, usize)> = ranges.to_vec();
        sorted_ranges.sort_by_key(|&(s, _)| s);
        let mut new_cursors: Vec<usize> = Vec::with_capacity(sorted_ranges.len());
        let mut cumulative_shift = 0usize;
        for &(start, end) in &sorted_ranges {
            let pos = start.saturating_sub(cumulative_shift).min(new_len);
            new_cursors.push(pos);
            cumulative_shift += end.saturating_sub(start);
        }
        if new_cursors.is_empty() {
            new_cursors.push(0.min(new_len));
        }
        self.cursors = new_cursors;
        self.selections = vec![None; self.cursors.len()];
        self.sort_and_dedup_cursors();

        self.history.update_cursors_after(&self.cursors);
        if started_transaction {
            self.history.commit_transaction();
        }

        self.dirty = true;
        self.bump_version();
        combined
    }

    /// Delete the whole current line including its trailing newline. The cursor
    /// lands at the start of the (now-shifted) line. No-op on an empty buffer.
    /// Used by the browser emacs PoC's whole-line delete (Ctrl+Shift+K).
    pub fn delete_current_line(&mut self) {
        let line = self.cursor_line();
        let start = self.rope.line_to_char(line);
        let total_lines = self.rope.len_lines();
        let end = if line + 1 < total_lines {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        };
        self.clear_anchor();
        self.delete_range(start, end);
    }

    /// Delete every non-empty selection range, returning the removed text
    /// (ranges concatenated), or `None` when nothing is selected. The editor
    /// mode is left untouched — unlike the Visual-mode `DeleteSelection` path,
    /// the browser emacs PoC stays in Insert mode for VSCode-style editing.
    pub fn delete_active_selection(&mut self) -> Option<String> {
        let ranges: Vec<(usize, usize)> = self
            .merged_selection_ranges()
            .into_iter()
            .filter(|&(s, e)| s < e)
            .collect();
        if ranges.is_empty() {
            return None;
        }
        let deleted = self.delete_ranges(&ranges);
        self.clear_anchor();
        Some(deleted)
    }

    /// Insert text at a given char position with full undo/redo recording.
    pub fn insert_text_at(&mut self, pos: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        let text = Self::normalize_newlines_for_insert(text);
        let text = text.as_ref();

        let cursors_before = self.cursors.clone();
        let pos = pos.min(self.rope.len_chars());

        let byte_pos = self.rope.char_to_byte(pos);
        let line = self.rope.char_to_line(pos);
        let line_byte_start = self.rope.line_to_byte(line);
        let col_byte = byte_pos - line_byte_start;

        self.rope.insert(pos, text);
        let char_count = if text.is_ascii() {
            text.len()
        } else {
            text.chars().count()
        };
        self.cursors[0] = pos + char_count;
        self.sync_selection_head();
        self.dirty = true;
        self.bump_version();

        let new_end_pos = self.compute_end_position(line, col_byte, text);

        let edit_event = EditEvent {
            start_byte: byte_pos,
            old_end_byte: byte_pos,
            new_end_byte: byte_pos + text.len(),
            start_position: (line, col_byte),
            old_end_position: (line, col_byte),
            new_end_position: new_end_pos,
        };
        self.pending_edits.push(edit_event.clone());

        self.history.record(
            EditRecord {
                char_offset: pos,
                old_text: Rc::from(""),
                new_text: Rc::from(text),
                edit_event,
            },
            &cursors_before,
            &self.cursors,
        );
    }
}

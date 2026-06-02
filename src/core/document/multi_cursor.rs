use super::*;

impl Document {
    /// Number of active cursors
    pub fn cursor_count(&self) -> usize {
        self.cursors.len()
    }

    /// Check if multiple cursors are active
    pub fn has_multiple_cursors(&self) -> bool {
        self.cursors.len() > 1
    }

    /// Sort cursors by position and remove duplicates, keeping `selections`
    /// aligned. Primary (originally index 0) stays at index 0 in both vectors.
    pub(super) fn sort_and_dedup_cursors(&mut self) {
        debug_assert_eq!(self.cursors.len(), self.selections.len());
        if self.cursors.len() <= 1 {
            return;
        }
        // Build a permutation of indices sorted by cursor position. Keep the
        // primary's index identifiable so we can swap it back to the front.
        let mut indices: Vec<usize> = (0..self.cursors.len()).collect();
        indices.sort_by_key(|&i| self.cursors[i]);

        // Dedup by cursor position. Prefer keeping the original primary (idx 0)
        // when duplicates collapse so the primary's selection survives.
        let mut deduped: Vec<usize> = Vec::with_capacity(indices.len());
        for &i in &indices {
            match deduped.last() {
                Some(&last) if self.cursors[last] == self.cursors[i] => {
                    if i == 0 {
                        *deduped.last_mut().unwrap() = 0;
                    }
                }
                _ => deduped.push(i),
            }
        }

        let new_cursors: Vec<usize> = deduped.iter().map(|&i| self.cursors[i]).collect();
        let new_selections: Vec<Option<Selection>> =
            deduped.iter().map(|&i| self.selections[i]).collect();
        self.cursors = new_cursors;
        self.selections = new_selections;

        // Restore primary (original index 0) to the front.
        if let Some(new_primary_idx) = deduped.iter().position(|&i| i == 0)
            && new_primary_idx != 0
        {
            self.cursors.swap(0, new_primary_idx);
            self.selections.swap(0, new_primary_idx);
        }
        debug_assert_eq!(self.cursors.len(), self.selections.len());
    }

    /// Add a cursor at the given char offset.
    /// Returns true if a new cursor was added.
    pub fn add_cursor_at(&mut self, pos: usize) -> bool {
        let pos = pos.min(self.rope.len_chars());
        if self.cursors.contains(&pos) {
            return false;
        }
        self.cursors.push(pos);
        self.selections.push(None);
        self.sort_and_dedup_cursors();
        true
    }

    /// Add a selection `[start, end)` (cursor at `end`) as the new *primary*
    /// cursor, keeping the existing cursors. Used by the browser editor's Cmd+D
    /// (VSCode "add selection to next match"): inserting at the front and then
    /// `sort_and_dedup_cursors` (which preserves the original index-0 as primary)
    /// keeps the freshly added match primary so the viewport follows it.
    pub fn add_primary_selection(&mut self, start: usize, end: usize) {
        let n = self.rope.len_chars();
        self.cursors.insert(0, end.min(n));
        self.selections.insert(
            0,
            Some(Selection::tail_on_forward(start.min(n), end.min(n))),
        );
        self.sort_and_dedup_cursors();
    }

    /// Add a cursor on the line above at the same column (best effort).
    /// Uses the primary cursor's column but adds above the topmost existing cursor.
    /// Returns true if a cursor was added.
    pub fn add_cursor_above(&mut self) -> bool {
        // Use primary cursor's column
        let primary_pos = self.cursors[0];
        let primary_line = self.rope.char_to_line(primary_pos);
        let col = primary_pos - self.rope.line_to_char(primary_line);

        // Find the topmost (smallest line number) cursor
        let top_cursor = *self.cursors.iter().min().unwrap();
        let top_line = self.rope.char_to_line(top_cursor);

        if top_line == 0 {
            return false;
        }

        let target_line = top_line - 1;
        let target_line_start = self.rope.line_to_char(target_line);
        let target_line_len = self.line_len(target_line);
        let clamped_col = col.min(target_line_len);
        let new_pos = target_line_start + clamped_col;

        // Don't add if already exists
        if self.cursors.contains(&new_pos) {
            return false;
        }

        self.cursors.push(new_pos);
        self.selections.push(None);
        self.sort_and_dedup_cursors();
        true
    }

    /// Add a cursor on the line below at the same column (best effort).
    /// Uses the primary cursor's column but adds below the bottommost existing cursor.
    /// Returns true if a cursor was added.
    pub fn add_cursor_below(&mut self) -> bool {
        // Use primary cursor's column
        let primary_pos = self.cursors[0];
        let primary_line = self.rope.char_to_line(primary_pos);
        let col = primary_pos - self.rope.line_to_char(primary_line);

        // Find the bottommost (largest line number) cursor
        let bottom_cursor = *self.cursors.iter().max().unwrap();
        let bottom_line = self.rope.char_to_line(bottom_cursor);

        if bottom_line + 1 >= self.rope.len_lines() {
            return false;
        }

        let target_line = bottom_line + 1;
        let target_line_start = self.rope.line_to_char(target_line);
        let target_line_len = self.line_len(target_line);
        let clamped_col = col.min(target_line_len);
        let new_pos = target_line_start + clamped_col;

        // Don't add if already exists
        if self.cursors.contains(&new_pos) {
            return false;
        }

        self.cursors.push(new_pos);
        self.selections.push(None);
        self.sort_and_dedup_cursors();
        true
    }

    /// Add cursors from the topmost existing cursor up to the first line,
    /// one cursor per line at the primary cursor's column.
    pub fn add_cursors_to_top(&mut self) {
        let primary_pos = self.cursors[0];
        let primary_line = self.rope.char_to_line(primary_pos);
        let col = primary_pos - self.rope.line_to_char(primary_line);

        let top_cursor = *self.cursors.iter().min().unwrap();
        let top_line = self.rope.char_to_line(top_cursor);

        for line in (0..top_line).rev() {
            let line_start = self.rope.line_to_char(line);
            let clamped_col = col.min(self.line_len(line));
            let new_pos = line_start + clamped_col;
            if !self.cursors.contains(&new_pos) {
                self.cursors.push(new_pos);
                self.selections.push(None);
            }
        }
        self.sort_and_dedup_cursors();
    }

    /// Add cursors from the bottommost existing cursor down to the last line,
    /// one cursor per line at the primary cursor's column.
    pub fn add_cursors_to_bottom(&mut self) {
        let primary_pos = self.cursors[0];
        let primary_line = self.rope.char_to_line(primary_pos);
        let col = primary_pos - self.rope.line_to_char(primary_line);

        let bottom_cursor = *self.cursors.iter().max().unwrap();
        let bottom_line = self.rope.char_to_line(bottom_cursor);
        let last_line = self.rope.len_lines().saturating_sub(1);

        for line in (bottom_line + 1)..=last_line {
            let line_start = self.rope.line_to_char(line);
            let clamped_col = col.min(self.line_len(line));
            let new_pos = line_start + clamped_col;
            if !self.cursors.contains(&new_pos) {
                self.cursors.push(new_pos);
                self.selections.push(None);
            }
        }
        self.sort_and_dedup_cursors();
    }

    /// Remove all secondary cursors, keeping only the primary cursor.
    pub fn remove_secondary_cursors(&mut self) {
        self.cursors.truncate(1);
        self.selections.truncate(1);
    }
}

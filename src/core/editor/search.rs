use super::*;
use ropey::Rope;

/// Find the next occurrence of `pat_lower` (already lowercased) at or after
/// `from_char` in `rope`, comparing case-insensitively. When `wrap` is true and
/// nothing is found in `[from_char, end)`, the scan continues from char 0 up
/// to `from_char`.
///
/// Returns the char offset of the first character of the match.
pub fn find_match_forward(
    rope: &Rope,
    pat_lower: &str,
    from_char: usize,
    wrap: bool,
) -> Option<usize> {
    if pat_lower.is_empty() {
        return None;
    }
    let rope_chars = rope.len_chars();
    let from = from_char.min(rope_chars);
    if let Some(hit) = find_match_in_range(rope, pat_lower, from, rope_chars) {
        return Some(hit);
    }
    if wrap && from > 0 {
        let wrap_end = (from + pat_lower.chars().count()).min(rope_chars);
        return find_match_in_range(rope, pat_lower, 0, wrap_end);
    }
    None
}

/// Find the last occurrence of `pat_lower` whose start char is strictly
/// before `before_char`. Wraps to the rope tail when nothing earlier matches.
pub fn find_match_backward(
    rope: &Rope,
    pat_lower: &str,
    before_char: usize,
    wrap: bool,
) -> Option<usize> {
    if pat_lower.is_empty() {
        return None;
    }
    let rope_chars = rope.len_chars();
    let upper = before_char.min(rope_chars);
    if upper > 0
        && let Some(hit) = last_match_in_range(rope, pat_lower, 0, upper)
    {
        return Some(hit);
    }
    if wrap
        && upper < rope_chars
        && let Some(hit) = last_match_in_range(rope, pat_lower, upper, rope_chars)
    {
        return Some(hit);
    }
    None
}

/// Collect up to `limit` matches with char offsets in `[start_char, end_char)`.
/// Intended for render-time viewport highlighting; pass a viewport-sized range.
pub fn find_matches_in_range(
    rope: &Rope,
    pat_lower: &str,
    start_char: usize,
    end_char: usize,
    limit: usize,
) -> Vec<usize> {
    let mut out = Vec::new();
    if pat_lower.is_empty() || limit == 0 {
        return out;
    }
    let pat_char_len = pat_lower.chars().count().max(1);
    let mut cursor = start_char;
    let end = end_char.min(rope.len_chars());
    while cursor < end && out.len() < limit {
        match find_match_in_range(rope, pat_lower, cursor, end) {
            Some(hit) => {
                out.push(hit);
                cursor = hit.saturating_add(pat_char_len);
            }
            None => break,
        }
    }
    out
}

/// Walks chunks forward and returns the last match in `[start_char, end_char)`.
fn last_match_in_range(
    rope: &Rope,
    pat_lower: &str,
    start_char: usize,
    end_char: usize,
) -> Option<usize> {
    let pat_char_len = pat_lower.chars().count().max(1);
    let mut cursor = start_char;
    let mut last: Option<usize> = None;
    while cursor < end_char {
        match find_match_in_range(rope, pat_lower, cursor, end_char) {
            Some(hit) => {
                last = Some(hit);
                cursor = hit.saturating_add(pat_char_len);
            }
            None => break,
        }
    }
    last
}

/// Core forward chunk-scanner: returns the first match whose start char is in
/// `[start_char, end_char)`.
///
/// Walks `rope` via `Chunks`, lowercases each chunk (small allocation —
/// ropey chunks are typically ≤ ~1 KB), and uses `str::find` for the actual
/// substring match. A trailing-byte overlap is kept across chunk boundaries
/// so a match straddling two chunks still gets found.
fn find_match_in_range(
    rope: &Rope,
    pat_lower: &str,
    start_char: usize,
    end_char: usize,
) -> Option<usize> {
    if pat_lower.is_empty() || start_char >= end_char {
        return None;
    }
    let rope_chars = rope.len_chars();
    if start_char >= rope_chars {
        return None;
    }

    let start_byte = rope.char_to_byte(start_char);
    let (chunks_iter, _, mut cur_char, _) = rope.chunks_at_byte(start_byte);

    // Overlap from the previous chunk so a match straddling a chunk boundary
    // still hits. Always kept on a char boundary.
    let mut overlap_lower = String::new();
    let mut overlap_char_off: usize = 0;

    for chunk in chunks_iter {
        let chunk_char_off = cur_char;
        cur_char += chunk.chars().count();

        if chunk_char_off >= end_char {
            break;
        }

        let lower_chunk: String = chunk.to_lowercase();

        // Concatenate the overlap with the new chunk so a straddling match
        // sees both halves of itself.
        let (search_str, base_char_off) = if overlap_lower.is_empty() {
            (lower_chunk, chunk_char_off)
        } else {
            let mut joined = String::with_capacity(overlap_lower.len() + lower_chunk.len());
            joined.push_str(&overlap_lower);
            joined.push_str(&lower_chunk);
            (joined, overlap_char_off)
        };

        // Skip into `search_str` past anything that corresponds to char
        // positions before `start_char`.
        let skip_chars = start_char.saturating_sub(base_char_off);
        let skip_bytes = if skip_chars == 0 {
            0
        } else {
            search_str
                .char_indices()
                .nth(skip_chars)
                .map(|(b, _)| b)
                .unwrap_or(search_str.len())
        };

        if skip_bytes < search_str.len()
            && let Some(rel) = search_str[skip_bytes..].find(pat_lower)
        {
            let match_byte = skip_bytes + rel;
            let chars_before = search_str[..match_byte].chars().count();
            let abs_char = base_char_off + chars_before;
            if abs_char < end_char {
                return Some(abs_char);
            }
        }

        // Stage the overlap for the next iteration: the trailing
        // `pat_lower.len() - 1` bytes of search_str, rounded to a char
        // boundary.
        overlap_lower.clear();
        let want = pat_lower.len().saturating_sub(1);
        if want > 0 {
            let start = search_str.len().saturating_sub(want);
            let start = floor_char_boundary(&search_str, start);
            overlap_char_off = base_char_off + search_str[..start].chars().count();
            overlap_lower.push_str(&search_str[start..]);
        }
    }

    None
}

/// Round `idx` down to the nearest UTF-8 char boundary in `s`.
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

impl Editor {
    /// Update the search pattern and move the primary cursor to the first
    /// match at or after `search.anchor` (the cursor at the time `/` was
    /// opened). Wraps around the end of the buffer.
    pub fn search_update(&mut self, pattern: &str) {
        self.search.pattern = pattern.to_string();
        self.search.pattern_lower = pattern.to_lowercase();
        self.search.last_search_found = false;

        if self.search.pattern_lower.is_empty() {
            return;
        }

        let anchor = self.search.anchor;
        let rope = &self.documents[self.active_index].rope;
        if let Some(hit) = find_match_forward(rope, &self.search.pattern_lower, anchor, true) {
            self.documents[self.active_index].cursors[0] = hit;
            self.search.last_search_found = true;
        }
    }

    /// Move the cursor to the next match after the current primary cursor.
    /// Wraps around. Returns whether a match was found.
    pub fn search_next(&mut self) -> bool {
        if self.search.pattern_lower.is_empty() {
            return false;
        }
        let cursor = self.documents[self.active_index].cursors[0];
        let pat_chars = self.search.pattern.chars().count().max(1);
        // Advance past the current match (if any) so consecutive `n`-presses
        // step through non-overlapping occurrences.
        let from = cursor.saturating_add(pat_chars);
        let rope = &self.documents[self.active_index].rope;
        if let Some(hit) = find_match_forward(rope, &self.search.pattern_lower, from, true) {
            self.documents[self.active_index].cursors[0] = hit;
            self.search.last_search_found = true;
            return true;
        }
        false
    }

    /// Move the cursor to the previous match before the current primary
    /// cursor. Wraps around. Returns whether a match was found.
    pub fn search_prev(&mut self) -> bool {
        if self.search.pattern_lower.is_empty() {
            return false;
        }
        let cursor = self.documents[self.active_index].cursors[0];
        let rope = &self.documents[self.active_index].rope;
        if let Some(hit) = find_match_backward(rope, &self.search.pattern_lower, cursor, true) {
            self.documents[self.active_index].cursors[0] = hit;
            self.search.last_search_found = true;
            return true;
        }
        false
    }

    /// Add a secondary cursor at the next match after the primary cursor.
    /// Skips matches that overlap an existing cursor; wraps around.
    pub fn add_cursor_to_next_search_match(&mut self) -> bool {
        if self.search.pattern_lower.is_empty() {
            return false;
        }
        let match_len = self.search.pattern.chars().count();
        if match_len == 0 {
            return false;
        }
        let rope_chars = self.documents[self.active_index].rope.len_chars();
        let primary = self.documents[self.active_index].cursors[0];
        let pat_lower = self.search.pattern_lower.clone();
        let mut probe = primary.saturating_add(1).min(rope_chars);
        let mut wrapped = false;
        loop {
            let rope = &self.documents[self.active_index].rope;
            let Some(hit) = find_match_forward(rope, &pat_lower, probe, true) else {
                return false;
            };
            // Detect a full lap with no acceptable insertion point.
            if wrapped && hit >= primary {
                return false;
            }
            if hit < probe {
                // wrap-around happened
                wrapped = true;
                if hit >= primary {
                    return false;
                }
            }
            let end = hit.saturating_add(match_len);
            let overlap = self.documents[self.active_index]
                .cursors
                .iter()
                .any(|&c| c >= hit && c < end);
            if !overlap && self.documents[self.active_index].add_cursor_at(hit) {
                return true;
            }
            probe = end.max(hit + 1);
            if probe >= rope_chars {
                probe = 0;
                wrapped = true;
            }
        }
    }

    /// Add a secondary cursor at the previous match before the primary cursor.
    pub fn add_cursor_to_prev_search_match(&mut self) -> bool {
        if self.search.pattern_lower.is_empty() {
            return false;
        }
        let match_len = self.search.pattern.chars().count();
        if match_len == 0 {
            return false;
        }
        let rope_chars = self.documents[self.active_index].rope.len_chars();
        let primary = self.documents[self.active_index].cursors[0];
        let pat_lower = self.search.pattern_lower.clone();
        let mut probe = primary;
        let mut wrapped = false;
        loop {
            let rope = &self.documents[self.active_index].rope;
            let Some(hit) = find_match_backward(rope, &pat_lower, probe, true) else {
                return false;
            };
            if wrapped && hit <= primary {
                return false;
            }
            if hit >= probe {
                wrapped = true;
                if hit <= primary {
                    return false;
                }
            }
            let end = hit.saturating_add(match_len);
            let overlap = self.documents[self.active_index]
                .cursors
                .iter()
                .any(|&c| c >= hit && c < end);
            if !overlap && self.documents[self.active_index].add_cursor_at(hit) {
                return true;
            }
            probe = hit;
            if probe == 0 {
                probe = rope_chars;
                wrapped = true;
            }
        }
    }

    /// Add a secondary cursor at every match in the buffer. This is an
    /// explicit user action — a one-shot O(buffer) scan is acceptable here.
    pub fn add_cursor_to_all_search_matches(&mut self) -> usize {
        if self.search.pattern_lower.is_empty() {
            return 0;
        }
        let match_len = self.search.pattern.chars().count();
        if match_len == 0 {
            return 0;
        }
        let rope_chars = self.documents[self.active_index].rope.len_chars();
        let pat_lower = self.search.pattern_lower.clone();
        let mut added = 0usize;
        let mut probe = 0usize;
        while probe < rope_chars {
            let rope = &self.documents[self.active_index].rope;
            let Some(hit) = find_match_forward(rope, &pat_lower, probe, false) else {
                break;
            };
            let end = hit.saturating_add(match_len);
            let occupied = self.documents[self.active_index]
                .cursors
                .iter()
                .any(|&c| c >= hit && c < end);
            if !occupied && self.documents[self.active_index].add_cursor_at(hit) {
                added += 1;
            }
            probe = end.max(hit + 1);
        }
        added
    }

    pub fn reset_search(&mut self) {
        self.search.clear();
    }
}

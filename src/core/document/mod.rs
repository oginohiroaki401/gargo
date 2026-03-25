use crate::ui::text::char_display_width;
use ropey::Rope;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::command::git::GitLineStatus;
use crate::core::buffer::{CharClass, EditEvent, char_class};
use crate::core::history::{EditRecord, History};

mod cursor;
mod display;
mod editing;
mod file_io;
mod movement;
mod multi_cursor;
mod selection;
mod viewport;

pub type DocumentId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    pub cursor_display: SelectionCursorDisplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionCursorDisplay {
    TailOnForward,
    Head,
}

impl Selection {
    pub fn tail_on_forward(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            cursor_display: SelectionCursorDisplay::TailOnForward,
        }
    }

    pub fn head(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            cursor_display: SelectionCursorDisplay::Head,
        }
    }
}

/// A document is a file-backed (or scratch) editing unit.
///
/// Combines a text buffer (Rope + EditEvent recording) with cursor state,
/// scroll position, file path, dirty tracking, and undo/redo history.
pub struct Document {
    pub id: DocumentId,
    pub rope: Rope,
    /// Multiple cursors. The first cursor (index 0) is the "primary" cursor.
    /// Invariants: never empty, sorted by position when multiple cursors exist.
    pub cursors: Vec<usize>,
    pub scroll_offset: usize,
    pub horizontal_scroll_offset: usize,
    pub file_path: Option<PathBuf>,
    pub dirty: bool,
    pub pending_edits: Vec<EditEvent>,
    pub history: History,
    /// Selection for the primary cursor only
    pub selection: Option<Selection>,
    pub git_gutter: HashMap<usize, GitLineStatus>,
    cached_status_bar_path: String,
}

impl Document {
    fn normalize_newlines_for_insert(text: &str) -> Cow<'_, str> {
        if !text.as_bytes().contains(&b'\r') {
            return Cow::Borrowed(text);
        }

        let mut normalized = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\r' {
                if matches!(chars.peek(), Some('\n')) {
                    chars.next();
                }
                normalized.push('\n');
            } else {
                normalized.push(ch);
            }
        }
        Cow::Owned(normalized)
    }

    pub fn new_scratch(id: DocumentId) -> Self {
        Self {
            id,
            rope: Rope::new(),
            cursors: vec![0],
            scroll_offset: 0,
            horizontal_scroll_offset: 0,
            file_path: None,
            dirty: false,
            pending_edits: Vec::new(),
            history: History::new(),
            selection: None,
            git_gutter: HashMap::new(),
            cached_status_bar_path: "[scratch]".to_string(),
        }
    }

    /// Length of line content excluding trailing newline
    fn line_len(&self, line_idx: usize) -> usize {
        let line = self.rope.line(line_idx);
        let len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len - 1
        } else {
            len
        }
    }

    fn line_display_width(&self, line_idx: usize) -> usize {
        let line = self.rope.line(line_idx);
        let mut width = 0usize;
        for idx in 0..line.len_chars() {
            let ch = line.char(idx);
            if ch == '\n' {
                break;
            }
            width += char_display_width(ch);
        }
        width
    }

    /// Compute (row, col_byte) end position after applying text starting at (start_line, start_col_byte).
    fn compute_end_position(
        &self,
        start_line: usize,
        start_col_byte: usize,
        text: &str,
    ) -> (usize, usize) {
        if text.is_empty() {
            return (start_line, start_col_byte);
        }
        let newline_count = text.as_bytes().iter().filter(|&&b| b == b'\n').count();
        if newline_count == 0 {
            (start_line, start_col_byte + text.len())
        } else {
            let last_newline = text.rfind('\n').unwrap();
            let after_last_newline = &text[last_newline + 1..];
            (start_line + newline_count, after_last_newline.len())
        }
    }

    pub fn current_line_is_empty(&self) -> bool {
        self.line_len(self.cursor_line()) == 0
    }

    pub fn indent_for_empty_line(&self) -> String {
        let line = self.cursor_line();
        let source_line = if line > 0 { line - 1 } else { line };
        let text = self.rope.line(source_line).to_string();
        text.chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    pub fn append_newline_at_eof(&mut self) {
        let end = self.rope.len_chars();
        self.insert_text_at(end, "\n");
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

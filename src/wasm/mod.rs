//! Browser (WASM) editor bindings.
//!
//! `WebEditor` wraps the shared [`Editor`] core and exposes a small JS-facing
//! API: feed keys (routed through the same `keymap` + a minimal dispatcher used
//! by the terminal), commit IME/paste text, and produce a render model (visible
//! line texts + cursor/selection positions) for the DOM renderer.
//!
//! All editing happens locally in the tab; the server only reads files on open
//! and writes them (with conflict detection) on save.

use wasm_bindgen::prelude::*;

use crate::core::document::Selection;
use crate::core::editor::Editor;
use crate::core::mode::Mode;
use crate::input::action::{Action, CoreAction};
use crate::input::chord::KeyState;
use crate::input::key::{KeyCode, KeyEvent, KeyModifiers};
use crate::input::keymap;
use crate::ui::text::{TAB_DISPLAY_WIDTH, char_display_width};
use ropey::Rope;

/// Indent width for the browser editor (matches the terminal default).
const TAB_WIDTH: usize = 4;

#[wasm_bindgen]
pub struct WebEditor {
    editor: Editor,
    key_state: KeyState,
}

#[derive(serde::Serialize)]
struct CursorPos {
    row: usize,
    col: usize,
    /// Character offset into the tab-expanded row (tabs counted as their
    /// display width). The browser measures this prefix to place the caret on
    /// the exact glyph boundary even when Latin and full-width (CJK) glyphs mix.
    char_col: usize,
    primary: bool,
}

#[derive(serde::Serialize)]
struct SelRange {
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    /// Tab-expanded character offsets (see [`CursorPos::char_col`]).
    start_char: usize,
    end_char: usize,
}

#[derive(serde::Serialize)]
struct RenderModel {
    top: usize,
    total_lines: usize,
    /// Visible line texts (tabs expanded to spaces; trailing newline stripped).
    rows: Vec<String>,
    cursors: Vec<CursorPos>,
    selections: Vec<SelRange>,
    mode: String,
}

#[wasm_bindgen]
impl WebEditor {
    /// Create an editor for `path` initialized with `content` read from disk.
    #[wasm_bindgen(constructor)]
    pub fn new(path: String, content: String) -> WebEditor {
        let mut editor = Editor::new();
        {
            let doc = editor.active_buffer_mut();
            doc.rope = Rope::from_str(&content);
            doc.cursors = vec![0];
            doc.file_path = Some(std::path::PathBuf::from(path));
            doc.dirty = false;
        }
        WebEditor {
            editor,
            key_state: KeyState::Normal,
        }
    }

    /// Route a DOM key through the modal keymap. `code` is the browser
    /// `KeyboardEvent.key` for printable keys, or a named key
    /// (`"Enter"`, `"Backspace"`, `"Escape"`, `"ArrowLeft"`, `"F4"`, …).
    pub fn key(&mut self, code: &str, ctrl: bool, shift: bool, alt: bool) {
        let Some(key) = build_key(code, ctrl, shift, alt) else {
            return;
        };
        let action = keymap::resolve(key, &mut self.key_state, &self.editor.mode, false);
        // The browser MVP only supports editing (Core) actions. Ui/App actions
        // (pickers, window management, save, …) are handled in JS or ignored.
        if let Action::Core(core) = action {
            self.editor.dispatch_core(core, TAB_WIDTH);
        }
    }

    /// Commit IME-composed text or a paste as a single edit (Insert mode).
    pub fn insert_text(&mut self, text: &str) {
        self.editor
            .dispatch_core(CoreAction::InsertText(text.to_string()), TAB_WIDTH);
    }

    /// True if any cursor has a non-empty selection. Used by the emacs PoC to
    /// implement VSCode-style "selection delete / type-to-replace" while
    /// staying in Insert mode.
    pub fn has_selection(&self) -> bool {
        self.editor
            .active_buffer()
            .merged_selection_ranges()
            .into_iter()
            .any(|(s, e)| s < e)
    }

    /// Delete the active selection(s) without changing the editor mode (unlike
    /// `CoreAction::DeleteSelection`, which drops to Normal). Returns `true` if
    /// anything was deleted. The deleted text goes to the editor register.
    pub fn delete_selection(&mut self) -> bool {
        if let Some(deleted) = self.editor.active_buffer_mut().delete_active_selection() {
            self.editor.register = Some(deleted);
            self.editor.mark_highlights_dirty();
            true
        } else {
            false
        }
    }

    /// Delete the whole current line including its trailing newline, staying in
    /// the current mode. Bound to Ctrl+Shift+K in the emacs PoC.
    pub fn delete_line(&mut self) {
        self.editor.active_buffer_mut().delete_current_line();
        self.editor.mark_highlights_dirty();
    }

    /// Move the primary cursor to a display (row, col) and clear any selection.
    /// Used for mouse-click cursor placement. `col` is a display column (tab/CJK
    /// aware), matching the coordinates produced by `render`.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        let off = {
            let buf = self.editor.active_buffer();
            line_col_to_offset(&buf.rope, row, col)
        };
        let buf = self.editor.active_buffer_mut();
        buf.clear_anchor();
        buf.cursors = vec![off];
        buf.selections = vec![None];
    }

    /// Select the word (or whitespace/punctuation run) under a display
    /// (row, col), placing the cursor at the word end. Used for double-click
    /// word selection. No-op (clears selection) when the click lands on a
    /// newline / empty area. `col` is a display column, like `set_cursor`.
    pub fn select_word_at(&mut self, row: usize, col: usize) {
        let range = {
            let buf = self.editor.active_buffer();
            let off = line_col_to_offset(&buf.rope, row, col);
            crate::core::document::expand::word_range_at(&buf.rope, off)
        };
        let buf = self.editor.active_buffer_mut();
        match range {
            Some((start, end)) => {
                buf.cursors = vec![end];
                buf.selections = vec![Some(Selection::tail_on_forward(start, end))];
            }
            None => {
                buf.clear_anchor();
                buf.selections = vec![None];
            }
        }
    }

    /// Primary cursor's selection anchor as a display (row, col), or `-1` in
    /// both slots when there is no active selection. Used by JS shift-click to
    /// extend from the existing anchor. Returned as a 2-element `[row, col]`.
    pub fn anchor_row(&self) -> i64 {
        let buf = self.editor.active_buffer();
        match buf.selection_anchor() {
            Some(off) => buf.rope.char_to_line(off.min(buf.rope.len_chars())) as i64,
            None => -1,
        }
    }

    pub fn anchor_col(&self) -> i64 {
        let buf = self.editor.active_buffer();
        match buf.selection_anchor() {
            Some(off) => offset_to_display_col(&buf.rope, off) as i64,
            None => -1,
        }
    }

    /// Undo / redo the last edit transaction, staying in the current (Insert)
    /// mode. Bound to Cmd+Z / Cmd+Shift+Z in the browser editor; the modal
    /// keymap has no Ctrl+Z binding in Insert mode, so JS calls these directly.
    pub fn undo(&mut self) {
        self.editor.dispatch_core(CoreAction::Undo, TAB_WIDTH);
    }

    pub fn redo(&mut self) {
        self.editor.dispatch_core(CoreAction::Redo, TAB_WIDTH);
    }

    /// Set a single selection from an anchor (row, col) to a head (row, col),
    /// placing the primary cursor at the head. Used for mouse-drag selection.
    /// An empty range (anchor == head) clears the selection.
    pub fn set_selection(
        &mut self,
        anchor_row: usize,
        anchor_col: usize,
        head_row: usize,
        head_col: usize,
    ) {
        let (anchor, head) = {
            let buf = self.editor.active_buffer();
            (
                line_col_to_offset(&buf.rope, anchor_row, anchor_col),
                line_col_to_offset(&buf.rope, head_row, head_col),
            )
        };
        let buf = self.editor.active_buffer_mut();
        buf.cursors = vec![head];
        buf.selections = if anchor == head {
            vec![None]
        } else {
            vec![Some(Selection::tail_on_forward(anchor, head))]
        };
    }

    /// Full buffer contents (for saving).
    pub fn content(&self) -> String {
        self.editor.active_buffer().rope.to_string()
    }

    /// Monotonic version, bumped on every edit (for render invalidation).
    pub fn version(&self) -> u64 {
        self.editor.active_buffer().version
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.active_buffer().dirty
    }

    pub fn line_count(&self) -> usize {
        self.editor.active_buffer().rope.len_lines()
    }

    pub fn mode(&self) -> String {
        match self.editor.mode {
            Mode::Normal => "normal",
            Mode::Insert => "insert",
            Mode::Visual => "visual",
        }
        .to_string()
    }

    /// Primary cursor row (0-based line index).
    pub fn cursor_row(&self) -> usize {
        let buf = self.editor.active_buffer();
        buf.rope.char_to_line(primary_cursor(buf))
    }

    /// Primary cursor display column (tab/CJK aware), for IME caret placement.
    pub fn cursor_col(&self) -> usize {
        let buf = self.editor.active_buffer();
        offset_to_display_col(&buf.rope, primary_cursor(buf))
    }

    /// Produce a render model for the visible window `[top, top + height)`.
    pub fn render(&mut self, top: usize, height: usize) -> Result<JsValue, JsValue> {
        let buf = self.editor.active_buffer();
        let rope = &buf.rope;
        let total_lines = rope.len_lines();
        let end = (top + height).min(total_lines);

        let mut rows = Vec::with_capacity(end.saturating_sub(top));
        for line in top..end {
            let mut s: String = rope.line(line).to_string();
            // Strip the trailing newline; the renderer lays out rows itself.
            if s.ends_with('\n') {
                s.pop();
                if s.ends_with('\r') {
                    s.pop();
                }
            }
            rows.push(expand_tabs(&s));
        }

        let cursors = buf
            .cursors
            .iter()
            .enumerate()
            .map(|(i, &off)| {
                let (row, col) = offset_to_row_col(rope, off);
                CursorPos {
                    row,
                    col,
                    char_col: offset_to_expanded_char_col(rope, off),
                    primary: i == 0,
                }
            })
            .collect();

        let selections = buf
            .merged_selection_ranges()
            .into_iter()
            .filter(|&(s, e)| s < e)
            .map(|(s, e)| {
                let (start_row, start_col) = offset_to_row_col(rope, s);
                let (end_row, end_col) = offset_to_row_col(rope, e);
                SelRange {
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                    start_char: offset_to_expanded_char_col(rope, s),
                    end_char: offset_to_expanded_char_col(rope, e),
                }
            })
            .collect();

        let model = RenderModel {
            top,
            total_lines,
            rows,
            cursors,
            selections,
            mode: self.mode(),
        };
        serde_wasm_bindgen::to_value(&model).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

fn primary_cursor(buf: &crate::core::document::Document) -> usize {
    buf.cursors
        .first()
        .copied()
        .unwrap_or(0)
        .min(buf.rope.len_chars())
}

fn offset_to_row_col(rope: &Rope, off: usize) -> (usize, usize) {
    let off = off.min(rope.len_chars());
    (rope.char_to_line(off), offset_to_display_col(rope, off))
}

/// Inverse of `offset_to_display_col`: map a display (row, col) to a char
/// offset, clamping the row to the document and the column to the line's end
/// (the trailing newline is never selected). Uses the same `char_display_width`
/// accounting as the renderer/caret so clicks land where the caret is drawn.
fn line_col_to_offset(rope: &Rope, row: usize, col: usize) -> usize {
    let total_lines = rope.len_lines();
    if total_lines == 0 {
        return 0;
    }
    let line = row.min(total_lines - 1);
    let line_start = rope.line_to_char(line);
    let mut acc = 0usize;
    let mut offset = line_start;
    for ch in rope.line(line).chars() {
        if ch == '\n' || ch == '\r' || acc >= col {
            break;
        }
        acc += char_display_width(ch);
        offset += 1;
    }
    offset
}

fn offset_to_display_col(rope: &Rope, off: usize) -> usize {
    let off = off.min(rope.len_chars());
    let line = rope.char_to_line(off);
    let line_start = rope.line_to_char(line);
    rope.slice(line_start..off)
        .chars()
        .map(char_display_width)
        .sum()
}

/// Character offset of `off` within the tab-expanded rendering of its line:
/// tabs count as `TAB_DISPLAY_WIDTH` characters (matching [`expand_tabs`]),
/// every other char counts as one (CJK included — it's a single code point in
/// the expanded string). The browser measures this many leading characters of
/// the rendered row to place the caret/selection at the right pixel.
fn offset_to_expanded_char_col(rope: &Rope, off: usize) -> usize {
    let off = off.min(rope.len_chars());
    let line = rope.char_to_line(off);
    let line_start = rope.line_to_char(line);
    rope.slice(line_start..off)
        .chars()
        .map(|c| if c == '\t' { TAB_DISPLAY_WIDTH } else { 1 })
        .sum()
}

fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    s.replace('\t', &" ".repeat(TAB_DISPLAY_WIDTH))
}

/// Map a DOM key name + modifier flags to a `keymap` key event.
fn build_key(code: &str, ctrl: bool, shift: bool, alt: bool) -> Option<KeyEvent> {
    let key_code = match code {
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Escape" => KeyCode::Esc,
        "Tab" if shift => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Insert" => KeyCode::Insert,
        _ => {
            if let Some(n) = code.strip_prefix('F').and_then(|d| d.parse::<u8>().ok())
                && code.len() > 1
                && (1..=12).contains(&n)
            {
                KeyCode::F(n)
            } else {
                // Printable: the browser sends the resolved character in `key`.
                let mut chars = code.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    // Multi-char name we don't recognize → ignore.
                    return None;
                }
                KeyCode::Char(c)
            }
        }
    };

    let mut mods = KeyModifiers::empty();
    if ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if shift {
        mods |= KeyModifiers::SHIFT;
    }
    if alt {
        mods |= KeyModifiers::ALT;
    }
    Some(KeyEvent::new(key_code, mods))
}

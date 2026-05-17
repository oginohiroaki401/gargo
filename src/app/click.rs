use super::*;

use crate::core::buffer::BufferId;
use crate::core::document::{Document, Selection};
use crate::ui::framework::window_manager::PaneRect;
use crate::ui::text::{char_display_width, slice_display_window};
use crate::ui::views::text_view::reserved_left_gutter_width;
use std::time::{Duration, Instant};

use super::expand::{ExpandChain, expand_selection};

const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);
const MULTI_CLICK_RADIUS_CHARS: usize = 1;

/// Result of mapping a screen click to a document position.
struct ClickTarget {
    line: usize,
    char_pos: usize,
    /// True when the click landed in the gutter; cursor goes to column 0,
    /// no escalation occurs.
    on_gutter: bool,
}

impl App {
    pub(super) fn handle_buffer_click(
        &mut self,
        buffer_id: BufferId,
        screen_col: u16,
        screen_row: u16,
    ) {
        let cols = self.last_term_cols;
        let rows = self.last_term_rows;
        let Some(pane) = self.compositor.pane_at(screen_col, screen_row, cols, rows) else {
            return;
        };
        if pane.buffer_id != buffer_id {
            return;
        }

        // Focus the clicked pane / buffer if it's not already active.
        let switched = self.editor.active_buffer().id != buffer_id;
        if switched && !self.editor.switch_to_buffer(buffer_id) {
            return;
        }

        let Some(target) = screen_to_doc_pos(
            self.editor.active_buffer(),
            pane.rect,
            screen_col,
            screen_row,
            self.config.show_line_number,
            self.config.line_number_width,
        ) else {
            return;
        };

        // Seed the drag anchor for a potential drag-to-select gesture. Gutter
        // clicks still record the line-start so dragging out into the content
        // extends from there.
        self.drag_anchor = Some((buffer_id, target.char_pos));

        let now = Instant::now();
        // Decide whether this click continues the current expand chain.
        let continues_chain = !switched
            && !target.on_gutter
            && match self.expand_chain {
                Some(chain) => {
                    chain.buffer == buffer_id
                        && now.duration_since(chain.last_click_time) <= MULTI_CLICK_WINDOW
                        && target.char_pos.abs_diff(chain.last_click_pos)
                            <= MULTI_CLICK_RADIUS_CHARS
                }
                None => false,
            };

        if !continues_chain {
            // Fresh chain: position cursor only, no selection.
            let doc = self.editor.active_buffer_mut();
            doc.clear_anchor();
            if target.on_gutter {
                doc.set_cursor_line_char(target.line, 0);
            } else {
                set_cursor_to_pos(doc, target.char_pos);
            }
            // Gutter clicks don't seed a chain (subsequent clicks should still
            // start fresh rather than escalating from a gutter origin).
            self.expand_chain = if target.on_gutter {
                None
            } else {
                Some(ExpandChain {
                    buffer: buffer_id,
                    origin: target.char_pos,
                    last_range: None,
                    expected_cursor: target.char_pos,
                    last_click_time: now,
                    last_click_pos: target.char_pos,
                })
            };
            return;
        }

        // Continue chain: ensure highlights are fresh (engine reads tree-sitter)
        // then call the shared expand engine.
        self.editor.update_highlights_if_dirty();
        let chain = self.expand_chain.expect("chain checked above");
        let new_range = expand_selection(&self.editor, buffer_id, chain.origin, chain.last_range);
        if let Some((s, e)) = new_range {
            let doc = self.editor.active_buffer_mut();
            doc.selections[0] = Some(Selection::tail_on_forward(s, e));
            doc.cursors[0] = e;
            self.expand_chain = Some(ExpandChain {
                buffer: buffer_id,
                origin: chain.origin,
                last_range: Some((s, e)),
                expected_cursor: e,
                last_click_time: now,
                last_click_pos: target.char_pos,
            });
        } else {
            // No further expansion possible — keep current selection, refresh timing.
            self.expand_chain = Some(ExpandChain {
                last_click_time: now,
                last_click_pos: target.char_pos,
                ..chain
            });
        }
    }

    /// Extend selection from the drag anchor (seeded at mouse-down) to the
    /// current pointer position. No-op until a buffer click has seeded the
    /// anchor.
    pub(super) fn handle_buffer_drag(
        &mut self,
        buffer_id: BufferId,
        screen_col: u16,
        screen_row: u16,
    ) {
        let Some((anchor_buffer, anchor_pos)) = self.drag_anchor else {
            return;
        };
        if anchor_buffer != buffer_id {
            return;
        }

        let cols = self.last_term_cols;
        let rows = self.last_term_rows;
        let Some(pane) = self.compositor.pane_at(screen_col, screen_row, cols, rows) else {
            return;
        };
        if pane.buffer_id != buffer_id {
            return;
        }
        if self.editor.active_buffer().id != buffer_id && !self.editor.switch_to_buffer(buffer_id) {
            return;
        }

        let Some(target) = screen_to_doc_pos(
            self.editor.active_buffer(),
            pane.rect,
            screen_col,
            screen_row,
            self.config.show_line_number,
            self.config.line_number_width,
        ) else {
            return;
        };

        // Once the pointer has moved off the anchor, a drag selection starts.
        // Any in-flight rapid-click expand chain is irrelevant here — clear it
        // so the next click lands fresh.
        self.expand_chain = None;

        let head = target.char_pos;
        let doc = self.editor.active_buffer_mut();
        if head == anchor_pos {
            doc.clear_anchor();
            set_cursor_to_pos(doc, head);
        } else {
            // Selection keeps the mouse-down position as the anchor; `head`
            // follows the pointer. `selection_range` normalises ordering,
            // so this handles both forward and backward drags.
            doc.selections[0] = Some(Selection::tail_on_forward(anchor_pos, head));
            set_cursor_to_pos(doc, head);
        }
    }
}

fn set_cursor_to_pos(doc: &mut Document, pos: usize) {
    let line = doc.rope.char_to_line(pos.min(doc.rope.len_chars()));
    let line_start = doc.rope.line_to_char(line);
    let col = pos.saturating_sub(line_start);
    doc.set_cursor_line_char(line, col);
}

/// Inverse of `text_view::cursor_for_buffer`. Maps a terminal cell at
/// `(screen_col, screen_row)` inside `pane` to a document `(line, char_pos)`.
/// Returns None if the click is outside the pane.
fn screen_to_doc_pos(
    doc: &Document,
    pane: PaneRect,
    screen_col: u16,
    screen_row: u16,
    show_line_number: bool,
    line_number_width: usize,
) -> Option<ClickTarget> {
    let col = usize::from(screen_col);
    let row = usize::from(screen_row);
    if col < pane.x || col >= pane.x + pane.width {
        return None;
    }
    if row < pane.y || row >= pane.y + pane.height {
        return None;
    }

    let total_lines = doc.rope.len_lines();
    let gutter_w = reserved_left_gutter_width(total_lines, show_line_number, line_number_width);

    let row_in_pane = row - pane.y;
    let raw_line = doc.scroll_offset.saturating_add(row_in_pane);
    let line = if total_lines == 0 {
        0
    } else {
        raw_line.min(total_lines - 1)
    };

    let col_in_pane = col - pane.x;
    if col_in_pane < gutter_w {
        let line_start = if total_lines == 0 {
            0
        } else {
            doc.rope.line_to_char(line)
        };
        return Some(ClickTarget {
            line,
            char_pos: line_start,
            on_gutter: true,
        });
    }
    let col_in_content = col_in_pane - gutter_w;

    let line_str = if total_lines == 0 {
        String::new()
    } else {
        doc.rope.line(line).to_string()
    };
    let line_display = line_str.trim_end_matches('\n');

    let available = pane.width.saturating_sub(gutter_w);
    let horizontal = slice_display_window(line_display, doc.horizontal_scroll_offset, available);
    let view_start_col = horizontal.start_col;
    let target_display_col = view_start_col + col_in_content;

    // Walk display columns to find the char index containing target_display_col.
    let mut col_acc = 0usize;
    let mut char_idx = 0usize;
    let mut found = false;
    for ch in line_display.chars() {
        let ch_w = char_display_width(ch);
        if col_acc + ch_w > target_display_col {
            found = true;
            break;
        }
        col_acc += ch_w;
        char_idx += 1;
    }
    if !found {
        // Click is past EOL — snap to one-past-last-char.
        char_idx = line_display.chars().count();
    }

    let line_start = if total_lines == 0 {
        0
    } else {
        doc.rope.line_to_char(line)
    };
    Some(ClickTarget {
        line,
        char_pos: line_start + char_idx,
        on_gutter: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use ropey::Rope;

    fn doc_from(s: &str) -> Document {
        let mut doc = Document::new_scratch(1);
        doc.rope = Rope::from_str(s);
        doc
    }

    fn pane(width: usize, height: usize) -> PaneRect {
        PaneRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn screen_to_doc_pos_basic_ascii_no_line_numbers() {
        let doc = doc_from("hello world\nbye\n");
        // gutter_w = 1 (no line numbers, just git lane). Screen col 7 →
        // content col 6 → display col 6 → 'w' at char index 6.
        let target = screen_to_doc_pos(&doc, pane(40, 10), 7, 0, false, 4).unwrap();
        assert_eq!(target.line, 0);
        assert_eq!(target.char_pos, 6); // 'w' in "world"
        assert!(!target.on_gutter);
    }

    #[test]
    fn screen_to_doc_pos_handles_tabs() {
        let doc = doc_from("a\tb\n");
        // gutter_w=1; click at screen col 6 means content col 5 — past tab into 'b'
        let target = screen_to_doc_pos(&doc, pane(40, 10), 6, 0, false, 4).unwrap();
        assert_eq!(target.char_pos, 2); // 'b'
    }

    #[test]
    fn screen_to_doc_pos_past_eol_snaps_to_last() {
        let doc = doc_from("hi\n");
        let target = screen_to_doc_pos(&doc, pane(40, 10), 30, 0, false, 4).unwrap();
        assert_eq!(target.char_pos, 2); // one past last char ('i')
    }

    #[test]
    fn screen_to_doc_pos_gutter_click() {
        let doc = doc_from("alpha\nbeta\n");
        // line numbers on → gutter occupies cols 0..(>1)
        let target = screen_to_doc_pos(&doc, pane(40, 10), 0, 1, true, 4).unwrap();
        assert!(target.on_gutter);
        assert_eq!(target.line, 1);
        assert_eq!(target.char_pos, 6); // start of "beta"
    }

    #[test]
    fn screen_to_doc_pos_outside_pane_returns_none() {
        let doc = doc_from("x\n");
        assert!(screen_to_doc_pos(&doc, pane(10, 5), 20, 2, false, 4).is_none());
        assert!(screen_to_doc_pos(&doc, pane(10, 5), 5, 8, false, 4).is_none());
    }

    #[test]
    fn screen_to_doc_pos_horizontal_scroll() {
        let mut doc = doc_from("0123456789abcdef\n");
        doc.horizontal_scroll_offset = 5;
        // gutter_w=1; click at screen col 1 → content col 0 → display col 5 → '5'
        let target = screen_to_doc_pos(&doc, pane(40, 10), 1, 0, false, 4).unwrap();
        assert_eq!(target.char_pos, 5);
    }

    #[test]
    fn screen_to_doc_pos_cjk_fullwidth() {
        // Each CJK char is 2 display columns wide.
        let doc = doc_from("吾輩は猫\n");
        // gutter_w=1; clicking at screen col 4 → content col 3 → display col 3 →
        // first char width=2 ends at col 2, so col 3 is inside second char (輩).
        let target = screen_to_doc_pos(&doc, pane(40, 10), 4, 0, false, 4).unwrap();
        assert_eq!(target.char_pos, 1); // '輩'
    }

    #[test]
    fn screen_to_doc_pos_row_past_last_line_clamps() {
        let doc = doc_from("only\n");
        // total_lines = 2 (one for "only\n", one for the trailing empty line).
        // Row 9 is past the end → clamped to last line (1, the empty trailing).
        let target = screen_to_doc_pos(&doc, pane(20, 10), 5, 9, false, 4).unwrap();
        assert_eq!(target.line, 1);
    }
}

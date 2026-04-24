//! "Expand selection" engine — picks the next-larger structural unit
//! containing `origin`. Shared between the mouse-click handler and the
//! visual-mode `v` key.

use super::*;

use crate::core::buffer::BufferId;
use crate::core::document::expand::{enclosing_brackets, line_range_at, word_range_at};
use crate::core::document::Selection;
use crate::core::editor::Editor;

/// Per-buffer state for an in-flight expand chain. Both mouse clicks and
/// visual `v` presses contribute to the same chain.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExpandChain {
    pub buffer: BufferId,
    /// Anchor position used to compute candidates. Stays fixed across the
    /// whole chain; doesn't drift with mouse jitter or selection growth.
    pub origin: usize,
    /// The selection range produced by the most recent expand step. Used to
    /// detect "did the user move the cursor or modify the selection between
    /// presses?" — when the active selection no longer matches, the chain
    /// resets.
    pub last_range: Option<(usize, usize)>,
    /// Cursor position we left things at after the last expand (= last_range.1).
    /// Used to detect cursor drift between visual `v` presses.
    pub expected_cursor: usize,
    /// Time of the last click — only consulted by the mouse path for the
    /// rapid-click window.
    pub last_click_time: std::time::Instant,
    /// Position of the last click — only consulted by the mouse path for the
    /// proximity check (so a click that drifts a couple cells over the same
    /// word still counts as "same spot").
    pub last_click_pos: usize,
}

/// Pick the smallest enclosing structural range that strictly grows beyond
/// `current` (or any enclosing range when `current` is None).
///
/// Candidates considered, smallest to largest:
/// 1. word at `origin`
/// 2. each enclosing `() / [] / {}` pair, innermost first
/// 3. line containing `origin`
/// 4. enclosing markdown `block_quote` / `fenced_code_block` / `indented_code_block`
///    (only when the buffer's language is Markdown and a tree is parsed)
/// 5. whole file (final fallback so the chain always has somewhere to go)
///
/// All candidates must contain `origin`; when `current` is Some, candidates
/// must also fully contain `current` (selections only grow, never shrink).
pub(crate) fn expand_selection(
    editor: &Editor,
    buffer_id: BufferId,
    origin: usize,
    current: Option<(usize, usize)>,
) -> Option<(usize, usize)> {
    let doc = editor.buffer_by_id(buffer_id)?;
    let rope = &doc.rope;

    let mut candidates: Vec<(usize, usize)> = Vec::new();
    if let Some(r) = word_range_at(rope, origin) {
        candidates.push(r);
    }
    candidates.extend(enclosing_brackets(rope, origin));
    candidates.push(line_range_at(rope, origin));
    if editor.language_name_for(buffer_id) == Some("Markdown")
        && let Some(r) = markdown_block_range(editor, buffer_id, origin)
    {
        candidates.push(r);
    }
    candidates.push((0, rope.len_chars()));

    let cur_size = current.map(|(s, e)| e.saturating_sub(s)).unwrap_or(0);
    candidates
        .into_iter()
        .filter(|(s, e)| *s <= origin && *e >= origin)
        .filter(|(s, e)| (e - s) > cur_size)
        .filter(|(s, e)| match current {
            Some((cs, ce)) => *s <= cs && *e >= ce,
            None => true,
        })
        .min_by_key(|(s, e)| e - s)
}

impl App {
    /// Handle a `v` press in visual mode: expand the selection one structural
    /// step. If the cursor has moved (or the selection was modified) since
    /// the last expand, restart the chain at the current cursor position.
    pub(super) fn handle_visual_expand(&mut self) {
        // Make sure tree-sitter is up to date before the engine reads it.
        self.editor.update_highlights_if_dirty();

        let buffer_id = self.editor.active_buffer().id;
        let cursor_now = self.editor.active_buffer().cursors[0];
        let selection_now = self.editor.active_buffer().selection_range();

        let chain_continues = match self.expand_chain {
            Some(chain) => {
                chain.buffer == buffer_id
                    && chain.expected_cursor == cursor_now
                    && chain.last_range == selection_now
            }
            None => false,
        };

        let (origin, current) = if chain_continues {
            let chain = self.expand_chain.expect("checked above");
            (chain.origin, chain.last_range)
        } else {
            // Fresh start: anchor at current cursor, treat as "no extent yet".
            (cursor_now, None)
        };

        let Some((s, e)) = expand_selection(&self.editor, buffer_id, origin, current) else {
            // Nothing larger to grow into — refresh chain timing so subsequent
            // `v` presses still continue from the same origin.
            if let Some(chain) = self.expand_chain {
                self.expand_chain = Some(ExpandChain {
                    last_click_time: std::time::Instant::now(),
                    ..chain
                });
            }
            return;
        };

        let doc = self.editor.active_buffer_mut();
        doc.selection = Some(Selection::tail_on_forward(s, e));
        doc.cursors[0] = e;

        self.expand_chain = Some(ExpandChain {
            buffer: buffer_id,
            origin,
            last_range: Some((s, e)),
            expected_cursor: e,
            last_click_time: std::time::Instant::now(),
            last_click_pos: origin,
        });
    }
}

fn markdown_block_range(
    editor: &Editor,
    buffer_id: BufferId,
    origin: usize,
) -> Option<(usize, usize)> {
    let tree = editor.highlight_manager.tree(buffer_id)?;
    let doc = editor.buffer_by_id(buffer_id)?;
    let pos = origin.min(doc.rope.len_chars());
    let byte = doc.rope.char_to_byte(pos);
    let mut node = tree.root_node().descendant_for_byte_range(byte, byte)?;
    loop {
        match node.kind() {
            "block_quote" | "fenced_code_block" | "indented_code_block" => {
                let s = doc.rope.byte_to_char(node.start_byte());
                let e = doc.rope.byte_to_char(node.end_byte().min(doc.rope.len_bytes()));
                if s >= e {
                    return None;
                }
                return Some((s, e));
            }
            _ => {}
        }
        node = node.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::editor::Editor;

    fn editor_with(text: &str) -> Editor {
        let mut e = Editor::new();
        e.active_buffer_mut().insert_text(text);
        e
    }

    #[test]
    fn first_step_from_no_selection_picks_word() {
        let e = editor_with("hello world\n");
        let id = e.active_buffer().id;
        // origin on 'e' in "hello" (char index 1)
        let r = expand_selection(&e, id, 1, None).unwrap();
        assert_eq!(r, (0, 5));
    }

    #[test]
    fn next_step_after_word_picks_innermost_bracket_when_smaller_than_line() {
        // "foo(bar baz qux)\n" — origin on 'b' in "bar".
        // word = "bar" (4..7). bracket pair = "(bar baz qux)" (3..16). line = (0..17).
        let e = editor_with("foo(bar baz qux)\n");
        let id = e.active_buffer().id;
        let after_word = (4, 7);
        let r = expand_selection(&e, id, 4, Some(after_word)).unwrap();
        assert_eq!(r, (3, 16));
    }

    #[test]
    fn next_step_after_bracket_picks_line_when_no_outer_bracket() {
        let e = editor_with("foo(bar baz qux)\n");
        let id = e.active_buffer().id;
        let after_bracket = (3, 16);
        let r = expand_selection(&e, id, 4, Some(after_bracket)).unwrap();
        assert_eq!(r, (0, 17));
    }

    #[test]
    fn step_after_line_picks_outer_bracket_when_present() {
        // "{\n   foo;\n   bar();\n}\n" — origin in "foo".
        // After "line of foo", next bigger should be the {} block.
        let text = "{\n   foo;\n   bar();\n}\n";
        let e = editor_with(text);
        let id = e.active_buffer().id;
        // line "   foo;\n" is chars 2..10 (newline at 1, then 3 spaces+foo;+\n).
        // Verify: walk text.
        // chars: { \n _ _ _ f o o ; \n _ _ _ b a r ( ) ; \n }
        // 0:{ 1:\n 2:' ' 3:' ' 4:' ' 5:f 6:o 7:o 8:; 9:\n 10:' ' ...
        // So origin on 'f' = 5. line range = (2, 10).
        let after_line = (2, 10);
        let r = expand_selection(&e, id, 5, Some(after_line)).unwrap();
        // Outer bracket {} = (0, 22) (covers all chars including final \n? No, } is at 21, +1 = 22.)
        let len = e.active_buffer().rope.len_chars();
        // Expect the {} pair.
        assert!(r.0 == 0 && r.1 < len, "expected {{ pair, got {:?}", r);
    }

    #[test]
    fn step_after_largest_returns_whole_file() {
        let e = editor_with("hello\n");
        let id = e.active_buffer().id;
        let len = e.active_buffer().rope.len_chars();
        // Current = whole file already; should return None (nothing larger).
        let r = expand_selection(&e, id, 1, Some((0, len)));
        assert!(r.is_none());
    }

    #[test]
    fn empty_buffer_returns_none() {
        let e = editor_with("");
        let id = e.active_buffer().id;
        let r = expand_selection(&e, id, 0, None);
        // Whole-file candidate is (0, 0) which has zero size — filtered out.
        assert!(r.is_none());
    }
}

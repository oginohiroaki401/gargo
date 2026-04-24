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
/// The candidate set is the UNION of every structural level we know how to
/// derive at `origin`:
///   - the word at `origin`
///   - every enclosing `() / [] / {}` pair (all depths)
///   - the line containing `origin`
///   - every enclosing tree-sitter AST node (all depths, for any language
///     with a parser registered)
///   - the whole file
///
/// Which one gets picked at each step is decided dynamically: we filter to
/// candidates that contain `origin` and strictly contain `current`, then take
/// the smallest. Over successive calls the selection climbs the tightest
/// available enclosure — "word or bracket or line or block, whichever is
/// closest but broader" — rather than following a fixed word → bracket →
/// line → block ladder.
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
    candidates.extend(ast_ancestor_ranges(editor, buffer_id, origin));
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

/// Every enclosing tree-sitter AST node at `origin`, smallest first. Empty
/// when the buffer has no parser registered (plain text, unknown language).
///
/// Each ancestor contributes a candidate, letting `expand_selection` step
/// through the structural tree one level at a time regardless of language.
/// The filter in `expand_selection` will only pick a node if it's the
/// smallest candidate strictly larger than the current selection, so the
/// selection walks the tree at whatever granularity the tree actually has.
fn ast_ancestor_ranges(
    editor: &Editor,
    buffer_id: BufferId,
    origin: usize,
) -> Vec<(usize, usize)> {
    let Some(tree) = editor.highlight_manager.tree(buffer_id) else {
        return Vec::new();
    };
    let Some(doc) = editor.buffer_by_id(buffer_id) else {
        return Vec::new();
    };
    let pos = origin.min(doc.rope.len_chars());
    let byte = doc.rope.char_to_byte(pos);
    let Some(mut node) = tree.root_node().descendant_for_byte_range(byte, byte) else {
        return Vec::new();
    };

    let len_bytes = doc.rope.len_bytes();
    let mut ranges = Vec::new();
    loop {
        let start_byte = node.start_byte().min(len_bytes);
        let end_byte = node.end_byte().min(len_bytes);
        if start_byte < end_byte {
            let s = doc.rope.byte_to_char(start_byte);
            let e = doc.rope.byte_to_char(end_byte);
            if s < e {
                ranges.push((s, e));
            }
        }
        match node.parent() {
            Some(p) => node = p,
            None => break,
        }
    }
    ranges
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

    #[test]
    fn ast_ancestors_empty_when_no_tree() {
        // No language registered → no tree → no AST candidates, and the
        // other candidate sources still work.
        let e = editor_with("hello world\n");
        let id = e.active_buffer().id;
        assert!(ast_ancestor_ranges(&e, id, 3).is_empty());
    }

    #[test]
    fn ast_ancestors_drive_dynamic_steps_on_rust_source() {
        // With a tree-sitter parser attached, the AST ancestors participate
        // in the same "smallest larger" pick as word/bracket/line. Each
        // successive call must return a strictly larger enclosing range
        // that contains the previous one — the core invariant of the
        // dynamic ladder.
        let mut e = Editor::new();
        e.active_buffer_mut().insert_text("fn f() { let x = 1 + 2; }\n");
        e.register_highlights_for_extension("rs");
        e.update_highlights_if_dirty();
        let id = e.active_buffer().id;

        // Origin on 'x' (the binding name). Walk the ladder and assert that
        // every step grows and fully contains the previous selection.
        let origin = e
            .active_buffer()
            .rope
            .to_string()
            .find('x')
            .expect("'x' is in the text");
        let mut prev: Option<(usize, usize)> = None;
        for _ in 0..6 {
            let next = expand_selection(&e, id, origin, prev);
            let Some((s, ne)) = next else { break };
            if let Some((ps, pe)) = prev {
                assert!(s <= ps && ne >= pe, "step must contain previous");
                assert!((ne - s) > (pe - ps), "step must strictly grow");
            }
            prev = Some((s, ne));
        }
        // Final step must reach whole file.
        let len = e.active_buffer().rope.len_chars();
        assert_eq!(prev, Some((0, len)));
    }
}

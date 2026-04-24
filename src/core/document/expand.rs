//! Pure rope-level helpers for "expand selection" semantics.
//!
//! Each function returns a half-open `(start, end)` char range (or `None`),
//! never mutates the document, and treats the rope as opaque text — no
//! language-specific knowledge lives here.

use ropey::Rope;

use crate::core::buffer::char_class;

/// Range of the word/whitespace/punctuation run at `pos`.
/// Newlines are hard boundaries. Returns `None` for empty rope or when `pos`
/// lands on a newline.
pub fn word_range_at(rope: &Rope, pos: usize) -> Option<(usize, usize)> {
    let len = rope.len_chars();
    if len == 0 {
        return None;
    }
    let pos = pos.min(len.saturating_sub(1));
    let pivot = rope.char(pos);
    if pivot == '\n' {
        return None;
    }
    let cls = char_class(pivot);
    let mut start = pos;
    while start > 0 {
        let c = rope.char(start - 1);
        if c == '\n' || char_class(c) != cls {
            break;
        }
        start -= 1;
    }
    let mut end = pos;
    while end < len {
        let c = rope.char(end);
        if c == '\n' || char_class(c) != cls {
            break;
        }
        end += 1;
    }
    if start == end {
        None
    } else {
        Some((start, end))
    }
}

/// Range of the line containing `pos`, including the trailing newline if any.
pub fn line_range_at(rope: &Rope, pos: usize) -> (usize, usize) {
    let total_lines = rope.len_lines();
    if total_lines == 0 {
        return (0, 0);
    }
    let pos = pos.min(rope.len_chars());
    let line = rope.char_to_line(pos);
    let line_start = rope.line_to_char(line);
    let line_end = if line + 1 < total_lines {
        rope.line_to_char(line + 1)
    } else {
        rope.len_chars()
    };
    (line_start, line_end)
}

/// All bracket pairs `(start, end)` containing `pos`, sorted innermost first
/// (smallest range first). Considers `()`, `[]`, and `{}`. The returned ranges
/// include the brackets themselves.
///
/// Bracket matching uses simple depth counting, with no awareness of strings
/// or comments — brackets inside string literals will participate in pairing.
/// That's acceptable for this feature; users get sensible structural selection
/// in most code.
pub fn enclosing_brackets(rope: &Rope, pos: usize) -> Vec<(usize, usize)> {
    let mut paren_stack: Vec<usize> = Vec::new();
    let mut bracket_stack: Vec<usize> = Vec::new();
    let mut brace_stack: Vec<usize> = Vec::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();

    let len = rope.len_chars();
    if len == 0 {
        return pairs;
    }

    let close_pair = |stack: &mut Vec<usize>, close_idx: usize, pairs: &mut Vec<(usize, usize)>| {
        if let Some(open_pos) = stack.pop() {
            let pair = (open_pos, close_idx + 1);
            if pair.0 <= pos && pos < pair.1 {
                pairs.push(pair);
            }
        }
    };

    for (i, ch) in rope.chars().enumerate() {
        match ch {
            '(' => paren_stack.push(i),
            ')' => close_pair(&mut paren_stack, i, &mut pairs),
            '[' => bracket_stack.push(i),
            ']' => close_pair(&mut bracket_stack, i, &mut pairs),
            '{' => brace_stack.push(i),
            '}' => close_pair(&mut brace_stack, i, &mut pairs),
            _ => {}
        }
    }
    pairs.sort_by_key(|(s, e)| e - s);
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn word_range_basic() {
        assert_eq!(word_range_at(&r("hello world"), 2), Some((0, 5)));
        assert_eq!(word_range_at(&r("hello world"), 6), Some((6, 11)));
    }

    #[test]
    fn word_range_punctuation() {
        assert_eq!(word_range_at(&r("foo::bar"), 3), Some((3, 5)));
    }

    #[test]
    fn word_range_on_newline_is_none() {
        assert_eq!(word_range_at(&r("foo\nbar"), 3), None);
    }

    #[test]
    fn word_range_empty_rope() {
        assert_eq!(word_range_at(&r(""), 0), None);
    }

    #[test]
    fn line_range_basic() {
        assert_eq!(line_range_at(&r("aaa\nbbb\nccc"), 5), (4, 8));
    }

    #[test]
    fn line_range_last_line_no_lf() {
        assert_eq!(line_range_at(&r("first\nlast"), 7), (6, 10));
    }

    #[test]
    fn brackets_innermost_first() {
        // pos at 'x' inside the inner ()
        let pairs = enclosing_brackets(&r("foo(bar(x)y)z"), 8);
        // Inner: (x) at chars 7..10. Outer: (bar(x)y) at chars 3..12.
        assert_eq!(pairs, vec![(7, 10), (3, 12)]);
    }

    #[test]
    fn brackets_mixed_kinds() {
        // pos on 'b' inside [b], wrapped by {} … {[b]}
        let pairs = enclosing_brackets(&r("a{[b]}c"), 3);
        assert_eq!(pairs, vec![(2, 5), (1, 6)]);
    }

    #[test]
    fn brackets_none_when_unmatched() {
        let pairs = enclosing_brackets(&r("foo bar"), 2);
        assert!(pairs.is_empty());
    }

    #[test]
    fn brackets_cursor_on_opener_includes_pair() {
        let pairs = enclosing_brackets(&r("(abc)"), 0);
        assert_eq!(pairs, vec![(0, 5)]);
    }

    #[test]
    fn brackets_cursor_on_closer_includes_pair() {
        let pairs = enclosing_brackets(&r("(abc)"), 4);
        assert_eq!(pairs, vec![(0, 5)]);
    }
}

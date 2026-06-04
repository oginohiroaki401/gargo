use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Language, Parser, Point, Query, QueryCursor, Tree};

use crate::core::buffer::{BufferId, EditEvent};
use crate::syntax::language::{LanguageDef, LanguageRegistry};

/// A colored span within a line.
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    /// Byte offset from the start of the line.
    pub start: usize,
    /// Byte offset from the start of the line (exclusive).
    pub end: usize,
    /// Capture name from the tree-sitter query (e.g. "keyword", "string").
    pub capture_name: String,
}

struct BufferHighlight {
    parser: Parser,
    tree: Option<Tree>,
    query: Arc<Query>,
    indent_query: Option<Arc<Query>>,
    visible_cache: RefCell<Option<VisibleHighlightCache>>,
    #[allow(dead_code)]
    language: Language,
}

pub struct HighlightManager {
    highlights: HashMap<BufferId, BufferHighlight>,
}

const HIGHLIGHT_TEXT_CACHE_CAPACITY: usize = 64;

struct VisibleHighlightCache {
    start_line: usize,
    end_line: usize,
    spans: HashMap<usize, Vec<HighlightSpan>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct HighlightTextCacheKey {
    language_name: &'static str,
    byte_len: usize,
    line_count: usize,
    content_hash: u64,
}

struct HighlightTextCache {
    order: VecDeque<HighlightTextCacheKey>,
    entries: HashMap<HighlightTextCacheKey, HashMap<usize, Vec<HighlightSpan>>>,
}

impl HighlightTextCache {
    fn new() -> Self {
        Self {
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, key: HighlightTextCacheKey) -> Option<HashMap<usize, Vec<HighlightSpan>>> {
        let spans = self.entries.get(&key)?.clone();
        self.touch(key);
        Some(spans)
    }

    fn insert(&mut self, key: HighlightTextCacheKey, spans: HashMap<usize, Vec<HighlightSpan>>) {
        if self.entries.insert(key, spans).is_some() {
            self.touch(key);
            return;
        }

        self.order.push_back(key);
        while self.order.len() > HIGHLIGHT_TEXT_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, key: HighlightTextCacheKey) {
        if let Some(position) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(position);
        }
        self.order.push_back(key);
    }
}

fn highlight_text_cache() -> &'static Mutex<HighlightTextCache> {
    static CACHE: OnceLock<Mutex<HighlightTextCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HighlightTextCache::new()))
}

/// Compile and cache a tree-sitter `Query` from a `&'static` query source,
/// returning a shared handle. `Query::new` compiles the query string (the
/// dominant cost of a highlight request), so memoizing it turns repeated
/// highlights of the same language from a cache miss into a near-free clone.
///
/// Keyed by the source string's pointer: each grammar's HIGHLIGHTS / indent /
/// tags query is a distinct `&'static str`, so the pointer uniquely identifies a
/// `(language, query)` pair. Shared across the terminal editor and the server's
/// `/api/highlight` + `/blob` paths.
pub(crate) fn compiled_query(language: &Language, src: &'static str) -> Option<Arc<Query>> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Query>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = src.as_ptr() as usize;

    if let Some(query) = cache.lock().expect("query cache lock poisoned").get(&key) {
        return Some(query.clone());
    }

    let query = Arc::new(Query::new(language, src).ok()?);
    cache
        .lock()
        .expect("query cache lock poisoned")
        .insert(key, query.clone());
    Some(query)
}

fn highlight_text_cache_key(text: &str, lang_def: &LanguageDef) -> HighlightTextCacheKey {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lang_def.name.hash(&mut hasher);
    text.hash(&mut hasher);
    HighlightTextCacheKey {
        language_name: lang_def.name,
        byte_len: text.len(),
        line_count: text.lines().count(),
        content_hash: hasher.finish(),
    }
}

struct RopeChunkIter<'a> {
    rope: &'a Rope,
    byte: usize,
    end: usize,
}

impl<'a> Iterator for RopeChunkIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.byte >= self.end {
            return None;
        }
        let (chunk, chunk_start, _, _) = self.rope.chunk_at_byte(self.byte);
        let chunk_bytes = chunk.as_bytes();
        let start = self.byte - chunk_start;
        let end = (self.end - chunk_start).min(chunk_bytes.len());
        let slice = &chunk_bytes[start..end];
        self.byte = chunk_start + end;
        Some(slice)
    }
}

impl Default for HighlightManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HighlightManager {
    pub fn new() -> Self {
        Self {
            highlights: HashMap::new(),
        }
    }

    /// Register a buffer for highlighting. Performs the initial parse.
    pub fn register_buffer(&mut self, buf_id: BufferId, rope: &Rope, lang_def: &LanguageDef) {
        let language = LanguageRegistry::ts_language(lang_def.language_fn);
        let mut parser = Parser::new();
        if parser.set_language(&language).is_err() {
            return;
        }

        let query = match compiled_query(&language, lang_def.highlight_query) {
            Some(q) => q,
            None => return,
        };

        let indent_query = lang_def
            .indent_query
            .and_then(|iq| compiled_query(&language, iq));

        let tree = parse_rope(&mut parser, rope, None);

        self.highlights.insert(
            buf_id,
            BufferHighlight {
                parser,
                tree,
                query,
                indent_query,
                visible_cache: RefCell::new(None),
                language,
            },
        );
    }

    /// Remove a buffer's highlight state.
    pub fn unregister_buffer(&mut self, buf_id: BufferId) {
        self.highlights.remove(&buf_id);
    }

    /// Apply pending edits and re-parse incrementally.
    pub fn update(&mut self, buf_id: BufferId, rope: &Rope, edits: &[EditEvent]) {
        let Some(bh) = self.highlights.get_mut(&buf_id) else {
            return;
        };

        if let Some(ref mut tree) = bh.tree {
            for edit in edits {
                tree.edit(&InputEdit {
                    start_byte: edit.start_byte,
                    old_end_byte: edit.old_end_byte,
                    new_end_byte: edit.new_end_byte,
                    start_position: Point {
                        row: edit.start_position.0,
                        column: edit.start_position.1,
                    },
                    old_end_position: Point {
                        row: edit.old_end_position.0,
                        column: edit.old_end_position.1,
                    },
                    new_end_position: Point {
                        row: edit.new_end_position.0,
                        column: edit.new_end_position.1,
                    },
                });
            }
        }

        let t0 = std::time::Instant::now();
        bh.tree = parse_rope(&mut bh.parser, rope, bh.tree.as_ref());
        let parse_elapsed = t0.elapsed();
        if parse_elapsed.as_millis() > 10 {
            eprintln!(
                "[gargo] tree-sitter parse: {} bytes, took {:?}",
                rope.len_bytes(),
                parse_elapsed
            );
        }
        *bh.visible_cache.get_mut() = None;
    }

    /// Get a reference to the parsed tree for a buffer.
    pub fn tree(&self, buf_id: BufferId) -> Option<&Tree> {
        self.highlights.get(&buf_id).and_then(|bh| bh.tree.as_ref())
    }

    /// Get a reference to the indent query for a buffer.
    pub fn indent_query(&self, buf_id: BufferId) -> Option<&Query> {
        self.highlights
            .get(&buf_id)
            .and_then(|bh| bh.indent_query.as_deref())
    }

    /// Query highlight spans for visible lines. Returns a map from line index to spans.
    pub fn query_visible(
        &self,
        buf_id: BufferId,
        rope: &Rope,
        start_line: usize,
        end_line: usize,
    ) -> HashMap<usize, Vec<HighlightSpan>> {
        let mut result: HashMap<usize, Vec<HighlightSpan>> = HashMap::new();

        let Some(bh) = self.highlights.get(&buf_id) else {
            return result;
        };
        let Some(ref tree) = bh.tree else {
            return result;
        };

        let total_lines = rope.len_lines();
        let end_line = end_line.min(total_lines);
        if start_line >= end_line {
            return result;
        }

        if let Some(cached) = bh.visible_cache.borrow().as_ref()
            && cached.start_line == start_line
            && cached.end_line == end_line
        {
            return cached.spans.clone();
        }

        let start_byte = rope.line_to_byte(start_line);
        let end_byte = if end_line >= total_lines {
            rope.len_bytes()
        } else {
            rope.line_to_byte(end_line)
        };

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);

        let root = tree.root_node();

        let text_provider = |node: tree_sitter::Node| {
            let range = node.byte_range();
            RopeChunkIter {
                rope,
                byte: range.start,
                end: range.end,
            }
        };
        let mut captures = cursor.captures(&bh.query, root, text_provider);

        while let Some((match_, capture_index)) = captures.next() {
            let capture = &match_.captures[*capture_index];
            let capture_name = &bh.query.capture_names()[capture.index as usize];
            let node = capture.node;
            let node_start = node.start_byte();
            let node_end = node.end_byte();

            // Distribute spans across lines
            let node_start_line = rope.byte_to_line(node_start);
            let node_end_line = if node_end > node_start {
                rope.byte_to_line(node_end.saturating_sub(1))
            } else {
                node_start_line
            };

            for line_idx in node_start_line..=node_end_line {
                if line_idx < start_line || line_idx >= end_line {
                    continue;
                }
                let line_byte_start = rope.line_to_byte(line_idx);
                let line_byte_end = if line_idx + 1 < total_lines {
                    rope.line_to_byte(line_idx + 1)
                } else {
                    rope.len_bytes()
                };

                let span_start = node_start.max(line_byte_start) - line_byte_start;
                let span_end = node_end.min(line_byte_end) - line_byte_start;

                if span_start < span_end {
                    result.entry(line_idx).or_default().push(HighlightSpan {
                        start: span_start,
                        end: span_end,
                        capture_name: capture_name.to_string(),
                    });
                }
            }
        }

        // Sort spans within each line by start position
        for spans in result.values_mut() {
            spans.sort_by_key(|s| s.start);
        }

        *bh.visible_cache.borrow_mut() = Some(VisibleHighlightCache {
            start_line,
            end_line,
            spans: result.clone(),
        });

        result
    }
}

/// Parse a plain `&str` with a given `LanguageDef` and return highlight spans
/// keyed by line index. This is a standalone function that doesn't require a
/// registered buffer — useful for preview panels, etc.
pub fn highlight_text(text: &str, lang_def: &LanguageDef) -> HashMap<usize, Vec<HighlightSpan>> {
    let cache_key = highlight_text_cache_key(text, lang_def);
    if let Some(cached) = highlight_text_cache()
        .lock()
        .expect("highlight text cache lock poisoned")
        .get(cache_key)
    {
        return cached;
    }

    let mut result: HashMap<usize, Vec<HighlightSpan>> = HashMap::new();

    let language = LanguageRegistry::ts_language(lang_def.language_fn);
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return result;
    }
    let query = match compiled_query(&language, lang_def.highlight_query) {
        Some(q) => q,
        None => return result,
    };

    let tree = match parser.parse(text, None) {
        Some(t) => t,
        None => return result,
    };

    let source_bytes = text.as_bytes();
    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, root, source_bytes);

    // Pre-compute line byte offsets
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(text.bytes().enumerate().filter_map(
            |(i, b)| {
                if b == b'\n' { Some(i + 1) } else { None }
            },
        ))
        .collect();
    let total_lines = line_starts.len();

    let line_end = |line_idx: usize| -> usize {
        if line_idx + 1 < total_lines {
            line_starts[line_idx + 1]
        } else {
            text.len()
        }
    };

    let byte_to_line = |byte_offset: usize| -> usize {
        match line_starts.binary_search(&byte_offset) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
    };

    while let Some((match_, capture_index)) = captures.next() {
        let capture = &match_.captures[*capture_index];
        let capture_name = &query.capture_names()[capture.index as usize];
        let node = capture.node;
        let node_start = node.start_byte();
        let node_end = node.end_byte();

        let node_start_line = byte_to_line(node_start);
        let node_end_line = if node_end > node_start {
            byte_to_line(node_end.saturating_sub(1))
        } else {
            node_start_line
        };

        for (line_idx, &line_byte_start) in line_starts
            .iter()
            .enumerate()
            .take(node_end_line + 1)
            .skip(node_start_line)
        {
            let line_byte_end = line_end(line_idx);

            let span_start = node_start.max(line_byte_start) - line_byte_start;
            let span_end = node_end.min(line_byte_end) - line_byte_start;

            if span_start < span_end {
                result.entry(line_idx).or_default().push(HighlightSpan {
                    start: span_start,
                    end: span_end,
                    capture_name: capture_name.to_string(),
                });
            }
        }
    }

    for spans in result.values_mut() {
        spans.sort_by_key(|s| s.start);
    }

    highlight_text_cache()
        .lock()
        .expect("highlight text cache lock poisoned")
        .insert(cache_key, result.clone());

    result
}

/// Parse a Rope using tree-sitter's callback API (zero-copy).
fn parse_rope(parser: &mut Parser, rope: &Rope, old_tree: Option<&Tree>) -> Option<Tree> {
    let len = rope.len_bytes();
    parser.parse_with_options(
        &mut |byte_offset: usize, _position: Point| -> &[u8] {
            if byte_offset >= len {
                return b"";
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte_offset);
            &chunk.as_bytes()[byte_offset - chunk_start..]
        },
        old_tree,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_query_rust() {
        let source = r#"fn main() {
    let x = 42;
    println!("hello");
}
"#;
        let rope = Rope::from_str(source);
        let registry = crate::syntax::language::LanguageRegistry::new();
        let lang_def = registry.detect_by_extension("test.rs").unwrap();

        let mut mgr = HighlightManager::new();
        mgr.register_buffer(1, &rope, lang_def);

        let spans = mgr.query_visible(1, &rope, 0, 4);
        // Should have some spans for keywords, strings, numbers, etc.
        assert!(!spans.is_empty(), "Expected highlight spans for Rust code");

        // Line 0 should have "fn" as keyword
        let line0 = spans.get(&0).expect("Expected spans for line 0");
        let has_keyword = line0.iter().any(|s| s.capture_name.starts_with("keyword"));
        assert!(has_keyword, "Expected keyword capture on line 0");
    }

    #[test]
    fn incremental_update() {
        let mut rope = Rope::from_str("let x = 1;\n");
        let registry = crate::syntax::language::LanguageRegistry::new();
        let lang_def = registry.detect_by_extension("test.rs").unwrap();

        let mut mgr = HighlightManager::new();
        mgr.register_buffer(1, &rope, lang_def);
        let initial_spans = mgr.query_visible(1, &rope, 0, 2);
        assert!(!initial_spans.is_empty());

        // Insert "fn foo() { " at the beginning
        let insert_text = "fn foo() { ";
        let insert_bytes = insert_text.len();
        rope.insert(0, insert_text);

        let edits = vec![EditEvent {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: insert_bytes,
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, insert_bytes),
        }];

        mgr.update(1, &rope, &edits);

        let spans = mgr.query_visible(1, &rope, 0, 2);
        assert!(!spans.is_empty());
        let line0 = spans.get(&0).expect("Expected spans for line 0");
        let has_keyword = line0.iter().any(|s| s.capture_name.starts_with("keyword"));
        assert!(has_keyword, "Expected refreshed keyword capture on line 0");
    }

    #[test]
    fn no_spans_for_unregistered_buffer() {
        let rope = Rope::from_str("fn main() {}\n");
        let mgr = HighlightManager::new();
        let spans = mgr.query_visible(99, &rope, 0, 1);
        assert!(spans.is_empty());
    }
}

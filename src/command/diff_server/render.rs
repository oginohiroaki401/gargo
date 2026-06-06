//! Diff rendering, syntax highlighting, and viewed-state persistence.

use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::diff_render::{
    DiffFile, DiffHighlights, LineKind, content_hash_of, parse_unified_diff, render_file_body_html,
    render_file_body_html_with_highlights,
};
use crate::syntax::highlight::highlight_text;
use crate::syntax::language::{LanguageDef, LanguageRegistry};

/// Bounded LRU cache of fully-rendered single-file diff JSON responses.
///
/// Keyed by a content-addressed string built from resolved git object ids
/// (commit / tree OIDs) plus the file path. A diff between two immutable objects
/// never changes, so cached entries are always valid — the only eviction is the
/// LRU bound, which simply caps memory as the user browses many files. This skips
/// the whole git-diff + tree-sitter-highlight pipeline (~200ms for large files)
/// on a hit. It is deliberately NOT used for working-tree (status) diffs, whose
/// output depends on mutable on-disk content.
pub(crate) struct DiffRenderCache {
    inner: Mutex<DiffRenderCacheInner>,
    capacity: usize,
}

struct DiffRenderCacheInner {
    order: VecDeque<String>,
    entries: HashMap<String, serde_json::Value>,
}

impl DiffRenderCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(DiffRenderCacheInner {
                order: VecDeque::new(),
                entries: HashMap::new(),
            }),
            capacity: 256,
        }
    }

    /// Return the cached response for `key`, marking it most-recently-used.
    pub(crate) fn get(&self, key: &str) -> Option<serde_json::Value> {
        let mut inner = self.inner.lock().ok()?;
        let value = inner.entries.get(key).cloned()?;
        if let Some(pos) = inner.order.iter().position(|k| k == key)
            && let Some(k) = inner.order.remove(pos)
        {
            inner.order.push_back(k);
        }
        Some(value)
    }

    /// Insert (or refresh) `key`, evicting the least-recently-used entries once
    /// the capacity is exceeded.
    pub(crate) fn insert(&self, key: String, value: serde_json::Value) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(pos) = inner.order.iter().position(|k| *k == key) {
            inner.order.remove(pos);
        }
        inner.entries.insert(key.clone(), value);
        inner.order.push_back(key);
        while inner.order.len() > self.capacity {
            if let Some(old) = inner.order.pop_front() {
                inner.entries.remove(&old);
            } else {
                break;
            }
        }
    }
}

impl Default for DiffRenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DiffRenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.inner.lock().map(|i| i.entries.len()).unwrap_or(0);
        f.debug_struct("DiffRenderCache")
            .field("len", &len)
            .field("capacity", &self.capacity)
            .finish()
    }
}

pub(crate) fn file_metadata_json(file: &DiffFile, viewed: bool) -> serde_json::Value {
    serde_json::json!({
        "path": file.path,
        "old_path": file.old_path,
        "status": file.status.as_str(),
        "binary": file.binary,
        "additions": file.additions,
        "deletions": file.deletions,
        "viewed": viewed,
    })
}

/// `(section, path) -> stored content hash` for one page / branch context.
pub(crate) type ViewedMap = HashMap<(String, String), String>;

/// Load every viewed-file record for a page / branch context off the async
/// runtime, since a contended SQLite read can block briefly on `busy_timeout`.
pub(crate) async fn load_viewed_map(
    state: &Arc<DiffServerState>,
    page: &'static str,
    base_ref: String,
    compare_ref: String,
) -> ViewedMap {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        state
            .viewed
            .viewed_map(&state.repo_key(), page, &base_ref, &compare_ref)
    })
    .await
    .unwrap_or_default()
}

/// Persist (or, with `hash == None`, clear) a file's viewed record off the
/// async runtime. Best-effort: failures leave the viewed state unpersisted.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn store_viewed(
    state: &Arc<DiffServerState>,
    page: &'static str,
    base_ref: String,
    compare_ref: String,
    section: String,
    path: String,
    hash: Option<String>,
) {
    let state = state.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let key = state.repo_key();
        match hash {
            Some(h) => state
                .viewed
                .set(&key, page, &base_ref, &compare_ref, &section, &path, &h),
            None => state
                .viewed
                .unset(&key, page, &base_ref, &compare_ref, &section, &path),
        }
    })
    .await;
}

/// Whether `file`'s current content matches its stored viewed record.
pub(crate) fn diff_file_is_viewed(viewed: &ViewedMap, section: &str, file: &DiffFile) -> bool {
    viewed
        .get(&(section.to_string(), file.path.clone()))
        .is_some_and(|stored| *stored == content_hash_of(file))
}

pub(crate) fn empty_diff_html() -> String {
    r#"<div class="gr-diff-body"><div class="gr-line gr-line-hunk"><span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span><span class="gr-text">(no content changes)</span></div></div>"#
        .to_string()
}

/// Build the standard single-file diff JSON response from unified-diff text:
/// parse the first file, syntax-highlight it, and serialize the metadata + HTML.
/// `fallback_status` is used for the empty-diff shape when `diff_text` contains
/// no change for the path. Shared by the compare and commit file endpoints.
pub(crate) fn file_diff_json_from_text(
    diff_text: &str,
    path: &str,
    fallback_status: &str,
) -> serde_json::Value {
    match parse_unified_diff(diff_text).into_iter().next() {
        Some(file) => serde_json::json!({
            "path": file.path,
            "status": file.status.as_str(),
            "additions": file.additions,
            "deletions": file.deletions,
            "binary": file.binary,
            "html": render_highlighted(&file),
        }),
        None => serde_json::json!({
            "path": path,
            "status": fallback_status,
            "additions": 0,
            "deletions": 0,
            "binary": false,
            "html": empty_diff_html(),
        }),
    }
}

/// Render `file` to HTML, applying tree-sitter syntax highlighting when
/// the file's extension maps to a known language. Falls back to plain
/// rendering for unknown languages, binary files, or rename-only entries.
pub(crate) fn render_highlighted(file: &DiffFile) -> String {
    if file.binary || file.hunks.is_empty() {
        return render_file_body_html(file);
    }
    let registry = LanguageRegistry::new();
    let Some(lang) = registry.detect_by_extension(&file.path) else {
        return render_file_body_html(file);
    };
    let highlights = compute_diff_highlights(file, lang);
    render_file_body_html_with_highlights(file, &highlights)
}

/// Reconstruct the new- and old-side line streams for `file`, run
/// `highlight_text` over each, and translate the per-row span maps back
/// into `(hunk_idx, line_idx) → LineHighlights`.
///
/// Single-line fragments don't parse cleanly under tree-sitter (`fn foo(`
/// alone isn't valid Rust), so we feed both sides as a single body each.
/// Context lines receive their spans from the new-side pass; the old-side
/// pass only attaches to actual Remove lines.
pub(crate) fn compute_diff_highlights(file: &DiffFile, lang: &LanguageDef) -> DiffHighlights {
    let mut result: DiffHighlights = HashMap::new();

    // New side: Context + Add.
    let mut new_text = String::new();
    let mut new_map: Vec<(usize, usize)> = Vec::new();
    for (hi, hunk) in file.hunks.iter().enumerate() {
        for (li, line) in hunk.lines.iter().enumerate() {
            if matches!(line.kind, LineKind::Context | LineKind::Add) {
                new_map.push((hi, li));
                new_text.push_str(&line.content);
                new_text.push('\n');
            }
        }
    }
    if !new_text.is_empty() {
        let spans_per_row = highlight_text(&new_text, lang);
        for (row, key) in new_map.iter().enumerate() {
            let Some(spans) = spans_per_row.get(&row) else {
                continue;
            };
            let content_len = file.hunks[key.0].lines[key.1].content.len();
            let entry = result.entry(*key).or_default();
            for s in spans {
                let start = s.start.min(content_len);
                let end = s.end.min(content_len);
                if start < end {
                    entry.spans.push((start, end, s.capture_name.clone()));
                }
            }
        }
    }

    // Old side: Context + Remove, but only attach to Remove lines.
    let mut old_text = String::new();
    let mut old_map: Vec<(usize, usize)> = Vec::new();
    for (hi, hunk) in file.hunks.iter().enumerate() {
        for (li, line) in hunk.lines.iter().enumerate() {
            if matches!(line.kind, LineKind::Context | LineKind::Remove) {
                old_map.push((hi, li));
                old_text.push_str(&line.content);
                old_text.push('\n');
            }
        }
    }
    if !old_text.is_empty() {
        let spans_per_row = highlight_text(&old_text, lang);
        for (row, key) in old_map.iter().enumerate() {
            let (hi, li) = *key;
            if file.hunks[hi].lines[li].kind != LineKind::Remove {
                continue;
            }
            let Some(spans) = spans_per_row.get(&row) else {
                continue;
            };
            let content_len = file.hunks[hi].lines[li].content.len();
            let entry = result.entry(*key).or_default();
            for s in spans {
                let start = s.start.min(content_len);
                let end = s.end.min(content_len);
                if start < end {
                    entry.spans.push((start, end, s.capture_name.clone()));
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_render::parse_unified_diff;

    #[test]
    fn diff_cache_round_trips_and_evicts_lru() {
        let cache = DiffRenderCache {
            inner: Mutex::new(DiffRenderCacheInner {
                order: VecDeque::new(),
                entries: HashMap::new(),
            }),
            capacity: 2,
        };
        assert!(cache.get("a").is_none());
        cache.insert("a".into(), serde_json::json!({"v": 1}));
        cache.insert("b".into(), serde_json::json!({"v": 2}));
        assert_eq!(cache.get("a"), Some(serde_json::json!({"v": 1})));

        // Touching "a" makes "b" the least-recently-used, so inserting "c"
        // (over capacity 2) evicts "b" and keeps "a".
        cache.insert("c".into(), serde_json::json!({"v": 3}));
        assert!(cache.get("b").is_none());
        assert_eq!(cache.get("a"), Some(serde_json::json!({"v": 1})));
        assert_eq!(cache.get("c"), Some(serde_json::json!({"v": 3})));

        // Re-inserting an existing key refreshes its value without growing.
        cache.insert("a".into(), serde_json::json!({"v": 9}));
        assert_eq!(cache.get("a"), Some(serde_json::json!({"v": 9})));
    }

    #[test]
    fn render_highlighted_emits_syntax_classes_for_rust_diff() {
        let diff = "\
diff --git a/lib.rs b/lib.rs
index 1..2 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,3 +1,3 @@
 fn keep() {}
-fn old() { let x = 1; }
+fn renamed() { let y = 2; }
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        // Diff line wrappers still present.
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-remove">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-context">"#));
        // Tree-sitter Rust should classify "fn" and "let" as keywords on
        // both the added and removed lines.
        assert!(
            html.contains("gr-hl-keyword"),
            "expected gr-hl-keyword class, got:\n{}",
            html
        );
    }

    #[test]
    fn render_highlighted_falls_back_for_unknown_extension() {
        let diff = "\
diff --git a/notes.unknownext b/notes.unknownext
index 1..2 100644
--- a/notes.unknownext
+++ b/notes.unknownext
@@ -1,1 +1,1 @@
-old line
+new line
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        assert!(!html.contains("gr-hl-"), "should not highlight: {}", html);
        // Plain diff body still renders normally.
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
    }

    #[test]
    fn render_highlighted_falls_back_for_binary() {
        let diff = "\
diff --git a/img.rs b/img.rs
index abc..def
Binary files a/img.rs and b/img.rs differ
";
        let file = parse_unified_diff(diff).into_iter().next().unwrap();
        let html = render_highlighted(&file);
        assert!(html.contains("(binary file changes not shown)"));
        assert!(!html.contains("gr-hl-"));
    }
}

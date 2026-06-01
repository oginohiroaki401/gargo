//! WASM stub for the `syntax` module.
//!
//! Tree-sitter grammars are native C libraries and cannot target
//! `wasm32-unknown-unknown`, so syntax highlighting and tree-based auto-indent
//! are unavailable in the browser MVP. This stub mirrors only the public
//! surface that `core` consumes (`highlight::HighlightManager`,
//! `language::{LanguageRegistry, LanguageDef}`, `indent::*`) and degrades every
//! operation to a no-op. Auto-indent falls back to copying the previous line's
//! leading whitespace (see [`indent::copy_line_indent`]).

use crate::core::buffer::{BufferId, EditEvent};
use ropey::Rope;

pub mod language {
    /// Minimal stand-in exposing the `name` field `core` reads.
    #[derive(Debug, Clone, Copy)]
    pub struct LanguageDef {
        pub name: &'static str,
    }

    pub struct LanguageRegistry;

    impl LanguageRegistry {
        pub fn new() -> Self {
            LanguageRegistry
        }

        /// No grammars available in the browser → language detection is off.
        pub fn detect_by_extension(&self, _path: &str) -> Option<&LanguageDef> {
            None
        }
    }

    impl Default for LanguageRegistry {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod highlight {
    use super::language::LanguageDef;
    use super::{BufferId, EditEvent, Rope};

    /// Stand-ins for `tree_sitter::Tree`/`Query`. Never constructed under wasm
    /// (the accessors below always return `None`); they exist only so the
    /// `core` call sites that reference these types type-check.
    pub struct Tree;
    pub struct Query;

    pub struct HighlightManager;

    impl HighlightManager {
        pub fn new() -> Self {
            HighlightManager
        }

        pub fn register_buffer(&mut self, _id: BufferId, _rope: &Rope, _lang: &LanguageDef) {}

        pub fn unregister_buffer(&mut self, _id: BufferId) {}

        pub fn update(&mut self, _id: BufferId, _rope: &Rope, _edits: &[EditEvent]) {}

        pub fn tree(&self, _id: BufferId) -> Option<&Tree> {
            None
        }

        pub fn indent_query(&self, _id: BufferId) -> Option<&Query> {
            None
        }
    }

    impl Default for HighlightManager {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod indent {
    use super::highlight::{Query, Tree};
    use ropey::Rope;

    /// Unreachable under wasm (no tree is ever produced), but must type-check.
    pub fn calculate_indent_level(
        _tree: &Tree,
        _query: &Query,
        _source: &[u8],
        _cursor_byte: usize,
    ) -> usize {
        0
    }

    pub fn indent_string(level: usize, tab_width: usize) -> String {
        " ".repeat(level * tab_width)
    }

    /// Fallback used by the browser editor: copy the leading whitespace of the
    /// given line. Matches the native `syntax::indent::copy_line_indent`.
    pub fn copy_line_indent(rope: &Rope, line_idx: usize) -> String {
        if line_idx >= rope.len_lines() {
            return String::new();
        }
        let line = rope.line(line_idx);
        let mut indent = String::new();
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' {
                indent.push(ch);
            } else {
                break;
            }
        }
        indent
    }
}

use std::collections::BTreeSet;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, QueryCursor};

use crate::syntax::highlight::compiled_query;
use crate::syntax::language::{LanguageDef, LanguageRegistry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub char_col: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionSection {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub char_col: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
}

pub fn extract_definition_sections(text: &str, lang_def: &LanguageDef) -> Vec<DefinitionSection> {
    let Some(tags_query_src) = lang_def.tags_query else {
        return Vec::new();
    };

    let language = LanguageRegistry::ts_language(lang_def.language_fn);
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(query) = compiled_query(&language, tags_query_src) else {
        return Vec::new();
    };
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };

    let source = text.as_bytes();
    let root = tree.root_node();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, root, source);
    let capture_names = query.capture_names();
    let line_starts = line_start_offsets(text);

    let mut dedupe = BTreeSet::new();
    while let Some(m) = matches.next() {
        let mut name_node: Option<tree_sitter::Node<'_>> = None;
        let mut kind: Option<String> = None;
        let mut definition_node: Option<tree_sitter::Node<'_>> = None;

        for capture in m.captures.iter() {
            let capture_idx = capture.index as usize;
            let Some(capture_name) = capture_names.get(capture_idx) else {
                continue;
            };
            let capture_name: &str = capture_name;
            if capture_name == "name" {
                name_node = Some(capture.node);
            } else if let Some(rest) = capture_name.strip_prefix("definition.") {
                kind = Some(rest.to_string());
                definition_node = Some(capture.node);
            }
        }

        let (Some(name_node), Some(kind), Some(definition_node)) =
            (name_node, kind, definition_node)
        else {
            continue;
        };
        let Ok(name) = name_node.utf8_text(source) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }

        let start_byte = definition_node.start_byte();
        let end_byte = definition_node.end_byte();
        if start_byte >= end_byte || end_byte > source.len() {
            continue;
        }

        let line = name_node.start_position().row;
        let byte_col = name_node.start_position().column;
        let char_col = byte_col_to_char_col(text, &line_starts, line, byte_col);
        let start_line = definition_node.start_position().row;

        dedupe.insert((
            line,
            char_col,
            name.to_string(),
            kind,
            start_byte,
            end_byte,
            start_line,
        ));
    }

    if lang_def.name == "Markdown" {
        collect_markdown_fenced_code_blocks(root, source, text, &line_starts, &mut dedupe);
    }

    dedupe
        .into_iter()
        .map(
            |(line, char_col, name, kind, start_byte, end_byte, start_line)| DefinitionSection {
                name,
                kind,
                line,
                char_col,
                start_byte,
                end_byte,
                start_line,
            },
        )
        .collect()
}

fn collect_markdown_fenced_code_blocks(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    text: &str,
    line_starts: &[usize],
    dedupe: &mut BTreeSet<(usize, usize, String, String, usize, usize, usize)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "fenced_code_block" {
            let start_byte = node.start_byte();
            let end_byte = node.end_byte();
            if start_byte < end_byte && end_byte <= source.len() {
                let line = node.start_position().row;
                let byte_col = node.start_position().column;
                let char_col = byte_col_to_char_col(text, line_starts, line, byte_col);
                let start_line = node.start_position().row;
                let info = markdown_code_fence_info_string(node, source);
                let name = if info.is_empty() {
                    "code block".to_string()
                } else {
                    format!("code block ({info})")
                };
                dedupe.insert((
                    line,
                    char_col,
                    name,
                    "code_block".to_string(),
                    start_byte,
                    end_byte,
                    start_line,
                ));
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn markdown_code_fence_info_string(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "info_string" {
            continue;
        }
        if let Ok(raw) = child.utf8_text(source) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

pub fn extract_symbols(text: &str, lang_def: &LanguageDef) -> Vec<DocumentSymbol> {
    extract_definition_sections(text, lang_def)
        .into_iter()
        .map(|section| DocumentSymbol {
            name: section.name,
            kind: section.kind,
            line: section.line,
            char_col: section.char_col,
        })
        .collect()
}

fn line_start_offsets(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            text.bytes()
                .enumerate()
                .filter_map(|(i, b)| if b == b'\n' { Some(i + 1) } else { None }),
        )
        .collect()
}

fn byte_col_to_char_col(text: &str, line_starts: &[usize], line: usize, byte_col: usize) -> usize {
    let Some(&line_start) = line_starts.get(line) else {
        return 0;
    };
    let end = line_start.saturating_add(byte_col).min(text.len());
    text[line_start..end].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_definitions_only() {
        let source = r#"
struct User {
    id: u32,
}

fn helper() {}

fn main() {
    helper();
}
"#;
        let registry = LanguageRegistry::new();
        let lang = registry.detect_by_extension("main.rs").unwrap();
        let symbols = extract_symbols(source, lang);

        assert!(
            symbols
                .iter()
                .any(|s| s.kind == "class" && s.name == "User")
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.kind == "function" && s.name == "helper")
        );
        assert!(symbols.iter().all(|s| s.kind != "call"));
    }

    #[test]
    fn extracts_definition_section_ranges() {
        let source = r#"
struct User {
    id: u32,
}

fn helper() {}
"#;
        let registry = LanguageRegistry::new();
        let lang = registry.detect_by_extension("main.rs").unwrap();
        let sections = extract_definition_sections(source, lang);

        let helper = sections
            .iter()
            .find(|s| s.name == "helper" && s.kind == "function")
            .expect("helper function should be extracted");
        assert!(helper.start_byte < helper.end_byte);
        assert_eq!(helper.start_line, 5);
        let snippet = &source[helper.start_byte..helper.end_byte];
        assert!(snippet.contains("fn helper()"));
    }

    #[test]
    fn returns_empty_when_tags_query_is_missing() {
        let registry = LanguageRegistry::new();
        let lang = registry.detect_by_extension("data.json").unwrap();
        let symbols = extract_symbols("{\"a\":1}", lang);
        assert!(symbols.is_empty());
    }

    #[test]
    fn extracts_markdown_headings() {
        let source = r#"# Title

body

## Details
"#;
        let registry = LanguageRegistry::new();
        let lang = registry.detect_by_extension("README.md").unwrap();
        let symbols = extract_symbols(source, lang);

        assert!(symbols.iter().any(|s| s.name == "Title"));
        assert!(symbols.iter().any(|s| s.name == "Details"));
    }

    #[test]
    fn extracts_markdown_fenced_code_blocks_with_and_without_info_string() {
        let source = r#"```rust
fn main() {}
```

```
plain
```
"#;
        let registry = LanguageRegistry::new();
        let lang = registry.detect_by_extension("README.md").unwrap();
        let sections = extract_definition_sections(source, lang);

        let rust_block = sections
            .iter()
            .find(|s| s.kind == "code_block" && s.name == "code block (rust)")
            .expect("rust fenced block should be extracted");
        assert!(rust_block.start_byte < rust_block.end_byte);
        let rust_snippet = &source[rust_block.start_byte..rust_block.end_byte];
        assert!(rust_snippet.starts_with("```rust"));

        let plain_block = sections
            .iter()
            .find(|s| s.kind == "code_block" && s.name == "code block")
            .expect("plain fenced block should be extracted");
        assert!(plain_block.start_byte < plain_block.end_byte);
        let plain_snippet = &source[plain_block.start_byte..plain_block.end_byte];
        assert!(plain_snippet.starts_with("```"));
    }
}

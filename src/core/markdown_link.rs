use std::path::{Path, PathBuf};

use crate::core::document::Document;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEditContext {
    pub path_start_char: usize,
    pub cursor_char: usize,
    pub typed_fragment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteLinkTarget {
    pub target: String,
    pub link_start_char: usize,
    pub link_end_char: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    Url(String),
    LocalPath(PathBuf),
}

pub fn link_edit_context_at_cursor(doc: &Document) -> Option<LinkEditContext> {
    let line_idx = doc.cursor_line();
    let line_start_char = doc.rope.line_to_char(line_idx);
    let line_text = doc
        .rope
        .line(line_idx)
        .to_string()
        .trim_end_matches('\n')
        .to_string();
    let chars: Vec<char> = line_text.chars().collect();
    let cursor_col = doc.cursor_col().min(chars.len());

    let mut path_start_col = None;
    for idx in 0..cursor_col.saturating_sub(1) {
        if chars[idx] == ']' && chars[idx + 1] == '(' {
            path_start_col = Some(idx + 2);
        }
    }

    let path_start_col = path_start_col?;
    if path_start_col > cursor_col {
        return None;
    }

    if !chars[..path_start_col.saturating_sub(1)].contains(&'[') {
        return None;
    }

    if chars[path_start_col..cursor_col].contains(&')') {
        return None;
    }

    let typed_fragment = chars[path_start_col..cursor_col].iter().collect::<String>();

    Some(LinkEditContext {
        path_start_char: line_start_char + path_start_col,
        cursor_char: line_start_char + cursor_col,
        typed_fragment,
    })
}

pub fn complete_link_target_at_cursor(doc: &Document) -> Option<CompleteLinkTarget> {
    if doc.rope.len_chars() == 0 {
        return None;
    }

    let cursor = doc
        .display_cursor()
        .min(doc.rope.len_chars().saturating_sub(1));
    let line_idx = doc.rope.char_to_line(cursor);
    let line_start_char = doc.rope.line_to_char(line_idx);
    let line_text = doc
        .rope
        .line(line_idx)
        .to_string()
        .trim_end_matches('\n')
        .to_string();
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return None;
    }

    let cursor_col = cursor.saturating_sub(line_start_char).min(chars.len());
    if cursor_col >= chars.len() {
        return None;
    }

    for link_start_col in 0..chars.len() {
        if chars[link_start_col] != '[' {
            continue;
        }

        let mut close_text_col = None;
        for idx in (link_start_col + 1)..chars.len().saturating_sub(1) {
            if chars[idx] == ']' && chars[idx + 1] == '(' {
                close_text_col = Some(idx);
                break;
            }
        }
        let Some(close_text_col) = close_text_col else {
            continue;
        };

        let path_start_col = close_text_col + 2;
        let Some(path_end_col) = (path_start_col..chars.len()).find(|idx| chars[*idx] == ')')
        else {
            continue;
        };

        if cursor_col < link_start_col || cursor_col > path_end_col {
            continue;
        }

        let target = chars[path_start_col..path_end_col]
            .iter()
            .collect::<String>();

        return Some(CompleteLinkTarget {
            target,
            link_start_char: line_start_char + link_start_col,
            link_end_char: line_start_char + path_end_col,
        });
    }

    None
}

pub fn bare_url_at_cursor(doc: &Document) -> Option<String> {
    if doc.rope.len_chars() == 0 {
        return None;
    }

    let cursor = doc
        .display_cursor()
        .min(doc.rope.len_chars().saturating_sub(1));
    let line_idx = doc.rope.char_to_line(cursor);
    let line_start_char = doc.rope.line_to_char(line_idx);
    let line_text = doc
        .rope
        .line(line_idx)
        .to_string()
        .trim_end_matches('\n')
        .to_string();
    let chars: Vec<char> = line_text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let cursor_col = cursor.saturating_sub(line_start_char).min(chars.len());

    let mut col = 0;
    while col < chars.len() {
        let scheme_len = if starts_with_ignore_ascii_case(&chars, col, "https://") {
            8
        } else if starts_with_ignore_ascii_case(&chars, col, "http://") {
            7
        } else {
            col += 1;
            continue;
        };

        if col > 0 && chars[col - 1] == '(' {
            col += scheme_len;
            continue;
        }

        let mut end = col + scheme_len;
        while end < chars.len() && is_url_body_char(chars[end]) {
            end += 1;
        }
        while end > col + scheme_len && is_url_trailing_punct(chars[end - 1]) {
            end -= 1;
        }

        if cursor_col >= col && cursor_col <= end.saturating_sub(1).max(col) {
            return Some(chars[col..end].iter().collect::<String>());
        }

        col = end.max(col + 1);
    }
    None
}

fn starts_with_ignore_ascii_case(chars: &[char], pos: usize, prefix: &str) -> bool {
    let prefix_chars: Vec<char> = prefix.chars().collect();
    if pos + prefix_chars.len() > chars.len() {
        return false;
    }
    for (i, pc) in prefix_chars.iter().enumerate() {
        if !chars[pos + i].eq_ignore_ascii_case(pc) {
            return false;
        }
    }
    true
}

fn is_url_body_char(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    !matches!(c, '<' | '>' | '"' | '`' | '|' | '\\' | '^' | '{' | '}')
}

fn is_url_trailing_punct(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\''
    )
}

pub fn resolve_link_target(
    target: &str,
    current_doc_path: &Path,
    project_root: &Path,
) -> Option<ResolvedTarget> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return None;
    }

    if is_web_url(trimmed) {
        return Some(ResolvedTarget::Url(trimmed.to_string()));
    }

    let path_only = strip_fragment_and_query(trimmed);
    if path_only.is_empty() {
        return Some(ResolvedTarget::LocalPath(current_doc_path.to_path_buf()));
    }

    let resolved = if path_only.starts_with('/') {
        project_root.join(path_only.trim_start_matches('/'))
    } else {
        current_doc_path
            .parent()
            .unwrap_or(project_root)
            .join(path_only)
    };

    Some(ResolvedTarget::LocalPath(resolved))
}

fn is_web_url(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn strip_fragment_and_query(target: &str) -> &str {
    let query_idx = target.find('?').unwrap_or(target.len());
    let fragment_idx = target.find('#').unwrap_or(target.len());
    &target[..query_idx.min(fragment_idx)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use std::fs;
    use tempfile::tempdir;

    fn doc_with_cursor(text: &str, cursor_char: usize) -> Document {
        let mut doc = Document::new_scratch(1);
        doc.insert_text(text);
        doc.cursors[0] = cursor_char;
        doc
    }

    fn char_idx(text: &str, needle: &str) -> usize {
        let byte = text.find(needle).expect("needle must exist");
        text[..byte].chars().count()
    }

    #[test]
    fn link_edit_context_detects_basic_path_edit() {
        let text = "[x](./";
        let doc = doc_with_cursor(text, text.chars().count());
        let ctx = link_edit_context_at_cursor(&doc).expect("context");

        assert_eq!(ctx.typed_fragment, "./");
        assert_eq!(ctx.cursor_char, text.chars().count());
    }

    #[test]
    fn link_edit_context_detects_partial_query() {
        let text = "[x](./5";
        let doc = doc_with_cursor(text, text.chars().count());
        let ctx = link_edit_context_at_cursor(&doc).expect("context");

        assert_eq!(ctx.typed_fragment, "./5");
    }

    #[test]
    fn link_edit_context_none_after_closing_paren() {
        let text = "[x](./5)";
        let doc = doc_with_cursor(text, text.chars().count());
        assert!(link_edit_context_at_cursor(&doc).is_none());
    }

    #[test]
    fn complete_link_target_requires_cursor_in_link() {
        let text = "prefix [x](docs/abc.md#sec) suffix";
        let in_link = doc_with_cursor(text, char_idx(text, "docs"));
        let out_of_link = doc_with_cursor(text, char_idx(text, "suffix"));

        let target = complete_link_target_at_cursor(&in_link).expect("target");
        assert_eq!(target.target, "docs/abc.md#sec");
        assert!(complete_link_target_at_cursor(&out_of_link).is_none());
    }

    #[test]
    fn resolve_link_target_local_relative() {
        let tmp = tempdir().expect("temp dir");
        let project_root = tmp.path();
        let doc_path = project_root.join("notes").join("index.md");
        fs::create_dir_all(doc_path.parent().expect("parent")).expect("create");
        fs::write(&doc_path, "x").expect("write");

        let resolved =
            resolve_link_target("child/123.md#h", &doc_path, project_root).expect("resolved");
        assert_eq!(
            resolved,
            ResolvedTarget::LocalPath(project_root.join("notes").join("child").join("123.md"))
        );
    }

    #[test]
    fn resolve_link_target_project_root_relative() {
        let tmp = tempdir().expect("temp dir");
        let project_root = tmp.path();
        let doc_path = project_root.join("docs").join("index.md");

        let resolved =
            resolve_link_target("/README.md?view=1", &doc_path, project_root).expect("resolved");
        assert_eq!(
            resolved,
            ResolvedTarget::LocalPath(project_root.join("README.md"))
        );
    }

    #[test]
    fn resolve_link_target_web_url() {
        let doc_path = PathBuf::from("/tmp/doc.md");
        let project_root = PathBuf::from("/tmp");
        let resolved = resolve_link_target("https://example.com/a?b=1#c", &doc_path, &project_root)
            .expect("resolved");
        assert_eq!(
            resolved,
            ResolvedTarget::Url("https://example.com/a?b=1#c".to_string())
        );
    }

    #[test]
    fn bare_url_at_cursor_detects_https_url() {
        let text = "- https://www.google.com/";
        let doc = doc_with_cursor(text, char_idx(text, "google"));
        assert_eq!(
            bare_url_at_cursor(&doc),
            Some("https://www.google.com/".to_string())
        );
    }

    #[test]
    fn bare_url_at_cursor_detects_http_url_at_start_of_line() {
        let text = "http://example.com/path?q=1";
        let doc = doc_with_cursor(text, 0);
        assert_eq!(
            bare_url_at_cursor(&doc),
            Some("http://example.com/path?q=1".to_string())
        );
    }

    #[test]
    fn bare_url_at_cursor_strips_trailing_punctuation() {
        let text = "see https://example.com/page.";
        let doc = doc_with_cursor(text, char_idx(text, "example"));
        assert_eq!(
            bare_url_at_cursor(&doc),
            Some("https://example.com/page".to_string())
        );
    }

    #[test]
    fn bare_url_at_cursor_none_when_cursor_off_url() {
        let text = "before https://example.com after";
        let doc = doc_with_cursor(text, char_idx(text, "before"));
        assert!(bare_url_at_cursor(&doc).is_none());

        let doc = doc_with_cursor(text, char_idx(text, "after"));
        assert!(bare_url_at_cursor(&doc).is_none());
    }

    #[test]
    fn bare_url_at_cursor_skips_url_inside_markdown_link() {
        let text = "[x](https://example.com/page)";
        let doc = doc_with_cursor(text, char_idx(text, "example"));
        assert!(bare_url_at_cursor(&doc).is_none());
    }

    #[test]
    fn resolve_link_target_anchor_only_returns_current_doc() {
        let doc_path = PathBuf::from("/tmp/docs/index.md");
        let project_root = PathBuf::from("/tmp");
        let resolved = resolve_link_target("#section", &doc_path, &project_root).expect("resolved");
        assert_eq!(resolved, ResolvedTarget::LocalPath(doc_path));
    }
}

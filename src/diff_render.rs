//! Pure-Rust unified-diff parser and HTML renderer.
//!
//! Designed so this module can later be lifted into a standalone crate:
//! - depends only on `std`
//! - never imports anything from the rest of `gargo`
//! - all public surface is plain data + free functions
//!
//! The parser turns the output of `git diff` (with `core.quotepath=off`)
//! into structured records. The renderer turns one [`DiffFile`] into a
//! compact HTML body suitable for direct `innerHTML` injection in the
//! diff server's browser UI.
//!
//! Optional syntax highlighting is supported via
//! [`render_file_body_html_with_highlights`]: callers compute spans
//! externally (e.g. with tree-sitter) and pass them in. This keeps the
//! module free of any highlighting dependency. Side-by-side rendering is
//! still out of scope. Class names are all prefixed with `gr-`
//! (gargo-render) so they will not collide with anything else on the page.

use std::collections::HashMap;
use std::fmt::Write;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

impl FileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileStatus::Added => "added",
            FileStatus::Modified => "modified",
            FileStatus::Deleted => "deleted",
            FileStatus::Renamed => "renamed",
            FileStatus::Untracked => "untracked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: usize,
    pub new_start: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Remove,
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub content: String,
}

/// Highlight spans for a single [`DiffLine`].
///
/// Each tuple is `(start_byte, end_byte_exclusive, capture_name)` where
/// the offsets are into [`DiffLine::content`]. Tree-sitter capture names
/// (e.g. `"keyword"`, `"function.method"`) are emitted as CSS classes
/// `gr-hl-keyword`, `gr-hl-function gr-hl-function-method`. Spans may
/// overlap; the renderer flattens them so the innermost (shortest) span
/// wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineHighlights {
    pub spans: Vec<(usize, usize, String)>,
}

/// Map of `(hunk_index, line_index)` → spans for that line.
pub type DiffHighlights = HashMap<(usize, usize), LineHighlights>;

/// Parse the output of `git diff` (unified diff) into per-file records.
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current: Option<DiffFile> = None;
    let mut current_hunk: Option<DiffHunk> = None;
    let mut old_lineno: usize = 0;
    let mut new_lineno: usize = 0;
    let mut header_path_old: Option<String> = None;
    let mut header_path_new: Option<String> = None;
    let mut rename_from: Option<String> = None;
    let mut rename_to: Option<String> = None;

    for line in input.split_terminator('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file(
                &mut files,
                &mut current,
                &mut current_hunk,
                &mut header_path_old,
                &mut header_path_new,
                &mut rename_from,
                &mut rename_to,
            );
            current = Some(DiffFile {
                path: String::new(),
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
                additions: 0,
                deletions: 0,
            });
            current_hunk = None;
            old_lineno = 0;
            new_lineno = 0;
            if let Some((a, b)) = parse_diff_git_paths(rest) {
                header_path_old = Some(a.to_string());
                header_path_new = Some(b.to_string());
            }
            continue;
        }

        let Some(file) = current.as_mut() else {
            continue;
        };

        if line.starts_with("new file mode ") {
            file.status = FileStatus::Added;
            continue;
        }
        if line.starts_with("deleted file mode ") {
            file.status = FileStatus::Deleted;
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            rename_from = Some(rest.to_string());
            if file.status == FileStatus::Modified {
                file.status = FileStatus::Renamed;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            rename_to = Some(rest.to_string());
            if file.status == FileStatus::Modified {
                file.status = FileStatus::Renamed;
            }
            continue;
        }
        if line.starts_with("Binary files ") && line.ends_with(" differ") {
            file.binary = true;
            continue;
        }
        if line.starts_with("similarity index ")
            || line.starts_with("dissimilarity index ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("index ")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            match strip_diff_path_token(rest) {
                Some(p) => header_path_old = Some(p.to_string()),
                None => {
                    if file.status == FileStatus::Modified {
                        file.status = FileStatus::Added;
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            match strip_diff_path_token(rest) {
                Some(p) => header_path_new = Some(p.to_string()),
                None => {
                    if file.status == FileStatus::Modified {
                        file.status = FileStatus::Deleted;
                    }
                }
            }
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(h) = current_hunk.take() {
                file.hunks.push(h);
            }
            if let Some((os, ns, header)) = parse_hunk_header(line) {
                old_lineno = os;
                new_lineno = ns;
                current_hunk = Some(DiffHunk {
                    header,
                    old_start: os,
                    new_start: ns,
                    lines: Vec::new(),
                });
            }
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Add,
                    old_no: None,
                    new_no: Some(new_lineno),
                    content: rest.to_string(),
                });
                new_lineno += 1;
                file.additions += 1;
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Remove,
                    old_no: Some(old_lineno),
                    new_no: None,
                    content: rest.to_string(),
                });
                old_lineno += 1;
                file.deletions += 1;
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    old_no: Some(old_lineno),
                    new_no: Some(new_lineno),
                    content: rest.to_string(),
                });
                old_lineno += 1;
                new_lineno += 1;
            } else if let Some(rest) = line.strip_prefix('\\') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::NoNewline,
                    old_no: None,
                    new_no: None,
                    content: rest.trim_start().to_string(),
                });
            }
            // Lines that don't start with one of the unified-diff markers
            // (e.g. an artifact empty line) are ignored.
        }
    }

    flush_file(
        &mut files,
        &mut current,
        &mut current_hunk,
        &mut header_path_old,
        &mut header_path_new,
        &mut rename_from,
        &mut rename_to,
    );
    files
}

fn flush_file(
    files: &mut Vec<DiffFile>,
    current: &mut Option<DiffFile>,
    current_hunk: &mut Option<DiffHunk>,
    header_path_old: &mut Option<String>,
    header_path_new: &mut Option<String>,
    rename_from: &mut Option<String>,
    rename_to: &mut Option<String>,
) {
    let Some(mut file) = current.take() else {
        return;
    };
    if let Some(h) = current_hunk.take() {
        file.hunks.push(h);
    }

    if file.status == FileStatus::Renamed {
        if let Some(rt) = rename_to.take() {
            file.path = rt;
        }
        if let Some(rf) = rename_from.take() {
            file.old_path = Some(rf);
        }
    }
    if file.path.is_empty() {
        file.path = if file.status == FileStatus::Deleted {
            header_path_old.clone().unwrap_or_default()
        } else {
            header_path_new
                .clone()
                .or_else(|| header_path_old.clone())
                .unwrap_or_default()
        };
    }
    if file.old_path.is_none()
        && matches!(
            file.status,
            FileStatus::Modified | FileStatus::Renamed | FileStatus::Deleted
        )
        && let Some(op) = header_path_old.clone()
        && Some(&op) != Some(&file.path)
    {
        file.old_path = Some(op);
    }

    *header_path_old = None;
    *header_path_new = None;
    *rename_from = None;
    *rename_to = None;
    files.push(file);
}

fn parse_diff_git_paths(rest: &str) -> Option<(&str, &str)> {
    let a_idx = rest.find("a/")?;
    let after_a = &rest[a_idx + 2..];
    let rel_b = after_a.rfind(" b/")?;
    let a_path = &after_a[..rel_b];
    let b_path = &after_a[rel_b + 3..];
    Some((a_path, b_path))
}

fn strip_diff_path_token(s: &str) -> Option<&str> {
    let s = s.split('\t').next().unwrap_or(s).trim_end();
    if s == "/dev/null" {
        return None;
    }
    let stripped = s
        .strip_prefix("a/")
        .or_else(|| s.strip_prefix("b/"))
        .unwrap_or(s);
    Some(stripped)
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, String)> {
    let body = line.strip_prefix("@@ ")?;
    let (range_part, _rest) = body.split_once(" @@")?;
    let mut iter = range_part.split_whitespace();
    let old = iter.next()?;
    let new = iter.next()?;
    let old_start = parse_range_start(old.strip_prefix('-')?)?;
    let new_start = parse_range_start(new.strip_prefix('+')?)?;
    Some((old_start.max(1), new_start.max(1), line.to_string()))
}

fn parse_range_start(s: &str) -> Option<usize> {
    let n = s.split(',').next()?;
    n.parse::<usize>().ok()
}

/// Render a single [`DiffFile`]'s body as HTML.
///
/// The returned string is a fully-formed `<div class="gr-diff-body">…</div>`
/// element. Callers wrap it in their own file header.
pub fn render_file_body_html(file: &DiffFile) -> String {
    let empty = DiffHighlights::new();
    render_file_body_html_with_highlights(file, &empty)
}

/// Render a single [`DiffFile`] as HTML, applying per-line highlight spans.
///
/// Spans are supplied externally; this module does not depend on any
/// particular highlighter. Lines without an entry in `highlights` (or with
/// an empty span list) render exactly as in [`render_file_body_html`].
pub fn render_file_body_html_with_highlights(
    file: &DiffFile,
    highlights: &DiffHighlights,
) -> String {
    let mut out = String::new();
    out.push_str(r#"<div class="gr-diff-body">"#);
    if file.binary {
        push_marker_line(&mut out, "(binary file changes not shown)");
    } else if file.hunks.is_empty() {
        push_marker_line(&mut out, "(no content changes)");
    } else {
        for (hi, hunk) in file.hunks.iter().enumerate() {
            push_hunk_header(&mut out, &hunk.header);
            for (li, ln) in hunk.lines.iter().enumerate() {
                let hl = highlights.get(&(hi, li));
                push_diff_line(&mut out, ln, hl);
            }
        }
    }
    out.push_str("</div>");
    out
}

fn push_marker_line(out: &mut String, text: &str) {
    out.push_str(r#"<div class="gr-line gr-line-hunk">"#);
    out.push_str(
        r#"<span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span>"#,
    );
    out.push_str(r#"<span class="gr-text">"#);
    out.push_str(&html_escape(text));
    out.push_str("</span></div>");
}

fn push_hunk_header(out: &mut String, header: &str) {
    out.push_str(r#"<div class="gr-line gr-line-hunk">"#);
    out.push_str(
        r#"<span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span>"#,
    );
    out.push_str(r#"<span class="gr-text">"#);
    out.push_str(&html_escape(header));
    out.push_str("</span></div>");
}

fn push_diff_line(out: &mut String, line: &DiffLine, hl: Option<&LineHighlights>) {
    let (cls, sign) = match line.kind {
        LineKind::Context => ("gr-line-context", " "),
        LineKind::Add => ("gr-line-add", "+"),
        LineKind::Remove => ("gr-line-remove", "-"),
        LineKind::NoNewline => ("gr-line-nonewline", "\\"),
    };
    let _ = write!(out, r#"<div class="gr-line {}">"#, cls);
    out.push_str(r#"<span class="gr-ln">"#);
    if let Some(n) = line.old_no {
        let _ = write!(out, "{}", n);
    }
    out.push_str("</span>");
    out.push_str(r#"<span class="gr-lnr">"#);
    if let Some(n) = line.new_no {
        let _ = write!(out, "{}", n);
    }
    out.push_str("</span>");
    let _ = write!(out, r#"<span class="gr-sign">{}</span>"#, html_escape(sign));
    out.push_str(r#"<span class="gr-text">"#);
    match hl {
        Some(h) if !h.spans.is_empty() => push_highlighted_text(out, &line.content, &h.spans),
        _ => out.push_str(&html_escape(&line.content)),
    }
    out.push_str("</span></div>");
}

/// Emit `content` with `<span class="gr-hl-…">` wrappers per the supplied
/// spans. Overlapping spans are resolved with "innermost wins": the
/// shortest span covering a byte is the one that styles it. The unstyled
/// regions are emitted as escaped text.
fn push_highlighted_text(out: &mut String, content: &str, spans: &[(usize, usize, String)]) {
    let len = content.len();
    if len == 0 {
        return;
    }

    // Resolve overlaps: each byte gets the capture name of the shortest
    // (innermost) span that covers it. Longer spans are written first so
    // shorter ones override.
    let mut active: Vec<Option<&str>> = vec![None; len];
    let mut sorted: Vec<&(usize, usize, String)> =
        spans.iter().filter(|(s, _, _)| *s < len).collect();
    sorted.sort_by_key(|(s, e, _)| std::cmp::Reverse(e.saturating_sub(*s)));
    for (s, e, cap) in sorted {
        let s = *s;
        let e = (*e).min(len);
        if s >= e {
            continue;
        }
        for slot in active.iter_mut().take(e).skip(s) {
            *slot = Some(cap.as_str());
        }
    }

    // Walk and coalesce contiguous regions with the same capture.
    // Tree-sitter span boundaries fall on UTF-8 char boundaries, so the
    // resulting (i, j) ranges are safe to slice.
    let mut i = 0;
    while i < len {
        let cur = active[i];
        let mut j = i + 1;
        while j < len && active[j] == cur {
            j += 1;
        }
        if !content.is_char_boundary(i) || !content.is_char_boundary(j) {
            // Defensive: if a span lands inside a multi-byte character
            // (shouldn't happen with tree-sitter), fall through to plain
            // text to avoid a panic.
            out.push_str(&html_escape(&content[i..len]));
            return;
        }
        let seg = &content[i..j];
        match cur {
            Some(cap) => {
                let _ = write!(out, r#"<span class="{}">"#, hl_class_attr(cap));
                out.push_str(&html_escape(seg));
                out.push_str("</span>");
            }
            None => out.push_str(&html_escape(seg)),
        }
        i = j;
    }
}

/// Build the space-separated class list for a tree-sitter capture name.
///
/// `"function.method"` → `"gr-hl-function gr-hl-function-method"` so CSS
/// can target the general or specific level without duplicating colors.
pub(crate) fn hl_class_attr(capture: &str) -> String {
    let parts: Vec<&str> = capture.split('.').collect();
    let mut out = String::with_capacity(capture.len() + 16);
    for i in 0..parts.len() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str("gr-hl-");
        for (j, p) in parts[..=i].iter().enumerate() {
            if j > 0 {
                out.push('-');
            }
            for c in p.chars() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    out.push(c);
                }
            }
        }
    }
    out
}

pub fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Static CSS for the rendered diff bodies. Embedded inline in the diff
/// server HTML templates.
pub fn render_diff_styles() -> &'static str {
    r#"
.gr-diff-body {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 12px;
    line-height: 1.45;
    color: #1f2328;
    background: #ffffff;
    overflow-x: auto;
    border-top: 1px solid #d0d7de;
}
.gr-line {
    display: flex;
    align-items: stretch;
    min-height: 1.45em;
}
.gr-line .gr-ln,
.gr-line .gr-lnr {
    /* Wide enough for 5-digit line numbers plus padding. */
    flex: 0 0 calc(5ch + 16px);
    padding: 0 8px;
    text-align: right;
    color: #57606a;
    background: #f6f8fa;
    user-select: none;
    border-right: 1px solid #eaeef2;
    box-sizing: border-box;
}
.gr-line .gr-sign {
    flex: 0 0 16px;
    text-align: center;
    user-select: none;
    color: #57606a;
}
.gr-line .gr-text {
    flex: 1 1 auto;
    padding: 0 8px;
    white-space: pre;
    min-width: 0;
}
.gr-line-context { background: #ffffff; }
.gr-line-add { background: #e6ffec; }
.gr-line-add .gr-sign { background: #abf2bc; color: #1a7f37; }
.gr-line-add .gr-ln, .gr-line-add .gr-lnr { background: #ccffd8; }
.gr-line-remove { background: #ffebe9; }
.gr-line-remove .gr-sign { background: #ffc1c0; color: #cf222e; }
.gr-line-remove .gr-ln, .gr-line-remove .gr-lnr { background: #ffd7d5; }
.gr-line-hunk { background: #ddf4ff; color: #57606a; }
.gr-line-hunk .gr-ln, .gr-line-hunk .gr-lnr { background: #ddf4ff; border-right-color: #b6e3ff; }
.gr-line-nonewline { color: #57606a; font-style: italic; }

/* Syntax highlight palette: ANSI-light variants, tuned for the white
 * diff background. Captures with hierarchy (e.g. "function.method") get
 * a chained class list so the more general rule applies and the more
 * specific one can add weight/style on top. */
.gr-hl-keyword { color: #800080; font-weight: 600; }
.gr-hl-keyword-operator { font-weight: 400; }
.gr-hl-string { color: #008000; }
.gr-hl-character { color: #008000; }
.gr-hl-comment { color: #6e7781; font-style: italic; }
.gr-hl-function { color: #000080; }
.gr-hl-function-macro { font-weight: 600; }
.gr-hl-type { color: #808000; }
.gr-hl-constructor { color: #808000; }
.gr-hl-constant { color: #008080; }
.gr-hl-number { color: #008080; }
.gr-hl-float { color: #008080; }
.gr-hl-boolean { color: #008080; }
.gr-hl-variable-builtin { color: #800000; }
.gr-hl-variable-parameter { font-style: italic; }
.gr-hl-property { color: #1f2328; }
.gr-hl-attribute { color: #008080; }
.gr-hl-label { color: #008080; }
.gr-hl-escape { color: #008080; }
.gr-hl-embedded { color: #1f2328; }
.gr-hl-tag { color: #800000; }
.gr-hl-heading { color: #000080; font-weight: 600; }
.gr-hl-title { color: #000080; font-weight: 600; }
.gr-hl-link { color: #008080; }
.gr-hl-emphasis { font-style: italic; }
.gr-hl-strong { font-weight: 600; }
.gr-hl-namespace { color: #808000; }
.gr-hl-module { color: #808000; }
.gr-hl-text-title { color: #000080; font-weight: 600; }
.gr-hl-text-literal { color: #008000; }
.gr-hl-text-uri { color: #008080; }
.gr-hl-text-reference { color: #008080; }
.gr-hl-text-emphasis { font-style: italic; }
.gr-hl-text-strong { font-weight: 600; }
.gr-hl-punctuation-special { color: #6e7781; font-weight: 600; }

/* Code preview with line-number gutter (blob view, raw markdown view) */
.code-view { overflow-x: auto; }
.code-table {
    border-collapse: collapse;
    width: 100%;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 12px;
    line-height: 1.5;
    color: #1f2328;
}
.code-table td { padding: 0; vertical-align: top; }
.code-ln {
    width: 1%;
    /* Never narrower than a 5-digit line number plus padding. */
    min-width: calc(5ch + 20px);
    white-space: nowrap;
    text-align: right;
    padding: 0 10px;
    box-sizing: border-box;
    color: #57606a;
    background: #f6f8fa;
    user-select: none;
    border-right: 1px solid #eaeef2;
}
.code-ln::before { content: attr(data-line-number); }
.code-text { padding: 0 10px; white-space: pre; width: 100%; }
.code-table tr:target { background: #fff8c5; }

/* Markdown raw/preview toggle */
.md-view-toggle { display: flex; margin: 0 0 12px; }
.md-toggle-btn {
    padding: 5px 12px;
    font-size: 13px;
    border: 1px solid #d0d7de;
    color: #0969da;
    background: #f6f8fa;
    text-decoration: none;
}
.md-toggle-btn:first-child { border-radius: 6px 0 0 6px; }
.md-toggle-btn:last-child { border-radius: 0 6px 6px 0; border-left: 0; }
.md-toggle-btn.active { background: #0969da; color: #fff; border-color: #0969da; font-weight: 600; }
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_file(input: &str) -> DiffFile {
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1, "expected 1 file, got: {:#?}", files);
        files.into_iter().next().unwrap()
    }

    #[test]
    fn parses_modified_file_with_one_hunk() {
        let input = "\
diff --git a/foo.txt b/foo.txt
index abc..def 100644
--- a/foo.txt
+++ b/foo.txt
@@ -1,3 +1,4 @@
 keep one
-old line
+new line
+added line
 keep two
";
        let f = one_file(input);
        assert_eq!(f.path, "foo.txt");
        assert_eq!(f.status, FileStatus::Modified);
        assert!(!f.binary);
        assert_eq!(f.additions, 2);
        assert_eq!(f.deletions, 1);
        assert_eq!(f.hunks.len(), 1);
        let hunk = &f.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.lines.len(), 5);
        assert_eq!(hunk.lines[0].kind, LineKind::Context);
        assert_eq!(hunk.lines[0].content, "keep one");
        assert_eq!(hunk.lines[0].old_no, Some(1));
        assert_eq!(hunk.lines[0].new_no, Some(1));
        assert_eq!(hunk.lines[1].kind, LineKind::Remove);
        assert_eq!(hunk.lines[1].content, "old line");
        assert_eq!(hunk.lines[1].old_no, Some(2));
        assert_eq!(hunk.lines[1].new_no, None);
        assert_eq!(hunk.lines[2].kind, LineKind::Add);
        assert_eq!(hunk.lines[2].new_no, Some(2));
        assert_eq!(hunk.lines[4].kind, LineKind::Context);
        assert_eq!(hunk.lines[4].old_no, Some(3));
        assert_eq!(hunk.lines[4].new_no, Some(4));
    }

    #[test]
    fn parses_added_file() {
        let input = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..abc
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let f = one_file(input);
        assert_eq!(f.path, "new.txt");
        assert_eq!(f.status, FileStatus::Added);
        assert_eq!(f.additions, 2);
        assert_eq!(f.deletions, 0);
        assert_eq!(f.hunks[0].lines[0].new_no, Some(1));
        assert_eq!(f.hunks[0].lines[1].new_no, Some(2));
    }

    #[test]
    fn parses_deleted_file() {
        let input = "\
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index abc..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-line one
-line two
";
        let f = one_file(input);
        assert_eq!(f.path, "gone.txt");
        assert_eq!(f.status, FileStatus::Deleted);
        assert_eq!(f.deletions, 2);
        assert_eq!(f.additions, 0);
    }

    #[test]
    fn parses_rename_only() {
        let input = "\
diff --git a/old/path b/new/path
similarity index 100%
rename from old/path
rename to new/path
";
        let f = one_file(input);
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.path, "new/path");
        assert_eq!(f.old_path.as_deref(), Some("old/path"));
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn parses_rename_with_changes() {
        let input = "\
diff --git a/old.rs b/new.rs
similarity index 80%
rename from old.rs
rename to new.rs
index abc..def 100644
--- a/old.rs
+++ b/new.rs
@@ -1,2 +1,2 @@
-fn old() {}
+fn renamed() {}
 // comment
";
        let f = one_file(input);
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.path, "new.rs");
        assert_eq!(f.old_path.as_deref(), Some("old.rs"));
        assert_eq!(f.additions, 1);
        assert_eq!(f.deletions, 1);
    }

    #[test]
    fn marks_binary_file() {
        let input = "\
diff --git a/img.png b/img.png
index abc..def
Binary files a/img.png and b/img.png differ
";
        let f = one_file(input);
        assert!(f.binary);
        assert_eq!(f.path, "img.png");
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn handles_no_newline_marker() {
        let input = "\
diff --git a/x b/x
index abc..def 100644
--- a/x
+++ b/x
@@ -1,1 +1,1 @@
-old
\\ No newline at end of file
+new
";
        let f = one_file(input);
        assert_eq!(f.hunks[0].lines.len(), 3);
        assert_eq!(f.hunks[0].lines[1].kind, LineKind::NoNewline);
    }

    #[test]
    fn handles_multiple_hunks_and_multiple_files() {
        let input = "\
diff --git a/a.txt b/a.txt
index 1..2 100644
--- a/a.txt
+++ b/a.txt
@@ -1,1 +1,1 @@
-aa
+AA
@@ -10,1 +10,1 @@
-bb
+BB
diff --git a/b.txt b/b.txt
index 3..4 100644
--- a/b.txt
+++ b/b.txt
@@ -1,1 +1,2 @@
 keep
+new
";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[1].old_start, 10);
        assert_eq!(files[1].path, "b.txt");
        assert_eq!(files[1].additions, 1);
    }

    #[test]
    fn handles_non_ascii_filename() {
        let input = "\
diff --git a/日本語/ファイル.md b/日本語/ファイル.md
index abc..def 100644
--- a/日本語/ファイル.md
+++ b/日本語/ファイル.md
@@ -1,1 +1,1 @@
-旧
+新
";
        let f = one_file(input);
        assert_eq!(f.path, "日本語/ファイル.md");
        assert_eq!(f.hunks[0].lines[0].content, "旧");
        assert_eq!(f.hunks[0].lines[1].content, "新");
    }

    #[test]
    fn renders_html_escapes_special_chars() {
        let input = "\
diff --git a/x b/x
index 1..2 100644
--- a/x
+++ b/x
@@ -1,1 +1,1 @@
-<script>old</script>
+<script>new & \"safer\"</script>
";
        let f = one_file(input);
        let html = render_file_body_html(&f);
        assert!(!html.contains("<script>"), "raw <script> leaked: {}", html);
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&quot;"));
    }

    #[test]
    fn renders_file_body_structure() {
        let input = "\
diff --git a/x.txt b/x.txt
index 1..2 100644
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,3 @@
 keep
-old
+new1
+new2
";
        let f = one_file(input);
        let html = render_file_body_html(&f);
        assert!(html.starts_with(r#"<div class="gr-diff-body">"#));
        assert!(html.ends_with("</div>"));
        assert!(html.contains(r#"<div class="gr-line gr-line-hunk">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-context">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-remove">"#));
        // Line numbers
        assert!(html.contains(">1<"));
        assert!(html.contains(">2<"));
        assert!(html.contains(">3<"));
    }

    #[test]
    fn renders_binary_marker() {
        let input = "\
diff --git a/img.png b/img.png
index abc..def
Binary files a/img.png and b/img.png differ
";
        let f = one_file(input);
        let html = render_file_body_html(&f);
        assert!(html.contains("(binary file changes not shown)"));
    }

    #[test]
    fn renders_empty_for_rename_only() {
        let input = "\
diff --git a/old b/new
similarity index 100%
rename from old
rename to new
";
        let f = one_file(input);
        let html = render_file_body_html(&f);
        assert!(html.contains("(no content changes)"));
    }

    #[test]
    fn diff_styles_contains_class_rules() {
        let css = render_diff_styles();
        assert!(css.contains(".gr-line-add"));
        assert!(css.contains(".gr-line-remove"));
        assert!(css.contains(".gr-line-hunk"));
        assert!(css.contains(".gr-diff-body"));
        assert!(css.contains(".gr-hl-keyword"));
        assert!(css.contains(".gr-hl-string"));
        assert!(css.contains(".gr-hl-comment"));
    }

    #[test]
    fn hl_class_attr_chains_hierarchy() {
        assert_eq!(hl_class_attr("keyword"), "gr-hl-keyword");
        assert_eq!(
            hl_class_attr("function.method"),
            "gr-hl-function gr-hl-function-method"
        );
        assert_eq!(
            hl_class_attr("punctuation.bracket"),
            "gr-hl-punctuation gr-hl-punctuation-bracket"
        );
        // Non-alphanumeric (other than _) are stripped so attribute is safe.
        assert_eq!(hl_class_attr("weird/name"), "gr-hl-weirdname");
    }

    #[test]
    fn highlighted_text_emits_spans_and_escapes() {
        let mut out = String::new();
        let spans = vec![
            (0, 2, "keyword".to_string()),  // "fn"
            (3, 4, "function".to_string()), // "f"
        ];
        push_highlighted_text(&mut out, "fn f()", &spans);
        assert!(
            out.contains(r#"<span class="gr-hl-keyword">fn</span>"#),
            "{}",
            out
        );
        assert!(
            out.contains(r#"<span class="gr-hl-function">f</span>"#),
            "{}",
            out
        );
        // The unstyled "()" tail and the space between are preserved.
        assert!(out.ends_with("()"), "{}", out);
    }

    #[test]
    fn highlighted_text_innermost_span_wins() {
        // Outer span covers 0..10 as "function", inner 4..7 as "keyword".
        // The inner should style "abc", outer surrounds the rest.
        let mut out = String::new();
        let spans = vec![
            (0, 10, "function".to_string()),
            (4, 7, "keyword".to_string()),
        ];
        push_highlighted_text(&mut out, "0123abc789", &spans);
        // Three regions: function "0123", keyword "abc", function "789".
        assert!(
            out.contains(r#"<span class="gr-hl-function">0123</span>"#),
            "{}",
            out
        );
        assert!(
            out.contains(r#"<span class="gr-hl-keyword">abc</span>"#),
            "{}",
            out
        );
        assert!(
            out.contains(r#"<span class="gr-hl-function">789</span>"#),
            "{}",
            out
        );
    }

    #[test]
    fn highlighted_text_escapes_special_chars_inside_spans() {
        let mut out = String::new();
        let content = "<x> & 'q'";
        // One span covering the whole line.
        let spans = vec![(0, content.len(), "string".to_string())];
        push_highlighted_text(&mut out, content, &spans);
        assert!(!out.contains("<x>"));
        assert!(out.contains("&lt;x&gt;"));
        assert!(out.contains("&amp;"));
        assert!(out.contains("&#39;q&#39;"));
    }

    #[test]
    fn render_with_highlights_attaches_classes_to_diff_lines() {
        let input = "\
diff --git a/lib.rs b/lib.rs
index 1..2 100644
--- a/lib.rs
+++ b/lib.rs
@@ -1,1 +1,1 @@
-fn old() {}
+fn new() {}
";
        let f = one_file(input);
        let mut highlights: DiffHighlights = HashMap::new();
        // First hunk, first line is the removed "fn old() {}" — mark "fn".
        highlights.insert(
            (0, 0),
            LineHighlights {
                spans: vec![(0, 2, "keyword".to_string())],
            },
        );
        // Second line is the added "fn new() {}" — mark "fn" and "new".
        highlights.insert(
            (0, 1),
            LineHighlights {
                spans: vec![
                    (0, 2, "keyword".to_string()),
                    (3, 6, "function".to_string()),
                ],
            },
        );
        let html = render_file_body_html_with_highlights(&f, &highlights);
        assert!(
            html.contains(r#"<span class="gr-hl-keyword">fn</span>"#),
            "{}",
            html
        );
        assert!(
            html.contains(r#"<span class="gr-hl-function">new</span>"#),
            "{}",
            html
        );
        // The diff line wrappers are still there.
        assert!(html.contains(r#"<div class="gr-line gr-line-add">"#));
        assert!(html.contains(r#"<div class="gr-line gr-line-remove">"#));
    }

    #[test]
    fn render_without_highlights_is_unchanged() {
        // With no entry for a line, the renderer must produce the same
        // body as the legacy `render_file_body_html`.
        let input = "\
diff --git a/x.txt b/x.txt
index 1..2 100644
--- a/x.txt
+++ b/x.txt
@@ -1,2 +1,3 @@
 keep
-old
+new1
+new2
";
        let f = one_file(input);
        let legacy = render_file_body_html(&f);
        let with_empty = render_file_body_html_with_highlights(&f, &DiffHighlights::new());
        assert_eq!(legacy, with_empty);
    }
}

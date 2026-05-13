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
//! Syntax highlighting and side-by-side rendering are intentionally out of
//! scope here. Class names are all prefixed with `gr-` (gargo-render) so
//! they will not collide with anything else on the page.

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
    {
        if let Some(op) = header_path_old.clone() {
            if Some(&op) != Some(&file.path) {
                file.old_path = Some(op);
            }
        }
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
    let mut out = String::new();
    out.push_str(r#"<div class="gr-diff-body">"#);
    if file.binary {
        push_marker_line(&mut out, "(binary file changes not shown)");
    } else if file.hunks.is_empty() {
        push_marker_line(&mut out, "(no content changes)");
    } else {
        for hunk in &file.hunks {
            push_hunk_header(&mut out, &hunk.header);
            for ln in &hunk.lines {
                push_diff_line(&mut out, ln);
            }
        }
    }
    out.push_str("</div>");
    out
}

fn push_marker_line(out: &mut String, text: &str) {
    out.push_str(r#"<div class="gr-line gr-line-hunk">"#);
    out.push_str(r#"<span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span>"#);
    out.push_str(r#"<span class="gr-text">"#);
    out.push_str(&html_escape(text));
    out.push_str("</span></div>");
}

fn push_hunk_header(out: &mut String, header: &str) {
    out.push_str(r#"<div class="gr-line gr-line-hunk">"#);
    out.push_str(r#"<span class="gr-ln"></span><span class="gr-lnr"></span><span class="gr-sign"></span>"#);
    out.push_str(r#"<span class="gr-text">"#);
    out.push_str(&html_escape(header));
    out.push_str("</span></div>");
}

fn push_diff_line(out: &mut String, line: &DiffLine) {
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
    out.push_str(&html_escape(&line.content));
    out.push_str("</span></div>");
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
    flex: 0 0 48px;
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
    }
}

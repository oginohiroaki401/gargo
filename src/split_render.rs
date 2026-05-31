//! Side-by-side ("split view") diff renderer.
//!
//! Given the old and new full file contents plus the parsed [`DiffFile`],
//! produces a flat list of [`SplitRow`]s — each row carries an optional
//! left (old) and right (new) cell that the HTML layer renders into a
//! 4-column grid (line# | text | line# | text).
//!
//! Unchanged regions between hunks are paired line-by-line so the entire
//! file is visible, not just the diff. Within a hunk, consecutive removes
//! and adds are paired to produce side-by-side "change" rows; surplus on
//! either side becomes a single-sided row.

use std::collections::HashMap;
use std::fmt::Write;

use crate::diff_render::{DiffFile, LineHighlights, LineKind, html_escape, push_highlighted_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitKind {
    Context,
    Add,
    Remove,
    Change,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitCell {
    pub line_no: usize,
    pub content: String,
    pub kind: SplitKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitRow {
    pub left: Option<SplitCell>,
    pub right: Option<SplitCell>,
}

/// Highlights keyed by 1-based line number within the corresponding file.
pub type LineHl = HashMap<usize, LineHighlights>;

/// Build the aligned row list for a split view.
///
/// - `old_lines = None` → file did not exist on the old side (added/untracked).
///   Every new line is emitted as an Add row on the right; left is empty.
/// - `new_lines = None` → file removed on the new side. Every old line emits
///   as a Remove row on the left; right is empty.
/// - Otherwise: walk the file driven by `file.hunks`, pairing context lines
///   line-by-line outside hunks, and pairing consecutive Remove/Add lines
///   inside hunks (surplus → single-sided row).
pub fn build_split_rows(
    old_lines: Option<&[String]>,
    new_lines: Option<&[String]>,
    file: &DiffFile,
) -> Vec<SplitRow> {
    if old_lines.is_none() {
        let new_lines = new_lines.unwrap_or(&[]);
        return new_lines
            .iter()
            .enumerate()
            .map(|(i, line)| SplitRow {
                left: None,
                right: Some(SplitCell {
                    line_no: i + 1,
                    content: line.clone(),
                    kind: SplitKind::Add,
                }),
            })
            .collect();
    }
    if new_lines.is_none() {
        let old_lines = old_lines.unwrap_or(&[]);
        return old_lines
            .iter()
            .enumerate()
            .map(|(i, line)| SplitRow {
                left: Some(SplitCell {
                    line_no: i + 1,
                    content: line.clone(),
                    kind: SplitKind::Remove,
                }),
                right: None,
            })
            .collect();
    }

    let old_lines = old_lines.unwrap();
    let new_lines = new_lines.unwrap();
    let mut rows: Vec<SplitRow> = Vec::new();
    let mut old_cursor: usize = 1; // 1-based
    let mut new_cursor: usize = 1;

    for hunk in &file.hunks {
        // Emit unchanged context up to this hunk. The pre-hunk gap on both
        // sides is unchanged code and should be equal length, but clamp
        // defensively in case the hunk header is unusual.
        let pre_old = hunk.old_start.saturating_sub(old_cursor);
        let pre_new = hunk.new_start.saturating_sub(new_cursor);
        let pre = pre_old.min(pre_new);
        for i in 0..pre {
            let old_no = old_cursor + i;
            let new_no = new_cursor + i;
            let left_content = old_lines
                .get(old_no.saturating_sub(1))
                .cloned()
                .unwrap_or_default();
            let right_content = new_lines
                .get(new_no.saturating_sub(1))
                .cloned()
                .unwrap_or_default();
            rows.push(SplitRow {
                left: Some(SplitCell {
                    line_no: old_no,
                    content: left_content,
                    kind: SplitKind::Context,
                }),
                right: Some(SplitCell {
                    line_no: new_no,
                    content: right_content,
                    kind: SplitKind::Context,
                }),
            });
        }
        old_cursor += pre;
        new_cursor += pre;

        // Walk the hunk grouping consecutive Remove/Add into change blocks,
        // emitting Context lines on both sides.
        let mut i = 0;
        while i < hunk.lines.len() {
            let line = &hunk.lines[i];
            match line.kind {
                LineKind::Context => {
                    let old_no = line.old_no.unwrap_or(old_cursor);
                    let new_no = line.new_no.unwrap_or(new_cursor);
                    rows.push(SplitRow {
                        left: Some(SplitCell {
                            line_no: old_no,
                            content: line.content.clone(),
                            kind: SplitKind::Context,
                        }),
                        right: Some(SplitCell {
                            line_no: new_no,
                            content: line.content.clone(),
                            kind: SplitKind::Context,
                        }),
                    });
                    old_cursor = old_no + 1;
                    new_cursor = new_no + 1;
                    i += 1;
                }
                LineKind::Remove | LineKind::Add => {
                    // Gather a contiguous block of Remove* then Add* (any order;
                    // typical diffs emit removes-then-adds but we don't assume).
                    let mut removes: Vec<&crate::diff_render::DiffLine> = Vec::new();
                    let mut adds: Vec<&crate::diff_render::DiffLine> = Vec::new();
                    while i < hunk.lines.len() {
                        let l = &hunk.lines[i];
                        match l.kind {
                            LineKind::Remove => removes.push(l),
                            LineKind::Add => adds.push(l),
                            _ => break,
                        }
                        i += 1;
                    }
                    let pair_n = removes.len().min(adds.len());
                    for k in 0..pair_n {
                        let r = removes[k];
                        let a = adds[k];
                        rows.push(SplitRow {
                            left: Some(SplitCell {
                                line_no: r.old_no.unwrap_or(old_cursor),
                                content: r.content.clone(),
                                kind: SplitKind::Change,
                            }),
                            right: Some(SplitCell {
                                line_no: a.new_no.unwrap_or(new_cursor),
                                content: a.content.clone(),
                                kind: SplitKind::Change,
                            }),
                        });
                    }
                    // Surplus removes → left-only rows.
                    for r in &removes[pair_n..] {
                        rows.push(SplitRow {
                            left: Some(SplitCell {
                                line_no: r.old_no.unwrap_or(old_cursor),
                                content: r.content.clone(),
                                kind: SplitKind::Remove,
                            }),
                            right: None,
                        });
                    }
                    // Surplus adds → right-only rows.
                    for a in &adds[pair_n..] {
                        rows.push(SplitRow {
                            left: None,
                            right: Some(SplitCell {
                                line_no: a.new_no.unwrap_or(new_cursor),
                                content: a.content.clone(),
                                kind: SplitKind::Add,
                            }),
                        });
                    }
                    if let Some(last) = removes.last() {
                        old_cursor = last.old_no.unwrap_or(old_cursor) + 1;
                    }
                    if let Some(last) = adds.last() {
                        new_cursor = last.new_no.unwrap_or(new_cursor) + 1;
                    }
                }
                LineKind::NoNewline => {
                    i += 1;
                }
            }
        }
    }

    // Trailing unchanged tail.
    let tail_old = old_lines.len().saturating_sub(old_cursor - 1);
    let tail_new = new_lines.len().saturating_sub(new_cursor - 1);
    let tail = tail_old.min(tail_new);
    for i in 0..tail {
        let old_no = old_cursor + i;
        let new_no = new_cursor + i;
        let left_content = old_lines
            .get(old_no.saturating_sub(1))
            .cloned()
            .unwrap_or_default();
        let right_content = new_lines
            .get(new_no.saturating_sub(1))
            .cloned()
            .unwrap_or_default();
        rows.push(SplitRow {
            left: Some(SplitCell {
                line_no: old_no,
                content: left_content,
                kind: SplitKind::Context,
            }),
            right: Some(SplitCell {
                line_no: new_no,
                content: right_content,
                kind: SplitKind::Context,
            }),
        });
    }

    rows
}

/// Render the row list to an HTML body. The first row that contains any
/// non-Context cell receives `id="first-diff"` for scroll-to-diff on load.
pub fn render_split_html(
    rows: &[SplitRow],
    old_hl: Option<&LineHl>,
    new_hl: Option<&LineHl>,
) -> String {
    let mut out = String::new();
    let mut first_diff_id_used = false;
    for row in rows {
        let kind = row_kind(row);
        let class = match kind {
            SplitKind::Context => "sp-context",
            SplitKind::Add => "sp-add",
            SplitKind::Remove => "sp-remove",
            SplitKind::Change => "sp-change",
        };
        let id_attr = if !first_diff_id_used && kind != SplitKind::Context {
            first_diff_id_used = true;
            r#" id="first-diff""#
        } else {
            ""
        };
        let _ = write!(&mut out, r#"<div class="sp-row {class}"{id_attr}>"#);
        emit_cell(&mut out, row.left.as_ref(), "l", old_hl);
        emit_cell(&mut out, row.right.as_ref(), "r", new_hl);
        out.push_str("</div>");
    }
    out
}

fn row_kind(row: &SplitRow) -> SplitKind {
    match (row.left.as_ref(), row.right.as_ref()) {
        (Some(l), Some(r)) => {
            if l.kind == SplitKind::Change || r.kind == SplitKind::Change {
                SplitKind::Change
            } else {
                SplitKind::Context
            }
        }
        (Some(_), None) => SplitKind::Remove,
        (None, Some(_)) => SplitKind::Add,
        (None, None) => SplitKind::Context,
    }
}

fn emit_cell(out: &mut String, cell: Option<&SplitCell>, side: &str, hl: Option<&LineHl>) {
    match cell {
        Some(cell) => {
            let _ = write!(
                out,
                r#"<span class="sp-ln sp-ln-{side}">{}</span>"#,
                cell.line_no
            );
            let _ = write!(out, r#"<span class="sp-text sp-text-{side}">"#);
            let line_hl = hl.and_then(|m| m.get(&cell.line_no));
            match line_hl {
                Some(h) if !h.spans.is_empty() => {
                    push_highlighted_text(out, &cell.content, &h.spans);
                }
                _ => out.push_str(&html_escape(&cell.content)),
            }
            out.push_str("</span>");
        }
        None => {
            let _ = write!(
                out,
                r#"<span class="sp-ln sp-ln-{side} sp-empty"></span><span class="sp-text sp-text-{side} sp-empty"></span>"#
            );
        }
    }
}

/// CSS for the split layout. Pure layout/color; tree-sitter highlight CSS
/// comes from the shared `render_diff_styles` block.
pub fn render_split_styles() -> &'static str {
    r#"<style>
.split-grid {
    display: grid;
    grid-template-columns: minmax(3.5ch, max-content) minmax(0, 1fr) minmax(3.5ch, max-content) minmax(0, 1fr);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 12px;
    line-height: 1.55;
    background: #fff;
    border-top: 1px solid #d0d7de;
}
.sp-row { display: contents; }
.sp-ln {
    padding: 0 10px;
    text-align: right;
    color: #57606a;
    background: #f6f8fa;
    border-right: 1px solid #eaeef2;
    user-select: none;
    white-space: nowrap;
}
.sp-text {
    padding: 0 10px;
    white-space: pre;
    min-width: 0;
    overflow: hidden;
    text-overflow: clip;
}
.sp-context .sp-text { background: #fff; }
.sp-add .sp-text-r { background: #e6ffec; }
.sp-add .sp-text-l, .sp-add .sp-ln-l { background: #f3f4f6; }
.sp-remove .sp-text-l { background: #ffebe9; }
.sp-remove .sp-text-r, .sp-remove .sp-ln-r { background: #f3f4f6; }
.sp-change .sp-text-l { background: #ffebe9; }
.sp-change .sp-text-r { background: #e6ffec; }
.sp-empty { background: #f3f4f6; }
#first-diff { scroll-margin-top: 56px; }
.split-header {
    position: sticky;
    top: 0;
    z-index: 10;
    background: #f6f8fa;
    border-bottom: 1px solid #d0d7de;
    padding: 8px 12px;
    display: flex;
    gap: 12px;
    align-items: center;
    font-size: 13px;
}
.split-back {
    color: #0969da;
    text-decoration: none;
    flex-shrink: 0;
}
.split-back:hover { text-decoration: underline; }
.split-path {
    flex: 1 1 auto;
    min-width: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
}
.split-refs {
    flex-shrink: 0;
    color: #57606a;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
}
.split-notice {
    padding: 16px 20px;
    color: #57606a;
    font-size: 13px;
    background: #f6f8fa;
    border-bottom: 1px solid #d0d7de;
}
</style>"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_render::{DiffFile, DiffHunk, DiffLine, FileStatus, LineKind};

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn modified_file() -> DiffFile {
        // Old: ["a","b","c","d","e"]; New: ["a","B","c","D","e"]
        // Two hunks each containing one change.
        DiffFile {
            path: s("f.txt"),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            hunks: vec![
                DiffHunk {
                    header: s("@@ -1,3 +1,3 @@"),
                    old_start: 1,
                    new_start: 1,
                    lines: vec![
                        DiffLine {
                            kind: LineKind::Context,
                            old_no: Some(1),
                            new_no: Some(1),
                            content: s("a"),
                        },
                        DiffLine {
                            kind: LineKind::Remove,
                            old_no: Some(2),
                            new_no: None,
                            content: s("b"),
                        },
                        DiffLine {
                            kind: LineKind::Add,
                            old_no: None,
                            new_no: Some(2),
                            content: s("B"),
                        },
                        DiffLine {
                            kind: LineKind::Context,
                            old_no: Some(3),
                            new_no: Some(3),
                            content: s("c"),
                        },
                    ],
                },
                DiffHunk {
                    header: s("@@ -4,2 +4,2 @@"),
                    old_start: 4,
                    new_start: 4,
                    lines: vec![
                        DiffLine {
                            kind: LineKind::Remove,
                            old_no: Some(4),
                            new_no: None,
                            content: s("d"),
                        },
                        DiffLine {
                            kind: LineKind::Add,
                            old_no: None,
                            new_no: Some(4),
                            content: s("D"),
                        },
                        DiffLine {
                            kind: LineKind::Context,
                            old_no: Some(5),
                            new_no: Some(5),
                            content: s("e"),
                        },
                    ],
                },
            ],
            additions: 2,
            deletions: 2,
        }
    }

    #[test]
    fn modified_pairs_changes_and_keeps_context() {
        let old: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let new: Vec<String> = ["a", "B", "c", "D", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let file = modified_file();
        let rows = build_split_rows(Some(&old), Some(&new), &file);
        // 5 rows: ctx, change, ctx, change, ctx
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].left.as_ref().unwrap().line_no, 1);
        assert_eq!(rows[0].left.as_ref().unwrap().kind, SplitKind::Context);
        assert_eq!(rows[1].left.as_ref().unwrap().kind, SplitKind::Change);
        assert_eq!(rows[1].right.as_ref().unwrap().kind, SplitKind::Change);
        assert_eq!(rows[1].left.as_ref().unwrap().content, "b");
        assert_eq!(rows[1].right.as_ref().unwrap().content, "B");
        assert_eq!(rows[3].left.as_ref().unwrap().content, "d");
        assert_eq!(rows[3].right.as_ref().unwrap().content, "D");
        assert_eq!(rows[4].right.as_ref().unwrap().line_no, 5);
    }

    #[test]
    fn unequal_add_remove_makes_surplus_single_sided() {
        // Old: ["x"], New: ["a","b","c"] — 1 remove + 3 adds.
        let file = DiffFile {
            path: s("f"),
            old_path: None,
            status: FileStatus::Modified,
            binary: false,
            hunks: vec![DiffHunk {
                header: s("@@ -1 +1,3 @@"),
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: LineKind::Remove,
                        old_no: Some(1),
                        new_no: None,
                        content: s("x"),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(1),
                        content: s("a"),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(2),
                        content: s("b"),
                    },
                    DiffLine {
                        kind: LineKind::Add,
                        old_no: None,
                        new_no: Some(3),
                        content: s("c"),
                    },
                ],
            }],
            additions: 3,
            deletions: 1,
        };
        let old = vec![s("x")];
        let new = vec![s("a"), s("b"), s("c")];
        let rows = build_split_rows(Some(&old), Some(&new), &file);
        // 1 change (x↔a) + 2 right-only add (b, c)
        assert_eq!(rows.len(), 3);
        assert!(rows[0].left.is_some() && rows[0].right.is_some());
        assert_eq!(rows[1].left, None);
        assert_eq!(rows[1].right.as_ref().unwrap().content, "b");
        assert_eq!(rows[2].right.as_ref().unwrap().content, "c");
    }

    #[test]
    fn added_file_is_right_only() {
        let file = DiffFile {
            path: s("new.txt"),
            old_path: None,
            status: FileStatus::Added,
            binary: false,
            hunks: vec![],
            additions: 2,
            deletions: 0,
        };
        let new = vec![s("hello"), s("world")];
        let rows = build_split_rows(None, Some(&new), &file);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].left.is_none());
        assert_eq!(rows[0].right.as_ref().unwrap().kind, SplitKind::Add);
        assert_eq!(rows[1].right.as_ref().unwrap().line_no, 2);
    }

    #[test]
    fn deleted_file_is_left_only() {
        let file = DiffFile {
            path: s("gone.txt"),
            old_path: None,
            status: FileStatus::Deleted,
            binary: false,
            hunks: vec![],
            additions: 0,
            deletions: 1,
        };
        let old = vec![s("gone")];
        let rows = build_split_rows(Some(&old), None, &file);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].left.as_ref().unwrap().kind, SplitKind::Remove);
        assert!(rows[0].right.is_none());
    }

    #[test]
    fn no_hunks_pairs_full_files_line_by_line() {
        // Rename-only with identical content.
        let file = DiffFile {
            path: s("new"),
            old_path: Some(s("old")),
            status: FileStatus::Renamed,
            binary: false,
            hunks: vec![],
            additions: 0,
            deletions: 0,
        };
        let old = vec![s("one"), s("two")];
        let new = vec![s("one"), s("two")];
        let rows = build_split_rows(Some(&old), Some(&new), &file);
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.left.as_ref().unwrap().kind, SplitKind::Context);
            assert_eq!(r.right.as_ref().unwrap().kind, SplitKind::Context);
        }
    }

    #[test]
    fn html_renders_first_diff_marker() {
        let old: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let new: Vec<String> = ["a", "B", "c", "D", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let file = modified_file();
        let rows = build_split_rows(Some(&old), Some(&new), &file);
        let html = render_split_html(&rows, None, None);
        assert!(html.contains(r#"id="first-diff""#));
        // Only one marker overall.
        assert_eq!(html.matches(r#"id="first-diff""#).count(), 1);
        assert!(html.contains("sp-change"));
        assert!(html.contains("sp-context"));
    }
}

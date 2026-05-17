use super::*;

const MARKDOWN_JA: &str = "\
# 竹取物語

## あらすじ

今は昔、竹取の翁といふものありけり。
野山にまじりて竹を取りつつ、よろづのことに使ひけり。

## 登場人物

- **かぐや姫** — 竹の中から見つかった少女
- **翁（おきな）** — 竹取の翁
- **媼（おうな）** — 翁の妻

## コードブロック

```rust
fn main() {
    println!(\"かぐや\");
}
```

> 天人の中に持たせたる箱あり。天の羽衣入れり。

以上。
";

fn doc_ja() -> Document {
    let mut doc = Document::new_scratch(1);
    doc.rope = Rope::from_str(MARKDOWN_JA);
    doc
}

fn doc_from_str(s: &str) -> Document {
    let mut doc = Document::new_scratch(1);
    doc.rope = Rope::from_str(s);
    doc
}

// -------------------------------------------------------
// Line / char structure
// -------------------------------------------------------

#[test]
fn line_count() {
    let doc = doc_ja();
    let expected = MARKDOWN_JA.lines().count() + if MARKDOWN_JA.ends_with('\n') { 1 } else { 0 };
    assert_eq!(doc.rope.len_lines(), expected);
}

#[test]
fn char_count_matches() {
    let doc = doc_ja();
    assert_eq!(doc.rope.len_chars(), MARKDOWN_JA.chars().count());
}

#[test]
fn first_line_is_heading() {
    let doc = doc_ja();
    let line = doc.rope.line(0).to_string();
    assert_eq!(line.trim_end_matches('\n'), "# 竹取物語");
}

#[test]
fn line_with_bold_markdown() {
    let doc = doc_ja();
    let idx = (0..doc.rope.len_lines())
        .find(|&i| doc.rope.line(i).to_string().contains("**かぐや姫**"))
        .expect("bold markdown line not found");
    let line = doc.rope.line(idx).to_string();
    assert!(line.starts_with("- "));
}

#[test]
fn code_block_fence() {
    let doc = doc_ja();
    let idx = (0..doc.rope.len_lines())
        .find(|&i| doc.rope.line(i).to_string().starts_with("```rust"))
        .expect("code fence not found");
    let next = doc.rope.line(idx + 1).to_string();
    assert!(next.contains("fn main"));
}

#[test]
fn blockquote_line() {
    let doc = doc_ja();
    let idx = (0..doc.rope.len_lines())
        .find(|&i| doc.rope.line(i).to_string().starts_with("> "))
        .expect("blockquote not found");
    let line = doc.rope.line(idx).to_string();
    assert!(line.contains("天の羽衣"));
}

// -------------------------------------------------------
// Cursor movement over multi-byte chars
// -------------------------------------------------------

#[test]
fn move_right_across_japanese() {
    let mut doc = doc_ja();
    assert_eq!(doc.cursors[0], 0);
    doc.move_right(); // '#'
    doc.move_right(); // ' '
    assert_eq!(doc.cursors[0], 2);
    assert_eq!(doc.rope.char(doc.cursors[0]), '竹');
    doc.move_right(); // '竹'
    assert_eq!(doc.rope.char(doc.cursors[0]), '取');
}

#[test]
fn move_left_across_japanese() {
    let mut doc = doc_ja();
    doc.cursors[0] = 4;
    assert_eq!(doc.rope.char(doc.cursors[0]), '物');
    doc.move_left();
    assert_eq!(doc.rope.char(doc.cursors[0]), '取');
    doc.move_left();
    assert_eq!(doc.rope.char(doc.cursors[0]), '竹');
}

#[test]
fn move_right_stops_at_end() {
    let mut doc = doc_from_str("あ");
    doc.cursors[0] = 1;
    doc.move_right();
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn move_left_stops_at_zero() {
    let mut doc = doc_ja();
    doc.move_left();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn move_down_clamps_col_to_shorter_line() {
    let mut doc = doc_from_str("あいうえお\nかき\n");
    doc.move_to_line_end();
    assert_eq!(doc.cursor_col(), 5);
    doc.move_down();
    assert_eq!(doc.cursor_line(), 1);
    assert_eq!(doc.cursor_col(), 2);
}

#[test]
fn move_up_clamps_col() {
    let mut doc = doc_from_str("あ\nかきくけこ\n");
    doc.cursors[0] = doc.rope.line_to_char(1) + 5;
    assert_eq!(doc.cursor_col(), 5);
    doc.move_up();
    assert_eq!(doc.cursor_line(), 0);
    assert_eq!(doc.cursor_col(), 1);
}

#[test]
fn line_start_and_end() {
    let mut doc = doc_ja();
    doc.cursors[0] = 3;
    doc.move_to_line_start();
    assert_eq!(doc.cursors[0], 0);
    doc.move_to_line_end();
    assert_eq!(doc.cursor_col(), 6);
}

// -------------------------------------------------------
// Editing with Japanese text
// -------------------------------------------------------

#[test]
fn insert_japanese_char() {
    let mut doc = doc_from_str("あいう\n");
    doc.cursors[0] = 1;
    doc.insert_char('ん');
    assert_eq!(doc.rope.line(0).to_string(), "あんいう\n");
    assert_eq!(doc.cursors[0], 2);
    assert!(doc.dirty);
}

#[test]
fn insert_newline_splits_japanese_line() {
    let mut doc = doc_from_str("あいう\n");
    doc.cursors[0] = 2;
    doc.insert_newline();
    assert_eq!(doc.rope.line(0).to_string(), "あい\n");
    assert_eq!(doc.rope.line(1).to_string(), "う\n");
    assert_eq!(doc.cursor_line(), 1);
    assert_eq!(doc.cursor_col(), 0);
}

#[test]
fn delete_forward_japanese() {
    let mut doc = doc_from_str("かきく\n");
    doc.cursors[0] = 1;
    doc.delete_forward();
    assert_eq!(doc.rope.line(0).to_string(), "かく\n");
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn delete_backward_japanese() {
    let mut doc = doc_from_str("かきく\n");
    doc.cursors[0] = 2;
    doc.delete_backward();
    assert_eq!(doc.rope.line(0).to_string(), "かく\n");
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn kill_line_japanese() {
    let mut doc = doc_from_str("あいうえお\nかきく\n");
    doc.cursors[0] = 2;
    doc.kill_line();
    assert_eq!(doc.rope.line(0).to_string(), "あい\n");
    assert_eq!(doc.cursors[0], 2);
}

#[test]
fn kill_line_at_eol_joins_lines() {
    let mut doc = doc_from_str("あ\nい\n");
    doc.cursors[0] = 1;
    doc.kill_line();
    assert_eq!(doc.rope.to_string(), "あい\n");
}

#[test]
fn kill_line_on_markdown_heading() {
    let mut doc = doc_ja();
    doc.cursors[0] = 2;
    doc.kill_line();
    assert_eq!(doc.rope.line(0).to_string(), "# \n");
}

// -------------------------------------------------------
// Scroll
// -------------------------------------------------------

#[test]
fn ensure_cursor_visible_scrolls_down() {
    let mut doc = doc_ja();
    let last_line = doc.rope.len_lines() - 1;
    doc.cursors[0] = doc.rope.line_to_char(last_line);
    doc.ensure_cursor_visible(5);
    assert!(doc.scroll_offset > 0);
    assert!(doc.cursor_line() < doc.scroll_offset + 5);
}

#[test]
fn ensure_cursor_visible_scrolls_up() {
    let mut doc = doc_ja();
    doc.scroll_offset = 10;
    doc.cursors[0] = 0;
    doc.ensure_cursor_visible(5);
    assert_eq!(doc.scroll_offset, 0);
}

#[test]
fn ensure_cursor_visible_with_horizontal_scrolls_right_after_margin() {
    let mut doc = doc_from_str("0123456789abcdefghijklmnopqrstuvwxyz\n");
    let view_width = 10;
    let margin = 2;

    // right trigger = 10 - 1 - 2 = 7. At col 7 it should not scroll yet.
    doc.set_cursor_line_char(0, 7);
    doc.ensure_cursor_visible_with_horizontal(5, view_width, margin);
    assert_eq!(doc.horizontal_scroll_offset, 0);

    // Crossing trigger should scroll.
    doc.set_cursor_line_char(0, 8);
    doc.ensure_cursor_visible_with_horizontal(5, view_width, margin);
    assert_eq!(doc.horizontal_scroll_offset, 1);
}

#[test]
fn ensure_cursor_visible_with_horizontal_scrolls_left_after_margin() {
    let mut doc = doc_from_str("0123456789abcdefghijklmnopqrstuvwxyz\n");
    let view_width = 10;
    let margin = 2;
    doc.horizontal_scroll_offset = 10;

    // left trigger = 10 + 2 = 12. At col 12 it should not scroll.
    doc.set_cursor_line_char(0, 12);
    doc.ensure_cursor_visible_with_horizontal(5, view_width, margin);
    assert_eq!(doc.horizontal_scroll_offset, 10);

    // Crossing left trigger should scroll back.
    doc.set_cursor_line_char(0, 11);
    doc.ensure_cursor_visible_with_horizontal(5, view_width, margin);
    assert_eq!(doc.horizontal_scroll_offset, 9);
}

#[test]
fn ensure_cursor_visible_with_horizontal_resets_when_line_fits() {
    let mut doc = doc_from_str("short\nvery very long line here\n");
    doc.horizontal_scroll_offset = 7;
    doc.set_cursor_line_char(0, 2);
    doc.ensure_cursor_visible_with_horizontal(5, 20, 5);
    assert_eq!(doc.horizontal_scroll_offset, 0);
}

#[test]
fn display_cursor_display_col_counts_tabs() {
    let mut doc = doc_from_str("\tb\n");
    doc.set_cursor_line_char(0, 1);
    assert_eq!(doc.display_cursor_display_col(), 4);
}

// -------------------------------------------------------
// scroll_viewport
// -------------------------------------------------------

fn ten_line_doc() -> Document {
    let content: String = (0..10).map(|i| format!("line {i}\n")).collect();
    doc_from_str(&content)
}

#[test]
fn scroll_viewport_down() {
    let mut doc = ten_line_doc();
    doc.scroll_offset = 0;
    doc.cursors[0] = 0;
    doc.scroll_viewport(3, 5);
    assert_eq!(doc.scroll_offset, 3);
    // Cursor was at line 0, now outside viewport (0 < 3), so it should move to line 3
    assert_eq!(doc.cursor_line(), 3);
}

#[test]
fn scroll_viewport_up() {
    let mut doc = ten_line_doc();
    doc.scroll_offset = 5;
    doc.set_cursor_line_char(5, 0);
    doc.scroll_viewport(-3, 5);
    assert_eq!(doc.scroll_offset, 2);
    // Cursor at line 5 is within viewport [2..7), stays put
    assert_eq!(doc.cursor_line(), 5);
}

#[test]
fn scroll_viewport_clamps_at_zero() {
    let mut doc = ten_line_doc();
    doc.scroll_offset = 1;
    doc.cursors[0] = doc.rope.line_to_char(1);
    doc.scroll_viewport(-10, 5);
    assert_eq!(doc.scroll_offset, 0);
}

#[test]
fn scroll_viewport_clamps_at_end() {
    let mut doc = ten_line_doc();
    doc.scroll_viewport(100, 5);
    // 10 content lines + 1 trailing empty line = 11 lines, max scroll = 10
    assert_eq!(doc.scroll_offset, doc.rope.len_lines() - 1);
}

#[test]
fn scroll_viewport_preserves_column() {
    let mut doc = doc_from_str("abcdef\nghijkl\nmnopqr\nstuvwx\nyz1234\n56789a\nbcdefg\nhijklm\n");
    doc.set_cursor_line_char(0, 3); // cursor at col 3 of line 0
    doc.scroll_viewport(3, 3);
    // Cursor should have moved to line 3, col 3
    assert_eq!(doc.cursor_line(), 3);
    let line_start = doc.rope.line_to_char(3);
    assert_eq!(doc.cursors[0] - line_start, 3);
}

#[test]
fn scroll_viewport_no_op_when_cursor_in_view() {
    let mut doc = ten_line_doc();
    doc.scroll_offset = 0;
    doc.set_cursor_line_char(2, 0);
    let cursor_before = doc.cursors[0];
    doc.scroll_viewport(1, 5);
    assert_eq!(doc.scroll_offset, 1);
    // Cursor at line 2 is within [1..6), should not move
    assert_eq!(doc.cursors[0], cursor_before);
}

#[test]
fn scroll_viewport_ensure_cursor_visible_is_noop() {
    let mut doc = ten_line_doc();
    doc.scroll_offset = 0;
    doc.set_cursor_line_char(2, 0);
    doc.scroll_viewport(3, 5);
    let scroll_after = doc.scroll_offset;
    let cursor_after = doc.cursors[0];
    // ensure_cursor_visible should be a no-op since cursor is already in viewport
    doc.ensure_cursor_visible(5);
    assert_eq!(doc.scroll_offset, scroll_after);
    assert_eq!(doc.cursors[0], cursor_after);
}

// -------------------------------------------------------
// Word motions
// -------------------------------------------------------

#[test]
fn word_forward_ascii() {
    let mut doc = doc_from_str("hello world foo");
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 6);
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 12);
}

#[test]
fn word_forward_with_punctuation() {
    let mut doc = doc_from_str("foo, bar");
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 3);
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 5);
}

#[test]
fn word_forward_end_ascii() {
    let mut doc = doc_from_str("hello world");
    doc.move_word_forward_end();
    assert_eq!(doc.cursors[0], 4);
    doc.move_word_forward_end();
    assert_eq!(doc.cursors[0], 10);
}

#[test]
fn word_backward_ascii() {
    let mut doc = doc_from_str("hello world foo");
    doc.cursors[0] = 12;
    doc.move_word_backward();
    assert_eq!(doc.cursors[0], 6);
    doc.move_word_backward();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn word_forward_japanese() {
    let mut doc = doc_from_str("hello 世界 test");
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 6);
}

#[test]
fn word_backward_stops_at_zero() {
    let mut doc = doc_from_str("hello");
    doc.cursors[0] = 0;
    doc.move_word_backward();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn word_forward_stops_at_end() {
    let mut doc = doc_from_str("hello");
    doc.cursors[0] = 5;
    doc.move_word_forward();
    assert_eq!(doc.cursors[0], 5);
}

#[test]
fn long_word_forward_treats_punctuation_as_same_word() {
    let mut doc = doc_from_str("foo.bar baz");
    doc.move_long_word_forward();
    assert_eq!(doc.cursors[0], 8);
}

#[test]
fn long_word_backward_treats_punctuation_as_same_word() {
    let mut doc = doc_from_str("foo.bar baz");
    doc.cursors[0] = 8;
    doc.move_long_word_backward();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn visual_extend_word_keeps_anchor() {
    let mut doc = doc_from_str("hello world");
    doc.cursors[0] = 0;
    doc.set_anchor();
    doc.extend_word_forward();
    assert_eq!(doc.selection_anchor(), Some(0));
    assert_eq!(doc.cursors[0], 6);
}

#[test]
fn visual_extend_word_backward_keeps_anchor() {
    let mut doc = doc_from_str("hello world");
    doc.cursors[0] = 6;
    doc.set_anchor();
    doc.extend_word_backward();
    assert_eq!(doc.selection_anchor(), Some(6));
    assert_eq!(doc.cursors[0], 0);
}

// -------------------------------------------------------
// File I/O round-trip with Japanese markdown
// -------------------------------------------------------

#[test]
fn save_and_reopen_japanese() {
    let dir = std::env::temp_dir().join("kaguya_test");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_ja_doc.md");
    let path_str = path.to_str().unwrap();

    let mut doc = doc_from_str(MARKDOWN_JA);
    doc.file_path = Some(path.clone());
    doc.dirty = true;
    doc.save().unwrap();
    assert!(!doc.dirty);

    let doc2 = Document::from_file(2, path_str);
    assert_eq!(doc2.rope.to_string(), MARKDOWN_JA);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_as_sets_file_path_and_clears_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saved.md");

    let mut doc = doc_from_str("hello");
    doc.dirty = true;
    let msg = doc.save_as(&path).unwrap();
    let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());

    assert_eq!(doc.file_path.as_deref(), Some(canonical.as_path()));
    assert!(!doc.dirty);
    assert!(msg.contains("Wrote"));
    assert_eq!(fs::read_to_string(path).unwrap(), "hello");
}

#[test]
fn save_as_overwrites_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overwrite.txt");
    fs::write(&path, "old").unwrap();

    let mut doc = doc_from_str("new");
    doc.save_as(&path).unwrap();

    assert_eq!(fs::read_to_string(path).unwrap(), "new");
}

#[test]
fn save_as_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("deep").join("file.txt");

    let mut doc = doc_from_str("created");
    doc.save_as(&path).unwrap();

    assert!(path.exists());
    assert_eq!(fs::read_to_string(path).unwrap(), "created");
}

#[test]
fn save_as_updates_status_bar_path_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("status.rs");

    let mut doc = doc_from_str("fn main() {}");
    doc.save_as(&path).unwrap();

    assert_ne!(doc.status_bar_path(), "[scratch]");
}

// -------------------------------------------------------
// display_name
// -------------------------------------------------------

#[test]
fn display_name_scratch() {
    let doc = Document::new_scratch(1);
    assert_eq!(doc.display_name(), "[scratch]");
}

#[test]
fn display_name_with_path() {
    let doc = Document::from_file(1, "src/main.rs");
    assert_eq!(doc.display_name(), "src/main.rs");
}

#[test]
fn status_bar_path_scratch() {
    let doc = Document::new_scratch(1);
    assert_eq!(doc.status_bar_path(), "[scratch]");
}

#[test]
fn status_bar_path_in_git_repo() {
    // Test with a file that exists in the git repo
    let doc = Document::from_file(1, "src/core/document/mod.rs");
    let path = doc.status_bar_path();
    // Should be in format "[repo_name] relative/path"
    assert!(path.starts_with('['));
    assert!(path.contains("] "));
    assert!(path.contains("src/core/document/mod.rs"));
}

#[test]
fn extract_repo_name_from_github_ssh() {
    let remote = "git@github.com:user/my-repo.git";
    let name = Document::extract_repo_name_from_remote(remote);
    assert_eq!(name, Some("my-repo".to_string()));
}

#[test]
fn extract_repo_name_from_github_https() {
    let remote = "https://github.com/user/my-repo.git";
    let name = Document::extract_repo_name_from_remote(remote);
    assert_eq!(name, Some("my-repo".to_string()));
}

#[test]
fn extract_repo_name_without_dot_git() {
    let remote = "https://github.com/user/my-repo";
    let name = Document::extract_repo_name_from_remote(remote);
    assert_eq!(name, Some("my-repo".to_string()));
}

// -------------------------------------------------------
// Undo / Redo
// -------------------------------------------------------

#[test]
fn undo_insert_char() {
    let mut doc = doc_from_str("abc\n");
    doc.cursors[0] = 1;
    doc.insert_char('X');
    assert_eq!(doc.rope.to_string(), "aXbc\n");
    assert_eq!(doc.cursors[0], 2);

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "abc\n");
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn redo_insert_char() {
    let mut doc = doc_from_str("abc\n");
    doc.cursors[0] = 1;
    doc.insert_char('X');
    doc.undo();
    assert_eq!(doc.rope.to_string(), "abc\n");

    assert!(doc.redo());
    assert_eq!(doc.rope.to_string(), "aXbc\n");
    assert_eq!(doc.cursors[0], 2);
}

#[test]
fn undo_delete_backward() {
    let mut doc = doc_from_str("abc\n");
    doc.cursors[0] = 2;
    doc.delete_backward();
    assert_eq!(doc.rope.to_string(), "ac\n");
    assert_eq!(doc.cursors[0], 1);

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "abc\n");
    assert_eq!(doc.cursors[0], 2);
}

#[test]
fn undo_delete_forward() {
    let mut doc = doc_from_str("abc\n");
    doc.cursors[0] = 1;
    doc.delete_forward();
    assert_eq!(doc.rope.to_string(), "ac\n");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "abc\n");
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn undo_kill_line() {
    let mut doc = doc_from_str("hello world\n");
    doc.cursors[0] = 5;
    doc.kill_line();
    assert_eq!(doc.rope.to_string(), "hello\n");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "hello world\n");
    assert_eq!(doc.cursors[0], 5);
}

#[test]
fn undo_insert_newline() {
    let mut doc = doc_from_str("abc\n");
    doc.cursors[0] = 2;
    doc.insert_newline();
    assert_eq!(doc.rope.to_string(), "ab\nc\n");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "abc\n");
    assert_eq!(doc.cursors[0], 2);
}

#[test]
fn undo_japanese_insert() {
    let mut doc = doc_from_str("あいう\n");
    doc.cursors[0] = 1;
    doc.insert_char('ん');
    assert_eq!(doc.rope.to_string(), "あんいう\n");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "あいう\n");
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn undo_japanese_kill_line() {
    let mut doc = doc_from_str("あいうえお\n");
    doc.cursors[0] = 2;
    doc.kill_line();
    assert_eq!(doc.rope.to_string(), "あい\n");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "あいうえお\n");
    assert_eq!(doc.cursors[0], 2);
}

#[test]
fn multiple_undo_redo() {
    let mut doc = doc_from_str("");
    doc.insert_char('a');
    doc.insert_char('b');
    doc.insert_char('c');
    assert_eq!(doc.rope.to_string(), "abc");

    doc.undo(); // remove 'c'
    assert_eq!(doc.rope.to_string(), "ab");
    doc.undo(); // remove 'b'
    assert_eq!(doc.rope.to_string(), "a");
    doc.redo(); // re-insert 'b'
    assert_eq!(doc.rope.to_string(), "ab");
    doc.redo(); // re-insert 'c'
    assert_eq!(doc.rope.to_string(), "abc");
}

#[test]
fn undo_nothing_returns_false() {
    let mut doc = doc_from_str("hello");
    assert!(!doc.undo());
}

#[test]
fn redo_nothing_returns_false() {
    let mut doc = doc_from_str("hello");
    assert!(!doc.redo());
}

#[test]
fn new_edit_clears_redo() {
    let mut doc = doc_from_str("");
    doc.insert_char('a');
    doc.insert_char('b');
    doc.undo(); // redo has 'b'
    doc.insert_char('c'); // should clear redo
    assert!(!doc.redo()); // redo stack cleared
    assert_eq!(doc.rope.to_string(), "ac");
}

// -------------------------------------------------------
// Multi-cursor undo/redo
// -------------------------------------------------------

#[test]
fn multi_cursor_undo_insert_char() {
    let mut doc = doc_from_str("ab\ncd\n");
    doc.cursors[0] = 0; // before 'a'
    doc.cursors.push(3); // before 'c'
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();
    assert_eq!(doc.cursor_count(), 2);

    doc.insert_char('X');
    assert_eq!(doc.rope.to_string(), "Xab\nXcd\n");

    // Undo should reverse BOTH insertions
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "ab\ncd\n");
    // All cursor positions should be restored
    assert_eq!(doc.cursor_count(), 2);
    assert_eq!(doc.cursors[0], 0);
    assert_eq!(doc.cursors[1], 3);
}

#[test]
fn multi_cursor_undo_delete_backward() {
    let mut doc = doc_from_str("ab\ncd\n");
    doc.cursors[0] = 1; // after 'a'
    doc.cursors.push(4); // after 'c'
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();
    assert_eq!(doc.cursor_count(), 2);

    doc.delete_backward();
    assert_eq!(doc.rope.to_string(), "b\nd\n");

    // Undo should restore BOTH deleted characters
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "ab\ncd\n");
    // All cursor positions should be restored
    assert_eq!(doc.cursor_count(), 2);
    assert_eq!(doc.cursors[0], 1);
    assert_eq!(doc.cursors[1], 4);
}

#[test]
fn multi_cursor_redo_insert_char() {
    let mut doc = doc_from_str("ab\ncd\n");
    doc.cursors[0] = 0;
    doc.cursors.push(3);
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.insert_char('X');
    assert_eq!(doc.rope.to_string(), "Xab\nXcd\n");

    doc.undo();
    assert_eq!(doc.rope.to_string(), "ab\ncd\n");

    // Redo should re-insert BOTH characters
    assert!(doc.redo());
    assert_eq!(doc.rope.to_string(), "Xab\nXcd\n");
    // Cursor positions should be restored to after insertions
    assert_eq!(doc.cursor_count(), 2);
    assert_eq!(doc.cursors[0], 1);
    assert_eq!(doc.cursors[1], 5);
}

// -------------------------------------------------------
// Transaction-based undo/redo
// -------------------------------------------------------

#[test]
fn grouped_insert_single_undo() {
    let mut doc = doc_from_str("");
    doc.begin_transaction();
    doc.insert_char('a');
    doc.insert_char('b');
    doc.insert_char('c');
    doc.commit_transaction();
    assert_eq!(doc.rope.to_string(), "abc");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn grouped_insert_undo_redo_round_trip() {
    let mut doc = doc_from_str("");
    doc.begin_transaction();
    doc.insert_char('a');
    doc.insert_char('b');
    doc.insert_char('c');
    doc.commit_transaction();

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");

    assert!(doc.redo());
    assert_eq!(doc.rope.to_string(), "abc");
    assert_eq!(doc.cursors[0], 3);
}

#[test]
fn mixed_normal_and_insert_undo() {
    let mut doc = doc_from_str("hello\n");
    // Normal mode: atomic delete
    doc.cursors[0] = 0;
    doc.delete_forward(); // delete 'h'
    assert_eq!(doc.rope.to_string(), "ello\n");

    // Insert mode: grouped inserts
    doc.begin_transaction();
    doc.insert_char('H');
    doc.insert_char('i');
    doc.commit_transaction();
    assert_eq!(doc.rope.to_string(), "Hiello\n");

    // Undo grouped insert
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "ello\n");

    // Undo atomic delete
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "hello\n");
}

#[test]
fn undo_flushes_open_transaction() {
    let mut doc = doc_from_str("");
    doc.begin_transaction();
    doc.insert_char('a');
    doc.insert_char('b');
    // undo without explicit commit -- should flush first
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");
}

#[test]
fn empty_insert_session_no_undo_entry() {
    let mut doc = doc_from_str("hello");
    doc.begin_transaction();
    // No edits
    doc.commit_transaction();
    // Nothing to undo
    assert!(!doc.undo());
}

#[test]
fn grouped_insert_with_backspace() {
    let mut doc = doc_from_str("");
    doc.begin_transaction();
    doc.insert_char('a');
    doc.insert_char('b');
    doc.insert_char('c');
    doc.delete_backward(); // delete 'c'
    doc.commit_transaction();
    assert_eq!(doc.rope.to_string(), "ab");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn grouped_insert_with_newline() {
    let mut doc = doc_from_str("");
    doc.begin_transaction();
    doc.insert_char('a');
    doc.insert_newline();
    doc.insert_char('b');
    doc.commit_transaction();
    assert_eq!(doc.rope.to_string(), "a\nb");

    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");
    assert_eq!(doc.cursors[0], 0);
}

// -------------------------------------------------------
// OpenLineBelow (o)
//
// Simulates the `o` command sequence at Document level:
//   move_to_line_end -> begin_transaction -> insert_newline
// -------------------------------------------------------

/// Helper: perform the `o` (open-line-below) sequence on a document.
fn open_line_below(doc: &mut Document) {
    doc.move_to_line_end();
    doc.begin_transaction();
    doc.insert_newline();
}

#[test]
fn open_line_below_basic() {
    let mut doc = doc_from_str("hello\nworld\n");
    // Cursor in the middle of line 0
    doc.cursors[0] = 2;
    open_line_below(&mut doc);
    doc.commit_transaction();

    // A newline was inserted at the end of "hello", producing "hello\n\nworld\n"
    assert_eq!(doc.rope.to_string(), "hello\n\nworld\n");
    // Cursor is on the new (empty) line 1, column 0
    assert_eq!(doc.cursor_line(), 1);
    assert_eq!(doc.cursor_col(), 0);
}

#[test]
fn open_line_below_last_line() {
    let mut doc = doc_from_str("alpha\nbeta\n");
    // Place cursor on the last logical line (the empty line after trailing '\n')
    let last_line = doc.rope.len_lines() - 1;
    doc.cursors[0] = doc.rope.line_to_char(last_line);
    open_line_below(&mut doc);
    doc.commit_transaction();

    // A new line is appended at the very end
    assert_eq!(doc.rope.to_string(), "alpha\nbeta\n\n");
    assert_eq!(doc.cursor_line(), last_line + 1);
    assert_eq!(doc.cursor_col(), 0);
}

#[test]
fn open_line_below_empty_doc() {
    let mut doc = doc_from_str("");
    open_line_below(&mut doc);
    doc.commit_transaction();

    assert_eq!(doc.rope.to_string(), "\n");
    assert_eq!(doc.cursor_line(), 1);
    assert_eq!(doc.cursor_col(), 0);
}

#[test]
fn open_line_below_japanese() {
    let mut doc = doc_from_str("あいうえお\nかきくけこ\n");
    // Cursor in the middle of the first Japanese line
    doc.cursors[0] = 2; // after 'あ','い'
    open_line_below(&mut doc);
    doc.commit_transaction();

    // Original content preserved; new line inserted after line 0
    assert_eq!(doc.rope.to_string(), "あいうえお\n\nかきくけこ\n");
    assert_eq!(doc.cursor_line(), 1);
    assert_eq!(doc.cursor_col(), 0);
}

#[test]
fn open_line_below_undo() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 3;
    open_line_below(&mut doc);
    // Type some characters in the new line while still in the transaction
    doc.insert_char('x');
    doc.insert_char('y');
    doc.commit_transaction();

    assert_eq!(doc.rope.to_string(), "hello\nxy\nworld\n");

    // A single undo should revert the newline AND the typed characters.
    // Cursor restores to line-end (5) because move_to_line_end runs
    // before begin_transaction, so the transaction records cursor=5.
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "hello\nworld\n");
    assert_eq!(doc.cursors[0], 5);
}

#[test]
fn open_line_below_dirty() {
    let mut doc = doc_from_str("test\n");
    assert!(!doc.dirty);
    open_line_below(&mut doc);
    doc.commit_transaction();
    assert!(doc.dirty);
}

#[test]
fn open_line_below_pending_edits() {
    let mut doc = doc_from_str("test\n");
    assert!(doc.pending_edits.is_empty());
    open_line_below(&mut doc);
    doc.commit_transaction();
    assert!(!doc.pending_edits.is_empty());
}

// -------------------------------------------------------
// Selection
// -------------------------------------------------------

#[test]
fn set_and_clear_anchor() {
    let mut doc = doc_from_str("hello\n");
    doc.cursors[0] = 3;
    doc.set_anchor();
    assert_eq!(doc.selection_anchor(), Some(3));
    doc.clear_anchor();
    assert_eq!(doc.selection_anchor(), None);
}

#[test]
fn shift_extend_right_moves_display_cursor_to_head() {
    let mut doc = doc_from_str("abcd");
    doc.cursors[0] = 0;

    doc.extend_right();

    assert_eq!(doc.display_cursor(), 1);
    assert_eq!(doc.selection_range(), Some((0, 1)));
}

#[test]
fn clear_anchor_after_shift_forward_selection_keeps_cursor_position() {
    let mut doc = doc_from_str("abcd");
    doc.cursors[0] = 0;

    doc.extend_right();
    assert_eq!(doc.cursors[0], 1);
    doc.clear_anchor();

    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn clear_anchor_after_shift_word_forward_keeps_cursor_position() {
    let mut doc = doc_from_str("hello world");
    doc.cursors[0] = 0;

    doc.extend_word_forward_shift();
    assert_eq!(doc.cursors[0], 6);
    assert_eq!(doc.display_cursor(), 6);
    doc.clear_anchor();

    assert_eq!(doc.cursors[0], 6);
}

#[test]
fn selection_range_forward() {
    let mut doc = doc_from_str("hello\n");
    doc.selections[0] = Some(Selection::tail_on_forward(1, 3));
    doc.cursors[0] = 3;
    assert_eq!(doc.selection_range(), Some((1, 3)));
}

#[test]
fn selection_range_backward() {
    let mut doc = doc_from_str("hello\n");
    doc.selections[0] = Some(Selection::tail_on_forward(4, 1));
    doc.cursors[0] = 1;
    assert_eq!(doc.selection_range(), Some((1, 4)));
}

#[test]
fn selection_range_none() {
    let doc = doc_from_str("hello\n");
    assert_eq!(doc.selection_range(), None);
}

#[test]
fn selection_text_basic() {
    let mut doc = doc_from_str("hello world\n");
    doc.selections[0] = Some(Selection::tail_on_forward(0, 5));
    doc.cursors[0] = 4;
    assert_eq!(doc.selection_text(), Some("hello".to_string()));
}

#[test]
fn select_line_basic() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 2;
    doc.select_line();
    assert_eq!(doc.selection_anchor(), Some(0));
    assert_eq!(doc.cursors[0], 6); // one-past '\n' after "hello"
    assert_eq!(doc.selection_text(), Some("hello\n".to_string()));
}

#[test]
fn select_line_includes_newline_via_range() {
    let mut doc = doc_from_str("abc\ndef\n");
    doc.cursors[0] = 1;
    doc.select_line();
    // anchor=0, cursor=3 ('\n'), range = [0, 4) = "abc\n"
    assert_eq!(doc.selection_range(), Some((0, 4)));
}

#[test]
fn select_line_last_line_without_trailing_lf() {
    let mut doc = doc_from_str("top\nlast");
    doc.cursors[0] = 4; // 'l' in "last"
    doc.select_line();
    assert_eq!(doc.selection_range(), Some((4, 8)));
    assert_eq!(doc.selection_text(), Some("last".to_string()));
}

#[test]
fn select_line_empty_line_selects_lf() {
    let mut doc = doc_from_str("\nnext\n");
    doc.cursors[0] = 0;
    doc.select_line();
    assert_eq!(doc.selection_range(), Some((0, 1)));
    assert_eq!(doc.selection_text(), Some("\n".to_string()));
}

#[test]
fn delete_selected_line_removes_line_and_lf() {
    let mut doc = doc_from_str("keep top\nremove me\nkeep end\n");
    doc.cursors[0] = 10; // in "remove me"
    doc.select_line();
    let (start, end) = doc.selection_range().expect("line should be selected");
    let deleted = doc.delete_range(start, end);
    assert_eq!(deleted, "remove me\n");
    assert_eq!(doc.rope.to_string(), "keep top\nkeep end\n");
}

#[test]
fn select_word_at_middle_of_word() {
    let mut doc = doc_from_str("hello world\n");
    doc.select_word_at(2);
    assert_eq!(doc.selection_range(), Some((0, 5)));
    assert_eq!(doc.selection_text(), Some("hello".to_string()));
}

#[test]
fn select_word_at_start_of_word() {
    let mut doc = doc_from_str("hello world\n");
    doc.select_word_at(6);
    assert_eq!(doc.selection_range(), Some((6, 11)));
    assert_eq!(doc.selection_text(), Some("world".to_string()));
}

#[test]
fn select_word_at_whitespace_selects_run() {
    let mut doc = doc_from_str("foo   bar\n");
    doc.select_word_at(4);
    assert_eq!(doc.selection_text(), Some("   ".to_string()));
}

#[test]
fn select_word_at_punctuation_run() {
    let mut doc = doc_from_str("foo::bar\n");
    doc.select_word_at(3);
    assert_eq!(doc.selection_text(), Some("::".to_string()));
}

#[test]
fn select_word_at_does_not_cross_newline() {
    let mut doc = doc_from_str("foo\nbar\n");
    doc.select_word_at(2);
    assert_eq!(doc.selection_text(), Some("foo".to_string()));
}

#[test]
fn select_word_at_on_newline_is_noop() {
    let mut doc = doc_from_str("foo\nbar\n");
    doc.select_word_at(3); // the '\n' itself
    assert_eq!(doc.selection_range(), None);
}

#[test]
fn select_word_at_empty_doc_is_noop() {
    let mut doc = doc_from_str("");
    doc.select_word_at(0);
    assert_eq!(doc.selection_range(), None);
}

#[test]
fn select_word_at_past_eof_snaps_to_last_char() {
    let mut doc = doc_from_str("hello");
    doc.select_word_at(99);
    assert_eq!(doc.selection_text(), Some("hello".to_string()));
}

#[test]
fn select_word_at_multibyte() {
    // Each Japanese char is one rope char
    let mut doc = doc_from_str("吾輩は猫である\n");
    doc.select_word_at(2); // on 'は'
    // is_alphanumeric is true for Japanese — whole run becomes one word
    assert_eq!(doc.selection_text(), Some("吾輩は猫である".to_string()));
}

#[test]
fn extend_line_selection_down() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.select_line(); // selects line 0
    doc.extend_line_selection_down(); // extends to line 1
    assert_eq!(doc.selection_anchor(), Some(0));
    // one-past end of line 1 ("bbb\n"), i.e. start of line 2
    assert_eq!(doc.cursors[0], 8);
}

#[test]
fn delete_range_basic() {
    let mut doc = doc_from_str("hello world\n");
    let deleted = doc.delete_range(5, 11);
    assert_eq!(deleted, " world");
    assert_eq!(doc.rope.to_string(), "hello\n");
    assert_eq!(doc.cursors[0], 5);
}

#[test]
fn delete_range_undo() {
    let mut doc = doc_from_str("hello world\n");
    doc.delete_range(0, 6);
    assert_eq!(doc.rope.to_string(), "world\n");
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "hello world\n");
}

#[test]
fn delete_range_japanese() {
    let mut doc = doc_from_str("あいうえお\n");
    let deleted = doc.delete_range(1, 4);
    assert_eq!(deleted, "いうえ");
    assert_eq!(doc.rope.to_string(), "あお\n");
}

#[test]
fn insert_text_at_basic() {
    let mut doc = doc_from_str("hello\n");
    doc.insert_text_at(5, " world");
    assert_eq!(doc.rope.to_string(), "hello world\n");
    assert_eq!(doc.cursors[0], 11);
}

#[test]
fn insert_text_at_undo() {
    let mut doc = doc_from_str("hello\n");
    doc.insert_text_at(5, " world");
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "hello\n");
}

#[test]
fn insert_text_at_japanese() {
    let mut doc = doc_from_str("あう\n");
    doc.insert_text_at(1, "い");
    assert_eq!(doc.rope.to_string(), "あいう\n");
}

#[test]
fn insert_text_normalizes_crlf_to_lf() {
    let mut doc = doc_from_str("");
    doc.insert_text("a\r\nb");
    assert_eq!(doc.rope.to_string(), "a\nb");
}

#[test]
fn insert_text_normalizes_cr_to_lf() {
    let mut doc = doc_from_str("");
    doc.insert_text("a\rb");
    assert_eq!(doc.rope.to_string(), "a\nb");
}

#[test]
fn insert_text_crlf_undo_redo_roundtrip() {
    let mut doc = doc_from_str("");
    doc.insert_text("x\r\ny");
    assert_eq!(doc.rope.to_string(), "x\ny");
    assert!(doc.undo());
    assert_eq!(doc.rope.to_string(), "");
    assert!(doc.redo());
    assert_eq!(doc.rope.to_string(), "x\ny");
}

#[test]
fn insert_text_at_normalizes_crlf_to_lf() {
    let mut doc = doc_from_str("hello\n");
    doc.insert_text_at(5, "\r\nworld");
    assert_eq!(doc.rope.to_string(), "hello\nworld\n");
}

#[test]
fn insert_text_at_normalizes_cr_to_lf() {
    let mut doc = doc_from_str("ab\n");
    doc.insert_text_at(1, "\r");
    assert_eq!(doc.rope.to_string(), "a\nb\n");
}

// -------------------------------------------------------
// move_to_file_start / move_to_file_end
// -------------------------------------------------------

#[test]
fn move_to_file_start_from_middle() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 8;
    doc.move_to_file_start();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn move_to_file_start_already_at_start() {
    let mut doc = doc_from_str("hello\n");
    doc.cursors[0] = 0;
    doc.move_to_file_start();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn move_to_file_end_from_start() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 0;
    doc.move_to_file_end();
    assert_eq!(doc.cursors[0], doc.rope.len_chars());
}

#[test]
fn move_to_file_end_already_at_end() {
    let mut doc = doc_from_str("hello\n");
    let end = doc.rope.len_chars();
    doc.cursors[0] = end;
    doc.move_to_file_end();
    assert_eq!(doc.cursors[0], end);
}

#[test]
fn move_to_file_end_empty_doc() {
    let mut doc = doc_from_str("");
    doc.move_to_file_end();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn move_to_file_start_japanese() {
    let mut doc = doc_ja();
    doc.cursors[0] = 10;
    doc.move_to_file_start();
    assert_eq!(doc.cursors[0], 0);
}

#[test]
fn move_to_file_end_japanese() {
    let mut doc = doc_ja();
    doc.move_to_file_end();
    assert_eq!(doc.cursors[0], doc.rope.len_chars());
}

// -------------------------------------------------------
// Multi-cursor
// -------------------------------------------------------

#[test]
fn add_cursor_below_basic() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.cursors[0] = 1; // line 0, col 1
    assert!(doc.add_cursor_below());
    assert_eq!(doc.cursor_count(), 2);
    // Primary cursor unchanged
    assert_eq!(doc.cursors[0], 1);
    // Secondary cursor at line 1, col 1 (char offset 4+1=5)
    assert_eq!(doc.cursors[1], 5);
}

#[test]
fn add_cursor_above_basic() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.cursors[0] = 5; // line 1, col 1
    assert!(doc.add_cursor_above());
    assert_eq!(doc.cursor_count(), 2);
    // Primary cursor unchanged
    assert_eq!(doc.cursors[0], 5);
    // Secondary cursor at line 0, col 1
    assert_eq!(doc.cursors[1], 1);
}

#[test]
fn add_cursor_at_adds_new_cursor() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 1;
    assert!(doc.add_cursor_at(7));
    assert_eq!(doc.cursor_count(), 2);
    assert_eq!(doc.cursors[0], 1);
    assert!(doc.cursors.contains(&7));
}

#[test]
fn add_cursor_at_dedups_and_preserves_primary() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 7;
    assert!(doc.add_cursor_at(1));
    assert_eq!(doc.cursors[0], 7);
    assert!(!doc.add_cursor_at(1));
    assert_eq!(doc.cursor_count(), 2);
}

#[test]
fn add_cursor_below_clamps_to_shorter_line() {
    let mut doc = doc_from_str("hello\nab\nccc\n");
    doc.cursors[0] = 4; // line 0, col 4 ('o')
    assert!(doc.add_cursor_below());
    assert_eq!(doc.cursor_count(), 2);
    // Line 1 has only 2 chars, so col is clamped to 2
    let expected = doc.rope.line_to_char(1) + 2; // line 1 col 2
    assert_eq!(doc.cursors[1], expected);
}

#[test]
fn add_cursor_above_clamps_to_shorter_line() {
    let mut doc = doc_from_str("ab\nhello\n");
    doc.cursors[0] = doc.rope.line_to_char(1) + 4; // line 1, col 4
    assert!(doc.add_cursor_above());
    assert_eq!(doc.cursor_count(), 2);
    // Line 0 has only 2 chars, so col is clamped to 2
    assert_eq!(doc.cursors[1], 2);
}

#[test]
fn add_cursor_below_fails_at_last_line() {
    let mut doc = doc_from_str("only\n");
    doc.cursors[0] = 2;
    // Move to line 1 (empty line after trailing newline)
    doc.move_down();
    let result = doc.add_cursor_below();
    assert!(!result);
    assert_eq!(doc.cursor_count(), 1);
}

#[test]
fn add_cursor_above_fails_at_first_line() {
    let mut doc = doc_from_str("hello\nworld\n");
    doc.cursors[0] = 2; // line 0
    let result = doc.add_cursor_above();
    assert!(!result);
    assert_eq!(doc.cursor_count(), 1);
}

#[test]
fn remove_secondary_cursors() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.cursors[0] = 1;
    doc.add_cursor_below();
    doc.add_cursor_below();
    assert_eq!(doc.cursor_count(), 3);
    doc.remove_secondary_cursors();
    assert_eq!(doc.cursor_count(), 1);
    assert_eq!(doc.cursors[0], 1);
}

#[test]
fn multi_cursor_move_right() {
    let mut doc = doc_from_str("aaa\nbbb\n");
    doc.cursors[0] = 0;
    doc.add_cursor_below();
    assert_eq!(doc.cursor_count(), 2);
    doc.move_right();
    assert_eq!(doc.cursors[0], 1);
    assert_eq!(doc.cursors[1], 5); // line_to_char(1) + 1 = 4 + 1 = 5
}

#[test]
fn multi_cursor_insert_char() {
    let mut doc = doc_from_str("aa\nbb\n");
    doc.cursors[0] = 1;
    doc.add_cursor_below();
    // cursors at positions 1 and 4 (line 0 col 1, line 1 col 1)
    doc.insert_char('X');
    // After insert, text should be "aXa\nbXb\n"
    assert_eq!(doc.rope.to_string(), "aXa\nbXb\n");
}

#[test]
fn multi_cursor_delete_backward() {
    let mut doc = doc_from_str("abc\ndef\n");
    doc.cursors[0] = 2; // after 'b'
    doc.add_cursor_below();
    // cursors at positions 2 and 6 (line 0 col 2, line 1 col 2)
    doc.delete_backward();
    // Should delete 'b' and 'e'
    assert_eq!(doc.rope.to_string(), "ac\ndf\n");
}

#[test]
fn cursors_merge_when_they_overlap() {
    let mut doc = doc_from_str("a\nb\n");
    doc.cursors[0] = 0;
    doc.add_cursor_below();
    assert_eq!(doc.cursor_count(), 2);
    // Move both cursors to start of their lines, then keep moving left
    doc.move_to_line_start();
    // Both cursors at col 0, they should still be distinct (line 0 and line 1)
    assert_eq!(doc.cursor_count(), 2);
    // Now move cursor on line 1 up, it should merge with cursor on line 0
    doc.move_up();
    // After merging, should have 1 cursor
    assert_eq!(doc.cursor_count(), 1);
}

#[test]
fn has_multiple_cursors() {
    let mut doc = doc_from_str("aaa\nbbb\n");
    assert!(!doc.has_multiple_cursors());
    doc.add_cursor_below();
    assert!(doc.has_multiple_cursors());
    doc.remove_secondary_cursors();
    assert!(!doc.has_multiple_cursors());
}

#[test]
fn add_cursors_to_top_basic() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.cursors[0] = doc.rope.line_to_char(2) + 1; // line 2, col 1
    doc.add_cursors_to_top();
    assert_eq!(doc.cursor_count(), 3);
    // Primary stays at index 0
    assert_eq!(doc.cursors[0], doc.rope.line_to_char(2) + 1);
    // Should have cursors on lines 0 and 1 at col 1
    let mut positions = doc.cursors.clone();
    positions.sort();
    assert_eq!(
        positions,
        vec![
            1,
            doc.rope.line_to_char(1) + 1,
            doc.rope.line_to_char(2) + 1
        ]
    );
}

#[test]
fn add_cursors_to_bottom_basic() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\n");
    doc.cursors[0] = 1; // line 0, col 1
    doc.add_cursors_to_bottom();
    // Lines: 0="aaa\n", 1="bbb\n", 2="ccc\n", 3="" (trailing)
    // Should add cursors on lines 1, 2, and 3
    assert_eq!(doc.cursor_count(), 4);
    assert_eq!(doc.cursors[1], doc.rope.line_to_char(1) + 1);
    assert_eq!(doc.cursors[2], doc.rope.line_to_char(2) + 1);
    // Line 3 is empty, col clamped to 0
    assert_eq!(doc.cursors[3], doc.rope.line_to_char(3));
}

#[test]
fn add_cursors_to_top_clamps_columns() {
    let mut doc = doc_from_str("ab\nhello\nxy\n");
    doc.cursors[0] = doc.rope.line_to_char(1) + 4; // line 1, col 4
    doc.add_cursors_to_top();
    assert_eq!(doc.cursor_count(), 2);
    // Line 0 has 2 chars, col clamped to 2
    assert_eq!(doc.cursors[1], 2);
}

#[test]
fn add_cursors_to_bottom_clamps_columns() {
    let mut doc = doc_from_str("hello\nab\nxy\n");
    doc.cursors[0] = 4; // line 0, col 4
    doc.add_cursors_to_bottom();
    // Line 1 has 2 chars -> clamped to col 2
    // Line 2 has 2 chars -> clamped to col 2
    // Line 3 is empty -> clamped to col 0
    assert_eq!(doc.cursor_count(), 4);
    assert_eq!(doc.cursors[1], doc.rope.line_to_char(1) + 2);
    assert_eq!(doc.cursors[2], doc.rope.line_to_char(2) + 2);
    assert_eq!(doc.cursors[3], doc.rope.line_to_char(3));
}

#[test]
fn add_cursors_to_top_already_at_top() {
    let mut doc = doc_from_str("aaa\nbbb\n");
    doc.cursors[0] = 1; // line 0, col 1
    doc.add_cursors_to_top();
    assert_eq!(doc.cursor_count(), 1); // no cursors added
}

#[test]
fn add_cursors_to_bottom_already_at_bottom() {
    let mut doc = doc_from_str("aaa\n");
    // Last line is line 1 (empty after trailing newline)
    doc.cursors[0] = doc.rope.line_to_char(1); // line 1
    doc.add_cursors_to_bottom();
    assert_eq!(doc.cursor_count(), 1); // no cursors added
}

#[test]
fn add_cursors_to_top_with_existing_multi_cursors() {
    let mut doc = doc_from_str("aaa\nbbb\nccc\nddd\n");
    doc.cursors[0] = 1; // line 0, col 1
    doc.add_cursor_below(); // adds cursor on line 1
    doc.add_cursor_below(); // adds cursor on line 2
    assert_eq!(doc.cursor_count(), 3);
    // Now add cursors to top from topmost (line 0) -> nothing to add
    doc.add_cursors_to_top();
    assert_eq!(doc.cursor_count(), 3); // unchanged
}

#[test]
fn multi_cursor_japanese() {
    let mut doc = doc_from_str("あいう\nかきく\n");
    doc.cursors[0] = 1; // after 'あ'
    assert!(doc.add_cursor_below());
    assert_eq!(doc.cursor_count(), 2);
    // Line 1 starts at char 4, col 1 = char 5
    assert_eq!(doc.cursors[1], 5);
    doc.insert_char('ん');
    assert_eq!(doc.rope.to_string(), "あんいう\nかんきく\n");
}

#[test]
fn multi_cursor_word_forward() {
    let mut doc = doc_from_str("hello world\nfoo bar\n");
    doc.cursors[0] = 0; // start of "hello"
    doc.add_cursor_below();
    assert_eq!(doc.cursor_count(), 2);
    // Both cursors at col 0
    doc.move_word_forward();
    // Both should move to the word after first word
    assert_eq!(doc.cursors[0], 6); // start of "world"
    assert_eq!(doc.cursors[1], 16); // start of "bar" (line 1 char 12 + 4)
}

#[test]
fn multi_cursor_word_backward() {
    let mut doc = doc_from_str("hello world\nfoo bar\n");
    doc.cursors[0] = 6; // start of "world"
    doc.cursors.push(16); // start of "bar"
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();
    assert_eq!(doc.cursor_count(), 2);
    doc.move_word_backward();
    // Both should move back one word
    assert_eq!(doc.cursors[0], 0); // start of "hello"
    assert_eq!(doc.cursors[1], 12); // start of "foo"
}

#[test]
fn multi_cursor_word_forward_identical_lines() {
    // Reproduce user-reported bug: cursors at same column on identical lines
    // should move to the same column after word forward motion
    let mut doc = doc_from_str("123 4\n123 4\n");
    doc.cursors[0] = 0; // line 0, col 0
    doc.cursors.push(6); // line 1, col 0
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();
    assert_eq!(doc.cursor_count(), 2);

    doc.move_word_forward();

    // Both should land on '4' (col 4 of their respective lines)
    let col_a = doc.cursors[0] - doc.rope.line_to_char(0); // col on line 0
    let col_b = doc.cursors[1] - doc.rope.line_to_char(1); // col on line 1
    assert_eq!(col_a, col_b, "Cursors should move to same column");
    assert_eq!(col_a, 4, "Should land on '4' at column 4");
}

#[test]
fn clear_anchor_adjusts_all_cursors_forward() {
    let mut doc = doc_from_str("123 4\n123 4\n");
    doc.cursors[0] = 0;
    doc.cursors.push(6);
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    // Simulate the normal-mode word-forward flow: set_anchor then move
    doc.set_anchor();
    doc.move_word_forward();

    // Before clear_anchor: raw positions one past display position
    assert_eq!(doc.cursors[0], 4);
    assert_eq!(doc.cursors[1], 10);
    assert!(doc.has_selection());

    doc.clear_anchor();

    // After clear_anchor: all cursors adjusted back by 1
    let col_a = doc.cursors[0] - doc.rope.line_to_char(0);
    let col_b = doc.cursors[1] - doc.rope.line_to_char(1);
    assert_eq!(col_a, 3, "Primary should commit to display column");
    assert_eq!(col_b, 3, "Secondary should also be adjusted");
    assert_eq!(col_a, col_b, "Both cursors at same column after clear");
    assert!(!doc.has_selection());
}

#[test]
fn clear_anchor_no_adjust_for_backward_selection() {
    let mut doc = doc_from_str("123 4\n123 4\n");
    doc.cursors[0] = 4; // on '4' line 0
    doc.cursors.push(10); // on '4' line 1
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.set_anchor();
    doc.move_word_backward();

    let pos0 = doc.cursors[0];
    let pos1 = doc.cursors[1];

    doc.clear_anchor();

    // Backward selection: no adjustment
    assert_eq!(doc.cursors[0], pos0);
    assert_eq!(doc.cursors[1], pos1);
}

#[test]
fn dedent_after_select_line_multibyte_no_panic() {
    let mut doc = doc_from_str("    alpha\n    café bravo\n    charlie\n");
    doc.move_down();
    doc.select_line();

    let (sel_start, sel_end) = doc.selection_range().unwrap();
    let first_line = doc.rope.char_to_line(sel_start);
    let last_line = doc
        .rope
        .char_to_line(if sel_end > 0 { sel_end - 1 } else { 0 });
    let anchor = doc.selection_anchor().unwrap();
    let cursor = doc.cursors[0];
    // .min(last_line) is the fix — without it, cursor_line overflows per_line_removed
    let anchor_line = doc.rope.char_to_line(anchor).min(last_line);
    let cursor_line = doc.rope.char_to_line(cursor).min(last_line);

    let tab_width = 4usize;
    let mut per_line_removed = Vec::new();
    for line in first_line..=last_line {
        let line_text = doc.rope.line(line).to_string();
        let leading = line_text.chars().take_while(|c| *c == ' ').count();
        per_line_removed.push(leading.min(tab_width));
    }

    doc.begin_transaction();
    for line in (first_line..=last_line).rev() {
        let remove_count = per_line_removed[line - first_line];
        if remove_count > 0 {
            let line_start = doc.rope.line_to_char(line);
            doc.delete_range(line_start, line_start + remove_count);
        }
    }
    let _anchor_shift: usize = per_line_removed[..=(anchor_line - first_line)].iter().sum();
    let cursor_shift: usize = per_line_removed[..=(cursor_line - first_line)].iter().sum();
    doc.cursors[0] = cursor.saturating_sub(cursor_shift);
    doc.commit_transaction();

    assert_eq!(doc.rope.to_string(), "    alpha\ncafé bravo\n    charlie\n");
}

// -------------------------------------------------------------------------
// Multi-cursor selection invariants
// -------------------------------------------------------------------------

#[test]
fn select_line_with_multiple_cursors_selects_each_cursors_line() {
    let mut doc = doc_from_str("alpha\nbeta\ngamma\n");
    doc.cursors[0] = 0; // line 0
    doc.cursors.push(6); // line 1
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.select_line();

    let ranges = doc.selection_ranges();
    assert_eq!(
        ranges.len(),
        2,
        "select_line must give every cursor its own selection"
    );
    assert!(ranges.contains(&(0, 6)), "line 0 selected: got {ranges:?}");
    assert!(ranges.contains(&(6, 11)), "line 1 selected: got {ranges:?}");
}

#[test]
fn set_anchor_with_multiple_cursors_sets_anchor_per_cursor() {
    let mut doc = doc_from_str("hello world\nfoo bar\n");
    doc.cursors[0] = 0; // before "hello"
    doc.cursors.push(12); // before "foo"
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.set_anchor();

    let anchors: Vec<usize> = doc
        .selections
        .iter()
        .map(|s| s.expect("every cursor has anchor").anchor)
        .collect();
    assert!(anchors.contains(&0), "primary anchor at 0: {anchors:?}");
    assert!(anchors.contains(&12), "secondary anchor at 12: {anchors:?}");
}

#[test]
fn extend_word_forward_per_cursor_anchors_independently() {
    let mut doc = doc_from_str("hello world\nfoo bar\n");
    doc.cursors[0] = 0;
    doc.cursors.push(12);
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.set_anchor();
    doc.move_word_forward();

    // Both selections extend from their own anchor through the first word.
    let anchors: Vec<usize> = doc.selections.iter().map(|s| s.unwrap().anchor).collect();
    assert!(anchors.contains(&0));
    assert!(anchors.contains(&12));
}

#[test]
fn clear_anchor_per_cursor_forward_adjustment() {
    // Primary has a forward selection; secondary has a backward selection.
    // clear_anchor must only adjust the forward one back by 1.
    let mut doc = doc_from_str("abcdef\nuvwxyz\n");
    doc.cursors[0] = 0;
    doc.cursors.push(7);
    doc.selections.push(None);
    doc.sort_and_dedup_cursors();

    doc.selections[0] = Some(Selection::tail_on_forward(0, 3)); // forward
    doc.cursors[0] = 3;
    doc.selections[1] = Some(Selection::tail_on_forward(12, 7)); // backward (anchor > head)
    doc.cursors[1] = 7;

    doc.clear_anchor();

    // Forward selection: cursor stepped back by one (3 -> 2).
    // Backward selection: cursor unchanged (7).
    assert!(
        doc.cursors.contains(&2),
        "forward steps back: {:?}",
        doc.cursors
    );
    assert!(
        doc.cursors.contains(&7),
        "backward unchanged: {:?}",
        doc.cursors
    );
    assert!(!doc.has_selection());
}

#[test]
fn add_cursor_below_pushes_none_to_selections() {
    let mut doc = doc_from_str("line1\nline2\n");
    doc.cursors[0] = 0;
    doc.set_anchor();
    assert!(doc.add_cursor_below());

    assert_eq!(doc.cursors.len(), 2);
    assert_eq!(doc.selections.len(), 2, "selections grew with cursors");
    // The newly added cursor starts with no selection.
    assert!(doc.selections.iter().any(|s| s.is_none()));
}

#[test]
fn remove_secondary_cursors_truncates_selections() {
    let mut doc = doc_from_str("alpha\nbeta\ngamma\n");
    doc.cursors[0] = 0;
    doc.add_cursor_below();
    doc.add_cursor_below();
    doc.set_anchor();
    assert_eq!(doc.cursors.len(), doc.selections.len());

    doc.remove_secondary_cursors();
    assert_eq!(doc.cursors.len(), 1);
    assert_eq!(doc.selections.len(), 1);
}

#[test]
fn sort_and_dedup_cursors_reorders_selections_in_parallel() {
    let mut doc = doc_from_str("abcdef\n");
    doc.cursors = vec![4, 1, 2];
    doc.selections = vec![
        Some(Selection::tail_on_forward(4, 5)),
        Some(Selection::tail_on_forward(1, 2)),
        Some(Selection::tail_on_forward(2, 3)),
    ];

    doc.sort_and_dedup_cursors();

    // Primary (originally at index 0, cursor=4) stays at index 0.
    assert_eq!(doc.cursors[0], 4);
    assert_eq!(doc.selections[0], Some(Selection::tail_on_forward(4, 5)));
    // The remaining cursors are sorted; their selections track them.
    assert_eq!(doc.cursors.len(), 3);
    for (cursor, sel) in doc.cursors.iter().zip(doc.selections.iter()) {
        let s = sel.expect("every cursor still has its selection");
        assert_eq!(*cursor, s.anchor, "selection paired with right cursor");
    }
}

#[test]
fn selection_text_combined_concatenates_word_selections() {
    let mut doc = doc_from_str("foo bar baz\n");
    doc.cursors = vec![0];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 3))];
    doc.cursors.push(8);
    doc.selections.push(Some(Selection::tail_on_forward(8, 11)));

    // Concat with no separator (each segment carries its own trailing chars).
    assert_eq!(doc.selection_text_combined(), Some("foobaz".to_string()));
}

#[test]
fn selection_text_combined_concatenates_line_selections() {
    let mut doc = doc_from_str("## a\nsome\n## b\none\n");
    // Select line 0 ("## a\n") and line 2 ("## b\n").
    doc.cursors = vec![5];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 5))];
    doc.cursors.push(15);
    doc.selections
        .push(Some(Selection::tail_on_forward(10, 15)));

    assert_eq!(
        doc.selection_text_combined(),
        Some("## a\n## b\n".to_string())
    );
}

#[test]
fn merged_selection_ranges_merges_overlap() {
    let mut doc = doc_from_str("abcdefghij\n");
    doc.cursors = vec![5];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 5))];
    doc.cursors.push(7);
    doc.selections.push(Some(Selection::tail_on_forward(3, 7)));

    assert_eq!(doc.merged_selection_ranges(), vec![(0, 7)]);
}

#[test]
fn merged_selection_ranges_merges_touching() {
    let mut doc = doc_from_str("abcdefghij\n");
    doc.cursors = vec![5];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 5))];
    doc.cursors.push(10);
    doc.selections.push(Some(Selection::tail_on_forward(5, 10)));

    assert_eq!(doc.merged_selection_ranges(), vec![(0, 10)]);
}

#[test]
fn merged_selection_ranges_keeps_disjoint() {
    let mut doc = doc_from_str("abcdefghij\n");
    doc.cursors = vec![3];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 3))];
    doc.cursors.push(9);
    doc.selections.push(Some(Selection::tail_on_forward(6, 9)));

    assert_eq!(doc.merged_selection_ranges(), vec![(0, 3), (6, 9)]);
}

#[test]
fn merged_selection_ranges_preserves_per_cursor_anchors() {
    let mut doc = doc_from_str("abcdefghij\n");
    doc.cursors = vec![5];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 5))];
    doc.cursors.push(7);
    doc.selections.push(Some(Selection::tail_on_forward(3, 7)));

    // Merge view collapses the two ranges.
    assert_eq!(doc.merged_selection_ranges(), vec![(0, 7)]);
    // Underlying anchors are untouched, so a later motion that retracts a
    // head can dissolve the union back into the original two ranges.
    assert_eq!(doc.selections[0].unwrap().anchor, 0);
    assert_eq!(doc.selections[1].unwrap().anchor, 3);
}

#[test]
fn selection_text_combined_overlap_yields_union_text_once() {
    let mut doc = doc_from_str("abcdefghij\n");
    doc.cursors = vec![5];
    doc.selections = vec![Some(Selection::tail_on_forward(0, 5))];
    doc.cursors.push(7);
    doc.selections.push(Some(Selection::tail_on_forward(3, 7)));

    assert_eq!(doc.selection_text_combined(), Some("abcdefg".to_string()));
}

/// End-to-end scenario from the user's example: two cursors on `##` markers,
/// `x` thrice grows both line-selections until the document is fully covered.
/// `y` (selection_text_combined) returns the whole doc with no duplication.
/// Then moving the cursors back up preserves the per-cursor anchors so the
/// merged view dissolves naturally.
#[test]
fn user_scenario_select_line_grows_into_unified_selection() {
    let mut doc = doc_from_str("## a\nsome\n## b\none\n");
    // Cursor 0 on first `##`, cursor 1 on second `##`.
    doc.cursors = vec![0];
    doc.add_cursor_at(10);

    doc.select_line(); // x #1: each cursor selects its own line.
    doc.extend_line_selection_down(); // x #2: extend each by one more line.
    doc.extend_line_selection_down(); // x #3: extend again — ranges now overlap.

    let combined = doc.selection_text_combined().expect("merged text");
    assert_eq!(combined, "## a\nsome\n## b\none\n");

    // Anchors of each cursor are still the original line starts so a later
    // upward retract restores the per-cursor view.
    let primary_idx = doc.cursors.iter().position(|&c| c < 10).unwrap_or(0);
    let secondary_idx = 1 - primary_idx;
    let primary = doc.selections[primary_idx].unwrap();
    let secondary = doc.selections[secondary_idx].unwrap();
    let anchors = [primary.anchor, secondary.anchor];
    assert!(anchors.contains(&0));
    assert!(anchors.contains(&10));
}

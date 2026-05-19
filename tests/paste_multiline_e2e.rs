use std::fs;

use gargo::app::App;
use gargo::config::Config;
use gargo::core::editor::Editor;
use gargo::core::mode::Mode;
use gargo::input::action::{Action, CoreAction};
use tempfile::tempdir;

/// Open `file_path` (seeded with `contents`) inside an `App`, plugins disabled.
fn app_over_file(file_path: &std::path::Path, contents: &str) -> App {
    fs::write(file_path, contents).expect("seed file");
    let editor = Editor::open(file_path.to_str().expect("utf-8 path"));
    let mut config = Config::default();
    config.plugins.enabled.clear();
    App::new(editor, config, file_path.parent())
}

#[test]
fn test_paste_with_lf_in_insert_mode_creates_multiple_lines() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("paste_target.txt");
    fs::write(&file_path, "").expect("seed empty file");

    let mut editor = Editor::open(file_path.to_str().expect("utf-8 path"));

    // Match App::run paste behavior: Event::Paste(text) only inserts in Insert mode.
    editor.mode = Mode::Insert;
    editor.active_buffer_mut().insert_text("a\nb");

    editor.active_buffer_mut().save().expect("save pasted text");

    let contents = fs::read_to_string(&file_path).expect("read saved file");
    assert_eq!(contents, "a\nb");
    assert!(contents.contains('\n'));
    assert_ne!(contents, "ab");
}

#[test]
fn test_paste_with_crlf_in_insert_mode_normalizes_to_lf() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("paste_target_crlf.txt");
    fs::write(&file_path, "").expect("seed empty file");

    let mut editor = Editor::open(file_path.to_str().expect("utf-8 path"));
    editor.mode = Mode::Insert;
    editor
        .active_buffer_mut()
        .insert_text("flowchart TB\r\n    Start[hourly_flow start]");
    editor.active_buffer_mut().save().expect("save pasted text");

    let contents = fs::read_to_string(&file_path).expect("read saved file");
    assert_eq!(contents, "flowchart TB\n    Start[hourly_flow start]");
    assert!(contents.contains('\n'));
    assert!(!contents.contains('\r'));
}

#[test]
fn test_paste_with_bare_cr_in_insert_mode_normalizes_to_lf() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("paste_target_cr.txt");
    fs::write(&file_path, "").expect("seed empty file");

    let mut editor = Editor::open(file_path.to_str().expect("utf-8 path"));
    editor.mode = Mode::Insert;
    editor.active_buffer_mut().insert_text("a\rb");
    editor.active_buffer_mut().save().expect("save pasted text");

    let contents = fs::read_to_string(&file_path).expect("read saved file");
    assert_eq!(contents, "a\nb");
    assert!(contents.contains('\n'));
    assert!(!contents.contains('\r'));
}

// -------------------------------------------------------
// Multi-cursor paste: distribute lines across cursors
// -------------------------------------------------------

/// Open a file, add a cursor on the line below, then paste two-line
/// register content: each line lands at its matching cursor.
#[test]
fn test_multi_cursor_paste_distributes_lines_to_cursors() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("distribute.txt");
    let mut app = app_over_file(&file_path, "aaa\nbbb\nccc\n");

    // Cursor opens at the top of the file; add a second one on the line below.
    app.editor_mut().active_buffer_mut().cursors[0] = 0;
    app.dispatch_action(Action::Core(CoreAction::AddCursorBelow));
    assert_eq!(app.editor().active_buffer().cursor_count(), 2);

    // Two cursors, two lines in the register -> one line distributed each.
    app.editor_mut().register = Some("X\nY".to_string());
    app.dispatch_action(Action::Core(CoreAction::Paste));

    assert_eq!(
        app.editor().active_buffer().rope.to_string(),
        "Xaaa\nYbbb\nccc\n"
    );
}

/// When the register's line count does not match the cursor count, the
/// whole content is pasted at every cursor instead of being distributed.
#[test]
fn test_multi_cursor_paste_falls_back_when_line_count_mismatch() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("fallback.txt");
    let mut app = app_over_file(&file_path, "aaa\nbbb\nccc\n");

    app.editor_mut().active_buffer_mut().cursors[0] = 0;
    app.dispatch_action(Action::Core(CoreAction::AddCursorBelow));
    assert_eq!(app.editor().active_buffer().cursor_count(), 2);

    // Three lines but only two cursors -> paste whole content at each.
    app.editor_mut().register = Some("P\nQ\nR".to_string());
    app.dispatch_action(Action::Core(CoreAction::Paste));

    assert_eq!(
        app.editor().active_buffer().rope.to_string(),
        "P\nQ\nRaaa\nP\nQ\nRbbb\nccc\n"
    );
}

/// Full round-trip: a multi-cursor visual yank stores each cursor's
/// selection on its own line, and pasting it back with the same number
/// of cursors distributes the lines.
#[test]
fn test_multi_cursor_yank_then_paste_round_trip() {
    let temp = tempdir().expect("create temp dir");
    let file_path = temp.path().join("roundtrip.txt");
    let mut app = app_over_file(&file_path, "foo\nbar\n");

    // Two cursors at the start of each line.
    app.editor_mut().active_buffer_mut().cursors[0] = 0;
    app.dispatch_action(Action::Core(CoreAction::AddCursorBelow));
    assert_eq!(app.editor().active_buffer().cursor_count(), 2);

    // Visually select the word under each cursor ("foo" and "bar").
    app.dispatch_action(Action::Core(CoreAction::ChangeMode(Mode::Visual)));
    for _ in 0..3 {
        app.dispatch_action(Action::Core(CoreAction::MoveRight));
    }
    app.dispatch_action(Action::Core(CoreAction::YankSelection));

    // Multi-cursor copy: each cursor's selection is stored on its own line.
    assert_eq!(app.editor().register.as_deref(), Some("foo\nbar"));

    // Paste it back at the start of each line -> distribution kicks in.
    app.editor_mut().active_buffer_mut().cursors[0] = 0;
    app.editor_mut().active_buffer_mut().cursors[1] = 4;
    app.dispatch_action(Action::Core(CoreAction::Paste));

    assert_eq!(
        app.editor().active_buffer().rope.to_string(),
        "foofoo\nbarbar\n"
    );
}

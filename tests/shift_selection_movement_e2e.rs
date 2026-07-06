use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gargo::app::App;
use gargo::config::Config;
use gargo::core::editor::Editor;
use gargo::core::mode::Mode;
use gargo::input::action::{Action, AppAction, BufferAction, CoreAction};
use gargo::input::chord::KeyState;
use gargo::input::keymap::resolve;
use tempfile::{TempDir, tempdir};

fn test_config() -> Config {
    let mut config = Config::default();
    config.plugins.enabled.clear();
    config
}

fn app_with_text(filename: &str, text: &str) -> (TempDir, PathBuf, App) {
    let tmp = tempdir().expect("create temp dir");
    let file = tmp.path().join(filename);
    fs::write(&file, text).expect("seed file");
    let editor = Editor::open(file.to_str().expect("utf-8 path"));
    let app = App::new(editor, test_config(), Some(Path::new(".")));
    (tmp, file, app)
}

fn shift_arrow(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

fn arrow(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl_shift_arrow(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

fn dispatch_key(app: &mut App, state: &mut KeyState, mode: Mode, key: KeyEvent) {
    let action = resolve(key, state, &mode, false);
    app.dispatch_action(action);
}

#[test]
fn normal_shift_right_selects_char_and_delete_selection_removes_it() {
    let (_tmp, file, mut app) = app_with_text("normal_shift_char.txt", "abcd");
    let mut state = KeyState::Normal;

    dispatch_key(
        &mut app,
        &mut state,
        Mode::Normal,
        shift_arrow(KeyCode::Right),
    );
    app.dispatch_action(Action::Core(CoreAction::DeleteSelection));
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "bcd");
}

#[test]
fn normal_ctrl_shift_right_selects_word_and_delete_selection_removes_it() {
    let (_tmp, file, mut app) = app_with_text("normal_ctrl_shift_word.txt", "hello world");
    let mut state = KeyState::Normal;

    dispatch_key(
        &mut app,
        &mut state,
        Mode::Normal,
        ctrl_shift_arrow(KeyCode::Right),
    );
    app.dispatch_action(Action::Core(CoreAction::DeleteSelection));
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "world");
}

#[test]
fn visual_ctrl_shift_right_extends_word_and_delete_selection_removes_it() {
    let (_tmp, file, mut app) = app_with_text("visual_ctrl_shift_word.txt", "hello world");
    let mut state = KeyState::Normal;

    app.dispatch_action(Action::Core(CoreAction::ChangeMode(Mode::Visual)));
    dispatch_key(
        &mut app,
        &mut state,
        Mode::Visual,
        ctrl_shift_arrow(KeyCode::Right),
    );
    app.dispatch_action(Action::Core(CoreAction::DeleteSelection));
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "world");
}

#[test]
fn insert_shift_left_selects_then_backspace_deletes_whole_selection() {
    let (_tmp, file, mut app) = app_with_text("insert_shift_backspace.txt", "abcdef");
    let mut state = KeyState::Normal;

    // Enter Insert mode and move the cursor to end of line.
    app.dispatch_action(Action::Core(CoreAction::ChangeMode(Mode::Insert)));
    app.dispatch_action(Action::Core(CoreAction::MoveToLineEnd));

    // Shift+Left three times selects "def".
    for _ in 0..3 {
        dispatch_key(
            &mut app,
            &mut state,
            Mode::Insert,
            shift_arrow(KeyCode::Left),
        );
    }

    // Backspace deletes the whole selection, not just one char.
    dispatch_key(
        &mut app,
        &mut state,
        Mode::Insert,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "abc");
}

#[test]
fn normal_shift_right_then_right_moves_from_new_cursor_edge() {
    let (_tmp, file, mut app) = app_with_text("normal_shift_cursor_edge.txt", "abcd");
    let mut state = KeyState::Normal;

    dispatch_key(
        &mut app,
        &mut state,
        Mode::Normal,
        shift_arrow(KeyCode::Right),
    );
    dispatch_key(&mut app, &mut state, Mode::Normal, arrow(KeyCode::Right));
    app.dispatch_action(Action::Core(CoreAction::ChangeMode(Mode::Insert)));
    app.dispatch_action(Action::Core(CoreAction::InsertChar('X')));
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "abXcd");
}

#[test]
fn normal_ctrl_shift_right_then_right_moves_from_word_edge() {
    let (_tmp, file, mut app) = app_with_text("normal_ctrl_shift_cursor_edge.txt", "hello world");
    let mut state = KeyState::Normal;

    dispatch_key(
        &mut app,
        &mut state,
        Mode::Normal,
        ctrl_shift_arrow(KeyCode::Right),
    );
    dispatch_key(&mut app, &mut state, Mode::Normal, arrow(KeyCode::Right));
    app.dispatch_action(Action::Core(CoreAction::ChangeMode(Mode::Insert)));
    app.dispatch_action(Action::Core(CoreAction::InsertChar('X')));
    app.dispatch_action(Action::App(AppAction::Buffer(BufferAction::Save)));

    let saved = fs::read_to_string(&file).expect("read saved file");
    assert_eq!(saved, "hello wXorld");
}

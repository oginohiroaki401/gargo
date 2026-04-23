use super::*;
use crate::command::registry::CommandRegistry;
use crate::config::Config;
use crate::input::action::{Action, AppAction, BufferAction};
use crate::input::chord::KeyState;
use crate::syntax::language::LanguageRegistry;
use crate::ui::framework::component::EventResult;
use crate::ui::overlays::explorer::popup::ExplorerPopup;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind) -> MouseEvent {
    mouse_at(kind, 0, 0)
}

fn mouse_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn setup_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kaguya_test_comp_{}", name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(dir.join("aaa_dir")).unwrap();
    fs::write(dir.join("bbb.txt"), "bbb").unwrap();
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

/// After popup opens a file and is closed, keys must fall through
/// to EventResult::Ignored so the keymap can process them.
#[test]
fn keys_fall_through_after_popup_closed() {
    let dir = setup_dir("close");
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let key_state = KeyState::Normal;

    let mut comp = Compositor::new();
    comp.open_explorer_popup(ExplorerPopup::new(dir.clone(), &HashMap::new()));

    // Navigate to bbb.txt (index 1 — past the aaa_dir)
    let r = comp.handle_key(
        key(KeyCode::Char('j')),
        &registry,
        &lang_registry,
        &config,
        &key_state,
    );
    assert!(matches!(r, EventResult::Consumed));

    // Press Enter → file should produce an OpenFileFromExplorerPopup
    let r = comp.handle_key(
        key(KeyCode::Enter),
        &registry,
        &lang_registry,
        &config,
        &key_state,
    );
    match r {
        EventResult::Action(Action::App(AppAction::Buffer(
            BufferAction::OpenFileFromExplorerPopup(ref path),
        ))) => {
            assert!(path.ends_with("bbb.txt"));
        }
        _ => panic!("Expected OpenFileFromExplorerPopup from popup, got something else"),
    }

    // Simulate what app.rs dispatch does
    comp.close_explorer_popup();
    assert!(!comp.has_explorer_popup());

    // Subsequent key must NOT be consumed — it should reach the keymap
    let r = comp.handle_key(
        key(KeyCode::Char('j')),
        &registry,
        &lang_registry,
        &config,
        &key_state,
    );
    assert!(
        matches!(r, EventResult::Ignored),
        "After popup closed, key should be Ignored (pass to keymap)"
    );

    cleanup(&dir);
}

/// While popup is active, ALL keys should be intercepted (Consumed or Action).
#[test]
fn popup_intercepts_all_keys() {
    let dir = setup_dir("intercept");
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let key_state = KeyState::Normal;

    let mut comp = Compositor::new();
    comp.open_explorer_popup(ExplorerPopup::new(dir.clone(), &HashMap::new()));

    // Random keys should all be consumed or produce actions, never Ignored
    for code in [
        KeyCode::Char('x'),
        KeyCode::Char('q'),
        KeyCode::Tab,
        KeyCode::F(5),
    ] {
        let r = comp.handle_key(key(code), &registry, &lang_registry, &config, &key_state);
        assert!(
            !matches!(r, EventResult::Ignored),
            "Popup should intercept {:?}",
            code,
        );
    }

    cleanup(&dir);
}

#[test]
fn mouse_scroll_ignored_without_overlay() {
    let mut comp = Compositor::new();
    let result = comp.handle_mouse(&mouse(MouseEventKind::ScrollDown));
    assert!(matches!(result, EventResult::Ignored));
}

#[test]
fn mouse_scroll_consumed_by_issue_overlay() {
    let mut comp = Compositor::new();
    comp.open_issue_list_picker(
        crate::ui::overlays::github::issue_picker::IssueListPicker::new(vec![]),
    );
    let result = comp.handle_mouse(&mouse(MouseEventKind::ScrollDown));
    assert!(matches!(result, EventResult::Consumed));
}

#[test]
fn mouse_drag_vertical_divider_resizes_windows() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);

    let before = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout before");
    let divider = before
        .dividers
        .iter()
        .find(|divider| divider.orientation == DividerOrientation::Vertical)
        .copied()
        .expect("vertical divider");
    let (primary_window, _) =
        Compositor::mouse_windows_for_divider(&before, divider, divider.x as u16, divider.y as u16)
            .expect("divider windows");
    let anchor_width_before = before
        .panes
        .iter()
        .find(|pane| pane.window_id == primary_window)
        .expect("anchor pane before")
        .rect
        .width;

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Consumed));
    let drag = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        divider.x.saturating_add(3) as u16,
        divider.y as u16,
    ));
    assert!(matches!(drag, EventResult::Consumed));
    let up = comp.handle_mouse(&mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        divider.x.saturating_add(3) as u16,
        divider.y as u16,
    ));
    assert!(matches!(up, EventResult::Consumed));

    let after = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout after");
    let anchor_width_after = after
        .panes
        .iter()
        .find(|pane| pane.window_id == primary_window)
        .expect("anchor pane after")
        .rect
        .width;
    assert!(anchor_width_after > anchor_width_before);
}

#[test]
fn mouse_drag_horizontal_divider_resizes_windows() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Horizontal, 2);

    let before = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout before");
    let divider = before
        .dividers
        .iter()
        .find(|divider| divider.orientation == DividerOrientation::Horizontal)
        .copied()
        .expect("horizontal divider");
    let (primary_window, _) =
        Compositor::mouse_windows_for_divider(&before, divider, divider.x as u16, divider.y as u16)
            .expect("divider windows");
    let anchor_height_before = before
        .panes
        .iter()
        .find(|pane| pane.window_id == primary_window)
        .expect("anchor pane before")
        .rect
        .height;

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Consumed));
    let drag = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        divider.x as u16,
        divider.y.saturating_add(3) as u16,
    ));
    assert!(matches!(drag, EventResult::Consumed));
    let up = comp.handle_mouse(&mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        divider.x as u16,
        divider.y.saturating_add(3) as u16,
    ));
    assert!(matches!(up, EventResult::Consumed));

    let after = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout after");
    let anchor_height_after = after
        .panes
        .iter()
        .find(|pane| pane.window_id == primary_window)
        .expect("anchor pane after")
        .rect
        .height;
    assert!(anchor_height_after > anchor_height_before);
}

#[test]
fn mouse_drag_outer_divider_resizes_outer_split_with_nested_vertical_tree() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);
    comp.window_manager
        .focus_direction(
            Direction::Left,
            PaneRect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
        )
        .expect("focus left window");
    comp.window_manager.split_focused(SplitAxis::Vertical, 3);

    let before = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout before");
    let outer_divider = before
        .dividers
        .iter()
        .filter(|divider| divider.orientation == DividerOrientation::Vertical)
        .max_by_key(|divider| divider.x)
        .copied()
        .expect("outer vertical divider");

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        outer_divider.x as u16,
        outer_divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Consumed));
    let drag = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        outer_divider.x.saturating_add(4) as u16,
        outer_divider.y as u16,
    ));
    assert!(matches!(drag, EventResult::Consumed));
    let up = comp.handle_mouse(&mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        outer_divider.x.saturating_add(4) as u16,
        outer_divider.y as u16,
    ));
    assert!(matches!(up, EventResult::Consumed));

    let after = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout after");
    let outer_after = after
        .dividers
        .iter()
        .filter(|divider| divider.orientation == DividerOrientation::Vertical)
        .max_by_key(|divider| divider.x)
        .copied()
        .expect("outer vertical divider after");
    assert!(outer_after.x > outer_divider.x);
}

#[test]
fn mouse_drag_keeps_state_after_hitting_resize_limit() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);
    comp.current = Surface::new(200, 24);

    let layout = comp
        .window_layout_for_event_dims(200, 24)
        .expect("layout before");
    let divider = layout
        .dividers
        .iter()
        .find(|divider| divider.orientation == DividerOrientation::Vertical)
        .copied()
        .expect("vertical divider");

    let first_drag_col = divider.x.saturating_add(60).min(190) as u16;
    let second_drag_col = first_drag_col.saturating_add(5);
    assert!(second_drag_col > first_drag_col);

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Consumed));

    let drag_first = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        first_drag_col,
        divider.y as u16,
    ));
    assert!(matches!(drag_first, EventResult::Consumed));

    // Push again in the same direction; this can be a clamp/no-op error.
    let drag_at_limit = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        second_drag_col,
        divider.y as u16,
    ));
    assert!(matches!(drag_at_limit, EventResult::Consumed));

    // Reverse without releasing. Regression: this was ignored because drag
    // state was dropped on the previous no-op/error resize attempt.
    let drag_reverse = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        second_drag_col.saturating_sub(1),
        divider.y as u16,
    ));
    assert!(matches!(drag_reverse, EventResult::Consumed));

    let up = comp.handle_mouse(&mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        second_drag_col.saturating_sub(1),
        divider.y as u16,
    ));
    assert!(matches!(up, EventResult::Consumed));
}

#[test]
fn mouse_drag_non_divider_does_not_resize_windows() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);

    let before = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout before");
    let left = before
        .panes
        .iter()
        .find(|pane| pane.rect.x == 0)
        .expect("left pane");
    let start_col = left.rect.x as u16;
    let start_row = left.rect.y as u16;

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        start_col,
        start_row,
    ));
    // Pane clicks now emit a BufferClick action — but never a divider drag.
    assert!(matches!(
        down,
        EventResult::Action(crate::input::action::Action::BufferClick { .. })
    ));

    let drag = comp.handle_mouse(&mouse_at(
        MouseEventKind::Drag(MouseButton::Left),
        start_col.saturating_add(4),
        start_row,
    ));
    assert!(matches!(drag, EventResult::Ignored));

    let after = comp
        .window_layout_for_event_dims(80, 24)
        .expect("layout after");
    assert_eq!(after.panes, before.panes);
    assert_eq!(after.dividers, before.dividers);
}

#[test]
fn mouse_drag_is_blocked_when_modal_overlay_active() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);
    comp.open_search_bar(0, 0, 0);

    let layout = comp.window_layout_for_event_dims(80, 24).expect("layout");
    let divider = layout
        .dividers
        .iter()
        .find(|divider| divider.orientation == DividerOrientation::Vertical)
        .copied()
        .expect("vertical divider");

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Ignored));
    assert!(comp.mouse_drag.is_none());
}

#[test]
fn mouse_up_clears_divider_drag_state() {
    let mut comp = Compositor::new();
    comp.window_manager.split_focused(SplitAxis::Vertical, 2);
    let layout = comp.window_layout_for_event_dims(80, 24).expect("layout");
    let divider = layout
        .dividers
        .iter()
        .find(|divider| divider.orientation == DividerOrientation::Vertical)
        .copied()
        .expect("vertical divider");

    let down = comp.handle_mouse(&mouse_at(
        MouseEventKind::Down(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(down, EventResult::Consumed));
    assert!(comp.mouse_drag.is_some());

    let up = comp.handle_mouse(&mouse_at(
        MouseEventKind::Up(MouseButton::Left),
        divider.x as u16,
        divider.y as u16,
    ));
    assert!(matches!(up, EventResult::Consumed));
    assert!(comp.mouse_drag.is_none());
}

#[test]
fn mouse_divider_window_pair_uses_clicked_divider_segment() {
    let layout = Layout {
        panes: vec![
            crate::ui::framework::window_manager::PaneLayout {
                window_id: 1,
                buffer_id: 1,
                rect: PaneRect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 5,
                },
            },
            crate::ui::framework::window_manager::PaneLayout {
                window_id: 2,
                buffer_id: 2,
                rect: PaneRect {
                    x: 11,
                    y: 0,
                    width: 9,
                    height: 5,
                },
            },
            crate::ui::framework::window_manager::PaneLayout {
                window_id: 3,
                buffer_id: 3,
                rect: PaneRect {
                    x: 0,
                    y: 6,
                    width: 10,
                    height: 5,
                },
            },
            crate::ui::framework::window_manager::PaneLayout {
                window_id: 4,
                buffer_id: 4,
                rect: PaneRect {
                    x: 11,
                    y: 6,
                    width: 9,
                    height: 5,
                },
            },
        ],
        dividers: vec![
            Divider {
                orientation: DividerOrientation::Vertical,
                x: 10,
                y: 0,
                len: 5,
            },
            Divider {
                orientation: DividerOrientation::Vertical,
                x: 10,
                y: 6,
                len: 5,
            },
        ],
    };

    assert_eq!(
        Compositor::mouse_windows_for_divider(&layout, layout.dividers[0], 10, 1),
        Some((1, 2))
    );
    assert_eq!(
        Compositor::mouse_windows_for_divider(&layout, layout.dividers[1], 10, 7),
        Some((3, 4))
    );
}

#[test]
fn search_bar_insert_text_japanese() {
    // Test that Japanese text (from IME paste events) is correctly inserted
    let mut bar = SearchBar {
        input: TextInput::default(),
        saved_cursor: 0,
        saved_scroll: 0,
        saved_horizontal_scroll: 0,
    };

    // Insert Japanese text (simulating IME composition result)
    bar.insert_text("\u{30BF}\u{30FC}\u{30DF}\u{30CA}\u{30EB}");
    assert_eq!(bar.input.text, "\u{30BF}\u{30FC}\u{30DF}\u{30CA}\u{30EB}");
    assert_eq!(bar.input.cursor, 5); // 5 characters

    // Insert more text at the end
    bar.insert_text("\u{30C6}\u{30B9}\u{30C8}");
    assert_eq!(
        bar.input.text,
        "\u{30BF}\u{30FC}\u{30DF}\u{30CA}\u{30EB}\u{30C6}\u{30B9}\u{30C8}"
    );
    assert_eq!(bar.input.cursor, 8);

    // Insert at middle position
    bar.input.cursor = 5;
    bar.insert_text("\u{306E}");
    assert_eq!(
        bar.input.text,
        "\u{30BF}\u{30FC}\u{30DF}\u{30CA}\u{30EB}\u{306E}\u{30C6}\u{30B9}\u{30C8}"
    );
    assert_eq!(bar.input.cursor, 6);
}

#[test]
fn resize_emits_clear_screen() {
    use crate::config::Config;
    use crate::core::editor::Editor;
    use crate::input::chord::KeyState;
    use crate::syntax::theme::Theme;
    use crate::ui::framework::component::RenderContext;

    let editor = Editor::new();
    let config = Config::default();
    let theme = Theme::dark();
    let key_state = KeyState::Normal;

    let mut compositor = Compositor::new();

    // First render at 40x8
    let mut out1 = Vec::new();
    let ctx1 = RenderContext::new(
        40,
        8,
        &editor,
        &theme,
        &key_state,
        &config,
        std::path::Path::new("/tmp/gargo-test-root"),
        false,
        false,
    );
    compositor.render(&ctx1, &mut out1).expect("render frame 1");

    // Second render at 30x6 (simulating resize)
    let mut out2 = Vec::new();
    let ctx2 = RenderContext::new(
        30,
        6,
        &editor,
        &theme,
        &key_state,
        &config,
        std::path::Path::new("/tmp/gargo-test-root"),
        false,
        false,
    );
    compositor.render(&ctx2, &mut out2).expect("render frame 2");

    // The second render must contain \x1b[2J (clear all)
    let out2_str = String::from_utf8_lossy(&out2);
    assert!(
        out2_str.contains("\x1b[2J"),
        "Resize render must emit clear-screen escape sequence (\\x1b[2J)",
    );
}

#[test]
fn same_size_render_does_not_emit_clear() {
    use crate::config::Config;
    use crate::core::editor::Editor;
    use crate::input::chord::KeyState;
    use crate::syntax::theme::Theme;
    use crate::ui::framework::component::RenderContext;

    let editor = Editor::new();
    let config = Config::default();
    let theme = Theme::dark();
    let key_state = KeyState::Normal;

    let mut compositor = Compositor::new();

    // First render at 40x8
    let mut out1 = Vec::new();
    let ctx1 = RenderContext::new(
        40,
        8,
        &editor,
        &theme,
        &key_state,
        &config,
        std::path::Path::new("/tmp/gargo-test-root"),
        false,
        false,
    );
    compositor.render(&ctx1, &mut out1).expect("render frame 1");

    // Second render at same size 40x8
    let mut out2 = Vec::new();
    let ctx2 = RenderContext::new(
        40,
        8,
        &editor,
        &theme,
        &key_state,
        &config,
        std::path::Path::new("/tmp/gargo-test-root"),
        false,
        false,
    );
    compositor.render(&ctx2, &mut out2).expect("render frame 2");

    // No resize => no clear
    let out2_str = String::from_utf8_lossy(&out2);
    assert!(
        !out2_str.contains("\x1b[2J"),
        "Same-size render must NOT emit clear-screen escape sequence",
    );
}

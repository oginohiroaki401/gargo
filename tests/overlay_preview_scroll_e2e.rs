use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use gargo::command::registry::CommandRegistry;
use gargo::config::Config;
use gargo::core::editor::Editor;
use gargo::input::chord::KeyState;
use gargo::syntax::language::LanguageRegistry;
use gargo::syntax::theme::Theme;
use gargo::ui::framework::component::EventResult;
use gargo::ui::framework::component::RenderContext;
use gargo::ui::framework::compositor::Compositor;
use gargo::ui::overlays::github::issue_picker::{IssueCommentEntry, IssueEntry, IssueListPicker};
use gargo::ui::overlays::github::pr_picker::{PrEntry, PrListPicker};
use std::path::Path;

mod support;

fn render_bytes(compositor: &mut Compositor, editor: &Editor, cols: usize, rows: usize) -> Vec<u8> {
    let config = Config::default();
    let theme = Theme::dark();
    let key_state = KeyState::Normal;
    let ctx = RenderContext::new(
        cols,
        rows,
        editor,
        &theme,
        &key_state,
        &config,
        Path::new("."),
        false,
        false,
    );
    let mut out = Vec::new();
    compositor
        .render(&ctx, &mut out)
        .expect("render frame to memory");
    out
}

fn scroll_down_event() -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn shift_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

#[test]
fn issue_preview_mouse_scroll_reveals_later_lines() {
    let cols = 100;
    let rows = 20;
    let mut screen = vec![vec![' '; cols]; rows];
    let editor = Editor::new();
    let mut compositor = Compositor::new();

    let body: String = (0..30).map(|i| format!("ISSUE-LINE-{i:02}\n")).collect();
    let issue = IssueEntry {
        number: 77,
        title: "Mouse preview scroll".to_string(),
        body,
        url: "https://github.com/user/repo/issues/77".to_string(),
        state: "OPEN".to_string(),
        author: "alice".to_string(),
        created_at: "2026-02-01T10:00:00Z".to_string(),
        labels: vec![],
        comments: vec![IssueCommentEntry {
            author: "bob".to_string(),
            body: "comment".to_string(),
            created_at: "2026-02-02T08:00:00Z".to_string(),
        }],
        comment_count: 1,
    };
    compositor.open_issue_list_picker(IssueListPicker::new(vec![issue]));

    let frame1 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_before =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame1, cols, rows);
    let text_before = rows_before.join("\n");
    assert!(!text_before.contains("ISSUE-LINE-12"));

    let mouse = scroll_down_event();
    assert!(matches!(
        compositor.handle_mouse(&mouse),
        gargo::ui::framework::component::EventResult::Consumed
    ));

    let frame2 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_after =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame2, cols, rows);
    let text_after = rows_after.join("\n");
    assert!(text_after.contains("ISSUE-LINE-12"));
}

#[test]
fn pr_preview_mouse_scroll_reveals_later_lines() {
    let cols = 100;
    let rows = 20;
    let mut screen = vec![vec![' '; cols]; rows];
    let editor = Editor::new();
    let mut compositor = Compositor::new();

    let body: String = (0..30).map(|i| format!("PR-LINE-{i:02}\n")).collect();
    let pr = PrEntry {
        number: 42,
        title: "Mouse preview scroll".to_string(),
        body,
        url: "https://github.com/user/repo/pull/42".to_string(),
        state: "OPEN".to_string(),
        author: "alice".to_string(),
        head_ref: "feature/mouse-scroll".to_string(),
        created_at: "2026-02-01T10:00:00Z".to_string(),
        labels: vec![],
    };
    compositor.open_pr_list_picker(PrListPicker::new(vec![pr]));

    let frame1 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_before =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame1, cols, rows);
    let text_before = rows_before.join("\n");
    assert!(!text_before.contains("PR-LINE-14"));

    let mouse = scroll_down_event();
    assert!(matches!(
        compositor.handle_mouse(&mouse),
        gargo::ui::framework::component::EventResult::Consumed
    ));

    let frame2 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_after =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame2, cols, rows);
    let text_after = rows_after.join("\n");
    assert!(text_after.contains("PR-LINE-14"));
}

#[test]
fn issue_preview_shift_l_reveals_hidden_right_side() {
    let cols = 100;
    let rows = 20;
    let mut screen = vec![vec![' '; cols]; rows];
    let editor = Editor::new();
    let mut compositor = Compositor::new();
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let key_state = KeyState::Normal;

    let long_line = format!("LEFT-{}RIGHT-MARKER", "x".repeat(60));
    let issue = IssueEntry {
        number: 88,
        title: "Keyboard horizontal preview scroll".to_string(),
        body: long_line,
        url: "https://github.com/user/repo/issues/88".to_string(),
        state: "OPEN".to_string(),
        author: "alice".to_string(),
        created_at: "2026-02-01T10:00:00Z".to_string(),
        labels: vec![],
        comments: vec![],
        comment_count: 0,
    };
    compositor.open_issue_list_picker(IssueListPicker::new(vec![issue]));

    let frame1 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_before =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame1, cols, rows);
    let text_before = rows_before.join("\n");
    assert!(!text_before.contains("RIGHT-MARKER"));

    for _ in 0..8 {
        let result = compositor.handle_key(
            shift_key(KeyCode::Char('L')),
            &registry,
            &lang_registry,
            &config,
            &key_state,
        );
        assert!(matches!(result, EventResult::Consumed));
    }

    let frame2 = render_bytes(&mut compositor, &editor, cols, rows);
    let rows_after =
        support::render_snapshot::apply_ansi_to_screen(&mut screen, &frame2, cols, rows);
    let text_after = rows_after.join("\n");
    assert!(text_after.contains("RIGHT-MARKER"));
}

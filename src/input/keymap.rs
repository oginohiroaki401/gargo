use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::core::mode::Mode;
use crate::input::action::{
    Action, AppAction, BufferAction, CoreAction, IntegrationAction, LifecycleAction,
    NavigationAction, WindowAction, WindowDirection, WindowSplitAxis, WorkspaceAction,
};
use crate::input::chord::KeyState;

fn core(action: CoreAction) -> Action {
    Action::Core(action)
}

fn app(action: AppAction) -> Action {
    Action::App(action)
}

fn ctrl_motion_action(code: KeyCode) -> Option<CoreAction> {
    match code {
        KeyCode::Char('f') => Some(CoreAction::MoveRight),
        KeyCode::Char('b') => Some(CoreAction::MoveLeft),
        KeyCode::Char('n') => Some(CoreAction::MoveDown),
        KeyCode::Char('p') => Some(CoreAction::MoveUp),
        KeyCode::Char('a') => Some(CoreAction::MoveToLineStart),
        KeyCode::Char('e') => Some(CoreAction::MoveToLineEnd),
        KeyCode::Left => Some(CoreAction::MoveWordBackward),
        KeyCode::Down => Some(CoreAction::MoveDown),
        KeyCode::Up => Some(CoreAction::MoveUp),
        _ => None,
    }
}

fn ctrl_word_arrow_no_select_action(code: KeyCode) -> Option<CoreAction> {
    match code {
        KeyCode::Left => Some(CoreAction::MoveWordBackwardNoSelect),
        KeyCode::Right => Some(CoreAction::MoveWordForwardNoSelect),
        _ => None,
    }
}

fn ctrl_shift_word_arrow_extend_action(code: KeyCode) -> Option<CoreAction> {
    match code {
        KeyCode::Left => Some(CoreAction::ExtendWordBackwardShift),
        KeyCode::Right => Some(CoreAction::ExtendWordForwardShift),
        _ => None,
    }
}

fn shift_char_arrow_extend_action(code: KeyCode) -> Option<CoreAction> {
    match code {
        KeyCode::Left => Some(CoreAction::ExtendLeft),
        KeyCode::Right => Some(CoreAction::ExtendRight),
        KeyCode::Up => Some(CoreAction::ExtendUp),
        KeyCode::Down => Some(CoreAction::ExtendDown),
        _ => None,
    }
}

fn ctrl_shift_line_boundary_extend_action(code: KeyCode) -> Option<CoreAction> {
    match code {
        KeyCode::Char('a') | KeyCode::Char('A') => Some(CoreAction::ExtendToLineStart),
        KeyCode::Char('e') | KeyCode::Char('E') => Some(CoreAction::ExtendToLineEnd),
        _ => None,
    }
}

fn window_direction_key(code: KeyCode) -> Option<WindowDirection> {
    match code {
        KeyCode::Char('h') | KeyCode::Left => Some(WindowDirection::Left),
        KeyCode::Char('j') | KeyCode::Down => Some(WindowDirection::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(WindowDirection::Up),
        KeyCode::Char('l') | KeyCode::Right => Some(WindowDirection::Right),
        _ => None,
    }
}

fn window_swap_direction_key(code: KeyCode, shift: bool) -> Option<WindowDirection> {
    if shift {
        return match code {
            KeyCode::Left => Some(WindowDirection::Left),
            KeyCode::Down => Some(WindowDirection::Down),
            KeyCode::Up => Some(WindowDirection::Up),
            KeyCode::Right => Some(WindowDirection::Right),
            _ => None,
        };
    }

    match code {
        KeyCode::Char('H') => Some(WindowDirection::Left),
        KeyCode::Char('J') => Some(WindowDirection::Down),
        KeyCode::Char('K') => Some(WindowDirection::Up),
        KeyCode::Char('L') => Some(WindowDirection::Right),
        _ => None,
    }
}

pub fn resolve(key: KeyEvent, state: &mut KeyState, mode: &Mode, is_recording: bool) -> Action {
    // MacroRecord chord: Q was pressed, now waiting for register a-z
    if *state == KeyState::MacroRecord {
        *state = KeyState::Normal;
        return match key.code {
            KeyCode::Char(c @ 'a'..='z') => core(CoreAction::MacroRecord(c)),
            _ => core(CoreAction::Noop),
        };
    }

    // MacroPlay chord: q was pressed, now waiting for register a-z or q
    if *state == KeyState::MacroPlay {
        *state = KeyState::Normal;
        return match key.code {
            KeyCode::Char(c @ 'a'..='z') => core(CoreAction::MacroPlay(c)),
            KeyCode::Char('@') => core(CoreAction::MacroPlayLast),
            _ => core(CoreAction::Noop),
        };
    }

    // CtrlX chord takes priority regardless of mode
    if *state == KeyState::CtrlX {
        *state = KeyState::Normal;
        return match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app(AppAction::Buffer(BufferAction::Save))
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app(AppAction::Lifecycle(LifecycleAction::Quit))
            }
            _ => app(AppAction::Lifecycle(LifecycleAction::Cancel)),
        };
    }

    // Goto chord (normal/visual modes)
    if *state == KeyState::Goto {
        *state = KeyState::Normal;
        return match key.code {
            KeyCode::Char('g') => core(CoreAction::MoveToFileStart),
            KeyCode::Char('d') => app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.goto_definition".to_string(),
                },
            )),
            KeyCode::Char('r') => app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.find_references".to_string(),
                },
            )),
            KeyCode::Char('e') => core(CoreAction::MoveToFileEnd),
            KeyCode::Char('h') => core(CoreAction::MoveToLineStart),
            KeyCode::Char('l') => core(CoreAction::MoveToLineEnd),
            KeyCode::Char('p') => core(CoreAction::PrevBuffer),
            KeyCode::Char('n') => core(CoreAction::NextBuffer),
            _ => core(CoreAction::Noop),
        };
    }

    // Space chord (normal/visual mode)
    if *state == KeyState::Space {
        *state = KeyState::Normal;
        return match key.code {
            KeyCode::Char('w') => {
                *state = KeyState::SpaceWindow;
                core(CoreAction::Noop)
            }
            KeyCode::Char('e') => app(AppAction::Workspace(WorkspaceAction::ToggleExplorer)),
            KeyCode::Char('E') => app(AppAction::Workspace(WorkspaceAction::OpenExplorerPopup)),
            KeyCode::Char('f') => app(AppAction::Workspace(WorkspaceAction::OpenFilePicker)),
            KeyCode::Char('b') => app(AppAction::Workspace(WorkspaceAction::OpenBufferPicker)),
            KeyCode::Char('j') => app(AppAction::Workspace(WorkspaceAction::OpenJumpListPicker)),
            KeyCode::Char('s') => app(AppAction::Workspace(WorkspaceAction::OpenSymbolPicker)),
            KeyCode::Char('p') => app(AppAction::Workspace(WorkspaceAction::OpenCommandPalette)),
            KeyCode::Char('/') => app(AppAction::Workspace(WorkspaceAction::OpenGlobalSearch)),
            KeyCode::Char('g') => app(AppAction::Workspace(
                WorkspaceAction::ToggleChangedFilesSidebar,
            )),
            KeyCode::Char('G') => app(AppAction::Workspace(WorkspaceAction::OpenGitView)),
            KeyCode::Char('l') => app(AppAction::Workspace(WorkspaceAction::OpenCommitLog)),
            KeyCode::Char('d') => app(AppAction::Workspace(
                WorkspaceAction::OpenBranchCompareSidebarPicker,
            )),
            KeyCode::Char('D') => app(AppAction::Workspace(
                WorkspaceAction::OpenBranchComparePicker,
            )),
            _ => core(CoreAction::Noop),
        };
    }

    if *state == KeyState::SpaceWindow {
        *state = KeyState::Normal;
        if let Some(direction) =
            window_swap_direction_key(key.code, key.modifiers.contains(KeyModifiers::SHIFT))
        {
            return app(AppAction::Window(WindowAction::WindowSwap(direction)));
        }
        if let Some(direction) = window_direction_key(key.code) {
            return app(AppAction::Window(WindowAction::WindowFocus(direction)));
        }
        return match key.code {
            KeyCode::Char('v') => app(AppAction::Window(WindowAction::WindowSplit(
                WindowSplitAxis::Vertical,
            ))),
            KeyCode::Char('s') => app(AppAction::Window(WindowAction::WindowSplit(
                WindowSplitAxis::Horizontal,
            ))),
            KeyCode::Char('w') => app(AppAction::Window(WindowAction::WindowFocusNext)),
            KeyCode::Char('q') => app(AppAction::Window(WindowAction::WindowCloseCurrent)),
            KeyCode::Char('o') => app(AppAction::Window(WindowAction::WindowCloseOthers)),
            _ => core(CoreAction::Noop),
        };
    }

    // Global bindings (all modes)
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('s') => return app(AppAction::Buffer(BufferAction::Save)),
            KeyCode::Char('q') => return app(AppAction::Buffer(BufferAction::CloseBuffer)),
            KeyCode::Char('o') => return app(AppAction::Navigation(NavigationAction::JumpOlder)),
            KeyCode::Char('i') => return app(AppAction::Navigation(NavigationAction::JumpNewer)),
            KeyCode::Char('0') => {
                return app(AppAction::Workspace(WorkspaceAction::ShowLastUsedSidebar));
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c.to_digit(10).unwrap() - 1) as usize;
                return app(AppAction::Window(WindowAction::WindowFocusByCreationIndex(
                    idx,
                )));
            }
            _ => {}
        }
    }

    // F4 → replay the last recorded/played macro (all modes)
    if key.code == KeyCode::F(4) {
        return core(CoreAction::MacroPlayLast);
    }
    if key.code == KeyCode::F(12) {
        return app(AppAction::Integration(
            IntegrationAction::RunPluginCommand {
                id: "lsp.goto_definition".to_string(),
            },
        ));
    }

    match mode {
        Mode::Insert => resolve_insert(key, state),
        Mode::Normal => resolve_normal(key, state, is_recording),
        Mode::Visual => resolve_visual(key, state, is_recording),
    }
}

fn resolve_insert(key: KeyEvent, state: &mut KeyState) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_line_boundary_extend_action(key.code)
        {
            return core(action);
        }
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_word_arrow_extend_action(key.code)
        {
            return core(action);
        }
        if let Some(action) = ctrl_word_arrow_no_select_action(key.code) {
            return core(action);
        }
        if let Some(action) = ctrl_motion_action(key.code) {
            return core(action);
        }
        match key.code {
            KeyCode::Char('d') => core(CoreAction::DeleteForward),
            KeyCode::Char('h') => core(CoreAction::DeleteBackward),
            KeyCode::Char('j') => core(CoreAction::InsertNewline),
            KeyCode::Char('k') => core(CoreAction::KillLine),
            KeyCode::Char('x') => {
                *state = KeyState::CtrlX;
                core(CoreAction::Noop)
            }
            KeyCode::Char('g') => app(AppAction::Lifecycle(LifecycleAction::Cancel)),
            _ => core(CoreAction::Noop),
        }
    } else {
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = shift_char_arrow_extend_action(key.code)
        {
            return core(action);
        }
        match key.code {
            KeyCode::Esc => core(CoreAction::ChangeMode(Mode::Normal)),
            KeyCode::Right => core(CoreAction::MoveRight),
            KeyCode::Left => core(CoreAction::MoveLeft),
            KeyCode::Down => core(CoreAction::MoveDown),
            KeyCode::Up => core(CoreAction::MoveUp),
            KeyCode::Backspace => core(CoreAction::DeleteBackward),
            KeyCode::Enter => core(CoreAction::InsertNewline),
            // Keep auto-indent on real Enter, but treat raw LF/CR character input
            // as pasted text so multiline paste fallback does not add editor indent.
            KeyCode::Char('\n') | KeyCode::Char('\r') => {
                core(CoreAction::InsertText("\n".to_string()))
            }
            KeyCode::Tab => core(CoreAction::InsertChar('\t')),
            KeyCode::Char(c) => core(CoreAction::InsertChar(c)),
            _ => core(CoreAction::Noop),
        }
    }
}

fn resolve_normal(key: KeyEvent, state: &mut KeyState, is_recording: bool) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_line_boundary_extend_action(key.code)
        {
            return core(action);
        }
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_word_arrow_extend_action(key.code)
        {
            return core(action);
        }
        if let Some(action) = ctrl_word_arrow_no_select_action(key.code) {
            return core(action);
        }
        if let Some(action) = ctrl_motion_action(key.code) {
            return core(action);
        }
        return match key.code {
            KeyCode::Char('r') => core(CoreAction::Redo),
            _ => core(CoreAction::Noop),
        };
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        && let Some(action) = shift_char_arrow_extend_action(key.code)
    {
        return core(action);
    }
    match key.code {
        KeyCode::Char('Q') => {
            if is_recording {
                core(CoreAction::MacroStop)
            } else {
                *state = KeyState::MacroRecord;
                core(CoreAction::Noop)
            }
        }
        KeyCode::Char('q') => {
            *state = KeyState::MacroPlay;
            core(CoreAction::Noop)
        }
        KeyCode::Char('i') => core(CoreAction::ChangeMode(Mode::Insert)),
        KeyCode::Char('a') => core(CoreAction::InsertAfterCursor),
        KeyCode::Char('I') => core(CoreAction::InsertAtLineStart),
        KeyCode::Char('A') => core(CoreAction::InsertAtLineEnd),
        KeyCode::Char('o') => core(CoreAction::OpenLineBelow),
        KeyCode::Char('v') => core(CoreAction::ChangeMode(Mode::Visual)),
        KeyCode::Char('y') => core(CoreAction::Yank),
        KeyCode::Char('u') => core(CoreAction::Undo),
        KeyCode::Char('w') => core(CoreAction::MoveWordForward),
        KeyCode::Char('e') => core(CoreAction::MoveWordForwardEnd),
        KeyCode::Char('b') => core(CoreAction::MoveWordBackward),
        KeyCode::Char('W') => core(CoreAction::MoveLongWordForward),
        KeyCode::Char('E') => core(CoreAction::MoveLongWordForwardEnd),
        KeyCode::Char('B') => core(CoreAction::MoveLongWordBackward),
        KeyCode::Char('h') | KeyCode::Left => core(CoreAction::MoveLeft),
        KeyCode::Char('j') | KeyCode::Down => core(CoreAction::MoveDown),
        KeyCode::Char('k') | KeyCode::Up => core(CoreAction::MoveUp),
        KeyCode::Char('l') | KeyCode::Right => core(CoreAction::MoveRight),
        KeyCode::Char('0') => core(CoreAction::MoveToLineStart),
        KeyCode::Char('$') => core(CoreAction::MoveToLineEnd),
        KeyCode::Char('x') => core(CoreAction::SelectLine),
        KeyCode::Char('d') => core(CoreAction::DeleteSelection),
        KeyCode::Char('p') => core(CoreAction::Paste),
        KeyCode::Char('.') => core(CoreAction::RepeatLastEdit),
        KeyCode::Char('/') => app(AppAction::Workspace(WorkspaceAction::SearchForward)),
        KeyCode::Char('?') => app(AppAction::Workspace(WorkspaceAction::SearchForward)),
        KeyCode::Char('n') => core(CoreAction::SearchNext),
        KeyCode::Char('N') => core(CoreAction::SearchPrev),
        KeyCode::Char('G') => core(CoreAction::MoveToFileEnd),
        KeyCode::Char('>') => core(CoreAction::Indent),
        KeyCode::Char('<') => core(CoreAction::Dedent),
        KeyCode::Tab => app(AppAction::Navigation(NavigationAction::JumpNewer)),
        KeyCode::Char('[') => core(CoreAction::WrapSelection {
            open: '[',
            close: ']',
        }),
        KeyCode::Char('(') => core(CoreAction::WrapSelection {
            open: '(',
            close: ')',
        }),
        KeyCode::Char('g') => {
            *state = KeyState::Goto;
            core(CoreAction::Noop)
        }
        KeyCode::Char(' ') => {
            *state = KeyState::Space;
            core(CoreAction::Noop)
        }
        _ => core(CoreAction::Noop),
    }
}

fn resolve_visual(key: KeyEvent, state: &mut KeyState, is_recording: bool) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_line_boundary_extend_action(key.code)
        {
            return core(action);
        }
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && let Some(action) = ctrl_shift_word_arrow_extend_action(key.code)
        {
            return core(action);
        }
        if let Some(action) = ctrl_motion_action(key.code) {
            return core(action);
        }
        return core(CoreAction::Noop);
    }
    if key.modifiers.contains(KeyModifiers::SHIFT)
        && let Some(action) = shift_char_arrow_extend_action(key.code)
    {
        return core(action);
    }
    match key.code {
        KeyCode::Char('Q') => {
            if is_recording {
                core(CoreAction::MacroStop)
            } else {
                *state = KeyState::MacroRecord;
                core(CoreAction::Noop)
            }
        }
        KeyCode::Char('q') => {
            *state = KeyState::MacroPlay;
            core(CoreAction::Noop)
        }
        KeyCode::Esc => core(CoreAction::ChangeMode(Mode::Normal)),
        KeyCode::Char('h') | KeyCode::Left => core(CoreAction::MoveLeft),
        KeyCode::Char('j') | KeyCode::Down => core(CoreAction::MoveDown),
        KeyCode::Char('k') | KeyCode::Up => core(CoreAction::MoveUp),
        KeyCode::Char('l') | KeyCode::Right => core(CoreAction::MoveRight),
        KeyCode::Char('w') => core(CoreAction::ExtendWordForward),
        KeyCode::Char('b') => core(CoreAction::ExtendWordBackward),
        KeyCode::Char('e') => core(CoreAction::ExtendWordForwardEnd),
        KeyCode::Char('W') => core(CoreAction::ExtendLongWordForward),
        KeyCode::Char('B') => core(CoreAction::ExtendLongWordBackward),
        KeyCode::Char('E') => core(CoreAction::ExtendLongWordForwardEnd),
        KeyCode::Char('0') => core(CoreAction::MoveToLineStart),
        KeyCode::Char('$') => core(CoreAction::MoveToLineEnd),
        KeyCode::Char('x') => core(CoreAction::ExtendLineSelection),
        KeyCode::Char('d') => core(CoreAction::DeleteSelection),
        KeyCode::Char('y') => core(CoreAction::YankSelection),
        KeyCode::Char('v') => core(CoreAction::VisualExpand),
        KeyCode::Char(';') => core(CoreAction::CollapseSelection),
        KeyCode::Char('>') => core(CoreAction::Indent),
        KeyCode::Char('<') => core(CoreAction::Dedent),
        KeyCode::Char('[') => core(CoreAction::WrapSelection {
            open: '[',
            close: ']',
        }),
        KeyCode::Char('(') => core(CoreAction::WrapSelection {
            open: '(',
            close: ')',
        }),
        KeyCode::Char('g') => {
            *state = KeyState::Goto;
            core(CoreAction::Noop)
        }
        KeyCode::Char(' ') => {
            *state = KeyState::Space;
            core(CoreAction::Noop)
        }
        _ => core(CoreAction::Noop),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn ctrl_shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
    }

    // -------------------------------------------------------
    // Normal mode: g enters Goto state
    // -------------------------------------------------------

    #[test]
    fn normal_g_enters_goto_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('g'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Goto);
    }

    #[test]
    fn goto_gg_moves_to_file_start() {
        let mut state = KeyState::Goto;
        let action = resolve(key('g'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MoveToFileStart));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_ge_moves_to_file_end() {
        let mut state = KeyState::Goto;
        let action = resolve(key('e'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MoveToFileEnd));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gd_requests_lsp_definition() {
        let mut state = KeyState::Goto;
        let action = resolve(key('d'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.goto_definition".to_string(),
                }
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gr_requests_lsp_references() {
        let mut state = KeyState::Goto;
        let action = resolve(key('r'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.find_references".to_string(),
                }
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gh_moves_to_line_start() {
        let mut state = KeyState::Goto;
        let action = resolve(key('h'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MoveToLineStart));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gl_moves_to_line_end() {
        let mut state = KeyState::Goto;
        let action = resolve(key('l'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MoveToLineEnd));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gp_moves_to_previous_buffer() {
        let mut state = KeyState::Goto;
        let action = resolve(key('p'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::PrevBuffer));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_gn_moves_to_next_buffer() {
        let mut state = KeyState::Goto;
        let action = resolve(key('n'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::NextBuffer));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn goto_invalid_returns_noop() {
        let mut state = KeyState::Goto;
        let action = resolve(key('x'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Normal);
    }

    // -------------------------------------------------------
    // Normal mode: Space chord
    // -------------------------------------------------------

    #[test]
    fn normal_space_enters_space_state() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut state,
            &Mode::Normal,
            false,
        );
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Space);
    }

    #[test]
    fn space_e_opens_explorer() {
        let mut state = KeyState::Space;
        let action = resolve(key('e'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::ToggleExplorer))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_f_opens_file_picker() {
        let mut state = KeyState::Space;
        let action = resolve(key('f'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenFilePicker))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_p_opens_command_palette() {
        let mut state = KeyState::Space;
        let action = resolve(key('p'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenCommandPalette))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_j_opens_jump_picker() {
        let mut state = KeyState::Space;
        let action = resolve(key('j'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenJumpListPicker))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_s_opens_symbol_picker() {
        let mut state = KeyState::Space;
        let action = resolve(key('s'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenSymbolPicker))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_slash_opens_global_search() {
        let mut state = KeyState::Space;
        let action = resolve(key('/'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenGlobalSearch))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_g_toggles_changed_files_sidebar() {
        let mut state = KeyState::Space;
        let action = resolve(key('g'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(
                WorkspaceAction::ToggleChangedFilesSidebar
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_shift_g_opens_git_view() {
        let mut state = KeyState::Space;
        let action = resolve(key('G'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenGitView))
        );
        assert_eq!(state, KeyState::Normal);
    }

    // -------------------------------------------------------
    // Normal mode: full two-key sequence via resolve()
    // -------------------------------------------------------

    #[test]
    fn normal_gg_sequence() {
        let mut state = KeyState::Normal;
        let a1 = resolve(key('g'), &mut state, &Mode::Normal, false);
        assert_eq!(a1, core(CoreAction::Noop));
        let a2 = resolve(key('g'), &mut state, &Mode::Normal, false);
        assert_eq!(a2, core(CoreAction::MoveToFileStart));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn normal_ge_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Normal, false);
        let action = resolve(key('e'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MoveToFileEnd));
    }

    #[test]
    fn normal_gr_sequence_requests_lsp_references() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Normal, false);
        let action = resolve(key('r'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.find_references".to_string(),
                }
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn normal_gp_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Normal, false);
        let action = resolve(key('p'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::PrevBuffer));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn normal_gn_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Normal, false);
        let action = resolve(key('n'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::NextBuffer));
        assert_eq!(state, KeyState::Normal);
    }

    // -------------------------------------------------------
    // Visual mode: g enters Goto state
    // -------------------------------------------------------

    #[test]
    fn visual_g_enters_goto_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('g'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Goto);
    }

    #[test]
    fn visual_gg_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Visual, false);
        let action = resolve(key('g'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::MoveToFileStart));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn visual_ge_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Visual, false);
        let action = resolve(key('e'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::MoveToFileEnd));
    }

    #[test]
    fn visual_gr_sequence_requests_lsp_references() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Visual, false);
        let action = resolve(key('r'), &mut state, &Mode::Visual, false);
        assert_eq!(
            action,
            app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.find_references".to_string(),
                }
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn visual_gh_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Visual, false);
        let action = resolve(key('h'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::MoveToLineStart));
    }

    #[test]
    fn visual_gl_sequence() {
        let mut state = KeyState::Normal;
        resolve(key('g'), &mut state, &Mode::Visual, false);
        let action = resolve(key('l'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::MoveToLineEnd));
    }

    #[test]
    fn normal_long_word_keys_map_to_long_word_actions() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(key('W'), &mut state, &Mode::Normal, false),
            core(CoreAction::MoveLongWordForward)
        );
        assert_eq!(
            resolve(key('B'), &mut state, &Mode::Normal, false),
            core(CoreAction::MoveLongWordBackward)
        );
        assert_eq!(
            resolve(key('E'), &mut state, &Mode::Normal, false),
            core(CoreAction::MoveLongWordForwardEnd)
        );
    }

    #[test]
    fn visual_word_keys_map_to_extend_actions() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(key('w'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendWordForward)
        );
        assert_eq!(
            resolve(key('b'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendWordBackward)
        );
        assert_eq!(
            resolve(key('e'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendWordForwardEnd)
        );
    }

    #[test]
    fn visual_long_word_keys_map_to_extend_long_word_actions() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(key('W'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendLongWordForward)
        );
        assert_eq!(
            resolve(key('B'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendLongWordBackward)
        );
        assert_eq!(
            resolve(key('E'), &mut state, &Mode::Visual, false),
            core(CoreAction::ExtendLongWordForwardEnd)
        );
    }

    #[test]
    fn normal_wrap_keys_map_to_wrap_selection_actions() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(key('['), &mut state, &Mode::Normal, false),
            core(CoreAction::WrapSelection {
                open: '[',
                close: ']',
            })
        );
        assert_eq!(
            resolve(key('('), &mut state, &Mode::Normal, false),
            core(CoreAction::WrapSelection {
                open: '(',
                close: ')',
            })
        );
        assert_eq!(
            resolve(key('G'), &mut state, &Mode::Normal, false),
            core(CoreAction::MoveToFileEnd)
        );
    }

    #[test]
    fn ctrl_o_i_map_to_jumplist_navigation() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(
                ctrl_key(KeyCode::Char('o')),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Navigation(NavigationAction::JumpOlder))
        );
        assert_eq!(
            resolve(
                ctrl_key(KeyCode::Char('i')),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Navigation(NavigationAction::JumpNewer))
        );
    }

    #[test]
    fn ctrl_left_maps_to_word_backward_without_selection_in_insert_and_normal() {
        for mode in [Mode::Insert, Mode::Normal] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_key(KeyCode::Left), &mut state, &mode, false),
                core(CoreAction::MoveWordBackwardNoSelect)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn ctrl_up_maps_to_move_up_in_all_modes() {
        for mode in [Mode::Insert, Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_key(KeyCode::Up), &mut state, &mode, false),
                core(CoreAction::MoveUp)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn ctrl_down_maps_to_move_down_in_all_modes() {
        for mode in [Mode::Insert, Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_key(KeyCode::Down), &mut state, &mode, false),
                core(CoreAction::MoveDown)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn ctrl_right_maps_to_word_forward_without_selection_in_insert_and_normal() {
        for mode in [Mode::Insert, Mode::Normal] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_key(KeyCode::Right), &mut state, &mode, false),
                core(CoreAction::MoveWordForwardNoSelect)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn shift_up_down_extend_line_selection_in_all_modes() {
        for mode in [Mode::Insert, Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(shift_key(KeyCode::Up), &mut state, &mode, false),
                core(CoreAction::ExtendUp)
            );
            assert_eq!(state, KeyState::Normal);

            assert_eq!(
                resolve(shift_key(KeyCode::Down), &mut state, &mode, false),
                core(CoreAction::ExtendDown)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn shift_left_right_extend_char_selection_in_insert_mode() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(shift_key(KeyCode::Left), &mut state, &Mode::Insert, false),
            core(CoreAction::ExtendLeft)
        );
        assert_eq!(
            resolve(shift_key(KeyCode::Right), &mut state, &Mode::Insert, false),
            core(CoreAction::ExtendRight)
        );
    }

    #[test]
    fn ctrl_shift_a_e_extend_to_line_boundary_in_all_modes() {
        for mode in [Mode::Insert, Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_shift_key(KeyCode::Char('a')), &mut state, &mode, false),
                core(CoreAction::ExtendToLineStart)
            );
            assert_eq!(state, KeyState::Normal);

            assert_eq!(
                resolve(ctrl_shift_key(KeyCode::Char('e')), &mut state, &mode, false),
                core(CoreAction::ExtendToLineEnd)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn shift_left_right_extend_char_selection_in_normal_and_visual() {
        for mode in [Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(shift_key(KeyCode::Left), &mut state, &mode, false),
                core(CoreAction::ExtendLeft)
            );
            assert_eq!(state, KeyState::Normal);

            assert_eq!(
                resolve(shift_key(KeyCode::Right), &mut state, &mode, false),
                core(CoreAction::ExtendRight)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn ctrl_shift_left_right_extend_word_selection_in_normal_and_visual() {
        for mode in [Mode::Normal, Mode::Visual] {
            let mut state = KeyState::Normal;
            assert_eq!(
                resolve(ctrl_shift_key(KeyCode::Left), &mut state, &mode, false),
                core(CoreAction::ExtendWordBackwardShift)
            );
            assert_eq!(state, KeyState::Normal);

            assert_eq!(
                resolve(ctrl_shift_key(KeyCode::Right), &mut state, &mode, false),
                core(CoreAction::ExtendWordForwardShift)
            );
            assert_eq!(state, KeyState::Normal);
        }
    }

    #[test]
    fn visual_ctrl_word_arrows_keep_existing_behavior() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(ctrl_key(KeyCode::Left), &mut state, &Mode::Visual, false),
            core(CoreAction::MoveWordBackward)
        );
        assert_eq!(state, KeyState::Normal);

        assert_eq!(
            resolve(ctrl_key(KeyCode::Right), &mut state, &Mode::Visual, false),
            core(CoreAction::Noop)
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn f12_requests_lsp_definition() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
            &mut state,
            &Mode::Normal,
            false,
        );
        assert_eq!(
            action,
            app(AppAction::Integration(
                IntegrationAction::RunPluginCommand {
                    id: "lsp.goto_definition".to_string(),
                }
            ))
        );
    }

    #[test]
    fn f4_replays_last_macro() {
        for mode in [Mode::Normal, Mode::Visual, Mode::Insert] {
            let mut state = KeyState::Normal;
            let action = resolve(
                KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
                &mut state,
                &mode,
                false,
            );
            assert_eq!(action, core(CoreAction::MacroPlayLast));
        }
    }

    #[test]
    fn question_opens_search_bar() {
        let mut state = KeyState::Normal;
        let action = resolve(key('?'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::SearchForward))
        );
    }

    #[test]
    fn visual_wrap_keys_map_to_wrap_selection_actions() {
        let mut state = KeyState::Normal;
        assert_eq!(
            resolve(key('['), &mut state, &Mode::Visual, false),
            core(CoreAction::WrapSelection {
                open: '[',
                close: ']',
            })
        );
        assert_eq!(
            resolve(key('('), &mut state, &Mode::Visual, false),
            core(CoreAction::WrapSelection {
                open: '(',
                close: ')',
            })
        );
    }

    // -------------------------------------------------------
    // Insert mode: g does NOT enter Goto
    // -------------------------------------------------------

    #[test]
    fn insert_g_does_not_enter_goto() {
        let mut state = KeyState::Normal;
        let action = resolve(key('g'), &mut state, &Mode::Insert, false);
        assert_eq!(action, core(CoreAction::InsertChar('g')));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn insert_char_lf_maps_to_insert_text_newline() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::Char('\n'), KeyModifiers::NONE),
            &mut state,
            &Mode::Insert,
            false,
        );
        assert_eq!(action, core(CoreAction::InsertText("\n".to_string())));
    }

    #[test]
    fn insert_char_cr_maps_to_insert_text_newline() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::NONE),
            &mut state,
            &Mode::Insert,
            false,
        );
        assert_eq!(action, core(CoreAction::InsertText("\n".to_string())));
    }

    #[test]
    fn insert_ctrl_j_maps_to_insert_newline() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            &mut state,
            &Mode::Insert,
            false,
        );
        assert_eq!(action, core(CoreAction::InsertNewline));
    }

    // -------------------------------------------------------
    // Goto chord resolves before mode dispatch
    // -------------------------------------------------------

    #[test]
    fn goto_state_resolves_before_mode_dispatch() {
        let mut state = KeyState::Goto;
        let action = resolve(key('g'), &mut state, &Mode::Insert, false);
        assert_eq!(action, core(CoreAction::MoveToFileStart));
        assert_eq!(state, KeyState::Normal);
    }

    // -------------------------------------------------------
    // Macro key resolution tests
    // -------------------------------------------------------

    #[test]
    fn normal_shift_q_not_recording_enters_macro_record_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('Q'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::MacroRecord);
    }

    #[test]
    fn normal_shift_q_while_recording_stops_macro() {
        let mut state = KeyState::Normal;
        let action = resolve(key('Q'), &mut state, &Mode::Normal, true);
        assert_eq!(action, core(CoreAction::MacroStop));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn macro_record_chord_with_valid_register() {
        let mut state = KeyState::MacroRecord;
        let action = resolve(key('a'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MacroRecord('a')));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn macro_record_chord_with_invalid_register() {
        let mut state = KeyState::MacroRecord;
        let action = resolve(key('1'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn normal_q_enters_macro_play_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('q'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::MacroPlay);
    }

    #[test]
    fn macro_play_chord_with_valid_register() {
        let mut state = KeyState::MacroPlay;
        let action = resolve(key('b'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MacroPlay('b')));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn macro_play_chord_with_at_replays_last() {
        let mut state = KeyState::MacroPlay;
        let action = resolve(key('@'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::MacroPlayLast));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn visual_shift_q_not_recording_enters_macro_record_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('Q'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::MacroRecord);
    }

    #[test]
    fn visual_q_enters_macro_play_state() {
        let mut state = KeyState::Normal;
        let action = resolve(key('q'), &mut state, &Mode::Visual, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::MacroPlay);
    }

    #[test]
    fn insert_q_types_character() {
        let mut state = KeyState::Normal;
        let action = resolve(key('q'), &mut state, &Mode::Insert, false);
        assert_eq!(action, core(CoreAction::InsertChar('q')));
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_b_opens_buffer_picker() {
        let mut state = KeyState::Normal;
        resolve(key(' '), &mut state, &Mode::Normal, false);
        assert_eq!(state, KeyState::Space);
        let action = resolve(key('b'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenBufferPicker))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn visual_space_enters_space_state() {
        let mut state = KeyState::Normal;
        let action = resolve(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut state,
            &Mode::Visual,
            false,
        );
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::Space);
    }

    #[test]
    fn visual_space_p_opens_command_palette() {
        let mut state = KeyState::Normal;
        resolve(key(' '), &mut state, &Mode::Visual, false);
        assert_eq!(state, KeyState::Space);
        let action = resolve(key('p'), &mut state, &Mode::Visual, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::OpenCommandPalette))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn normal_dot_maps_to_repeat_last_edit() {
        let mut state = KeyState::Normal;
        let action = resolve(key('.'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::RepeatLastEdit));
    }

    #[test]
    fn insert_dot_types_period() {
        let mut state = KeyState::Normal;
        let action = resolve(key('.'), &mut state, &Mode::Insert, false);
        assert_eq!(action, core(CoreAction::InsertChar('.')));
    }

    #[test]
    fn space_w_enters_window_state() {
        let mut state = KeyState::Normal;
        resolve(key(' '), &mut state, &Mode::Normal, false);
        assert_eq!(state, KeyState::Space);
        let action = resolve(key('w'), &mut state, &Mode::Normal, false);
        assert_eq!(action, core(CoreAction::Noop));
        assert_eq!(state, KeyState::SpaceWindow);
    }

    #[test]
    fn space_window_split_and_focus_actions() {
        let mut state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('v'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowSplit(
                WindowSplitAxis::Vertical,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('s'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowSplit(
                WindowSplitAxis::Horizontal,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('h'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowFocus(
                WindowDirection::Left,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('w'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowFocusNext))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_window_close_swap_and_invalid() {
        let mut state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('q'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowCloseCurrent))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('o'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowCloseOthers))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('L'), &mut state, &Mode::Normal, false),
            app(AppAction::Window(WindowAction::WindowSwap(
                WindowDirection::Right,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(key('x'), &mut state, &Mode::Normal, false),
            core(CoreAction::Noop)
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_window_arrow_keys_map_to_focus_actions() {
        let mut state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowFocus(
                WindowDirection::Left,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowFocus(
                WindowDirection::Down,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowFocus(
                WindowDirection::Up,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowFocus(
                WindowDirection::Right,
            )))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_window_shift_arrow_keys_map_to_swap_actions() {
        let mut state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowSwap(
                WindowDirection::Left,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowSwap(
                WindowDirection::Down,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowSwap(
                WindowDirection::Up,
            )))
        );
        assert_eq!(state, KeyState::Normal);

        state = KeyState::SpaceWindow;
        assert_eq!(
            resolve(
                KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT),
                &mut state,
                &Mode::Normal,
                false
            ),
            app(AppAction::Window(WindowAction::WindowSwap(
                WindowDirection::Right,
            )))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn ctrl_digits_1_to_9_focus_window_by_creation_index() {
        for n in 1u32..=9 {
            let ch = char::from_digit(n, 10).unwrap();
            let mut state = KeyState::Normal;
            let action = resolve(
                ctrl_key(KeyCode::Char(ch)),
                &mut state,
                &Mode::Normal,
                false,
            );
            assert_eq!(
                action,
                app(AppAction::Window(WindowAction::WindowFocusByCreationIndex(
                    (n - 1) as usize
                ))),
                "ctrl+{ch} should focus index {}",
                n - 1
            );
        }
    }

    #[test]
    fn ctrl_zero_shows_last_used_sidebar() {
        let mut state = KeyState::Normal;
        let action = resolve(
            ctrl_key(KeyCode::Char('0')),
            &mut state,
            &Mode::Normal,
            false,
        );
        assert_eq!(
            action,
            app(AppAction::Workspace(WorkspaceAction::ShowLastUsedSidebar))
        );
    }

    #[test]
    fn ctrl_digits_fire_in_insert_mode_too() {
        let mut state = KeyState::Normal;
        let action = resolve(
            ctrl_key(KeyCode::Char('3')),
            &mut state,
            &Mode::Insert,
            false,
        );
        assert_eq!(
            action,
            app(AppAction::Window(WindowAction::WindowFocusByCreationIndex(
                2
            )))
        );
    }

    #[test]
    fn space_d_opens_branch_compare_sidebar_picker() {
        let mut state = KeyState::Space;
        let action = resolve(key('d'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(
                WorkspaceAction::OpenBranchCompareSidebarPicker
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }

    #[test]
    fn space_capital_d_opens_branch_compare_buffer_picker() {
        let mut state = KeyState::Space;
        let action = resolve(key('D'), &mut state, &Mode::Normal, false);
        assert_eq!(
            action,
            app(AppAction::Workspace(
                WorkspaceAction::OpenBranchComparePicker
            ))
        );
        assert_eq!(state, KeyState::Normal);
    }
}

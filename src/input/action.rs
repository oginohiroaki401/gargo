use std::path::PathBuf;

use crate::core::mode::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Core(CoreAction),
    Ui(UiAction),
    App(AppAction),
    /// A left-click landed inside a buffer pane; coordinates are in terminal
    /// cells. The dispatcher converts to a doc position and applies cursor →
    /// word → line → block escalation based on click history.
    BufferClick {
        buffer_id: usize,
        screen_col: u16,
        screen_row: u16,
    },
    /// The pointer moved while the left button is held after a click landed in
    /// a buffer pane. The dispatcher extends the selection from the original
    /// click anchor to the current screen position.
    BufferDrag {
        buffer_id: usize,
        screen_col: u16,
        screen_row: u16,
    },
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreAction {
    // Cursor movement
    MoveRight,
    MoveLeft,
    MoveDown,
    MoveUp,
    MoveToLineStart,
    MoveToLineEnd,
    MoveWordForward,
    MoveWordForwardEnd,
    MoveWordBackward,
    MoveWordForwardNoSelect,
    MoveWordBackwardNoSelect,
    MoveLongWordForward,
    MoveLongWordForwardEnd,
    MoveLongWordBackward,
    MoveToLineNumber(usize),
    MoveToFileStart,
    MoveToFileEnd,

    // Editing (generates EditEvent)
    InsertChar(char),
    InsertText(String),
    InsertNewline,
    DeleteForward,
    DeleteBackward,
    KillLine,

    // Mode transitions
    ChangeMode(Mode),
    InsertAfterCursor,
    InsertAtLineStart,
    InsertAtLineEnd,
    OpenLineBelow,

    // Buffer management
    NewBuffer,
    NextBuffer,
    PrevBuffer,
    SwitchBufferByIndex(usize),

    // Undo/Redo
    Undo,
    Redo,

    // Search
    SearchUpdate(String),
    SearchNext,
    SearchPrev,
    AddCursorToNextMatch,
    AddCursorToPrevMatch,
    AddCursorToAllMatches,

    // Selection / Visual mode
    SelectLine,
    ExtendLineSelection,
    ExtendRight,
    ExtendLeft,
    ExtendUp,
    ExtendDown,
    ExtendToLineStart,
    ExtendToLineEnd,
    ExtendWordForwardShift,
    ExtendWordBackwardShift,
    ExtendWordForward,
    ExtendWordForwardEnd,
    ExtendWordBackward,
    ExtendLongWordForward,
    ExtendLongWordForwardEnd,
    ExtendLongWordBackward,
    DeleteSelection,
    YankSelection,
    Paste,
    CollapseSelection,
    /// Expand the visual-mode selection one structural step (word → enclosing
    /// brackets → line → markdown block → file), or restart the expand chain
    /// at the current cursor when it has moved since the last step.
    VisualExpand,
    Indent,
    Dedent,
    WrapSelection {
        open: char,
        close: char,
    },

    // Macros
    MacroRecord(char),
    MacroStop,
    MacroPlay(char),
    MacroPlayLast,

    // Dot repeat
    RepeatLastEdit,

    // Multi-cursor
    AddCursorAbove,
    AddCursorBelow,
    AddCursorsToTop,
    AddCursorsToBottom,
    RemoveSecondaryCursors,

    // Other
    Yank,
    Noop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    ClosePalette,
    CloseExplorerPopup,
    CloseProjectRootPopup,
    CloseRecentProjectPopup,
    CloseSaveAsPopup,
    CloseGitView,
    CloseCommitLog,
    ClosePrListPicker,
    CloseIssueListPicker,
    CloseFindReplacePopup,
    OpenSearchBar {
        saved_cursor: usize,
        saved_scroll: usize,
        saved_horizontal_scroll: usize,
    },
    CloseSearchBar,
    SetSearchBarInput(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowSplitAxis {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowDirection {
    Left,
    Down,
    Up,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferAction {
    Save,
    OpenSaveBufferAsPopup,
    SaveBufferAs(String),
    OpenRenameFilePopup,
    RenameBufferFile(String),
    RefreshBuffer,
    CloseBuffer,
    OpenFileFromExplorer(String),
    OpenFileFromExplorerPopup(String),
    OpenFileFromGitView {
        path: String,
        line: Option<usize>,
    },
    OpenProjectFile(String),
    OpenProjectFileAt {
        rel_path: String,
        line: usize,
        char_col: usize,
    },
    SwitchBufferById(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectAction {
    OpenProjectRootPicker,
    OpenRecentProjectPicker,
    ChangeProjectRoot(String),
    SwitchToRecentProject(String),
    SwitchGitBranch(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAction {
    OpenCommandPalette,
    OpenFilePicker,
    OpenBufferPicker,
    OpenJumpListPicker,
    OpenSymbolPicker,
    OpenSmartCopy,
    OpenGlobalSearch,
    ToggleExplorer,
    ToggleChangedFilesSidebar,
    RevealInExplorer,
    OpenExplorerPopup,
    OpenGitView,
    OpenCommitLog,
    OpenGitCommitMessageBuffer,
    OpenGitBranchPicker,
    OpenPrList,
    OpenIssueList,
    OpenFindReplace,
    OpenInEditorDiffView,
    RefreshInEditorDiffView,
    OpenBranchComparePicker,
    OpenBranchCompareView(String),
    OpenBranchCompareSidebarPicker,
    OpenBranchCompareSidebar(String),
    OpenCommitDiffView(String),
    ShowLastUsedSidebar,
    OpenSearchResultsBuffer {
        query: String,
        entries: Vec<SearchResultEntry>,
    },
    ExecuteFindReplace {
        find: String,
        replace: String,
        use_regex: bool,
        replace_all: bool,
    },
    SearchForward,
    SearchConfirm,
    SearchCancel {
        saved_cursor: usize,
        saved_scroll: usize,
        saved_horizontal_scroll: usize,
    },
    SearchHistoryPrev,
    SearchHistoryNext,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultEntry {
    pub rel_path: String,
    pub line: usize,
    pub char_col: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    WindowSplit(WindowSplitAxis),
    WindowFocus(WindowDirection),
    WindowFocusNext,
    WindowCloseCurrent,
    WindowCloseOthers,
    WindowSwap(WindowDirection),
    WindowFocusByCreationIndex(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationAction {
    RunPluginCommand { id: String },
    OpenPrUrl(String),
    OpenIssueUrl(String),
    ApplyMarkdownLinkCompletion { candidate: String },
    CopyToClipboard { text: String, description: String },
    ShowMessage(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleAction {
    ReloadConfig,
    OpenConfigFile,
    CreateDefaultConfig,
    ToggleDebug,
    ToggleLineNumber,
    Quit,
    ForceQuit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationAction {
    JumpToLineChar {
        line: usize,
        char_col: usize,
    },
    JumpOlder,
    JumpNewer,
    JumpToListIndex(usize),
    OpenFileAtLspLocation {
        path: PathBuf,
        line: usize,
        character_utf16: usize,
    },
    ExecutePaletteCommand(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAction {
    Buffer(BufferAction),
    Project(ProjectAction),
    Workspace(WorkspaceAction),
    Window(WindowAction),
    Integration(IntegrationAction),
    Lifecycle(LifecycleAction),
    Navigation(NavigationAction),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_variants() {
        // Test that all action wrapper variants exist
        let _core = Action::Core(CoreAction::Noop);
        let _ui = Action::Ui(UiAction::ClosePalette);
        let _app = Action::App(AppAction::Lifecycle(LifecycleAction::Quit));
        let _noop = Action::Noop;
    }

    #[test]
    fn test_core_action_movement() {
        // Verify movement actions exist
        let movements = vec![
            CoreAction::MoveRight,
            CoreAction::MoveLeft,
            CoreAction::MoveDown,
            CoreAction::MoveUp,
            CoreAction::MoveToLineStart,
            CoreAction::MoveToLineEnd,
            CoreAction::MoveWordForward,
            CoreAction::MoveWordForwardEnd,
            CoreAction::MoveWordBackward,
            CoreAction::MoveWordForwardNoSelect,
            CoreAction::MoveWordBackwardNoSelect,
            CoreAction::MoveLongWordForward,
            CoreAction::MoveLongWordForwardEnd,
            CoreAction::MoveLongWordBackward,
            CoreAction::MoveToLineNumber(1),
            CoreAction::MoveToFileStart,
            CoreAction::MoveToFileEnd,
        ];
        assert_eq!(movements.len(), 17);
    }

    #[test]
    fn test_core_action_editing() {
        // Verify editing actions exist
        let _char = CoreAction::InsertChar('a');
        let _text = CoreAction::InsertText("hello\nworld".to_string());
        let _newline = CoreAction::InsertNewline;
        let _del_fwd = CoreAction::DeleteForward;
        let _del_back = CoreAction::DeleteBackward;
        let _kill = CoreAction::KillLine;
    }

    #[test]
    fn test_core_action_mode_transitions() {
        // Verify mode transition actions
        let _change_mode = CoreAction::ChangeMode(Mode::Normal);
        let _insert_after = CoreAction::InsertAfterCursor;
        let _insert_start = CoreAction::InsertAtLineStart;
        let _insert_end = CoreAction::InsertAtLineEnd;
        let _open_below = CoreAction::OpenLineBelow;
    }

    #[test]
    fn test_core_action_buffer_management() {
        // Verify buffer management actions
        let _new = CoreAction::NewBuffer;
        let _next = CoreAction::NextBuffer;
        let _prev = CoreAction::PrevBuffer;
        let _switch = CoreAction::SwitchBufferByIndex(0);
    }

    #[test]
    fn test_core_action_undo_redo() {
        // Verify undo/redo actions
        let _undo = CoreAction::Undo;
        let _redo = CoreAction::Redo;
    }

    #[test]
    fn test_core_action_search() {
        // Verify search actions
        let _update = CoreAction::SearchUpdate("test".to_string());
        let _next = CoreAction::SearchNext;
        let _prev = CoreAction::SearchPrev;
        let _add_next = CoreAction::AddCursorToNextMatch;
        let _add_prev = CoreAction::AddCursorToPrevMatch;
        let _add_all = CoreAction::AddCursorToAllMatches;
    }

    #[test]
    fn test_core_action_selection() {
        // Verify selection actions
        let actions = [
            CoreAction::SelectLine,
            CoreAction::ExtendLineSelection,
            CoreAction::ExtendRight,
            CoreAction::ExtendLeft,
            CoreAction::ExtendWordForwardShift,
            CoreAction::ExtendWordBackwardShift,
            CoreAction::ExtendWordForward,
            CoreAction::ExtendWordForwardEnd,
            CoreAction::ExtendWordBackward,
            CoreAction::ExtendLongWordForward,
            CoreAction::ExtendLongWordForwardEnd,
            CoreAction::ExtendLongWordBackward,
            CoreAction::DeleteSelection,
            CoreAction::YankSelection,
            CoreAction::Paste,
            CoreAction::CollapseSelection,
            CoreAction::Indent,
            CoreAction::Dedent,
            CoreAction::WrapSelection {
                open: '[',
                close: ']',
            },
        ];
        assert_eq!(actions.len(), 19);
    }

    #[test]
    fn test_core_action_macros() {
        // Verify macro actions
        let _record = CoreAction::MacroRecord('q');
        let _stop = CoreAction::MacroStop;
        let _play = CoreAction::MacroPlay('q');
        let _play_last = CoreAction::MacroPlayLast;
    }

    #[test]
    fn test_ui_action_palette() {
        // Verify palette UI actions
        let _close = UiAction::ClosePalette;
    }

    #[test]
    fn test_ui_action_explorer() {
        // Verify explorer UI actions
        let _close_popup = UiAction::CloseExplorerPopup;
        let _close_project_root = UiAction::CloseProjectRootPopup;
        let _close_recent_project = UiAction::CloseRecentProjectPopup;
        let _close_save_as = UiAction::CloseSaveAsPopup;
    }

    #[test]
    fn test_ui_action_git_view() {
        // Verify git view UI actions
        let _close = UiAction::CloseGitView;
        let _close_commit_log = UiAction::CloseCommitLog;
        let _close_pr = UiAction::ClosePrListPicker;
        let _close_issue = UiAction::CloseIssueListPicker;
    }

    #[test]
    fn test_ui_action_search_bar() {
        // Verify search bar UI actions
        let _open = UiAction::OpenSearchBar {
            saved_cursor: 0,
            saved_scroll: 0,
            saved_horizontal_scroll: 0,
        };
        let _close = UiAction::CloseSearchBar;
        let _set_input = UiAction::SetSearchBarInput("test".to_string());
    }

    #[test]
    fn test_app_action_file_operations() {
        // Verify file operation app actions
        let _save = AppAction::Buffer(BufferAction::Save);
        let _save_as_popup = AppAction::Buffer(BufferAction::OpenSaveBufferAsPopup);
        let _save_as = AppAction::Buffer(BufferAction::SaveBufferAs("/tmp/file.txt".to_string()));
        let _reload_config = AppAction::Lifecycle(LifecycleAction::ReloadConfig);
        let _open_config = AppAction::Lifecycle(LifecycleAction::OpenConfigFile);
        let _create_config = AppAction::Lifecycle(LifecycleAction::CreateDefaultConfig);
        let _toggle_debug = AppAction::Lifecycle(LifecycleAction::ToggleDebug);
        let _toggle_line_number = AppAction::Lifecycle(LifecycleAction::ToggleLineNumber);
        let _close = AppAction::Buffer(BufferAction::CloseBuffer);
    }

    #[test]
    fn test_app_action_quit() {
        // Verify quit actions
        let _quit = AppAction::Lifecycle(LifecycleAction::Quit);
        let _force_quit = AppAction::Lifecycle(LifecycleAction::ForceQuit);
        let _cancel = AppAction::Lifecycle(LifecycleAction::Cancel);
    }

    #[test]
    fn test_app_action_pickers() {
        // Verify picker actions
        let _cmd_palette = AppAction::Workspace(WorkspaceAction::OpenCommandPalette);
        let _file_picker = AppAction::Workspace(WorkspaceAction::OpenFilePicker);
        let _buf_picker = AppAction::Workspace(WorkspaceAction::OpenBufferPicker);
        let _jump_picker = AppAction::Workspace(WorkspaceAction::OpenJumpListPicker);
        let _symbol_picker = AppAction::Workspace(WorkspaceAction::OpenSymbolPicker);
        let _smart_copy = AppAction::Workspace(WorkspaceAction::OpenSmartCopy);
        let _global_search = AppAction::Workspace(WorkspaceAction::OpenGlobalSearch);
        let _root_picker = AppAction::Project(ProjectAction::OpenProjectRootPicker);
        let _recent_picker = AppAction::Project(ProjectAction::OpenRecentProjectPicker);
        let _switch_git = AppAction::Project(ProjectAction::SwitchGitBranch("main".to_string()));
    }

    #[test]
    fn test_app_action_explorer_operations() {
        // Verify explorer operations
        let _toggle = AppAction::Workspace(WorkspaceAction::ToggleExplorer);
        let _toggle_changed = AppAction::Workspace(WorkspaceAction::ToggleChangedFilesSidebar);
        let _reveal = AppAction::Workspace(WorkspaceAction::RevealInExplorer);
        let _open_popup = AppAction::Workspace(WorkspaceAction::OpenExplorerPopup);
        let _open_from_explorer =
            AppAction::Buffer(BufferAction::OpenFileFromExplorer("/path".to_string()));
        let _open_from_popup =
            AppAction::Buffer(BufferAction::OpenFileFromExplorerPopup("/path".to_string()));
        let _copy = AppAction::Integration(IntegrationAction::CopyToClipboard {
            text: "/path".to_string(),
            description: "file path".to_string(),
        });
        let _msg = AppAction::Integration(IntegrationAction::ShowMessage("ok".to_string()));
    }

    #[test]
    fn test_app_action_git_operations() {
        // Verify git operations
        let _open_git = AppAction::Workspace(WorkspaceAction::OpenGitView);
        let _open_commit_log = AppAction::Workspace(WorkspaceAction::OpenCommitLog);
        let _open_commit = AppAction::Workspace(WorkspaceAction::OpenGitCommitMessageBuffer);
        let _open_switch_branch = AppAction::Workspace(WorkspaceAction::OpenGitBranchPicker);
        let _open_diff = AppAction::Workspace(WorkspaceAction::OpenInEditorDiffView);
        let _refresh_diff = AppAction::Workspace(WorkspaceAction::RefreshInEditorDiffView);
        let _compare_picker = AppAction::Workspace(WorkspaceAction::OpenBranchComparePicker);
        let _compare_view =
            AppAction::Workspace(WorkspaceAction::OpenBranchCompareView("main".to_string()));
        let _commit_diff =
            AppAction::Workspace(WorkspaceAction::OpenCommitDiffView("abc123".to_string()));
        let _open_from_git = AppAction::Buffer(BufferAction::OpenFileFromGitView {
            path: "/path".to_string(),
            line: Some(3),
        });
        let _open_pr_list = AppAction::Workspace(WorkspaceAction::OpenPrList);
        let _open_pr_url = AppAction::Integration(IntegrationAction::OpenPrUrl(
            "https://github.com/foo/bar/pull/1".to_string(),
        ));
        let _open_issue_list = AppAction::Workspace(WorkspaceAction::OpenIssueList);
        let _open_issue_url = AppAction::Integration(IntegrationAction::OpenIssueUrl(
            "https://github.com/foo/bar/issues/1".to_string(),
        ));
    }

    #[test]
    fn test_app_action_search_operations() {
        // Verify search operations
        let _forward = AppAction::Workspace(WorkspaceAction::SearchForward);
        let _confirm = AppAction::Workspace(WorkspaceAction::SearchConfirm);
        let _cancel = AppAction::Workspace(WorkspaceAction::SearchCancel {
            saved_cursor: 0,
            saved_scroll: 0,
            saved_horizontal_scroll: 0,
        });
        let _hist_prev = AppAction::Workspace(WorkspaceAction::SearchHistoryPrev);
        let _hist_next = AppAction::Workspace(WorkspaceAction::SearchHistoryNext);
    }

    #[test]
    fn test_app_action_palette_command() {
        // Verify palette command execution
        let _execute = AppAction::Navigation(NavigationAction::ExecutePaletteCommand(0));
    }

    #[test]
    fn test_app_action_buffer_switching() {
        // Verify buffer switching actions
        let _switch = AppAction::Buffer(BufferAction::SwitchBufferById(42));
        let _jump_older = AppAction::Navigation(NavigationAction::JumpOlder);
        let _jump_newer = AppAction::Navigation(NavigationAction::JumpNewer);
        let _jump_to = AppAction::Navigation(NavigationAction::JumpToListIndex(3));
        let _jump_line_char = AppAction::Navigation(NavigationAction::JumpToLineChar {
            line: 10,
            char_col: 5,
        });
        let _open_at_lsp = AppAction::Navigation(NavigationAction::OpenFileAtLspLocation {
            path: PathBuf::from("/tmp/file.rs"),
            line: 12,
            character_utf16: 8,
        });
        let _open_project = AppAction::Buffer(BufferAction::OpenProjectFile(
            "/path/to/file.rs".to_string(),
        ));
        let _change_project_root =
            AppAction::Project(ProjectAction::ChangeProjectRoot("/new/root".to_string()));
        let _switch_recent_project = AppAction::Project(ProjectAction::SwitchToRecentProject(
            "/new/root".to_string(),
        ));
        let _open_at = AppAction::Buffer(BufferAction::OpenProjectFileAt {
            rel_path: "/path/to/file.rs".to_string(),
            line: 10,
            char_col: 5,
        });
    }

    #[test]
    fn test_action_equality() {
        // Test that actions can be compared
        assert_eq!(CoreAction::Noop, CoreAction::Noop);
        assert_ne!(CoreAction::MoveRight, CoreAction::MoveLeft);

        assert_eq!(Action::Noop, Action::Noop);
        assert_ne!(
            Action::Core(CoreAction::Noop),
            Action::App(AppAction::Lifecycle(LifecycleAction::Quit))
        );
    }

    #[test]
    fn test_action_cloning() {
        // Test that actions can be cloned
        let action = Action::Core(CoreAction::InsertChar('x'));
        let cloned = action.clone();
        assert_eq!(action, cloned);
    }

    #[test]
    fn test_core_action_with_data() {
        // Test actions that carry data
        let char_action = CoreAction::InsertChar('α');
        if let CoreAction::InsertChar(c) = char_action {
            assert_eq!(c, 'α');
        } else {
            panic!("Expected InsertChar");
        }

        let search_action = CoreAction::SearchUpdate("query".to_string());
        if let CoreAction::SearchUpdate(query) = search_action {
            assert_eq!(query, "query");
        } else {
            panic!("Expected SearchUpdate");
        }
    }

    #[test]
    fn test_ui_action_with_data() {
        // Test UI actions that carry data
        let search_bar = UiAction::OpenSearchBar {
            saved_cursor: 123,
            saved_scroll: 456,
            saved_horizontal_scroll: 789,
        };
        if let UiAction::OpenSearchBar {
            saved_cursor,
            saved_scroll,
            saved_horizontal_scroll,
        } = search_bar
        {
            assert_eq!(saved_cursor, 123);
            assert_eq!(saved_scroll, 456);
            assert_eq!(saved_horizontal_scroll, 789);
        } else {
            panic!("Expected OpenSearchBar");
        }
    }

    #[test]
    fn test_app_action_with_data() {
        // Test app actions that carry data
        let open_file = AppAction::Buffer(BufferAction::OpenFileFromExplorer(
            "/test/path.txt".to_string(),
        ));
        if let AppAction::Buffer(BufferAction::OpenFileFromExplorer(path)) = open_file {
            assert_eq!(path, "/test/path.txt");
        } else {
            panic!("Expected OpenFileFromExplorer");
        }
    }

    #[test]
    fn test_action_debug_format() {
        // Verify Debug trait works (useful for logging)
        let action = Action::Core(CoreAction::MoveRight);
        let debug_str = format!("{:?}", action);
        assert!(debug_str.contains("Core"));
        assert!(debug_str.contains("MoveRight"));
    }

    #[test]
    fn test_mode_in_core_action() {
        // Verify Mode enum integration
        let modes = [
            CoreAction::ChangeMode(Mode::Normal),
            CoreAction::ChangeMode(Mode::Insert),
            CoreAction::ChangeMode(Mode::Visual),
        ];
        assert_eq!(modes.len(), 3);
    }
}

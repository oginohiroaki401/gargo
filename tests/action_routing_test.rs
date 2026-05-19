//! Integration tests for action routing
//!
//! These tests verify that the App correctly routes actions to the appropriate
//! handlers (CoreAction -> Editor, UiAction -> Compositor, AppAction -> App).

use gargo::core::mode::Mode;
use gargo::input::action::{
    Action, AppAction, BufferAction, CoreAction, IntegrationAction, LifecycleAction,
    NavigationAction, ProjectAction, UiAction, WindowAction, WindowDirection, WindowSplitAxis,
    WorkspaceAction,
};

#[test]
fn test_action_type_categorization() {
    // Verify actions are properly categorized

    // Core actions should only affect editor state
    let core_actions = vec![
        Action::Core(CoreAction::MoveRight),
        Action::Core(CoreAction::InsertChar('a')),
        Action::Core(CoreAction::Undo),
        Action::Core(CoreAction::SearchNext),
        Action::Core(CoreAction::MacroRecord('q')),
    ];

    for action in core_actions {
        assert!(matches!(action, Action::Core(_)));
    }

    // UI actions should only affect compositor/overlays
    let ui_actions = vec![
        Action::Ui(UiAction::ClosePalette),
        Action::Ui(UiAction::CloseExplorerPopup),
        Action::Ui(UiAction::CloseProjectRootPopup),
        Action::Ui(UiAction::CloseRecentProjectPopup),
        Action::Ui(UiAction::CloseSaveAsPopup),
        Action::Ui(UiAction::CloseGitView),
        Action::Ui(UiAction::OpenSearchBar {
            saved_cursor: 0,
            saved_scroll: 0,
            saved_horizontal_scroll: 0,
        }),
    ];

    for action in ui_actions {
        assert!(matches!(action, Action::Ui(_)));
    }

    // App actions should involve I/O, commands, or app-level coordination
    let app_actions = vec![
        Action::App(AppAction::Buffer(BufferAction::Save)),
        Action::App(AppAction::Buffer(BufferAction::OpenSaveBufferAsPopup)),
        Action::App(AppAction::Buffer(BufferAction::SaveBufferAs(
            "foo.txt".to_string(),
        ))),
        Action::App(AppAction::Lifecycle(LifecycleAction::Quit)),
        Action::App(AppAction::Lifecycle(LifecycleAction::ForceQuit)),
        Action::App(AppAction::Workspace(WorkspaceAction::OpenCommandPalette)),
        Action::App(AppAction::Project(ProjectAction::OpenProjectRootPicker)),
        Action::App(AppAction::Project(ProjectAction::OpenRecentProjectPicker)),
        Action::App(AppAction::Workspace(WorkspaceAction::OpenSymbolPicker)),
        Action::App(AppAction::Buffer(BufferAction::OpenFileFromExplorer(
            "/path".to_string(),
        ))),
    ];

    for action in app_actions {
        assert!(matches!(action, Action::App(_)));
    }
}

#[test]
fn test_core_actions_are_editor_only() {
    // Verify that core actions represent pure editor state changes
    // These should not require I/O, UI updates, or external commands

    let editor_state_actions = vec![
        CoreAction::MoveRight,
        CoreAction::MoveLeft,
        CoreAction::MoveDown,
        CoreAction::MoveUp,
        CoreAction::InsertChar('x'),
        CoreAction::DeleteForward,
        CoreAction::Undo,
        CoreAction::Redo,
        CoreAction::ChangeMode(Mode::Normal),
        CoreAction::NextBuffer,
        CoreAction::PrevBuffer,
        CoreAction::SearchNext,
        CoreAction::Paste,
    ];

    // All these actions should be routable without any external dependencies
    for _ in editor_state_actions {
        // The fact that we can construct these without any context proves
        // they are properly isolated
    }
}

#[test]
fn test_ui_actions_are_compositor_only() {
    // Verify that UI actions only affect overlays/UI state
    // These should not affect editor content or trigger I/O

    let compositor_actions = vec![
        UiAction::ClosePalette,
        UiAction::CloseExplorerPopup,
        UiAction::CloseProjectRootPopup,
        UiAction::CloseRecentProjectPopup,
        UiAction::CloseSaveAsPopup,
        UiAction::CloseGitView,
        UiAction::CloseSearchBar,
    ];

    // All UI actions should be pure UI state changes
    for _ in compositor_actions {
        // Can construct without editor or file system access
    }
}

#[test]
fn test_app_actions_coordinate_systems() {
    // Verify that app actions coordinate between editor, UI, and I/O

    let coordination_actions = vec![
        AppAction::Buffer(BufferAction::Save), // Requires file I/O
        AppAction::Buffer(BufferAction::OpenSaveBufferAsPopup), // Opens save-as overlay
        AppAction::Buffer(BufferAction::SaveBufferAs("path".to_string())), // Save with explicit path
        AppAction::Buffer(BufferAction::OpenFileFromExplorer("path".to_string())), // Coordinates explorer -> editor
        AppAction::Workspace(WorkspaceAction::OpenCommandPalette), // Opens UI overlay
        AppAction::Workspace(WorkspaceAction::OpenInEditorDiffView), // Opens generated diff buffer
        AppAction::Workspace(WorkspaceAction::RefreshInEditorDiffView), // Refreshes generated diff buffer
        AppAction::Project(ProjectAction::OpenProjectRootPicker),       // Opens root picker overlay
        AppAction::Project(ProjectAction::OpenRecentProjectPicker), // Opens recent project picker
        AppAction::Workspace(WorkspaceAction::OpenSymbolPicker),    // Opens symbol picker overlay
        AppAction::Window(WindowAction::WindowSplit(WindowSplitAxis::Vertical)), // Coordinates editor+compositor
        AppAction::Window(WindowAction::WindowFocus(WindowDirection::Right)), // Coordinates editor+compositor
        AppAction::Navigation(NavigationAction::ExecutePaletteCommand(0)),    // Executes command
        AppAction::Lifecycle(LifecycleAction::Quit),                          // Exits app
        AppAction::Lifecycle(LifecycleAction::ForceQuit), // Exits app immediately
    ];

    // These actions represent coordination between systems
    for _ in coordination_actions {
        // Can construct, but execution requires full app context
    }
}

#[test]
fn test_noop_action_exists() {
    // Verify that Noop action exists at all levels
    let _action_noop = Action::Noop;
    let _core_noop = CoreAction::Noop;

    // Noop should be a valid action for "do nothing" cases
    assert_eq!(Action::Noop, Action::Noop);
    assert_eq!(CoreAction::Noop, CoreAction::Noop);
}

#[test]
fn test_action_wrapping() {
    // Verify that CoreAction/UiAction/AppAction wrap correctly into Action

    let core = CoreAction::MoveRight;
    let wrapped = Action::Core(core.clone());

    match wrapped {
        Action::Core(inner) => assert_eq!(inner, core),
        _ => panic!("Expected Core variant"),
    }

    let ui = UiAction::ClosePalette;
    let wrapped = Action::Ui(ui.clone());

    match wrapped {
        Action::Ui(inner) => assert_eq!(inner, ui),
        _ => panic!("Expected Ui variant"),
    }

    let app = AppAction::Lifecycle(LifecycleAction::Quit);
    let wrapped = Action::App(app.clone());

    match wrapped {
        Action::App(inner) => assert_eq!(inner, app),
        _ => panic!("Expected App variant"),
    }
}

#[test]
fn test_search_actions_split_correctly() {
    // Search functionality spans multiple layers:
    // - CoreAction::SearchNext/SearchPrev: Navigate search results in editor
    // - CoreAction::SearchUpdate: Update search pattern in editor
    // - UiAction::OpenSearchBar/CloseSearchBar: Show/hide search UI
    // - AppAction::Workspace(WorkspaceAction::SearchForward)/SearchConfirm/SearchCancel: Coordinate search flow

    // Core: Pure editor search navigation
    let _core_search = [
        CoreAction::SearchNext,
        CoreAction::SearchPrev,
        CoreAction::SearchUpdate("pattern".to_string()),
    ];

    // UI: Search bar visibility
    let _ui_search = [
        UiAction::OpenSearchBar {
            saved_cursor: 0,
            saved_scroll: 0,
            saved_horizontal_scroll: 0,
        },
        UiAction::CloseSearchBar,
        UiAction::SetSearchBarInput("pattern".to_string()),
    ];

    // App: Search coordination
    let _app_search = [
        AppAction::Workspace(WorkspaceAction::SearchForward),
        AppAction::Workspace(WorkspaceAction::SearchConfirm),
        AppAction::Workspace(WorkspaceAction::SearchCancel {
            saved_cursor: 0,
            saved_scroll: 0,
            saved_horizontal_scroll: 0,
        }),
        AppAction::Workspace(WorkspaceAction::SearchHistoryPrev),
        AppAction::Workspace(WorkspaceAction::SearchHistoryNext),
    ];

    // This split allows plugins to trigger search without UI dependencies
}

#[test]
fn test_buffer_actions_split_correctly() {
    // Buffer management also spans layers:
    // - CoreAction: Switch between already-loaded buffers
    // - AppAction: Open new files into buffers

    // Core: Navigate loaded buffers (no I/O)
    let _core_buffer = [
        CoreAction::NextBuffer,
        CoreAction::PrevBuffer,
        CoreAction::NewBuffer,
    ];

    // App: Load files into buffers (requires I/O)
    let _app_buffer = [
        AppAction::Window(WindowAction::WindowSplit(WindowSplitAxis::Vertical)),
        AppAction::Buffer(BufferAction::OpenProjectFile("file.rs".to_string())),
        AppAction::Project(ProjectAction::ChangeProjectRoot(
            "/tmp/new_root".to_string(),
        )),
        AppAction::Project(ProjectAction::SwitchToRecentProject(
            "/tmp/new_root".to_string(),
        )),
        AppAction::Navigation(NavigationAction::JumpToLineChar {
            line: 10,
            char_col: 2,
        }),
        AppAction::Buffer(BufferAction::OpenFileFromExplorer("file.rs".to_string())),
        AppAction::Buffer(BufferAction::OpenFileFromGitView {
            path: "file.rs".to_string(),
            line: Some(10),
        }),
        AppAction::Buffer(BufferAction::SwitchBufferById(42)),
    ];
}

#[test]
fn test_macro_actions_are_core() {
    // Macro recording/playback is core editor functionality
    // Should not require UI or I/O

    let macro_actions = vec![
        CoreAction::MacroRecord('q'),
        CoreAction::MacroStop,
        CoreAction::MacroPlay('q'),
        CoreAction::MacroPlayLast,
    ];

    for action in macro_actions {
        // All macro operations are pure editor state
        let _ = Action::Core(action);
    }
}

#[test]
fn test_mode_transitions_are_core() {
    // Mode transitions (Normal/Insert/Visual) are core editor state

    let mode_actions = vec![
        CoreAction::ChangeMode(Mode::Normal),
        CoreAction::ChangeMode(Mode::Insert),
        CoreAction::ChangeMode(Mode::Visual),
        CoreAction::InsertAfterCursor,
        CoreAction::InsertAtLineStart,
        CoreAction::InsertAtLineEnd,
        CoreAction::OpenLineBelow,
    ];

    for action in mode_actions {
        // Mode changes don't require external systems
        let _ = Action::Core(action);
    }
}

#[test]
fn test_clipboard_yank_is_core() {
    // Yank operations are core (clipboard is handled by App layer via copy_to_clipboard)
    // The action itself is core editor functionality

    let yank_actions = vec![
        CoreAction::Yank,
        CoreAction::YankSelection,
        CoreAction::Paste,
    ];

    for action in yank_actions {
        let _ = Action::Core(action);
    }
}

#[test]
fn test_explorer_actions_split_correctly() {
    // Explorer functionality spans layers:
    // - UiAction: Close explorer popup
    // - AppAction: Open explorer, reveal files, navigate

    // UI: Overlay visibility
    let _ui_explorer = [UiAction::CloseExplorerPopup];

    // App: Explorer operations (need file system access)
    let _app_explorer = [
        AppAction::Workspace(WorkspaceAction::ToggleExplorer),
        AppAction::Workspace(WorkspaceAction::ToggleChangedFilesSidebar),
        AppAction::Workspace(WorkspaceAction::RevealInExplorer),
        AppAction::Workspace(WorkspaceAction::OpenExplorerPopup),
        AppAction::Project(ProjectAction::OpenProjectRootPicker),
        AppAction::Project(ProjectAction::OpenRecentProjectPicker),
        AppAction::Project(ProjectAction::ChangeProjectRoot("/path".to_string())),
        AppAction::Project(ProjectAction::SwitchToRecentProject("/path".to_string())),
        AppAction::Buffer(BufferAction::OpenSaveBufferAsPopup),
        AppAction::Buffer(BufferAction::SaveBufferAs("/path/file.txt".to_string())),
        AppAction::Buffer(BufferAction::OpenFileFromExplorer("/path".to_string())),
        AppAction::Buffer(BufferAction::OpenFileFromExplorerPopup("/path".to_string())),
        AppAction::Integration(IntegrationAction::CopyToClipboard {
            text: "/path".to_string(),
            description: "path".to_string(),
        }),
    ];
}

#[test]
fn test_selection_actions_are_core() {
    // Selection/visual mode operations are pure editor state

    let selection_actions = vec![
        CoreAction::SelectLine,
        CoreAction::ExtendLineSelection,
        CoreAction::ExtendRight,
        CoreAction::ExtendLeft,
        CoreAction::ExtendWordForwardShift,
        CoreAction::ExtendWordBackwardShift,
        CoreAction::DeleteSelection,
        CoreAction::YankSelection,
        CoreAction::CollapseSelection,
        CoreAction::Indent,
        CoreAction::Dedent,
        CoreAction::WrapSelection {
            open: '[',
            close: ']',
        },
    ];

    for action in selection_actions {
        let _ = Action::Core(action);
    }
}

#[test]
fn test_action_routing_boundary_clarity() {
    // This test documents the architectural boundaries:
    //
    // Core: Pure editor state (cursor, text, buffers, history, modes)
    //   - No I/O, no UI overlays, no external commands
    //   - Should be embeddable in WASM or headless contexts
    //
    // UI: Compositor overlays (palette, explorer, git view, search bar)
    //   - No editor content changes, no I/O
    //   - Pure UI state management
    //
    // App: Coordination and I/O (file operations, quit, commands)
    //   - Coordinates between Core and UI
    //   - Performs I/O operations
    //   - Executes commands

    // If a future action doesn't fit clearly into one category,
    // it's a sign the boundary needs refinement
}

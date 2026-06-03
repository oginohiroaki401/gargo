# Gargo Architecture

Gargo is a terminal-based text editor written in Rust. It is modal (Vim-like), supports multi-buffer editing, Tree-sitter syntax highlighting, and a plugin system.

## Overview

The application runs a 60 FPS event loop. Keyboard events are resolved into actions via a keymap, dispatched to the appropriate handler, and then the UI is re-rendered. Long-running work (git operations, LSP, diff computation) runs asynchronously on background threads and communicates with the main loop through channels, so the render path never blocks.

Web URL behavior:
- visible `http://` and `https://` text is emitted as OSC 8 terminal hyperlinks during render, so supporting terminals can click and open in a browser
- explicit URL-open actions use a platform launcher (`xdg-open` on Linux, `open` on macOS, `cmd /C start` on Windows)
- terminals without OSC 8 support still show plain URL text

## Directory Structure

```
src/
  main.rs           Entry point. Parses CLI, handles upgrade/check, runs the app.
  app.rs            Main event loop and shared app helpers.
  app/
    dispatch_core.rs  CoreAction dispatcher implementation.
    dispatch_app.rs   AppAction dispatcher implementation.
  cli.rs            clap-based CLI arguments and mode selection.
  config.rs         Configuration (TOML) loading and types.
  lib.rs            Public API re-exports.
  log.rs            Debug logging.
  upgrade.rs        Self-upgrade and update-check flow.

  core/             Core editing engine
    editor.rs       Multi-buffer manager, jump list, search state.
    document.rs     Single document: rope text buffer, cursors, selection, history, git gutter.
    mode.rs         Normal / Insert / Visual mode enum.
    history.rs      Undo/redo transaction stacks.
    buffer.rs       EditEvent for tree-sitter, CharClass.
    macro_rec.rs    Macro recording and playback.
    lsp_types.rs    LSP diagnostic types.

  input/            Keyboard input handling
    action.rs       Action enums: CoreAction, UiAction, AppAction.
    keymap.rs       Mode-dependent key-to-action resolution.
    chord.rs        Multi-key sequence state machine.

  ui/               Terminal UI rendering
    framework/      Shared UI runtime primitives
      compositor.rs Layer-based compositor that owns all UI components.
      window_manager.rs  Split-tree window layout manager for editor panes.
      component.rs  Component trait and render context.
      surface.rs    Terminal cell grid abstraction.
      cell.rs       Cell styling.
    views/          Always-on main view components
      text_view.rs  Main editor view with gutter, highlighting, cursors.
      status_bar.rs Mode, file path, cursor position display.
      notification_bar.rs
    shared/         Reused overlay helpers
      filtering.rs  Fuzzy matching and scoring functions.
    overlays/       Feature-specific overlays and transient UI
      palette/
        picker.rs   Palette state and UI glue for picker modes.
        workers.rs  Background worker functions for preview and global search.
      explorer/
        sidebar.rs  Sidebar file tree.
        popup.rs    Large tree explorer overlay.
      git/
        view.rs     Git status view.
      github/
        pr_picker.rs    GitHub PR list picker (uses gh CLI).
        issue_picker.rs GitHub issue list picker (uses gh CLI).
      editor/
        find_replace.rs
        markdown_link_hover.rs
      project/
        root_picker.rs   Absolute-path project root picker overlay.
        recent_picker.rs Recent project switcher overlay with fzf-style filter.
        save_as_popup.rs File-path save-as overlay (absolute or project-root-relative).
      command_helper.rs

  syntax/           Syntax highlighting
    highlight.rs    Tree-sitter incremental highlighting manager.
    language.rs     Language registry (Rust, JS, TS, Python, Go, C, JSON, TOML, Diff, Markdown).
    theme.rs        Color theme with presets and user overrides.
    symbol.rs       Symbol extraction for outline picker.
    indent.rs       Per-language indentation queries.

  command/          Commands and async services
    registry.rs     Built-in command registry.
    recent_projects.rs  SQLite-backed recent project metadata store.
    git.rs          Git status and diff operations.
    git_view_diff_runtime.rs  Async git diff loading for Git view.
    in_editor_diff.rs  In-editor diff text builder and diff-line jump target mapper.
    git_backend.rs  Git library (gix) wrapper.
    git_runtime.rs  Async git worker with debouncing.
    gargo_server.rs  Unified local repository browser, diff, compare, and commit server.
    lsp.rs          LSP client.
    diff_server.rs  Legacy async diff computation used by the unified server.
    gargo_preview_server.rs  Legacy repository preview routes used by the unified server.
    async_runtime.rs  Tokio runtime management.

  plugin/           Plugin system
    host.rs         Plugin host that manages all plugins.
    registry.rs     Plugin registry builder.
    types.rs        Plugin trait and event/output types.
    lsp.rs          LSP plugin implementation.
    gargo_server.rs  Unified gargo server plugin.
    diff_ui.rs      Legacy Diff UI plugin.
    gargo_preview.rs  Legacy gargo markdown preview plugin.

  io/               I/O utilities
    file_io.rs      File collection (git-aware, respects .git boundaries in nested repos).
    terminal.rs     Terminal setup and teardown.

tests/              Integration and E2E tests
```

## Startup

1. Load config from `~/.config/gargo/config.toml`.
2. Parse CLI args with clap (`--check`, `--update`, optional path).
3. If `--check` or `--update` is set, run update flow and exit without entering terminal raw mode.
4. Create an `Editor` (opens the file or creates a scratch buffer).
5. Create an `App` with the editor, config, and plugin host.
   - Git status cache starts empty and is populated asynchronously by `GitRuntime`.
   - Git index preload starts in the background at app startup (branch picker data + Git view file lists).
   - File list indexing is lazy by default and starts in the background at app startup.
   - If `[performance.file_index].mode = "eager"`, startup blocks until file collection finishes.
6. Enter raw terminal mode.
7. Run the main event loop (`App::run`).
8. Restore terminal on exit.

## Main Event Loop (app.rs)

`App::run` loops at 60 FPS (16ms frame budget):

1. Poll terminal events (keyboard, resize, paste) via crossterm.
2. Pass the event to the compositor. UI layers (palette, explorer, popups) get first chance to consume it.
3. If not consumed, resolve the key through the keymap into an `Action`.
4. Dispatch the action:
   - `CoreAction` -- text editing, cursor movement, undo/redo. Handled by `dispatch_core`.
   - `UiAction` -- open/close UI components. Handled by `compositor.apply`.
   - `AppAction` -- application coordination and I/O. Routed by `dispatch_app` into domain handlers, then executed in `dispatch_app_flat`.
5. Poll plugins for async results (LSP diagnostics, etc.).
6. Poll git runtimes for status/gutter, git index snapshots, and Git view diff updates.
7. If the document changed, trigger incremental tree-sitter parse.
8. Render all UI layers to the terminal.

## Action System (input/)

All user-facing operations are represented as one of three action enums:

- `CoreAction` -- text mutations and cursor movement (InsertChar, DeleteForward, MoveRight, Undo, Redo, search operations, multi-cursor operations, etc.).
- `UiAction` -- UI state changes (ClosePalette, CloseExplorerPopup, CloseProjectRootPopup, CloseRecentProjectPopup, CloseSaveAsPopup, ShowFindReplace, etc.).
- `AppAction` -- application-level operations grouped by domain:
  - `Buffer(BufferAction)` -- save/save-as, refresh/close, open/switch buffers.
  - `Project(ProjectAction)` -- open project pickers, change/switch project root.
  - `Workspace(WorkspaceAction)` -- palette/search/explorer/git/find-replace/diff-view flows.
  - `Window(WindowAction)` -- split/focus/close/swap window operations.
  - `Integration(IntegrationAction)` -- plugin commands, URL open, clipboard, messages.
  - `Lifecycle(LifecycleAction)` -- config and quit/cancel actions.
  - `Navigation(NavigationAction)` -- jump list, jump to location, command execution.

Key-to-action resolution lives in `input/keymap.rs`. It maps keys differently depending on the current mode (Normal, Insert, Visual) and chord state.

### Chords

Some commands use multi-key sequences. A state machine in `input/chord.rs` tracks the current chord state:

- `Space` chord: available in Normal and Visual modes. `SPC f` opens file picker, `SPC p` opens command palette, etc.
- In explorer popup (`SPC E`), deep tree indentation is collapsed with a `..` prefix when needed so filenames stay visible in narrow widths.
- `Space w` chord: split, focus, swap, and close editor windows.
- `Ctrl+X` chord: `C-x C-s` saves, `C-x C-c` quits.
- Global `Ctrl+Q`: closes the active buffer. If only a single clean scratch buffer remains and windows are split, it closes the focused window first; it quits only when that state has one window.
- `Ctrl+Left` / `Ctrl+Right` in Insert and Normal modes move by word backward/forward without creating a selection in Normal mode. `Ctrl+Up` and `Ctrl+Down` are aliases for normal cursor up/down movement.
- `Shift+Left` / `Shift+Right` in Normal and Visual modes extend selection by one character and keep the cursor on the moving edge.
- `Ctrl+Shift+Left` / `Ctrl+Shift+Right` in Normal and Visual modes extend selection by one word and keep the cursor on the moving edge.
- Entering Insert mode clears any active selection anchor.
- In Insert mode, real `Enter` keeps auto-indent behavior. Raw `LF`/`CR` character input is treated as pasted text and inserts a plain newline without auto-indent.
- `g` (goto) chord: `gg` goes to file start, `ge` to file end, `gd` goes to definition.
- In the in-editor diff buffer, `gd` opens the file location represented by the current diff line.
- In the in-editor diff buffer, `r` refreshes the diff content and jump mapping.
- `SPC g` opens a flat changed-files sidebar with git status badges (`[M]`, `[A]`, `[D]`, `[?]`, `[U]`).
- In Git view (`SPC G`), `C` opens a commit message buffer generated from git's `COMMIT_EDITMSG` template. Closing that buffer strips comments and commits when non-empty; empty/unparsable messages abort the commit.
- In Git view (`SPC G`), `Changed` and `Staged` headers are selectable. `u` on `Changed` stages all changed files, `u` on `Staged` unstages all staged files, and `u` on a file row toggles that file.
- `Q` / `q` for macro record/play; `F4` replays the last recorded/played macro.

## Core Editing (core/)

### Document

`Document` holds the state for a single open file:

- `rope: Rope` -- text content stored in a rope (ropey crate). Gives O(log n) insert/delete, which keeps large files fast.
- `cursors: Vec<usize>` -- multiple cursor byte offsets. `cursors[0]` is the primary cursor.
- `selection: Option<Selection>` -- visual mode selection for the primary cursor.
- `history: History` -- undo/redo stacks.
- `git_gutter: HashMap<usize, GitLineStatus>` -- per-line git diff status, updated asynchronously.
- File path, dirty flag, scroll offsets.

### Editor

`Editor` manages multiple documents:

- `buffers: Vec<Document>` with an `active_index`.
- Jump list for back/forward navigation (`Ctrl+O` / `Ctrl+I`).
- Search state shared across buffers.
  - Search offsets are treated as buffer-relative data. Before search navigation, stale offsets are pruned against the active rope length.
  - `TextView` also bounds-checks search offsets during highlight rendering so stale search data cannot panic Ropey.
- Macro recorder.
- LSP diagnostics storage.

### Undo/Redo

`History` uses transaction-based stacks. A transaction groups multiple edits (e.g., an entire insert-mode session) into one undo step. Each transaction records cursor positions before and after, so undo restores cursors correctly, including for multi-cursor edits.

## UI (ui/)

UI modules are split by responsibility:

- `ui/framework` contains rendering/runtime primitives and compositor orchestration.
- `ui/views` contains always-rendered document/status/message views.
- `ui/shared` contains reusable helpers that are not tied to one feature overlay.
- `ui/overlays` contains feature-specific overlays tied to one action/plugin/domain.
- shared low-level text primitives live in `crates/salad-core`:
  - `salad_core::ui::text` for width/truncation/window helpers
  - `salad_core::text::input` for `TextInput` struct (text + cursor + editing methods) and standalone input helpers
  - `salad_core::ui::file_browser` for single-name validation and case-insensitive name sorting

Layering rule:
- `ui/framework` and `ui/shared` must not depend on feature overlays.
- exception: `ui/framework/compositor.rs` is the orchestration hub and is allowed to depend on feature overlays.
- `ui/overlays/*` can depend on `ui/framework`, `ui/shared`, and `ui/views`.

### Compositor

`Compositor` owns all UI components and renders them in layers:

1. `TextView` -- main editor content (always rendered).
2. `StatusBar` -- mode, file name, cursor position (always rendered).
3. `NotificationBar` -- messages (always rendered).
4. `Explorer` -- sidebar file tree (optional).
5. Modal overlays: `Palette`, `GitView`, `PrListPicker`, `IssueListPicker`, `FindReplacePopup`, `ProjectRootPopup`, `SaveAsPopup`, `ExplorerPopup`.

When the home screen is active, TextView renders three centered lines:
- `gargo v<version>`
- absolute project root path (from `App.project_root`)
- `Press i to start editing`

Opening the in-editor diff view exits home screen mode immediately, even when the active buffer is still a scratch buffer.

Editor panes are managed by `ui/framework/window_manager.rs`. It uses a binary split tree, fixed 50/50 splits, and 1-cell divider lines. Each pane maps to a buffer id, and the focused pane controls active buffer selection.

Preview panels in split overlays (`GitView`, `PrListPicker`, `IssueListPicker`, `ExplorerPopup`) support independent scrolling:
- Keyboard: `Shift+J` / `Shift+K` (or existing overlay-specific preview scroll bindings)
- Keyboard horizontal: `Shift+H` / `Shift+L`
- Mouse wheel: scrolls preview panel by 3 lines when the split preview panel is visible
- Vertical and horizontal scroll offsets are clamped to avoid overscrolling past available content

Editor split dividers are mouse-draggable:
- left-button down on a pane divider starts a drag
- drag horizontally for vertical dividers and vertically for horizontal dividers
- releasing left-button ends the drag
- this resizes only editor pane splits (not the explorer sidebar border)

Each component implements the `Component` trait:

```rust
pub trait Component {
    fn render(&self, ctx: &RenderContext, surface: &mut Surface);
    fn cursor(&self, ctx: &RenderContext) -> Option<(u16, u16, SetCursorStyle)>;
}
```

For keyboard handling, the compositor routes events top-down. The topmost modal gets first chance to consume the event.

Markdown link completion hover is rendered as a borderless candidate list near the cursor. It consumes:
- `Tab` / `Shift+Tab` for next/previous candidate
- `Ctrl+n` / `Ctrl+p` for next/previous candidate
- `Up` / `Down` for previous/next candidate
- `Enter` to apply selection
- `Esc` to close
Hover rows use a subtle gray background so the overlay is visible against editor content.
The colors can be customized with `[theme.ui]` keys.

### Palette

The palette is a fuzzy-matching picker used for multiple purposes. It supports fuzzy scoring, preview, and git status indicators.

The unified palette (opened via SPC p or SPC f) uses input prefixes to switch modes:

- No prefix: file picker (browse/search project files)
- `>` prefix: command palette (execute editor commands)
- `@` prefix: symbol picker (in-file symbols from current document)
- `:` prefix: go to line (jump to a specific line number)

Buffer picker, jump list picker, and global search remain as standalone modes with their own keybindings.

Command palette includes `project.change_root` (`Change Project Root`) and `project.switch_recent` (`Switch to Recent Project`).
`project.change_root` opens the project root path popup.
`project.switch_recent` opens the recent project popup.
Cursor commands include `cursor.add_next_match` (`Add Cursor to Next Match`) and `cursor.add_prev_match` (`Add Cursor to Previous Match`).
Diff commands `diff.open_in_editor` and `diff.refresh_in_editor` open/refresh the in-editor diff buffer.
Git commands include `git.switch_branch` (`Git: Switch Branch`), which opens a branch picker backed by fzf-style filtering. The right preview panel shows working tree status plus recent commits for the selected branch.
Server commands include `server.start_gargo` (`Start Gargo Server`) and `server.stop_gargo` (`Stop Gargo Server`); the former `server.start_github` / `server.stop_github` ids still work as aliases. Starting the server opens the repository root at a dynamic `127.0.0.1` port. The browser UI is read-only and serves local/offline pages for code browsing, file previews, working-tree changes, branch compare, commit log, and commit diffs. Legacy visible commands for the separate diff server, compare page, and gargo preview server are no longer registered by the default plugin set.
The command list includes `Save current buffer as ...`, which opens `SaveAsPopup`. The popup accepts absolute and project-root-relative paths, and save creates missing parent directories.

Command palette includes `project.change_root` (`Change Project Root`), which opens a project root path popup.
Command palette includes `project.change_root` (`Change Project Root`) and `project.switch_recent` (`Switch to Recent Project`).
`project.change_root` opens the project root path popup.
`project.switch_recent` opens the recent project popup.
Command palette also includes `config.toggle_debug` and `config.toggle_line_numbers`.
Their labels switch dynamically between Show/Hide based on current runtime values.
Both toggles update runtime config only and do not write `~/.config/gargo/config.toml`.

Project root popup behavior:
- Input defaults to current project root absolute path.
- Supports editing keys: `Ctrl+f`, `Ctrl+b`, `Ctrl+w`, `Ctrl+k`.
- Candidate list is built from local child directories under the typed parent path.
- Candidate scoring uses fzf-style fuzzy matching on the last path segment.
- Navigation uses `Ctrl+n`, `Ctrl+p`, `Up`, `Down`.
- `Tab` completes with the selected candidate.
- `Enter` submits selected candidate when candidate selection is active; otherwise submits typed path.

Explorer popup remains file-oriented. In file mode, `Ctrl+R` changes project root to the selected directory. If a file is selected, its parent directory is used.

Recent project popup behavior:
- Candidate source is SQLite metadata under `$XDG_DATA_HOME/gargo/history.db` (fallback `~/.local/share/gargo/history.db`).
- `recent_projects` table stores project path, `last_open_at`, `last_edit_at`, `last_open_file`, and `last_edit_file`.
- Candidate filtering uses fzf-style matching.
- Navigation uses `Ctrl+n`, `Ctrl+p`, `Up`, `Down`.
- `Enter` switches project root to the selected candidate and then reopens `last_open_file` when it still exists.

## Syntax Highlighting (syntax/)

Tree-sitter is used for incremental parsing. Each buffer has its own parser and syntax tree. On edit, only the changed subtree is re-parsed. During rendering, highlight queries are run only over visible lines.

Supported languages: Rust, JavaScript, TypeScript, TSX, Python, Go, C, JSON, TOML, Markdown.

Themes are configurable in `config.toml`:

```toml
[theme]
preset = "ansi_dark"

[theme.captures]
"keyword" = { fg = "magenta", bold = true }

[theme.ui]
markdown_link_hover_bg = "dark_grey"
markdown_link_hover_selected_bg = "grey"
```

## Async Architecture

The main loop must never block. Long operations run in background threads/tasks and communicate via channels.

On quit, runtime handles signal their worker threads to shut down but do not join them. The process exit kills any still-running workers. This keeps quit instant even when a worker is mid-operation (e.g. git status on a large repo).

### Git Runtime

`GitRuntime` runs on a separate thread. The app sends commands (refresh status, update document) through a channel. The runtime executes git operations (using gix) with debouncing and sends results back (file status map, per-line gutter data). The app polls for results each frame.

`GitIndexRuntime` runs on a separate thread and preloads:
- branch picker entries (including per-branch preview text)
- Git view index snapshot (current branch + changed/staged file lists)

Startup and project-root changes trigger a non-blocking refresh so branch picker and Git view can open without synchronous git calls on the UI thread.

`GitViewDiffRuntime` runs on another thread dedicated to `SPC g` Git view diff text. Git view selection changes enqueue diff requests and optional neighbor prefetch requests. Results are delivered back by channel and rendered without blocking the UI thread.

### File Index Runtime

`FileIndexRuntime` runs file collection on a dedicated background thread. This avoids blocking startup in large non-git directory trees.

- Lazy mode starts indexing in the background during startup and after project-root changes.
- If a feature requests files before the background refresh completes, it keeps running and picks up indexed results when they arrive.
- Open palettes and markdown link completion consume the index when results arrive.

### LSP

The LSP plugin spawns language server processes and communicates over stdin/stdout. Requests and responses are async. Diagnostics arrive as plugin outputs and get stored in the editor.

- Default mode is on-demand start: servers are configured at startup, but process start is deferred until a matching file is activated or a command needs that server.
- Optional eager mode starts configured servers during plugin initialization.

### Diff Server

Diff computation runs as a Tokio task to avoid blocking rendering.

## Plugin System (plugin/)

Plugins implement the `Plugin` trait:

```rust
pub trait Plugin: Send {
    fn id(&self) -> &str;
    fn commands(&self) -> &[PluginCommandSpec];
    fn on_command(&mut self, cmd_id: &str, ctx: &PluginContext) -> Vec<PluginOutput>;
    fn on_event(&mut self, event: &PluginEvent, ctx: &PluginContext) -> Vec<PluginOutput>;
    fn poll(&mut self, ctx: &PluginContext) -> Vec<PluginOutput>;
}
```

The plugin host emits events (`BufferActivated`, `BufferChanged`, `BufferSaved`, etc.) and polls each plugin every frame. Plugins return `PluginOutput` values (messages, open file at location, set diagnostics, etc.).

Built-in plugins: LSP, Diff UI, GitHub Preview.
Diff UI and GitHub Preview web servers bind to `127.0.0.1:0` so each instance gets an OS-assigned free localhost port. Plugins use the `Started { port }` event to display and open the correct URL.

Diff UI web page header shows the absolute git project root path.
Diff UI server uses the editor project root captured when starting the server, not the process current working directory.
Diff UI page auto-refreshes `/api/status` every 2 seconds with `no-store` fetch/cache headers, with a loading guard to avoid overlapping requests.
Diff UI browser page supports per-file show/hide diff toggles, plus expand-all and collapse-all controls.
Diff UI keeps collapsed file state in session storage so it survives polling refresh and page reload in the same tab session.
Diff UI browser page includes a sticky `Go top` button in the bottom-right corner that appears after scrolling and smoothly scrolls back to the top.
GitHub Preview web pages show the absolute git project root path and current displayed file or directory path.
GitHub Preview navigation uses segmented breadcrumb pills for Root, GitHub, and path segments.
GitHub Preview page polls `/events?since=<version>` and updates in the same browser tab when active buffer path changes.
GitHub Preview plugin emits refresh events when the attached active file is saved or changed externally on disk.
GitHub Preview client stores the last seen event version in session storage so reload-triggered refresh events are not replayed in a loop.
GitHub Preview plugin still detaches follow mode when browser navigation moves to another path and reattaches on active-buffer change.

## Configuration

`~/.config/gargo/config.toml` controls:

- `show_line_number`, `tab_width`, `horizontal_scroll_margin`
- Tab characters in buffer text are rendered as visible spaces in the editor viewport and are included in cursor/horizontal-scroll column calculations.
- `[git]` -- git runtime tuning:
  - `gutter_debounce_high_priority_ms`
  - `gutter_debounce_normal_ms`
  - `git_view_diff_cache_max_entries`
  - `git_view_diff_prefetch_radius`
- `[performance]` -- startup behavior
  - `[performance.file_index] mode = "lazy" | "eager"` (`lazy`: non-blocking background indexing during startup/root-switch, `eager`: blocking indexing during startup/root-switch)
  - `[performance.lsp] start_mode = "on_demand" | "eager"`
- `[theme]` -- preset, capture overrides, and UI color overrides
- `[lsp]` -- language server definitions
- `[plugins]` -- enabled plugins

Runtime command toggles:
- `config.toggle_debug` flips `debug`
- `config.toggle_line_numbers` flips `show_line_number`
- These toggles are session-local unless the config file is edited manually.

## Key Dependencies

- ropey -- rope data structure for text buffers
- crossterm -- terminal I/O and events
- tree-sitter + language grammars -- syntax parsing
- tokio -- async runtime for background tasks
- gix -- pure-Rust git operations
- axum -- web server for diff/preview UIs
- serde, toml -- configuration
- rusqlite -- command history and recent project persistence

## Testing

- Performance tests ensure hot-path methods stay under 16ms.
- Static analysis tests scan for blocking calls in render-path files.
- Action routing tests verify keymap resolution.
- E2E tests cover editing flows, undo/redo, visual mode, paste, diff, and preview.

See `tests/README.md` for the full test catalogue and CI details.

### Render Snapshot Fixture Testing

Render snapshot tests compare the editor's terminal output against golden fixture files. This catches visual regressions without a real terminal.

How it works:

1. Tests create an `Editor`, render through a `Compositor` into an in-memory `Vec<u8>`.
2. `tests/support/render_snapshot.rs` parses the ANSI output into a 2D character grid.
3. The grid is compared against a fixture file in `tests/fixtures/render/`.

Files:

- `tests/support/render_snapshot.rs` -- ANSI parser and fixture helpers.
- `tests/fixtures/render/*.txt` -- golden files.
- `tests/render_snapshot_e2e.rs` -- basic snapshot tests.
- `tests/resize_e2e.rs` -- resize snapshot tests.

To generate or update fixtures:

```bash
UPDATE_RENDER_FIXTURES=1 cargo test --test render_snapshot_e2e
UPDATE_RENDER_FIXTURES=1 cargo test --test resize_e2e
```

This writes actual output to fixture files instead of asserting. Review the generated files, then commit them.

Adding a new snapshot test:

```rust
use gargo::core::editor::Editor;
mod support;

#[test]
fn my_scenario_matches_fixture() {
    let mut editor = Editor::new();
    editor.active_buffer_mut().insert_text("hello\nworld\n");
    support::render_snapshot::assert_render_matches_fixture(
        "my_scenario",  // -> tests/fixtures/render/my_scenario.txt
        &editor,
        50, 8,
    );
}
```

For multi-render scenarios (e.g., resize), use `apply_ansi_to_screen` to apply ANSI output onto a screen that already has content from a previous render. See `tests/resize_e2e.rs` for examples.

## How to Add Things

### New language

1. Add the tree-sitter grammar crate to `Cargo.toml`.
2. Register it in `LanguageRegistry::new` in `syntax/language.rs` with file extensions and queries.
3. If the language is synthetic (for example in-editor diff), add any extra line-based overlay in `ui/views/text_view.rs` and map captures in `syntax/theme.rs`.

### New keybinding

1. Add to the `resolve` function in `input/keymap.rs`.
2. Add a corresponding action variant if needed (`CoreAction` or the correct `AppAction` sub-enum).
3. Handle it in the appropriate dispatcher:
   - `dispatch_core` for `CoreAction`
   - `dispatch_app_*` domain router (or `dispatch_app_flat`) for `AppAction`

### New UI component

1. Choose target group by responsibility:
   - always-on view: `src/ui/views/`
   - reusable helper: `src/ui/shared/`
   - feature-specific overlay: `src/ui/overlays/<domain>/`
2. Implement `Component` when needed and wire it from `src/ui/framework/compositor.rs`.
3. Keep dependencies one-way (`framework/shared` do not depend on feature overlays).

### New plugin

1. Implement `Plugin` in a new file under `src/plugin/`.
2. Register it in `build_plugin_host` in `plugin/registry.rs`.
3. Handle events and return outputs as needed.

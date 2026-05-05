use std::io::{self, Write};

use crossterm::{
    cursor::{self, MoveTo, SetCursorStyle},
    event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind},
    queue,
    terminal::{self, ClearType},
};

use crate::command::registry::CommandRegistry;
use crate::config::Config;
use crate::core::buffer::BufferId;
use crate::input::action::{
    Action, AppAction, CoreAction, IntegrationAction, UiAction, WindowDirection, WindowSplitAxis,
    WorkspaceAction,
};
use crate::input::chord::KeyState;
use crate::syntax::language::LanguageRegistry;
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::{Component, EventResult, RenderContext};
use crate::ui::framework::surface::Surface;
use crate::ui::framework::window_manager::{
    Direction, Divider, DividerOrientation, Layout, PaneRect, SplitAxis, WindowId, WindowManager,
};
use crate::ui::overlays::command_helper::CommandHelper;
use crate::ui::overlays::editor::find_replace::FindReplacePopup;
use crate::ui::overlays::editor::markdown_link_hover::{HoverKeyResult, MarkdownLinkHover};
use crate::ui::overlays::explorer::popup::ExplorerPopup;
use crate::ui::overlays::explorer::sidebar::Explorer;
use crate::ui::overlays::git::commit_log::CommitLogView;
use crate::ui::overlays::git::view::GitView;
use crate::ui::overlays::github::issue_picker::IssueListPicker;
use crate::ui::overlays::github::pr_picker::PrListPicker;
use crate::ui::overlays::palette::Palette;
use crate::ui::overlays::project::recent_picker::RecentProjectPopup;
use crate::ui::overlays::project::root_picker::ProjectRootPopup;
use crate::ui::overlays::project::save_as_popup::SaveAsPopup;
use crate::ui::text::display_width;
use crate::ui::text_input::TextInput;
use crate::ui::views::notification_bar::NotificationBar;
use crate::ui::views::status_bar::StatusBar;
use crate::ui::views::text_view::TextView;

mod actions;
mod input;
mod overlays;
mod rendering;
mod search_bar;
mod windows;

#[cfg(test)]
mod tests;

pub struct SearchBar {
    pub input: TextInput,
    pub saved_cursor: usize,
    pub saved_scroll: usize,
    pub saved_horizontal_scroll: usize,
}

impl SearchBar {
    /// Insert text at the current cursor position (for IME/paste support).
    pub fn insert_text(&mut self, text: &str) {
        self.input.insert_text(text);
    }
}

#[derive(Debug, Clone, Copy)]
struct MouseDividerDragState {
    primary_window_id: WindowId,
    secondary_window_id: WindowId,
    orientation: DividerOrientation,
    last_col: u16,
    last_row: u16,
}

pub struct Compositor {
    text_view: TextView,
    status_bar: StatusBar,
    notification_bar: NotificationBar,
    palette: Option<Palette>,
    git_view: Option<GitView>,
    commit_log: Option<CommitLogView>,
    pr_list_picker: Option<PrListPicker>,
    issue_list_picker: Option<IssueListPicker>,
    explorer_popup: Option<ExplorerPopup>,
    project_root_popup: Option<ProjectRootPopup>,
    recent_project_popup: Option<RecentProjectPopup>,
    save_as_popup: Option<SaveAsPopup>,
    find_replace_popup: Option<FindReplacePopup>,
    markdown_link_hover: Option<MarkdownLinkHover>,
    search_bar: Option<SearchBar>,
    explorer: Option<Explorer>,
    command_helper: Option<CommandHelper>,
    mouse_drag: Option<MouseDividerDragState>,
    /// Buffer id currently captured for a drag-to-select gesture. Set on left
    /// mouse-down over a buffer pane; cleared on mouse-up. While `Some`, drag
    /// motion events are forwarded to the dispatcher as `BufferDrag` actions.
    text_drag: Option<BufferId>,
    window_manager: WindowManager,
    current: Surface,
    previous: Surface,
    displayed_image: Option<DisplayedImage>,
}

#[derive(Debug, Clone)]
struct DisplayedImage {
    key: std::path::PathBuf,
}

impl Default for Compositor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compositor {
    pub fn new() -> Self {
        Self {
            text_view: TextView::new(),
            status_bar: StatusBar::new(),
            notification_bar: NotificationBar::new(),
            palette: None,
            git_view: None,
            commit_log: None,
            pr_list_picker: None,
            issue_list_picker: None,
            explorer_popup: None,
            project_root_popup: None,
            recent_project_popup: None,
            save_as_popup: None,
            find_replace_popup: None,
            markdown_link_hover: None,
            search_bar: None,
            explorer: None,
            command_helper: None,
            mouse_drag: None,
            text_drag: None,
            window_manager: WindowManager::new(1),
            current: Surface::new(0, 0),
            previous: Surface::new(0, 0),
            displayed_image: None,
        }
    }

    fn editor_rect_for_dims(&self, cols: usize, rows: usize) -> Option<PaneRect> {
        let height = rows.saturating_sub(2);
        if height == 0 {
            return None;
        }
        let (x, width) = self
            .explorer_layout(cols)
            .map(|(_, _, editor_x, editor_w)| (editor_x, editor_w))
            .unwrap_or((0, cols));
        if width == 0 {
            return None;
        }
        Some(PaneRect {
            x,
            y: 0,
            width,
            height,
        })
    }

    pub fn focused_pane_rect(&self, cols: usize, rows: usize) -> Option<PaneRect> {
        let area = self.editor_rect_for_dims(cols, rows)?;
        self.window_manager.focused_pane(area).map(|pane| pane.rect)
    }

    /// Returns (explorer_width, border_col, editor_x, editor_width) if explorer is open.
    /// In split mode (cols >= 80): explorer gets 30 cols, border at col 30, editor starts at 31.
    /// In fullscreen mode (cols < 80): explorer takes full width, no editor.
    pub fn explorer_layout(&self, cols: usize) -> Option<(usize, usize, usize, usize)> {
        self.explorer.as_ref()?;
        if cols >= 80 {
            let ew = 30;
            let border_col = ew;
            let editor_x = ew + 1;
            let editor_w = cols.saturating_sub(editor_x);
            Some((ew, border_col, editor_x, editor_w))
        } else {
            // Fullscreen: explorer takes all cols, no editor visible
            Some((cols, 0, 0, 0))
        }
    }
}

fn map_direction(direction: WindowDirection) -> Direction {
    match direction {
        WindowDirection::Left => Direction::Left,
        WindowDirection::Down => Direction::Down,
        WindowDirection::Up => Direction::Up,
        WindowDirection::Right => Direction::Right,
    }
}

fn draw_diff(prev: &Surface, curr: &Surface, stdout: &mut impl Write) -> io::Result<()> {
    crate::ui::diff::draw_diff(prev, curr, stdout)
}

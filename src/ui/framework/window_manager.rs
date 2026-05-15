use crate::ui::framework::layout as engine;

pub type BufferId = usize;
pub type WindowId = engine::WindowId;

pub use engine::{Direction, Divider, DividerOrientation, PaneRect, SplitAxis};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneLayout {
    pub window_id: WindowId,
    pub buffer_id: BufferId,
    pub rect: PaneRect,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Layout {
    pub panes: Vec<PaneLayout>,
    pub dividers: Vec<Divider>,
}

pub struct WindowManager {
    core: engine::WindowTree<BufferId>,
}

impl Default for WindowManager {
    fn default() -> Self {
        Self::new(1)
    }
}

impl WindowManager {
    pub fn new(initial_buffer_id: BufferId) -> Self {
        Self {
            core: engine::WindowTree::new(initial_buffer_id),
        }
    }

    pub fn window_count(&self) -> usize {
        self.core.window_count()
    }

    pub fn focused_buffer_id(&self) -> BufferId {
        self.core.focused_item_id().unwrap_or_default()
    }

    pub fn focused_window_id(&self) -> WindowId {
        self.core.focused_window_id()
    }

    pub fn set_focused_buffer(&mut self, buffer_id: BufferId) {
        self.core.set_focused_item(buffer_id);
    }

    pub fn replace_buffer_refs(&mut self, old_buffer_id: BufferId, new_buffer_id: BufferId) {
        self.core.replace_item_refs(old_buffer_id, new_buffer_id);
    }

    pub fn split_focused(&mut self, axis: SplitAxis, new_buffer_id: BufferId) {
        self.core.split_focused(axis, new_buffer_id);
    }

    pub fn window_ids_by_creation(&self) -> Vec<WindowId> {
        self.core.window_ids_by_creation()
    }

    pub fn focus_window_id(&mut self, window_id: WindowId) -> Result<(), String> {
        self.core
            .focus_window_id(window_id)
            .map_err(map_error_for_gargo)
    }

    pub fn layout(&self, area: PaneRect) -> Layout {
        Layout::from(self.core.layout(area))
    }

    pub fn focused_pane(&self, area: PaneRect) -> Option<PaneLayout> {
        self.core.focused_pane(area).map(PaneLayout::from)
    }

    pub fn focus_direction(&mut self, direction: Direction, area: PaneRect) -> Result<(), String> {
        self.core
            .focus_direction(direction, area)
            .map_err(map_error_for_gargo)
    }

    pub fn focus_next(&mut self, area: PaneRect) -> Result<(), String> {
        self.core
            .focus_next_by_geometry(area)
            .map_err(map_error_for_gargo)
    }

    pub fn swap_direction(&mut self, direction: Direction, area: PaneRect) -> Result<(), String> {
        self.core
            .swap_direction(direction, area)
            .map_err(map_error_for_gargo)
    }

    pub fn close_focused(&mut self) -> Result<(), String> {
        self.core
            .close_focused()
            .map(|_| ())
            .map_err(map_error_for_gargo)
    }

    pub fn close_others(&mut self) {
        self.core.close_others();
    }

    pub fn resize_window(
        &mut self,
        window_id: WindowId,
        direction: Direction,
        amount: u16,
    ) -> Result<(), String> {
        self.core
            .resize_window(window_id, direction, amount)
            .map_err(map_error_for_gargo)
    }

    pub fn resize_between_windows(
        &mut self,
        primary_window: WindowId,
        secondary_window: WindowId,
        orientation: DividerOrientation,
        delta: i16,
    ) -> Result<(), String> {
        self.core
            .resize_between_windows(primary_window, secondary_window, orientation, delta)
            .map_err(map_error_for_gargo)
    }
}

impl From<engine::PaneLayout<BufferId>> for PaneLayout {
    fn from(value: engine::PaneLayout<BufferId>) -> Self {
        Self {
            window_id: value.window_id,
            buffer_id: value.item_id,
            rect: value.rect,
        }
    }
}

impl From<engine::Layout<BufferId>> for Layout {
    fn from(value: engine::Layout<BufferId>) -> Self {
        Self {
            panes: value.panes.into_iter().map(PaneLayout::from).collect(),
            dividers: value.dividers,
        }
    }
}

fn map_error_for_gargo(message: String) -> String {
    match message.as_str() {
        "No focused pane" => "No focused window".to_string(),
        "No pane in that direction" => "No window in that direction".to_string(),
        "Cannot close the last pane" => "Cannot close the last window".to_string(),
        "Focused pane not found" => "Focused window not found".to_string(),
        "No panes available" => "No windows available".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> PaneRect {
        PaneRect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        }
    }

    #[test]
    fn vertical_split_places_new_window_right_and_focuses_it() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        let layout = wm.layout(rect());
        assert_eq!(layout.panes.len(), 2);
        assert_eq!(wm.focused_buffer_id(), 2);
        assert!(
            layout
                .panes
                .iter()
                .any(|pane| pane.buffer_id == 2 && pane.rect.x > 0)
        );
    }

    #[test]
    fn horizontal_split_places_new_window_below_and_focuses_it() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Horizontal, 2);
        let layout = wm.layout(rect());
        assert_eq!(layout.panes.len(), 2);
        assert_eq!(wm.focused_buffer_id(), 2);
        assert!(
            layout
                .panes
                .iter()
                .any(|pane| pane.buffer_id == 2 && pane.rect.y > 0)
        );
    }

    #[test]
    fn focus_direction_moves_to_adjacent_window() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        assert_eq!(wm.focused_buffer_id(), 2);
        wm.focus_direction(Direction::Left, rect()).unwrap();
        assert_eq!(wm.focused_buffer_id(), 1);
    }

    #[test]
    fn focus_next_cycles_windows_top_left_to_bottom_right() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        wm.split_focused(SplitAxis::Horizontal, 3);
        assert_eq!(wm.focused_buffer_id(), 3);
        wm.focus_next(rect()).unwrap();
        assert_eq!(wm.focused_buffer_id(), 1);
        wm.focus_next(rect()).unwrap();
        assert_eq!(wm.focused_buffer_id(), 2);
    }

    #[test]
    fn swap_direction_exchanges_buffer_assignments() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        wm.swap_direction(Direction::Left, rect()).unwrap();
        assert_eq!(wm.focused_buffer_id(), 1);
        wm.focus_direction(Direction::Left, rect()).unwrap();
        assert_eq!(wm.focused_buffer_id(), 2);
    }

    #[test]
    fn close_focused_collapses_tree() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        wm.close_focused().unwrap();
        assert_eq!(wm.window_count(), 1);
        assert_eq!(wm.focused_buffer_id(), 1);
    }

    #[test]
    fn close_others_keeps_only_focused_window() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        wm.split_focused(SplitAxis::Horizontal, 3);
        wm.close_others();
        assert_eq!(wm.window_count(), 1);
        assert_eq!(wm.focused_buffer_id(), 3);
    }

    #[test]
    fn edge_direction_operations_return_error() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);
        assert!(wm.focus_direction(Direction::Right, rect()).is_err());
        assert!(wm.swap_direction(Direction::Right, rect()).is_err());
    }

    #[test]
    fn resize_window_changes_target_layout_without_focus_change() {
        let mut wm = WindowManager::new(1);
        wm.split_focused(SplitAxis::Vertical, 2);

        let focused_before = wm.focused_window_id();
        let before = wm.layout(rect());
        let left = before
            .panes
            .iter()
            .find(|pane| pane.buffer_id == 1)
            .expect("left pane");

        wm.resize_window(left.window_id, Direction::Left, 10)
            .expect("resize target window");

        let after = wm.layout(rect());
        let left_after = after
            .panes
            .iter()
            .find(|pane| pane.buffer_id == 1)
            .expect("left pane after");
        assert!(left_after.rect.width > left.rect.width);
        assert_eq!(wm.focused_window_id(), focused_before);
    }

    #[test]
    fn resize_window_missing_window_returns_error() {
        let mut wm = WindowManager::new(1);
        let err = wm
            .resize_window(999, Direction::Left, 5)
            .expect_err("missing window should fail");
        assert_eq!(err, "Window not found");
    }
}

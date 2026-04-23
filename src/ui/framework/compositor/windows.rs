use super::*;

impl Compositor {
    pub fn window_count(&self) -> usize {
        self.window_manager.window_count()
    }

    pub fn focused_buffer_id(&self) -> BufferId {
        self.window_manager.focused_buffer_id()
    }

    pub fn set_focused_buffer(&mut self, buffer_id: BufferId) {
        self.window_manager.set_focused_buffer(buffer_id);
    }

    pub fn replace_window_buffer_refs(&mut self, old_buffer_id: BufferId, new_buffer_id: BufferId) {
        self.window_manager
            .replace_buffer_refs(old_buffer_id, new_buffer_id);
    }

    pub fn split_focused_window(
        &mut self,
        axis: WindowSplitAxis,
        new_buffer_id: BufferId,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        let area = self
            .editor_rect_for_dims(cols, rows)
            .ok_or_else(|| "No editor area available".to_string())?;
        let focused = self
            .window_manager
            .focused_pane(area)
            .ok_or_else(|| "No focused window".to_string())?;
        match axis {
            WindowSplitAxis::Vertical => {
                if focused.rect.width < 3 {
                    return Err("Window too narrow to split".to_string());
                }
                self.window_manager
                    .split_focused(SplitAxis::Vertical, new_buffer_id);
            }
            WindowSplitAxis::Horizontal => {
                if focused.rect.height < 3 {
                    return Err("Window too short to split".to_string());
                }
                self.window_manager
                    .split_focused(SplitAxis::Horizontal, new_buffer_id);
            }
        }
        Ok(())
    }

    pub fn focus_window_direction(
        &mut self,
        direction: WindowDirection,
        cols: usize,
        rows: usize,
    ) -> Result<BufferId, String> {
        let area = self
            .editor_rect_for_dims(cols, rows)
            .ok_or_else(|| "No editor area available".to_string())?;
        self.window_manager
            .focus_direction(map_direction(direction), area)?;
        Ok(self.window_manager.focused_buffer_id())
    }

    pub fn focus_next_window(&mut self, cols: usize, rows: usize) -> Result<BufferId, String> {
        let area = self
            .editor_rect_for_dims(cols, rows)
            .ok_or_else(|| "No editor area available".to_string())?;
        self.window_manager.focus_next(area)?;
        Ok(self.window_manager.focused_buffer_id())
    }

    pub fn swap_window_direction(
        &mut self,
        direction: WindowDirection,
        cols: usize,
        rows: usize,
    ) -> Result<(), String> {
        let area = self
            .editor_rect_for_dims(cols, rows)
            .ok_or_else(|| "No editor area available".to_string())?;
        self.window_manager
            .swap_direction(map_direction(direction), area)
    }

    pub fn close_focused_window(&mut self) -> Result<BufferId, String> {
        self.window_manager.close_focused()?;
        Ok(self.window_manager.focused_buffer_id())
    }

    pub fn close_other_windows(&mut self) -> BufferId {
        self.window_manager.close_others();
        self.window_manager.focused_buffer_id()
    }

    /// Returns the pane containing the given terminal coordinates, if any.
    pub fn pane_at(
        &self,
        col: u16,
        row: u16,
        cols: usize,
        rows: usize,
    ) -> Option<crate::ui::framework::window_manager::PaneLayout> {
        let area = self.editor_rect_for_dims(cols, rows)?;
        let col = usize::from(col);
        let row = usize::from(row);
        self.window_manager.layout(area).panes.into_iter().find(|p| {
            col >= p.rect.x
                && col < p.rect.x + p.rect.width
                && row >= p.rect.y
                && row < p.rect.y + p.rect.height
        })
    }
}

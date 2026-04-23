use super::*;

impl Compositor {
    /// Handle key event. If palette or search bar is active, it gets priority.
    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
        key_state: &KeyState,
    ) -> EventResult {
        if let Some(ref mut palette) = self.palette {
            return palette.handle_key_event(key, registry, lang_registry, config);
        }
        if let Some(ref mut find_replace_popup) = self.find_replace_popup {
            return find_replace_popup.handle_key(key);
        }
        if let Some(ref mut git_view) = self.git_view {
            return git_view.handle_key(key);
        }
        if let Some(ref mut commit_log) = self.commit_log {
            return commit_log.handle_key(key);
        }
        if let Some(ref mut pr_list_picker) = self.pr_list_picker {
            return pr_list_picker.handle_key(key);
        }
        if let Some(ref mut issue_list_picker) = self.issue_list_picker {
            return issue_list_picker.handle_key(key);
        }
        if let Some(ref mut project_root_popup) = self.project_root_popup {
            return project_root_popup.handle_key(key);
        }
        if let Some(ref mut recent_project_popup) = self.recent_project_popup {
            return recent_project_popup.handle_key(key);
        }
        if let Some(ref mut save_as_popup) = self.save_as_popup {
            return save_as_popup.handle_key(key);
        }
        if let Some(ref mut explorer_popup) = self.explorer_popup {
            return explorer_popup.handle_key(key);
        }
        if let Some(ref mut bar) = self.search_bar {
            if key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                return match key.code {
                    KeyCode::Char('q') => {
                        let saved_cursor = bar.saved_cursor;
                        let saved_scroll = bar.saved_scroll;
                        let saved_horizontal_scroll = bar.saved_horizontal_scroll;
                        EventResult::Action(Action::App(AppAction::Workspace(
                            WorkspaceAction::SearchCancel {
                                saved_cursor,
                                saved_scroll,
                                saved_horizontal_scroll,
                            },
                        )))
                    }
                    KeyCode::Char('p') => EventResult::Action(Action::App(AppAction::Workspace(
                        WorkspaceAction::SearchHistoryPrev,
                    ))),
                    KeyCode::Char('n') => EventResult::Action(Action::App(AppAction::Workspace(
                        WorkspaceAction::SearchHistoryNext,
                    ))),
                    KeyCode::Char('a') => {
                        bar.input.move_start();
                        EventResult::Consumed
                    }
                    KeyCode::Char('e') => {
                        bar.input.move_end();
                        EventResult::Consumed
                    }
                    KeyCode::Char('f') => {
                        bar.input.move_right();
                        EventResult::Consumed
                    }
                    KeyCode::Char('b') => {
                        bar.input.move_left();
                        EventResult::Consumed
                    }
                    KeyCode::Char('k') => {
                        let _ = bar.input.delete_to_end();
                        EventResult::Action(Action::Core(CoreAction::SearchUpdate(
                            bar.input.text.clone(),
                        )))
                    }
                    KeyCode::Char('w') => {
                        let _ = bar.input.delete_prev_word();
                        EventResult::Action(Action::Core(CoreAction::SearchUpdate(
                            bar.input.text.clone(),
                        )))
                    }
                    _ => EventResult::Consumed,
                };
            }
            return match key.code {
                KeyCode::Up => EventResult::Action(Action::App(AppAction::Workspace(
                    WorkspaceAction::SearchHistoryPrev,
                ))),
                KeyCode::Down => EventResult::Action(Action::App(AppAction::Workspace(
                    WorkspaceAction::SearchHistoryNext,
                ))),
                KeyCode::Left => {
                    bar.input.move_left();
                    EventResult::Consumed
                }
                KeyCode::Right => {
                    bar.input.move_right();
                    EventResult::Consumed
                }
                KeyCode::Esc => {
                    let saved_cursor = bar.saved_cursor;
                    let saved_scroll = bar.saved_scroll;
                    let saved_horizontal_scroll = bar.saved_horizontal_scroll;
                    EventResult::Action(Action::App(AppAction::Workspace(
                        WorkspaceAction::SearchCancel {
                            saved_cursor,
                            saved_scroll,
                            saved_horizontal_scroll,
                        },
                    )))
                }
                KeyCode::Enter => EventResult::Action(Action::App(AppAction::Workspace(
                    WorkspaceAction::SearchConfirm,
                ))),
                KeyCode::Backspace => {
                    let _ = bar.input.backspace();
                    EventResult::Action(Action::Core(CoreAction::SearchUpdate(
                        bar.input.text.clone(),
                    )))
                }
                KeyCode::Char(c) => {
                    bar.input.insert_char(c);
                    EventResult::Action(Action::Core(CoreAction::SearchUpdate(
                        bar.input.text.clone(),
                    )))
                }
                _ => EventResult::Consumed,
            };
        }
        if let Some(ref mut hover) = self.markdown_link_hover {
            match hover.handle_key(key) {
                HoverKeyResult::Ignored => {}
                HoverKeyResult::Consumed => return EventResult::Consumed,
                HoverKeyResult::Close => {
                    self.markdown_link_hover = None;
                    return EventResult::Consumed;
                }
                HoverKeyResult::Apply(candidate) => {
                    return EventResult::Action(Action::App(AppAction::Integration(
                        IntegrationAction::ApplyMarkdownLinkCompletion { candidate },
                    )));
                }
            }
        }
        if let Some(ref mut explorer) = self.explorer {
            let result = explorer.handle_key(key, key_state);
            if !matches!(result, EventResult::Ignored) {
                return result;
            }
        }
        EventResult::Ignored
    }

    fn event_surface_size(&self) -> (usize, usize) {
        let cols = self.current.width.max(self.previous.width);
        let rows = self.current.height.max(self.previous.height);
        if cols == 0 || rows == 0 {
            (80, 24)
        } else {
            (cols, rows)
        }
    }

    pub(super) fn window_layout_for_event_dims(&self, cols: usize, rows: usize) -> Option<Layout> {
        let area = self.editor_rect_for_dims(cols, rows)?;
        Some(self.window_manager.layout(area))
    }

    fn has_modal_mouse_overlay(&self) -> bool {
        self.palette.is_some()
            || self.git_view.is_some()
            || self.pr_list_picker.is_some()
            || self.issue_list_picker.is_some()
            || self.explorer_popup.is_some()
            || self.project_root_popup.is_some()
            || self.recent_project_popup.is_some()
            || self.save_as_popup.is_some()
            || self.find_replace_popup.is_some()
            || self.search_bar.is_some()
    }

    fn mouse_divider_at(layout: &Layout, col: u16, row: u16) -> Option<Divider> {
        let col = usize::from(col);
        let row = usize::from(row);
        layout
            .dividers
            .iter()
            .copied()
            .find(|divider| match divider.orientation {
                DividerOrientation::Vertical => {
                    col == divider.x && row >= divider.y && row < divider.y + divider.len
                }
                DividerOrientation::Horizontal => {
                    row == divider.y && col >= divider.x && col < divider.x + divider.len
                }
            })
    }

    pub(super) fn mouse_windows_for_divider(
        layout: &Layout,
        divider: Divider,
        mouse_col: u16,
        mouse_row: u16,
    ) -> Option<(WindowId, WindowId)> {
        let mouse_col = usize::from(mouse_col);
        let mouse_row = usize::from(mouse_row);
        match divider.orientation {
            DividerOrientation::Vertical => {
                let primary = layout.panes.iter().find(|pane| {
                    pane.rect.x + pane.rect.width == divider.x
                        && mouse_row >= pane.rect.y
                        && mouse_row < pane.rect.y + pane.rect.height
                })?;
                let secondary = layout.panes.iter().find(|pane| {
                    pane.rect.x == divider.x + 1
                        && mouse_row >= pane.rect.y
                        && mouse_row < pane.rect.y + pane.rect.height
                })?;
                Some((primary.window_id, secondary.window_id))
            }
            DividerOrientation::Horizontal => {
                let primary = layout.panes.iter().find(|pane| {
                    pane.rect.y + pane.rect.height == divider.y
                        && mouse_col >= pane.rect.x
                        && mouse_col < pane.rect.x + pane.rect.width
                })?;
                let secondary = layout.panes.iter().find(|pane| {
                    pane.rect.y == divider.y + 1
                        && mouse_col >= pane.rect.x
                        && mouse_col < pane.rect.x + pane.rect.width
                })?;
                Some((primary.window_id, secondary.window_id))
            }
        }
    }

    pub fn handle_mouse(&mut self, mouse: &MouseEvent) -> EventResult {
        let (cols, rows) = self.event_surface_size();
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.mouse_drag = None;
                if let Some(ref mut git_view) = self.git_view {
                    let result = git_view.handle_mouse_scroll(mouse.kind, cols, rows);
                    if !matches!(result, EventResult::Ignored) {
                        return result;
                    }
                }

                if let Some(ref mut commit_log) = self.commit_log {
                    commit_log.handle_mouse_scroll(mouse.kind, mouse.column, mouse.row, cols, rows);
                    return EventResult::Consumed;
                }

                if let Some(ref mut pr_list_picker) = self.pr_list_picker {
                    let result = pr_list_picker.handle_mouse_scroll(mouse.kind, cols, rows);
                    if !matches!(result, EventResult::Ignored) {
                        return result;
                    }
                }

                if let Some(ref mut issue_list_picker) = self.issue_list_picker {
                    let result = issue_list_picker.handle_mouse_scroll(mouse.kind, cols, rows);
                    if !matches!(result, EventResult::Ignored) {
                        return result;
                    }
                }

                if let Some(ref mut explorer_popup) = self.explorer_popup {
                    let result = explorer_popup.handle_mouse_scroll(mouse.kind, cols, rows);
                    if !matches!(result, EventResult::Ignored) {
                        return result;
                    }
                }

                EventResult::Ignored
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_drag = None;
                if self.has_modal_mouse_overlay() {
                    return EventResult::Ignored;
                }
                let Some(layout) = self.window_layout_for_event_dims(cols, rows) else {
                    return EventResult::Ignored;
                };
                if let Some(divider) = Self::mouse_divider_at(&layout, mouse.column, mouse.row) {
                    if let Some((primary_window_id, secondary_window_id)) =
                        Self::mouse_windows_for_divider(&layout, divider, mouse.column, mouse.row)
                    {
                        self.mouse_drag = Some(MouseDividerDragState {
                            primary_window_id,
                            secondary_window_id,
                            orientation: divider.orientation,
                            last_col: mouse.column,
                            last_row: mouse.row,
                        });
                        return EventResult::Consumed;
                    }
                    return EventResult::Ignored;
                }

                let col = usize::from(mouse.column);
                let row = usize::from(mouse.row);
                let pane = layout.panes.iter().find(|p| {
                    col >= p.rect.x
                        && col < p.rect.x + p.rect.width
                        && row >= p.rect.y
                        && row < p.rect.y + p.rect.height
                });
                match pane {
                    Some(pane) => EventResult::Action(Action::BufferClick {
                        buffer_id: pane.buffer_id,
                        screen_col: mouse.column,
                        screen_row: mouse.row,
                    }),
                    None => EventResult::Ignored,
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.has_modal_mouse_overlay() {
                    self.mouse_drag = None;
                    return EventResult::Ignored;
                }

                let Some(mut drag) = self.mouse_drag.take() else {
                    return EventResult::Ignored;
                };

                let delta_col = i32::from(mouse.column) - i32::from(drag.last_col);
                let delta_row = i32::from(mouse.row) - i32::from(drag.last_row);
                let delta = match drag.orientation {
                    DividerOrientation::Vertical => {
                        if delta_col == 0 {
                            self.mouse_drag = Some(drag);
                            return EventResult::Consumed;
                        }
                        delta_col as i16
                    }
                    DividerOrientation::Horizontal => {
                        if delta_row == 0 {
                            self.mouse_drag = Some(drag);
                            return EventResult::Consumed;
                        }
                        delta_row as i16
                    }
                };

                // Keep drag state even when resize is clamped/no-op so the same
                // gesture can recover as soon as pointer motion reverses.
                let _ = self.window_manager.resize_between_windows(
                    drag.primary_window_id,
                    drag.secondary_window_id,
                    drag.orientation,
                    delta,
                );
                drag.last_col = mouse.column;
                drag.last_row = mouse.row;
                self.mouse_drag = Some(drag);
                EventResult::Consumed
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.mouse_drag.take().is_some() {
                    EventResult::Consumed
                } else {
                    EventResult::Ignored
                }
            }
            _ => EventResult::Ignored,
        }
    }
}

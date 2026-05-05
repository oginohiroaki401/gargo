use super::*;

impl Compositor {
    pub fn render(&mut self, ctx: &RenderContext, stdout: &mut impl Write) -> io::Result<()> {
        if ctx
            .editor
            .buffer_by_id(self.window_manager.focused_buffer_id())
            .is_none()
        {
            self.window_manager
                .set_focused_buffer(ctx.editor.active_buffer().id);
        }

        let cols = ctx.cols;
        let rows = ctx.rows;

        // Resize buffers if terminal size changed
        if self.current.width != cols || self.current.height != rows {
            self.current.resize(cols, rows);
            self.previous.resize(cols, rows);
            // Clear terminal so stale content from the old layout doesn't persist.
            // After clear, the terminal screen is all blank, matching the all-default
            // `previous` buffer — so cells the diff skips are already blank on screen.
            queue!(stdout, terminal::Clear(ClearType::All))?;
            if self.displayed_image.is_some() {
                let _ = crate::ui::image::clear_kitty_images(stdout);
                self.displayed_image = None;
            }
        }

        // Clear current buffer
        self.current.reset();

        let layout = self.explorer_layout(cols);
        let is_fullscreen_explorer = layout.is_some() && cols < 80;
        let preview_active = self
            .explorer
            .as_ref()
            .map(|e| e.preview_mode_active())
            .unwrap_or(false);

        // Render explorer if present
        if let (Some((ew, _border_col, _editor_x, _editor_w)), Some(explorer)) =
            (layout, &mut self.explorer)
        {
            let explorer_height = rows.saturating_sub(2); // stop before status bar
            explorer.render(&mut self.current, 0, ew, explorer_height);

            // Draw border column in split mode
            if cols >= 80 {
                let border_col = ew;
                let border_style = CellStyle {
                    dim: true,
                    ..CellStyle::default()
                };
                for r in 0..explorer_height {
                    self.current
                        .put_str(border_col, r, "\u{2502}", &border_style);
                }
            }
        }

        // Render the editor area: either the file/dir preview (when the sidebar
        // has preview mode on) or the normal window panes. Skip entirely when
        // the explorer is fullscreen (no editor area exists).
        let mut explorer_image_request: Option<crate::ui::image::ImageRenderRequest> = None;
        if !is_fullscreen_explorer {
            if preview_active
                && let (Some((_, _, editor_x, editor_w)), Some(explorer)) =
                    (layout, &mut self.explorer)
                && editor_w > 0
            {
                let editor_h = rows.saturating_sub(2);
                explorer.render_preview(&mut self.current, editor_x, 0, editor_w, editor_h, ctx.theme);
                explorer_image_request = explorer.take_pending_image_request();
            } else {
                self.render_windows(ctx);
            }
        }

        // Status bar always renders full width
        self.status_bar.render(ctx, &mut self.current);

        // Add notification bar below status bar
        self.notification_bar.render(ctx, &mut self.current);

        // Render command helper if active (after notification_bar, before overlays)
        if let Some(ref helper) = self.command_helper {
            helper.render_overlay(&mut self.current, cols, rows, ctx.theme);
        }

        // Search bar overlay on status row
        if let Some(ref bar) = self.search_bar {
            let status_row = rows.saturating_sub(1);
            let prompt = format!("/{}", bar.input.text);
            let reverse_style = CellStyle {
                reverse: true,
                ..CellStyle::default()
            };
            // Clear the status row and draw search prompt
            self.current
                .fill_region(0, status_row, cols, ' ', &reverse_style);
            self.current.put_str(0, status_row, &prompt, &reverse_style);

            // Draw diff between previous and current
            draw_diff(&self.previous, &self.current, stdout)?;

            // Position cursor at bar.input.cursor within the input
            let before_cursor = &bar.input.text[..bar.input.byte_index_at_cursor()];
            let cursor_x = (1 + display_width(before_cursor)) as u16;
            let cursor_y = status_row as u16;
            queue!(stdout, MoveTo(cursor_x, cursor_y))?;
            queue!(stdout, SetCursorStyle::BlinkingBar)?;
            queue!(stdout, cursor::Show)?;
            stdout.flush()?;

            // Swap buffers
            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut git_view) = self.git_view {
            let cursor = git_view.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut commit_log) = self.commit_log {
            let cursor = commit_log.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut pr_list_picker) = self.pr_list_picker {
            let cursor = pr_list_picker.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut issue_list_picker) = self.issue_list_picker {
            let cursor = issue_list_picker.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut find_replace_popup) = self.find_replace_popup {
            let cursor = find_replace_popup.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref project_root_popup) = self.project_root_popup {
            let cursor = project_root_popup.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref recent_project_popup) = self.recent_project_popup {
            let cursor = recent_project_popup.render_overlay(&mut self.current);
            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref save_as_popup) = self.save_as_popup {
            let cursor = save_as_popup.render_overlay(&mut self.current);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut explorer_popup) = self.explorer_popup {
            let cursor = explorer_popup.render_overlay(&mut self.current, ctx.theme);

            draw_diff(&self.previous, &self.current, stdout)?;

            if let Some((cx, cy)) = cursor {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
            stdout.flush()?;

            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }

        if let Some(ref mut palette) = self.palette {
            let (cx, cy) = palette.render_overlay(&mut self.current, ctx.theme);
            let image_request = palette.take_pending_image_request();

            // Draw diff between previous and current
            draw_diff(&self.previous, &self.current, stdout)?;

            self.update_image_overlay(stdout, image_request)?;

            queue!(stdout, MoveTo(cx, cy))?;
            queue!(stdout, SetCursorStyle::BlinkingBar)?;
            queue!(stdout, cursor::Show)?;
            stdout.flush()?;

            // Swap buffers
            std::mem::swap(&mut self.current, &mut self.previous);
            return Ok(());
        }


        if let Some(ref hover) = self.markdown_link_hover
            && let Some((cursor_x, cursor_y, _)) = self.focused_window_cursor(ctx)
        {
            hover.render_overlay(
                &mut self.current,
                cursor_x as usize,
                cursor_y as usize,
                ctx.theme,
            );
        }

        // Draw diff between previous and current
        draw_diff(&self.previous, &self.current, stdout)?;

        self.update_image_overlay(stdout, explorer_image_request)?;

        // Handle cursor: explorer find mode cursor takes priority when explorer is present
        if let Some(ref explorer) = self.explorer {
            let explorer_height = rows.saturating_sub(2);
            if let Some((cx, cy)) = explorer.find_cursor(0, explorer_height) {
                queue!(stdout, MoveTo(cx, cy))?;
                queue!(stdout, SetCursorStyle::BlinkingBar)?;
                queue!(stdout, cursor::Show)?;
            } else if preview_active {
                // Preview pane shows static content; no editable cursor.
                queue!(stdout, cursor::Hide)?;
            } else if !is_fullscreen_explorer {
                // Explorer is open but not in find mode: show focused pane cursor
                if let Some((col, row, style)) = self.focused_window_cursor(ctx) {
                    queue!(stdout, MoveTo(col, row))?;
                    queue!(stdout, style)?;
                    queue!(stdout, cursor::Show)?;
                } else {
                    queue!(stdout, cursor::Hide)?;
                }
            } else {
                queue!(stdout, cursor::Hide)?;
            }
        } else {
            // No explorer: focused pane cursor
            if let Some((col, row, style)) = self.focused_window_cursor(ctx) {
                queue!(stdout, MoveTo(col, row))?;
                queue!(stdout, style)?;
                queue!(stdout, cursor::Show)?;
            } else {
                queue!(stdout, cursor::Hide)?;
            }
        }

        stdout.flush()?;

        // Swap buffers
        std::mem::swap(&mut self.current, &mut self.previous);
        Ok(())
    }

    fn render_windows(&mut self, ctx: &RenderContext) {
        let Some(area) = self.editor_rect_for_dims(ctx.cols, ctx.rows) else {
            return;
        };
        let layout = self.window_manager.layout(area);
        let focused_window = self.window_manager.focused_window_id();
        let active_buffer_id = ctx.editor.active_buffer().id;

        for pane in layout.panes {
            let Some(buffer) = ctx.editor.buffer_by_id(pane.buffer_id) else {
                continue;
            };
            let is_focused = pane.window_id == focused_window;
            let show_home = ctx.home_screen_active && is_focused && buffer.id == active_buffer_id;
            let show_search = is_focused && buffer.id == active_buffer_id;
            self.text_view.render_buffer(
                ctx,
                &mut self.current,
                buffer,
                pane.rect,
                show_search,
                show_home,
            );
        }

        let divider_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };
        for divider in layout.dividers {
            match divider.orientation {
                DividerOrientation::Vertical => {
                    for row in 0..divider.len {
                        self.current.put_str(
                            divider.x,
                            divider.y + row,
                            "\u{2502}",
                            &divider_style,
                        );
                    }
                }
                DividerOrientation::Horizontal => {
                    for col in 0..divider.len {
                        self.current.put_str(
                            divider.x + col,
                            divider.y,
                            "\u{2500}",
                            &divider_style,
                        );
                    }
                }
            }
        }
    }

    fn update_image_overlay(
        &mut self,
        stdout: &mut impl Write,
        request: Option<crate::ui::image::ImageRenderRequest>,
    ) -> io::Result<()> {
        match (request, self.displayed_image.clone()) {
            (None, Some(_)) => {
                crate::ui::image::clear_kitty_images(stdout)?;
                self.displayed_image = None;
            }
            (Some(req), Some(prev)) if prev.key == req.key => {
                // Same image already on screen; leave it in place.
            }
            (Some(req), prev) => {
                if prev.is_some() {
                    crate::ui::image::clear_kitty_images(stdout)?;
                }
                crate::ui::image::emit_kitty_image(
                    stdout,
                    1,
                    req.col,
                    req.row,
                    req.cell_cols,
                    req.cell_rows,
                    &req.data,
                )?;
                self.displayed_image = Some(super::DisplayedImage { key: req.key });
            }
            (None, None) => {}
        }
        Ok(())
    }

    fn focused_window_cursor(&self, ctx: &RenderContext) -> Option<(u16, u16, SetCursorStyle)> {
        let area = self.editor_rect_for_dims(ctx.cols, ctx.rows)?;
        let focused = self.window_manager.focused_pane(area)?;
        let buffer = ctx.editor.buffer_by_id(focused.buffer_id)?;
        let show_home = ctx.home_screen_active && buffer.id == ctx.editor.active_buffer().id;
        self.text_view
            .cursor_for_buffer(ctx, buffer, focused.rect, show_home)
    }
}

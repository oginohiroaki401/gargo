use crossterm::cursor::SetCursorStyle;
use crossterm::style::Color;
use std::path::Path;

use crate::command::in_editor_diff::IN_EDITOR_DIFF_TITLE;
use crate::core::document::{Document, SelectionCursorDisplay};
use crate::core::lsp_types::LspSeverity;
use crate::core::mode::Mode;
use crate::syntax::highlight::HighlightSpan;
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::{Component, RenderContext};
use crate::ui::framework::surface::Surface;
use crate::ui::framework::window_manager::PaneRect;
use crate::ui::text::{
    char_display_width, display_width, gutter_width, slice_display_window, truncate_to_width,
};

pub struct TextView;

impl Default for TextView {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn reserved_left_gutter_width(
    total_lines: usize,
    show_line_number: bool,
    min_digits: usize,
) -> usize {
    if show_line_number {
        gutter_width(total_lines).max(min_digits + 1)
    } else {
        1
    }
}

impl TextView {
    pub fn new() -> Self {
        Self
    }

    pub fn render_buffer(
        &self,
        ctx: &RenderContext,
        surface: &mut Surface,
        buf: &Document,
        area: PaneRect,
        render_search: bool,
        show_home_screen: bool,
    ) {
        if show_home_screen {
            render_home_screen_in_area(surface, area, ctx.project_root, ctx.home_screen_notice);
            return;
        }
        self.render_document(ctx, surface, buf, area, render_search);
    }

    pub fn cursor_for_buffer(
        &self,
        ctx: &RenderContext,
        buf: &Document,
        area: PaneRect,
        show_home_screen: bool,
    ) -> Option<(u16, u16, SetCursorStyle)> {
        if show_home_screen || area.width == 0 || area.height == 0 {
            return None;
        }

        let cursor_line = buf.display_cursor_line();
        let cursor_display_col = buf.display_cursor_display_col();
        let total_lines = buf.rope.len_lines();
        let gutter_w = reserved_left_gutter_width(
            total_lines,
            ctx.config.show_line_number,
            ctx.config.line_number_width,
        );

        let available = area.width.saturating_sub(gutter_w);
        let line = buf.rope.line(cursor_line).to_string();
        let line_display = line.trim_end_matches('\n');
        let horizontal =
            slice_display_window(line_display, buf.horizontal_scroll_offset, available);
        let view_start_col = horizontal.start_col;
        let cursor_col = cursor_display_col
            .saturating_sub(view_start_col)
            .min(available.saturating_sub(1));
        let screen_row = area.y + cursor_line.saturating_sub(buf.scroll_offset);
        if screen_row >= area.y + area.height {
            return None;
        }
        let screen_col = area.x + gutter_w + cursor_col;

        let cursor_style = match ctx.editor.mode {
            Mode::Insert => SetCursorStyle::BlinkingBar,
            Mode::Normal | Mode::Visual => SetCursorStyle::SteadyBlock,
        };

        Some((screen_col as u16, screen_row as u16, cursor_style))
    }

    fn render_document(
        &self,
        ctx: &RenderContext,
        surface: &mut Surface,
        buf: &Document,
        area: PaneRect,
        render_search: bool,
    ) {
        let area_x = area.x;
        let area_y = area.y;
        let area_w = area.width;
        let view_height = area.height.max(1);
        let show_line_number = ctx.config.show_line_number;
        let total_lines = buf.rope.len_lines();
        let gutter_w =
            reserved_left_gutter_width(total_lines, show_line_number, ctx.config.line_number_width);

        let start_line = buf.scroll_offset;
        let end_line = (buf.scroll_offset + view_height).min(total_lines);
        let highlight_spans = ctx
            .highlight_manager
            .query_visible(buf.id, &buf.rope, start_line, end_line);
        let is_in_editor_diff = is_in_editor_diff_buffer(buf);

        let dim_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };
        let default_style = CellStyle::default();
        let is_active_buffer = buf.id == ctx.editor.active_buffer().id;
        let diagnostic_lines = if is_active_buffer {
            ctx.editor.active_diagnostic_severity_by_line()
        } else {
            None
        };

        for row in 0..view_height {
            let line_idx = buf.scroll_offset + row;
            let screen_y = area_y + row;

            if line_idx < total_lines {
                let line = buf.rope.line(line_idx);
                let line_str = line.to_string();
                let display = line_str.trim_end_matches('\n');
                let diagnostic_severity =
                    diagnostic_lines.and_then(|lines| lines.get(&line_idx).copied());
                if show_line_number {
                    let line_number_width = gutter_w.saturating_sub(1);
                    let mut number_style = dim_style;
                    if let Some(severity) = diagnostic_severity {
                        apply_diagnostic_style(&mut number_style, severity);
                    }
                    let num_str = format!("{:>width$}", line_idx + 1, width = line_number_width);
                    surface.put_str(area_x, screen_y, &num_str, &number_style);

                    let git_lane_x = area_x + line_number_width;
                    if let Some(status) = buf.git_gutter.get(&line_idx) {
                        let symbol = status.gutter_symbol().to_string();
                        let symbol_style = git_gutter_style(status, ctx.theme);
                        surface.put_str(git_lane_x, screen_y, &symbol, &symbol_style);
                    } else {
                        surface.put_str(git_lane_x, screen_y, " ", &default_style);
                    }
                } else if let Some(status) = buf.git_gutter.get(&line_idx) {
                    surface.put_str(area_x, screen_y, " ", &default_style);
                    let cell = surface.get_mut(area_x, screen_y);
                    cell.style.bg = Some(status.gutter_bg());
                } else {
                    surface.put_str(area_x, screen_y, " ", &default_style);
                }

                let available = area_w.saturating_sub(gutter_w);
                let horizontal =
                    slice_display_window(display, buf.horizontal_scroll_offset, available);
                let visible_display = horizontal.visible;
                let used_width = horizontal.used_width;

                let line_spans = highlight_spans.get(&line_idx);
                if is_in_editor_diff {
                    if let Some(capture_name) = diff_overlay_capture_for_line(display) {
                        render_captured_line(
                            surface,
                            screen_y,
                            area_x + gutter_w,
                            visible_display,
                            available,
                            capture_name,
                            ctx.theme,
                        );
                    } else if let Some(spans) = line_spans {
                        render_highlighted_line_windowed(
                            surface,
                            (screen_y, area_x + gutter_w),
                            visible_display,
                            spans,
                            horizontal.start_byte..horizontal.end_byte,
                            available,
                            ctx.theme,
                        );
                    } else {
                        surface.put_str(
                            area_x + gutter_w,
                            screen_y,
                            visible_display,
                            &default_style,
                        );
                        if available > used_width {
                            surface.fill_region(
                                area_x + gutter_w + used_width,
                                screen_y,
                                available - used_width,
                                ' ',
                                &default_style,
                            );
                        }
                    }
                } else if let Some(spans) = line_spans {
                    render_highlighted_line_windowed(
                        surface,
                        (screen_y, area_x + gutter_w),
                        visible_display,
                        spans,
                        horizontal.start_byte..horizontal.end_byte,
                        available,
                        ctx.theme,
                    );
                } else {
                    surface.put_str(area_x + gutter_w, screen_y, visible_display, &default_style);
                    if available > used_width {
                        surface.fill_region(
                            area_x + gutter_w + used_width,
                            screen_y,
                            available - used_width,
                            ' ',
                            &default_style,
                        );
                    }
                }
            } else if show_line_number {
                let num_pad = " ".repeat(gutter_w.saturating_sub(1));
                let tilde_str = format!("{}~ ", num_pad);
                surface.put_str(area_x, screen_y, &tilde_str, &dim_style);
                let rest = area_w.saturating_sub(gutter_w);
                if rest > 0 {
                    surface.fill_region(area_x + gutter_w, screen_y, rest, ' ', &default_style);
                }
            } else {
                surface.put_str(area_x, screen_y, " ", &default_style);
                if area_w > gutter_w {
                    surface.put_str(area_x + gutter_w, screen_y, "~ ", &dim_style);
                }
                let tilde_end = gutter_w + 2;
                let rest = area_w.saturating_sub(tilde_end);
                if rest > 0 {
                    surface.fill_region(area_x + tilde_end, screen_y, rest, ' ', &default_style);
                }
            }
        }

        if render_search {
            let search = &ctx.editor.search;
            let pattern_len = search.pattern.chars().count();
            if !search.matches.is_empty() && pattern_len > 0 {
                let rope = &buf.rope;
                let rope_len = rope.len_chars();
                for (match_idx, &match_offset) in search.matches.iter().enumerate() {
                    if match_offset >= rope_len {
                        continue;
                    }
                    let match_end = match_offset.saturating_add(pattern_len).min(rope_len);
                    if match_end <= match_offset {
                        continue;
                    }
                    let match_line = rope.char_to_line(match_offset);
                    let match_end_line = rope.char_to_line(match_end.saturating_sub(1));

                    for line_idx in match_line..=match_end_line {
                        if line_idx < start_line || line_idx >= end_line {
                            continue;
                        }
                        let screen_row = area_y + (line_idx - start_line);
                        let line_start_char = rope.line_to_char(line_idx);

                        let char_start = match_offset.saturating_sub(line_start_char);
                        let line_char_count = rope.line(line_idx).len_chars();
                        let char_end = (match_end - line_start_char).min(line_char_count);

                        if char_start >= char_end {
                            continue;
                        }

                        let line_str = rope.line(line_idx).to_string();
                        let line_display = line_str.trim_end_matches('\n');
                        let available = area_w.saturating_sub(gutter_w);
                        let horizontal = slice_display_window(
                            line_display,
                            buf.horizontal_scroll_offset,
                            available,
                        );
                        let view_start_col = horizontal.start_col;
                        let view_end_col = view_start_col + available;
                        let prefix_end = line_display
                            .char_indices()
                            .nth(char_start)
                            .map(|(i, _)| i)
                            .unwrap_or(line_display.len());
                        let col_start = display_width(&line_display[..prefix_end]);

                        let is_current = search.current_match == Some(match_idx);
                        let (bg, fg) = if is_current {
                            (Some(Color::Yellow), Some(Color::Black))
                        } else {
                            (Some(Color::DarkYellow), None)
                        };

                        let mut char_col = col_start;
                        for ch in line_display
                            .chars()
                            .skip(char_start)
                            .take(char_end - char_start)
                        {
                            let ch_w = char_display_width(ch);
                            let next_char_col = char_col + ch_w;
                            if next_char_col <= view_start_col {
                                char_col = next_char_col;
                                continue;
                            }
                            if char_col >= view_end_col {
                                break;
                            }
                            if char_col < view_start_col {
                                char_col = next_char_col;
                                continue;
                            }
                            let screen_col = area_x + gutter_w + (char_col - view_start_col);
                            let cell = surface.get_mut(screen_col, screen_row);
                            cell.style.bg = bg;
                            if let Some(fg_color) = fg {
                                cell.style.fg = Some(fg_color);
                            }
                            if ch_w == 2
                                && screen_col + 1 < area_x + area_w
                                && char_col + 1 < view_end_col
                            {
                                let cont = surface.get_mut(screen_col + 1, screen_row);
                                cont.style.bg = bg;
                                if let Some(fg_color) = fg {
                                    cont.style.fg = Some(fg_color);
                                }
                            }
                            char_col = next_char_col;
                        }
                    }
                }
            }
        }

        for (sel_start, sel_end) in buf.merged_selection_ranges() {
            for row in 0..view_height {
                let line_idx = buf.scroll_offset + row;
                if line_idx >= total_lines {
                    break;
                }
                let line_char_start = buf.rope.line_to_char(line_idx);
                let line = buf.rope.line(line_idx);
                let line_str = line.to_string();
                let line_content = line_str.trim_end_matches('\n');
                let line_char_end = line_char_start + line_content.chars().count();

                if sel_start >= line_char_end && line_char_end > line_char_start {
                    continue;
                }
                if sel_end <= line_char_start {
                    continue;
                }

                let overlap_start = sel_start.max(line_char_start) - line_char_start;
                let overlap_end = sel_end.min(line_char_end) - line_char_start;

                let mut char_idx = 0usize;
                let mut col = 0usize;
                let mut col_start = 0usize;
                let mut col_end = 0usize;
                for ch in line_content.chars() {
                    let cw = char_display_width(ch);
                    if char_idx == overlap_start {
                        col_start = col;
                    }
                    if char_idx == overlap_end {
                        col_end = col;
                    }
                    char_idx += 1;
                    col += cw;
                }
                if overlap_end >= char_idx {
                    col_end = col;
                }

                let available = area_w.saturating_sub(gutter_w);
                let horizontal =
                    slice_display_window(line_content, buf.horizontal_scroll_offset, available);
                let view_start_col = horizontal.start_col;
                let view_end_col = view_start_col + available;
                let paint_start = col_start.max(view_start_col);
                let paint_end = col_end.min(view_end_col);

                for c in paint_start..paint_end {
                    let cell =
                        surface.get_mut(area_x + gutter_w + (c - view_start_col), area_y + row);
                    cell.style.reverse = !cell.style.reverse;
                }
            }
        }

        // Secondary cursors overlay pass
        if buf.cursors.len() > 1 {
            let available = area_w.saturating_sub(gutter_w);
            let rope_len = buf.rope.len_chars();
            for (i, &cursor_pos) in buf.cursors.iter().enumerate().skip(1) {
                // Each cursor decides its own one-back display adjustment based
                // on its own selection (not the primary's).
                let forward_sel = buf
                    .selections
                    .get(i)
                    .copied()
                    .flatten()
                    .is_some_and(|s| {
                        s.head > s.anchor
                            && matches!(s.cursor_display, SelectionCursorDisplay::TailOnForward)
                    });
                let cursor_pos = if forward_sel {
                    cursor_pos.saturating_sub(1)
                } else {
                    cursor_pos
                }
                .min(rope_len);
                let cursor_line = buf.rope.char_to_line(cursor_pos);
                if cursor_line < start_line || cursor_line >= end_line {
                    continue;
                }
                let screen_row = cursor_line - start_line;
                let line_start = buf.rope.line_to_char(cursor_line);
                let col_in_line = cursor_pos - line_start;

                // Calculate display column
                let line_slice = buf.rope.line(cursor_line);
                let line_str = line_slice.to_string();
                let line_content = line_str.trim_end_matches('\n');
                let mut display_col = 0usize;
                for (i, ch) in line_content.chars().enumerate() {
                    if i >= col_in_line {
                        break;
                    }
                    display_col += char_display_width(ch);
                }

                // Account for horizontal scroll
                if display_col < buf.horizontal_scroll_offset {
                    continue;
                }
                let screen_col = display_col - buf.horizontal_scroll_offset;
                if screen_col >= available {
                    continue;
                }

                // Render secondary cursor with distinct style
                let cell = surface.get_mut(area_x + gutter_w + screen_col, screen_row);
                cell.style.bg = Some(Color::DarkGrey);
                cell.style.fg = Some(Color::White);
            }
        }
    }
}

impl Component for TextView {
    fn render(&self, ctx: &RenderContext, surface: &mut Surface) {
        let area = PaneRect {
            x: ctx.editor_area_x,
            y: 0,
            width: ctx.editor_area_width,
            height: if ctx.rows > 2 { ctx.rows - 2 } else { 1 },
        };
        self.render_buffer(
            ctx,
            surface,
            ctx.editor.active_buffer(),
            area,
            true,
            ctx.home_screen_active,
        );
    }

    fn cursor(&self, ctx: &RenderContext) -> Option<(u16, u16, SetCursorStyle)> {
        let area = PaneRect {
            x: ctx.editor_area_x,
            y: 0,
            width: ctx.editor_area_width,
            height: if ctx.rows > 2 { ctx.rows - 2 } else { 1 },
        };
        self.cursor_for_buffer(
            ctx,
            ctx.editor.active_buffer(),
            area,
            ctx.home_screen_active,
        )
    }
}

fn is_in_editor_diff_buffer(buf: &Document) -> bool {
    if buf.file_path.is_some() || buf.rope.len_lines() == 0 {
        return false;
    }
    buf.rope.line(0).to_string().trim_end_matches('\n') == IN_EDITOR_DIFF_TITLE
}

fn diff_overlay_capture_for_line(line: &str) -> Option<&'static str> {
    if line == IN_EDITOR_DIFF_TITLE {
        Some("diff.header")
    } else if line.starts_with("## ") {
        Some("diff.section")
    } else if line.starts_with("Project: ")
        || line.starts_with("Changed files: ")
        || line.starts_with("Staged files: ")
        || line.starts_with("Untracked files: ")
        || line == "(no changes)"
    {
        Some("diff.meta")
    } else if line.starts_with("gd on ") || line.starts_with("Use command ") {
        Some("diff.help")
    } else if line.starts_with("@@") {
        Some("diff.hunk")
    } else if line.starts_with("+++ ") || (line.starts_with('+') && !line.starts_with("+++")) {
        Some("diff.plus")
    } else if line.starts_with("--- ") || (line.starts_with('-') && !line.starts_with("---")) {
        Some("diff.minus")
    } else if line.starts_with("diff --git ")
        || line.starts_with("index ")
        || line.starts_with("new file mode ")
        || line.starts_with("deleted file mode ")
        || line.starts_with("similarity index ")
        || line.starts_with("rename from ")
        || line.starts_with("rename to ")
        || line.starts_with("Binary files ")
    {
        Some("diff.meta")
    } else if line.starts_with(' ') {
        Some("diff.context")
    } else {
        None
    }
}

fn render_captured_line(
    surface: &mut Surface,
    y: usize,
    x_offset: usize,
    display: &str,
    max_width: usize,
    capture_name: &str,
    theme: &crate::syntax::theme::Theme,
) {
    let style = theme
        .style_for_capture(capture_name)
        .map(|theme_style| CellStyle {
            fg: theme_style.fg,
            bold: theme_style.bold,
            italic: theme_style.italic,
            ..CellStyle::default()
        })
        .unwrap_or_default();
    let (truncated, used) = truncate_to_width(display, max_width);
    surface.put_str(x_offset, y, truncated, &style);
    if max_width > used {
        surface.fill_region(x_offset + used, y, max_width - used, ' ', &style);
    }
}

fn render_home_screen_in_area(
    surface: &mut Surface,
    area: PaneRect,
    project_root: &Path,
    update_notice: Option<&str>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let center_row = area.y + area.height / 2;
    let identity = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let root_path = project_root.display().to_string();
    let hint = "Press i to start editing";
    let style = CellStyle::default();
    let secondary_style = CellStyle {
        dim: true,
        ..CellStyle::default()
    };

    let identity_w = display_width(&identity);
    let identity_x = area.x + area.width.saturating_sub(identity_w) / 2;
    surface.put_str(identity_x, center_row, &identity, &style);

    if center_row + 1 < area.y + area.height {
        let (path_text, _) = truncate_to_width(&root_path, area.width);
        let path_w = display_width(path_text);
        let path_x = area.x + area.width.saturating_sub(path_w) / 2;
        surface.put_str(path_x, center_row + 1, path_text, &secondary_style);
    }

    let hint_row = if let Some(notice) = update_notice {
        if center_row + 2 < area.y + area.height {
            let (notice_text, _) = truncate_to_width(notice, area.width);
            let notice_w = display_width(notice_text);
            let notice_x = area.x + area.width.saturating_sub(notice_w) / 2;
            surface.put_str(notice_x, center_row + 2, notice_text, &secondary_style);
        }
        center_row + 3
    } else {
        center_row + 2
    };

    if hint_row < area.y + area.height {
        let hint_w = display_width(hint);
        let hint_x = area.x + area.width.saturating_sub(hint_w) / 2;
        surface.put_str(hint_x, hint_row, hint, &secondary_style);
    }
}

/// Render a single line with syntax highlight spans onto a Surface.
pub fn render_highlighted_line(
    surface: &mut Surface,
    y: usize,
    x_offset: usize,
    display: &str,
    spans: &[HighlightSpan],
    max_width: usize,
    theme: &crate::syntax::theme::Theme,
) {
    render_highlighted_line_windowed(
        surface,
        (y, x_offset),
        display,
        spans,
        0..display.len(),
        max_width,
        theme,
    );
}

pub fn render_highlighted_line_windowed(
    surface: &mut Surface,
    pos: (usize, usize),
    display: &str,
    spans: &[HighlightSpan],
    byte_window: std::ops::Range<usize>,
    max_width: usize,
    theme: &crate::syntax::theme::Theme,
) {
    let (y, x_offset) = pos;
    let start_byte = byte_window.start;
    let end_byte = byte_window.end;
    let line_len = display.len();
    let mut col_width = 0usize;
    let mut byte_pos = 0usize;
    let mut next_span_idx = 0usize;
    let mut active_spans: Vec<usize> = Vec::new();
    let default_style = CellStyle::default();

    while byte_pos < line_len && col_width < max_width {
        let absolute_byte_pos = start_byte + byte_pos;

        while next_span_idx < spans.len() && spans[next_span_idx].start <= absolute_byte_pos {
            if spans[next_span_idx].end > absolute_byte_pos {
                active_spans.push(next_span_idx);
            }
            next_span_idx += 1;
        }

        while let Some(span_idx) = active_spans.last().copied() {
            if spans[span_idx].end > absolute_byte_pos {
                break;
            }
            active_spans.pop();
        }

        let current_capture = active_spans.last().copied();
        let next_boundary = {
            let mut boundary = end_byte;
            if let Some(span_idx) = current_capture {
                boundary = boundary.min(spans[span_idx].end.min(end_byte));
            }
            if next_span_idx < spans.len() {
                boundary = boundary.min(spans[next_span_idx].start.min(end_byte));
            }
            let mut local = if boundary > absolute_byte_pos {
                boundary - start_byte
            } else {
                byte_pos + display[byte_pos..].chars().next().unwrap().len_utf8()
            };
            // Spans may briefly be out of sync with `display` (e.g., right after a
            // reload-from-disk before the tree is re-parsed). Snap forward so we
            // never slice inside a multi-byte char.
            while local < line_len && !display.is_char_boundary(local) {
                local += 1;
            }
            local
        };

        let run_start = byte_pos;
        byte_pos = next_boundary;

        let seg_text = &display[run_start..byte_pos];
        let (truncated, used) = truncate_to_width(seg_text, max_width - col_width);

        let style = if let Some(span_idx) = current_capture {
            let capture_name = &spans[span_idx].capture_name;
            if let Some(theme_style) = theme.style_for_capture(capture_name) {
                CellStyle {
                    fg: theme_style.fg,
                    bold: theme_style.bold,
                    italic: theme_style.italic,
                    ..CellStyle::default()
                }
            } else {
                default_style
            }
        } else {
            default_style
        };

        surface.put_str(x_offset + col_width, y, truncated, &style);
        col_width += used;
    }

    // Pad the rest
    if col_width < max_width {
        surface.fill_region(
            x_offset + col_width,
            y,
            max_width - col_width,
            ' ',
            &default_style,
        );
    }
}

fn git_gutter_style(
    status: &crate::command::git::GitLineStatus,
    theme: &crate::syntax::theme::Theme,
) -> CellStyle {
    let capture_name = match status {
        crate::command::git::GitLineStatus::Added => "diff.plus.gutter",
        crate::command::git::GitLineStatus::Modified => "diff.delta.gutter",
        crate::command::git::GitLineStatus::Deleted => "diff.minus.gutter",
    };
    let fg = theme
        .style_for_capture(capture_name)
        .and_then(|style| style.fg)
        .or(match status {
            crate::command::git::GitLineStatus::Added => Some(Color::Green),
            crate::command::git::GitLineStatus::Modified => Some(Color::Yellow),
            crate::command::git::GitLineStatus::Deleted => Some(Color::Red),
        });

    CellStyle {
        fg,
        ..CellStyle::default()
    }
}

fn apply_diagnostic_style(style: &mut CellStyle, severity: LspSeverity) {
    style.bold = true;
    style.fg = Some(match severity {
        LspSeverity::Error => Color::Red,
        LspSeverity::Warning => Color::Yellow,
        LspSeverity::Info => Color::Blue,
        LspSeverity::Hint => Color::DarkGrey,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::core::editor::Editor;
    use crate::input::chord::KeyState;
    use crate::syntax::theme::Theme;
    use std::path::Path;

    fn row_text(surface: &Surface, row: usize) -> String {
        (0..surface.width)
            .map(|x| {
                let symbol = &surface.get(x, row).symbol;
                if symbol.is_empty() {
                    ' '
                } else {
                    symbol.chars().next().unwrap_or(' ')
                }
            })
            .collect()
    }

    fn render_editor_surface(
        editor: &Editor,
        config: &Config,
        width: usize,
        height: usize,
    ) -> Surface {
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            width,
            height,
            editor,
            &theme,
            &key_state,
            config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );
        let mut surface = Surface::new(width, height);
        TextView::new().render(&ctx, &mut surface);
        surface
    }

    fn find_char_in_row(surface: &Surface, row: usize, ch: char) -> usize {
        (0..surface.width)
            .find(|&x| surface.get(x, row).symbol == ch.to_string())
            .expect("expected char in rendered row")
    }

    #[test]
    fn diff_overlay_capture_classifies_diff_lines() {
        assert_eq!(diff_overlay_capture_for_line("+added"), Some("diff.plus"));
        assert_eq!(
            diff_overlay_capture_for_line("-removed"),
            Some("diff.minus")
        );
        assert_eq!(
            diff_overlay_capture_for_line("@@ -1,2 +1,2 @@"),
            Some("diff.hunk")
        );
        assert_eq!(
            diff_overlay_capture_for_line("diff --git a/a.txt b/a.txt"),
            Some("diff.meta")
        );
    }

    #[test]
    fn in_editor_diff_lines_are_colorized_without_file_extension() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text(
            "IN-EDITOR DIFF VIEW\n\
Project: /tmp/repo\n\
\n\
## Changed (unstaged)\n\
diff --git a/a.txt b/a.txt\n\
@@ -1 +1 @@\n\
-old\n\
+new\n",
        );
        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let surface = render_editor_surface(&editor, &config, 40, 12);

        let minus_x = find_char_in_row(&surface, 6, '-');
        assert_eq!(surface.get(minus_x, 6).style.fg, Some(Color::Red));

        let plus_x = find_char_in_row(&surface, 7, '+');
        assert_eq!(surface.get(plus_x, 7).style.fg, Some(Color::Green));

        let hunk_x = find_char_in_row(&surface, 5, '@');
        assert_eq!(surface.get(hunk_x, 5).style.fg, Some(Color::Yellow));
    }

    #[test]
    fn non_diff_scratch_lines_are_not_colorized_by_diff_overlay() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("+plain text\n");
        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let surface = render_editor_surface(&editor, &config, 20, 4);

        let plus_x = find_char_in_row(&surface, 0, '+');
        assert_eq!(surface.get(plus_x, 0).style.fg, None);
    }

    #[test]
    fn home_screen_renders_identity_text() {
        let editor = Editor::new();
        let config = Config::default();
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let root = Path::new("/tmp/gargo-home-screen");
        let ctx = RenderContext::new(
            60, 10, &editor, &theme, &key_state, &config, root, false, true,
        );
        let mut surface = Surface::new(60, 10);

        TextView::new().render(&ctx, &mut surface);

        let identity = format!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        let root_path = root.display().to_string();
        let hint = "Press i to start editing";
        let rendered = (0..10)
            .map(|row| row_text(&surface, row))
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|line| line.contains(&identity)));
        assert!(rendered.iter().any(|line| line.contains(&root_path)));
        assert!(rendered.iter().any(|line| line.contains(hint)));
    }

    #[test]
    fn home_screen_hides_cursor() {
        let editor = Editor::new();
        let config = Config::default();
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            60,
            10,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            true,
        );

        assert!(TextView::new().cursor(&ctx).is_none());
    }

    #[test]
    fn home_screen_path_and_hint_use_dim_style() {
        let editor = Editor::new();
        let config = Config::default();
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            60,
            10,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            true,
        );
        let mut surface = Surface::new(60, 10);

        TextView::new().render(&ctx, &mut surface);

        let rendered = (0..10)
            .map(|row| row_text(&surface, row))
            .collect::<Vec<_>>();

        let path = "/tmp/gargo-test-root";
        let path_row = rendered
            .iter()
            .position(|line| line.contains(path))
            .expect("path line should be rendered");
        let path_x = rendered[path_row]
            .find(path)
            .expect("path should be centered in row");
        assert_eq!(
            surface.get(path_x, path_row).style,
            CellStyle {
                dim: true,
                ..CellStyle::default()
            }
        );

        let hint = "Press i to start editing";
        let hint_row = rendered
            .iter()
            .position(|line| line.contains(hint))
            .expect("hint line should be rendered");
        let hint_x = rendered[hint_row]
            .find(hint)
            .expect("hint should be centered in row");
        assert_eq!(
            surface.get(hint_x, hint_row).style,
            CellStyle {
                dim: true,
                ..CellStyle::default()
            }
        );
    }

    #[test]
    fn home_screen_renders_update_notice_when_present() {
        let editor = Editor::new();
        let config = Config::default();
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let mut ctx = RenderContext::new(
            72,
            10,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            true,
        );
        ctx.home_screen_notice = Some("Update available: v0.1.20 (gargo --update)");
        let mut surface = Surface::new(72, 10);

        TextView::new().render(&ctx, &mut surface);

        let rendered = (0..10)
            .map(|row| row_text(&surface, row))
            .collect::<Vec<_>>();
        let notice = "Update available: v0.1.20 (gargo --update)";
        let notice_row = rendered
            .iter()
            .position(|line| line.contains(notice))
            .expect("notice line should be rendered");
        let notice_x = rendered[notice_row]
            .find(notice)
            .expect("notice should be centered in row");
        assert_eq!(
            surface.get(notice_x, notice_row).style,
            CellStyle {
                dim: true,
                ..CellStyle::default()
            }
        );
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Press i to start editing"))
        );
    }

    #[test]
    fn render_uses_horizontal_scroll_offset() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("0123456789abcdef\n");
        editor.active_buffer_mut().horizontal_scroll_offset = 5;

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            12,
            6,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );
        let mut surface = Surface::new(12, 6);

        TextView::new().render(&ctx, &mut surface);
        assert!(row_text(&surface, 0).starts_with(" 56789abcdef"));
    }

    #[test]
    fn tab_characters_render_as_spaces() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("a\tb\n");

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let surface = render_editor_surface(&editor, &config, 12, 6);

        assert_eq!(find_char_in_row(&surface, 0, 'b'), 6);
        for x in 2..=5 {
            assert_eq!(surface.get(x, 0).symbol, " ");
        }
    }

    #[test]
    fn cursor_respects_horizontal_scroll_offset() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("0123456789\n");
        editor.active_buffer_mut().set_cursor_line_char(0, 9);
        editor.active_buffer_mut().horizontal_scroll_offset = 5;

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            12,
            6,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );

        let (col, row, _) = TextView::new()
            .cursor(&ctx)
            .expect("cursor should be visible");
        assert_eq!(row, 0);
        assert_eq!(col, 5);
    }

    #[test]
    fn cursor_column_accounts_for_tab_width() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("\tb\n");
        editor.active_buffer_mut().set_cursor_line_char(0, 1);

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            12,
            6,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );

        let (col, row, _) = TextView::new()
            .cursor(&ctx)
            .expect("cursor should be visible");
        assert_eq!(row, 0);
        assert_eq!(col, 5);
    }

    #[test]
    fn selection_overlay_tracks_horizontal_scroll_offset() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("abcdefghij\n");
        editor.active_buffer_mut().set_cursor_line_char(0, 2);
        editor.active_buffer_mut().set_anchor();
        editor.active_buffer_mut().set_cursor_line_char(0, 6);
        editor.active_buffer_mut().horizontal_scroll_offset = 3;

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            10,
            6,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );
        let mut surface = Surface::new(10, 6);

        TextView::new().render(&ctx, &mut surface);

        assert!(surface.get(1, 0).style.reverse);
        assert!(surface.get(2, 0).style.reverse);
        assert!(surface.get(3, 0).style.reverse);
        assert!(!surface.get(4, 0).style.reverse);
    }

    #[test]
    fn search_overlay_tracks_horizontal_scroll_offset() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("abcdefghij\n");
        editor.active_buffer_mut().horizontal_scroll_offset = 2;
        editor.search.pattern = "de".to_string();
        editor.search.matches = vec![3];
        editor.search.current_match = Some(0);

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let theme = Theme::dark();
        let key_state = KeyState::Normal;
        let ctx = RenderContext::new(
            10,
            6,
            &editor,
            &theme,
            &key_state,
            &config,
            Path::new("/tmp/gargo-test-root"),
            false,
            false,
        );
        let mut surface = Surface::new(10, 6);

        TextView::new().render(&ctx, &mut surface);

        assert_eq!(surface.get(2, 0).style.bg, Some(Color::Yellow));
        assert_eq!(surface.get(3, 0).style.bg, Some(Color::Yellow));
    }

    #[test]
    fn line_numbers_do_not_shift_when_git_status_appears() {
        let mut without_git = Editor::new();
        without_git.active_buffer_mut().insert_text("alpha\n");

        let mut with_git = Editor::new();
        with_git.active_buffer_mut().insert_text("alpha\n");
        with_git
            .active_buffer_mut()
            .git_gutter
            .insert(0, crate::command::git::GitLineStatus::Modified);

        let config = Config::default();
        let without_git_surface = render_editor_surface(&without_git, &config, 20, 4);
        let with_git_surface = render_editor_surface(&with_git, &config, 20, 4);

        let number_col_without_git = find_char_in_row(&without_git_surface, 0, '1');
        let number_col_with_git = find_char_in_row(&with_git_surface, 0, '1');
        assert_eq!(number_col_without_git, number_col_with_git);

        let content_col_without_git = find_char_in_row(&without_git_surface, 0, 'a');
        let content_col_with_git = find_char_in_row(&with_git_surface, 0, 'a');
        assert_eq!(content_col_without_git, content_col_with_git);
    }

    #[test]
    fn line_number_off_reserves_git_lane() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("alpha\n");

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let surface = render_editor_surface(&editor, &config, 10, 4);

        assert_eq!(surface.get(0, 0).symbol, " ");
        assert_eq!(find_char_in_row(&surface, 0, 'a'), 1);
    }

    #[test]
    fn line_number_off_git_lane_uses_background_tint() {
        let mut editor = Editor::new();
        editor.active_buffer_mut().insert_text("alpha\n");
        editor
            .active_buffer_mut()
            .git_gutter
            .insert(0, crate::command::git::GitLineStatus::Added);

        let config = Config {
            show_line_number: false,
            ..Config::default()
        };
        let surface = render_editor_surface(&editor, &config, 10, 4);

        assert_eq!(surface.get(0, 0).symbol, " ");
        assert_eq!(surface.get(0, 0).style.bg, Some(Color::DarkGreen));
        assert_eq!(find_char_in_row(&surface, 0, 'a'), 1);
    }

    #[test]
    fn highlighted_line_later_overlap_wins_without_losing_text() {
        let mut surface = Surface::new(8, 1);
        let theme = Theme::dark();
        let spans = vec![
            HighlightSpan {
                start: 0,
                end: 6,
                capture_name: "keyword".to_string(),
            },
            HighlightSpan {
                start: 2,
                end: 4,
                capture_name: "string".to_string(),
            },
        ];

        render_highlighted_line(&mut surface, 0, 0, "abcdef", &spans, 8, &theme);

        assert_eq!(&row_text(&surface, 0)[..6], "abcdef");
        assert_eq!(
            surface.get(0, 0).style.fg,
            theme
                .style_for_capture("keyword")
                .and_then(|style| style.fg)
        );
        assert_eq!(
            surface.get(2, 0).style.fg,
            theme.style_for_capture("string").and_then(|style| style.fg)
        );
        assert_eq!(
            surface.get(4, 0).style.fg,
            theme
                .style_for_capture("keyword")
                .and_then(|style| style.fg)
        );
    }

    #[test]
    fn highlighted_line_survives_spans_misaligned_with_multibyte_chars() {
        // Regression: a stale span (e.g., from a tree not yet re-parsed after
        // reload-from-disk) can land on a byte index that falls inside a
        // multi-byte UTF-8 character. The renderer must clamp to a char
        // boundary instead of panicking on string slicing.
        let mut surface = Surface::new(20, 1);
        let theme = Theme::dark();
        // "実" occupies bytes 0..3; a span ending at byte 2 would slice mid-char.
        let spans = vec![HighlightSpan {
            start: 0,
            end: 2,
            capture_name: "string".to_string(),
        }];

        render_highlighted_line(&mut surface, 0, 0, "実a", &spans, 20, &theme);
    }

    #[test]
    fn highlighted_line_windowed_clips_without_rebasing_spans() {
        let mut surface = Surface::new(8, 1);
        let theme = Theme::dark();
        let spans = vec![HighlightSpan {
            start: 2,
            end: 5,
            capture_name: "string".to_string(),
        }];

        render_highlighted_line_windowed(&mut surface, (0, 0), "cde", &spans, 2..5, 8, &theme);

        assert_eq!(&row_text(&surface, 0)[..3], "cde");
        assert_eq!(
            surface.get(0, 0).style.fg,
            theme.style_for_capture("string").and_then(|style| style.fg)
        );
        assert_eq!(
            surface.get(2, 0).style.fg,
            theme.style_for_capture("string").and_then(|style| style.fg)
        );
    }
}

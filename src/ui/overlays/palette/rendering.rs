use super::*;

impl Palette {
    /// Render the palette overlay onto a Surface. Returns (cursor_x, cursor_y) for the input field.
    pub fn render_overlay(&mut self, surface: &mut Surface, theme: &Theme) -> (u16, u16) {
        self.pump_global_search();

        let cols = surface.width;
        let rows = surface.height;
        let popup_w = (cols * 80 / 100).max(3);
        let popup_h = (rows * 80 / 100).max(3);
        let offset_x = (cols.saturating_sub(popup_w)) / 2;
        let offset_y = (rows.saturating_sub(popup_h)) / 2;

        let split_threshold = 60;
        let left_x = offset_x;
        let left_w;

        if popup_w >= split_threshold {
            let gap = 2;
            left_w = (popup_w - gap) / 2;
            let right_w = popup_w - gap - left_w;
            let right_x = offset_x + left_w + gap;

            self.render_left_panel(surface, left_x, offset_y, left_w, popup_h);
            self.render_right_panel(surface, right_x, offset_y, right_w, popup_h, theme);
        } else {
            left_w = popup_w;
            self.render_left_panel(surface, left_x, offset_y, left_w, popup_h);
        }

        // Cursor position in input field
        let prompt = " ";
        let inner_w = left_w.saturating_sub(2);
        let max_input_w = inner_w.saturating_sub(prompt.len());
        let cursor_byte = self.input.byte_index_at_cursor();
        let cursor_slice = &self.input.text[..cursor_byte];
        let (_, visible_w) = truncate_to_width(cursor_slice, max_input_w);

        let cursor_x = (left_x + 1 + prompt.len() + visible_w) as u16;
        let cursor_y = (offset_y + 1) as u16;

        (cursor_x, cursor_y)
    }

    fn render_left_panel(&mut self, surface: &mut Surface, x: usize, y: usize, w: usize, h: usize) {
        let inner_w = w.saturating_sub(2);
        let candidate_area_h = h.saturating_sub(4);
        let default_style = CellStyle::default();

        self.ensure_selection_visible(candidate_area_h);

        for row in 0..h {
            if row == 0 {
                surface.put_str(x, y + row, "\u{250c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                if let Some(caller_label) = self.caller_label.as_deref() {
                    let label_text = format!(" {} ", caller_label);
                    let (truncated, _) = truncate_to_width(&label_text, inner_w);
                    surface.put_str(x + 1, y + row, truncated, &default_style);
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2510}", &default_style);
            } else if row == 1 {
                let prompt = " ";
                let max_input_w = inner_w.saturating_sub(prompt.len());
                let (truncated_input, used_w) = truncate_to_width(&self.input.text, max_input_w);
                let padding = inner_w.saturating_sub(prompt.len() + used_w);

                surface.put_str(x, y + row, "\u{2502}", &default_style);
                let mut col = x + 1;
                col += surface.put_str(col, y + row, prompt, &default_style);
                col += surface.put_str(col, y + row, truncated_input, &default_style);
                surface.fill_region(col, y + row, padding, ' ', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else if row == 2 && h > 3 {
                surface.put_str(x, y + row, "\u{251c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2524}", &default_style);
            } else if row == h - 1 {
                surface.put_str(x, y + row, "\u{2514}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2518}", &default_style);
            } else if candidate_area_h > 0 {
                let candidate_row = row - 3;
                let candidate_idx = self.scroll_offset + candidate_row;

                surface.put_str(x, y + row, "\u{2502}", &default_style);

                if candidate_idx < self.candidates.len() {
                    let is_selected = candidate_idx == self.selected;

                    let status_color = match self.candidates[candidate_idx].kind {
                        CandidateKind::File(idx) => self
                            .git_status_map
                            .get(&self.file_entries[idx])
                            .map(|s| s.color()),
                        _ => None,
                    };

                    render_candidate_label(
                        surface,
                        x + 1,
                        y + row,
                        &self.candidates[candidate_idx],
                        inner_w,
                        is_selected,
                        status_color,
                    );
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }

                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else {
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            }
        }
    }

    fn render_right_panel(
        &self,
        surface: &mut Surface,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        theme: &Theme,
    ) {
        let inner_w = w.saturating_sub(2);
        let content_h = h.saturating_sub(2);
        let has_preview = !self.preview_lines.is_empty();
        let default_style = CellStyle::default();
        let dim_style = CellStyle {
            dim: true,
            ..CellStyle::default()
        };
        let preview_start_col = if let (Some(target_line), Some(target_char_col)) =
            (self.jump_target_preview_line, self.jump_target_char_col)
        {
            self.preview_lines
                .get(target_line)
                .and_then(|line| jump_marker_column(line, target_char_col))
                .map(|(marker_col, _)| marker_col.saturating_sub(inner_w / 2))
                .unwrap_or(0)
        } else {
            0
        };

        let preview_row_offset = if let Some(target_line) = self.jump_target_preview_line {
            if content_h > 0 && target_line >= content_h {
                let centered = target_line.saturating_sub(content_h / 2);
                let max_offset = self.preview_lines.len().saturating_sub(content_h);
                centered.min(max_offset)
            } else {
                0
            }
        } else {
            0
        };

        for row in 0..h {
            if row == 0 {
                surface.put_str(x, y + row, "\u{250c}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2510}", &default_style);
            } else if row == h - 1 {
                surface.put_str(x, y + row, "\u{2514}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, '\u{2500}', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2518}", &default_style);
            } else if has_preview {
                let line_idx = (row - 1) + preview_row_offset;
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                if (row - 1) < content_h && line_idx < self.preview_lines.len() {
                    let line = &self.preview_lines[line_idx];
                    let window = slice_preview_display_window(line, preview_start_col, inner_w);
                    if let Some(spans) = self.preview_spans.get(&line_idx) {
                        render_highlighted_line_windowed(
                            surface,
                            (y + row, x + 1),
                            window.visible,
                            spans,
                            window.start_byte..window.end_byte,
                            inner_w,
                            theme,
                        );
                    } else {
                        surface.put_str(x + 1, y + row, window.visible, &dim_style);
                        let pad = inner_w.saturating_sub(window.used_width);
                        if pad > 0 {
                            surface.fill_region(
                                x + 1 + window.used_width,
                                y + row,
                                pad,
                                ' ',
                                &default_style,
                            );
                        }
                    }
                    if self.jump_target_preview_line == Some(line_idx)
                        && let Some(target_char_col) = self.jump_target_char_col
                        && let Some((marker_col, marker_width)) =
                            jump_marker_column(line, target_char_col)
                        && marker_col >= window.start_col
                    {
                        let visible_col = marker_col.saturating_sub(window.start_col);
                        let clamped_col = visible_col.min(inner_w.saturating_sub(1));
                        if clamped_col < inner_w {
                            let marker_x = x + 1 + clamped_col;
                            let marker_cell = surface.get_mut(marker_x, y + row);
                            marker_cell.style.reverse = true;

                            if marker_width == 2 && clamped_col + 1 < inner_w {
                                let continuation = surface.get_mut(marker_x + 1, y + row);
                                continuation.style.reverse = true;
                            }
                        }
                    }
                } else {
                    surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                }
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            } else {
                surface.put_str(x, y + row, "\u{2502}", &default_style);
                surface.fill_region(x + 1, y + row, inner_w, ' ', &default_style);
                surface.put_str(x + 1 + inner_w, y + row, "\u{2502}", &default_style);
            }
        }
    }
}

/// Shorten a file path to fit within `max_width` display cells.
/// Always preserves the filename; adds parent directories right-to-left
/// as space permits, prefixed with ".../" when truncated.
/// Returns (display_string, original_chars_skipped, display_prefix_char_count).
pub(super) fn shorten_path(path: &str, max_width: usize) -> (String, usize, usize) {
    let chars: Vec<char> = path.chars().collect();
    let total_w: usize = chars
        .iter()
        .map(|c| UnicodeWidthChar::width(*c).unwrap_or(0))
        .sum();

    if total_w <= max_width {
        return (path.to_string(), 0, 0);
    }

    let slash_positions: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter_map(|(i, &c)| if c == '/' { Some(i) } else { None })
        .collect();

    if slash_positions.is_empty() {
        let (t, _) = truncate_to_width(path, max_width);
        return (t.to_string(), 0, 0);
    }

    let prefix = ".../";
    let prefix_w = 4;
    let available = max_width.saturating_sub(prefix_w);

    // Try cuts from rightmost '/' to leftmost, finding longest fitting tail
    let mut best: Option<usize> = None;
    for &sp in slash_positions.iter().rev() {
        let tail_start = sp + 1;
        let tail_w: usize = chars[tail_start..]
            .iter()
            .map(|c| UnicodeWidthChar::width(*c).unwrap_or(0))
            .sum();
        if tail_w <= available {
            best = Some(tail_start);
        } else {
            break;
        }
    }

    match best {
        Some(cut) => {
            let tail: String = chars[cut..].iter().collect();
            (format!("{}{}", prefix, tail), cut, prefix.len())
        }
        None => {
            // Even filename doesn't fit with ".../" prefix; show filename only
            let last_slash = *slash_positions.last().unwrap();
            let filename: String = chars[last_slash + 1..].iter().collect();
            let (t, _) = truncate_to_width(&filename, max_width);
            (t.to_string(), last_slash + 1, 0)
        }
    }
}

pub(super) fn render_candidate_label(
    surface: &mut Surface,
    x: usize,
    y: usize,
    candidate: &ScoredCandidate,
    max_width: usize,
    is_selected: bool,
    status_color: Option<Color>,
) {
    let base_style = if is_selected {
        CellStyle {
            reverse: true,
            fg: status_color,
            ..CellStyle::default()
        }
    } else {
        CellStyle {
            fg: status_color,
            ..CellStyle::default()
        }
    };

    let bold_style = if is_selected {
        CellStyle {
            bold: true,
            reverse: true,
            fg: status_color,
            ..CellStyle::default()
        }
    } else {
        CellStyle {
            bold: true,
            fg: status_color,
            ..CellStyle::default()
        }
    };

    // Write prefix space
    let prefix = " ";
    let mut col = x;
    col += surface.put_str(col, y, prefix, &base_style);

    let effective_w = max_width.saturating_sub(1);

    // For buffer candidates, shorten path to fit; otherwise use label as-is
    let (display_chars, match_set) = if matches!(candidate.kind, CandidateKind::Buffer(_)) {
        let (shortened, skip, prefix_len) = shorten_path(&candidate.label, effective_w);
        let dchars: Vec<char> = shortened.chars().collect();
        let adjusted: HashSet<usize> = candidate
            .match_positions
            .iter()
            .filter(|&&p| p >= skip)
            .map(|&p| p - skip + prefix_len)
            .collect();
        (dchars, adjusted)
    } else {
        let dchars: Vec<char> = candidate.label.chars().collect();
        let set: HashSet<usize> = candidate.match_positions.iter().copied().collect();
        (dchars, set)
    };

    for (i, &ch) in display_chars.iter().enumerate() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col + ch_width > x + max_width {
            break;
        }

        let style = if match_set.contains(&i) {
            &bold_style
        } else {
            &base_style
        };

        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        col += surface.put_str(col, y, s, style);
    }

    // Pad remaining
    let used = col - x;
    let padding = max_width.saturating_sub(used);
    if padding > 0 {
        surface.fill_region(col, y, padding, ' ', &base_style);
    }
}

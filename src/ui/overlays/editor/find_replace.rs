use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::Color;
use regex::Regex;
use ropey::Rope;

use crate::input::action::{Action, AppAction, UiAction, WorkspaceAction};
use crate::ui::framework::cell::CellStyle;
use crate::ui::framework::component::EventResult;
use crate::ui::framework::surface::Surface;
use crate::ui::text::display_width;
use crate::ui::text_input::TextInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedField {
    Find,
    Replace,
    Regex,
    All,
}

pub struct FindReplacePopup {
    // Input fields
    find: TextInput,
    replace: TextInput,
    use_regex: bool,
    replace_all: bool,

    // UI state
    focused_field: FocusedField,

    // Preview
    preview_buffer: Option<Rope>,
    preview_scroll: usize,
    matches: Vec<(usize, usize)>, // (start_char, end_char) for each match

    // Error handling
    error_message: Option<String>,

    // Document cursor position for relative search
    document_cursor: usize,
}

impl FindReplacePopup {
    pub fn new(document_cursor: usize) -> Self {
        Self {
            find: TextInput::default(),
            replace: TextInput::default(),
            use_regex: false,
            replace_all: true,
            focused_field: FocusedField::Find,
            preview_buffer: None,
            preview_scroll: 0,
            matches: Vec::new(),
            error_message: None,
            document_cursor,
        }
    }

    /// Update preview based on current inputs and document
    pub fn update_preview(&mut self, document_rope: &Rope) {
        self.error_message = None;
        self.matches.clear();
        self.preview_buffer = None;

        if self.find.text.is_empty() {
            return;
        }

        // Find all matches
        self.matches = self.find_matches(document_rope);

        if self.matches.is_empty() {
            return;
        }

        // Create preview by applying replacements
        let mut preview = document_rope.clone();

        // Apply replacements in reverse order to preserve indices
        for (start, end) in self.matches.iter().rev() {
            preview.remove(*start..*end);
            preview.insert(*start, &self.replace.text);
        }

        self.preview_buffer = Some(preview);
    }

    fn find_matches(&mut self, rope: &Rope) -> Vec<(usize, usize)> {
        let text = rope.to_string();

        if self.use_regex {
            match Regex::new(&self.find.text) {
                Ok(re) => {
                    let matches: Vec<_> = re
                        .find_iter(&text)
                        .map(|m| {
                            let start_char = rope.byte_to_char(m.start());
                            let end_char = rope.byte_to_char(m.end());
                            (start_char, end_char)
                        })
                        .collect();

                    if self.replace_all {
                        matches
                    } else {
                        // Find first match after cursor
                        matches
                            .into_iter()
                            .find(|(start, _)| *start >= self.document_cursor)
                            .map(|m| vec![m])
                            .unwrap_or_default()
                    }
                }
                Err(e) => {
                    self.error_message = Some(format!("Invalid regex: {}", e));
                    vec![]
                }
            }
        } else {
            // Literal string search
            let mut matches = Vec::new();
            let pattern = &self.find.text;
            let mut search_start = 0;

            while let Some(pos) = text[search_start..].find(pattern) {
                let abs_byte_pos = search_start + pos;
                let start_char = rope.byte_to_char(abs_byte_pos);
                let end_char = rope.byte_to_char(abs_byte_pos + pattern.len());

                if !self.replace_all {
                    // Only first match after cursor
                    if start_char >= self.document_cursor {
                        return vec![(start_char, end_char)];
                    }
                } else {
                    matches.push((start_char, end_char));
                }

                search_start = abs_byte_pos + pattern.len();
            }

            matches
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> EventResult {
        // Handle Escape to close
        if key.code == KeyCode::Esc {
            return EventResult::Action(Action::Ui(UiAction::CloseFindReplacePopup));
        }

        // Handle Tab navigation
        if key.code == KeyCode::Tab {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+Tab: previous field
                self.focused_field = match self.focused_field {
                    FocusedField::Find => FocusedField::All,
                    FocusedField::Replace => FocusedField::Find,
                    FocusedField::Regex => FocusedField::Replace,
                    FocusedField::All => FocusedField::Regex,
                };
            } else {
                // Tab: next field
                self.focused_field = match self.focused_field {
                    FocusedField::Find => FocusedField::Replace,
                    FocusedField::Replace => FocusedField::Regex,
                    FocusedField::Regex => FocusedField::All,
                    FocusedField::All => FocusedField::Find,
                };
            }
            return EventResult::Consumed;
        }

        // Handle BackTab (some terminals send this for Shift+Tab)
        if key.code == KeyCode::BackTab {
            self.focused_field = match self.focused_field {
                FocusedField::Find => FocusedField::All,
                FocusedField::Replace => FocusedField::Find,
                FocusedField::Regex => FocusedField::Replace,
                FocusedField::All => FocusedField::Regex,
            };
            return EventResult::Consumed;
        }

        // Handle Enter to execute replacement
        if key.code == KeyCode::Enter {
            if !self.find.text.is_empty() && !self.matches.is_empty() {
                return EventResult::Action(Action::App(AppAction::Workspace(
                    WorkspaceAction::ExecuteFindReplace {
                        find: self.find.text.clone(),
                        replace: self.replace.text.clone(),
                        use_regex: self.use_regex,
                        replace_all: self.replace_all,
                    },
                )));
            }
            return EventResult::Consumed;
        }

        // Handle checkbox toggles
        if key.code == KeyCode::Char(' ') {
            match self.focused_field {
                FocusedField::Regex => {
                    self.use_regex = !self.use_regex;
                    return EventResult::Consumed;
                }
                FocusedField::All => {
                    self.replace_all = !self.replace_all;
                    return EventResult::Consumed;
                }
                _ => {}
            }
        }

        // Handle preview scrolling
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') | KeyCode::Down => {
                    self.scroll_preview_down();
                    return EventResult::Consumed;
                }
                KeyCode::Char('p') | KeyCode::Up => {
                    self.scroll_preview_up();
                    return EventResult::Consumed;
                }
                _ => {}
            }
        }

        // Handle text input for Find and Replace fields
        match self.focused_field {
            FocusedField::Find => handle_text_input(&mut self.find, key),
            FocusedField::Replace => handle_text_input(&mut self.replace, key),
            _ => EventResult::Consumed,
        }
    }

    fn scroll_preview_down(&mut self) {
        if let Some(ref preview) = self.preview_buffer {
            let line_count = preview.len_lines();
            if self.preview_scroll + 1 < line_count {
                self.preview_scroll += 1;
            }
        }
    }

    fn scroll_preview_up(&mut self) {
        if self.preview_scroll > 0 {
            self.preview_scroll -= 1;
        }
    }

    pub fn render_overlay(&self, surface: &mut Surface) -> Option<(u16, u16)> {
        let term_width = surface.width;
        let term_height = surface.height;

        // Popup size from config (default 80% × 80%), with absolute floors so the
        // input/preview layout stays usable on small terminals.
        let (configured_w, configured_h) =
            crate::ui::popup_layout::popup_size(term_width, term_height);
        let popup_width = configured_w.max(40).min(term_width);
        let popup_height = configured_h.max(15).min(term_height);
        let popup_x = (term_width.saturating_sub(popup_width)) / 2;
        let popup_y = (term_height.saturating_sub(popup_height)) / 2;

        // Split layout: left panel for inputs, right panel for preview
        let show_split = popup_width >= 60;
        let left_width = if show_split {
            popup_width / 2
        } else {
            popup_width
        };

        // Border style
        let border_style = CellStyle::default();
        let bg_style = CellStyle::default();

        // Clear popup area
        for y in popup_y..popup_y + popup_height {
            surface.fill_region(popup_x, y, popup_width, ' ', &bg_style);
        }

        // Draw border
        draw_border(
            surface,
            popup_x,
            popup_y,
            popup_width,
            popup_height,
            &border_style,
        );

        // Title
        let title = " Find and Replace ";
        let title_x = popup_x + (popup_width.saturating_sub(title.len())) / 2;
        surface.put_str(title_x, popup_y, title, &border_style);

        // Render left panel (inputs)
        let content_y = popup_y + 1;
        let mut y = content_y + 1;

        // Find input
        let find_label = "Find:    ";
        let find_focused = self.focused_field == FocusedField::Find;
        let find_style = if find_focused {
            CellStyle {
                reverse: true,
                ..CellStyle::default()
            }
        } else {
            bg_style
        };

        surface.put_str(popup_x + 2, y, find_label, &bg_style);
        let input_x = popup_x + 2 + find_label.len();
        let input_width = left_width.saturating_sub(4 + find_label.len());
        let (find_display, _) = crate::ui::text::truncate_to_width(&self.find.text, input_width);
        surface.fill_region(input_x, y, input_width, ' ', &find_style);
        surface.put_str(input_x, y, find_display, &find_style);
        y += 1;

        // Replace input
        let replace_label = "Replace: ";
        let replace_focused = self.focused_field == FocusedField::Replace;
        let replace_style = if replace_focused {
            CellStyle {
                reverse: true,
                ..CellStyle::default()
            }
        } else {
            bg_style
        };

        surface.put_str(popup_x + 2, y, replace_label, &bg_style);
        let (replace_display, _) =
            crate::ui::text::truncate_to_width(&self.replace.text, input_width);
        surface.fill_region(input_x, y, input_width, ' ', &replace_style);
        surface.put_str(input_x, y, replace_display, &replace_style);
        y += 1;

        // Regex checkbox
        let regex_focused = self.focused_field == FocusedField::Regex;
        let regex_style = if regex_focused {
            CellStyle {
                reverse: true,
                ..CellStyle::default()
            }
        } else {
            bg_style
        };
        let regex_text = if self.use_regex {
            "[x] Regex"
        } else {
            "[ ] Regex"
        };
        surface.put_str(popup_x + 2, y, regex_text, &regex_style);
        y += 1;

        // Replace All checkbox
        let all_focused = self.focused_field == FocusedField::All;
        let all_style = if all_focused {
            CellStyle {
                reverse: true,
                ..CellStyle::default()
            }
        } else {
            bg_style
        };
        let all_text = if self.replace_all {
            "[x] Replace All"
        } else {
            "[ ] Replace All"
        };
        surface.put_str(popup_x + 2, y, all_text, &all_style);
        y += 2;

        // Status message
        if let Some(ref error) = self.error_message {
            let error_style = CellStyle {
                fg: Some(Color::Red),
                ..CellStyle::default()
            };
            let (error_display, _) =
                crate::ui::text::truncate_to_width(error, left_width.saturating_sub(4));
            surface.put_str(popup_x + 2, y, error_display, &error_style);
        } else if !self.matches.is_empty() {
            let count_text = format!(
                "Found {} match{}",
                self.matches.len(),
                if self.matches.len() == 1 { "" } else { "es" }
            );
            surface.put_str(popup_x + 2, y, &count_text, &bg_style);
        } else if !self.find.text.is_empty() {
            surface.put_str(popup_x + 2, y, "No matches found", &bg_style);
        }

        // Render right panel (preview) if in split mode
        if show_split {
            let preview_x = popup_x + left_width + 1;
            let preview_width = popup_width.saturating_sub(left_width + 1);
            let preview_height = popup_height.saturating_sub(3);

            // Draw vertical separator
            for py in content_y..popup_y + popup_height - 1 {
                surface.put_str(popup_x + left_width, py, "│", &border_style);
            }

            // Preview label
            surface.put_str(preview_x + 2, content_y, "Preview:", &bg_style);

            // Render preview content
            if let Some(ref preview) = self.preview_buffer {
                let start_line = self.preview_scroll;
                let end_line = (start_line + preview_height).min(preview.len_lines());

                let mut py = content_y + 1;
                for line_idx in start_line..end_line {
                    if py >= popup_y + popup_height - 1 {
                        break;
                    }

                    let line = preview.line(line_idx).to_string();
                    let (line_display, _) = crate::ui::text::truncate_to_width(
                        line.trim_end_matches('\n'),
                        preview_width.saturating_sub(4),
                    );

                    // Check if this line contains a changed region
                    let line_start_char = preview.line_to_char(line_idx);
                    let line_end_char = if line_idx + 1 < preview.len_lines() {
                        preview.line_to_char(line_idx + 1)
                    } else {
                        preview.len_chars()
                    };

                    // Simple highlight: if any match touches this line, highlight the whole line
                    let is_changed = self.matches.iter().any(|(start, end)| {
                        (*start >= line_start_char && *start < line_end_char)
                            || (*end > line_start_char && *end <= line_end_char)
                    });

                    let line_style = if is_changed {
                        CellStyle {
                            fg: Some(Color::Green),
                            ..CellStyle::default()
                        }
                    } else {
                        bg_style
                    };

                    surface.put_str(preview_x + 2, py, line_display, &line_style);
                    py += 1;
                }
            }
        }

        // Help text at bottom
        let help_text = "Enter:Execute  Tab:Next  Esc:Cancel";
        let help_x = popup_x + (popup_width.saturating_sub(help_text.len())) / 2;
        surface.put_str(help_x, popup_y + popup_height - 1, help_text, &border_style);

        // Calculate cursor position
        let cursor_x = match self.focused_field {
            FocusedField::Find => {
                let before_cursor = &self.find.text[..self.find.byte_index_at_cursor()];
                input_x + display_width(before_cursor)
            }
            FocusedField::Replace => {
                let before_cursor = &self.replace.text[..self.replace.byte_index_at_cursor()];
                input_x + display_width(before_cursor)
            }
            _ => popup_x + 2, // On checkbox
        };
        let cursor_y = match self.focused_field {
            FocusedField::Find => content_y + 1,
            FocusedField::Replace => content_y + 2,
            FocusedField::Regex => content_y + 3,
            FocusedField::All => content_y + 4,
        };

        Some((cursor_x as u16, cursor_y as u16))
    }

    pub fn matches(&self) -> &[(usize, usize)] {
        &self.matches
    }
}

fn handle_text_input(input: &mut TextInput, key: KeyEvent) -> EventResult {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Left => {
                input.move_word_left();
                EventResult::Consumed
            }
            KeyCode::Right => {
                input.move_word_right();
                EventResult::Consumed
            }
            _ => EventResult::Consumed,
        };
    }

    match key.code {
        KeyCode::Char(c) => {
            input.insert_char(c);
            EventResult::Consumed
        }
        KeyCode::Backspace => {
            let _ = input.backspace();
            EventResult::Consumed
        }
        KeyCode::Left => {
            input.move_left();
            EventResult::Consumed
        }
        KeyCode::Right => {
            input.move_right();
            EventResult::Consumed
        }
        _ => EventResult::Consumed,
    }
}

fn draw_border(
    surface: &mut Surface,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    style: &CellStyle,
) {
    // Top border
    surface.put_str(x, y, "┌", style);
    for i in 1..width - 1 {
        surface.put_str(x + i, y, "─", style);
    }
    surface.put_str(x + width - 1, y, "┐", style);

    // Side borders
    for i in 1..height - 1 {
        surface.put_str(x, y + i, "│", style);
        surface.put_str(x + width - 1, y + i, "│", style);
    }

    // Bottom border
    surface.put_str(x, y + height - 1, "└", style);
    for i in 1..width - 1 {
        surface.put_str(x + i, y + height - 1, "─", style);
    }
    surface.put_str(x + width - 1, y + height - 1, "┘", style);
}

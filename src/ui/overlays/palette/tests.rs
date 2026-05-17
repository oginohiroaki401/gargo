use super::*;
use crate::config::Config;

#[test]
fn fuzzy_match_exact() {
    let result = fuzzy_match("Save File", "Save File");
    assert!(result.is_some());
    let (score, positions) = result.unwrap();
    assert!(score > 0);
    assert_eq!(positions, vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn fuzzy_match_prefix() {
    let result = fuzzy_match("Save File", "sav");
    assert!(result.is_some());
    let (_, positions) = result.unwrap();
    assert_eq!(positions, vec![0, 1, 2]);
}

#[test]
fn fuzzy_match_case_insensitive() {
    let result = fuzzy_match("Save File", "sf");
    assert!(result.is_some());
    let (_, positions) = result.unwrap();
    assert_eq!(positions[0], 0); // 'S'
    assert_eq!(positions[1], 5); // 'F'
}

#[test]
fn fuzzy_match_no_match() {
    let result = fuzzy_match("Save File", "xyz");
    assert!(result.is_none());
}

#[test]
fn fuzzy_match_empty_needle() {
    let result = fuzzy_match("Save File", "");
    assert!(result.is_some());
    let (score, positions) = result.unwrap();
    assert_eq!(score, 0);
    assert!(positions.is_empty());
}

#[test]
fn fuzzy_match_word_boundary_bonus() {
    let (score_sf, _) = fuzzy_match("Save File", "sf").unwrap();
    let (score_sa, _) = fuzzy_match("Save File", "sa").unwrap();
    assert!(score_sf > 0 || score_sa > 0);
}

#[test]
fn fuzzy_match_consecutive_beats_sparse() {
    let (score_con, _) = fuzzy_match("Save File", "sav").unwrap();
    let (score_spr, _) = fuzzy_match("Save File", "sfe").unwrap();
    assert!(
        score_con > score_spr,
        "consecutive matches should score higher"
    );
}

#[test]
fn file_picker_renders_japanese_filename_correctly() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(
        vec!["テスト.md".to_string(), "english.txt".to_string()],
        Path::new(""),
        &HashMap::new(),
        None,
        vec![],
        vec![],
    );
    palette.input.text = "テス".into();
    palette.update_candidates(&registry, &lang_registry, &config);
    assert!(!palette.candidates.is_empty(), "expected file matches");

    let mut surface = Surface::new(120, 30);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);

    // Find a row containing the Japanese filename.
    let mut japanese_row: Option<usize> = None;
    for y in 0..surface.height {
        let row_text: String = (0..surface.width)
            .map(|x| {
                let s = surface.get(x, y).symbol.as_str();
                if s.is_empty() {
                    String::new()
                } else {
                    s.to_string()
                }
            })
            .collect();
        if row_text.contains("テスト.md") {
            japanese_row = Some(y);
            break;
        }
    }
    let japanese_row = japanese_row.expect("Japanese filename should appear in surface");

    // Each wide char (テ, ス, ト) must occupy 2 cells: a "main" cell with the
    // glyph followed by a "continuation" cell with empty symbol. If any
    // continuation cell still holds a stray character, the rendered text drifts.
    let mut col = 0;
    let mut found_text = String::new();
    while col < surface.width {
        let s = surface.get(col, japanese_row).symbol.as_str();
        if s.is_empty() {
            col += 1;
            continue;
        }
        found_text.push_str(s);
        let ch = s.chars().next().unwrap();
        let w = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(1)
            .max(1);
        if w == 2 {
            // Continuation must be empty.
            assert_eq!(
                surface.get(col + 1, japanese_row).symbol.as_str(),
                "",
                "wide char at col {} ({:?}) must have empty continuation but has {:?}",
                col,
                ch,
                surface.get(col + 1, japanese_row).symbol
            );
        }
        col += w;
    }
    assert!(
        found_text.contains("テスト.md"),
        "rendered row {:?}",
        found_text
    );
}

#[test]
fn fuzzy_match_order_matters() {
    let result = fuzzy_match("Quit Editor", "qe");
    assert!(result.is_some());
    let result2 = fuzzy_match("Quit Editor", "eq");
    assert!(result2.is_none());
}

#[test]
fn fzf_style_match_prefers_consecutive_matches() {
    let (score_consecutive, _) = fzf_style_match("abcdef", "abc").unwrap();
    let (score_sparse, _) = fzf_style_match("axbxcxdef", "abc").unwrap();
    assert!(score_consecutive > score_sparse);
}

#[test]
fn palette_mode_detection() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    assert_eq!(palette.input.text, ">");
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::Command);

    palette.input.text = "hello".into();
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::FileFinder);
}

#[test]
fn palette_selection_wraps() {
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    palette.candidates = vec![
        ScoredCandidate {
            kind: CandidateKind::Command(0),
            label: "A".into(),
            score: 0,
            match_positions: vec![],
            preview_lines: vec![],
        },
        ScoredCandidate {
            kind: CandidateKind::Command(1),
            label: "B".into(),
            score: 0,
            match_positions: vec![],
            preview_lines: vec![],
        },
    ];
    palette.selected = 0;
    palette.select_next(&lang_registry, &config);
    assert_eq!(palette.selected, 1);
    palette.select_next(&lang_registry, &config);
    assert_eq!(palette.selected, 0); // wraps to first
    palette.select_prev(&lang_registry, &config);
    assert_eq!(palette.selected, 1); // wraps to last
    palette.select_prev(&lang_registry, &config);
    assert_eq!(palette.selected, 0);
}

#[test]
fn global_search_literal_case_insensitive() {
    let text = "First Line\nsecond line\nTHIRD line";
    let matches = workers::find_global_search_matches("src/main.rs", text, "line", 10);
    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0].line, 0);
    assert_eq!(matches[1].line, 1);
    assert_eq!(matches[2].line, 2);
    assert!(matches[0].preview_lines[0].starts_with("src/main.rs:1:"));
}

#[test]
fn global_search_enter_opens_selected_match() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());

    palette.global_search_entries = vec![GlobalSearchResultEntry {
        rel_path: "src/main.rs".to_string(),
        display_path: "src/main.rs".to_string(),
        line: 12,
        char_col: 7,
        preview_lines: vec![
            "src/main.rs:13:8".to_string(),
            "   13 | let x = 1;".to_string(),
        ],
    }];
    palette.candidates = vec![ScoredCandidate {
        kind: CandidateKind::SearchResult(0),
        label: "src/main.rs:13 let x = 1;".to_string(),
        score: 0,
        match_positions: vec![],
        preview_lines: vec![
            "src/main.rs:13:8".to_string(),
            "   13 | let x = 1;".to_string(),
        ],
    }];

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );

    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Buffer(
            BufferAction::OpenProjectFileAt {
                rel_path: "src/main.rs".to_string(),
                line: 12,
                char_col: 7,
            },
        )))
    );
}

#[test]
fn global_search_alt_enter_sends_results_to_buffer() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());
    palette.input.text = "let".to_string();

    palette.global_search_entries = vec![
        GlobalSearchResultEntry {
            rel_path: "src/main.rs".to_string(),
            display_path: "src/main.rs".to_string(),
            line: 12,
            char_col: 7,
            preview_lines: vec![
                "src/main.rs:13:8".to_string(),
                "   13 | let x = 1;".to_string(),
            ],
        },
        GlobalSearchResultEntry {
            rel_path: "src/lib.rs".to_string(),
            display_path: "src/lib.rs".to_string(),
            line: 0,
            char_col: 4,
            preview_lines: vec![
                "src/lib.rs:1:5".to_string(),
                "    1 | let y = 2;".to_string(),
            ],
        },
    ];

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        &registry,
        &lang_registry,
        &config,
    );

    let expected_entries = vec![
        crate::input::action::SearchResultEntry {
            rel_path: "src/main.rs".to_string(),
            line: 12,
            char_col: 7,
            excerpt: "let x = 1;".to_string(),
        },
        crate::input::action::SearchResultEntry {
            rel_path: "src/lib.rs".to_string(),
            line: 0,
            char_col: 4,
            excerpt: "let y = 2;".to_string(),
        },
    ];
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Workspace(
            WorkspaceAction::OpenSearchResultsBuffer {
                query: "let".to_string(),
                entries: expected_entries,
            },
        )))
    );
}

#[test]
fn global_search_alt_enter_with_no_results_closes_palette() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());
    palette.input.text = "nomatch".to_string();

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT),
        &registry,
        &lang_registry,
        &config,
    );

    assert_eq!(
        result,
        EventResult::Action(Action::Ui(UiAction::ClosePalette))
    );
}

#[test]
fn global_search_worker_error_updates_preview_message() {
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());
    let (tx, rx) = mpsc::channel::<GlobalSearchBatch>();
    palette.global_search_result_rx = Some(rx);

    tx.send(GlobalSearchBatch {
        generation: 1,
        results: Vec::new(),
        append: false,
        error: Some("bad global search filter".to_string()),
    })
    .unwrap();

    palette.pump_global_search();

    assert!(palette.global_search_entries.is_empty());
    assert!(palette.candidates.is_empty());
    assert_eq!(
        palette.preview_lines,
        vec!["Global search error: bad global search filter".to_string()]
    );
}

#[test]
fn buffer_picker_mode() {
    let entries = vec![
        (1, "main.rs".to_string(), vec!["fn main() {}".to_string()]),
        (2, "[scratch]".to_string(), vec![]),
        (3, "lib.rs".to_string(), vec!["pub mod foo;".to_string()]),
    ];
    let palette = Palette::new_buffer_picker(entries);
    assert_eq!(palette.mode, PaletteMode::BufferPicker);
    assert_eq!(palette.candidates.len(), 3);
    assert_eq!(palette.selected_buffer_id(), Some(1));
}

#[test]
fn buffer_picker_filter() {
    let entries = vec![
        (1, "main.rs".to_string(), vec!["fn main() {}".to_string()]),
        (2, "[scratch]".to_string(), vec![]),
        (3, "lib.rs".to_string(), vec!["pub mod foo;".to_string()]),
    ];
    let mut palette = Palette::new_buffer_picker(entries);
    palette.on_char_buffer('m');
    // "main.rs" matches, "[scratch]" doesn't have 'm'. "lib.rs" doesn't either.
    assert!(!palette.candidates.is_empty());
    assert_eq!(palette.selected_buffer_id(), Some(1));
}

#[test]
fn buffer_picker_preview_populated_on_creation() {
    let entries = vec![
        (
            1,
            "main.rs".to_string(),
            vec!["fn main() {}".to_string(), "// end".to_string()],
        ),
        (2, "[scratch]".to_string(), vec![]),
    ];
    let palette = Palette::new_buffer_picker(entries);
    assert_eq!(palette.preview_lines, vec!["fn main() {}", "// end"]);
}

#[test]
fn buffer_picker_preview_updates_on_selection_change() {
    let entries = vec![
        (1, "main.rs".to_string(), vec!["fn main() {}".to_string()]),
        (2, "lib.rs".to_string(), vec!["pub mod foo;".to_string()]),
    ];
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new_buffer_picker(entries);
    assert_eq!(palette.preview_lines, vec!["fn main() {}"]);
    palette.select_next(&lang_registry, &config);
    assert_eq!(palette.preview_lines, vec!["pub mod foo;"]);
}

#[test]
fn buffer_picker_preview_updates_on_filter() {
    let entries = vec![
        (1, "main.rs".to_string(), vec!["fn main() {}".to_string()]),
        (2, "lib.rs".to_string(), vec!["pub mod foo;".to_string()]),
    ];
    let mut palette = Palette::new_buffer_picker(entries);
    palette.on_char_buffer('l');
    // only "lib.rs" matches
    assert_eq!(palette.candidates.len(), 1);
    assert_eq!(palette.preview_lines, vec!["pub mod foo;"]);
}

#[test]
fn jump_picker_mode_and_selection() {
    let entries = vec![
        JumpPickerEntry {
            jump_index: 3,
            label: "src/main.rs:10:1 main".to_string(),
            preview_lines: vec![
                "src/main.rs:10:1".to_string(),
                "   10 | fn main() {}".to_string(),
            ],
            source_path: Some("src/main.rs".to_string()),
            target_preview_line: Some(1),
            target_char_col: 3,
        },
        JumpPickerEntry {
            jump_index: 1,
            label: "src/lib.rs:2:1 run".to_string(),
            preview_lines: vec![
                "src/lib.rs:2:1".to_string(),
                "    2 | pub fn run() {}".to_string(),
            ],
            source_path: Some("src/lib.rs".to_string()),
            target_preview_line: Some(1),
            target_char_col: 8,
        },
    ];
    let palette = Palette::new_jump_picker(entries);
    assert_eq!(palette.mode, PaletteMode::JumpPicker);
    assert_eq!(palette.selected_jump_index(), Some(3));
    assert_eq!(
        palette.preview_lines,
        vec!["src/main.rs:10:1", "   10 | fn main() {}"]
    );
}

#[test]
fn jump_picker_enter_dispatches_jump_action() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let entries = vec![JumpPickerEntry {
        jump_index: 7,
        label: "src/main.rs:1:1 main".to_string(),
        preview_lines: vec!["src/main.rs:1:1".to_string()],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: None,
        target_char_col: 0,
    }];
    let mut palette = Palette::new_jump_picker(entries);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Navigation(
            NavigationAction::JumpToListIndex(7),
        )))
    );
}

#[test]
fn jump_picker_filter_matches_word_suffix_with_fzf() {
    let entries = vec![
        JumpPickerEntry {
            jump_index: 3,
            label: "src/main.rs:10:1 helper".to_string(),
            preview_lines: vec!["src/main.rs:10:1".to_string()],
            source_path: Some("src/main.rs".to_string()),
            target_preview_line: None,
            target_char_col: 0,
        },
        JumpPickerEntry {
            jump_index: 1,
            label: "src/lib.rs:2:1 render_overlay".to_string(),
            preview_lines: vec!["src/lib.rs:2:1".to_string()],
            source_path: Some("src/lib.rs".to_string()),
            target_preview_line: None,
            target_char_col: 0,
        },
    ];
    let mut palette = Palette::new_jump_picker(entries);
    palette.on_char_jump('o');
    palette.on_char_jump('v');
    palette.on_char_jump('r');
    assert_eq!(palette.candidates.len(), 1);
    assert_eq!(palette.candidates[0].label, "src/lib.rs:2:1 render_overlay");
}

#[test]
fn jump_picker_preview_builds_syntax_spans() {
    let entries = vec![JumpPickerEntry {
        jump_index: 1,
        label: "src/main.rs:1:1 main".to_string(),
        preview_lines: vec![
            "src/main.rs:1:1".to_string(),
            "    1 | fn main() {}".to_string(),
        ],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: Some(1),
        target_char_col: 3,
    }];
    let palette = Palette::new_jump_picker(entries);
    assert!(!palette.preview_spans.is_empty());
}

fn right_preview_panel_geometry(cols: usize, rows: usize) -> (usize, usize, usize) {
    let (popup_w, popup_h) = crate::ui::popup_layout::popup_size(cols, rows);
    let offset_x = (cols.saturating_sub(popup_w)) / 2;
    let _offset_y = (rows.saturating_sub(popup_h)) / 2;
    let gap = 2;
    let left_w = (popup_w - gap) / 2;
    let right_w = popup_w - gap - left_w;
    let right_x = offset_x + left_w + gap;
    (right_x + 1, right_w.saturating_sub(2), popup_h)
}

fn reversed_columns_on_row(surface: &Surface, row: usize) -> Vec<usize> {
    (0..surface.width)
        .filter(|&x| surface.get(x, row).style.reverse)
        .collect()
}

/// Find the first row that contains any reversed cell. Used to locate the jump
/// marker row independent of popup size / vertical centering.
fn find_first_reversed_row(surface: &Surface) -> Option<usize> {
    (0..surface.height).find(|&y| (0..surface.width).any(|x| surface.get(x, y).style.reverse))
}

#[test]
fn jump_picker_preview_auto_scrolls_to_center_marker() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let long = format!("let left = 1; let right = {}; // marker", "x".repeat(100));
    let mut palette = Palette::new_jump_picker(vec![JumpPickerEntry {
        jump_index: 1,
        label: "src/main.rs:1:1 main".to_string(),
        preview_lines: vec!["src/main.rs:1:1".to_string(), format!("    1 | {}", long)],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: Some(1),
        target_char_col: 85,
    }]);

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);
    let (preview_x, inner_w, _) = right_preview_panel_geometry(100, 20);
    let marker_row = find_first_reversed_row(&surface).expect("marker row");
    let reversed = reversed_columns_on_row(&surface, marker_row);
    assert!(!reversed.is_empty());
    let center = preview_x + inner_w / 2;
    assert!((reversed[0] as isize - center as isize).abs() <= 2);
}

#[test]
fn jump_picker_preview_keeps_syntax_when_horizontally_sliced() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let mut palette = Palette::new_jump_picker(vec![JumpPickerEntry {
        jump_index: 1,
        label: "src/main.rs:1:1 main".to_string(),
        preview_lines: vec![
            "src/main.rs:1:1".to_string(),
            format!(
                "    1 | fn main() {{ let target = \"{}\"; }}",
                "value".repeat(20)
            ),
        ],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: Some(1),
        target_char_col: 70,
    }]);

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);
    let styled_cells = (0..surface.height)
        .map(|y| {
            (0..surface.width)
                .filter(|&x| surface.get(x, y).style.fg.is_some())
                .count()
        })
        .max()
        .unwrap_or(0);
    assert!(styled_cells > 0);
}

#[test]
fn symbol_picker_preview_auto_scrolls_to_center_marker() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let long = format!("let left = 1; let right = {}; // marker", "x".repeat(100));
    let mut palette = Palette::new_symbol_picker(vec![(
        "marker [function]  10:86".to_string(),
        9,
        85,
        vec![format!("   10 | {}", long)],
    )]);

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);
    let (preview_x, inner_w, _) = right_preview_panel_geometry(100, 20);
    let marker_row = find_first_reversed_row(&surface).expect("marker row");
    let reversed = reversed_columns_on_row(&surface, marker_row);
    assert!(!reversed.is_empty());
    let center = preview_x + inner_w / 2;
    assert!((reversed[0] as isize - center as isize).abs() <= 2);
}

#[test]
fn jump_picker_preview_marks_wide_char_continuation() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let code = format!("let s = \"{}あ{}\";", "x".repeat(40), "x".repeat(40));
    let target_char_col = code
        .chars()
        .position(|ch| ch == 'あ')
        .expect("contains wide char");
    let mut palette = Palette::new_jump_picker(vec![JumpPickerEntry {
        jump_index: 1,
        label: "src/main.rs:1:1 wide".to_string(),
        preview_lines: vec!["src/main.rs:1:1".to_string(), format!("    1 | {}", code)],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: Some(1),
        target_char_col,
    }]);

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);
    let reversed = reversed_columns_on_row(&surface, 4);
    assert!(reversed.len() >= 2);
}

#[test]
fn jump_picker_preview_defaults_to_no_horizontal_scroll_without_target() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let mut palette = Palette::new_jump_picker(vec![JumpPickerEntry {
        jump_index: 1,
        label: "src/main.rs:1:1".to_string(),
        preview_lines: vec!["src/main.rs:1:1".to_string()],
        source_path: Some("src/main.rs".to_string()),
        target_preview_line: None,
        target_char_col: 0,
    }]);

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);
    let (preview_x, _, _) = right_preview_panel_geometry(100, 20);
    // First content row inside the right panel is offset_y + 1, where
    // offset_y depends on popup_h and rows.
    let (_, popup_h) = crate::ui::popup_layout::popup_size(100, 20);
    let offset_y = (20usize.saturating_sub(popup_h)) / 2;
    assert_eq!(surface.get(preview_x, offset_y + 1).symbol, "s");
}

#[test]
fn reference_picker_mode_and_enter_dispatches_open_file_at_lsp_location() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let target = PathBuf::from("/tmp/src/main.rs");
    let mut palette = Palette::new_reference_picker(
        "LSP: Find References".to_string(),
        vec![ReferencePickerEntry {
            label: "src/main.rs:10:4 helper".to_string(),
            path: target.clone(),
            line: 9,
            character_utf16: 3,
            preview_lines: vec![
                "src/main.rs:10:4".to_string(),
                "   10 | fn helper() {}".to_string(),
            ],
            source_path: Some("src/main.rs".to_string()),
            target_preview_line: Some(1),
            target_char_col: 3,
        }],
    );
    assert_eq!(palette.mode, PaletteMode::ReferencePicker);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Navigation(
            NavigationAction::OpenFileAtLspLocation {
                path: target,
                line: 9,
                character_utf16: 3,
            },
        )))
    );
}

#[test]
fn git_branch_picker_mode_and_enter_dispatches_switch_branch() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new_git_branch_picker(vec![
        GitBranchPickerEntry {
            branch_name: "feature/login".to_string(),
            label: "  feature/login".to_string(),
            preview_lines: vec!["Branch: feature/login".to_string()],
        },
        GitBranchPickerEntry {
            branch_name: "main".to_string(),
            label: "* main".to_string(),
            preview_lines: vec!["Branch: main".to_string()],
        },
    ]);

    assert_eq!(palette.mode, PaletteMode::GitBranchPicker);
    assert_eq!(palette.selected_git_branch().as_deref(), Some("main"));

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Project(
            ProjectAction::SwitchGitBranch("main".to_string()),
        )))
    );
}

#[test]
fn reference_picker_filter_matches_with_fzf() {
    let mut palette = Palette::new_reference_picker(
        "LSP: Find References".to_string(),
        vec![
            ReferencePickerEntry {
                label: "src/main.rs:10:4 helper".to_string(),
                path: PathBuf::from("/tmp/src/main.rs"),
                line: 9,
                character_utf16: 3,
                preview_lines: vec!["src/main.rs:10:4".to_string()],
                source_path: Some("src/main.rs".to_string()),
                target_preview_line: None,
                target_char_col: 3,
            },
            ReferencePickerEntry {
                label: "src/lib.rs:2:1 render_overlay".to_string(),
                path: PathBuf::from("/tmp/src/lib.rs"),
                line: 1,
                character_utf16: 0,
                preview_lines: vec!["src/lib.rs:2:1".to_string()],
                source_path: Some("src/lib.rs".to_string()),
                target_preview_line: None,
                target_char_col: 0,
            },
        ],
    );

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    palette.on_char('o', &registry, &lang_registry, &config);
    palette.on_char('v', &registry, &lang_registry, &config);
    palette.on_char('r', &registry, &lang_registry, &config);

    assert_eq!(palette.candidates.len(), 1);
    assert_eq!(palette.candidates[0].label, "src/lib.rs:2:1 render_overlay");
}

#[test]
fn reference_picker_reuses_cached_preview_spans_on_revisit() {
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new_reference_picker(
        "LSP: Find References".to_string(),
        vec![
            ReferencePickerEntry {
                label: "src/main.rs:10:4 helper".to_string(),
                path: PathBuf::from("/tmp/src/main.rs"),
                line: 9,
                character_utf16: 3,
                preview_lines: vec![
                    "src/main.rs:10:4".to_string(),
                    "   10 | fn helper() {}".to_string(),
                ],
                source_path: Some("src/main.rs".to_string()),
                target_preview_line: Some(1),
                target_char_col: 3,
            },
            ReferencePickerEntry {
                label: "src/lib.rs:2:1 render_overlay".to_string(),
                path: PathBuf::from("/tmp/src/lib.rs"),
                line: 1,
                character_utf16: 0,
                preview_lines: vec![
                    "src/lib.rs:2:1".to_string(),
                    "    2 | pub fn render_overlay() {}".to_string(),
                ],
                source_path: Some("src/lib.rs".to_string()),
                target_preview_line: Some(1),
                target_char_col: 7,
            },
        ],
    );

    assert!(palette.reference_highlight_cache.contains_key(&0));
    palette.select_next(&lang_registry, &config);
    assert!(palette.reference_highlight_cache.contains_key(&1));

    let mut sentinel = HashMap::new();
    sentinel.insert(
        1,
        vec![HighlightSpan {
            start: 0,
            end: 1,
            capture_name: "sentinel.capture".to_string(),
        }],
    );
    palette.reference_highlight_cache.insert(0, sentinel);

    palette.select_prev(&lang_registry, &config);
    let spans = palette
        .preview_spans
        .get(&1)
        .expect("cached spans should be used");
    assert_eq!(spans[0].capture_name, "sentinel.capture");
}

#[test]
fn reference_picker_renders_caller_label_on_top_border() {
    use crate::syntax::theme::Theme;
    use crate::ui::framework::surface::Surface;

    let mut palette = Palette::new_reference_picker(
        "LSP: Find References".to_string(),
        vec![ReferencePickerEntry {
            label: "src/main.rs:1:1".to_string(),
            path: PathBuf::from("/tmp/src/main.rs"),
            line: 0,
            character_utf16: 0,
            preview_lines: vec!["src/main.rs:1:1".to_string()],
            source_path: Some("src/main.rs".to_string()),
            target_preview_line: None,
            target_char_col: 0,
        }],
    );

    let mut surface = Surface::new(100, 20);
    let theme = Theme::dark();
    let _ = palette.render_overlay(&mut surface, &theme);

    let (popup_w, popup_h) = crate::ui::popup_layout::popup_size(100, 20);
    let offset_x = (100usize.saturating_sub(popup_w)) / 2;
    let offset_y = (20usize.saturating_sub(popup_h)) / 2;
    let left_w = (popup_w - 2) / 2;

    let row_text: String = (offset_x + 1..offset_x + 1 + left_w.saturating_sub(2))
        .map(|x| {
            surface
                .get(x, offset_y)
                .symbol
                .chars()
                .next()
                .unwrap_or(' ')
        })
        .collect();
    assert!(row_text.contains("LSP: Find References"));
}

#[test]
fn symbol_picker_mode_and_enter_dispatches_jump_line_char() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new_symbol_picker(vec![(
        "helper [function]  10:4".to_string(),
        9,
        3,
        vec!["   10 | fn helper() {}".to_string()],
    )]);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Navigation(
            NavigationAction::JumpToLineChar {
                line: 9,
                char_col: 3,
            },
        )))
    );
}

#[test]
fn smart_copy_picker_enter_dispatches_copy_to_clipboard() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new_smart_copy_picker(vec![SmartCopyPickerEntry {
        label: "helper [function]  10:4".to_string(),
        line: 9,
        char_col: 3,
        preview_lines: vec!["   10 | fn helper() {}".to_string()],
        copy_text: "fn helper() {}".to_string(),
    }]);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(
        result,
        EventResult::Action(Action::App(AppAction::Integration(
            IntegrationAction::CopyToClipboard {
                text: "fn helper() {}".to_string(),
                description: "smart copy section".to_string(),
            },
        )))
    );
}

#[test]
fn picker_ctrl_f_b_moves_input_cursor() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    palette.set_input("abc".to_string());
    palette.input.cursor = 1;

    palette.handle_key_event(
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(palette.input.cursor, 2);

    palette.handle_key_event(
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(palette.input.cursor, 1);
}

#[test]
fn picker_ctrl_w_and_ctrl_k_edit_query() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    palette.set_input("alpha beta gamma".to_string());
    palette.input.cursor = palette.input.char_len();

    palette.handle_key_event(
        KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(palette.input.text, "alpha beta ");

    palette.input.cursor = 6;
    palette.handle_key_event(
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(palette.input.text, "alpha ");
}

#[test]
fn picker_ctrl_j_selects_next_candidate() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    palette.candidates = vec![
        ScoredCandidate {
            kind: CandidateKind::Command(0),
            label: "A".into(),
            score: 0,
            match_positions: vec![],
            preview_lines: vec![],
        },
        ScoredCandidate {
            kind: CandidateKind::Command(1),
            label: "B".into(),
            score: 0,
            match_positions: vec![],
            preview_lines: vec![],
        },
    ];
    palette.selected = 0;

    palette.handle_key_event(
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        &registry,
        &lang_registry,
        &config,
    );
    assert_eq!(palette.selected, 1);
}

#[test]
fn ctrl_c_closes_palette() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let result = palette.handle_key_event(key, &registry, &lang_registry, &config);
    assert_eq!(
        result,
        EventResult::Action(Action::Ui(UiAction::ClosePalette))
    );
}

#[test]
fn ctrl_q_closes_palette() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    let result = palette.handle_key_event(key, &registry, &lang_registry, &config);
    assert_eq!(
        result,
        EventResult::Action(Action::Ui(UiAction::ClosePalette))
    );
}

#[test]
fn shorten_path_fits() {
    let (display, skip, prefix_len) = shorten_path("src/main.rs", 20);
    assert_eq!(display, "src/main.rs");
    assert_eq!(skip, 0);
    assert_eq!(prefix_len, 0);
}

#[test]
fn shorten_path_truncates_leading_dirs() {
    // "src/deeply/nested/dir/file.rs" = 28 chars
    // With max_width=19: ".../dir/file.rs" = 15, fits
    let (display, skip, prefix_len) = shorten_path("src/deeply/nested/dir/file.rs", 19);
    assert_eq!(display, ".../dir/file.rs");
    assert!(skip > 0);
    assert_eq!(prefix_len, 4);
}

#[test]
fn shorten_path_shows_filename_only() {
    // max_width=12: ".../file.rs" = 11, fits
    let (display, _, _) = shorten_path("src/deeply/nested/dir/file.rs", 12);
    assert_eq!(display, ".../file.rs");
}

#[test]
fn shorten_path_no_slash() {
    // No directory separators, truncate from right
    let (display, skip, prefix_len) = shorten_path("verylongfilename.rs", 10);
    assert_eq!(skip, 0);
    assert_eq!(prefix_len, 0);
    assert!(display.len() <= 10);
}

#[test]
fn shorten_path_scratch_fits() {
    let (display, skip, prefix_len) = shorten_path("[scratch]", 20);
    assert_eq!(display, "[scratch]");
    assert_eq!(skip, 0);
    assert_eq!(prefix_len, 0);
}

#[test]
fn command_history_sorts_by_last_used() {
    use crate::command::history::CommandHistory;
    use crate::command::registry::{CommandEntry, CommandRegistry};
    use std::time::SystemTime;

    // Create unique temp dir for this test
    let timestamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("gargo_test_palette_history_{}", timestamp));
    std::fs::create_dir_all(&temp_dir).unwrap();

    // Create a test history and record some commands
    let history = CommandHistory::new_with_data_dir(
        &std::path::PathBuf::from("/tmp/test_repo_palette"),
        temp_dir.clone(),
    );

    // Record commands in specific order: Save, Quit, Open
    history.record_execution("test.save").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    history.record_execution("test.quit").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    history.record_execution("test.open").unwrap();

    // Create a registry with test commands
    let mut registry = CommandRegistry::new();
    registry.register(CommandEntry {
        id: "test.save".into(),
        label: "Save File".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.open".into(),
        label: "Open File".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.quit".into(),
        label: "Quit Editor".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.unused".into(),
        label: "Unused Command".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });

    // Test: Empty query WITH history should sort by last-used
    let config = Config::default();
    let candidates = Palette::filter_commands(&registry, "", Some(&history), &config);

    // Most recent first: Open, Quit, Save, then alphabetically: Unused
    assert_eq!(candidates.len(), 4);
    assert_eq!(candidates[0].label, "Open File"); // Most recent
    assert_eq!(candidates[1].label, "Quit Editor"); // Second most recent
    assert_eq!(candidates[2].label, "Save File"); // Third most recent
    assert_eq!(candidates[3].label, "Unused Command"); // Not in history, alphabetical

    // Clean up
    std::fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn command_history_alphabetical_without_history() {
    use crate::command::registry::{CommandEntry, CommandRegistry};

    // Create a registry with test commands
    let mut registry = CommandRegistry::new();
    registry.register(CommandEntry {
        id: "test.zzz".into(),
        label: "Zzz Last".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.aaa".into(),
        label: "Aaa First".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.mmm".into(),
        label: "Mmm Middle".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });

    // Test: Empty query WITHOUT history should sort alphabetically
    let config = Config::default();
    let candidates = Palette::filter_commands(&registry, "", None, &config);

    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[0].label, "Aaa First");
    assert_eq!(candidates[1].label, "Mmm Middle");
    assert_eq!(candidates[2].label, "Zzz Last");
}

#[test]
fn command_history_fuzzy_search_overrides_history() {
    use crate::command::history::CommandHistory;
    use crate::command::registry::{CommandEntry, CommandRegistry};
    use std::time::SystemTime;

    // Create unique temp dir for this test
    let timestamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!("gargo_test_fuzzy_history_{}", timestamp));
    std::fs::create_dir_all(&temp_dir).unwrap();

    // Create a test history
    let history = CommandHistory::new_with_data_dir(
        &std::path::PathBuf::from("/tmp/test_repo_fuzzy"),
        temp_dir.clone(),
    );

    // Record "Quit" as most recent
    history.record_execution("test.quit").unwrap();

    // Create a registry with test commands
    let mut registry = CommandRegistry::new();
    registry.register(CommandEntry {
        id: "test.save".into(),
        label: "Save File".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });
    registry.register(CommandEntry {
        id: "test.quit".into(),
        label: "Quit Editor".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });

    // Test: Query "save" should match by fuzzy score, not history
    let config = Config::default();
    let candidates = Palette::filter_commands(&registry, "save", Some(&history), &config);

    // Only "Save File" should match
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].label, "Save File");

    // Clean up
    std::fs::remove_dir_all(&temp_dir).ok();
}

#[test]
fn command_history_graceful_degradation() {
    use crate::command::registry::{CommandEntry, CommandRegistry};

    // Create a registry with test commands
    let mut registry = CommandRegistry::new();
    registry.register(CommandEntry {
        id: "test.cmd".into(),
        label: "Test Command".into(),
        category: None,
        action: Box::new(|_ctx| crate::command::registry::CommandEffect::None),
    });

    // Test with None history (should not crash, just use alphabetical)
    let config = Config::default();
    let candidates = Palette::filter_commands(&registry, "", None, &config);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].label, "Test Command");
}

#[test]
fn command_labels_for_config_toggles_are_dynamic() {
    let mut registry = CommandRegistry::new();
    crate::command::registry::register_builtins(&mut registry);

    let config = Config {
        debug: false,
        show_line_number: true,
        ..Config::default()
    };
    let candidates = Palette::filter_commands(&registry, "", None, &config);
    let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"Show Debug"));
    assert!(labels.contains(&"Hide Line Number"));

    let config = Config {
        debug: true,
        show_line_number: false,
        ..Config::default()
    };
    let candidates = Palette::filter_commands(&registry, "", None, &config);
    let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"Hide Debug"));
    assert!(labels.contains(&"Show Line Number"));
}

#[test]
fn release_key_events_are_ignored() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());
    palette.input.text = "test".to_string();

    // Simulate Enter key release (happens during IME composition confirmation)
    let result = palette.handle_key_event(
        KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Release),
        &registry,
        &lang_registry,
        &config,
    );

    // Release events should be consumed without action, input preserved
    assert_eq!(result, EventResult::Consumed);
    assert_eq!(palette.input.text, "test"); // Input should NOT be cleared
}

#[test]
fn palette_insert_text_japanese() {
    // Test that Japanese text (from IME paste events) is correctly inserted
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette =
        Palette::new_global_search(vec![], Path::new(""), &HashMap::new(), Vec::new());

    // Insert Japanese text (simulating IME composition result)
    palette.insert_text("日本語", &registry, &lang_registry, &config);
    assert_eq!(palette.input.text, "日本語");
    assert_eq!(palette.input.cursor, 3); // 3 characters

    // Insert more Japanese text
    palette.insert_text("テスト", &registry, &lang_registry, &config);
    assert_eq!(palette.input.text, "日本語テスト");
    assert_eq!(palette.input.cursor, 6);
}

#[test]
fn palette_at_prefix_activates_symbol_mode() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let symbols = vec![(
        "main [function]  1:1".to_string(),
        0,
        0,
        vec!["    1 | fn main() {}".to_string()],
    )];
    let mut palette = Palette::new(
        vec![],
        Path::new(""),
        &HashMap::new(),
        None,
        symbols,
        vec![],
    );
    palette.set_input("@".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);
    assert_eq!(palette.candidates.len(), 1);
}

#[test]
fn palette_colon_prefix_activates_goto_line_mode() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let doc_lines = vec![
        "line one".to_string(),
        "line two".to_string(),
        "line three".to_string(),
    ];
    let mut palette = Palette::new(
        vec![],
        Path::new(""),
        &HashMap::new(),
        None,
        vec![],
        doc_lines,
    );
    palette.set_input(":2".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::GotoLine);
    assert!(palette.candidates.is_empty());
}

#[test]
fn palette_mode_transition_on_prefix_change() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let symbols = vec![("main [function]  1:1".to_string(), 0, 0, vec![])];
    let files = vec!["src/main.rs".to_string()];
    let mut palette = Palette::new(files, Path::new(""), &HashMap::new(), None, symbols, vec![]);

    // Start in file mode
    palette.set_input(String::new());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::FileFinder);

    // Switch to symbol mode
    palette.set_input("@".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);

    // Switch to command mode
    palette.set_input(">".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::Command);

    // Switch to goto line mode
    palette.set_input(":".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::GotoLine);
}

#[test]
fn parse_goto_line_only() {
    let result = Palette::parse_goto_line(":42");
    assert_eq!(result, Some((41, 0)));
}

#[test]
fn parse_goto_line_and_char() {
    let result = Palette::parse_goto_line(":42:10");
    assert_eq!(result, Some((41, 9)));
}

#[test]
fn parse_goto_line_empty() {
    assert_eq!(Palette::parse_goto_line(":"), None);
    assert_eq!(Palette::parse_goto_line(": "), None);
}

#[test]
fn parse_goto_line_invalid() {
    assert_eq!(Palette::parse_goto_line(":abc"), None);
}

#[test]
fn parse_goto_line_one_based_floor() {
    // Line 0 should not underflow
    let result = Palette::parse_goto_line(":0");
    assert_eq!(result, Some((0, 0)));
    // Line 1 is the first line (0-based: 0)
    let result = Palette::parse_goto_line(":1");
    assert_eq!(result, Some((0, 0)));
}

#[test]
fn palette_symbol_filter_with_at_prefix() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let symbols = vec![
        ("main [function]  1:1".to_string(), 0, 0, vec![]),
        ("helper [function]  5:1".to_string(), 4, 0, vec![]),
    ];
    let mut palette = Palette::new(
        vec![],
        Path::new(""),
        &HashMap::new(),
        None,
        symbols,
        vec![],
    );
    palette.set_input("@hel".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);
    assert_eq!(palette.candidates.len(), 1);
    assert!(palette.candidates[0].label.contains("helper"));
}

#[test]
fn palette_unified_allows_prefix_deletion() {
    // Unified palettes allow cursor at position 0 so prefixes can be deleted
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    assert!(palette.is_unified);
    palette.set_input("@test".to_string());
    palette.input.cursor = 0;
    palette.clamp_input_cursor();
    assert_eq!(palette.input.cursor, 0);

    palette.set_input(":42".to_string());
    palette.input.cursor = 0;
    palette.clamp_input_cursor();
    assert_eq!(palette.input.cursor, 0);
}

#[test]
fn palette_standalone_protects_prefix() {
    // Standalone symbol picker protects the prefix
    let palette =
        Palette::new_symbol_picker(vec![("main [function]  1:1".to_string(), 0, 0, vec![])]);
    assert!(!palette.is_unified);
    // Standalone symbol picker has no prefix in input, so min_input_cursor is 0
    assert_eq!(palette.min_input_cursor(), 0);
}

#[test]
fn palette_goto_line_enter_dispatches_jump() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let doc_lines: Vec<String> = (0..50).map(|i| format!("line {}", i)).collect();
    let mut palette = Palette::new(
        vec![],
        Path::new(""),
        &HashMap::new(),
        None,
        vec![],
        doc_lines,
    );
    palette.set_input(":42".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    match result {
        EventResult::Action(Action::App(AppAction::Navigation(
            NavigationAction::JumpToLineChar { line, char_col },
        ))) => {
            assert_eq!(line, 41);
            assert_eq!(char_col, 0);
        }
        other => panic!("Expected JumpToLineChar, got {:?}", other),
    }
}

#[test]
fn palette_goto_line_enter_invalid_closes() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let mut palette = Palette::new(vec![], Path::new(""), &HashMap::new(), None, vec![], vec![]);
    palette.set_input(":".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);

    let result = palette.handle_key_event(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &registry,
        &lang_registry,
        &config,
    );
    match result {
        EventResult::Action(Action::Ui(UiAction::ClosePalette)) => {}
        other => panic!("Expected ClosePalette, got {:?}", other),
    }
}

#[test]
fn palette_goto_line_preview_shows_context() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let doc_lines: Vec<String> = (0..20).map(|i| format!("content line {}", i + 1)).collect();
    let mut palette = Palette::new(
        vec![],
        Path::new(""),
        &HashMap::new(),
        None,
        vec![],
        doc_lines,
    );
    palette.set_input(":10".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);

    assert!(!palette.preview_lines.is_empty());
    assert!(palette.preview_lines.iter().any(|l| l.contains("10 |")));
    assert!(palette.jump_target_preview_line.is_some());
}

#[test]
fn palette_unified_mode_transition_via_typing() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let symbols = vec![("main [function]  1:1".to_string(), 0, 0, vec![])];
    let mut palette = Palette::new(
        vec!["test.rs".to_string()],
        Path::new(""),
        &HashMap::new(),
        None,
        symbols,
        vec!["hello".to_string()],
    );
    assert!(palette.is_unified);

    // Start as command mode (default input is ">")
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::Command);

    // Type @ via refresh_after_input_edit (simulating user input)
    palette.set_input("@".to_string());
    palette.refresh_after_input_edit(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);

    // Type : for goto line
    palette.set_input(":5".to_string());
    palette.refresh_after_input_edit(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::GotoLine);

    // Clear to file mode
    palette.set_input(String::new());
    palette.refresh_after_input_edit(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::FileFinder);
}

#[test]
fn palette_backspace_deletes_prefix_and_switches_mode() {
    let registry = CommandRegistry::new();
    let lang_registry = LanguageRegistry::new();
    let config = Config::default();
    let symbols = vec![("main [function]  1:1".to_string(), 0, 0, vec![])];
    let mut palette = Palette::new(
        vec!["test.rs".to_string()],
        Path::new(""),
        &HashMap::new(),
        None,
        symbols,
        vec![],
    );

    // Start in symbol mode with "@"
    palette.set_input("@".to_string());
    palette.update_candidates(&registry, &lang_registry, &config);
    assert_eq!(palette.mode, PaletteMode::SymbolPicker);

    // Backspace removes "@", transitions to file picker
    palette.on_backspace(&registry, &lang_registry, &config);
    assert_eq!(palette.input.text, "");
    assert_eq!(palette.mode, PaletteMode::FileFinder);
}

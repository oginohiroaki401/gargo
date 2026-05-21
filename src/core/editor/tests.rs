use super::*;

#[test]
fn new_editor_has_one_scratch_buffer() {
    let ed = Editor::new();
    assert_eq!(ed.buffer_count(), 1);
    assert_eq!(ed.active_buffer().display_name(), "[scratch]");
}

#[test]
fn new_buffer_increments_id() {
    let mut ed = Editor::new();
    let id1 = ed.active_buffer_id();
    assert_eq!(id1, 1);
    let id2 = ed.new_buffer();
    assert_eq!(id2, 2);
    assert_eq!(ed.buffer_count(), 2);
    assert_eq!(ed.active_buffer_id(), 2);
}

#[test]
fn next_prev_buffer_cycles() {
    let mut ed = Editor::new();
    ed.new_buffer();
    ed.new_buffer();
    // active is buffer 3 (index 2)
    assert_eq!(ed.active_index(), 2);

    ed.next_buffer();
    assert_eq!(ed.active_index(), 0); // wraps

    ed.prev_buffer();
    assert_eq!(ed.active_index(), 2); // wraps back
}

#[test]
fn open_file_reopen_promotes_to_mru() {
    let mut ed = Editor::new();
    ed.open_file("foo.txt");
    ed.open_file("bar.txt");
    assert_eq!(ed.buffer_history, vec![2, 3]);

    ed.open_file("foo.txt");
    assert_eq!(ed.buffer_count(), 3);
    assert_eq!(ed.active_buffer_id(), 2);
    assert_eq!(ed.buffer_history, vec![3, 2]);
}

#[test]
fn prev_next_buffer_history_navigates_without_reordering() {
    let mut ed = Editor::new();
    ed.open_file("a.txt");
    ed.open_file("b.txt");
    ed.open_file("c.txt");
    let history_before = ed.buffer_history.clone();

    assert!(ed.prev_buffer_history());
    assert_eq!(ed.active_buffer_id(), 3);
    assert_eq!(ed.buffer_history, history_before);

    assert!(ed.prev_buffer_history());
    assert_eq!(ed.active_buffer_id(), 2);
    assert_eq!(ed.buffer_history, history_before);

    assert!(ed.next_buffer_history());
    assert_eq!(ed.active_buffer_id(), 3);
    assert_eq!(ed.buffer_history, history_before);
}

#[test]
fn scratch_buffer_is_excluded_from_history() {
    let mut ed = Editor::new();
    ed.open_file("a.txt");
    assert_eq!(ed.buffer_history, vec![2]);

    let scratch_id = ed.new_buffer();
    assert_eq!(ed.active_buffer_id(), scratch_id);
    assert_eq!(ed.buffer_history, vec![2]);

    assert!(ed.prev_buffer_history());
    assert_eq!(ed.active_buffer_id(), 2);
    assert!(!ed.next_buffer_history());
}

#[test]
fn close_buffer_removes_history_entry_and_navigation_skips_it() {
    let mut ed = Editor::new();
    ed.open_file("a.txt");
    ed.open_file("b.txt");
    ed.open_file("c.txt");
    assert_eq!(ed.buffer_history, vec![2, 3, 4]);

    assert!(ed.switch_to_buffer(3));
    ed.force_close_active_buffer();
    assert_eq!(ed.buffer_history, vec![2, 4]);
    assert!(!ed.buffer_history.contains(&3));

    assert!(ed.prev_buffer_history());
    assert_eq!(ed.active_buffer_id(), 2);
    assert!(ed.next_buffer_history());
    assert_eq!(ed.active_buffer_id(), 4);
}

#[test]
fn switch_to_buffer_by_id() {
    let mut ed = Editor::new();
    ed.new_buffer();
    ed.new_buffer();
    assert!(ed.switch_to_buffer(1));
    assert_eq!(ed.active_index(), 0);
    assert!(!ed.switch_to_buffer(999)); // nonexistent
}

#[test]
fn close_buffer_removes_it() {
    let mut ed = Editor::new();
    ed.new_buffer();
    assert_eq!(ed.buffer_count(), 2);
    ed.close_active_buffer().unwrap();
    assert_eq!(ed.buffer_count(), 1);
}

#[test]
fn close_last_buffer_replaces_with_scratch() {
    let mut ed = Editor::new();
    ed.close_active_buffer().unwrap();
    assert_eq!(ed.buffer_count(), 1);
    assert_eq!(ed.active_buffer().display_name(), "[scratch]");
}

#[test]
fn close_dirty_buffer_fails() {
    let mut ed = Editor::new();
    ed.active_buffer_mut().dirty = true;
    let result = ed.close_active_buffer();
    assert!(result.is_err());
}

#[test]
fn open_file_deduplicates() {
    let mut ed = Editor::new();
    ed.open_file("foo.txt");
    ed.open_file("bar.txt");
    assert_eq!(ed.buffer_count(), 3);
    ed.open_file("foo.txt"); // should not create new buffer
    assert_eq!(ed.buffer_count(), 3);
}

#[test]
fn jumplist_records_transition_and_navigates() {
    let mut ed = editor_with_text("a\nb\nc\n");
    ed.active_buffer_mut().set_cursor_line_char(0, 0);
    let before = ed.current_jump_location();
    ed.active_buffer_mut().set_cursor_line_char(2, 0);
    let after = ed.current_jump_location();

    ed.record_jump_transition(before, after);
    assert_eq!(ed.jump_list_entries().len(), 2);
    assert_eq!(ed.jump_list_index(), Some(1));

    ed.jump_older().unwrap();
    assert_eq!(ed.active_buffer().cursor_line(), 0);
    assert_eq!(ed.jump_list_index(), Some(0));

    ed.jump_newer().unwrap();
    assert_eq!(ed.active_buffer().cursor_line(), 2);
    assert_eq!(ed.jump_list_index(), Some(1));
}

#[test]
fn jumplist_deduplicates_equivalent_locations() {
    let mut ed = editor_with_text("hello\n");
    let loc = ed.current_jump_location();
    ed.push_jump_location(loc.clone());
    ed.push_jump_location(loc);
    assert_eq!(ed.jump_list_entries().len(), 1);
}

#[test]
fn jumplist_rejects_invalid_index() {
    let mut ed = Editor::new();
    let err = ed.jump_to_list_index(0).unwrap_err();
    assert!(err.contains("Invalid jump location"));
}

// -------------------------------------------------------
// Search helpers
// -------------------------------------------------------

fn editor_with_text(s: &str) -> Editor {
    let mut ed = Editor::new();
    ed.active_buffer_mut().rope = ropey::Rope::from_str(s);
    ed
}

// -------------------------------------------------------
// search_update tests
// -------------------------------------------------------

#[test]
fn search_update_moves_cursor_to_first_match_from_anchor() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    assert_eq!(ed.active_buffer().cursors[0], 0);
    assert!(ed.search.last_search_found);
}

#[test]
fn search_update_searches_forward_from_anchor() {
    let mut ed = editor_with_text("hello world hello test hello");
    ed.search.set_anchor(1);
    ed.search_update("hello");
    // First match at or after anchor=1 is at offset 12.
    assert_eq!(ed.active_buffer().cursors[0], 12);
}

#[test]
fn search_update_wraps_to_top_when_nothing_after_anchor() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(15);
    ed.search_update("hello");
    // No match at or after 15, wraps to 0.
    assert_eq!(ed.active_buffer().cursors[0], 0);
}

#[test]
fn search_update_case_insensitive() {
    let mut ed = editor_with_text("Hello HELLO hello");
    ed.search.set_anchor(3);
    ed.search_update("hello");
    // First match at or after anchor=3 is "HELLO" at offset 6.
    assert_eq!(ed.active_buffer().cursors[0], 6);
    assert!(ed.search.last_search_found);
}

#[test]
fn search_update_empty_pattern() {
    let mut ed = editor_with_text("hello world");
    ed.search_update("");
    assert!(!ed.search.last_search_found);
    assert!(ed.search.pattern_lower.is_empty());
}

#[test]
fn search_update_no_match() {
    let mut ed = editor_with_text("hello world");
    let original_cursor = ed.active_buffer().cursors[0];
    ed.search_update("xyz");
    assert!(!ed.search.last_search_found);
    assert_eq!(ed.active_buffer().cursors[0], original_cursor);
}

#[test]
fn search_update_japanese() {
    let mut ed = editor_with_text("竹取の翁といふものありけり");
    ed.search.set_anchor(0);
    ed.search_update("翁");
    assert_eq!(ed.active_buffer().cursors[0], 3); // char offset of '翁'
    assert!(ed.search.last_search_found);
}

#[test]
fn search_update_non_overlapping_via_next() {
    let mut ed = editor_with_text("aaaa");
    ed.search.set_anchor(0);
    ed.search_update("aa");
    assert_eq!(ed.active_buffer().cursors[0], 0);
    ed.search_next();
    assert_eq!(ed.active_buffer().cursors[0], 2);
}

// -------------------------------------------------------
// search_next tests
// -------------------------------------------------------

#[test]
fn search_next_moves_to_first_match_after_cursor() {
    let mut ed = editor_with_text("hello world hello test hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // search_update parked the cursor at 0; search_next should advance to 12.
    ed.search_next();
    assert_eq!(ed.active_buffer().cursors[0], 12);
}

#[test]
fn search_next_wraps_around() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    ed.active_buffer_mut().cursors[0] = 16;
    ed.search_next();
    assert_eq!(ed.active_buffer().cursors[0], 0); // wraps to first
}

#[test]
fn search_next_no_matches() {
    let mut ed = editor_with_text("hello world");
    ed.search_update("xyz");
    ed.active_buffer_mut().cursors[0] = 0;
    let returned = ed.search_next();
    assert!(!returned);
    assert_eq!(ed.active_buffer().cursors[0], 0); // unchanged
}

#[test]
fn search_next_from_match_position() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // Cursor at 0 after search_update; search_next should go to 12.
    ed.search_next();
    assert_eq!(ed.active_buffer().cursors[0], 12);
}

// -------------------------------------------------------
// search_prev tests
// -------------------------------------------------------

#[test]
fn search_prev_moves_to_last_match_before_cursor() {
    let mut ed = editor_with_text("hello world hello test hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    ed.active_buffer_mut().cursors[0] = 13;
    ed.search_prev();
    assert_eq!(ed.active_buffer().cursors[0], 12);
}

#[test]
fn search_prev_wraps_around() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    ed.active_buffer_mut().cursors[0] = 0;
    ed.search_prev();
    assert_eq!(ed.active_buffer().cursors[0], 12);
}

#[test]
fn search_prev_no_matches() {
    let mut ed = editor_with_text("hello world");
    ed.search_update("xyz");
    ed.active_buffer_mut().cursors[0] = 5;
    let returned = ed.search_prev();
    assert!(!returned);
    assert_eq!(ed.active_buffer().cursors[0], 5); // unchanged
}

#[test]
fn add_cursor_to_next_search_match_adds_secondary_cursor() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // search_update parked cursor at 0; adding next match yields 12.
    assert!(ed.add_cursor_to_next_search_match());
    assert_eq!(ed.active_buffer().cursors, vec![0, 12]);
}

#[test]
fn add_cursor_to_prev_search_match_wraps_and_adds_secondary_cursor() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // Cursor at 0; prev wraps to the last match (12).
    assert!(ed.add_cursor_to_prev_search_match());
    assert_eq!(ed.active_buffer().cursors, vec![0, 12]);
}

#[test]
fn add_cursor_to_next_search_match_skips_existing_cursor_positions() {
    let mut ed = editor_with_text("hello world hello test hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    assert!(ed.add_cursor_to_next_search_match());
    assert!(ed.add_cursor_to_next_search_match());
    assert!(!ed.add_cursor_to_next_search_match());
    let mut sorted = ed.active_buffer().cursors.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 12, 23]);
}

#[test]
fn add_cursor_to_next_search_match_treats_cursor_inside_match_as_occupied() {
    let mut ed = editor_with_text("hello world hello");
    ed.active_buffer_mut().cursors[0] = 2;
    ed.search.set_anchor(2);
    ed.search_update("hello");
    // search_update from anchor=2 finds the next match at 12 (cursor moves
    // there). To recreate the "cursor inside first match" scenario, override
    // the cursor back to inside the first match before calling add_cursor.
    ed.active_buffer_mut().cursors[0] = 2;
    assert!(ed.add_cursor_to_next_search_match());
    assert!(!ed.add_cursor_to_next_search_match());
    let mut sorted = ed.active_buffer().cursors.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![2, 12]);
}

#[test]
fn add_cursor_to_prev_search_match_treats_cursor_inside_match_as_occupied() {
    let mut ed = editor_with_text("hello world hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // Re-park cursor inside the last match.
    ed.active_buffer_mut().cursors[0] = 14;
    assert!(ed.add_cursor_to_prev_search_match());
    assert!(!ed.add_cursor_to_prev_search_match());
    let mut sorted = ed.active_buffer().cursors.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 14]);
}

#[test]
fn add_cursor_to_all_search_matches_adds_every_unoccupied_match() {
    let mut ed = editor_with_text("foo bar foo baz foo");
    ed.search.set_anchor(0);
    ed.search_update("foo");
    let added = ed.add_cursor_to_all_search_matches();
    assert_eq!(added, 2);
    let mut sorted = ed.active_buffer().cursors.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 8, 16]);
}

#[test]
fn add_cursor_to_all_search_matches_returns_zero_when_all_occupied() {
    let mut ed = editor_with_text("foo bar foo");
    ed.search.set_anchor(0);
    ed.search_update("foo");
    // First pass adds the second occurrence; second pass should add nothing.
    assert_eq!(ed.add_cursor_to_all_search_matches(), 1);
    assert_eq!(ed.add_cursor_to_all_search_matches(), 0);
}

#[test]
fn add_cursor_to_all_search_matches_with_no_matches_returns_zero() {
    let mut ed = editor_with_text("foo bar baz");
    ed.search_update("qux");
    assert_eq!(ed.add_cursor_to_all_search_matches(), 0);
}

#[test]
fn add_cursor_to_all_search_matches_skips_cursor_inside_match() {
    let mut ed = editor_with_text("hello world hello hello");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    // Place the primary cursor inside the first match.
    ed.active_buffer_mut().cursors[0] = 2;
    let added = ed.add_cursor_to_all_search_matches();
    assert_eq!(added, 2);
    let mut sorted = ed.active_buffer().cursors.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![2, 12, 18]);
}

// -------------------------------------------------------
// Integration tests
// -------------------------------------------------------

#[test]
fn search_next_then_prev_round_trip() {
    let mut ed = editor_with_text("aaa bbb aaa bbb aaa");
    ed.search.set_anchor(0);
    ed.search_update("bbb");
    // search_update parks the cursor at the first match (4).
    assert_eq!(ed.active_buffer().cursors[0], 4);
    ed.search_next();
    assert_eq!(ed.active_buffer().cursors[0], 12);
    ed.search_prev();
    assert_eq!(ed.active_buffer().cursors[0], 4);
}

#[test]
fn search_update_replaces_previous_pattern() {
    let mut ed = editor_with_text("hello world foo");
    ed.search.set_anchor(0);
    ed.search_update("hello");
    assert_eq!(ed.active_buffer().cursors[0], 0);
    assert!(ed.search.last_search_found);
    ed.search_update("foo");
    assert_eq!(ed.search.pattern, "foo");
    assert_eq!(ed.active_buffer().cursors[0], 12);
    assert!(ed.search.last_search_found);
}

// -------------------------------------------------------
// Lazy primitive tests (chunk boundaries, wrap, multi-byte)
// -------------------------------------------------------

#[test]
fn find_match_forward_finds_first_after_position() {
    use ropey::Rope;
    let rope = Rope::from_str("hello world hello");
    let hit = crate::core::editor::search::find_match_forward(&rope, "hello", 1, true).unwrap();
    assert_eq!(hit, 12);
}

#[test]
fn find_match_forward_wraps_to_top() {
    use ropey::Rope;
    let rope = Rope::from_str("foo bar foo");
    let hit = crate::core::editor::search::find_match_forward(&rope, "foo", 9, true).unwrap();
    assert_eq!(hit, 0);
}

#[test]
fn find_match_backward_finds_last_before_position() {
    use ropey::Rope;
    let rope = Rope::from_str("hello world hello test hello");
    let hit = crate::core::editor::search::find_match_backward(&rope, "hello", 26, true).unwrap();
    assert_eq!(hit, 23);
}

#[test]
fn find_match_backward_wraps_to_bottom() {
    use ropey::Rope;
    let rope = Rope::from_str("foo bar foo");
    let hit = crate::core::editor::search::find_match_backward(&rope, "foo", 0, true).unwrap();
    assert_eq!(hit, 8);
}

#[test]
fn find_matches_in_range_honors_limit() {
    use ropey::Rope;
    let rope = Rope::from_str("aaaaa"); // overlapping-but-non-overlap-matched 'a'
    let hits = crate::core::editor::search::find_matches_in_range(&rope, "a", 0, 5, 3);
    assert_eq!(hits.len(), 3);
    assert_eq!(hits, vec![0, 1, 2]);
}

#[test]
fn find_match_forward_across_chunk_boundary() {
    use ropey::Rope;
    // Build a rope large enough that ropey will split it into multiple
    // chunks. The pattern is placed precisely in the middle so that
    // (with high probability) it straddles a chunk boundary.
    let mut s = String::with_capacity(8192);
    for _ in 0..4000 {
        s.push('x');
    }
    s.push_str("needle");
    for _ in 0..4000 {
        s.push('x');
    }
    let rope = Rope::from_str(&s);
    let hit = crate::core::editor::search::find_match_forward(&rope, "needle", 0, false).unwrap();
    assert_eq!(hit, 4000);
}

#[test]
fn find_match_forward_japanese() {
    use ropey::Rope;
    let rope = Rope::from_str("竹取の翁といふものありけり");
    let hit = crate::core::editor::search::find_match_forward(&rope, "翁", 0, false).unwrap();
    assert_eq!(hit, 3);
}

// -------------------------------------------------------
// Search history tests
// -------------------------------------------------------

#[test]
fn push_history_stores_entries() {
    let mut s = SearchState::new();
    s.push_history("foo");
    s.push_history("bar");
    assert_eq!(s.history, vec!["foo", "bar"]);
}

#[test]
fn push_history_deduplicates_consecutive() {
    let mut s = SearchState::new();
    s.push_history("foo");
    s.push_history("foo");
    s.push_history("bar");
    s.push_history("bar");
    s.push_history("foo");
    assert_eq!(s.history, vec!["foo", "bar", "foo"]);
}

#[test]
fn push_history_ignores_empty() {
    let mut s = SearchState::new();
    s.push_history("");
    assert!(s.history.is_empty());
}

#[test]
fn history_prev_navigates_older() {
    let mut s = SearchState::new();
    s.push_history("alpha");
    s.push_history("beta");
    s.push_history("gamma");

    // First prev → newest entry
    assert_eq!(s.history_prev("current"), Some("gamma".to_string()));
    assert_eq!(s.history_prev("current"), Some("beta".to_string()));
    assert_eq!(s.history_prev("current"), Some("alpha".to_string()));
    // At oldest → None
    assert_eq!(s.history_prev("current"), None);
}

#[test]
fn history_next_navigates_newer() {
    let mut s = SearchState::new();
    s.push_history("alpha");
    s.push_history("beta");

    // Go to oldest
    s.history_prev("typed");
    s.history_prev("typed");
    assert_eq!(s.history_index, Some(0));

    // Navigate forward
    assert_eq!(s.history_next(), Some("beta".to_string()));
    // Past newest → restore saved input
    assert_eq!(s.history_next(), Some("typed".to_string()));
    assert_eq!(s.history_index, None);
}

#[test]
fn history_next_returns_none_when_not_browsing() {
    let mut s = SearchState::new();
    s.push_history("foo");
    assert_eq!(s.history_next(), None);
}

#[test]
fn history_prev_returns_none_when_empty() {
    let mut s = SearchState::new();
    assert_eq!(s.history_prev("anything"), None);
}

#[test]
fn input_before_history_preserved() {
    let mut s = SearchState::new();
    s.push_history("old1");
    s.push_history("old2");

    // Start browsing with user text "partial"
    s.history_prev("partial");
    assert_eq!(s.input_before_history, "partial");

    // Navigate back to user text
    s.history_prev("partial"); // → old1
    s.history_next(); // → old2
    s.history_next(); // → "partial"
    assert_eq!(s.history_next(), None); // not browsing anymore
}

#[test]
fn reset_history_browse_clears_state() {
    let mut s = SearchState::new();
    s.push_history("foo");
    s.history_prev("bar");
    assert!(s.history_index.is_some());
    s.reset_history_browse();
    assert_eq!(s.history_index, None);
    assert!(s.input_before_history.is_empty());
}

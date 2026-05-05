use super::*;

impl Palette {
    pub fn new(
        files: Vec<String>,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
        command_history: Option<Rc<CommandHistory>>,
        symbols: Vec<(String, usize, usize, Vec<String>)>,
        active_doc_lines: Vec<String>,
    ) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PreviewRequest>();
        let (res_tx, res_rx) = mpsc::channel::<PreviewResult>();
        let root = project_root.to_path_buf();
        let handle = thread::spawn(move || {
            workers::preview_worker(req_rx, res_tx, root);
        });

        let symbol_entries: Vec<SymbolEntry> = symbols
            .into_iter()
            .map(|(label, line, char_col, preview_lines)| SymbolEntry {
                label,
                line,
                char_col,
                preview_lines,
                copy_text: None,
            })
            .collect();

        Self {
            input: TextInput::new(">".into(), 1),
            mode: PaletteMode::Command,
            candidates: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries,
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: files,
            project_root: project_root.to_path_buf(),
            request_tx: Some(req_tx),
            result_rx: Some(res_rx),
            _worker: Some(handle),
            requested_paths: HashSet::new(),
            git_status_map: git_status_map.clone(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines,
            is_unified: true,
            caller_label: None,
        }
    }

    pub fn new_buffer_picker(entries: Vec<(BufferId, String, Vec<String>)>) -> Self {
        let candidates = entries
            .iter()
            .map(|(id, name, _)| ScoredCandidate {
                kind: CandidateKind::Buffer(*id),
                label: name.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::BufferPicker,
            candidates,
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: entries,
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: Some(LanguageRegistry::new()),
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: None,
        };
        palette.update_buffer_preview();
        palette
    }

    pub fn new_jump_picker(entries: Vec<JumpPickerEntry>) -> Self {
        let jump_entries: Vec<JumpEntry> = entries
            .into_iter()
            .map(|entry| JumpEntry {
                jump_index: entry.jump_index,
                label: entry.label,
                preview_lines: entry.preview_lines,
                source_path: entry.source_path,
                target_preview_line: entry.target_preview_line,
                target_char_col: entry.target_char_col,
            })
            .collect();
        let candidates = jump_entries
            .iter()
            .map(|entry| ScoredCandidate {
                kind: CandidateKind::Jump(entry.jump_index),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::JumpPicker,
            candidates,
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries,
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: None,
        };
        palette.update_jump_preview();
        palette
    }

    pub fn new_reference_picker(caller_label: String, entries: Vec<ReferencePickerEntry>) -> Self {
        let reference_entries: Vec<ReferenceEntry> = entries
            .into_iter()
            .map(|entry| ReferenceEntry {
                label: entry.label,
                path: entry.path,
                line: entry.line,
                character_utf16: entry.character_utf16,
                preview_lines: entry.preview_lines,
                source_path: entry.source_path,
                target_preview_line: entry.target_preview_line,
                target_char_col: entry.target_char_col,
            })
            .collect();
        let candidates = reference_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| ScoredCandidate {
                kind: CandidateKind::Reference(idx),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::ReferencePicker,
            candidates,
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries,
            git_branch_entries: Vec::new(),
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: Some(caller_label),
        };
        palette.update_reference_preview();
        palette
    }

    pub fn new_git_branch_picker(entries: Vec<GitBranchPickerEntry>) -> Self {
        let git_branch_entries: Vec<GitBranchEntry> = entries
            .into_iter()
            .map(|entry| GitBranchEntry {
                branch_name: entry.branch_name,
                label: entry.label,
                preview_lines: entry.preview_lines,
            })
            .collect();
        let selected = git_branch_entries
            .iter()
            .position(|entry| entry.label.starts_with("* "))
            .unwrap_or(0);
        let candidates = git_branch_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| ScoredCandidate {
                kind: CandidateKind::GitBranch(idx),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::GitBranchPicker,
            candidates,
            selected,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries,
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: Some("Git Branches".to_string()),
        };
        palette.update_git_branch_preview();
        palette
    }

    pub fn new_git_branch_compare_picker(entries: Vec<GitBranchPickerEntry>) -> Self {
        let git_branch_entries: Vec<GitBranchEntry> = entries
            .into_iter()
            .map(|entry| GitBranchEntry {
                branch_name: entry.branch_name,
                label: entry.label,
                preview_lines: entry.preview_lines,
            })
            .collect();
        let selected = git_branch_entries
            .iter()
            .position(|entry| entry.label.starts_with("* "))
            .unwrap_or(0);
        let candidates = git_branch_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| ScoredCandidate {
                kind: CandidateKind::GitBranch(idx),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::GitBranchComparePicker,
            candidates,
            selected,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries,
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: Some("Compare Branch".to_string()),
        };
        palette.update_git_branch_preview();
        palette
    }

    pub fn new_symbol_picker(entries: Vec<(String, usize, usize, Vec<String>)>) -> Self {
        let symbol_entries: Vec<SymbolEntry> = entries
            .into_iter()
            .map(|(label, line, char_col, preview_lines)| SymbolEntry {
                label,
                line,
                char_col,
                preview_lines,
                copy_text: None,
            })
            .collect();
        let candidates = symbol_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| ScoredCandidate {
                kind: CandidateKind::Symbol(idx),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::SymbolPicker,
            candidates,
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries,
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: None,
        };
        palette.update_symbol_preview();
        palette
    }

    pub fn new_smart_copy_picker(entries: Vec<SmartCopyPickerEntry>) -> Self {
        let symbol_entries: Vec<SymbolEntry> = entries
            .into_iter()
            .map(|entry| SymbolEntry {
                label: entry.label,
                line: entry.line,
                char_col: entry.char_col,
                preview_lines: entry.preview_lines,
                copy_text: Some(entry.copy_text),
            })
            .collect();
        let candidates = symbol_entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| ScoredCandidate {
                kind: CandidateKind::Symbol(idx),
                label: entry.label.clone(),
                score: 0,
                match_positions: Vec::new(),
                preview_lines: Vec::new(),
            })
            .collect();
        let mut palette = Self {
            input: TextInput::default(),
            mode: PaletteMode::SymbolPicker,
            candidates,
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries,
            symbol_submit_behavior: SymbolSubmitBehavior::CopyToClipboard,
            file_entries: Vec::new(),
            project_root: PathBuf::new(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: HashMap::new(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: Vec::new(),
            global_search_entries: Vec::new(),
            global_search_request_tx: None,
            global_search_result_rx: None,
            _global_search_worker: None,
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: false,
            global_search_changed_at: None,
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: None,
        };
        palette.update_symbol_preview();
        palette
    }

    pub fn set_input(&mut self, input: String) {
        self.input.set_text(input);
    }

    pub fn new_global_search(
        files: Vec<String>,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
        unsaved_buffers: Vec<GlobalSearchBufferSource>,
    ) -> Self {
        let (search_req_tx, search_req_rx) = mpsc::channel::<GlobalSearchRequest>();
        let (search_res_tx, search_res_rx) = mpsc::channel::<GlobalSearchBatch>();
        let root = project_root.to_path_buf();
        let worker_files = files.clone();
        let worker_unsaved_buffers = unsaved_buffers.clone();
        let search_handle = thread::spawn(move || {
            workers::global_search_worker(
                search_req_rx,
                search_res_tx,
                root,
                worker_files,
                worker_unsaved_buffers,
            );
        });

        Self {
            input: TextInput::default(),
            mode: PaletteMode::GlobalSearch,
            candidates: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            preview_lines: Vec::new(),
            preview_spans: HashMap::new(),
            preview_cache: HashMap::new(),
            buffer_entries: Vec::new(),
            jump_entries: Vec::new(),
            reference_entries: Vec::new(),
            git_branch_entries: Vec::new(),
            symbol_entries: Vec::new(),
            symbol_submit_behavior: SymbolSubmitBehavior::JumpToLocation,
            file_entries: files,
            project_root: project_root.to_path_buf(),
            request_tx: None,
            result_rx: None,
            _worker: None,
            requested_paths: HashSet::new(),
            git_status_map: git_status_map.clone(),
            last_previewed_buffer: None,
            last_previewed_jump_index: None,
            last_previewed_reference_index: None,
            last_previewed_git_branch_index: None,
            last_previewed_symbol_index: None,
            last_previewed_search_index: None,
            jump_target_preview_line: None,
            jump_target_char_col: None,
            buffer_highlight_cache: HashMap::new(),
            reference_highlight_cache: HashMap::new(),
            lang_registry_owned: None,
            command_history: None,
            global_search_unsaved_buffers: unsaved_buffers,
            global_search_entries: Vec::new(),
            global_search_request_tx: Some(search_req_tx),
            global_search_result_rx: Some(search_res_rx),
            _global_search_worker: Some(search_handle),
            global_search_generation: 0,
            global_search_latest_applied: 0,
            global_search_dirty: true,
            global_search_changed_at: Some(Instant::now()),
            active_doc_lines: Vec::new(),
            is_unified: false,
            caller_label: None,
        }
    }
}

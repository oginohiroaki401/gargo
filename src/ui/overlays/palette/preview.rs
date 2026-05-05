use super::*;

impl Palette {
    pub fn update_preview(&mut self, lang_registry: &LanguageRegistry, config: &Config) {
        let t_total = Instant::now();
        self.preview_lines.clear();
        self.preview_spans.clear();
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;
        if self.mode == PaletteMode::GlobalSearch {
            self.pump_global_search();
            self.update_global_search_preview();
            return;
        }
        if self.mode == PaletteMode::GotoLine {
            self.update_goto_line_preview();
            return;
        }
        if self.mode == PaletteMode::BufferPicker {
            self.update_buffer_preview();
            return;
        }
        if self.mode == PaletteMode::JumpPicker {
            self.update_jump_preview();
            return;
        }
        if self.mode == PaletteMode::ReferencePicker {
            self.update_reference_preview();
            return;
        }
        if self.mode == PaletteMode::GitBranchPicker {
            self.update_git_branch_preview();
            return;
        }
        if self.mode == PaletteMode::SymbolPicker {
            self.update_symbol_preview();
            return;
        }
        if self.mode == PaletteMode::Command {
            self.preview_lines = self
                .candidates
                .get(self.selected)
                .map(|c| c.preview_lines.clone())
                .unwrap_or_default();
            return;
        }
        self.last_previewed_buffer = None;
        self.last_previewed_jump_index = None;
        self.last_previewed_reference_index = None;
        self.last_previewed_git_branch_index = None;
        self.last_previewed_symbol_index = None;
        if self.mode != PaletteMode::FileFinder {
            return;
        }

        // Drain any completed background results into cache
        self.drain_preview_results();

        if let Some(rel_path) = self
            .candidates
            .get(self.selected)
            .and_then(|c| match c.kind {
                CandidateKind::File(idx) => Some(&self.file_entries[idx]),
                _ => None,
            })
        {
            let rel_path = rel_path.clone();

            // Check cache first (includes background-generated results)
            if let Some(cached) = self.preview_cache.get(&rel_path) {
                self.preview_lines = cached.0.clone();
                self.preview_spans = cached.1.clone();
                debug_log!(
                    config,
                    "preview: file={} cache hit total={}us",
                    rel_path,
                    t_total.elapsed().as_micros()
                );
                self.schedule_nearby_previews(config);
                return;
            }

            // Sync fallback: generate preview on main thread for current selection
            let full_path = self.project_root.join(&rel_path);

            let t_read = Instant::now();
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                let read_us = t_read.elapsed().as_micros();

                self.preview_lines = content.lines().take(200).map(|l| l.to_string()).collect();

                if let Some(lang_def) = lang_registry.detect_by_extension(&rel_path) {
                    let preview_text: String = self.preview_lines.join("\n");

                    let t_hl = Instant::now();
                    self.preview_spans = highlight_text(&preview_text, lang_def);
                    let hl_us = t_hl.elapsed().as_micros();

                    debug_log!(
                        config,
                        "preview: file={} size={} read={}us highlight={}us total={}us (sync fallback)",
                        rel_path,
                        content.len(),
                        read_us,
                        hl_us,
                        t_total.elapsed().as_micros()
                    );
                } else {
                    debug_log!(
                        config,
                        "preview: file={} size={} read={}us (no lang) total={}us (sync fallback)",
                        rel_path,
                        content.len(),
                        read_us,
                        t_total.elapsed().as_micros()
                    );
                }

                // Store in cache
                self.preview_cache.insert(
                    rel_path,
                    (self.preview_lines.clone(), self.preview_spans.clone()),
                );
            }
        }

        // Schedule nearby previews for background generation
        self.schedule_nearby_previews(config);
    }

    pub(super) fn drain_preview_results(&mut self) {
        let Some(ref rx) = self.result_rx else { return };
        while let Ok(result) = rx.try_recv() {
            self.preview_cache
                .insert(result.rel_path, (result.lines, result.spans));
        }
    }

    pub(super) fn schedule_nearby_previews(&mut self, config: &Config) {
        let Some(ref tx) = self.request_tx else {
            return;
        };
        if self.candidates.is_empty() {
            return;
        }

        let start = self.selected.saturating_sub(5);
        let end = (self.selected + 15).min(self.candidates.len());
        let mut count = 0;

        for idx in start..end {
            if let Some(rel_path) = self.candidates.get(idx).and_then(|c| match c.kind {
                CandidateKind::File(i) => Some(&self.file_entries[i]),
                _ => None,
            }) {
                let rel_path = rel_path.clone();
                if self.preview_cache.contains_key(&rel_path)
                    || self.requested_paths.contains(&rel_path)
                {
                    continue;
                }
                self.requested_paths.insert(rel_path.clone());
                if tx.send(PreviewRequest { rel_path }).is_err() {
                    break;
                }
                count += 1;
            }
        }

        if count > 0 {
            debug_log!(config, "preview: scheduled {} nearby previews", count);
        }
    }

    pub(super) fn update_global_search_preview(&mut self) {
        let entry_idx = match self.candidates.get(self.selected).map(|c| &c.kind) {
            Some(CandidateKind::SearchResult(idx)) => *idx,
            _ => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.jump_target_preview_line = None;
                self.jump_target_char_col = None;
                self.last_previewed_search_index = None;
                return;
            }
        };

        if self.last_previewed_search_index == Some(entry_idx) {
            return;
        }

        let Some(entry) = self.global_search_entries.get(entry_idx).cloned() else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.jump_target_preview_line = None;
            self.jump_target_char_col = None;
            self.last_previewed_search_index = None;
            return;
        };

        self.preview_spans.clear();
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;

        let header = entry.preview_lines.first().cloned().unwrap_or_default();
        let is_files_entry = header.starts_with("[files]");
        let is_unsaved_entry = header.starts_with("[unsaved]");

        let content_opt: Option<String> = if is_unsaved_entry {
            self.global_search_unsaved_buffers
                .iter()
                .find(|buf| buf.path.to_string_lossy() == entry.rel_path)
                .map(|buf| buf.content.clone())
        } else {
            let path = PathBuf::from(&entry.rel_path);
            let full = if path.is_absolute() {
                path
            } else {
                self.project_root.join(&path)
            };
            std::fs::read_to_string(&full).ok()
        };

        let Some(content) = content_opt else {
            self.preview_lines = entry.preview_lines.clone();
            self.last_previewed_search_index = Some(entry_idx);
            return;
        };

        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            self.preview_lines = entry.preview_lines.clone();
            self.last_previewed_search_index = Some(entry_idx);
            return;
        }

        const PREVIEW_HALF_WINDOW: usize = 80;
        const FILES_PREVIEW_LINES: usize = 200;

        let (start, end) = if is_files_entry {
            (0, FILES_PREVIEW_LINES.min(lines.len()))
        } else {
            let s = entry.line.saturating_sub(PREVIEW_HALF_WINDOW);
            let e = (entry.line + PREVIEW_HALF_WINDOW + 1).min(lines.len());
            (s, e)
        };

        let mut preview = Vec::with_capacity(end.saturating_sub(start) + 1);
        preview.push(header);
        for (offset, line) in lines[start..end].iter().enumerate() {
            preview.push(format!("{:>5} | {}", start + offset + 1, line));
        }

        if !is_files_entry && entry.line >= start && entry.line < end {
            self.jump_target_preview_line = Some(1 + (entry.line - start));
            self.jump_target_char_col = Some(entry.char_col);
        }

        self.preview_lines = preview;

        let lang_registry = self
            .lang_registry_owned
            .get_or_insert_with(LanguageRegistry::new);
        if let Some(lang_def) = lang_registry.detect_by_extension(&entry.rel_path) {
            let mut code_lines = Vec::new();
            let mut line_map: Vec<(usize, usize)> = Vec::new();
            for (preview_idx, line) in self.preview_lines.iter().enumerate().skip(1) {
                if let Some((_, code)) = split_numbered_preview_line(line) {
                    let prefix_len = line.len().saturating_sub(code.len());
                    code_lines.push(code.to_string());
                    line_map.push((preview_idx, prefix_len));
                } else {
                    code_lines.push(line.clone());
                    line_map.push((preview_idx, 0));
                }
            }
            if !code_lines.is_empty() {
                let preview_text = code_lines.join("\n");
                let raw_spans = highlight_text(&preview_text, lang_def);
                for (line_idx, spans) in raw_spans {
                    if let Some((preview_idx, prefix_len)) = line_map.get(line_idx).copied() {
                        self.preview_spans.insert(
                            preview_idx,
                            spans
                                .into_iter()
                                .map(|span| HighlightSpan {
                                    start: span.start + prefix_len,
                                    end: span.end + prefix_len,
                                    capture_name: span.capture_name,
                                })
                                .collect(),
                        );
                    }
                }
            }
        }

        self.last_previewed_search_index = Some(entry_idx);
    }

    pub(super) fn update_buffer_preview(&mut self) {
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;
        let selected_id = match self.candidates.get(self.selected) {
            Some(c) => match c.kind {
                CandidateKind::Buffer(id) => id,
                _ => {
                    self.preview_lines.clear();
                    self.preview_spans.clear();
                    self.last_previewed_buffer = None;
                    return;
                }
            },
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_buffer = None;
                return;
            }
        };

        // Skip redundant work if the selected buffer hasn't changed
        if self.last_previewed_buffer == Some(selected_id) {
            return;
        }

        let entry = self
            .buffer_entries
            .iter()
            .find(|(id, _, _)| *id == selected_id);
        let (_, name, lines) = match entry {
            Some(e) => e,
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_buffer = None;
                return;
            }
        };

        self.preview_lines = lines.clone();

        // Check highlight cache first
        if let Some(cached_spans) = self.buffer_highlight_cache.get(&selected_id) {
            self.preview_spans = cached_spans.clone();
        } else {
            let lang_registry = self
                .lang_registry_owned
                .get_or_insert_with(LanguageRegistry::new);
            if let Some(lang_def) = lang_registry.detect_by_extension(name) {
                let preview_text: String = self.preview_lines.join("\n");
                let spans = highlight_text(&preview_text, lang_def);
                self.buffer_highlight_cache
                    .insert(selected_id, spans.clone());
                self.preview_spans = spans;
            } else {
                self.preview_spans.clear();
            }
        }

        self.last_previewed_buffer = Some(selected_id);
    }

    pub(super) fn update_jump_preview(&mut self) {
        let selected_jump_index = match self.candidates.get(self.selected) {
            Some(c) => match c.kind {
                CandidateKind::Jump(idx) => idx,
                _ => {
                    self.preview_lines.clear();
                    self.preview_spans.clear();
                    self.last_previewed_jump_index = None;
                    self.jump_target_preview_line = None;
                    self.jump_target_char_col = None;
                    return;
                }
            },
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_jump_index = None;
                self.jump_target_preview_line = None;
                self.jump_target_char_col = None;
                return;
            }
        };

        if self.last_previewed_jump_index == Some(selected_jump_index) {
            return;
        }

        if let Some(entry) = self
            .jump_entries
            .iter()
            .find(|entry| entry.jump_index == selected_jump_index)
        {
            self.preview_lines = entry.preview_lines.clone();
            self.jump_target_preview_line = entry.target_preview_line;
            self.jump_target_char_col = Some(entry.target_char_col);
            self.preview_spans.clear();
            if let Some(source_path) = entry.source_path.as_deref() {
                let lang_registry = self
                    .lang_registry_owned
                    .get_or_insert_with(LanguageRegistry::new);
                if let Some(lang_def) = lang_registry.detect_by_extension(source_path) {
                    let mut code_lines = Vec::new();
                    let mut line_map: Vec<(usize, usize)> = Vec::new();
                    for (preview_idx, line) in self.preview_lines.iter().enumerate().skip(1) {
                        if let Some((_, code)) = split_numbered_preview_line(line) {
                            let prefix_len = line.len().saturating_sub(code.len());
                            code_lines.push(code.to_string());
                            line_map.push((preview_idx, prefix_len));
                        } else {
                            code_lines.push(line.clone());
                            line_map.push((preview_idx, 0));
                        }
                    }
                    if !code_lines.is_empty() {
                        let preview_text = code_lines.join("\n");
                        let raw_spans = highlight_text(&preview_text, lang_def);
                        for (line_idx, spans) in raw_spans {
                            if let Some((preview_idx, prefix_len)) = line_map.get(line_idx).copied()
                            {
                                self.preview_spans.insert(
                                    preview_idx,
                                    spans
                                        .into_iter()
                                        .map(|span| HighlightSpan {
                                            start: span.start + prefix_len,
                                            end: span.end + prefix_len,
                                            capture_name: span.capture_name,
                                        })
                                        .collect(),
                                );
                            }
                        }
                    }
                }
            }
            self.last_previewed_jump_index = Some(selected_jump_index);
        } else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.last_previewed_jump_index = None;
            self.jump_target_preview_line = None;
            self.jump_target_char_col = None;
        }
    }

    pub(super) fn update_reference_preview(&mut self) {
        let selected_reference_index = match self.candidates.get(self.selected) {
            Some(c) => match c.kind {
                CandidateKind::Reference(idx) => idx,
                _ => {
                    self.preview_lines.clear();
                    self.preview_spans.clear();
                    self.last_previewed_reference_index = None;
                    self.jump_target_preview_line = None;
                    self.jump_target_char_col = None;
                    return;
                }
            },
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_reference_index = None;
                self.jump_target_preview_line = None;
                self.jump_target_char_col = None;
                return;
            }
        };

        if self.last_previewed_reference_index == Some(selected_reference_index) {
            return;
        }

        if let Some(entry) = self.reference_entries.get(selected_reference_index) {
            self.preview_lines = entry.preview_lines.clone();
            self.jump_target_preview_line = entry.target_preview_line;
            self.jump_target_char_col = Some(entry.target_char_col);
            if let Some(cached_spans) = self
                .reference_highlight_cache
                .get(&selected_reference_index)
            {
                self.preview_spans = cached_spans.clone();
            } else {
                self.preview_spans.clear();
                if let Some(source_path) = entry.source_path.as_deref() {
                    let lang_registry = self
                        .lang_registry_owned
                        .get_or_insert_with(LanguageRegistry::new);
                    if let Some(lang_def) = lang_registry.detect_by_extension(source_path) {
                        let mut code_lines = Vec::new();
                        let mut line_map: Vec<(usize, usize)> = Vec::new();
                        for (preview_idx, line) in self.preview_lines.iter().enumerate().skip(1) {
                            if let Some((_, code)) = split_numbered_preview_line(line) {
                                let prefix_len = line.len().saturating_sub(code.len());
                                code_lines.push(code.to_string());
                                line_map.push((preview_idx, prefix_len));
                            } else {
                                code_lines.push(line.clone());
                                line_map.push((preview_idx, 0));
                            }
                        }
                        if !code_lines.is_empty() {
                            let preview_text = code_lines.join("\n");
                            let raw_spans = highlight_text(&preview_text, lang_def);
                            for (line_idx, spans) in raw_spans {
                                if let Some((preview_idx, prefix_len)) =
                                    line_map.get(line_idx).copied()
                                {
                                    self.preview_spans.insert(
                                        preview_idx,
                                        spans
                                            .into_iter()
                                            .map(|span| HighlightSpan {
                                                start: span.start + prefix_len,
                                                end: span.end + prefix_len,
                                                capture_name: span.capture_name,
                                            })
                                            .collect(),
                                    );
                                }
                            }
                        }
                    }
                }
                self.reference_highlight_cache
                    .insert(selected_reference_index, self.preview_spans.clone());
            }
            self.last_previewed_reference_index = Some(selected_reference_index);
        } else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.last_previewed_reference_index = None;
            self.jump_target_preview_line = None;
            self.jump_target_char_col = None;
        }
    }

    pub(super) fn update_git_branch_preview(&mut self) {
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;
        let selected_branch_index = match self.candidates.get(self.selected) {
            Some(c) => match c.kind {
                CandidateKind::GitBranch(idx) => idx,
                _ => {
                    self.preview_lines.clear();
                    self.preview_spans.clear();
                    self.last_previewed_git_branch_index = None;
                    return;
                }
            },
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_git_branch_index = None;
                return;
            }
        };

        if self.last_previewed_git_branch_index == Some(selected_branch_index) {
            return;
        }

        if let Some(entry) = self.git_branch_entries.get(selected_branch_index) {
            self.preview_lines = entry.preview_lines.clone();
            self.preview_spans.clear();
            self.last_previewed_git_branch_index = Some(selected_branch_index);
        } else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.last_previewed_git_branch_index = None;
        }
    }

    pub(super) fn update_symbol_preview(&mut self) {
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;
        let selected_symbol_index = match self.candidates.get(self.selected) {
            Some(c) => match c.kind {
                CandidateKind::Symbol(idx) => idx,
                _ => {
                    self.preview_lines.clear();
                    self.preview_spans.clear();
                    self.last_previewed_symbol_index = None;
                    return;
                }
            },
            None => {
                self.preview_lines.clear();
                self.preview_spans.clear();
                self.last_previewed_symbol_index = None;
                return;
            }
        };

        if self.last_previewed_symbol_index == Some(selected_symbol_index) {
            return;
        }

        if let Some(entry) = self.symbol_entries.get(selected_symbol_index) {
            self.preview_lines = entry.preview_lines.clone();
            self.preview_spans.clear();
            self.jump_target_preview_line =
                self.preview_lines
                    .iter()
                    .enumerate()
                    .find_map(|(preview_idx, line)| {
                        let (prefix, _) = split_numbered_preview_line(line)?;
                        let line_no = prefix.trim().parse::<usize>().ok()?;
                        (line_no == entry.line + 1).then_some(preview_idx)
                    });
            self.jump_target_char_col = Some(entry.char_col);
            self.last_previewed_symbol_index = Some(selected_symbol_index);
        } else {
            self.preview_lines.clear();
            self.preview_spans.clear();
            self.last_previewed_symbol_index = None;
            self.jump_target_preview_line = None;
            self.jump_target_char_col = None;
        }
    }

    pub(super) fn update_goto_line_preview(&mut self) {
        self.preview_lines.clear();
        self.preview_spans.clear();
        self.jump_target_preview_line = None;
        self.jump_target_char_col = None;

        let line_str = self.input.text[1..].trim();
        let target_line = match line_str.parse::<usize>() {
            Ok(n) if n > 0 => n - 1,
            _ => return,
        };

        let total = self.active_doc_lines.len();
        if total == 0 {
            return;
        }
        let target_line = target_line.min(total.saturating_sub(1));
        let start = target_line.saturating_sub(5);
        let end = (target_line + 6).min(total);

        for line_idx in start..end {
            let text = &self.active_doc_lines[line_idx];
            self.preview_lines
                .push(format!("{:>5} | {}", line_idx + 1, text));
        }

        self.jump_target_preview_line = Some(target_line - start);
        self.jump_target_char_col = Some(0);
    }
}

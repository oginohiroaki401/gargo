use super::*;

impl Palette {
    pub fn update_candidates(
        &mut self,
        registry: &CommandRegistry,
        lang_registry: &LanguageRegistry,
        config: &Config,
    ) {
        if self.input.text.starts_with('>') {
            self.mode = PaletteMode::Command;
            let query = self.input.text[1..].trim_start();
            self.candidates =
                Self::filter_commands(registry, query, self.command_history.as_deref(), config);
        } else if self.input.text.starts_with('@') {
            self.mode = PaletteMode::SymbolPicker;
            let query = self.input.text[1..].to_string();
            self.filter_symbol_candidates_with_query(&query);
        } else if self.input.text.starts_with(':') {
            self.mode = PaletteMode::GotoLine;
            self.candidates.clear();
        } else {
            self.mode = PaletteMode::FileFinder;
            self.candidates = Self::filter_files(
                &self.file_entries,
                &self.input.text,
                &self.project_root,
                &self.git_status_map,
            );
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
        self.clamp_input_cursor();
        self.update_preview(lang_registry, config);
    }

    pub(super) fn filter_commands(
        registry: &CommandRegistry,
        query: &str,
        history: Option<&CommandHistory>,
        config: &Config,
    ) -> Vec<ScoredCandidate> {
        let mut scored: Vec<ScoredCandidate> = registry
            .commands()
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let display_label = command_display_label(entry, config);
                if query.is_empty() {
                    return Some(ScoredCandidate {
                        kind: CandidateKind::Command(i),
                        label: display_label.clone(),
                        score: 0,
                        match_positions: Vec::new(),
                        preview_lines: command_preview_lines(entry, &display_label),
                    });
                }
                fuzzy_match(&display_label, query).map(|(score, positions)| ScoredCandidate {
                    kind: CandidateKind::Command(i),
                    label: display_label.clone(),
                    score,
                    match_positions: positions,
                    preview_lines: command_preview_lines(entry, &display_label),
                })
            })
            .collect();

        // History-based sorting when query is empty
        if query.is_empty() {
            if let Some(hist) = history {
                let recent_ids = hist.get_recent_commands(100);
                let id_to_rank: HashMap<&str, usize> = recent_ids
                    .iter()
                    .enumerate()
                    .map(|(rank, id)| (id.as_str(), rank))
                    .collect();

                scored.sort_by(|a, b| {
                    let a_idx = match a.kind {
                        CandidateKind::Command(i) => i,
                        _ => return Ordering::Equal,
                    };
                    let b_idx = match b.kind {
                        CandidateKind::Command(i) => i,
                        _ => return Ordering::Equal,
                    };

                    let a_id = &registry.commands()[a_idx].id;
                    let b_id = &registry.commands()[b_idx].id;

                    let a_rank = id_to_rank.get(a_id.as_str());
                    let b_rank = id_to_rank.get(b_id.as_str());

                    match (a_rank, b_rank) {
                        (Some(r1), Some(r2)) => r1.cmp(r2), // Both in history: by recency
                        (Some(_), None) => Ordering::Less,  // a in history, b not
                        (None, Some(_)) => Ordering::Greater, // b in history, a not
                        (None, None) => a.label.cmp(&b.label), // Neither: alphabetical
                    }
                });
            } else {
                // No history: alphabetical fallback
                scored.sort_by(|a, b| a.label.cmp(&b.label));
            }
        } else {
            // With query: fuzzy match score (existing behavior)
            scored.sort_by(|a, b| b.score.cmp(&a.score));
        }

        scored
    }

    pub(super) fn filter_files(
        file_entries: &[String],
        query: &str,
        project_root: &Path,
        git_status_map: &HashMap<String, GitFileStatus>,
    ) -> Vec<ScoredCandidate> {
        if query.is_empty() {
            // Sort by last edit (mtime) descending. Uncommitted files (present in
            // git_status_map) are grouped first; both groups are mtime-desc within.
            let mut indexed: Vec<(usize, &String, Option<std::time::SystemTime>, bool)> =
                file_entries
                    .iter()
                    .enumerate()
                    .map(|(i, path)| {
                        let abs = project_root.join(path);
                        let mtime = std::fs::metadata(&abs).and_then(|m| m.modified()).ok();
                        let uncommitted = git_status_map.contains_key(path);
                        (i, path, mtime, uncommitted)
                    })
                    .collect();
            indexed.sort_by(|a, b| {
                b.3.cmp(&a.3)
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| a.1.cmp(b.1))
            });
            return indexed
                .into_iter()
                .map(|(i, path, _, _)| ScoredCandidate {
                    kind: CandidateKind::File(i),
                    label: path.clone(),
                    score: 0,
                    match_positions: Vec::new(),
                    preview_lines: Vec::new(),
                })
                .collect();
        }

        let mut scored: Vec<ScoredCandidate> = file_entries
            .iter()
            .enumerate()
            .filter_map(|(i, path)| {
                fuzzy_match(path, query).map(|(score, positions)| ScoredCandidate {
                    kind: CandidateKind::File(i),
                    label: path.clone(),
                    score,
                    match_positions: positions,
                    preview_lines: Vec::new(),
                })
            })
            .collect();

        scored.sort_by(|a, b| b.score.cmp(&a.score));

        scored
    }

    pub(super) fn filter_buffer_candidates(&mut self) {
        let query = &self.input.text;
        if query.is_empty() {
            self.candidates = self
                .buffer_entries
                .iter()
                .map(|(id, name, _)| ScoredCandidate {
                    kind: CandidateKind::Buffer(*id),
                    label: name.clone(),
                    score: 0,
                    match_positions: Vec::new(),
                    preview_lines: Vec::new(),
                })
                .collect();
        } else {
            let mut scored: Vec<ScoredCandidate> = self
                .buffer_entries
                .iter()
                .filter_map(|(id, name, _)| {
                    fuzzy_match(name, query).map(|(score, positions)| ScoredCandidate {
                        kind: CandidateKind::Buffer(*id),
                        label: name.clone(),
                        score,
                        match_positions: positions,
                        preview_lines: Vec::new(),
                    })
                })
                .collect();
            scored.sort_by(|a, b| b.score.cmp(&a.score));
            self.candidates = scored;
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }

    pub(super) fn filter_jump_candidates(&mut self) {
        let query = &self.input.text;
        if query.is_empty() {
            self.candidates = self
                .jump_entries
                .iter()
                .map(|entry| ScoredCandidate {
                    kind: CandidateKind::Jump(entry.jump_index),
                    label: entry.label.clone(),
                    score: 0,
                    match_positions: Vec::new(),
                    preview_lines: Vec::new(),
                })
                .collect();
        } else {
            let mut scored: Vec<ScoredCandidate> = self
                .jump_entries
                .iter()
                .filter_map(|entry| {
                    fzf_style_match(&entry.label, query).map(|(score, positions)| ScoredCandidate {
                        kind: CandidateKind::Jump(entry.jump_index),
                        label: entry.label.clone(),
                        score,
                        match_positions: positions,
                        preview_lines: Vec::new(),
                    })
                })
                .collect();
            scored.sort_by(|a, b| b.score.cmp(&a.score));
            self.candidates = scored;
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }

    pub(super) fn filter_reference_candidates(&mut self) {
        let query = &self.input.text;
        if query.is_empty() {
            self.candidates = self
                .reference_entries
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
        } else {
            let mut scored: Vec<ScoredCandidate> = self
                .reference_entries
                .iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    fzf_style_match(&entry.label, query).map(|(score, positions)| ScoredCandidate {
                        kind: CandidateKind::Reference(idx),
                        label: entry.label.clone(),
                        score,
                        match_positions: positions,
                        preview_lines: Vec::new(),
                    })
                })
                .collect();
            scored.sort_by(|a, b| b.score.cmp(&a.score));
            self.candidates = scored;
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }

    pub(super) fn filter_git_branch_candidates(&mut self) {
        let query = &self.input.text;
        if query.is_empty() {
            self.candidates = self
                .git_branch_entries
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
        } else {
            let mut scored: Vec<ScoredCandidate> = self
                .git_branch_entries
                .iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    fzf_style_match(&entry.label, query).map(|(score, positions)| ScoredCandidate {
                        kind: CandidateKind::GitBranch(idx),
                        label: entry.label.clone(),
                        score,
                        match_positions: positions,
                        preview_lines: Vec::new(),
                    })
                })
                .collect();
            scored.sort_by(|a, b| b.score.cmp(&a.score));
            self.candidates = scored;
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }

    pub(super) fn filter_symbol_candidates(&mut self) {
        let query = self.input.text.clone();
        self.filter_symbol_candidates_with_query(&query);
    }

    pub(super) fn filter_symbol_candidates_with_query(&mut self, query: &str) {
        if query.is_empty() {
            self.candidates = self
                .symbol_entries
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
        } else {
            let mut scored: Vec<ScoredCandidate> = self
                .symbol_entries
                .iter()
                .enumerate()
                .filter_map(|(idx, entry)| {
                    fzf_style_match(&entry.label, query).map(|(score, positions)| ScoredCandidate {
                        kind: CandidateKind::Symbol(idx),
                        label: entry.label.clone(),
                        score,
                        match_positions: positions,
                        preview_lines: Vec::new(),
                    })
                })
                .collect();
            scored.sort_by(|a, b| b.score.cmp(&a.score));
            self.candidates = scored;
        }

        if self.selected >= self.candidates.len() {
            self.selected = self.candidates.len().saturating_sub(1);
        }
    }
}

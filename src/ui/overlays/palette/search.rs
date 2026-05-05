use super::*;

impl Palette {
    pub(super) fn restart_global_search_worker(&mut self) {
        if self.global_search_request_tx.is_none() {
            return;
        }

        self.global_search_request_tx = None;
        self.global_search_result_rx = None;
        self._global_search_worker = None;

        let (search_req_tx, search_req_rx) = mpsc::channel::<GlobalSearchRequest>();
        let (search_res_tx, search_res_rx) = mpsc::channel::<GlobalSearchBatch>();
        let root = self.project_root.clone();
        let worker_files = self.file_entries.clone();
        let worker_unsaved_buffers = self.global_search_unsaved_buffers.clone();
        let search_handle = thread::spawn(move || {
            workers::global_search_worker(
                search_req_rx,
                search_res_tx,
                root,
                worker_files,
                worker_unsaved_buffers,
            );
        });

        self.global_search_request_tx = Some(search_req_tx);
        self.global_search_result_rx = Some(search_res_rx);
        self._global_search_worker = Some(search_handle);
        self.global_search_entries.clear();
        self.candidates.clear();
        self.selected = 0;
        self.preview_lines.clear();
        self.preview_spans.clear();
        self.global_search_generation = 0;
        self.global_search_latest_applied = 0;
        self.global_search_dirty = true;
        self.global_search_changed_at = Some(Instant::now());
    }

    pub(super) fn mark_global_search_dirty(&mut self) {
        self.global_search_dirty = true;
        self.global_search_changed_at = Some(Instant::now());
    }

    pub(super) fn pump_global_search(&mut self) {
        if self.mode != PaletteMode::GlobalSearch {
            return;
        }

        let mut batches: Vec<GlobalSearchBatch> = Vec::new();
        if let Some(ref rx) = self.global_search_result_rx {
            while let Ok(batch) = rx.try_recv() {
                batches.push(batch);
            }
        } else {
            return;
        }

        for batch in batches {
            if batch.generation < self.global_search_latest_applied {
                continue;
            }

            self.global_search_latest_applied = batch.generation;
            if let Some(error) = batch.error {
                self.global_search_entries.clear();
                self.candidates.clear();
                self.selected = 0;
                self.preview_lines = vec![format!("Global search error: {error}")];
                self.preview_spans.clear();
                self.last_previewed_search_index = None;
                continue;
            }

            if batch.append {
                self.global_search_entries.extend(batch.results);
            } else {
                self.global_search_entries = batch.results;
            }
            self.candidates = self
                .global_search_entries
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let excerpt = entry
                        .preview_lines
                        .get(1)
                        .map(|s| {
                            s.split_once('|')
                                .map(|(_, right)| right)
                                .unwrap_or(s.as_str())
                        })
                        .unwrap_or("");
                    ScoredCandidate {
                        kind: CandidateKind::SearchResult(i),
                        label: format!(
                            "{}:{} {}",
                            entry.display_path,
                            entry.line + 1,
                            excerpt.trim()
                        ),
                        score: 0,
                        match_positions: Vec::new(),
                        preview_lines: entry.preview_lines.clone(),
                    }
                })
                .collect();

            if self.selected >= self.candidates.len() {
                self.selected = self.candidates.len().saturating_sub(1);
            }
            self.last_previewed_search_index = None;
            self.update_global_search_preview();
        }

        if !self.global_search_dirty {
            return;
        }

        let Some(changed_at) = self.global_search_changed_at else {
            return;
        };
        if changed_at.elapsed() < Duration::from_millis(workers::GLOBAL_SEARCH_DEBOUNCE_MS) {
            return;
        }

        let Some(ref tx) = self.global_search_request_tx else {
            return;
        };

        self.global_search_generation = self.global_search_generation.saturating_add(1);
        let request = GlobalSearchRequest {
            query: self.input.text.clone(),
            generation: self.global_search_generation,
        };
        let _ = tx.send(request);
        self.global_search_dirty = false;
    }
}

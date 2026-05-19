use super::*;

impl Editor {
    /// Clear stale search matches and cache when the active buffer changes.
    fn on_buffer_switch(&mut self) {
        self.search.matches.clear();
        self.search.current_match = None;
        self.search.invalidate_cache();
    }

    pub fn active_buffer(&self) -> &Document {
        &self.documents[self.active_index]
    }

    pub fn active_buffer_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active_index]
    }

    pub fn set_git_gutter_for_doc(
        &mut self,
        doc_id: DocumentId,
        gutter: HashMap<usize, GitLineStatus>,
    ) {
        if let Some(doc) = self.documents.iter_mut().find(|doc| doc.id == doc_id) {
            doc.git_gutter = gutter;
        }
    }

    pub fn buffers(&self) -> &[Document] {
        &self.documents
    }

    pub fn buffer_by_id(&self, id: BufferId) -> Option<&Document> {
        self.documents.iter().find(|doc| doc.id == id)
    }

    pub fn buffer_count(&self) -> usize {
        self.documents.len()
    }

    #[cfg(test)]
    pub fn active_buffer_id(&self) -> BufferId {
        self.documents[self.active_index].id
    }

    pub fn active_index(&self) -> usize {
        self.active_index
    }

    pub fn new_buffer(&mut self) -> DocumentId {
        let id = self.next_id;
        self.next_id += 1;
        let doc = Document::new_scratch(id);
        self.documents.push(doc);
        self.active_index = self.documents.len() - 1;
        self.on_buffer_switch();
        self.set_history_index_to_active();
        id
    }

    pub fn open_file(&mut self, path: &str) {
        // Check if already open
        for (i, doc) in self.documents.iter().enumerate() {
            if let Some(ref fp) = doc.file_path
                && fp.to_str() == Some(path)
            {
                self.active_index = i;
                self.on_buffer_switch();
                self.push_active_to_mru();
                return;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut doc = Document::from_file(id, path);
        doc.git_gutter = git_diff_line_status(path);

        if let Some(lang_def) = self.language_registry.detect_by_extension(path) {
            self.highlight_manager
                .register_buffer(doc.id, &doc.rope, lang_def);
            self.language_names.insert(doc.id, lang_def.name);
        }

        self.documents.push(doc);
        self.active_index = self.documents.len() - 1;
        self.on_buffer_switch();
        self.push_active_to_mru();
    }

    pub fn next_buffer(&mut self) {
        if self.documents.len() > 1 {
            self.active_index = (self.active_index + 1) % self.documents.len();
            self.on_buffer_switch();
        }
    }

    pub fn prev_buffer(&mut self) {
        if self.documents.len() > 1 {
            if self.active_index == 0 {
                self.active_index = self.documents.len() - 1;
            } else {
                self.active_index -= 1;
            }
            self.on_buffer_switch();
        }
    }

    pub fn switch_to_buffer(&mut self, id: BufferId) -> bool {
        if let Some(idx) = self.documents.iter().position(|d| d.id == id) {
            self.active_index = idx;
            self.on_buffer_switch();
            self.push_active_to_mru();
            true
        } else {
            false
        }
    }

    pub fn close_active_buffer(&mut self) -> Result<(), String> {
        if self.documents[self.active_index].dirty {
            return Err("Buffer has unsaved changes".to_string());
        }
        self.force_close_active_buffer();
        Ok(())
    }

    /// Close the active buffer without checking for unsaved changes.
    pub fn force_close_active_buffer(&mut self) {
        let old_id = self.documents[self.active_index].id;
        self.highlight_manager.unregister_buffer(old_id);
        self.language_names.remove(&old_id);
        self.buffer_history.retain(|id| *id != old_id);

        if self.documents.len() == 1 {
            // Replace with a new scratch document
            let id = self.next_id;
            self.next_id += 1;
            self.documents[0] = Document::new_scratch(id);
            self.active_index = 0;
        } else {
            self.documents.remove(self.active_index);
            if self.active_index >= self.documents.len() {
                self.active_index = self.documents.len() - 1;
            }
        }
        self.on_buffer_switch();
        self.set_history_index_to_active();
    }

    fn is_history_eligible(doc: &Document) -> bool {
        doc.file_path.is_some()
    }

    pub(crate) fn push_active_to_mru(&mut self) {
        let active = &self.documents[self.active_index];
        if !Self::is_history_eligible(active) {
            self.set_history_index_to_active();
            return;
        }
        let active_id = active.id;
        if let Some(pos) = self.buffer_history.iter().position(|id| *id == active_id) {
            self.buffer_history.remove(pos);
        }
        self.buffer_history.push(active_id);
        self.buffer_history_index = Some(self.buffer_history.len() - 1);
    }

    pub(crate) fn set_history_index_to_active(&mut self) {
        let active_id = self.documents[self.active_index].id;
        self.buffer_history_index = self.buffer_history.iter().position(|id| *id == active_id);
    }

    /// Move to the previous (older) entry in buffer MRU history.
    /// Returns true when the active buffer changed.
    pub fn prev_buffer_history(&mut self) -> bool {
        if self.buffer_history.is_empty() {
            return false;
        }

        let active_id = self.documents[self.active_index].id;
        let active_in_history = self.buffer_history.iter().position(|id| *id == active_id);
        let target_idx = match active_in_history {
            None => self.buffer_history.len() - 1,
            Some(0) => {
                self.buffer_history_index = Some(0);
                return false;
            }
            Some(idx) => idx - 1,
        };

        let target_id = self.buffer_history[target_idx];
        if let Some(doc_idx) = self.documents.iter().position(|d| d.id == target_id) {
            self.active_index = doc_idx;
            self.on_buffer_switch();
            self.buffer_history_index = Some(target_idx);
            true
        } else {
            false
        }
    }

    /// Move to the next (newer) entry in buffer MRU history.
    /// Returns true when the active buffer changed.
    pub fn next_buffer_history(&mut self) -> bool {
        if self.buffer_history.is_empty() {
            return false;
        }

        let active_id = self.documents[self.active_index].id;
        let Some(current_idx) = self.buffer_history.iter().position(|id| *id == active_id) else {
            return false;
        };
        if current_idx + 1 >= self.buffer_history.len() {
            self.buffer_history_index = Some(current_idx);
            return false;
        }

        let target_idx = current_idx + 1;
        let target_id = self.buffer_history[target_idx];
        if let Some(doc_idx) = self.documents.iter().position(|d| d.id == target_id) {
            self.active_index = doc_idx;
            self.on_buffer_switch();
            self.buffer_history_index = Some(target_idx);
            true
        } else {
            false
        }
    }
}

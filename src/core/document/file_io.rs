use super::*;

impl Document {
    pub fn from_file(id: DocumentId, path: &str) -> Self {
        let file_path = PathBuf::from(path);
        let rope = match fs::read_to_string(path) {
            Ok(contents) => Rope::from_str(&contents),
            Err(_) => match fs::read(path) {
                Ok(bytes) => Rope::from_str(&String::from_utf8_lossy(&bytes)),
                Err(_) => Rope::new(),
            },
        };
        let cached_status_bar_path = Self::compute_status_bar_path(&Some(file_path.clone()));
        Self {
            id,
            rope,
            cursors: vec![0],
            scroll_offset: 0,
            horizontal_scroll_offset: 0,
            file_path: Some(file_path),
            dirty: false,
            pending_edits: Vec::new(),
            history: History::new(),
            selections: vec![None],
            git_gutter: HashMap::new(),
            cached_status_bar_path,
            version: 0,
        }
    }

    pub fn save(&mut self) -> Result<String, String> {
        let path = match &self.file_path {
            Some(p) => p.clone(),
            None => return Err("No file path".to_string()),
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let contents = self.rope.to_string();
        fs::write(&path, &contents).map_err(|e| e.to_string())?;
        self.dirty = false;
        Ok(format!("Wrote {}", path.display()))
    }

    pub fn save_as(&mut self, path: &Path) -> Result<String, String> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let contents = self.rope.to_string();
        fs::write(path, &contents).map_err(|e| e.to_string())?;

        let normalized = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.file_path = Some(normalized.clone());
        self.cached_status_bar_path = Self::compute_status_bar_path(&self.file_path);
        self.dirty = false;
        Ok(format!("Wrote {}", normalized.display()))
    }

    pub fn rename_file(&mut self, new_path: &Path) -> Result<String, String> {
        let old_path = self
            .file_path
            .clone()
            .ok_or_else(|| "No file path to rename".to_string())?;

        if let Some(parent) = new_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        fs::rename(&old_path, new_path).map_err(|e| e.to_string())?;

        let normalized = fs::canonicalize(new_path).unwrap_or_else(|_| new_path.to_path_buf());
        self.file_path = Some(normalized.clone());
        self.cached_status_bar_path = Self::compute_status_bar_path(&self.file_path);
        Ok(format!("Renamed to {}", normalized.display()))
    }

    pub fn reload_from_disk(&mut self) -> Result<String, String> {
        let path = match &self.file_path {
            Some(p) => p.clone(),
            None => return Err("No file path to reload".to_string()),
        };
        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                let bytes = fs::read(&path).map_err(|e| e.to_string())?;
                String::from_utf8_lossy(&bytes).into_owned()
            }
        };
        let old_cursor = self.cursors[0];
        self.rope = Rope::from_str(&contents);
        self.bump_version();
        // Preserve cursor position if still valid, reset to single cursor
        self.cursors = vec![old_cursor.min(self.rope.len_chars())];
        self.dirty = false;
        self.pending_edits.clear();
        self.history = History::new();
        self.selections = vec![None];
        Ok(format!("Reloaded {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use tempfile::tempdir;

    #[test]
    fn save_creates_missing_parent_directories() {
        let tmp = tempdir().expect("temp dir");
        let target = tmp.path().join("a/b/c/new.md");
        assert!(!target.exists());
        let mut doc = Document::from_file(1, target.to_str().unwrap());
        doc.rope = ropey::Rope::from_str("hello\n");
        doc.dirty = true;
        let msg = doc.save().expect("save");
        assert!(target.exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello\n");
        assert!(!doc.dirty);
        assert!(msg.contains("Wrote"));
    }
}

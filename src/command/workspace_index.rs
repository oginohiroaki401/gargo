use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::command::gargo_server::FileEntry;

const PUBLISH_BATCH_SIZE: usize = 512;
const MAX_WORKSPACE_FILES: usize = 200_000;

#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndexSnapshot {
    pub entries: Vec<FileEntry>,
    pub ready: bool,
    pub search_ready: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceIndexPage {
    pub entries: Vec<FileEntry>,
    pub ready: bool,
    pub search_ready: bool,
    pub truncated: bool,
    pub total: usize,
    pub next_offset: Option<usize>,
}

#[derive(Debug)]
pub struct WorkspaceIndex {
    root: PathBuf,
    is_git: bool,
    snapshot: Mutex<WorkspaceIndexSnapshot>,
    refreshing: AtomicBool,
    rerun_requested: AtomicBool,
    /// Bumped once per completed scan. Lets callers (e.g. the Cmd+P picker's
    /// on-open refresh) request a rescan and then wait until a *fresh* snapshot
    /// — one that sees just-created files — has actually been published.
    generation: AtomicU64,
}

impl WorkspaceIndex {
    pub fn new(root: PathBuf, is_git: bool) -> Arc<Self> {
        let index = Arc::new(Self {
            root,
            is_git,
            snapshot: Mutex::new(WorkspaceIndexSnapshot::default()),
            refreshing: AtomicBool::new(false),
            rerun_requested: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        index.request_refresh();
        index
    }

    pub fn is_git(&self) -> bool {
        self.is_git
    }

    pub fn snapshot(&self) -> WorkspaceIndexSnapshot {
        self.snapshot.lock().unwrap().clone()
    }

    pub fn is_ready(&self) -> bool {
        self.snapshot.lock().unwrap().ready
    }

    /// Number of scans that have completed. Monotonically increasing.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn is_search_ready(&self) -> bool {
        self.snapshot.lock().unwrap().search_ready
    }

    pub fn page(&self, offset: usize, limit: Option<usize>) -> WorkspaceIndexPage {
        let snapshot = self.snapshot.lock().unwrap();
        let total = snapshot.entries.len();
        let offset = offset.min(total);
        let limit = limit.map(|limit| limit.clamp(1, 10_000)).unwrap_or(total);
        let end = offset.saturating_add(limit).min(total);
        WorkspaceIndexPage {
            entries: snapshot.entries[offset..end].to_vec(),
            ready: snapshot.ready,
            search_ready: snapshot.search_ready,
            truncated: snapshot.truncated,
            total,
            next_offset: (end < total || !snapshot.ready).then_some(end),
        }
    }

    pub fn request_refresh(self: &Arc<Self>) {
        self.rerun_requested.store(true, Ordering::Release);
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let index = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("gargo-workspace-index".to_string())
            .spawn(move || index.refresh_loop());
    }

    fn refresh_loop(self: Arc<Self>) {
        loop {
            self.rerun_requested.store(false, Ordering::Release);
            self.refresh_once();
            if !self.rerun_requested.swap(false, Ordering::AcqRel) {
                self.refreshing.store(false, Ordering::Release);
                if !self.rerun_requested.load(Ordering::Acquire) {
                    break;
                }
                if self
                    .refreshing
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    fn refresh_once(&self) {
        {
            let mut snapshot = self.snapshot.lock().unwrap();
            *snapshot = WorkspaceIndexSnapshot::default();
        }

        let opens = crate::command::recent_projects::RecentProjectsStore::new()
            .get_file_open_times(&self.root);
        let changed = if self.is_git {
            crate::command::git_backend::status_map(&self.root)
        } else {
            HashMap::new()
        };

        let (files, truncated) = if self.is_git {
            let files = crate::project::collect_files(&self.root);
            self.publish_paths(&files, &changed, &opens);
            (files, false)
        } else {
            self.walk_workspace(&changed, &opens)
        };

        {
            let mut snapshot = self.snapshot.lock().unwrap();
            snapshot.ready = true;
            snapshot.truncated = truncated;
        }
        // Publish the new generation as soon as the file list is ready — the
        // Cmd+P picker only needs entries, not the (slower) search index — so an
        // on-open refresh unblocks the moment fresh files are visible.
        self.generation.fetch_add(1, Ordering::Release);

        let search_ready =
            crate::command::global_search_index::refresh_repo_with_files(&self.root, files).is_ok();
        self.snapshot.lock().unwrap().search_ready = search_ready;
    }

    fn walk_workspace(
        &self,
        changed: &HashMap<String, crate::command::git::GitFileStatus>,
        opens: &HashMap<String, i64>,
    ) -> (Vec<String>, bool) {
        let mut dirs = vec![self.root.clone()];
        let mut files = Vec::new();
        let mut pending = Vec::with_capacity(PUBLISH_BATCH_SIZE);
        let mut truncated = false;

        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if file_type.is_symlink() && path.is_dir() {
                    continue;
                }
                if !file_type.is_file() && !file_type.is_symlink() {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(&self.root) else {
                    continue;
                };
                let rel = rel.to_string_lossy().replace('\\', "/");
                files.push(rel.clone());
                pending.push(self.file_entry(rel, changed, opens));
                if pending.len() >= PUBLISH_BATCH_SIZE {
                    self.publish_batch(&mut pending);
                }
                if files.len() >= MAX_WORKSPACE_FILES {
                    truncated = true;
                    break;
                }
            }
            if truncated {
                break;
            }
        }
        self.publish_batch(&mut pending);
        (files, truncated)
    }

    fn publish_paths(
        &self,
        files: &[String],
        changed: &HashMap<String, crate::command::git::GitFileStatus>,
        opens: &HashMap<String, i64>,
    ) {
        let mut pending = Vec::with_capacity(PUBLISH_BATCH_SIZE);
        for path in files {
            pending.push(self.file_entry(path.clone(), changed, opens));
            if pending.len() >= PUBLISH_BATCH_SIZE {
                self.publish_batch(&mut pending);
            }
        }
        self.publish_batch(&mut pending);
    }

    fn publish_batch(&self, pending: &mut Vec<FileEntry>) {
        if pending.is_empty() {
            return;
        }
        self.snapshot.lock().unwrap().entries.append(pending);
    }

    fn file_entry(
        &self,
        path: String,
        changed: &HashMap<String, crate::command::git::GitFileStatus>,
        opens: &HashMap<String, i64>,
    ) -> FileEntry {
        let full = self.root.join(&path);
        let mtime = mtime_ms(&full);
        let opened = opens.get(&path).copied().unwrap_or(0).max(0) as u64;
        (path.clone(), mtime, changed.contains_key(&path), opened)
    }
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    #[cfg(unix)]
    #[test]
    fn non_git_index_does_not_descend_into_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(dir.path().join("local.txt"), "local\n").unwrap();
        std::fs::write(outside.path().join("outside.txt"), "outside\n").unwrap();
        symlink(outside.path(), dir.path().join("linked")).unwrap();

        let index = WorkspaceIndex::new(dir.path().to_path_buf(), false);
        let deadline = Instant::now() + Duration::from_secs(3);
        let snapshot = loop {
            let snapshot = index.snapshot();
            if snapshot.ready && snapshot.search_ready {
                break snapshot;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        let paths: Vec<_> = snapshot.entries.into_iter().map(|entry| entry.0).collect();
        assert!(paths.contains(&"local.txt".to_string()));
        assert!(!paths.iter().any(|path| path.starts_with("linked/")));
    }
}

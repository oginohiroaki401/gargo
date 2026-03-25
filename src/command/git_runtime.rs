use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::command::git::{GitFileStatus, GitLineStatus};
use crate::command::git_backend;
use crate::core::document::DocumentId;

const CHANNEL_POLL_FALLBACK_MS: u64 = 20;

#[derive(Debug)]
pub enum GitRuntimeCommand {
    RefreshStatus {
        project_root: PathBuf,
        high_priority: bool,
    },
    UpdateDocument {
        doc_id: DocumentId,
        path: PathBuf,
        content: String,
        high_priority: bool,
    },
    ClearDocument {
        doc_id: DocumentId,
    },
    RefreshMultiStatus {
        repos: Vec<PathBuf>,
        high_priority: bool,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum GitRuntimeEvent {
    FileStatusMapUpdated(HashMap<String, GitFileStatus>),
    MultiFileStatusMapUpdated(Vec<(PathBuf, HashMap<String, GitFileStatus>)>),
    DocumentGutterUpdated {
        doc_id: DocumentId,
        gutter: HashMap<usize, GitLineStatus>,
    },
}

pub struct GitRuntimeHandle {
    pub command_tx: mpsc::Sender<GitRuntimeCommand>,
    pub event_rx: mpsc::Receiver<GitRuntimeEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy)]
pub struct GitRuntimeDebounceConfig {
    pub gutter_high_priority_ms: u64,
    pub gutter_normal_ms: u64,
}

impl GitRuntimeHandle {
    pub fn new(debounce: GitRuntimeDebounceConfig) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = GitRuntimeWorker::new(command_rx, event_tx, debounce);
        let worker_thread = thread::Builder::new()
            .name("gargo-git-runtime".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("failed to spawn git runtime worker: {}", e))?;

        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }
}

impl Drop for GitRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.command_tx.send(GitRuntimeCommand::Shutdown);
    }
}

struct PendingStatusUpdate {
    project_root: PathBuf,
    due: Instant,
}

struct PendingMultiStatusUpdate {
    repos: Vec<PathBuf>,
    due: Instant,
}

struct PendingDocUpdate {
    doc_id: DocumentId,
    path: PathBuf,
    content: String,
    due: Instant,
}

struct GitRuntimeWorker {
    command_rx: mpsc::Receiver<GitRuntimeCommand>,
    event_tx: mpsc::Sender<GitRuntimeEvent>,
    pending_status: Option<PendingStatusUpdate>,
    pending_multi_status: Option<PendingMultiStatusUpdate>,
    pending_docs: HashMap<DocumentId, PendingDocUpdate>,
    debounce: GitRuntimeDebounceConfig,
}

impl GitRuntimeWorker {
    fn new(
        command_rx: mpsc::Receiver<GitRuntimeCommand>,
        event_tx: mpsc::Sender<GitRuntimeEvent>,
        debounce: GitRuntimeDebounceConfig,
    ) -> Self {
        Self {
            command_rx,
            event_tx,
            pending_status: None,
            pending_multi_status: None,
            pending_docs: HashMap::new(),
            debounce,
        }
    }

    fn run(mut self) {
        loop {
            let timeout = self.next_timeout();
            let received = self.command_rx.recv_timeout(timeout);
            match received {
                Ok(GitRuntimeCommand::Shutdown) => break,
                Ok(command) => self.handle_command(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            self.process_due_updates();
        }
    }

    fn handle_command(&mut self, command: GitRuntimeCommand) {
        match command {
            GitRuntimeCommand::RefreshStatus {
                project_root,
                high_priority,
            } => {
                let due = Instant::now() + self.debounce_duration(high_priority);
                match &mut self.pending_status {
                    Some(pending) => {
                        pending.project_root = project_root;
                        pending.due = pending.due.min(due);
                    }
                    None => {
                        self.pending_status = Some(PendingStatusUpdate { project_root, due });
                    }
                }
            }
            GitRuntimeCommand::UpdateDocument {
                doc_id,
                path,
                content,
                high_priority,
            } => {
                let due = Instant::now() + self.debounce_duration(high_priority);
                match self.pending_docs.get_mut(&doc_id) {
                    Some(pending) => {
                        pending.path = path;
                        pending.content = content;
                        pending.due = pending.due.min(due);
                    }
                    None => {
                        self.pending_docs.insert(
                            doc_id,
                            PendingDocUpdate {
                                doc_id,
                                path,
                                content,
                                due,
                            },
                        );
                    }
                }
            }
            GitRuntimeCommand::ClearDocument { doc_id } => {
                self.pending_docs.remove(&doc_id);
                let _ = self.event_tx.send(GitRuntimeEvent::DocumentGutterUpdated {
                    doc_id,
                    gutter: HashMap::new(),
                });
            }
            GitRuntimeCommand::RefreshMultiStatus {
                repos,
                high_priority,
            } => {
                let due = Instant::now() + self.debounce_duration(high_priority);
                match &mut self.pending_multi_status {
                    Some(pending) => {
                        pending.repos = repos;
                        pending.due = pending.due.min(due);
                    }
                    None => {
                        self.pending_multi_status = Some(PendingMultiStatusUpdate { repos, due });
                    }
                }
            }
            GitRuntimeCommand::Shutdown => {}
        }
    }

    fn process_due_updates(&mut self) {
        let now = Instant::now();

        if self
            .pending_status
            .as_ref()
            .is_some_and(|pending| pending.due <= now)
        {
            let pending = self.pending_status.take().expect("checked above");
            let map = git_backend::status_map(&pending.project_root);
            let _ = self
                .event_tx
                .send(GitRuntimeEvent::FileStatusMapUpdated(map));
        }

        if self
            .pending_multi_status
            .as_ref()
            .is_some_and(|pending| pending.due <= now)
        {
            let pending = self.pending_multi_status.take().expect("checked above");
            let results: Vec<(PathBuf, HashMap<String, GitFileStatus>)> = pending
                .repos
                .into_iter()
                .map(|repo| {
                    let map = git_backend::status_map(&repo);
                    (repo, map)
                })
                .collect();
            let _ = self
                .event_tx
                .send(GitRuntimeEvent::MultiFileStatusMapUpdated(results));
        }

        let ready_docs: Vec<DocumentId> = self
            .pending_docs
            .iter()
            .filter_map(|(doc_id, pending)| (pending.due <= now).then_some(*doc_id))
            .collect();

        for doc_id in ready_docs {
            let Some(pending) = self.pending_docs.remove(&doc_id) else {
                continue;
            };

            let gutter = git_backend::diff_line_status_for_content(&pending.path, &pending.content);
            let _ = self.event_tx.send(GitRuntimeEvent::DocumentGutterUpdated {
                doc_id: pending.doc_id,
                gutter,
            });
        }
    }

    fn next_timeout(&self) -> Duration {
        let now = Instant::now();
        let mut next_due: Option<Instant> = self.pending_status.as_ref().map(|s| s.due);

        if let Some(multi) = &self.pending_multi_status {
            next_due = Some(match next_due {
                Some(existing) => existing.min(multi.due),
                None => multi.due,
            });
        }

        for pending in self.pending_docs.values() {
            next_due = Some(match next_due {
                Some(existing) => existing.min(pending.due),
                None => pending.due,
            });
        }

        match next_due {
            Some(due) if due > now => due.duration_since(now),
            Some(_) => Duration::from_millis(0),
            None => Duration::from_millis(CHANNEL_POLL_FALLBACK_MS),
        }
    }

    fn debounce_duration(&self, high_priority: bool) -> Duration {
        if high_priority {
            Duration::from_millis(self.debounce.gutter_high_priority_ms)
        } else {
            Duration::from_millis(self.debounce.gutter_normal_ms)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_uses_configured_debounce_values() {
        let (_command_tx, command_rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let worker = GitRuntimeWorker::new(
            command_rx,
            event_tx,
            GitRuntimeDebounceConfig {
                gutter_high_priority_ms: 7,
                gutter_normal_ms: 150,
            },
        );

        assert_eq!(worker.debounce_duration(true), Duration::from_millis(7));
        assert_eq!(worker.debounce_duration(false), Duration::from_millis(150));
    }
}

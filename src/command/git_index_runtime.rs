use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::command::git::{self, GitFileEntry};

const CHANNEL_POLL_FALLBACK_MS: u64 = 20;

#[derive(Debug, Clone, Default)]
pub struct GitIndexBranchEntry {
    pub name: String,
    pub is_current: bool,
    pub preview_lines: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GitIndexSnapshot {
    pub branch: String,
    pub changed: Vec<GitFileEntry>,
    pub staged: Vec<GitFileEntry>,
    pub branches: Vec<GitIndexBranchEntry>,
    pub branches_ready: bool,
}

#[derive(Debug)]
pub enum GitIndexRuntimeCommand {
    Refresh { project_root: PathBuf },
    RefreshMulti { repos: Vec<PathBuf> },
    Shutdown,
}

#[derive(Debug)]
pub enum GitIndexRuntimeEvent {
    Ready {
        project_root: PathBuf,
        snapshot: GitIndexSnapshot,
    },
    MultiReady {
        snapshots: Vec<(PathBuf, GitIndexSnapshot)>,
    },
}

pub struct GitIndexRuntimeHandle {
    pub command_tx: mpsc::Sender<GitIndexRuntimeCommand>,
    pub event_rx: mpsc::Receiver<GitIndexRuntimeEvent>,
    _worker_thread: Option<thread::JoinHandle<()>>,
}

impl GitIndexRuntimeHandle {
    pub fn new() -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        let worker = GitIndexRuntimeWorker::new(command_rx, event_tx);
        let worker_thread = thread::Builder::new()
            .name("gargo-git-index-runtime".to_string())
            .spawn(move || worker.run())
            .map_err(|e| format!("failed to spawn git index runtime worker: {}", e))?;

        Ok(Self {
            command_tx,
            event_rx,
            _worker_thread: Some(worker_thread),
        })
    }
}

impl Drop for GitIndexRuntimeHandle {
    fn drop(&mut self) {
        let _ = self.command_tx.send(GitIndexRuntimeCommand::Shutdown);
    }
}

struct GitIndexRuntimeWorker {
    command_rx: mpsc::Receiver<GitIndexRuntimeCommand>,
    event_tx: mpsc::Sender<GitIndexRuntimeEvent>,
    pending_project_root: Option<PathBuf>,
    pending_multi_repos: Option<Vec<PathBuf>>,
}

impl GitIndexRuntimeWorker {
    fn new(
        command_rx: mpsc::Receiver<GitIndexRuntimeCommand>,
        event_tx: mpsc::Sender<GitIndexRuntimeEvent>,
    ) -> Self {
        Self {
            command_rx,
            event_tx,
            pending_project_root: None,
            pending_multi_repos: None,
        }
    }

    fn run(mut self) {
        loop {
            match self
                .command_rx
                .recv_timeout(Duration::from_millis(CHANNEL_POLL_FALLBACK_MS))
            {
                Ok(GitIndexRuntimeCommand::Refresh { project_root }) => {
                    self.pending_project_root = Some(project_root);
                    self.pending_multi_repos = None;
                }
                Ok(GitIndexRuntimeCommand::RefreshMulti { repos }) => {
                    self.pending_multi_repos = Some(repos);
                    self.pending_project_root = None;
                }
                Ok(GitIndexRuntimeCommand::Shutdown) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }

            if let Some(project_root) = self.pending_project_root.take() {
                // Send basic snapshot (branch + status) immediately so the
                // git view can render without waiting for branch previews.
                let branch =
                    git::git_branch_in(&project_root).unwrap_or_else(|_| "???".to_string());
                let (changed, staged) = git::git_status_files_in(&project_root).unwrap_or_default();
                let _ = self.event_tx.send(GitIndexRuntimeEvent::Ready {
                    project_root: project_root.clone(),
                    snapshot: GitIndexSnapshot {
                        branch: branch.clone(),
                        changed: changed.clone(),
                        staged: staged.clone(),
                        branches: Vec::new(),
                        branches_ready: false,
                    },
                });

                // Check for a newer request before starting the slow branch
                // preview work; if one arrived, skip straight to it.
                if self.drain_latest_refresh().is_some() {
                    continue;
                }

                // Now collect the (potentially slow) branch previews and send
                // a complete snapshot.
                let branches = collect_branch_entries(&project_root);
                let _ = self.event_tx.send(GitIndexRuntimeEvent::Ready {
                    project_root,
                    snapshot: GitIndexSnapshot {
                        branch,
                        changed,
                        staged,
                        branches,
                        branches_ready: true,
                    },
                });
            }

            if let Some(repos) = self.pending_multi_repos.take() {
                // Collect basic snapshots for all repos (skip branch previews
                // to keep multi-repo responsive).
                let snapshots: Vec<(PathBuf, GitIndexSnapshot)> = repos
                    .into_iter()
                    .map(|repo| {
                        let branch =
                            git::git_branch_in(&repo).unwrap_or_else(|_| "???".to_string());
                        let (changed, staged) = git::git_status_files_in(&repo).unwrap_or_default();
                        let snapshot = GitIndexSnapshot {
                            branch,
                            changed,
                            staged,
                            branches: Vec::new(),
                            branches_ready: true,
                        };
                        (repo, snapshot)
                    })
                    .collect();
                let _ = self
                    .event_tx
                    .send(GitIndexRuntimeEvent::MultiReady { snapshots });
            }
        }
    }

    /// Drain all pending commands, returning the latest Refresh project_root
    /// (if any). Returns None if no Refresh was pending.
    fn drain_latest_refresh(&mut self) -> Option<PathBuf> {
        let mut latest: Option<PathBuf> = None;
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                GitIndexRuntimeCommand::Refresh { project_root } => {
                    latest = Some(project_root);
                }
                GitIndexRuntimeCommand::RefreshMulti { repos } => {
                    self.pending_multi_repos = Some(repos);
                    latest = None;
                }
                GitIndexRuntimeCommand::Shutdown => {
                    // Put it back isn't possible, so set pending and let the
                    // outer loop handle it on next iteration.
                    self.pending_project_root = latest;
                    return None;
                }
            }
        }
        if let Some(root) = &latest {
            self.pending_project_root = Some(root.clone());
        }
        latest
    }
}

fn collect_branch_entries(project_root: &std::path::Path) -> Vec<GitIndexBranchEntry> {
    git::git_local_branches_in(project_root)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, is_current)| {
            let preview_lines =
                git::git_branch_preview_in(project_root, &name).unwrap_or_else(|e| {
                    vec![
                        format!("Branch: {}", name),
                        String::new(),
                        format!("Preview unavailable: {}", e),
                    ]
                });
            GitIndexBranchEntry {
                name,
                is_current,
                preview_lines,
            }
        })
        .collect()
}

pub fn collect_git_index_snapshot(project_root: &std::path::Path) -> GitIndexSnapshot {
    let branch = git::git_branch_in(project_root).unwrap_or_else(|_| "???".to_string());
    let (changed, staged) = git::git_status_files_in(project_root).unwrap_or_default();
    let branches = collect_branch_entries(project_root);

    GitIndexSnapshot {
        branch,
        changed,
        staged,
        branches,
        branches_ready: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn refresh_returns_snapshot() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        run_git(tmp.path(), &["init"]);
        run_git(tmp.path(), &["config", "user.name", "gargo-test"]);
        run_git(
            tmp.path(),
            &["config", "user.email", "gargo-test@example.com"],
        );

        fs::write(tmp.path().join("main.txt"), "main\n").expect("write main");
        run_git(tmp.path(), &["add", "main.txt"]);
        run_git(tmp.path(), &["commit", "-m", "init"]);
        run_git(tmp.path(), &["branch", "feature/ui"]);
        fs::write(tmp.path().join("main.txt"), "main\nchanged\n").expect("modify main");

        let runtime = GitIndexRuntimeHandle::new().expect("start runtime");
        runtime
            .command_tx
            .send(GitIndexRuntimeCommand::Refresh {
                project_root: tmp.path().to_path_buf(),
            })
            .expect("send refresh");

        // First event: quick snapshot with branch + status (no branch previews).
        let event = runtime
            .event_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("receive quick event");
        match event {
            GitIndexRuntimeEvent::Ready {
                project_root,
                snapshot,
            } => {
                assert_eq!(project_root, tmp.path());
                assert!(snapshot.branch == "master" || snapshot.branch == "main");
                assert!(
                    snapshot
                        .changed
                        .iter()
                        .any(|entry| entry.path == "main.txt")
                );
                assert!(snapshot.branches.is_empty());
                assert!(!snapshot.branches_ready);
            }
            GitIndexRuntimeEvent::MultiReady { .. } => {
                panic!("unexpected MultiReady event");
            }
        }

        // Second event: full snapshot including branch previews.
        let event = runtime
            .event_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("receive full event");
        match event {
            GitIndexRuntimeEvent::Ready {
                project_root,
                snapshot,
            } => {
                assert_eq!(project_root, tmp.path());
                assert!(
                    snapshot
                        .branches
                        .iter()
                        .any(|entry| entry.name == "feature/ui")
                );
                assert!(snapshot.branches_ready);
            }
            GitIndexRuntimeEvent::MultiReady { .. } => {
                panic!("unexpected MultiReady event");
            }
        }
    }
}

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gix::bstr::ByteSlice;
use gix::diff::Rewrites;
use gix::dir::entry::Status;
use gix::filter::plumbing::driver::apply::Delay;
use gix::objs::tree::EntryKind;
use gix::sec::trust::DefaultForLevel;
use gix::status::{
    UntrackedFiles,
    index_worktree::Item,
    plumbing::index_as_worktree::{Change, EntryStatus},
};
use gix::{Commit, ObjectId, Repository, ThreadSafeRepository};
use imara_diff::{Algorithm, InternedInput, Interner};

use crate::command::git::{GitFileEntry, GitFileStatus, GitLineStatus};

const ALGORITHM: Algorithm = Algorithm::Histogram;
const MAX_DIFF_LINES: usize = 64 * u16::MAX as usize;
const MAX_DIFF_BYTES: usize = MAX_DIFF_LINES * 128;

pub fn status_map(project_root: &Path) -> HashMap<String, GitFileStatus> {
    let mut map = HashMap::new();

    let repo = match shared_repo(project_root) {
        Some(repo) => repo.to_thread_local(),
        None => return map,
    };

    let work_dir = match repo.workdir() {
        Some(dir) => dir.to_path_buf(),
        None => return map,
    };

    let status_platform = match repo.status(gix::progress::Discard) {
        Ok(status) => status
            .untracked_files(UntrackedFiles::Files)
            .index_worktree_rewrites(Some(Rewrites {
                copies: None,
                percentage: Some(0.5),
                limit: 1000,
                ..Default::default()
            })),
        Err(_) => return map,
    };

    let status_iter = match status_platform.into_index_worktree_iter(Vec::new()) {
        Ok(iter) => iter,
        Err(_) => return map,
    };

    for item in status_iter.flatten() {
        match item {
            Item::Modification {
                rela_path, status, ..
            } => {
                let Ok(path) = rela_path.to_path() else {
                    continue;
                };
                let rel = path.to_string_lossy().to_string();
                upsert_status(&mut map, rel, map_entry_status(status));
            }
            Item::DirectoryContents { entry, .. } if entry.status == Status::Untracked => {
                let Ok(path) = entry.rela_path.to_path() else {
                    continue;
                };
                let rel = path.to_string_lossy().to_string();
                upsert_status(&mut map, rel, GitFileStatus::Untracked);
            }
            Item::Rewrite {
                source,
                dirwalk_entry,
                ..
            } => {
                let Ok(from_path) = source.rela_path().to_path() else {
                    continue;
                };
                let Ok(to_path) = dirwalk_entry.rela_path.to_path() else {
                    continue;
                };

                let from_rel = from_path.to_string_lossy().to_string();
                let to_rel = to_path.to_string_lossy().to_string();

                upsert_status(&mut map, from_rel, GitFileStatus::Deleted);
                upsert_status(&mut map, to_rel, GitFileStatus::Added);
            }
            _ => {}
        }
    }

    if map.is_empty() {
        // Keep behavior resilient when gix status iterator cannot emit paths.
        let _ = work_dir;
    }

    map
}

/// `git ls-files -t --cached --others --exclude-standard --deleted`, reduced to
/// present tracked files plus ignored-filtered untracked files.
pub(crate) fn collect_files(root: &Path) -> Option<Vec<String>> {
    let repo = shared_repo(root)?.to_thread_local();
    let work_dir = repo.workdir()?.to_path_buf();
    let index = repo.index_or_load_from_head_or_empty().ok()?;

    let mut files: HashSet<String> = HashSet::new();
    for entry in index.entries() {
        if entry.stage() != gix::index::entry::Stage::Unconflicted {
            continue;
        }
        let path = entry.path(&index).to_str_lossy().into_owned();
        if work_dir.join(&path).exists() {
            files.insert(path);
        }
    }

    for (path, status) in status_map(root) {
        if status == GitFileStatus::Untracked {
            files.insert(path);
        }
    }

    let mut files: Vec<String> = files.into_iter().collect();
    files.sort();
    Some(files)
}

/// Porcelain-style changed/staged file entries and line counts without spawning
/// `git status`/`git diff --numstat`.
pub(crate) fn status_files(root: &Path) -> Option<(Vec<GitFileEntry>, Vec<GitFileEntry>)> {
    let repo = shared_repo(root)?.to_thread_local();
    let work_dir = repo.workdir()?.to_path_buf();
    let index = repo.index_or_load_from_head_or_empty().ok()?;

    let mut staged = staged_entries(&repo, &index);
    let mut changed = Vec::new();

    let status_platform = repo
        .status(gix::progress::Discard)
        .ok()?
        .untracked_files(UntrackedFiles::Files)
        .index_worktree_rewrites(Some(Rewrites {
            copies: None,
            percentage: Some(0.5),
            limit: 1000,
            ..Default::default()
        }));
    let status_iter = status_platform.into_index_worktree_iter(Vec::new()).ok()?;
    for item in status_iter.flatten() {
        match item {
            Item::Modification {
                entry,
                rela_path,
                status,
                ..
            } => {
                let path = rela_path.to_str_lossy().into_owned();
                let status_char = status_char_from_entry_status(status);
                let (additions, deletions) = match status_char {
                    'D' => (0, blob_line_count(&blob_bytes(&repo, entry.id))),
                    _ => {
                        let new_bytes = std::fs::read(work_dir.join(&path)).unwrap_or_default();
                        diff_counts(&blob_bytes(&repo, entry.id), &new_bytes)
                    }
                };
                changed.push(GitFileEntry {
                    path,
                    status_char,
                    staged: false,
                    additions,
                    deletions,
                });
            }
            Item::DirectoryContents { entry, .. } if entry.status == Status::Untracked => {
                let Ok(path) = entry.rela_path.to_path() else {
                    continue;
                };
                let path = path.to_string_lossy().to_string();
                changed.push(GitFileEntry {
                    additions: file_line_count(&work_dir.join(&path)),
                    deletions: 0,
                    path,
                    status_char: '?',
                    staged: false,
                });
            }
            Item::Rewrite {
                source,
                dirwalk_entry,
                ..
            } => {
                let Ok(from_path) = source.rela_path().to_path() else {
                    continue;
                };
                let Ok(to_path) = dirwalk_entry.rela_path.to_path() else {
                    continue;
                };
                let deletions = match &source {
                    gix::status::index_worktree::RewriteSource::RewriteFromIndex {
                        source_entry,
                        ..
                    } => blob_line_count(&blob_bytes(&repo, source_entry.id)),
                    gix::status::index_worktree::RewriteSource::CopyFromDirectoryEntry {
                        source_dirwalk_entry,
                        ..
                    } => {
                        file_line_count(&work_dir.join(source_dirwalk_entry.rela_path.to_string()))
                    }
                };
                changed.push(GitFileEntry {
                    path: from_path.to_string_lossy().to_string(),
                    status_char: 'D',
                    staged: false,
                    additions: 0,
                    deletions,
                });
                let to_path = to_path.to_string_lossy().to_string();
                changed.push(GitFileEntry {
                    additions: file_line_count(&work_dir.join(&to_path)),
                    deletions: 0,
                    path: to_path,
                    status_char: 'A',
                    staged: false,
                });
            }
            _ => {}
        }
    }

    changed.sort_by(|a, b| a.path.cmp(&b.path));
    staged.sort_by(|a, b| a.path.cmp(&b.path));
    Some((changed, staged))
}

pub fn diff_line_status_for_content(path: &Path, content: &str) -> HashMap<usize, GitLineStatus> {
    let content_line_count = line_count(content);
    if !within_diff_limits(content, content_line_count) {
        return HashMap::new();
    }

    let Some(base) = diff_base(path) else {
        return full_added_map(content);
    };

    let base_line_count = line_count(&base);
    if !within_diff_limits(&base, base_line_count) {
        return HashMap::new();
    }

    let mut input = InternedInput {
        before: Vec::with_capacity(base_line_count),
        after: Vec::with_capacity(content_line_count),
        interner: Interner::new(base_line_count + content_line_count),
    };
    input.update_before(base.split_inclusive('\n'));
    input.update_after(content.split_inclusive('\n'));

    let mut diff = imara_diff::Diff::default();
    diff.compute_with(
        ALGORITHM,
        &input.before,
        &input.after,
        input.interner.num_tokens(),
    );

    let mut map = HashMap::new();
    for hunk in diff.hunks() {
        let before = hunk.before.clone();
        let after = hunk.after.clone();

        if before.is_empty() && !after.is_empty() {
            for line in after.start..after.end {
                map.insert(line as usize, GitLineStatus::Added);
            }
            continue;
        }

        if after.is_empty() && !before.is_empty() {
            let line = after.start.saturating_sub(1) as usize;
            map.insert(line, GitLineStatus::Deleted);
            continue;
        }

        for line in after.start..after.end {
            map.insert(line as usize, GitLineStatus::Modified);
        }
    }

    map
}

pub fn diff_line_status_for_file(path: &Path) -> HashMap<usize, GitLineStatus> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return HashMap::new(),
    };
    diff_line_status_for_content(path, &content)
}

fn diff_base(path: &Path) -> Option<String> {
    let file = gix::path::realpath(path).ok()?;
    let repo_dir = file.parent()?;

    let repo = shared_repo(repo_dir)?.to_thread_local();
    let head = repo.head_commit().ok()?;
    let oid = find_file_in_commit(&repo, &head, &file).ok()?;

    let file_object = repo.find_object(oid).ok()?;
    let data = file_object.detach().data;

    let bytes = if let Some(work_dir) = repo.workdir() {
        let rel_path = file.strip_prefix(work_dir).ok()?;
        let rel_path = gix::path::try_into_bstr(rel_path).ok()?;
        let (mut pipeline, _) = repo.filter_pipeline(None).ok()?;
        let mut worktree_outcome = pipeline
            .convert_to_worktree(&data, rel_path.as_ref(), Delay::Forbid)
            .ok()?;
        let mut buf = Vec::with_capacity(data.len());
        worktree_outcome.read_to_end(&mut buf).ok()?;
        buf
    } else {
        data
    };

    Some(String::from_utf8_lossy(&bytes).to_string())
}

/// Process-global cache of opened repositories, keyed by the path handed to
/// `shared_repo`. Opening (discovering) a repo is comparatively expensive and
/// the server only ever touches a fixed set of roots, so we open once and reuse
/// the `Arc<ThreadSafeRepository>` (which is `Send + Sync`). gix reads refs and
/// objects from disk on demand, so a cached handle still observes new commits
/// and branches; only the config snapshot is captured at open time (fine — the
/// remote URL etc. don't change mid-session).
static REPO_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<ThreadSafeRepository>>>> = OnceLock::new();

/// Open (or reuse) the repository discovered upward from `root`, returning a
/// shared handle. Returns `None` when `root` is not inside a git repo, so all
/// callers degrade exactly as the previous subprocess paths did on git failure.
pub(crate) fn shared_repo(root: &Path) -> Option<Arc<ThreadSafeRepository>> {
    let cache = REPO_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(root).cloned() {
        return Some(hit);
    }
    let repo = Arc::new(open_repo(root).ok()?);
    cache
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), repo.clone());
    Some(repo)
}

// ---------------------------------------------------------------------------
// Read-only git facts via in-process gix (no subprocess spawn).
// ---------------------------------------------------------------------------

/// `git config --get remote.origin.url`.
pub(crate) fn remote_origin_url(root: &Path) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let snapshot = repo.config_snapshot();
    let value = snapshot.string("remote.origin.url")?;
    Some(value.to_str_lossy().into_owned())
}

/// Short name that `refs/remotes/origin/HEAD` points to (e.g. `"origin/master"`),
/// matching `git symbolic-ref --short refs/remotes/origin/HEAD`. `None` when the
/// symbolic ref is absent or not symbolic.
pub(crate) fn origin_head_short(root: &Path) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let reference = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    match reference.target() {
        gix::refs::TargetRef::Symbolic(name) => Some(name.shorten().to_str_lossy().into_owned()),
        gix::refs::TargetRef::Object(_) => None,
    }
}

/// Whether `refs/heads/<name>` exists (`git rev-parse --verify --quiet refs/heads/<name>`).
pub(crate) fn local_branch_exists(root: &Path, name: &str) -> bool {
    let Some(repo) = shared_repo(root) else {
        return false;
    };
    let repo = repo.to_thread_local();
    repo.find_reference(format!("refs/heads/{name}").as_str())
        .is_ok()
}

/// Current branch short name (`git rev-parse --abbrev-ref HEAD`), or `None` when
/// HEAD is detached or unborn.
pub(crate) fn current_branch(root: &Path) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let name = repo.head_name().ok()??;
    Some(name.shorten().to_str_lossy().into_owned())
}

/// Short HEAD hash (`git rev-parse --short HEAD`) for the detached-HEAD fallback.
pub(crate) fn head_short_hash(root: &Path) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let id = repo.head_id().ok()?;
    Some(id.shorten_or_id().to_string())
}

pub(crate) fn repo_root(root: &Path) -> Option<PathBuf> {
    let repo = shared_repo(root)?.to_thread_local();
    Some(repo.workdir()?.to_path_buf())
}

pub(crate) fn git_path(root: &Path, path: &str) -> Option<PathBuf> {
    let repo = shared_repo(root)?.to_thread_local();
    Some(repo.git_dir().join(path))
}

pub(crate) fn has_staged_changes(root: &Path) -> Option<bool> {
    let repo = shared_repo(root)?.to_thread_local();
    let index = repo.index_or_load_from_head_or_empty().ok()?;
    Some(!staged_entries(&repo, &index).is_empty())
}

/// Local + remote branches with the layout `/api/branches` needs. `branches`
/// holds every short name (heads then remotes) in `for-each-ref` lexical order
/// by full refname, `remotes` the remote subset, `current` the checked-out
/// branch. `*/HEAD` symbolic refs are skipped (they shadow a real branch).
pub(crate) struct BranchList {
    pub branches: Vec<String>,
    pub remotes: Vec<String>,
    pub current: Option<String>,
    /// Per-branch tip commit info (short hash, summary, author unix time),
    /// aligned with `branches` by name. Lets the picker show what each ref
    /// points at without a second round-trip.
    pub tips: Vec<BranchTip>,
}

/// The commit a branch/ref points at, for the picker's secondary line.
pub(crate) struct BranchTip {
    pub name: String,
    pub hash: String,
    pub summary: String,
    pub time: i64,
}

pub(crate) fn list_branches(root: &Path) -> Option<BranchList> {
    let repo = shared_repo(root)?.to_thread_local();
    let current = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_str_lossy().into_owned());

    // (full refname, short name, is_remote, tip) — sorted by full name to
    // reproduce `git for-each-ref refs/heads/ refs/remotes/` ordering.
    let mut entries: Vec<(String, String, bool, BranchTip)> = Vec::new();
    for reference in repo.references().ok()?.all().ok()?.flatten() {
        let full = reference.name().as_bstr().to_str_lossy().into_owned();
        let is_head = full.starts_with("refs/heads/");
        let is_remote = full.starts_with("refs/remotes/");
        if !is_head && !is_remote {
            continue;
        }
        let short = reference.name().shorten().to_str_lossy().into_owned();
        if short.ends_with("/HEAD") {
            continue;
        }
        // Peel the ref to its commit so the picker can show the tip's hash,
        // subject and date. Best-effort: a ref we can't peel still lists.
        let mut tip = BranchTip {
            name: short.clone(),
            hash: String::new(),
            summary: String::new(),
            time: 0,
        };
        if let Ok(id) = reference.into_fully_peeled_id() {
            tip.hash = id.shorten_or_id().to_string();
            if let Ok(commit) = repo.find_commit(id.detach()) {
                if let Ok(author) = commit.author() {
                    tip.time = author.time().map(|t| t.seconds).unwrap_or(0);
                }
                tip.summary = commit
                    .message()
                    .map(|m| m.summary().to_str_lossy().into_owned())
                    .unwrap_or_default();
            }
        }
        entries.push((full, short, is_remote, tip));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut branches = Vec::new();
    let mut remotes = Vec::new();
    let mut tips = Vec::new();
    for (_full, short, is_remote, tip) in entries {
        if is_remote {
            remotes.push(short.clone());
        }
        tips.push(tip);
        branches.push(short);
    }
    Some(BranchList {
        branches,
        remotes,
        current,
        tips,
    })
}

pub(crate) fn list_local_branches(root: &Path) -> Option<Vec<(String, bool)>> {
    let repo = shared_repo(root)?.to_thread_local();
    let current = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_str_lossy().into_owned());
    let mut branches = Vec::new();
    for reference in repo.references().ok()?.all().ok()?.flatten() {
        let full = reference.name().as_bstr().to_str_lossy();
        if !full.starts_with("refs/heads/") {
            continue;
        }
        let name = reference.name().shorten().to_str_lossy().into_owned();
        let is_current = current.as_deref() == Some(name.as_str());
        branches.push((name, is_current));
    }
    branches.sort_by(|a, b| a.0.cmp(&b.0));
    Some(branches)
}

/// One row of the commits list (`git log --pretty`).
pub(crate) struct CommitInfo {
    pub hash: String,      // %h (min-unique abbreviation)
    pub full_hash: String, // %H
    pub author: String,    // %an
    pub date: String,      // %ad with --date=short (YYYY-MM-DD)
    pub message: String,   // %s (subject)
}

/// `git log --skip=<skip> -n <count> --date=short` from HEAD, newest commit
/// first. Returns `None` when HEAD can't be resolved (e.g. an unborn branch),
/// matching the previous subprocess path which errored there.
pub(crate) fn commit_log(root: &Path, skip: usize, count: usize) -> Option<Vec<CommitInfo>> {
    commit_log_for_rev(root, "HEAD", skip, count)
}

pub(crate) fn commit_log_for_rev(
    root: &Path,
    rev: &str,
    skip: usize,
    count: usize,
) -> Option<Vec<CommitInfo>> {
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;

    let repo = shared_repo(root)?.to_thread_local();
    let head = repo.rev_parse_single(rev.as_bytes().as_bstr()).ok()?;
    let walk = repo
        .rev_walk([head.detach()])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()
        .ok()?;

    let mut out = Vec::with_capacity(count);
    for info in walk.skip(skip).take(count) {
        let Ok(info) = info else { break };
        let Ok(commit) = repo.find_commit(info.id) else {
            break;
        };
        let Ok(author) = commit.author() else { break };
        let date = match author.time() {
            Ok(time) => time.format_or_unix(gix::date::time::format::SHORT),
            Err(_) => String::new(),
        };
        let message = commit
            .message()
            .map(|m| m.summary().to_str_lossy().into_owned())
            .unwrap_or_default();
        out.push(CommitInfo {
            hash: info.id().shorten_or_id().to_string(),
            full_hash: info.id.to_string(),
            author: author.name.to_str_lossy().into_owned(),
            date,
            message,
        });
    }
    Some(out)
}

pub(crate) fn last_commit_for_path(root: &Path, rel_path: &str) -> Option<CommitInfo> {
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;

    let repo = shared_repo(root)?.to_thread_local();
    let head = repo.head_id().ok()?;
    let walk = repo
        .rev_walk([head.detach()])
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()
        .ok()?;
    let path = (rel_path != "." && !rel_path.is_empty()).then_some(rel_path);

    for info in walk {
        let Ok(info) = info else { break };
        let Ok(commit) = repo.find_commit(info.id) else {
            break;
        };
        if let Some(path) = path
            && !commit_changes_path(&commit, path)
        {
            continue;
        }
        let Ok(author) = commit.author() else { break };
        let date = match author.time() {
            Ok(time) => time.format_or_unix(gix::date::time::format::SHORT),
            Err(_) => String::new(),
        };
        let message = commit
            .message()
            .map(|m| m.summary().to_str_lossy().into_owned())
            .unwrap_or_default();
        return Some(CommitInfo {
            hash: info.id().shorten_or_id().to_string(),
            full_hash: info.id.to_string(),
            author: author.name.to_str_lossy().into_owned(),
            date,
            message,
        });
    }
    None
}

fn commit_changes_path(commit: &Commit<'_>, rel_path: &str) -> bool {
    let Ok(tree) = commit.tree() else {
        return false;
    };
    let current = tree
        .lookup_entry_by_path(rel_path)
        .ok()
        .flatten()
        .map(|entry| entry.object_id());
    let parent = commit
        .parent_ids()
        .next()
        .and_then(|pid| commit.repo.find_commit(pid.detach()).ok())
        .and_then(|parent| parent.tree().ok())
        .and_then(|tree| tree.lookup_entry_by_path(rel_path).ok().flatten())
        .map(|entry| entry.object_id());
    current != parent
}

// ---------------------------------------------------------------------------
// Unified-diff TEXT via in-process gix + imara (drop-in for `git diff`/`git show`).
//
// These produce the same git-style unified diff text the subprocess paths did,
// so callers keep feeding the result to `parse_unified_diff` unchanged. Two
// documented cosmetic divergences from git: the `\ No newline at end of file`
// marker is omitted, and hunk headers always carry an explicit `,1` length
// (`@@ -5,1 +5,1 @@` vs git's `@@ -5 +5 @@`). Neither affects the parsed
// `DiffFile` content, line numbers, or additions/deletions counts.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum FileChangeKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

/// Commit metadata for the commit-detail header (`git show -s`).
pub(crate) struct CommitMeta {
    pub full_hash: String,    // %H
    pub author: String,       // %an
    pub author_email: String, // %ae
    pub date: String,         // %ad --date=short
    pub message: String,      // %B (raw full message)
}

/// Resolve `rev` and read its commit metadata. `None` if it can't be resolved.
pub(crate) fn commit_meta(root: &Path, rev: &str) -> Option<CommitMeta> {
    let repo = shared_repo(root)?.to_thread_local();
    let id = repo.rev_parse_single(rev.as_bytes().as_bstr()).ok()?;
    let commit = repo.find_commit(id).ok()?;
    let author = commit.author().ok()?;
    let date = match author.time() {
        Ok(time) => time.format_or_unix(gix::date::time::format::SHORT),
        Err(_) => String::new(),
    };
    let message = commit
        .message_raw()
        .map(|m| m.to_str_lossy().into_owned())
        .unwrap_or_default();
    Some(CommitMeta {
        full_hash: commit.id().to_string(),
        author: author.name.to_str_lossy().into_owned(),
        author_email: author.email.to_str_lossy().into_owned(),
        date,
        message,
    })
}

/// `git show <rev>` patch text: the commit's tree against its first parent
/// (against the empty tree for a root commit). `only_path` restricts output to
/// a single file (`git show <rev> -- <path>`). `None` if `rev` can't resolve.
pub(crate) fn commit_diff_text(root: &Path, rev: &str, only_path: Option<&str>) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let id = repo.rev_parse_single(rev.as_bytes().as_bstr()).ok()?;
    let commit = repo.find_commit(id).ok()?;
    let new_tree = commit.tree().ok()?;
    let parent_tree = commit
        .parent_ids()
        .next()
        .and_then(|pid| repo.find_commit(pid.detach()).ok())
        .and_then(|p| p.tree().ok());
    Some(tree_diff_text(
        &repo,
        parent_tree.as_ref(),
        Some(&new_tree),
        only_path,
    ))
}

/// Resolve each revspec to its full hex object id, opening the repo once.
/// Returns `None` if the repo can't be opened or any revspec fails to resolve —
/// used to build content-addressed cache keys (a moved branch resolves to a new
/// id, so stale entries are never served).
pub(crate) fn resolve_oids(root: &Path, revs: &[&str]) -> Option<Vec<String>> {
    let repo = shared_repo(root)?.to_thread_local();
    let mut out = Vec::with_capacity(revs.len());
    for rev in revs {
        let id = repo.rev_parse_single(rev.as_bytes().as_bstr()).ok()?;
        out.push(id.detach().to_hex().to_string());
    }
    Some(out)
}

/// `git diff <base>...<compare>` patch text: the merge-base of `base` and
/// `compare` against `compare`. `None` if either side can't resolve.
pub(crate) fn compare_diff_text(
    root: &Path,
    base: &str,
    compare: &str,
    only_path: Option<&str>,
) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let base_id = repo.rev_parse_single(base.as_bytes().as_bstr()).ok()?;
    let compare_id = repo.rev_parse_single(compare.as_bytes().as_bstr()).ok()?;
    let compare_tree = repo.find_commit(compare_id).ok()?.tree().ok()?;
    // `base...compare` diffs from the merge-base, matching `git diff A...B`.
    let merge_base = repo
        .merge_base(base_id.detach(), compare_id.detach())
        .ok()?;
    let base_tree = repo.find_commit(merge_base.detach()).ok()?.tree().ok()?;
    Some(tree_diff_text(
        &repo,
        Some(&base_tree),
        Some(&compare_tree),
        only_path,
    ))
}

pub(crate) fn file_diff_text(root: &Path, path: &str, staged: bool) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let old = if staged {
        blob_at_revspec(root, &format!("HEAD:{path}")).map(String::into_bytes)
    } else {
        blob_at_revspec(root, &format!(":{path}"))
            .or_else(|| blob_at_revspec(root, &format!("HEAD:{path}")))
            .map(String::into_bytes)
    };
    let new = if staged {
        blob_at_revspec(root, &format!(":{path}")).map(String::into_bytes)
    } else {
        let work_dir = repo.workdir()?;
        std::fs::read(work_dir.join(path)).ok()
    };

    if old == new {
        return Some(String::new());
    }

    let kind = match (&old, &new) {
        (None, Some(_)) => FileChangeKind::Added,
        (Some(_), None) => FileChangeKind::Deleted,
        _ => FileChangeKind::Modified,
    };
    let old = old.unwrap_or_default();
    let new = new.unwrap_or_default();
    let mut out = String::new();
    append_file_diff(&mut out, path, path, &old, &new, kind);
    Some(out)
}

/// Build git-style unified diff text for every change between two trees, in
/// path order, optionally filtered to a single file.
fn tree_diff_text(
    repo: &Repository,
    old_tree: Option<&gix::Tree<'_>>,
    new_tree: Option<&gix::Tree<'_>>,
    only_path: Option<&str>,
) -> String {
    let Ok(mut changes) = repo.diff_tree_to_tree(old_tree, new_tree, None) else {
        return String::new();
    };
    // git emits changed files sorted by path; sort by the destination path.
    changes.sort_by_key(change_sort_key);

    let mut out = String::new();
    for change in &changes {
        if let Some(only) = only_path
            && change_sort_key(change) != only
        {
            continue;
        }
        append_change(&mut out, repo, change);
    }
    out
}

/// The path a change is filed under (destination for adds/mods/renames, the
/// removed path for deletions) — used for ordering and `-- <path>` filtering.
fn change_sort_key(change: &gix::diff::tree_with_rewrites::Change) -> String {
    use gix::diff::tree_with_rewrites::Change;
    let loc = match change {
        Change::Addition { location, .. }
        | Change::Deletion { location, .. }
        | Change::Modification { location, .. }
        | Change::Rewrite { location, .. } => location,
    };
    loc.to_str_lossy().into_owned()
}

fn blob_bytes(repo: &Repository, id: gix::ObjectId) -> Vec<u8> {
    repo.find_object(id)
        .map(|o| o.detach().data)
        .unwrap_or_default()
}

fn staged_entries(repo: &Repository, index: &gix::index::File) -> Vec<GitFileEntry> {
    let mut entries = Vec::new();
    let head_tree_id = repo.head().ok().and_then(|head| head.id()).and_then(|id| {
        id.object()
            .ok()
            .and_then(|obj| obj.peel_to_commit().ok())
            .and_then(|commit| commit.tree_id().ok())
    });

    let Some(tree_id) = head_tree_id else {
        for entry in index.entries() {
            if entry.stage() != gix::index::entry::Stage::Unconflicted {
                continue;
            }
            let path = entry.path(index).to_str_lossy().into_owned();
            entries.push(GitFileEntry {
                additions: blob_line_count(&blob_bytes(repo, entry.id)),
                deletions: 0,
                path,
                status_char: 'A',
                staged: true,
            });
        }
        return entries;
    };

    let _ = repo.tree_index_status(
        tree_id.as_ref(),
        index,
        None,
        gix::status::tree_index::TrackRenames::Disabled,
        |change,
         _tree_index,
         _worktree_index|
         -> Result<gix::diff::index::Action, std::convert::Infallible> {
            use gix::diff::index::ChangeRef;
            match change {
                ChangeRef::Addition { location, id, .. } => {
                    entries.push(GitFileEntry {
                        path: location.to_str_lossy().into_owned(),
                        status_char: 'A',
                        staged: true,
                        additions: blob_line_count(&blob_bytes(repo, id.as_ref().to_owned())),
                        deletions: 0,
                    });
                }
                ChangeRef::Deletion { location, id, .. } => {
                    entries.push(GitFileEntry {
                        path: location.to_str_lossy().into_owned(),
                        status_char: 'D',
                        staged: true,
                        additions: 0,
                        deletions: blob_line_count(&blob_bytes(repo, id.as_ref().to_owned())),
                    });
                }
                ChangeRef::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                } => {
                    let (additions, deletions) = diff_counts(
                        &blob_bytes(repo, previous_id.as_ref().to_owned()),
                        &blob_bytes(repo, id.as_ref().to_owned()),
                    );
                    entries.push(GitFileEntry {
                        path: location.to_str_lossy().into_owned(),
                        status_char: 'M',
                        staged: true,
                        additions,
                        deletions,
                    });
                }
                ChangeRef::Rewrite {
                    location,
                    source_id,
                    id,
                    ..
                } => {
                    let (additions, deletions) = diff_counts(
                        &blob_bytes(repo, source_id.as_ref().to_owned()),
                        &blob_bytes(repo, id.as_ref().to_owned()),
                    );
                    entries.push(GitFileEntry {
                        path: location.to_str_lossy().into_owned(),
                        status_char: 'R',
                        staged: true,
                        additions,
                        deletions,
                    });
                }
            }
            Ok(std::ops::ControlFlow::Continue(()))
        },
    );

    entries
}

fn status_char_from_entry_status<TSubmodule, TConflict>(
    status: EntryStatus<TSubmodule, TConflict>,
) -> char {
    match status {
        EntryStatus::Conflict { .. } => 'U',
        EntryStatus::Change(Change::Removed) => 'D',
        EntryStatus::IntentToAdd => '?',
        EntryStatus::Change(_) | EntryStatus::NeedsUpdate(_) => 'M',
    }
}

fn file_line_count(path: &Path) -> usize {
    std::fs::read(path)
        .map(|bytes| blob_line_count(&bytes))
        .unwrap_or(0)
}

fn blob_line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let nl_count = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.last() == Some(&b'\n') {
        nl_count
    } else {
        nl_count + 1
    }
}

fn diff_counts(old_bytes: &[u8], new_bytes: &[u8]) -> (usize, usize) {
    if looks_binary(old_bytes) || looks_binary(new_bytes) {
        return (0, 0);
    }
    let old = String::from_utf8_lossy(old_bytes);
    let new = String::from_utf8_lossy(new_bytes);
    let mut input = InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: Interner::new(0),
    };
    input.update_before(old.split_inclusive('\n'));
    input.update_after(new.split_inclusive('\n'));

    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Myers, &input);
    diff.postprocess_lines(&input);
    let mut additions = 0usize;
    let mut deletions = 0usize;
    for hunk in diff.hunks() {
        additions += (hunk.after.end - hunk.after.start) as usize;
        deletions += (hunk.before.end - hunk.before.start) as usize;
    }
    (additions, deletions)
}

fn append_change(
    out: &mut String,
    repo: &Repository,
    change: &gix::diff::tree_with_rewrites::Change,
) {
    use gix::diff::tree_with_rewrites::Change;
    match change {
        Change::Addition {
            location,
            entry_mode,
            id,
            ..
        } => {
            if !entry_mode.is_blob() {
                return;
            }
            let path = location.to_str_lossy();
            append_file_diff(
                out,
                &path,
                &path,
                &[],
                &blob_bytes(repo, *id),
                FileChangeKind::Added,
            );
        }
        Change::Deletion {
            location,
            entry_mode,
            id,
            ..
        } => {
            if !entry_mode.is_blob() {
                return;
            }
            let path = location.to_str_lossy();
            append_file_diff(
                out,
                &path,
                &path,
                &blob_bytes(repo, *id),
                &[],
                FileChangeKind::Deleted,
            );
        }
        Change::Modification {
            location,
            previous_id,
            id,
            entry_mode,
            previous_entry_mode,
            ..
        } => {
            if !entry_mode.is_blob() && !previous_entry_mode.is_blob() {
                return;
            }
            let path = location.to_str_lossy();
            append_file_diff(
                out,
                &path,
                &path,
                &blob_bytes(repo, *previous_id),
                &blob_bytes(repo, *id),
                FileChangeKind::Modified,
            );
        }
        Change::Rewrite {
            source_location,
            source_id,
            location,
            id,
            ..
        } => {
            let old_path = source_location.to_str_lossy();
            let new_path = location.to_str_lossy();
            append_file_diff(
                out,
                &old_path,
                &new_path,
                &blob_bytes(repo, *source_id),
                &blob_bytes(repo, *id),
                FileChangeKind::Renamed,
            );
        }
    }
}

/// Whether a blob looks binary by git's heuristic: a NUL byte in the first 8000.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Append one file's git-style unified diff (header + hunks) to `out`.
fn append_file_diff(
    out: &mut String,
    old_path: &str,
    new_path: &str,
    old_bytes: &[u8],
    new_bytes: &[u8],
    kind: FileChangeKind,
) {
    use std::fmt::Write as _;

    let _ = writeln!(out, "diff --git a/{old_path} b/{new_path}");
    match kind {
        FileChangeKind::Added => {
            let _ = writeln!(out, "new file mode 100644");
        }
        FileChangeKind::Deleted => {
            let _ = writeln!(out, "deleted file mode 100644");
        }
        FileChangeKind::Renamed => {
            let _ = writeln!(out, "rename from {old_path}");
            let _ = writeln!(out, "rename to {new_path}");
        }
        FileChangeKind::Modified => {}
    }

    if looks_binary(old_bytes) || looks_binary(new_bytes) {
        let _ = writeln!(out, "Binary files a/{old_path} and b/{new_path} differ");
        return;
    }
    if old_bytes == new_bytes {
        // Pure rename / mode change with identical content — no hunks.
        return;
    }

    let a_label = if matches!(kind, FileChangeKind::Added) {
        "/dev/null".to_string()
    } else {
        format!("a/{old_path}")
    };
    let b_label = if matches!(kind, FileChangeKind::Deleted) {
        "/dev/null".to_string()
    } else {
        format!("b/{new_path}")
    };
    let _ = writeln!(out, "--- {a_label}");
    let _ = writeln!(out, "+++ {b_label}");

    out.push_str(&unified_hunks(old_bytes, new_bytes));
}

/// Render just the `@@` hunks for a pair of blobs via imara's unified printer
/// (3 lines of context, matching git's default).
fn unified_hunks(old_bytes: &[u8], new_bytes: &[u8]) -> String {
    let old = String::from_utf8_lossy(old_bytes);
    let new = String::from_utf8_lossy(new_bytes);

    let mut input = InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: Interner::new(0),
    };
    input.update_before(old.split_inclusive('\n'));
    input.update_after(new.split_inclusive('\n'));

    // git defaults to the Myers algorithm; `postprocess_lines` applies the same
    // slider / indent heuristic git enables by default (since 2.11), so hunk
    // boundaries line up with `git diff`.
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Myers, &input);
    diff.postprocess_lines(&input);

    let printer = imara_diff::BasicLineDiffPrinter(&input.interner);
    diff.unified_diff(&printer, imara_diff::UnifiedDiffConfig::default(), &input)
        .to_string()
}

/// Raw bytes of a blob at a gix revspec such as `"HEAD:src/x.rs"`, `"master:x"`,
/// or `":src/x.rs"` (the index). Mirrors `git show <ref>:<path>` — no smudge
/// filter is applied, matching git's blob output. `None` if the path/ref can't
/// be resolved.
pub(crate) fn blob_at_revspec(root: &Path, spec: &str) -> Option<String> {
    let repo = shared_repo(root)?.to_thread_local();
    let id = repo.rev_parse_single(spec.as_bytes().as_bstr()).ok()?;
    let object = repo.find_object(id).ok()?;
    Some(String::from_utf8_lossy(&object.data).into_owned())
}

fn open_repo(path: &Path) -> Result<ThreadSafeRepository, Box<gix::discover::Error>> {
    let mut open_opts_map = gix::sec::trust::Mapping::<gix::open::Options>::default();

    let config = gix::open::permissions::Config {
        system: true,
        git: true,
        user: true,
        env: true,
        includes: true,
        git_binary: cfg!(windows),
    };

    open_opts_map.reduced = open_opts_map.reduced.permissions(gix::open::Permissions {
        config,
        ..gix::open::Permissions::default_for_level(gix::sec::Trust::Reduced)
    });
    open_opts_map.full = open_opts_map.full.permissions(gix::open::Permissions {
        config,
        ..gix::open::Permissions::default_for_level(gix::sec::Trust::Full)
    });

    let discover_opts = gix::discover::upwards::Options {
        dot_git_only: true,
        ..Default::default()
    };

    ThreadSafeRepository::discover_with_environment_overrides_opts(
        path,
        discover_opts,
        open_opts_map,
    )
    .map_err(Box::new)
}

fn find_file_in_commit(
    repo: &Repository,
    commit: &Commit<'_>,
    file: &Path,
) -> Result<ObjectId, String> {
    let repo_dir = repo
        .workdir()
        .ok_or_else(|| "repo has no worktree".to_string())?;
    let rel_path = file
        .strip_prefix(repo_dir)
        .map_err(|_| "file is outside worktree".to_string())?;

    let tree = commit.tree().map_err(|e| e.to_string())?;
    let entry = tree
        .lookup_entry_by_path(rel_path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "file is untracked".to_string())?;

    match entry.mode().kind() {
        EntryKind::Blob | EntryKind::BlobExecutable => Ok(entry.object_id()),
        _ => Err("entry is not a regular file".to_string()),
    }
}

fn map_entry_status<TSubmodule, TConflict>(
    status: EntryStatus<TSubmodule, TConflict>,
) -> GitFileStatus {
    match status {
        EntryStatus::Conflict { .. } => GitFileStatus::Conflict,
        EntryStatus::Change(Change::Removed) => GitFileStatus::Deleted,
        EntryStatus::IntentToAdd => GitFileStatus::Untracked,
        EntryStatus::Change(_) => GitFileStatus::Modified,
        EntryStatus::NeedsUpdate(_) => GitFileStatus::Modified,
    }
}

fn upsert_status(map: &mut HashMap<String, GitFileStatus>, path: String, status: GitFileStatus) {
    let entry = map.entry(path).or_insert(status);
    if priority(status) > priority(*entry) {
        *entry = status;
    }
}

fn priority(status: GitFileStatus) -> u8 {
    match status {
        GitFileStatus::Conflict => 4,
        GitFileStatus::Deleted => 3,
        GitFileStatus::Modified => 2,
        GitFileStatus::Added => 1,
        GitFileStatus::Untracked => 0,
    }
}

fn line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.split_inclusive('\n').count()
    }
}

fn within_diff_limits(content: &str, line_count: usize) -> bool {
    line_count <= MAX_DIFF_LINES && content.len() <= MAX_DIFF_BYTES
}

fn full_added_map(content: &str) -> HashMap<usize, GitLineStatus> {
    let mut out = HashMap::new();
    for (idx, line) in content.split_inclusive('\n').enumerate() {
        if !line.is_empty() {
            out.insert(idx, GitLineStatus::Added);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_added_marks_each_non_empty_line() {
        let map = full_added_map("a\nb\n");
        assert_eq!(map.get(&0), Some(&GitLineStatus::Added));
        assert_eq!(map.get(&1), Some(&GitLineStatus::Added));
    }

    #[test]
    fn diff_limits_enforced() {
        let huge = "x".repeat(MAX_DIFF_BYTES + 1);
        assert!(!within_diff_limits(&huge, 1));
    }

    // ---- Stage 1 differential parity: gix vs subprocess git on THIS repo ----

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// Run `git <args>` in the repo, returning trimmed stdout (None on failure).
    fn git(args: &[&str]) -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_root())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[test]
    fn gix_remote_url_matches_git() {
        let root = repo_root();
        assert_eq!(
            remote_origin_url(&root),
            git(&["config", "--get", "remote.origin.url"])
        );
    }

    #[test]
    fn gix_current_branch_matches_git() {
        let root = repo_root();
        let git_branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        match git_branch.as_deref() {
            // Detached HEAD: git prints "HEAD"; gix returns None.
            Some("HEAD") | None => assert_eq!(current_branch(&root), None),
            Some(name) => assert_eq!(current_branch(&root).as_deref(), Some(name)),
        }
    }

    #[test]
    fn gix_origin_head_matches_git() {
        let root = repo_root();
        assert_eq!(
            origin_head_short(&root),
            git(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        );
    }

    #[test]
    fn gix_branches_match_git_for_each_ref() {
        let root = repo_root();
        let Some(list) = list_branches(&root) else {
            return; // not a git repo in this environment
        };
        // Reproduce the endpoint's expectation from `for-each-ref`.
        let raw = git(&[
            "for-each-ref",
            "--format=%(refname)|%(refname:short)|%(HEAD)",
            "refs/heads/",
            "refs/remotes/",
        ])
        .unwrap_or_default();
        let mut expected: Vec<(String, String)> = Vec::new();
        for line in raw.lines() {
            let mut it = line.splitn(3, '|');
            let full = it.next().unwrap_or("").trim().to_string();
            let short = it.next().unwrap_or("").trim().to_string();
            // Skip the remote HEAD pointer (`refs/remotes/origin/HEAD`). Filter on
            // the FULL refname: git shortens it to just `origin` (not `origin/HEAD`),
            // so a `short.ends_with("/HEAD")` check would miss it and wrongly list
            // `origin` as a branch — which `list_branches` (gix) correctly omits.
            if short.is_empty() || full.ends_with("/HEAD") {
                continue;
            }
            expected.push((full, short));
        }
        expected.sort_by(|a, b| a.0.cmp(&b.0));
        let expected_branches: Vec<String> = expected.iter().map(|(_, s)| s.clone()).collect();
        assert_eq!(list.branches, expected_branches);
    }

    // ---- Stage 4 differential oracle: gix diff text vs `git show`/`git diff` ----

    /// File-level facts that MUST match git exactly: the set of changed files,
    /// each with its status, rename source, and binary flag. (Exact per-line
    /// counts/attribution are NOT compared — imara's and git's Myers make
    /// different but equally-valid choices among duplicate lines.)
    fn file_facts(
        files: &[crate::diff_render::DiffFile],
    ) -> Vec<(String, Option<String>, String, bool)> {
        let mut v: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    f.path.clone(),
                    f.old_path.clone(),
                    f.status.as_str().to_string(),
                    f.binary,
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Assert the gix diff describes a valid transformation of the real blobs:
    /// every added line exists in the new blob, every removed line in the old.
    /// This catches wrong-blob / old↔new-swap bugs while staying invariant to
    /// the exact edit script git chose.
    fn assert_lines_belong(
        files: &[crate::diff_render::DiffFile],
        new_blob_of: impl Fn(&str) -> String,
        old_blob_of: impl Fn(&str) -> String,
        ctx: &str,
    ) {
        use crate::diff_render::LineKind;
        for f in files {
            if f.binary {
                continue;
            }
            let new_blob = new_blob_of(&f.path);
            let new_lines: std::collections::HashSet<&str> = new_blob.lines().collect();
            let old_path = f.old_path.as_deref().unwrap_or(&f.path);
            let old_blob = old_blob_of(old_path);
            let old_lines: std::collections::HashSet<&str> = old_blob.lines().collect();
            for h in &f.hunks {
                for l in &h.lines {
                    match l.kind {
                        LineKind::Add => assert!(
                            l.content.is_empty() || new_lines.contains(l.content.as_str()),
                            "{ctx} {}: added line not in new blob: {:?}",
                            f.path,
                            l.content
                        ),
                        LineKind::Remove => assert!(
                            l.content.is_empty() || old_lines.contains(l.content.as_str()),
                            "{ctx} {}: removed line not in old blob: {:?}",
                            f.path,
                            l.content
                        ),
                        _ => {}
                    }
                }
            }
        }
    }

    fn git_raw_stdout(args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo_root())
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn gix_commit_diff_matches_git_show() {
        let root = repo_root();
        let hashes = git_raw_stdout(&["log", "-n8", "--format=%H"]);
        for hash in hashes.lines() {
            let gix_text = commit_diff_text(&root, hash, None)
                .unwrap_or_else(|| panic!("gix commit_diff_text none for {hash}"));
            let git_text = git_raw_stdout(&["show", "--format=", "--no-ext-diff", hash]);
            let gix_files = crate::diff_render::parse_unified_diff(&gix_text);
            let git_files = crate::diff_render::parse_unified_diff(&git_text);
            assert_eq!(
                file_facts(&gix_files),
                file_facts(&git_files),
                "commit {hash} file facts"
            );
            assert_lines_belong(
                &gix_files,
                |path| git_raw_stdout(&["show", &format!("{hash}:{path}")]),
                |path| git_raw_stdout(&["show", &format!("{hash}^:{path}")]),
                &format!("commit {hash}"),
            );
        }
    }

    #[test]
    fn gix_compare_diff_matches_git() {
        let root = repo_root();
        let base = git_raw_stdout(&["rev-parse", "HEAD~5"]).trim().to_string();
        let compare = git_raw_stdout(&["rev-parse", "HEAD"]).trim().to_string();
        if base.is_empty() || compare.is_empty() {
            return;
        }
        // `A...B` diffs from the merge-base; for this linear history that's `A`.
        let mb = git_raw_stdout(&["merge-base", &base, &compare])
            .trim()
            .to_string();
        let gix_text = compare_diff_text(&root, &base, &compare, None).expect("gix compare none");
        let git_text = git_raw_stdout(&["diff", "--no-ext-diff", &format!("{base}...{compare}")]);
        let gix_files = crate::diff_render::parse_unified_diff(&gix_text);
        let git_files = crate::diff_render::parse_unified_diff(&git_text);
        assert_eq!(
            file_facts(&gix_files),
            file_facts(&git_files),
            "compare file facts"
        );
        assert_lines_belong(
            &gix_files,
            |path| git_raw_stdout(&["show", &format!("{compare}:{path}")]),
            |path| git_raw_stdout(&["show", &format!("{mb}:{path}")]),
            "compare",
        );
    }

    #[test]
    fn gix_blob_at_revspec_matches_git_show() {
        let root = repo_root();
        for path in ["Cargo.toml", "src/command/git_backend.rs", "README.md"] {
            let spec = format!("HEAD:{path}");
            let got = blob_at_revspec(&root, &spec);
            // Raw `git show` stdout (untrimmed), matching the non-trimming
            // diff_server::git_output_in_repo the old code used here.
            let expected = std::process::Command::new("git")
                .args(["show", &spec])
                .current_dir(&root)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            assert_eq!(got, expected, "blob {spec}");
        }
    }

    #[test]
    fn gix_commit_log_matches_git() {
        let root = repo_root();
        let Some(rows) = commit_log(&root, 0, 20) else {
            return; // not a git repo / unborn HEAD in this environment
        };
        let raw = git(&[
            "log",
            "-n20",
            "--date=short",
            "--pretty=format:%H%x00%an%x00%ad%x00%s",
        ])
        .unwrap_or_default();
        let expected: Vec<Vec<&str>> = raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.splitn(4, '\0').collect())
            .collect();
        assert_eq!(rows.len(), expected.len(), "commit count");
        for (got, exp) in rows.iter().zip(expected.iter()) {
            assert_eq!(got.full_hash, exp[0], "full hash");
            assert_eq!(got.author, exp[1], "author");
            assert_eq!(got.date, exp[2], "date");
            assert_eq!(got.message, exp[3], "subject");
            // gix `%h` and git `%h` are both min-unique abbreviations of the
            // same commit; assert the short hash is a genuine prefix.
            assert!(
                got.full_hash.starts_with(&got.hash),
                "short hash {} not a prefix of {}",
                got.hash,
                got.full_hash
            );
        }
    }
}

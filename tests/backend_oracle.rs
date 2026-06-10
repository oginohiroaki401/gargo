//! Backend behavior oracle for the gargo server.
//!
//! This is a characterization test that locks down the **backend** contract of
//! the `/api/*` JSON endpoints. It deliberately covers the JSON APIs and
//! git-state semantics only — NOT the HTML pages — so a later frontend redesign
//! can rewrite the templates freely without regenerating these goldens.
//!
//! Determinism: the fixture repo pins author/committer dates and identity, so
//! commit dates (`--date=short`) are stable. Commit hashes are still normalized
//! to `<HASH>` before snapshotting to stay robust across environments.
//!
//! Golden files live in `tests/golden/backend_oracle/*.json`. Run with
//! `UPDATE_GOLDEN=1 cargo test --test backend_oracle` to (re)write them.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use gargo::command::gargo_server::{GargoServerCommand, GargoServerEvent, GargoServerHandle};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

// --- git + http harness ------------------------------------------------------

/// A fixed timestamp for every commit so `--date=short` output is deterministic.
const FIXED_DATE: &str = "2021-06-01T12:00:00 +0000";

/// Redirect the persisted viewed-state store to a process-wide temp dir so the
/// tests never touch the real data dir. Shared across tests (same value) so the
/// parallel `set_var` calls don't race on a meaningful difference.
fn isolate_data_dir() {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| tempdir().expect("temp data dir"));
    unsafe {
        std::env::set_var("XDG_DATA_HOME", dir.path());
    }
}

/// Run a git command with pinned author/committer dates (harmless for
/// non-commit subcommands; required so commit dates/hashes are reproducible).
fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_DATE", FIXED_DATE)
        .env("GIT_COMMITTER_DATE", FIXED_DATE)
        .output()
        .expect("run git command");
    assert!(
        output.status.success(),
        "git command failed: git {}\nstdout={}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git command");
    assert!(output.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn get_json_with_retry(url: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                assert_eq!(resp.status(), 200, "GET {url}");
                return resp.into_json().expect("valid json body");
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to call {url}: {err}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn post_json(url: &str, payload: Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::post(url).send_json(payload.clone()) {
            Ok(resp) => {
                assert_eq!(resp.status(), 200, "POST {url}");
                return resp.into_json().expect("valid json body");
            }
            Err(ureq::Error::Status(code, _)) => panic!("POST {url} returned status {code}"),
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to POST {url}: {err}");
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn start_gargo_server(repo: &Path, handle: &GargoServerHandle) -> Option<u16> {
    handle
        .command_tx
        .send(GargoServerCommand::Start {
            repo_root: repo.to_path_buf(),
            port: None,
            ai_config: Default::default(),
        })
        .expect("send gargo start");
    match handle.event_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(GargoServerEvent::Started { port, .. }) => Some(port),
        Ok(GargoServerEvent::Error(msg)) if msg.starts_with("Failed to bind Gargo server") => {
            eprintln!("skip oracle: {msg}");
            None
        }
        other => panic!("expected gargo Started, got {other:?}"),
    }
}

// --- normalization + golden compare ------------------------------------------

/// Replace non-deterministic fields (commit hashes) in-place so snapshots are
/// stable. Recurses into the whole JSON tree; any `hash` / `full_hash` string
/// value becomes `<HASH>`.
fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if (k == "hash" || k == "full_hash") && v.is_string() {
                    *v = Value::String("<HASH>".to_string());
                } else {
                    normalize(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize),
        _ => {}
    }
}

/// Compare `value` (after normalization) against the committed golden file.
/// `UPDATE_GOLDEN=1` rewrites the golden instead of asserting.
fn assert_golden(name: &str, mut value: Value) {
    normalize(&mut value);
    let pretty = serde_json::to_string_pretty(&value).expect("serialize golden");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/backend_oracle");
    let path = dir.join(format!("{name}.json"));

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        fs::create_dir_all(&dir).expect("create golden dir");
        fs::write(&path, format!("{pretty}\n")).expect("write golden");
        return;
    }

    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run UPDATE_GOLDEN=1 cargo test --test backend_oracle",
            path.display()
        )
    });
    assert_eq!(
        pretty.trim(),
        expected.trim(),
        "golden mismatch for {name} (run UPDATE_GOLDEN=1 to refresh after an intentional change)"
    );
}

// --- fixture -----------------------------------------------------------------

/// Build a deterministic repo:
/// - master: initial commit (README.md, src/lib.rs)
/// - feature branch: src/lib.rs gains a function (for compare)
/// - working tree on master: src/lib.rs modified (unstaged), added.rs staged,
///   untracked.txt untracked
fn setup_repo(repo: &Path) {
    git(repo, &["init", "-b", "master"]);
    git(repo, &["config", "user.name", "gargo-test"]);
    git(repo, &["config", "user.email", "gargo-test@example.com"]);

    fs::write(repo.join("README.md"), "# Test Repo\n\nhello\n").unwrap();
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn base() {}\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "initial commit"]);

    git(repo, &["checkout", "-b", "feature"]);
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn base() {}\npub fn feature() {}\n",
    )
    .unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", "feature commit"]);
    git(repo, &["checkout", "master"]);

    // Working-tree state: unstaged modification, a staged new file, an untracked file.
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn base() {}\npub fn local() {}\n",
    )
    .unwrap();
    fs::write(repo.join("added.rs"), "pub fn added() {}\n").unwrap();
    git(repo, &["add", "added.rs"]);
    fs::write(repo.join("untracked.txt"), "scratch\n").unwrap();
}

// --- read-endpoint golden snapshots ------------------------------------------

#[test]
fn backend_oracle_read_endpoints() {
    isolate_data_dir();
    let repo_dir = tempdir().expect("temp repo");
    let repo = repo_dir.path();
    setup_repo(repo);

    let gargo_handle = GargoServerHandle::new().expect("gargo handle");
    let Some(gargo_port) = start_gargo_server(repo, &gargo_handle) else {
        return;
    };

    let g = format!("http://127.0.0.1:{gargo_port}");

    // status + compare endpoints
    assert_golden("status", get_json_with_retry(&format!("{g}/api/status")));
    assert_golden(
        "status_file_unstaged",
        get_json_with_retry(&format!(
            "{g}/api/status/file?section=unstaged&path=src/lib.rs"
        )),
    );
    assert_golden(
        "status_file_staged",
        get_json_with_retry(&format!("{g}/api/status/file?section=staged&path=added.rs")),
    );
    assert_golden(
        "branches",
        get_json_with_retry(&format!("{g}/api/branches")),
    );
    assert_golden(
        "compare",
        get_json_with_retry(&format!("{g}/api/compare?base=master&compare=feature")),
    );
    assert_golden(
        "compare_file",
        get_json_with_retry(&format!(
            "{g}/api/compare/file?base=master&compare=feature&path=src/lib.rs"
        )),
    );

    // tree + blob + commit endpoints
    assert_golden(
        "tree_src",
        get_json_with_retry(&format!("{g}/api/tree/src")),
    );
    assert_golden(
        "blob_readme",
        get_json_with_retry(&format!("{g}/api/blob/README.md")),
    );
    assert_golden(
        "blob_lib",
        get_json_with_retry(&format!("{g}/api/blob/src/lib.rs")),
    );
    assert_golden("commits", get_json_with_retry(&format!("{g}/api/commits")));

    let head = git_output(repo, &["rev-parse", "HEAD"]);
    assert_golden(
        "commit",
        get_json_with_retry(&format!("{g}/api/commit/{head}")),
    );
    assert_golden(
        "commit_file",
        get_json_with_retry(&format!("{g}/api/commit/{head}/file?path=src/lib.rs")),
    );

    let _ = gargo_handle.command_tx.send(GargoServerCommand::Stop);
}

// --- POST state-transition semantics (structural, not golden) ----------------

/// Collect `path -> viewed` for a status section array.
fn viewed_by_path(section: &Value) -> BTreeMap<String, bool> {
    section
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some((
                        v.get("path")?.as_str()?.to_string(),
                        v.get("viewed")?.as_bool()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn paths_of(section: &Value) -> Vec<String> {
    section
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.get("path")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn backend_oracle_stage_unstage_viewed_commit() {
    isolate_data_dir();
    let repo_dir = tempdir().expect("temp repo");
    let repo = repo_dir.path();
    setup_repo(repo);

    let handle = GargoServerHandle::new().expect("gargo handle");
    let Some(port) = start_gargo_server(repo, &handle) else {
        return;
    };
    let base = format!("http://127.0.0.1:{port}");

    // Baseline: src/lib.rs unstaged, added.rs staged, untracked.txt untracked.
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert_eq!(paths_of(&status["unstaged"]), vec!["src/lib.rs"]);
    assert_eq!(paths_of(&status["staged"]), vec!["added.rs"]);
    assert_eq!(paths_of(&status["untracked"]), vec!["untracked.txt"]);

    // Stage src/lib.rs -> moves unstaged -> staged.
    let resp = post_json(
        &format!("{base}/api/status/stage"),
        serde_json::json!({ "path": "src/lib.rs" }),
    );
    assert_eq!(resp["ok"], Value::Bool(true), "stage response: {resp}");
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert!(
        !paths_of(&status["unstaged"]).contains(&"src/lib.rs".to_string()),
        "src/lib.rs should leave unstaged after stage"
    );
    assert!(
        paths_of(&status["staged"]).contains(&"src/lib.rs".to_string()),
        "src/lib.rs should appear in staged after stage"
    );

    // Unstage it back -> returns to unstaged.
    let resp = post_json(
        &format!("{base}/api/status/unstage"),
        serde_json::json!({ "path": "src/lib.rs" }),
    );
    assert_eq!(resp["ok"], Value::Bool(true), "unstage response: {resp}");
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert!(
        paths_of(&status["unstaged"]).contains(&"src/lib.rs".to_string()),
        "src/lib.rs should return to unstaged after unstage"
    );

    // Mark added.rs viewed -> persists, then clears.
    let resp = post_json(
        &format!("{base}/api/status/viewed"),
        serde_json::json!({ "section": "staged", "path": "added.rs", "viewed": true }),
    );
    assert_eq!(resp["viewed"], Value::Bool(true));
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert_eq!(
        viewed_by_path(&status["staged"]).get("added.rs"),
        Some(&true),
        "added.rs should read back as viewed"
    );
    let resp = post_json(
        &format!("{base}/api/status/viewed"),
        serde_json::json!({ "section": "staged", "path": "added.rs", "viewed": false }),
    );
    assert_eq!(resp["viewed"], Value::Bool(false));
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert_eq!(
        viewed_by_path(&status["staged"]).get("added.rs"),
        Some(&false),
        "added.rs viewed flag should clear"
    );

    // commit-prepare reports the staged files + branch.
    let prep = get_json_with_retry(&format!("{base}/api/status/commit-prepare"));
    assert!(
        paths_of(&prep["staged"]).contains(&"added.rs".to_string()),
        "commit-prepare should list staged added.rs: {prep}"
    );

    // Commit the staged file, then verify it leaves the staged set and lands in git log.
    let resp = post_json(
        &format!("{base}/api/status/commit"),
        serde_json::json!({ "message": "oracle commit" }),
    );
    assert_eq!(resp["ok"], Value::Bool(true), "commit response: {resp}");
    let log = git_output(repo, &["log", "-1", "--pretty=%s"]);
    assert_eq!(log, "oracle commit");
    let status = get_json_with_retry(&format!("{base}/api/status"));
    assert!(
        !paths_of(&status["staged"]).contains(&"added.rs".to_string()),
        "added.rs should be gone from staged after commit"
    );

    let _ = handle.command_tx.send(GargoServerCommand::Stop);
}

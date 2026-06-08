//! End-to-end coverage for the browser editor's last-file endpoint
//! (`/api/last-file`) that backs a bare `/editor` reopening the file you last
//! had open. The record is persisted server-side (keyed by repo root) so it
//! survives the fresh random port the server binds on every start — the reason
//! the previous localStorage-based approach broke. Spins up the real server
//! against a temp repo, with `XDG_DATA_HOME` redirected so the store stays out
//! of the developer's real data dir.

use std::path::Path;
use std::time::Duration;

use gargo::command::gargo_server::{GargoServerCommand, GargoServerEvent, GargoServerHandle};
use tempfile::tempdir;

fn run_git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git command");
    assert!(output.status.success(), "git {} failed", args.join(" "));
}

fn start_server(repo: &Path, handle: &GargoServerHandle) -> Option<u16> {
    handle
        .command_tx
        .send(GargoServerCommand::Start {
            repo_root: repo.to_path_buf(),
            port: None,
        })
        .expect("send start");
    match handle.event_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(GargoServerEvent::Started { port, .. }) => Some(port),
        Ok(GargoServerEvent::Error(msg)) if msg.starts_with("Failed to bind Gargo server") => {
            eprintln!("skip web editor last-file test: {msg}");
            None
        }
        other => panic!("expected Started, got {other:?}"),
    }
}

fn setup_repo() -> tempfile::TempDir {
    let dir = tempdir().expect("temp repo");
    let repo = dir.path();
    run_git(repo, &["init", "-b", "master"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    std::fs::write(repo.join("README.md"), "# Test\n").expect("write readme");
    std::fs::create_dir(repo.join("src")).expect("mkdir src");
    std::fs::write(repo.join("src/lib.rs"), "pub fn base() {}\n").expect("write lib");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "initial"]);
    dir
}

#[test]
fn last_file_records_validates_and_forgets() {
    // Redirect the persisted-store location so the test never touches the real
    // data dir. Single test in this binary → no env-var races with other tests.
    let data_dir = tempdir().expect("temp data dir");
    unsafe {
        std::env::set_var("XDG_DATA_HOME", data_dir.path());
    }

    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let handle = GargoServerHandle::new().expect("server handle");
    let Some(port) = start_server(repo, &handle) else {
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    std::thread::sleep(Duration::from_millis(150));

    // Nothing recorded yet → null.
    let resp: serde_json::Value = ureq::get(&format!("{base}/api/last-file"))
        .call()
        .expect("get last-file")
        .into_json()
        .expect("json");
    assert_eq!(resp["path"], serde_json::Value::Null);

    // Record a real file → echoed back, then readable on a fresh GET.
    let resp = ureq::post(&format!("{base}/api/last-file"))
        .send_json(ureq::json!({ "path": "src/lib.rs" }))
        .expect("record");
    assert_eq!(resp.status(), 200);
    let resp: serde_json::Value = ureq::get(&format!("{base}/api/last-file"))
        .call()
        .expect("get last-file")
        .into_json()
        .expect("json");
    assert_eq!(resp["path"], "src/lib.rs");

    // Path traversal is rejected and leaves the record untouched.
    let err = ureq::post(&format!("{base}/api/last-file"))
        .send_json(ureq::json!({ "path": "../escape.rs" }))
        .expect_err("traversal rejected");
    match err {
        ureq::Error::Status(code, _) => assert_eq!(code, 400),
        other => panic!("expected 400, got {other:?}"),
    }
    let resp: serde_json::Value = ureq::get(&format!("{base}/api/last-file"))
        .call()
        .expect("get last-file")
        .into_json()
        .expect("json");
    assert_eq!(resp["path"], "src/lib.rs");

    // A recorded file that has since been deleted degrades to null (no broken
    // redirect), without the client having to forget it explicitly.
    std::fs::remove_file(repo.join("src/lib.rs")).expect("delete recorded file");
    let resp: serde_json::Value = ureq::get(&format!("{base}/api/last-file"))
        .call()
        .expect("get last-file")
        .into_json()
        .expect("json");
    assert_eq!(resp["path"], serde_json::Value::Null);

    // Re-record an existing file, then explicitly forget it with a null path.
    ureq::post(&format!("{base}/api/last-file"))
        .send_json(ureq::json!({ "path": "README.md" }))
        .expect("record readme");
    ureq::post(&format!("{base}/api/last-file"))
        .send_json(ureq::json!({ "path": serde_json::Value::Null }))
        .expect("forget");
    let resp: serde_json::Value = ureq::get(&format!("{base}/api/last-file"))
        .call()
        .expect("get last-file")
        .into_json()
        .expect("json");
    assert_eq!(resp["path"], serde_json::Value::Null);
}

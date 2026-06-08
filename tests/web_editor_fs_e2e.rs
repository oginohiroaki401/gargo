//! End-to-end coverage for the browser editor's sidebar filesystem endpoints
//! (`/api/fs/create`, `/api/fs/rename`, `/api/fs/reveal`) that back the
//! right-click context menu. Spins up the real server against a temp repo.

use std::fs;
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
            eprintln!("skip web editor fs test: {msg}");
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
    fs::write(repo.join("README.md"), "# Test\n").expect("write readme");
    fs::create_dir(repo.join("src")).expect("mkdir src");
    fs::write(repo.join("src/lib.rs"), "pub fn base() {}\n").expect("write lib");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "initial"]);
    dir
}

#[test]
fn fs_endpoints_create_rename_delete_and_validate_paths() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let handle = GargoServerHandle::new().expect("server handle");
    let Some(port) = start_server(repo, &handle) else {
        return;
    };
    let base = format!("http://127.0.0.1:{port}");

    // Give the listener a moment to accept connections.
    std::thread::sleep(Duration::from_millis(150));

    // Create a new file inside an existing dir.
    let resp = ureq::post(&format!("{base}/api/fs/create"))
        .send_json(ureq::json!({ "path": "src/new_mod.rs", "kind": "file" }))
        .expect("create file");
    assert_eq!(resp.status(), 200);
    assert!(repo.join("src/new_mod.rs").is_file());

    // Create a new (empty) directory, including a missing parent segment.
    let resp = ureq::post(&format!("{base}/api/fs/create"))
        .send_json(ureq::json!({ "path": "docs/guides", "kind": "dir" }))
        .expect("create dir");
    assert_eq!(resp.status(), 200);
    assert!(repo.join("docs/guides").is_dir());

    // Refusing to clobber an existing entry → 400.
    let err = ureq::post(&format!("{base}/api/fs/create"))
        .send_json(ureq::json!({ "path": "README.md", "kind": "file" }))
        .unwrap_err();
    assert_eq!(err.into_response().map(|r| r.status()), Some(400));

    // Rename the file.
    let resp = ureq::post(&format!("{base}/api/fs/rename"))
        .send_json(ureq::json!({ "from": "src/new_mod.rs", "to": "src/renamed.rs" }))
        .expect("rename");
    assert_eq!(resp.status(), 200);
    assert!(!repo.join("src/new_mod.rs").exists());
    assert!(repo.join("src/renamed.rs").is_file());

    // Delete a file.
    let resp = ureq::post(&format!("{base}/api/fs/delete"))
        .send_json(ureq::json!({ "path": "src/renamed.rs" }))
        .expect("delete file");
    assert_eq!(resp.status(), 200);
    assert!(!repo.join("src/renamed.rs").exists());

    // Delete a directory recursively.
    fs::write(repo.join("docs/guides/intro.md"), "hi\n").expect("write nested");
    let resp = ureq::post(&format!("{base}/api/fs/delete"))
        .send_json(ureq::json!({ "path": "docs" }))
        .expect("delete dir");
    assert_eq!(resp.status(), 200);
    assert!(!repo.join("docs").exists());

    // Deleting the repo root is refused.
    let err = ureq::post(&format!("{base}/api/fs/delete"))
        .send_json(ureq::json!({ "path": "" }))
        .unwrap_err();
    assert_eq!(err.into_response().map(|r| r.status()), Some(400));
    assert!(repo.join("README.md").exists());

    // Path traversal is rejected on every endpoint.
    let err = ureq::post(&format!("{base}/api/fs/create"))
        .send_json(ureq::json!({ "path": "../escape.txt", "kind": "file" }))
        .unwrap_err();
    assert_eq!(err.into_response().map(|r| r.status()), Some(400));
    assert!(!repo.parent().unwrap().join("escape.txt").exists());

    // Reveal on a non-existent path errors out without spawning a file manager.
    let err = ureq::post(&format!("{base}/api/fs/reveal"))
        .send_json(ureq::json!({ "path": "does/not/exist" }))
        .unwrap_err();
    assert_eq!(err.into_response().map(|r| r.status()), Some(400));
}

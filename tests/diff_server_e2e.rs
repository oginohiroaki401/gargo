use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gargo::command::diff_server::{DiffServerCommand, DiffServerEvent, DiffServerHandle};
use tempfile::tempdir;

struct WorkingDirGuard {
    original: PathBuf,
}

impl WorkingDirGuard {
    fn set(path: &Path) -> Self {
        let original = std::env::current_dir().expect("read current dir");
        std::env::set_current_dir(path).expect("switch current dir");
        Self { original }
    }
}

impl Drop for WorkingDirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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

fn run_git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git command");

    assert!(
        output.status.success(),
        "git command failed: git {}\nstdout={}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn read_event(rx: &std::sync::mpsc::Receiver<DiffServerEvent>) -> DiffServerEvent {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("timed out waiting for diff server event")
}

fn get_json_with_retry(url: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                return resp.into_json().expect("valid json body");
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to call {}: {}", url, err);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn get_status_code_with_retry(url: &str) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => return resp.status(),
            Err(ureq::Error::Status(code, _)) => return code,
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to call {}: {}", url, err);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn get_text_with_retry(url: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                return resp.into_string().expect("valid text body");
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to call {}: {}", url, err);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn cwd_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    match LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(lock) => lock,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn start_diff_server(project_root: &Path, handle: &DiffServerHandle) -> Option<u16> {
    handle
        .command_tx
        .send(DiffServerCommand::Start {
            project_root: project_root.to_path_buf(),
        })
        .expect("send start command");

    match read_event(&handle.event_rx) {
        DiffServerEvent::Started { port } => Some(port),
        DiffServerEvent::Error(msg) if msg.starts_with("Failed to bind diff server") => {
            eprintln!("skip diff server test: {}", msg);
            None
        }
        event => {
            panic!("expected Started event, got: {:?}", event);
        }
    }
}

fn stop_diff_server(handle: &DiffServerHandle) {
    let _ = handle.command_tx.send(DiffServerCommand::Stop);
}

#[test]
fn test_diff_server_start_stop_and_status_api_results() {
    let _cwd_lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    let tracked_file = repo.join("sample.txt");
    fs::write(&tracked_file, "line1\n").expect("write initial file");
    run_git(repo, &["add", "sample.txt"]);
    run_git(repo, &["commit", "-m", "init"]);
    let repo_root = run_git_output(repo, &["rev-parse", "--show-toplevel"]);

    fs::write(&tracked_file, "line1\nline2\n").expect("modify tracked file");

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    let status_url = format!("http://127.0.0.1:{}/api/status", port);
    let body = get_json_with_retry(&status_url);

    let diff = body["unstaged_diff"]
        .as_str()
        .expect("unstaged_diff field should be a string");
    assert!(
        diff.contains("+line2"),
        "expected patch to include added line, got: {}",
        diff
    );
    assert_eq!(body["staged_diff"].as_str(), Some(""));
    assert_eq!(body["untracked_diff"].as_str(), Some(""));
    let untracked = body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(untracked.is_empty(), "unexpected untracked files: {}", body);

    let html_url = format!("http://127.0.0.1:{}/diff", port);
    let html = get_text_with_retry(&html_url);
    assert!(
        html.contains("id=\"show-untracked\""),
        "expected diff UI to include show-untracked checkbox"
    );
    assert!(
        html.contains("id=\"expand-all-btn\"") && html.contains("id=\"collapse-all-btn\""),
        "expected diff UI to include expand/collapse all controls"
    );
    assert!(
        html.contains("id=\"go-top-btn\""),
        "expected diff UI to include sticky go-top button"
    );
    assert!(html.contains("Git diff"), "expected diff UI context label");
    assert!(
        html.contains(&format!(r#"<code id="root-path">{}</code>"#, repo_root)),
        "expected diff UI header to include absolute root path"
    );
    assert!(
        html.contains("urlParams.get(\"show_untracked\")")
            && html.contains("showUntrackedToggle.checked = parseBoolParam"),
        "expected diff UI to default show_untracked from query fallback"
    );
    assert!(
        html.contains("AUTO_REFRESH_INTERVAL_MS = 2000")
            && html.contains("window.setInterval(() =>")
            && html.contains("loadStatus({ showLoading: false })")
            && html.contains("cache: \"no-store\""),
        "expected diff UI to auto-refresh without loading flicker"
    );
    assert!(
        html.contains("GO_TOP_SHOW_SCROLL_Y = 240")
            && html.contains("updateGoTopButtonVisibility")
            && html.contains("window.addEventListener(\"scroll\", updateGoTopButtonVisibility")
            && html.contains("window.scrollTo({ top: 0, behavior: \"smooth\" })"),
        "expected diff UI to include sticky go-top visibility and smooth scroll behavior"
    );
    assert!(
        html.contains("COLLAPSED_FILES_STORAGE_KEY")
            && html.contains("loadIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY)")
            && html.contains("persistIdSet(sessionStorage, COLLAPSED_FILES_STORAGE_KEY"),
        "expected diff UI to persist collapsed file state in session storage"
    );
    assert!(
        html.contains("VIEWED_FILES_STORAGE_KEY")
            && html.contains("loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY)")
            && html.contains("persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY"),
        "expected diff UI to persist viewed file state in local storage"
    );
    assert!(
        html.contains("className = \"diff-toggle-btn\"")
            && html.contains("wrapper.classList.toggle(\"diff-file-collapsed\", isCollapsed)")
            && html.contains("wrapper.dataset.diffFileId = fileId"),
        "expected diff UI to wire per-file diff toggle and collapsed state"
    );
    assert!(
        html.contains("className = \"diff-viewed-label\"")
            && html.contains("wrapper.classList.toggle(\"diff-file-viewed\", isViewed)")
            && html.contains("textContent = \"Viewed\""),
        "expected diff UI to render a per-file Viewed checkbox"
    );
    assert!(
        html.contains("header.insertBefore(toggleButton, header.firstChild)"),
        "expected diff UI to place the toggle chevron on the left of the file header"
    );
    assert!(
        html.contains("id=\"files-list\"")
            && html.contains("id=\"files-heading\"")
            && html.contains("id=\"changed-diff\"")
            && html.contains("id=\"staged-diff\"")
            && html.contains("id=\"untracked-diff\""),
        "expected diff UI to include unified files list and diff containers"
    );
    let files_heading_idx = html
        .find("id=\"files-heading\"")
        .expect("expected files-heading element");
    let changed_diff_idx = html
        .find("<h2>Changed Diff</h2>")
        .expect("expected Changed Diff heading");
    let staged_diff_idx = html
        .find("<h2>Staged Diff</h2>")
        .expect("expected Staged Diff heading");
    let untracked_diff_idx = html
        .find("<h2>Untracked Diff</h2>")
        .expect("expected Untracked Diff heading");
    assert!(
        files_heading_idx < changed_diff_idx
            && changed_diff_idx < staged_diff_idx
            && staged_diff_idx < untracked_diff_idx,
        "expected unified files heading before diff sections in /diff HTML"
    );

    handle
        .command_tx
        .send(DiffServerCommand::Start {
            project_root: repo.to_path_buf(),
        })
        .expect("send duplicate start");

    match read_event(&handle.event_rx) {
        DiffServerEvent::Error(msg) => {
            assert!(msg.contains("already running"), "unexpected error: {}", msg)
        }
        event => panic!("expected already-running error, got: {:?}", event),
    }

    stop_diff_server(&handle);

    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }

    handle
        .command_tx
        .send(DiffServerCommand::Stop)
        .expect("send duplicate stop");

    match read_event(&handle.event_rx) {
        DiffServerEvent::Error(msg) => {
            assert!(msg.contains("not running"), "unexpected error: {}", msg)
        }
        event => panic!("expected not-running error, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_status_sections_and_removed_endpoints() {
    let _cwd_lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    let tracked_file = repo.join("sample.txt");
    fs::write(&tracked_file, "line1\n").expect("write initial file");
    run_git(repo, &["add", "sample.txt"]);
    run_git(repo, &["commit", "-m", "init"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    let status_url = format!("http://127.0.0.1:{}/api/status", port);

    // Clean repo: no unstaged/staged/untracked changes.
    let clean_body = get_json_with_retry(&status_url);
    assert_eq!(clean_body["unstaged_diff"].as_str(), Some(""));
    assert_eq!(clean_body["staged_diff"].as_str(), Some(""));
    assert_eq!(clean_body["untracked_diff"].as_str(), Some(""));
    let clean_untracked = clean_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        clean_untracked.is_empty(),
        "expected no untracked files: {}",
        clean_body
    );

    // Unstaged-only scenario.
    fs::write(&tracked_file, "line1\nline2\n").expect("write unstaged change");
    let unstaged_only_body = get_json_with_retry(&status_url);
    let unstaged_diff = unstaged_only_body["unstaged_diff"]
        .as_str()
        .unwrap_or_else(|| panic!("unstaged_diff should be a string: {}", unstaged_only_body));
    assert!(
        unstaged_diff.contains("+line2"),
        "expected unstaged diff to include +line2: {}",
        unstaged_diff
    );
    assert_eq!(unstaged_only_body["staged_diff"].as_str(), Some(""));
    let unstaged_only_untracked = unstaged_only_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        unstaged_only_untracked.is_empty(),
        "expected no untracked files in unstaged-only scenario: {}",
        unstaged_only_body
    );
    assert_eq!(unstaged_only_body["untracked_diff"].as_str(), Some(""));

    // Staged-only scenario.
    run_git(repo, &["add", "sample.txt"]);
    let staged_only_body = get_json_with_retry(&status_url);
    assert_eq!(staged_only_body["unstaged_diff"].as_str(), Some(""));
    let staged_diff = staged_only_body["staged_diff"]
        .as_str()
        .expect("staged_diff should be a string");
    assert!(
        staged_diff.contains("+line2"),
        "expected staged diff to include +line2: {}",
        staged_diff
    );
    let staged_only_untracked = staged_only_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        staged_only_untracked.is_empty(),
        "expected no untracked files in staged-only scenario: {}",
        staged_only_body
    );
    assert_eq!(staged_only_body["untracked_diff"].as_str(), Some(""));

    // Untracked-only addition on top of staged changes.
    let untracked_file = repo.join("new-untracked.txt");
    fs::write(&untracked_file, "new file\n").expect("write untracked file");
    let with_untracked_body = get_json_with_retry(&status_url);
    let with_untracked = with_untracked_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        with_untracked
            .iter()
            .any(|v| v.as_str() == Some("new-untracked.txt")),
        "expected untracked file in response: {}",
        with_untracked_body
    );
    let with_untracked_diff = with_untracked_body["untracked_diff"]
        .as_str()
        .expect("untracked_diff should be a string");
    assert!(
        with_untracked_diff.contains("diff --git a/new-untracked.txt b/new-untracked.txt"),
        "expected untracked_diff to contain synthetic patch header: {}",
        with_untracked_diff
    );

    let with_untracked_hidden_body =
        get_json_with_retry(&format!("{}?show_untracked=false", status_url));
    let with_untracked_hidden = with_untracked_hidden_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        with_untracked_hidden.is_empty(),
        "expected hidden-untracked response to omit untracked files: {}",
        with_untracked_hidden_body
    );
    assert_eq!(
        with_untracked_hidden_body["untracked_diff"].as_str(),
        Some("")
    );
    let hidden_staged = with_untracked_hidden_body["staged_diff"]
        .as_str()
        .expect("staged_diff should be a string");
    assert!(
        hidden_staged.contains("+line2"),
        "expected staged diff to remain available when show_untracked=false: {}",
        hidden_staged
    );

    // Mixed scenario: staged + unstaged + untracked.
    fs::write(&tracked_file, "line1\nline2\nline3\n").expect("write mixed scenario change");
    let mixed_body = get_json_with_retry(&status_url);
    let mixed_unstaged = mixed_body["unstaged_diff"]
        .as_str()
        .expect("unstaged_diff should be a string");
    assert!(
        mixed_unstaged.contains("+line3"),
        "expected unstaged diff to include +line3: {}",
        mixed_unstaged
    );
    assert!(
        !mixed_unstaged.contains("+line2"),
        "did not expect +line2 in unstaged diff: {}",
        mixed_unstaged
    );

    let mixed_staged = mixed_body["staged_diff"]
        .as_str()
        .expect("staged_diff should be a string");
    assert!(
        mixed_staged.contains("+line2"),
        "expected staged diff to include +line2: {}",
        mixed_staged
    );
    assert!(
        !mixed_staged.contains("+line3"),
        "did not expect +line3 in staged diff: {}",
        mixed_staged
    );
    let mixed_untracked = mixed_body["untracked_files"]
        .as_array()
        .expect("untracked_files should be an array");
    assert!(
        mixed_untracked
            .iter()
            .any(|v| v.as_str() == Some("new-untracked.txt")),
        "expected untracked file in mixed response: {}",
        mixed_body
    );
    let mixed_untracked_diff = mixed_body["untracked_diff"]
        .as_str()
        .expect("untracked_diff should be a string");
    assert!(
        mixed_untracked_diff.contains("diff --git a/new-untracked.txt b/new-untracked.txt"),
        "expected untracked diff in mixed response: {}",
        mixed_untracked_diff
    );

    let branches_body =
        get_json_with_retry(&format!("http://127.0.0.1:{}/api/branches", port));
    let branches_arr = branches_body["branches"]
        .as_array()
        .expect("branches should be an array");
    assert!(
        !branches_arr.is_empty(),
        "expected at least one local branch in /api/branches response: {}",
        branches_body
    );
    assert!(
        branches_body.get("current").is_some(),
        "expected /api/branches to include `current` field: {}",
        branches_body
    );
    assert!(
        branches_body.get("default").is_some(),
        "expected /api/branches to include `default` field: {}",
        branches_body
    );

    let diff_status = get_status_code_with_retry(&format!("http://127.0.0.1:{}/api/diff", port));
    assert_eq!(diff_status, 404, "expected /api/diff to be removed");

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_uses_explicit_project_root_instead_of_process_cwd() {
    let _cwd_lock = cwd_test_lock();

    let repo_a_dir = tempdir().expect("create temp repo A");
    let repo_a = repo_a_dir.path();
    run_git(repo_a, &["init"]);
    run_git(repo_a, &["config", "user.name", "gargo-test"]);
    run_git(repo_a, &["config", "user.email", "gargo-test@example.com"]);
    let file_a = repo_a.join("sample.txt");
    fs::write(&file_a, "line-a-1\n").expect("write initial file A");
    run_git(repo_a, &["add", "sample.txt"]);
    run_git(repo_a, &["commit", "-m", "init a"]);
    fs::write(&file_a, "line-a-1\nline-from-a\n").expect("modify file A");
    let repo_a_root = run_git_output(repo_a, &["rev-parse", "--show-toplevel"]);

    let repo_b_dir = tempdir().expect("create temp repo B");
    let repo_b = repo_b_dir.path();
    run_git(repo_b, &["init"]);
    run_git(repo_b, &["config", "user.name", "gargo-test"]);
    run_git(repo_b, &["config", "user.email", "gargo-test@example.com"]);
    let file_b = repo_b.join("sample.txt");
    fs::write(&file_b, "line-b-1\n").expect("write initial file B");
    run_git(repo_b, &["add", "sample.txt"]);
    run_git(repo_b, &["commit", "-m", "init b"]);
    fs::write(&file_b, "line-b-1\nline-from-b\n").expect("modify file B");
    let repo_b_root = run_git_output(repo_b, &["rev-parse", "--show-toplevel"]);

    let _cwd_guard = WorkingDirGuard::set(repo_a);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo_b, &handle) else {
        return;
    };

    let html = get_text_with_retry(&format!("http://127.0.0.1:{}/diff", port));
    assert!(
        html.contains(&format!(r#"<code id="root-path">{}</code>"#, repo_b_root)),
        "expected diff UI to show explicit project root B: {}",
        html
    );
    assert!(
        !html.contains(&format!(r#"<code id="root-path">{}</code>"#, repo_a_root)),
        "did not expect diff UI to show cwd repo root A: {}",
        html
    );

    let status = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port));
    let unstaged_diff = status["unstaged_diff"]
        .as_str()
        .expect("unstaged_diff should be a string");
    assert!(
        unstaged_diff.contains("+line-from-b"),
        "expected diff to come from project root B: {}",
        unstaged_diff
    );
    assert!(
        !unstaged_diff.contains("+line-from-a"),
        "did not expect diff from cwd repo A: {}",
        unstaged_diff
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_concurrent_instances_use_distinct_ports() {
    let _cwd_lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    let tracked_file = repo.join("sample.txt");
    fs::write(&tracked_file, "line1\n").expect("write initial file");
    run_git(repo, &["add", "sample.txt"]);
    run_git(repo, &["commit", "-m", "init"]);

    let _cwd_guard = WorkingDirGuard::set(repo);

    let handle_a = DiffServerHandle::new().expect("create first diff server handle");
    let handle_b = DiffServerHandle::new().expect("create second diff server handle");

    let Some(port_a) = start_diff_server(repo, &handle_a) else {
        return;
    };
    let Some(port_b) = start_diff_server(repo, &handle_b) else {
        stop_diff_server(&handle_a);
        return;
    };

    assert_ne!(
        port_a, port_b,
        "expected distinct ports for concurrent diff servers"
    );

    let status_a = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port_a));
    assert!(
        status_a.get("unstaged_diff").is_some(),
        "expected /api/status response for first server"
    );
    let status_b = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port_b));
    assert!(
        status_b.get("unstaged_diff").is_some(),
        "expected /api/status response for second server"
    );

    stop_diff_server(&handle_a);
    stop_diff_server(&handle_b);

    match read_event(&handle_a.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected first Stopped event, got: {:?}", event),
    }
    match read_event(&handle_b.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected second Stopped event, got: {:?}", event),
    }
}

fn get_status_and_json_with_retry(url: &str) -> (u16, serde_json::Value) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                let code = resp.status();
                let body = resp.into_json().expect("valid json body");
                return (code, body);
            }
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp
                    .into_json()
                    .unwrap_or(serde_json::Value::Null);
                return (code, body);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to call {}: {}", url, err);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn make_compare_repo(repo: &Path) {
    run_git(repo, &["init", "-b", "main"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    fs::write(repo.join("base.txt"), "line1\n").expect("write base file");
    run_git(repo, &["add", "base.txt"]);
    run_git(repo, &["commit", "-m", "init"]);

    run_git(repo, &["checkout", "-b", "feature"]);
    fs::write(repo.join("base.txt"), "line1\nfeature-line\n").expect("write feature change");
    fs::write(repo.join("feature-only.txt"), "feature-content\n").expect("write feature-only");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "feature work"]);

    run_git(repo, &["checkout", "main"]);
}

#[test]
fn test_diff_server_api_branches_lists_local_branches() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_compare_repo(repo);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    let body = get_json_with_retry(&format!("http://127.0.0.1:{}/api/branches", port));
    let branches: Vec<String> = body["branches"]
        .as_array()
        .expect("branches should be an array")
        .iter()
        .map(|v| v.as_str().expect("branch name should be string").to_string())
        .collect();
    assert!(
        branches.contains(&"main".to_string()),
        "expected main in branches: {:?}",
        branches
    );
    assert!(
        branches.contains(&"feature".to_string()),
        "expected feature in branches: {:?}",
        branches
    );
    assert_eq!(
        body["current"].as_str(),
        Some("main"),
        "expected current branch to be main: {}",
        body
    );
    assert_eq!(
        body["default"].as_str(),
        Some("main"),
        "expected default branch to fall back to main when no origin/HEAD is set: {}",
        body
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_api_compare_returns_branch_diff() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_compare_repo(repo);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    let url = format!(
        "http://127.0.0.1:{}/api/compare?base=main&compare=feature",
        port
    );
    let body = get_json_with_retry(&url);
    assert_eq!(body["base"].as_str(), Some("main"));
    assert_eq!(body["compare"].as_str(), Some("feature"));
    let diff = body["diff"].as_str().expect("diff should be a string");
    assert!(
        diff.contains("+feature-line"),
        "expected compare diff to include feature line: {}",
        diff
    );
    assert!(
        diff.contains("feature-only.txt"),
        "expected compare diff to include new file: {}",
        diff
    );

    let same_url = format!(
        "http://127.0.0.1:{}/api/compare?base=main&compare=main",
        port
    );
    let same_body = get_json_with_retry(&same_url);
    assert_eq!(
        same_body["diff"].as_str(),
        Some(""),
        "expected empty diff when comparing branch to itself: {}",
        same_body
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_api_compare_rejects_flag_injection() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_compare_repo(repo);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    // Branch name starting with `-` (flag injection).
    let inject_url = format!(
        "http://127.0.0.1:{}/api/compare?base=--upload-pack=evil&compare=main",
        port
    );
    let (code, body) = get_status_and_json_with_retry(&inject_url);
    assert_eq!(code, 400, "expected 400 for flag injection: {}", body);
    assert!(
        body["error"].as_str().is_some(),
        "expected error message in body: {}",
        body
    );

    // Disallowed character (semicolon).
    let bad_char_url = format!(
        "http://127.0.0.1:{}/api/compare?base=main%3Brm&compare=main",
        port
    );
    let (code, _body) = get_status_and_json_with_retry(&bad_char_url);
    assert_eq!(code, 400, "expected 400 for disallowed character");

    // Missing query parameter.
    let missing_url = format!("http://127.0.0.1:{}/api/compare?base=main", port);
    let (code, _body) = get_status_and_json_with_retry(&missing_url);
    assert_eq!(code, 400, "expected 400 when compare param is missing");

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_compare_html_page() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_compare_repo(repo);
    let repo_root = run_git_output(repo, &["rev-parse", "--show-toplevel"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server(repo, &handle) else {
        return;
    };

    let html = get_text_with_retry(&format!("http://127.0.0.1:{}/compare", port));
    assert!(
        html.contains("Compare branches"),
        "expected /compare HTML to include context label"
    );
    assert!(
        html.contains("id=\"base-select\"") && html.contains("id=\"compare-select\""),
        "expected /compare HTML to include base/compare selects"
    );
    assert!(
        html.contains("id=\"swap-btn\""),
        "expected /compare HTML to include swap button"
    );
    assert!(
        html.contains("/api/branches") && html.contains("/api/compare"),
        "expected /compare HTML to reference compare endpoints"
    );
    assert!(
        html.contains(&format!(r#"<code id="root-path">{}</code>"#, repo_root)),
        "expected /compare HTML to embed project root"
    );
    assert!(
        html.contains("data.default") && html.contains("defaultBranch"),
        "expected /compare HTML to apply default-branch fallback for base select"
    );
    assert!(
        html.contains("className = \"diff-viewed-label\"")
            && html.contains("textContent = \"Viewed\"")
            && html.contains("loadIdSet(localStorage, VIEWED_FILES_STORAGE_KEY)")
            && html.contains("persistIdSet(localStorage, VIEWED_FILES_STORAGE_KEY"),
        "expected /compare HTML to wire per-file Viewed checkbox backed by local storage"
    );
    assert!(
        html.contains("header.insertBefore(toggleButton, header.firstChild)"),
        "expected /compare HTML to place the toggle chevron on the left of the file header"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

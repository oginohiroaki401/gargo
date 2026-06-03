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
                let body = resp.into_json().unwrap_or(serde_json::Value::Null);
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
    start_diff_server_with_data_dir(project_root, None, handle)
}

/// Start the diff server, optionally pinning the viewed-state database to an
/// explicit data dir so tests stay isolated from `~/.local/share/gargo`.
fn start_diff_server_with_data_dir(
    project_root: &Path,
    data_dir: Option<&Path>,
    handle: &DiffServerHandle,
) -> Option<u16> {
    handle
        .command_tx
        .send(DiffServerCommand::Start {
            project_root: project_root.to_path_buf(),
            data_dir: data_dir.map(Path::to_path_buf),
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

fn paths_of(arr: &serde_json::Value) -> Vec<String> {
    arr.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
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

    let unstaged = paths_of(&body["unstaged"]);
    assert_eq!(unstaged, vec!["sample.txt".to_string()]);
    assert!(
        body["staged"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false)
    );
    assert!(
        body["untracked"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false)
    );

    // /api/status/file returns rendered HTML for one file
    let file_url = format!(
        "http://127.0.0.1:{}/api/status/file?section=unstaged&path=sample.txt",
        port
    );
    let file_body = get_json_with_retry(&file_url);
    let html_body = file_body["html"].as_str().expect("html should be string");
    assert!(
        html_body.contains("gr-diff-body")
            && html_body.contains("gr-line-add")
            && html_body.contains(">line2<"),
        "expected per-file HTML to contain added line: {}",
        html_body
    );

    let html_url = format!("http://127.0.0.1:{}/diff", port);
    let html = get_text_with_retry(&html_url);
    assert!(
        html.contains("id=\"show-untracked\""),
        "expected diff UI to include show-untracked checkbox"
    );
    assert!(
        html.contains("id=\"go-top-btn\""),
        "expected diff UI to include sticky go-top button"
    );
    assert!(
        html.contains(r#"class="app-rail-link app-rail-link-active" href="/status""#)
            && html.contains(r#"data-tab="status">Status</a>"#),
        "expected diff UI status tab to be active"
    );
    assert!(
        html.contains(&format!(
            r#"<code id="root-path" hidden>{}</code>"#,
            repo_root
        )),
        "expected diff UI body to include hidden absolute root path"
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
        "expected diff UI to persist expanded file state in session storage"
    );
    assert!(
        html.contains("fetch(\"/api/status/viewed\"")
            && html.contains("!!meta.viewed")
            && !html.contains("VIEWED_FILES_STORAGE_KEY"),
        "expected diff UI to persist viewed file state on the server, not local storage"
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
        html.contains("class=\"layout\"")
            && html.contains("class=\"sidebar\"")
            && html.contains("class=\"content\""),
        "expected diff UI to use sidebar + content layout"
    );
    assert!(
        html.contains("flex-wrap: nowrap") && html.contains("text-overflow: ellipsis"),
        "expected diff UI file header to use single-line ellipsis layout"
    );
    // Shared chrome (incl. the sticky sidebar rule) is now served as an
    // external cacheable stylesheet rather than inlined on every page, so the
    // page links it and the rule lives in the asset.
    assert!(
        html.contains(r#"<link rel="stylesheet" href="/assets/server-shared.css"#),
        "expected diff UI to link the shared stylesheet"
    );
    let shared_css = get_text_with_retry(&format!(
        "http://127.0.0.1:{}/assets/server-shared.css",
        port
    ));
    assert!(
        shared_css.contains("position: sticky"),
        "expected shared stylesheet to make the sidebar sticky"
    );
    assert!(
        !html.contains("diff2html"),
        "expected diff UI to no longer depend on diff2html: {}",
        &html[..200.min(html.len())]
    );
    assert!(
        html.contains("/api/status/file"),
        "expected diff UI to reference the lazy per-file endpoint"
    );
    assert!(
        html.contains(".gr-diff-body") && html.contains(".gr-line-add"),
        "expected diff UI to embed Rust-rendered diff styles"
    );
    assert!(
        html.contains("gargo.diff.collapsed.v3:")
            && html.contains("gargo.diff.expanded.v1:")
            && html.contains("HUGE_DIFF_LINES"),
        "expected diff UI to collapse huge diffs by default and track explicit expansion"
    );
    assert!(
        html.contains("IntersectionObserver") && html.contains("rootMargin"),
        "expected diff UI to use IntersectionObserver for lazy on-scroll fetch"
    );
    assert!(
        html.contains("SIDEBAR_COLLAPSED_KEY")
            && html.contains("loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY)")
            && html.contains("persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY"),
        "expected diff UI to persist sidebar tree collapse state"
    );
    assert!(
        html.contains("ul.className = \"file-tree\"")
            && html.contains("\"tree-dir\"")
            && html.contains("\"tree-file\"")
            && html.contains("tree-dir-toggle"),
        "expected sidebar to render a tree view"
    );
    assert!(
        html.contains("collapseSingleChainDirs") && html.contains("displayName"),
        "expected sidebar tree to collapse single-child directory chains"
    );

    handle
        .command_tx
        .send(DiffServerCommand::Start {
            project_root: repo.to_path_buf(),
            data_dir: None,
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
fn test_diff_server_status_sections_and_per_file_lazy_endpoint() {
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

    // Clean repo: all sections empty
    let clean = get_json_with_retry(&status_url);
    assert!(clean["unstaged"].as_array().unwrap().is_empty());
    assert!(clean["staged"].as_array().unwrap().is_empty());
    assert!(clean["untracked"].as_array().unwrap().is_empty());

    // Unstaged-only scenario
    fs::write(&tracked_file, "line1\nline2\n").expect("write unstaged change");
    let unstaged_only = get_json_with_retry(&status_url);
    let unstaged = unstaged_only["unstaged"].as_array().unwrap();
    assert_eq!(unstaged.len(), 1);
    assert_eq!(unstaged[0]["path"].as_str(), Some("sample.txt"));
    assert_eq!(unstaged[0]["status"].as_str(), Some("modified"));
    assert_eq!(unstaged[0]["additions"].as_u64(), Some(1));
    assert_eq!(unstaged[0]["deletions"].as_u64(), Some(0));
    assert!(unstaged_only["staged"].as_array().unwrap().is_empty());
    assert!(unstaged_only["untracked"].as_array().unwrap().is_empty());

    // Per-file fetch returns rendered HTML
    let file_url = format!(
        "http://127.0.0.1:{}/api/status/file?section=unstaged&path=sample.txt",
        port
    );
    let file_body = get_json_with_retry(&file_url);
    assert_eq!(file_body["status"].as_str(), Some("modified"));
    let html = file_body["html"].as_str().expect("html should be string");
    assert!(
        html.contains("gr-line-add") && html.contains(">line2<"),
        "expected per-file HTML to include added line: {}",
        html
    );

    // Staged scenario
    run_git(repo, &["add", "sample.txt"]);
    let staged_only = get_json_with_retry(&status_url);
    assert!(staged_only["unstaged"].as_array().unwrap().is_empty());
    let staged = staged_only["staged"].as_array().unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(staged[0]["path"].as_str(), Some("sample.txt"));
    assert_eq!(staged[0]["additions"].as_u64(), Some(1));

    let staged_file = get_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?section=staged&path=sample.txt",
        port
    ));
    let staged_html = staged_file["html"].as_str().unwrap();
    assert!(staged_html.contains("gr-line-add") && staged_html.contains(">line2<"));

    // Untracked addition
    let untracked_file = repo.join("new-untracked.txt");
    fs::write(&untracked_file, "new file\n").expect("write untracked file");
    let with_untracked = get_json_with_retry(&status_url);
    let untracked = with_untracked["untracked"].as_array().unwrap();
    let untracked_entry = untracked
        .iter()
        .find(|f| f["path"].as_str() == Some("new-untracked.txt"));
    assert!(
        untracked_entry.is_some(),
        "expected untracked file in listing: {}",
        with_untracked
    );
    // The listing reports the untracked file's line count so the client can
    // collapse huge new files by default.
    assert_eq!(
        untracked_entry.unwrap()["additions"].as_u64(),
        Some(1),
        "expected untracked file to report its line count: {}",
        with_untracked
    );

    // A huge untracked file reports a line count past the collapse threshold.
    let huge_untracked = repo.join("huge-untracked.txt");
    fs::write(&huge_untracked, "line\n".repeat(5000)).expect("write huge untracked file");
    let with_huge = get_json_with_retry(&status_url);
    let huge_entry = with_huge["untracked"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"].as_str() == Some("huge-untracked.txt"))
        .expect("huge untracked file in listing");
    assert_eq!(
        huge_entry["additions"].as_u64(),
        Some(5000),
        "expected huge untracked file to report its full line count: {}",
        with_huge
    );
    fs::remove_file(&huge_untracked).expect("remove huge untracked file");

    let untracked_file_body = get_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?section=untracked&path=new-untracked.txt",
        port
    ));
    let utext = untracked_file_body["html"].as_str().unwrap();
    assert!(
        utext.contains("gr-line-add") && utext.contains(">new file<"),
        "expected untracked file html to include added line: {}",
        utext
    );

    // show_untracked=false hides untracked listing
    let hidden = get_json_with_retry(&format!("{}?show_untracked=false", status_url));
    assert!(hidden["untracked"].as_array().unwrap().is_empty());

    // Path validation: traversal, flag injection
    let bad_paths = ["../escape", "-rf", "/etc/passwd", "foo/../bar", "bad\nname"];
    for bad in bad_paths {
        let encoded: String = bad
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '/' | '-' | '_' | '.' => c.to_string(),
                _ => format!("%{:02X}", c as u32),
            })
            .collect();
        let code = get_status_code_with_retry(&format!(
            "http://127.0.0.1:{}/api/status/file?section=unstaged&path={}",
            port, encoded
        ));
        assert_eq!(code, 400, "expected 400 for bad path {:?}", bad);
    }

    // Missing path or section
    let missing_section = get_status_code_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?path=sample.txt",
        port
    ));
    assert_eq!(missing_section, 400);
    let missing_path = get_status_code_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?section=unstaged",
        port
    ));
    assert_eq!(missing_path, 400);

    let bad_section = get_status_code_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?section=bogus&path=sample.txt",
        port
    ));
    assert_eq!(bad_section, 400);

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
        html.contains(&format!(
            r#"<code id="root-path" hidden>{}</code>"#,
            repo_b_root
        )),
        "expected diff UI to show explicit project root B: {}",
        html
    );
    assert!(
        !html.contains(&format!(
            r#"<code id="root-path" hidden>{}</code>"#,
            repo_a_root
        )),
        "did not expect diff UI to show cwd repo root A: {}",
        html
    );

    // The metadata listing now lives at /api/status with a `files` shape;
    // verify the file with project-B's content shows up.
    let status = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port));
    let unstaged = paths_of(&status["unstaged"]);
    assert_eq!(unstaged, vec!["sample.txt".to_string()]);

    let file_body = get_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/status/file?section=unstaged&path=sample.txt",
        port
    ));
    let html_body = file_body["html"].as_str().expect("html should be string");
    assert!(
        html_body.contains(">line-from-b<"),
        "expected diff to come from project root B: {}",
        html_body
    );
    assert!(
        !html_body.contains(">line-from-a<"),
        "did not expect diff from cwd repo A: {}",
        html_body
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
        status_a.get("unstaged").is_some(),
        "expected /api/status response for first server"
    );
    let status_b = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port_b));
    assert!(
        status_b.get("unstaged").is_some(),
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
        .map(|v| {
            v.as_str()
                .expect("branch name should be string")
                .to_string()
        })
        .collect();
    assert!(branches.contains(&"main".to_string()));
    assert!(branches.contains(&"feature".to_string()));
    assert_eq!(body["current"].as_str(), Some("main"));
    assert_eq!(body["default"].as_str(), Some("main"));

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_api_compare_returns_file_metadata() {
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
    let files = body["files"].as_array().expect("files should be array");
    let paths: Vec<&str> = files.iter().filter_map(|v| v["path"].as_str()).collect();
    assert!(paths.contains(&"base.txt"));
    assert!(paths.contains(&"feature-only.txt"));
    // No big diff field on the listing
    assert!(body.get("diff").is_none());

    // Per-file body returns the rendered HTML
    let file_url = format!(
        "http://127.0.0.1:{}/api/compare/file?base=main&compare=feature&path=base.txt",
        port
    );
    let file_body = get_json_with_retry(&file_url);
    let html = file_body["html"].as_str().expect("html should be string");
    assert!(
        html.contains("gr-line-add") && html.contains(">feature-line<"),
        "expected per-file compare HTML to contain added line: {}",
        html
    );

    let same_url = format!(
        "http://127.0.0.1:{}/api/compare?base=main&compare=main",
        port
    );
    let same_body = get_json_with_retry(&same_url);
    assert!(
        same_body["files"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
        "expected empty files list when comparing branch to itself: {}",
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

    // Branch flag injection
    let (code, body) = get_status_and_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/compare?base=--upload-pack=evil&compare=main",
        port
    ));
    assert_eq!(code, 400, "expected 400 for flag injection: {}", body);
    assert!(body["error"].as_str().is_some());

    // Bad char (semicolon URL-encoded)
    let (code, _) = get_status_and_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/compare?base=main%3Brm&compare=main",
        port
    ));
    assert_eq!(code, 400, "expected 400 for disallowed character");

    // Missing query parameter
    let (code, _) =
        get_status_and_json_with_retry(&format!("http://127.0.0.1:{}/api/compare?base=main", port));
    assert_eq!(code, 400, "expected 400 when compare param is missing");

    // Path traversal on compare/file endpoint
    let (code, _) = get_status_and_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/compare/file?base=main&compare=feature&path=..%2Fescape",
        port
    ));
    assert_eq!(code, 400, "expected 400 for path traversal in compare/file");

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
        html.contains(r#"class="app-rail-link app-rail-link-active" href="/branches""#)
            && html.contains(r#"data-tab="branches">Branches</a>"#),
        "expected compare UI branches tab to be active"
    );
    assert!(html.contains("id=\"base-select\"") && html.contains("id=\"compare-select\""));
    assert!(html.contains("id=\"swap-btn\""));
    assert!(html.contains("/api/branches") && html.contains("/api/compare"));
    assert!(html.contains("/api/compare/file"));
    assert!(html.contains(&format!(
        r#"<code id="root-path" hidden>{}</code>"#,
        repo_root
    )));
    assert!(
        html.contains("data.default") && html.contains("defaultBranch"),
        "expected /compare HTML to apply default-branch fallback for base select"
    );
    assert!(
        html.contains("className = \"diff-viewed-label\"")
            && html.contains("textContent = \"Viewed\"")
            && html.contains("fetch(\"/api/compare/viewed\"")
            && html.contains("!!meta.viewed")
            && !html.contains("VIEWED_FILES_STORAGE_KEY"),
        "expected /compare HTML to wire per-file Viewed checkbox backed by the server"
    );
    assert!(html.contains("header.insertBefore(toggleButton, header.firstChild)"));
    assert!(
        html.contains("class=\"layout\"")
            && html.contains("class=\"sidebar\"")
            && html.contains("class=\"content\"")
    );
    assert!(html.contains("flex-wrap: nowrap") && html.contains("text-overflow: ellipsis"));
    assert!(html.contains(r#"<link rel="stylesheet" href="/assets/server-shared.css"#));
    let shared_css = get_text_with_retry(&format!(
        "http://127.0.0.1:{}/assets/server-shared.css",
        port
    ));
    assert!(shared_css.contains("position: sticky"));
    assert!(
        !html.contains("diff2html"),
        "expected /compare HTML to no longer depend on diff2html"
    );
    assert!(
        html.contains(".gr-diff-body") && html.contains(".gr-line-add"),
        "expected /compare HTML to embed Rust-rendered diff styles"
    );
    assert!(
        html.contains("gargo.compare.collapsed.v3:")
            && html.contains("gargo.compare.expanded.v1:")
            && html.contains("HUGE_DIFF_LINES"),
        "expected /compare HTML to collapse huge diffs by default and track explicit expansion"
    );
    assert!(
        html.contains("IntersectionObserver") && html.contains("rootMargin"),
        "expected /compare HTML to use IntersectionObserver for lazy on-scroll fetch"
    );
    assert!(
        html.contains("SIDEBAR_COLLAPSED_KEY")
            && html.contains("loadIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY)")
            && html.contains("persistIdSet(sessionStorage, SIDEBAR_COLLAPSED_KEY"),
        "expected /compare HTML to persist sidebar tree collapse state"
    );
    assert!(
        html.contains("ul.className = \"file-tree\"")
            && html.contains("\"tree-dir\"")
            && html.contains("\"tree-file\"")
            && html.contains("tree-dir-toggle"),
        "expected /compare HTML sidebar to render a tree view"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

// --- Viewed-state persistence ------------------------------------------------

fn post_json(url: &str, payload: serde_json::Value) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::post(url).send_json(payload.clone()) {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                return resp.into_json().expect("valid json body");
            }
            Err(ureq::Error::Status(code, _)) => {
                panic!("POST {} returned status {}", url, code);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!("failed to POST {}: {}", url, err);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Find the `viewed` flag for `path` inside a status section or compare files
/// array. Returns `None` when the file is not listed at all.
fn viewed_flag(arr: &serde_json::Value, path: &str) -> Option<bool> {
    arr.as_array()?
        .iter()
        .find(|v| v.get("path").and_then(|p| p.as_str()) == Some(path))
        .and_then(|v| v.get("viewed").and_then(serde_json::Value::as_bool))
}

/// Init a repo with one committed file that then has an unstaged modification.
fn make_status_repo(repo: &Path) {
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    fs::write(repo.join("sample.txt"), "line1\n").expect("write initial file");
    run_git(repo, &["add", "sample.txt"]);
    run_git(repo, &["commit", "-m", "init"]);
    fs::write(repo.join("sample.txt"), "line1\nline2\n").expect("modify tracked file");
}

#[test]
fn test_diff_server_status_viewed_persists_and_invalidates() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_status_repo(repo);
    let data_dir = tempdir().expect("create temp data dir");

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle) else {
        return;
    };

    let status_url = format!("http://127.0.0.1:{}/api/status", port);
    let viewed_url = format!("http://127.0.0.1:{}/api/status/viewed", port);

    // Initially not viewed.
    let body = get_json_with_retry(&status_url);
    assert_eq!(viewed_flag(&body["unstaged"], "sample.txt"), Some(false));

    // Marking it viewed is reflected in the status listing.
    post_json(
        &viewed_url,
        serde_json::json!({ "section": "unstaged", "path": "sample.txt", "viewed": true }),
    );
    let body = get_json_with_retry(&status_url);
    assert_eq!(viewed_flag(&body["unstaged"], "sample.txt"), Some(true));

    // Changing the file content invalidates the viewed state.
    fs::write(repo.join("sample.txt"), "line1\nline2\nline3\n").expect("change file");
    let body = get_json_with_retry(&status_url);
    assert_eq!(
        viewed_flag(&body["unstaged"], "sample.txt"),
        Some(false),
        "viewed must reset once the diff content changes"
    );

    // Re-view against the new content, then explicitly un-view.
    post_json(
        &viewed_url,
        serde_json::json!({ "section": "unstaged", "path": "sample.txt", "viewed": true }),
    );
    assert_eq!(
        viewed_flag(&get_json_with_retry(&status_url)["unstaged"], "sample.txt"),
        Some(true)
    );
    post_json(
        &viewed_url,
        serde_json::json!({ "section": "unstaged", "path": "sample.txt", "viewed": false }),
    );
    assert_eq!(
        viewed_flag(&get_json_with_retry(&status_url)["unstaged"], "sample.txt"),
        Some(false)
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_status_viewed_carries_across_stage_unstage() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_status_repo(repo);
    let data_dir = tempdir().expect("create temp data dir");

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle) else {
        return;
    };

    let status_url = format!("http://127.0.0.1:{}/api/status", port);
    let viewed_url = format!("http://127.0.0.1:{}/api/status/viewed", port);
    let stage_url = format!("http://127.0.0.1:{}/api/status/stage", port);
    let unstage_url = format!("http://127.0.0.1:{}/api/status/unstage", port);

    // Mark the unstaged modification viewed.
    post_json(
        &viewed_url,
        serde_json::json!({ "section": "unstaged", "path": "sample.txt", "viewed": true }),
    );
    assert_eq!(
        viewed_flag(&get_json_with_retry(&status_url)["unstaged"], "sample.txt"),
        Some(true)
    );

    // Staging it moves the file to the staged section; the checkbox follows.
    post_json(&stage_url, serde_json::json!({ "path": "sample.txt" }));
    let body = get_json_with_retry(&status_url);
    assert!(body["unstaged"].as_array().unwrap().is_empty());
    assert_eq!(
        viewed_flag(&body["staged"], "sample.txt"),
        Some(true),
        "viewed state must be kept when a file is staged"
    );

    // Unstaging it moves the file back; the checkbox follows again.
    post_json(&unstage_url, serde_json::json!({ "path": "sample.txt" }));
    let body = get_json_with_retry(&status_url);
    assert!(body["staged"].as_array().unwrap().is_empty());
    assert_eq!(
        viewed_flag(&body["unstaged"], "sample.txt"),
        Some(true),
        "viewed state must be kept when a file is unstaged"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_untracked_viewed_invalidates_on_change() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
    run_git(repo, &["add", "seed.txt"]);
    run_git(repo, &["commit", "-m", "init"]);
    fs::write(repo.join("new.txt"), "fresh\n").expect("write untracked file");
    let data_dir = tempdir().expect("create temp data dir");

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle) else {
        return;
    };
    let status_url = format!("http://127.0.0.1:{}/api/status", port);

    post_json(
        &format!("http://127.0.0.1:{}/api/status/viewed", port),
        serde_json::json!({ "section": "untracked", "path": "new.txt", "viewed": true }),
    );
    assert_eq!(
        viewed_flag(&get_json_with_retry(&status_url)["untracked"], "new.txt"),
        Some(true)
    );

    fs::write(repo.join("new.txt"), "fresh\nmore\n").expect("grow untracked file");
    assert_eq!(
        viewed_flag(&get_json_with_retry(&status_url)["untracked"], "new.txt"),
        Some(false),
        "an untracked file's viewed state must reset when its content changes"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_viewed_persists_across_restart() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_status_repo(repo);
    let data_dir = tempdir().expect("create temp data dir");
    let _cwd_guard = WorkingDirGuard::set(repo);

    // First session: mark the file viewed, then shut the server down.
    {
        let handle = DiffServerHandle::new().expect("create diff server handle");
        let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle)
        else {
            return;
        };
        post_json(
            &format!("http://127.0.0.1:{}/api/status/viewed", port),
            serde_json::json!({ "section": "unstaged", "path": "sample.txt", "viewed": true }),
        );
        stop_diff_server(&handle);
        match read_event(&handle.event_rx) {
            DiffServerEvent::Stopped => {}
            event => panic!("expected Stopped event, got: {:?}", event),
        }
    }

    // Second session on the same data dir: the viewed state is still on disk.
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle) else {
        return;
    };
    let body = get_json_with_retry(&format!("http://127.0.0.1:{}/api/status", port));
    assert_eq!(
        viewed_flag(&body["unstaged"], "sample.txt"),
        Some(true),
        "viewed state must survive a server restart"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_diff_server_compare_viewed_scoped_to_branch_pair() {
    let _cwd_lock = cwd_test_lock();
    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();
    make_compare_repo(repo);
    // A second feature branch so `base.txt` is listed under a different pair.
    run_git(repo, &["checkout", "-b", "feature2"]);
    fs::write(repo.join("base.txt"), "line1\nfeature2-line\n").expect("write feature2 change");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "feature2 work"]);
    run_git(repo, &["checkout", "main"]);
    let data_dir = tempdir().expect("create temp data dir");

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = DiffServerHandle::new().expect("create diff server handle");
    let Some(port) = start_diff_server_with_data_dir(repo, Some(data_dir.path()), &handle) else {
        return;
    };

    post_json(
        &format!("http://127.0.0.1:{}/api/compare/viewed", port),
        serde_json::json!({
            "base": "main", "compare": "feature", "path": "base.txt", "viewed": true,
        }),
    );

    // Viewed for the exact base/compare pair the record was set on.
    let body = get_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/compare?base=main&compare=feature",
        port
    ));
    assert_eq!(viewed_flag(&body["files"], "base.txt"), Some(true));

    // A different compare branch does not inherit the viewed state.
    let other = get_json_with_retry(&format!(
        "http://127.0.0.1:{}/api/compare?base=main&compare=feature2",
        port
    ));
    assert_eq!(
        viewed_flag(&other["files"], "base.txt"),
        Some(false),
        "viewed state must be scoped to the base/compare branch pair"
    );

    stop_diff_server(&handle);
    match read_event(&handle.event_rx) {
        DiffServerEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }
}

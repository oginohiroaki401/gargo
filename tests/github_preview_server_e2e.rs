use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use gargo::command::github_preview_server::{
    GithubPreviewCommand, GithubPreviewEvent, GithubPreviewHandle,
};
use gargo::config::Config;
use gargo::core::editor::Editor;
use gargo::plugin::github_preview::GithubPreviewPlugin;
use gargo::plugin::types::{Plugin, PluginContext, PluginOutput};
use tempfile::tempdir;

// Global mutex to prevent tests from interfering with each other's working directory
static WORKING_DIR_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct WorkingDirGuard {
    original: std::path::PathBuf,
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

/// owner/repo/branch the preview server derives for `repo`, mirroring
/// `resolve_repo_url_context`: owner/repo from the GitHub remote when present,
/// else `local`/folder name; branch from `git rev-parse`.
fn repo_url_parts(repo: &Path) -> (String, String, String) {
    let remote = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(repo)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let (owner, name) = match remote {
        Some(r) if r.contains("github.com") => {
            let path = r
                .rsplit("github.com")
                .next()
                .unwrap_or("")
                .trim_start_matches([':', '/'])
                .trim_end_matches(".git");
            let mut parts = path.split('/');
            (
                parts.next().unwrap_or("local").to_string(),
                parts.next().unwrap_or("repo").to_string(),
            )
        }
        _ => (
            "local".to_string(),
            repo.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repo".to_string()),
        ),
    };
    let branch = run_git_output(repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    (owner, name, branch)
}

/// github.com-style blob URL the rewritten preview server serves.
fn github_blob_url(port: u16, repo: &Path, rel: &str) -> String {
    let (owner, name, branch) = repo_url_parts(repo);
    format!("http://127.0.0.1:{port}/{owner}/{name}/blob/{branch}/{rel}")
}

/// github.com-style tree URL the rewritten preview server serves.
fn github_tree_url(port: u16, repo: &Path, rel: &str) -> String {
    let (owner, name, branch) = repo_url_parts(repo);
    format!("http://127.0.0.1:{port}/{owner}/{name}/tree/{branch}/{rel}")
}

/// `/{owner}/{repo}` repo-home path.
fn github_repo_path(repo: &Path) -> String {
    let (owner, name, _) = repo_url_parts(repo);
    format!("/{owner}/{name}")
}

fn read_event(rx: &std::sync::mpsc::Receiver<GithubPreviewEvent>) -> GithubPreviewEvent {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("timed out waiting for github preview server event")
}

fn cwd_test_lock() -> std::sync::MutexGuard<'static, ()> {
    match WORKING_DIR_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(lock) => lock,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn start_preview_server(handle: &GithubPreviewHandle, repo_root: &Path) -> Option<u16> {
    handle
        .command_tx
        .send(GithubPreviewCommand::Start {
            repo_root: repo_root.to_path_buf(),
        })
        .expect("send start command");

    match read_event(&handle.event_rx) {
        GithubPreviewEvent::Started { port } => Some(port),
        GithubPreviewEvent::Error(msg)
            if msg.starts_with("Failed to bind GitHub preview server") =>
        {
            eprintln!("skip github preview test: {}", msg);
            None
        }
        event => {
            panic!("expected Started event, got: {:?}", event);
        }
    }
}

fn stop_preview_server(handle: &GithubPreviewHandle) {
    let _ = handle.command_tx.send(GithubPreviewCommand::Stop);
}

fn wait_for_plugin_start_url(
    plugin: &mut GithubPreviewPlugin,
    ctx: &PluginContext,
) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() >= deadline {
            panic!("timed out waiting for plugin start output");
        }
        let outputs = plugin.poll(ctx);
        if let Some(url) = extract_started_url(&outputs) {
            return Some(url);
        }
        if outputs.iter().any(|output| {
            matches!(
                output,
                PluginOutput::Message(message)
                    if message.starts_with("Server error: Failed to bind GitHub preview server on localhost:")
            )
        }) {
            if let Some(message) = outputs.iter().find_map(|output| {
                if let PluginOutput::Message(message) = output {
                    Some(message.clone())
                } else {
                    None
                }
            }) {
                eprintln!("skip github preview plugin test: {}", message);
            }
            return None;
        }
        thread::sleep(Duration::from_millis(30));
    }
}

fn expect_stopped_event(rx: &std::sync::mpsc::Receiver<GithubPreviewEvent>) {
    loop {
        match read_event(rx) {
            GithubPreviewEvent::Stopped => break,
            GithubPreviewEvent::Detached { .. } => continue,
            event => panic!("expected Stopped event, got: {:?}", event),
        }
    }
}

fn get_html_with_retry(url: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                return resp.into_string().expect("valid html body");
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

fn read_events_since(base_url: &str, since: u64) -> Option<serde_json::Value> {
    let url = format!("{base_url}/events?since={since}");
    let value = get_json_with_retry(&url);
    value.get("event").cloned().filter(|v| !v.is_null())
}

fn wait_for_event_kind(
    base_url: &str,
    mut since: u64,
    kind: &str,
    path: Option<&str>,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() >= deadline {
            panic!("timed out waiting for event kind={kind}");
        }
        let Some(event) = read_events_since(base_url, since) else {
            thread::sleep(Duration::from_millis(30));
            continue;
        };
        since = event["version"].as_u64().unwrap_or(since);
        if event["kind"].as_str() != Some(kind) {
            continue;
        }
        if let Some(expected_path) = path
            && event["path"].as_str() != Some(expected_path)
        {
            continue;
        }
        return event;
    }
}

fn extract_started_url(outputs: &[PluginOutput]) -> Option<String> {
    for output in outputs {
        match output {
            PluginOutput::Message(message) => {
                if let Some(url) = message.strip_prefix("GitHub preview: ") {
                    return Some(url.to_string());
                }
            }
            PluginOutput::OpenUrl(url) => return Some(url.clone()),
            _ => {}
        }
    }
    None
}

#[test]
fn test_github_preview_server_start_stop_and_mermaid_rendering() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    // Canonicalize because macOS tempdir lives under /var → /private/var; the
    // preview server echoes whatever path is passed in, and this test later
    // compares against `git rev-parse --show-toplevel`, which canonicalizes.
    let canonical_repo = std::fs::canonicalize(repo_dir.path()).expect("canonicalize repo");
    let repo = canonical_repo.as_path();

    // Initialize git repo
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    run_git(
        repo,
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    // Create a markdown file with mermaid diagram
    let readme = repo.join("README.md");
    fs::write(
        &readme,
        r#"# Test Document

## Regular content

This is a test document.

## Mermaid Diagram

```mermaid
graph TD
    A[Start] --> B[End]
```

## More content

Regular text after diagram.
"#,
    )
    .expect("write README.md");

    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "add readme"]);
    let repo_root = run_git_output(repo, &["rev-parse", "--show-toplevel"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = GithubPreviewHandle::new().expect("create github preview handle");

    // Test: Start server
    let Some(port) = start_preview_server(&handle, repo) else {
        return;
    };

    // Test: Fetch the README and verify Markdown renders without CDN dependencies.
    let readme_url = github_blob_url(port, repo, "README.md");
    let html = get_html_with_retry(&readme_url);

    assert!(
        !html.contains("cdn.jsdelivr.net"),
        "expected preview HTML to avoid CDN dependencies"
    );

    assert!(
        html.contains(r#"<script src="/assets/mermaid.min.js"></script>"#)
            && !html.contains("type=\"module\""),
        "expected preview HTML to use a local bundled mermaid asset"
    );
    let mermaid_js =
        get_html_with_retry(&format!("http://127.0.0.1:{}/assets/mermaid.min.js", port));
    assert!(
        mermaid_js.contains("startOnLoad"),
        "expected local mermaid asset response"
    );
    assert!(
        html.contains("fetch(`/events?since=${lastSeenVersion}`"),
        "expected preview page to include live sync poll client"
    );
    assert!(
        html.contains("gargo_preview_last_seen_event_version")
            && html.contains("window.sessionStorage.setItem"),
        "expected preview page to persist event version to prevent refresh loops"
    );

    // Test: Verify README content is rendered correctly
    let readme_html = html;

    assert!(
        readme_html.contains("Test Document"),
        "expected README content in HTML"
    );

    assert!(
        readme_html.contains(r#"<pre class="mermaid">"#),
        "expected mermaid block in HTML"
    );

    assert!(
        readme_html.contains(r#"class="repo-header""#),
        "expected repository header in preview UI"
    );

    assert!(
        readme_html.contains(&format!(r#"<code>{}</code>"#, repo_root)),
        "expected absolute root path in repository header"
    );

    assert!(
        readme_html.contains(&format!(
            r#"<a class="repo-tab repo-tab-active" href="{}">Code</a>"#,
            github_repo_path(repo)
        )),
        "expected code tab to be active in repository header"
    );

    assert!(
        readme_html.contains(r#"href="https://github.com/acme/repo""#)
            && readme_html.contains(r#"<span class="repo-owner">acme</span>"#),
        "expected owner/repo title to link to GitHub remote"
    );

    assert!(
        !readme_html.contains("Repository browser")
            && !readme_html.contains(r#"<span class="context-key">Showing</span>"#),
        "expected legacy context header to be omitted"
    );

    let root_url = format!("http://127.0.0.1:{}/", port);
    let root_html = get_html_with_retry(&root_url);
    assert!(
        root_html.contains(r#"class="repo-header""#),
        "expected repository header for root directory"
    );
    assert!(
        root_html.contains(r#"href="https://github.com/acme/repo""#),
        "expected root header title to link to GitHub remote"
    );

    // Test: Duplicate start should error
    handle
        .command_tx
        .send(GithubPreviewCommand::Start {
            repo_root: repo.to_path_buf(),
        })
        .expect("send duplicate start");

    match read_event(&handle.event_rx) {
        GithubPreviewEvent::Error(msg) => {
            assert!(msg.contains("already running"), "unexpected error: {}", msg)
        }
        event => panic!("expected already-running error, got: {:?}", event),
    }

    // Test: Stop server
    stop_preview_server(&handle);

    match read_event(&handle.event_rx) {
        GithubPreviewEvent::Stopped => {}
        event => panic!("expected Stopped event, got: {:?}", event),
    }

    // Test: Duplicate stop should error
    stop_preview_server(&handle);

    match read_event(&handle.event_rx) {
        GithubPreviewEvent::Error(msg) => {
            assert!(msg.contains("not running"), "unexpected error: {}", msg)
        }
        event => panic!("expected not-running error, got: {:?}", event),
    }
}

#[test]
fn test_github_preview_server_serves_tree_view() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    // Canonicalize — see comment in test_github_preview_server_start_stop_and_mermaid_rendering.
    let canonical_repo = std::fs::canonicalize(repo_dir.path()).expect("canonicalize repo");
    let repo = canonical_repo.as_path();

    // Initialize git repo
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    run_git(
        repo,
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    // Create multiple files
    fs::write(repo.join("file1.md"), "# File 1").expect("write file1");
    fs::write(repo.join("file2.md"), "# File 2").expect("write file2");
    fs::create_dir_all(repo.join("docs")).expect("create docs dir");
    fs::write(repo.join("docs/guide.md"), "# Guide").expect("write guide");

    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "add files"]);
    let repo_root = run_git_output(repo, &["rev-parse", "--show-toplevel"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = GithubPreviewHandle::new().expect("create github preview handle");

    let Some(port) = start_preview_server(&handle, repo) else {
        return;
    };

    // Test: Root view should list files
    let root_url = format!("http://127.0.0.1:{}/", port);
    let tree_html = get_html_with_retry(&root_url);

    assert!(
        tree_html.contains("file1.md"),
        "expected file1.md in tree view"
    );

    assert!(
        tree_html.contains("file2.md"),
        "expected file2.md in tree view"
    );

    assert!(
        tree_html.contains(&format!(r#"<code>{}</code>"#, repo_root)),
        "expected absolute root path in root tree header"
    );

    let nested_url = github_tree_url(port, repo, "docs");
    let nested_html = get_html_with_retry(&nested_url);
    assert!(
        nested_html.contains(r#"class="repo-header""#),
        "expected repository header for current directory path"
    );
    assert!(
        !nested_html.contains(r#"<span class="context-key">Showing</span>"#),
        "expected legacy displayed nested directory path header to be omitted"
    );

    // Cleanup
    stop_preview_server(&handle);
    expect_stopped_event(&handle.event_rx);
}

#[test]
fn test_github_preview_server_concurrent_instances_use_distinct_ports() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    run_git(
        repo,
        &["remote", "add", "origin", "git@github.com:acme/repo.git"],
    );

    fs::write(repo.join("README.md"), "# Readme").expect("write README");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "add readme"]);

    let _cwd_guard = WorkingDirGuard::set(repo);

    let handle_a = GithubPreviewHandle::new().expect("create first github preview handle");
    let handle_b = GithubPreviewHandle::new().expect("create second github preview handle");

    let Some(port_a) = start_preview_server(&handle_a, repo) else {
        return;
    };
    let Some(port_b) = start_preview_server(&handle_b, repo) else {
        stop_preview_server(&handle_a);
        return;
    };

    assert_ne!(
        port_a, port_b,
        "expected distinct ports for concurrent preview servers"
    );

    let root_a = get_html_with_retry(&format!("http://127.0.0.1:{}/", port_a));
    assert!(
        root_a.contains("README.md"),
        "expected root tree response for first server"
    );
    let root_b = get_html_with_retry(&format!("http://127.0.0.1:{}/", port_b));
    assert!(
        root_b.contains("README.md"),
        "expected root tree response for second server"
    );

    stop_preview_server(&handle_a);
    stop_preview_server(&handle_b);

    match read_event(&handle_a.event_rx) {
        GithubPreviewEvent::Stopped => {}
        event => panic!("expected first Stopped event, got: {:?}", event),
    }
    match read_event(&handle_b.event_rx) {
        GithubPreviewEvent::Stopped => {}
        event => panic!("expected second Stopped event, got: {:?}", event),
    }
}

#[test]
fn test_github_preview_server_detaches_when_browser_leaves_active_path() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    fs::write(repo.join("README.md"), "# Readme").expect("write README");
    fs::write(repo.join("docs.md"), "# Docs").expect("write docs");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "add files"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = GithubPreviewHandle::new().expect("create github preview handle");

    let Some(port) = start_preview_server(&handle, repo) else {
        return;
    };

    handle
        .command_tx
        .send(GithubPreviewCommand::SetActivePath {
            rel_path: Some("README.md".to_string()),
        })
        .expect("set active path");

    let active_url = github_blob_url(port, repo, "README.md");
    let _ = get_html_with_retry(&active_url);
    assert!(
        handle
            .event_rx
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "expected no detach event while browsing active path"
    );

    let off_path_url = github_blob_url(port, repo, "docs.md");
    let _ = get_html_with_retry(&off_path_url);
    match read_event(&handle.event_rx) {
        GithubPreviewEvent::Detached { requested_path } => {
            assert_eq!(requested_path, "docs.md");
        }
        event => panic!("expected Detached event, got: {:?}", event),
    }

    handle
        .command_tx
        .send(GithubPreviewCommand::SetActivePath {
            rel_path: Some("docs.md".to_string()),
        })
        .expect("set second active path");
    let _ = get_html_with_retry(&off_path_url);
    assert!(
        handle
            .event_rx
            .recv_timeout(Duration::from_millis(200))
            .is_err(),
        "expected no detach event after reattaching to docs.md"
    );

    stop_preview_server(&handle);
    expect_stopped_event(&handle.event_rx);
}

#[test]
fn test_github_preview_server_events_endpoint_emits_navigate_refresh_and_detached_events() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    fs::write(repo.join("README.md"), "# Readme").expect("write README");
    fs::write(repo.join("docs.md"), "# Docs").expect("write docs");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "add files"]);

    let _cwd_guard = WorkingDirGuard::set(repo);
    let handle = GithubPreviewHandle::new().expect("create github preview handle");

    let Some(port) = start_preview_server(&handle, repo) else {
        return;
    };

    let base_url = format!("http://127.0.0.1:{}", port);

    handle
        .command_tx
        .send(GithubPreviewCommand::SetActivePath {
            rel_path: Some("README.md".to_string()),
        })
        .expect("set active path");
    let navigate = wait_for_event_kind(&base_url, 0, "navigate", Some("README.md"));
    let expected_readme_url = github_blob_url(port, repo, "README.md");
    assert_eq!(navigate["url"].as_str(), Some(expected_readme_url.as_str()));
    let navigate_version = navigate["version"]
        .as_u64()
        .expect("navigate event version should be u64");

    handle
        .command_tx
        .send(GithubPreviewCommand::RefreshActive)
        .expect("refresh active path");
    let refresh = wait_for_event_kind(&base_url, navigate_version, "refresh", Some("README.md"));
    assert_eq!(refresh["url"].as_str(), Some(expected_readme_url.as_str()));
    let refresh_version = refresh["version"]
        .as_u64()
        .expect("refresh event version should be u64");

    let _ = get_html_with_retry(&github_blob_url(port, repo, "docs.md"));
    let detached = wait_for_event_kind(&base_url, refresh_version, "detached", Some("docs.md"));
    assert_eq!(detached["detached"].as_bool(), Some(true));

    stop_preview_server(&handle);
    expect_stopped_event(&handle.event_rx);
}

#[test]
fn test_github_preview_plugin_external_file_change_emits_refresh_event() {
    let _lock = cwd_test_lock();

    let repo_dir = tempdir().expect("create temp repo");
    let repo = repo_dir.path();

    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);

    let readme_path = repo.join("README.md");
    fs::write(&readme_path, "# Readme\n").expect("write README");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "add readme"]);

    let _cwd_guard = WorkingDirGuard::set(repo);

    let mut config = Config::default();
    config.plugin.github_preview.auto_open_browser = false;
    let editor = Editor::open(&readme_path.to_string_lossy());
    let ctx = PluginContext::new(&editor, repo, &config);
    let mut plugin = GithubPreviewPlugin::new(&config, repo);

    let _ = plugin.on_command("server.start_github_preview", &ctx);

    let Some(started_url) = wait_for_plugin_start_url(&mut plugin, &ctx) else {
        return;
    };

    let parsed = url::Url::parse(&started_url).expect("parse started URL");
    let host = parsed.host_str().expect("preview URL host");
    let port = parsed.port().expect("preview URL port");
    let base_url = format!("{}://{}:{}", parsed.scheme(), host, port);
    let expected_readme_url = github_blob_url(port, repo, "README.md");

    let navigate = wait_for_event_kind(&base_url, 0, "navigate", Some("README.md"));
    let navigate_version = navigate["version"]
        .as_u64()
        .expect("navigate event version should be u64");

    fs::write(&readme_path, "# Readme\nupdated from external process\n")
        .expect("rewrite README from external process");
    thread::sleep(Duration::from_millis(650));
    let _ = plugin.poll(&ctx);

    let refresh = wait_for_event_kind(&base_url, navigate_version, "refresh", Some("README.md"));
    assert_eq!(refresh["url"].as_str(), Some(expected_readme_url.as_str()));

    let _ = plugin.on_command("server.stop_github_preview", &ctx);
    let stop_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() >= stop_deadline {
            panic!("timed out waiting for plugin stop output");
        }
        let outputs = plugin.poll(&ctx);
        if outputs.iter().any(|output| {
            matches!(
                output,
                PluginOutput::Message(msg) if msg == "GitHub preview server stopped"
            )
        }) {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
}

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gargo::command::github_server::{GithubServerCommand, GithubServerEvent, GithubServerHandle};
use gargo::config::Config;
use gargo::core::editor::Editor;
use gargo::plugin::registry::build_plugin_host;
use gargo::plugin::types::{PluginContext, PluginOutput};
use tempfile::tempdir;

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

fn get_text_with_retry(url: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match ureq::get(url).call() {
            Ok(resp) => {
                assert_eq!(resp.status(), 200);
                return resp.into_string().expect("text response");
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
                return resp.into_json().expect("json response");
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

fn start_server(repo: &Path, handle: &GithubServerHandle) -> Option<u16> {
    handle
        .command_tx
        .send(GithubServerCommand::Start {
            repo_root: repo.to_path_buf(),
        })
        .expect("send start");
    match handle.event_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(GithubServerEvent::Started { port, .. }) => Some(port),
        Ok(GithubServerEvent::Error(msg)) if msg.starts_with("Failed to bind Gargo server") => {
            eprintln!("skip github server test: {}", msg);
            None
        }
        other => panic!("expected Started, got {:?}", other),
    }
}

fn setup_repo() -> tempfile::TempDir {
    let dir = tempdir().expect("temp repo");
    let repo = dir.path();
    run_git(repo, &["init"]);
    run_git(repo, &["config", "user.name", "gargo-test"]);
    run_git(repo, &["config", "user.email", "gargo-test@example.com"]);
    run_git(
        repo,
        &["remote", "add", "origin", "git@github.com:aplio/gargo.git"],
    );
    fs::write(repo.join("README.md"), "# Test Repo\n\nhello\n").expect("write readme");
    fs::create_dir(repo.join("src")).expect("mkdir src");
    fs::write(repo.join("src/lib.rs"), "pub fn base() {}\n").expect("write lib");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "initial commit"]);
    run_git(repo, &["checkout", "-b", "feature"]);
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn base() {}\npub fn feature() {}\n",
    )
    .expect("write feature");
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "feature commit"]);
    run_git(repo, &["checkout", "master"]);
    fs::write(
        repo.join("README.md"),
        "# Test Repo\n\nhello changed\n\n```mermaid\ngraph TD\n    A[Start] --> B[End]\n```\n",
    )
    .expect("modify readme");
    fs::write(repo.join("scratch.txt"), "new\n").expect("write untracked");
    dir
}

#[test]
fn unified_github_server_serves_code_diffs_compare_commits_and_events() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let handle = GithubServerHandle::new().expect("server handle");
    let Some(port) = start_server(repo, &handle) else {
        return;
    };
    let base_url = format!("http://127.0.0.1:{port}");

    let root_html = get_text_with_retry(&format!("{base_url}/"));
    assert!(root_html.contains("app-rail-link app-rail-link-active"));
    assert!(root_html.contains(">Code</a>"));
    assert!(root_html.contains(">Status</a>"));
    assert!(root_html.contains(">Branches</a>"));
    assert!(!root_html.contains("Repository browser"));
    assert!(!root_html.contains(r#"<span class="context-key">Showing</span>"#));
    assert!(root_html.contains(r#"href="https://github.com/aplio/gargo""#));
    assert!(root_html.contains(r#"<span class="repo-owner">aplio/</span>"#));
    assert!(root_html.contains("README.md"));

    let blob_html = get_text_with_retry(&format!("{base_url}/aplio/gargo/blob/master/README.md"));
    assert!(blob_html.contains("Test Repo"));
    assert!(blob_html.contains(r#"<pre class="mermaid">"#));
    assert!(blob_html.contains(r#"<script src="/assets/mermaid.min.js"></script>"#));
    assert!(!blob_html.contains("cdn.jsdelivr.net"));
    assert!(!blob_html.contains("language-mermaid"));
    // Markdown blob shows the rendered Preview by default plus a raw/preview toggle.
    assert!(blob_html.contains("md-view-toggle"));
    let mermaid_js = get_text_with_retry(&format!("{base_url}/assets/mermaid.min.js"));
    assert!(mermaid_js.contains("startOnLoad"));

    // `?plain=1` shows the raw markdown source with a line-number gutter.
    let raw_md_html = get_text_with_retry(&format!(
        "{base_url}/aplio/gargo/blob/master/README.md?plain=1"
    ));
    assert!(raw_md_html.contains("md-view-toggle"));
    assert!(raw_md_html.contains("code-table"));
    assert!(raw_md_html.contains(r#"data-line-number="1""#));
    // Raw view shows source, not the rendered mermaid diagram.
    assert!(!raw_md_html.contains(r#"<pre class="mermaid">"#));

    let rust_blob_html =
        get_text_with_retry(&format!("{base_url}/aplio/gargo/blob/master/src/lib.rs"));
    assert!(rust_blob_html.contains("gr-hl-keyword"));
    // Code blobs render with a line-number gutter.
    assert!(rust_blob_html.contains("code-table"));
    assert!(rust_blob_html.contains(r#"data-line-number="1""#));

    let blob_json = get_json_with_retry(&format!("{base_url}/api/blob/README.md"));
    assert_eq!(blob_json["path"], "README.md");
    assert!(
        blob_json["html"]
            .as_str()
            .unwrap_or("")
            .contains("Test Repo")
    );
    assert!(
        blob_json["html"]
            .as_str()
            .unwrap_or("")
            .contains(r#"<pre class="mermaid">"#)
    );

    let rust_blob_json = get_json_with_retry(&format!("{base_url}/api/blob/src/lib.rs"));
    assert!(
        rust_blob_json["html"]
            .as_str()
            .unwrap_or("")
            .contains("gr-hl-keyword")
    );

    let tree_json = get_json_with_retry(&format!("{base_url}/api/tree/src"));
    assert!(
        tree_json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "lib.rs")
    );

    let status = get_json_with_retry(&format!("{base_url}/api/status"));
    assert_eq!(status["unstaged"][0]["path"], "README.md");
    assert_eq!(status["untracked"][0]["path"], "scratch.txt");
    let status_html = get_text_with_retry(&format!("{base_url}/status"));
    assert!(status_html.contains(r#"href="/status">Status</a>"#));
    assert!(status_html.contains("app-rail-link app-rail-link-active"));
    assert!(status_html.contains(r#"href="https://github.com/aplio/gargo""#));
    let file_diff = get_json_with_retry(&format!(
        "{base_url}/api/status/file?section=unstaged&path=README.md"
    ));
    assert!(
        file_diff["html"]
            .as_str()
            .unwrap_or("")
            .contains("hello changed")
    );

    let branches = get_json_with_retry(&format!("{base_url}/api/branches"));
    assert!(
        branches["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b == "feature")
    );
    let branches_html = get_text_with_retry(&format!("{base_url}/branches"));
    assert!(branches_html.contains(r#"href="/branches">Branches</a>"#));
    assert!(branches_html.contains("app-rail-link app-rail-link-active"));
    assert!(branches_html.contains(r#"href="https://github.com/aplio/gargo""#));
    let commits_html = get_text_with_retry(&format!("{base_url}/aplio/gargo/commits/master"));
    assert!(commits_html.contains(r#"href="/status">Status</a>"#));
    assert!(commits_html.contains(r#"href="/branches">Branches</a>"#));
    assert!(!commits_html.contains("Repository commits"));
    assert!(commits_html.contains(r#"href="https://github.com/aplio/gargo""#));
    let compare = get_json_with_retry(&format!(
        "{base_url}/api/compare?base=master&compare=feature"
    ));
    assert!(
        compare["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "src/lib.rs")
    );
    let compare_file = get_json_with_retry(&format!(
        "{base_url}/api/compare/file?base=master&compare=feature&path=src/lib.rs"
    ));
    assert!(
        compare_file["html"]
            .as_str()
            .unwrap_or("")
            .contains("feature")
    );

    let commits = get_json_with_retry(&format!("{base_url}/api/commits"));
    let first_hash = commits["commits"][0]["full_hash"].as_str().unwrap();
    let commit = get_json_with_retry(&format!("{base_url}/api/commit/{first_hash}"));
    assert!(
        commit["message"]
            .as_str()
            .unwrap_or("")
            .contains("initial commit")
    );
    let commit_file = get_json_with_retry(&format!(
        "{base_url}/api/commit/{first_hash}/file?path=README.md"
    ));
    assert!(
        commit_file["html"]
            .as_str()
            .unwrap_or("")
            .contains("Test Repo")
    );
    let commit_html = get_text_with_retry(&format!("{base_url}/aplio/gargo/commit/{first_hash}"));
    assert!(commit_html.contains("Commit"));
    // Commit view collapses huge file diffs by default and defers fetching
    // their bodies so the browser stays light on big commits.
    assert!(
        commit_html.contains("HUGE_DIFF_LINES")
            && commit_html.contains("gr-file-collapsed")
            && commit_html.contains("gr-collapsed-note"),
        "expected commit view to collapse huge diffs by default"
    );

    handle
        .command_tx
        .send(GithubServerCommand::SetActivePath {
            rel_path: Some("README.md".to_string()),
        })
        .expect("set active path");
    let event = get_json_with_retry(&format!("{base_url}/events?since=0"));
    assert_eq!(event["event"]["kind"], "navigate");
    assert_eq!(event["event"]["path"], "README.md");

    let _ = handle.command_tx.send(GithubServerCommand::Stop);
}

#[test]
fn github_server_plugin_commands_replace_old_visible_server_commands() {
    let repo_dir = tempdir().expect("temp repo");
    let config = Config::default();
    let host = build_plugin_host(&config, repo_dir.path()).expect("plugin host");
    let commands: Vec<_> = host
        .command_specs()
        .iter()
        .map(|command| command.id.as_str())
        .collect();
    assert!(commands.contains(&"server.start_github"));
    assert!(commands.contains(&"server.stop_github"));
    assert!(!commands.contains(&"server.start_diff"));
    assert!(!commands.contains(&"server.stop_diff"));
    assert!(!commands.contains(&"server.open_compare"));
    assert!(!commands.contains(&"server.start_github_preview"));
    assert!(!commands.contains(&"server.stop_github_preview"));
    assert_eq!(
        Config::default().plugins.enabled,
        vec!["lsp".to_string(), "github_server".to_string()]
    );
}

#[test]
fn github_server_plugin_start_opens_repository_root_not_active_file() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let active_file = repo.join("README.md");
    let mut config = Config::default();
    config.plugins.enabled = vec!["github_server".to_string()];
    config.plugin.github_server.auto_open_browser = true;
    let editor = Editor::open(&active_file.to_string_lossy());
    let ctx = PluginContext::new(&editor, repo, &config);
    let mut host = build_plugin_host(&config, repo).expect("plugin host");

    let outputs = host.run_command("server.start_github", &ctx);
    assert!(outputs.is_empty());

    let deadline = Instant::now() + Duration::from_secs(3);
    let url = loop {
        if Instant::now() >= deadline {
            panic!("timed out waiting for github server start output");
        }
        let outputs = host.poll(&ctx);
        if let Some(url) = outputs.iter().find_map(|output| match output {
            PluginOutput::OpenUrl(url) => Some(url.clone()),
            PluginOutput::Message(message) => message
                .strip_prefix("Gargo server: ")
                .map(ToString::to_string),
            _ => None,
        }) {
            break url;
        }
        thread::sleep(Duration::from_millis(30));
    };
    assert!(
        url.ends_with("/aplio/gargo"),
        "expected server start to open repository root, got {url}"
    );
    assert!(
        !url.contains("/blob/README.md"),
        "expected server start not to open active file route"
    );
    let _ = host.run_command("server.stop_github", &ctx);
}

#[test]
fn github_server_concurrent_instances_use_distinct_ports() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let first = GithubServerHandle::new().expect("first handle");
    let second = GithubServerHandle::new().expect("second handle");
    let Some(port_a) = start_server(repo, &first) else {
        return;
    };
    let Some(port_b) = start_server(repo, &second) else {
        return;
    };
    assert_ne!(port_a, port_b);
    let _ = first.command_tx.send(GithubServerCommand::Stop);
    let _ = second.command_tx.send(GithubServerCommand::Stop);
}

#[test]
fn old_plugin_ids_normalize_to_single_github_server_plugin() {
    let config: Config = toml::from_str(
        r#"
[plugins]
enabled = ["diff_ui", "github_preview", "github_server"]
"#,
    )
    .unwrap();
    assert_eq!(
        config.plugins.normalized_enabled(),
        vec!["github_server".to_string()]
    );

    let repo_dir = tempdir().expect("temp repo");
    let host = build_plugin_host(&config, repo_dir.path()).expect("plugin host");
    let commands: Vec<_> = host
        .command_specs()
        .iter()
        .map(|command| command.id.as_str())
        .collect();
    assert_eq!(
        commands
            .iter()
            .filter(|command| **command == "server.start_github")
            .count(),
        1
    );
}

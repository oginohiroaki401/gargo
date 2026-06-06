use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use gargo::command::gargo_server::{GargoServerCommand, GargoServerEvent, GargoServerHandle};
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

fn start_server(repo: &Path, handle: &GargoServerHandle) -> Option<u16> {
    handle
        .command_tx
        .send(GargoServerCommand::Start {
            repo_root: repo.to_path_buf(),
        })
        .expect("send start");
    match handle.event_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(GargoServerEvent::Started { port, .. }) => Some(port),
        Ok(GargoServerEvent::Error(msg)) if msg.starts_with("Failed to bind Gargo server") => {
            eprintln!("skip github server test: {}", msg);
            None
        }
        other => panic!("expected Started, got {:?}", other),
    }
}

fn setup_repo() -> tempfile::TempDir {
    let dir = tempdir().expect("temp repo");
    let repo = dir.path();
    // Force the initial branch name so the later `checkout master` works
    // regardless of the host's `init.defaultBranch` config (e.g. `main`).
    run_git(repo, &["init", "-b", "master"]);
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
fn unified_gargo_server_serves_code_diffs_compare_commits_and_events() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let handle = GargoServerHandle::new().expect("server handle");
    let Some(port) = start_server(repo, &handle) else {
        return;
    };
    let base_url = format!("http://127.0.0.1:{port}");

    let root_html = get_text_with_retry(&format!("{base_url}/"));
    assert!(root_html.contains(r#"data-component="explorer">Explorer</button>"#));
    assert!(root_html.contains(r#"data-component="history">History</button>"#));
    assert!(root_html.contains(r#"data-component="compare">Compare</button>"#));
    assert!(root_html.contains(r#"data-component="status">Status</button>"#));
    assert!(root_html.contains("function renderCodeSurface"));
    assert!(root_html.contains("function renderHistory"));
    assert!(root_html.contains("function renderCompare"));
    assert!(root_html.contains("function renderStatus"));
    assert!(root_html.contains("openTreePicker"));
    assert!(root_html.contains("function buildTree"));
    assert!(root_html.contains("function updateTreePreview"));
    assert!(root_html.contains(r#"state.focusLevel === "app" && event.key === "t""#));
    assert!(root_html.contains(r#"["l", "r"].includes(event.key.toLowerCase())"#));
    assert!(root_html.contains("enterEditorInsertMode"));
    assert!(root_html.contains("toggleStatusViewed"));
    assert!(root_html.contains("toggleCompareViewed"));
    assert!(root_html.contains("openSelectedDiffFileInEditor"));
    assert!(root_html.contains("moveHistoryFile"));
    assert!(root_html.contains("scrollExplorer"));
    assert!(root_html.contains(r#"input.readOnly = state.editorMode !== "insert""#));
    assert!(root_html.contains(r#"state.component === "compare" && event.shiftKey"#));
    assert!(!root_html.contains("id=\"sidebar\""));

    let files = get_json_with_retry(&format!("{base_url}/api/files"));
    let readme = files["entries"]
        .as_array()
        .expect("file entries")
        .iter()
        .find(|entry| entry["path"] == "README.md")
        .expect("README metadata");
    assert_eq!(readme["changed"], true);
    assert!(readme["mtime"].as_u64().unwrap_or(0) > 0);

    // The editor header and "open" menu read repo identity from /api/repo-info.
    let repo_info = get_json_with_retry(&format!("{base_url}/api/repo-info"));
    assert_eq!(repo_info["owner"], "aplio");
    assert_eq!(repo_info["repo"], "gargo");
    assert_eq!(repo_info["branch"], "master");
    assert_eq!(repo_info["remote_url"], "https://github.com/aplio/gargo");
    assert!(
        repo_info["root"]
            .as_str()
            .is_some_and(|root| !root.is_empty())
    );

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
    assert!(status_html.contains(r#"data-component="status">Status</button>"#));
    assert!(status_html.contains(r#"location.pathname === "/status""#));
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
    assert!(branches_html.contains(r#"data-component="compare">Compare</button>"#));
    assert!(branches_html.contains(r#"location.pathname === "/branches""#));
    let commits_html = get_text_with_retry(&format!("{base_url}/aplio/gargo/commits/master"));
    assert!(commits_html.contains(r#"data-component="history">History</button>"#));
    assert!(commits_html.contains(r#"location.pathname.includes("/commits")"#));
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
    assert!(commit_html.contains(r#"data-component="history">History</button>"#));
    assert!(commit_html.contains("loadCurrentDiffPreview"));

    handle
        .command_tx
        .send(GargoServerCommand::SetActivePath {
            rel_path: Some("README.md".to_string()),
        })
        .expect("set active path");
    let event = get_json_with_retry(&format!("{base_url}/events?since=0"));
    assert_eq!(event["event"]["kind"], "navigate");
    assert_eq!(event["event"]["path"], "README.md");

    let _ = handle.command_tx.send(GargoServerCommand::Stop);
}

#[test]
fn split_view_serves_status_compare_and_commit_sources() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let handle = GargoServerHandle::new().expect("server handle");
    let Some(port) = start_server(repo, &handle) else {
        return;
    };
    let base_url = format!("http://127.0.0.1:{port}");

    // Status source: unstaged change to README.md — expect a paired change
    // row showing the new content and the auto-scroll marker.
    let status_html = get_text_with_retry(&format!(
        "{base_url}/split?source=status&section=unstaged&path=README.md"
    ));
    assert!(status_html.contains(r#"data-page="split""#));
    assert!(status_html.contains("split-grid"));
    assert!(status_html.contains("sp-row"));
    assert!(status_html.contains(r#"id="first-diff""#));
    // README.md is plain markdown without a registered highlighter, so its
    // changed content appears verbatim.
    assert!(status_html.contains("hello changed"));
    // Back link goes to /status
    assert!(status_html.contains(r#"href="/status""#));

    // Untracked file: one-sided (right pane only) + notice banner.
    let untracked_html = get_text_with_retry(&format!(
        "{base_url}/split?source=status&section=untracked&path=scratch.txt"
    ));
    assert!(untracked_html.contains("split-notice"));
    assert!(untracked_html.contains("sp-add"));

    // Compare source between master and feature on src/lib.rs. The added
    // function appears on the right side of a change/add row. Rust gets
    // syntax-highlighted, so check for the function name as plain text
    // (which falls between `<span>`s).
    let compare_html = get_text_with_retry(&format!(
        "{base_url}/split?source=compare&base=master&compare=feature&path=src/lib.rs"
    ));
    assert!(compare_html.contains("split-grid"));
    assert!(compare_html.contains("sp-text-r"));
    assert!(compare_html.contains("feature"));
    // Back link preserves both refs.
    assert!(compare_html.contains(r#"href="/compare?base=master&amp;compare=feature""#));

    // Commit source: pull the initial commit hash from /api/commits and
    // open the split view for README.md at that commit. The initial commit
    // is a root commit, so <hash>^ does not resolve — the handler should
    // gracefully fall back to right-only (still rendering the new side).
    let commits = get_json_with_retry(&format!("{base_url}/api/commits"));
    let first_hash = commits["commits"][0]["full_hash"].as_str().unwrap();
    let commit_html = get_text_with_retry(&format!(
        "{base_url}/split?source=commit&hash={first_hash}&path=README.md"
    ));
    assert!(commit_html.contains("split-grid"));
    assert!(commit_html.contains("Test Repo"));
    // Refs header reflects the parent → commit transition.
    assert!(commit_html.contains(&format!("{first_hash}^")));

    // Invalid source rejected with 400.
    let bad = ureq::get(&format!("{base_url}/split?source=nope&path=README.md")).call();
    match bad {
        Ok(_) => panic!("expected 400 on invalid source"),
        Err(ureq::Error::Status(status, _)) => assert_eq!(status, 400),
        Err(other) => panic!("unexpected error: {other}"),
    }

    let _ = handle.command_tx.send(GargoServerCommand::Stop);
}

#[test]
fn gargo_server_plugin_commands_replace_old_visible_server_commands() {
    let repo_dir = tempdir().expect("temp repo");
    let config = Config::default();
    let host = build_plugin_host(&config, repo_dir.path()).expect("plugin host");
    let commands: Vec<_> = host
        .command_specs()
        .iter()
        .map(|command| command.id.as_str())
        .collect();
    assert!(commands.contains(&"server.start_gargo"));
    assert!(commands.contains(&"server.stop_gargo"));
    assert!(!commands.contains(&"server.start_diff"));
    assert!(!commands.contains(&"server.stop_diff"));
    assert!(!commands.contains(&"server.open_compare"));
    assert!(!commands.contains(&"server.start_gargo_preview"));
    assert!(!commands.contains(&"server.stop_gargo_preview"));
    assert_eq!(
        Config::default().plugins.enabled,
        vec!["lsp".to_string(), "gargo_server".to_string()]
    );
}

#[test]
fn gargo_server_plugin_start_opens_repository_root_not_active_file() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let active_file = repo.join("README.md");
    let mut config = Config::default();
    config.plugins.enabled = vec!["gargo_server".to_string()];
    config.plugin.gargo_server.auto_open_browser = true;
    let editor = Editor::open(&active_file.to_string_lossy());
    let ctx = PluginContext::new(&editor, repo, &config);
    let mut host = build_plugin_host(&config, repo).expect("plugin host");

    let outputs = host.run_command("server.start_gargo", &ctx);
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
        url.ends_with('/'),
        "expected server start to open the gargo app root, got {url}"
    );
    assert!(
        !url.contains("/blob/README.md"),
        "expected server start not to open active file route"
    );
    let _ = host.run_command("server.stop_gargo", &ctx);
}

#[test]
fn gargo_server_concurrent_instances_use_distinct_ports() {
    let repo_dir = setup_repo();
    let repo = repo_dir.path();
    let first = GargoServerHandle::new().expect("first handle");
    let second = GargoServerHandle::new().expect("second handle");
    let Some(port_a) = start_server(repo, &first) else {
        return;
    };
    let Some(port_b) = start_server(repo, &second) else {
        return;
    };
    assert_ne!(port_a, port_b);
    let _ = first.command_tx.send(GargoServerCommand::Stop);
    let _ = second.command_tx.send(GargoServerCommand::Stop);
}

#[test]
fn old_plugin_ids_normalize_to_single_gargo_server_plugin() {
    let config: Config = toml::from_str(
        r#"
[plugins]
enabled = ["diff_ui", "gargo_preview", "gargo_server"]
"#,
    )
    .unwrap();
    assert_eq!(
        config.plugins.normalized_enabled(),
        vec!["gargo_server".to_string()]
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
            .filter(|command| **command == "server.start_gargo")
            .count(),
        1
    );
}

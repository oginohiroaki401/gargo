use std::path::PathBuf;

use clap::Parser;
use gargo::command::gargo_server::{GargoServerCommand, GargoServerEvent, GargoServerHandle};
use gargo::config::Config;
use gargo::core::editor::Editor;

fn main() {
    let cli = gargo::cli::Cli::parse();
    match cli.mode() {
        gargo::cli::CliMode::CheckUpgrade => {
            match gargo::upgrade::run(gargo::upgrade::UpgradeCommand::Check) {
                Ok(message) => {
                    println!("{message}");
                    return;
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        gargo::cli::CliMode::Update => {
            match gargo::upgrade::run(gargo::upgrade::UpgradeCommand::Update) {
                Ok(message) => {
                    println!("{message}");
                    return;
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        gargo::cli::CliMode::Server => {
            let start = cli.path.as_deref();
            let repo_root = gargo::project::find_project_root(start);
            if let Err(e) = run_server(repo_root, cli.open_browser(), cli.port) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
            return;
        }
        gargo::cli::CliMode::RunEditor => {}
    }

    let config = Config::load();
    let path_arg = cli.path.as_ref().and_then(|p| p.to_str());

    let editor = match path_arg {
        Some(path) => {
            let p = std::path::Path::new(path);
            if p.is_dir() {
                Editor::new()
            } else {
                Editor::open(path)
            }
        }
        None => Editor::new(),
    };

    let start_path = path_arg.map(std::path::Path::new);
    let mut app = gargo::app::App::new(editor, config, start_path);
    let mut stdout = gargo::terminal::setup();
    let result = app.run(&mut stdout);
    gargo::terminal::teardown(stdout);

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run_server(repo_root: PathBuf, open_browser: bool, port: Option<u16>) -> Result<(), String> {
    let handle = GargoServerHandle::new()?;
    handle
        .command_tx
        .send(GargoServerCommand::Start { repo_root, port })
        .map_err(|e| format!("Failed to send start command: {e}"))?;

    loop {
        match handle.event_rx.recv() {
            Ok(GargoServerEvent::Started { root_url, .. }) => {
                println!("{root_url}");
                if open_browser && let Err(e) = gargo::app::spawn_open_url(&root_url) {
                    eprintln!("Warning: failed to open browser: {e}");
                }
            }
            Ok(GargoServerEvent::Error(msg)) => {
                return Err(msg);
            }
            Ok(GargoServerEvent::Stopped) => return Ok(()),
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
}

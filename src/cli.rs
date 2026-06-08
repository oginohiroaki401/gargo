use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliMode {
    RunEditor,
    CheckUpgrade,
    Update,
    Server,
}

#[derive(Debug, Parser)]
#[command(name = "gargo")]
pub struct Cli {
    /// Check whether a newer version is available.
    #[arg(long, conflicts_with_all = ["update", "server"])]
    pub check: bool,

    /// Download and replace the current binary with the latest release.
    #[arg(long, conflicts_with_all = ["check", "server"])]
    pub update: bool,

    /// Start the gargo HTTP server without launching the editor.
    #[arg(long, conflicts_with_all = ["check", "update"])]
    pub server: bool,

    /// Do not open the browser after starting the server (server mode only).
    #[arg(long, requires = "server", conflicts_with_all = ["check", "update"])]
    pub no_open: bool,

    /// Port for the HTTP server (server mode only). Defaults to an
    /// OS-assigned ephemeral port when omitted.
    #[arg(long, requires = "server", conflicts_with_all = ["check", "update"])]
    pub port: Option<u16>,

    /// Optional file or directory to open.
    #[arg(value_name = "PATH", conflicts_with_all = ["check", "update"])]
    pub path: Option<PathBuf>,
}

impl Cli {
    pub fn mode(&self) -> CliMode {
        if self.check {
            CliMode::CheckUpgrade
        } else if self.update {
            CliMode::Update
        } else if self.server {
            CliMode::Server
        } else {
            CliMode::RunEditor
        }
    }

    /// Whether to open the browser after the server starts. Defaults to true;
    /// suppressed by `--no-open`.
    pub fn open_browser(&self) -> bool {
        !self.no_open
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, CliMode};

    #[test]
    fn parses_check_flag() {
        let cli = Cli::try_parse_from(["gargo", "--check"]).expect("parse --check");
        assert_eq!(cli.mode(), CliMode::CheckUpgrade);
        assert!(cli.path.is_none());
    }

    #[test]
    fn parses_update_flag() {
        let cli = Cli::try_parse_from(["gargo", "--update"]).expect("parse --update");
        assert_eq!(cli.mode(), CliMode::Update);
        assert!(cli.path.is_none());
    }

    #[test]
    fn parses_server_flag() {
        let cli = Cli::try_parse_from(["gargo", "--server"]).expect("parse --server");
        assert_eq!(cli.mode(), CliMode::Server);
        assert!(cli.path.is_none());
        assert!(cli.open_browser(), "server defaults to opening the browser");
    }

    #[test]
    fn parses_server_no_open_flag() {
        let cli = Cli::try_parse_from(["gargo", "--server", "--no-open"])
            .expect("parse --server --no-open");
        assert_eq!(cli.mode(), CliMode::Server);
        assert!(!cli.open_browser(), "--no-open suppresses the browser");
    }

    #[test]
    fn parses_server_port_flag() {
        let cli = Cli::try_parse_from(["gargo", "--server", "--port", "8080"])
            .expect("parse --server --port 8080");
        assert_eq!(cli.mode(), CliMode::Server);
        assert_eq!(cli.port, Some(8080));
    }

    #[test]
    fn rejects_port_without_server() {
        let err =
            Cli::try_parse_from(["gargo", "--port", "8080"]).expect_err("--port requires --server");
        let message = err.to_string();
        assert!(
            message.contains("following required") || message.contains("cannot be used"),
            "unexpected clap error: {message}"
        );
    }

    #[test]
    fn rejects_no_open_without_server() {
        let err =
            Cli::try_parse_from(["gargo", "--no-open"]).expect_err("--no-open requires --server");
        let message = err.to_string();
        assert!(
            message.contains("following required") || message.contains("cannot be used"),
            "unexpected clap error: {message}"
        );
    }

    #[test]
    fn parses_positional_path() {
        let cli = Cli::try_parse_from(["gargo", "README.md"]).expect("parse path");
        assert_eq!(cli.mode(), CliMode::RunEditor);
        assert_eq!(cli.path.as_deref(), Some(std::path::Path::new("README.md")));
    }

    #[test]
    fn parses_separator_for_path_like_flag() {
        let cli = Cli::try_parse_from(["gargo", "--", "--update"]).expect("parse -- separator");
        assert_eq!(cli.mode(), CliMode::RunEditor);
        assert_eq!(cli.path.as_deref(), Some(std::path::Path::new("--update")));
    }

    #[test]
    fn rejects_conflicting_flags() {
        let err = Cli::try_parse_from(["gargo", "--check", "--update"]).expect_err("conflict");
        let message = err.to_string();
        assert!(
            message.contains("cannot be used with"),
            "unexpected clap error: {message}"
        );
    }

    #[test]
    fn rejects_path_with_update_flag() {
        let err = Cli::try_parse_from(["gargo", "--update", "README.md"])
            .expect_err("path conflicts with --update");
        let message = err.to_string();
        assert!(
            message.contains("cannot be used with"),
            "unexpected clap error: {message}"
        );
    }
}

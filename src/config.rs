use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{collections::HashMap, fs, io, path::PathBuf};

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub debug: bool,
    pub debug_log_path: PathBuf,
    #[serde(alias = "show_line_numbers", alias = "line_numbers")]
    pub show_line_number: bool,
    #[serde(alias = "line_number_min_width")]
    pub line_number_width: usize,
    pub tab_width: usize,
    pub horizontal_scroll_margin: usize,
    pub plugins: PluginsConfig,
    pub lsp: LspConfig,
    pub plugin: PluginConfig,
    pub git: GitConfig,
    pub performance: PerformanceConfig,
    pub theme: ThemeConfig,
    pub ui: UiConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub enabled: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct LspConfig {
    pub servers: Vec<LspServerConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct LspServerConfig {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub languages: Vec<String>,
    pub root: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PluginConfig {
    pub github_server: PluginGithubServerConfig,
    pub diff_ui: PluginDiffUiConfig,
    pub github_preview: PluginGithubPreviewConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginGithubServerConfig {
    pub auto_open_browser: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginDiffUiConfig {
    pub auto_open_browser: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginGithubPreviewConfig {
    pub auto_open_browser: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GitConfig {
    pub gutter_debounce_high_priority_ms: u64,
    pub gutter_debounce_normal_ms: u64,
    pub git_view_diff_cache_max_entries: usize,
    pub git_view_diff_prefetch_radius: usize,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    pub popup_width_percent: u8,
    pub popup_height_percent: u8,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PerformanceConfig {
    pub file_index: PerformanceFileIndexConfig,
    pub lsp: PerformanceLspConfig,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PerformanceFileIndexConfig {
    pub mode: FileIndexMode,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PerformanceLspConfig {
    pub start_mode: LspStartMode,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileIndexMode {
    Eager,
    #[default]
    Lazy,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LspStartMode {
    Eager,
    #[default]
    OnDemand,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub preset: String,
    pub captures: HashMap<String, ThemeCaptureConfig>,
    pub ui: ThemeUiConfig,
    /// Coloring for the browser editor (`gargo server`). The terminal UI uses
    /// `preset`/`captures` above; the web editor renders in a browser with its
    /// own light/dark palettes and chrome, configured here.
    pub editor: ThemeEditorConfig,
}

/// Theme for the browser editor served by `gargo server`. Mirrors the terminal
/// theme in spirit (a preset plus per-scope overrides) but carries web-specific
/// chrome colors (background, gutter, selection, …) and uses CSS colors so it
/// can target a browser. Defaults to a light palette; set `preset = "dark"` for
/// the VSCode-dark palette.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct ThemeEditorConfig {
    /// `"light"` (default) or `"dark"`. Selects the built-in base palette.
    pub preset: Option<String>,
    /// Editor background.
    pub bg: Option<String>,
    /// Default foreground (text without a more specific token color).
    pub fg: Option<String>,
    /// Line-number gutter color.
    pub gutter: Option<String>,
    /// Caret color.
    pub caret: Option<String>,
    /// Selection / current-search-match background.
    pub selection: Option<String>,
    /// Status bar background.
    pub status_bg: Option<String>,
    /// Git change-gutter bar colors.
    pub git_added: Option<String>,
    pub git_modified: Option<String>,
    pub git_deleted: Option<String>,
    /// Per-scope syntax color overrides (e.g. `keyword`, `string`). Each value
    /// is a CSS color (`#rrggbb` or a named color); `bold`/`italic` optional.
    pub tokens: HashMap<String, ThemeCaptureConfig>,
    /// Soft-wrap long lines by default in the browser editor. When false
    /// (default) lines scroll horizontally; a per-tab toggle (Alt+Z) overrides
    /// this and is remembered in the browser.
    pub wrap: bool,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct ThemeCaptureConfig {
    pub fg: Option<String>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct ThemeUiConfig {
    pub markdown_link_hover_bg: Option<String>,
    pub markdown_link_hover_selected_bg: Option<String>,
}

impl Default for PluginDiffUiConfig {
    fn default() -> Self {
        Self {
            auto_open_browser: true,
        }
    }
}

impl Default for PluginGithubServerConfig {
    fn default() -> Self {
        Self {
            auto_open_browser: true,
        }
    }
}

impl Default for PluginGithubPreviewConfig {
    fn default() -> Self {
        Self {
            auto_open_browser: true,
        }
    }
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            gutter_debounce_high_priority_ms: 1,
            gutter_debounce_normal_ms: 96,
            git_view_diff_cache_max_entries: 64,
            git_view_diff_prefetch_radius: 1,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            popup_width_percent: 95,
            popup_height_percent: 90,
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            preset: "ansi_dark".to_string(),
            captures: HashMap::new(),
            ui: ThemeUiConfig::default(),
            editor: ThemeEditorConfig::default(),
        }
    }
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: vec!["lsp".to_string(), "github_server".to_string()],
        }
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            servers: vec![
                LspServerConfig::default(),
                LspServerConfig {
                    id: "rust-analyzer".to_string(),
                    command: "rust-analyzer".to_string(),
                    args: Vec::new(),
                    languages: vec!["rust".to_string()],
                    root: "project".to_string(),
                },
            ],
        }
    }
}

impl Default for LspServerConfig {
    fn default() -> Self {
        Self {
            id: "marksman".to_string(),
            command: "marksman".to_string(),
            args: vec!["server".to_string()],
            languages: vec!["markdown".to_string()],
            root: "project".to_string(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            debug: false,
            debug_log_path: PathBuf::from("/tmp/gargo.log"),
            show_line_number: true,
            line_number_width: 5,
            tab_width: 4,
            horizontal_scroll_margin: 5,
            plugins: PluginsConfig::default(),
            lsp: LspConfig::default(),
            plugin: PluginConfig::default(),
            git: GitConfig::default(),
            performance: PerformanceConfig::default(),
            theme: ThemeConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
}

fn load_toml_with_default<T>(path: &Path) -> io::Result<T>
where
    T: DeserializeOwned + Default,
{
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
        Err(err) => return Err(err),
    };

    toml::from_str::<T>(&content).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed parsing config {}: {err}", path.display()),
        )
    })
}

pub fn app_config_dir() -> PathBuf {
    config_home().join("gargo")
}

pub fn app_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("gargo")
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        Some(app_config_dir().join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let mut config: Self = load_toml_with_default(&path).unwrap_or_default();
        config.plugins.enabled = config.plugins.normalized_enabled();
        config
    }
}

impl PluginsConfig {
    pub fn normalized_enabled(&self) -> Vec<String> {
        let mut out = Vec::new();
        for id in &self.enabled {
            let normalized = match id.as_str() {
                "diff_ui" | "github_preview" => "github_server",
                other => other,
            };
            if !out.iter().any(|existing| existing == normalized) {
                out.push(normalized.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let cfg = Config::default();
        assert!(!cfg.debug);
        assert_eq!(cfg.debug_log_path, PathBuf::from("/tmp/gargo.log"));
        assert_eq!(cfg.line_number_width, 5);
        assert_eq!(cfg.horizontal_scroll_margin, 5);
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.performance.file_index.mode, FileIndexMode::Lazy);
        assert_eq!(cfg.performance.lsp.start_mode, LspStartMode::OnDemand);
        assert_eq!(cfg.theme.preset, "ansi_dark");
        assert!(cfg.theme.captures.is_empty());
        assert_eq!(cfg.theme.ui.markdown_link_hover_bg, None);
        assert_eq!(cfg.theme.ui.markdown_link_hover_selected_bg, None);
    }

    #[test]
    fn test_full_toml() {
        let toml_str = r#"
debug = true
debug_log_path = "/var/log/gargo.log"
show_line_number = false
[plugins]
enabled = ["diff_ui"]

[[lsp.servers]]
id = "marksman"
command = "/opt/marksman"
args = ["server"]
languages = ["markdown"]
root = "project"

[theme]
preset = "ansi_light"

[performance.file_index]
mode = "eager"

[performance.lsp]
start_mode = "eager"

[theme.captures]
"keyword" = { fg = "red", bold = false }

[theme.ui]
markdown_link_hover_bg = "dark_grey"
markdown_link_hover_selected_bg = "grey"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.debug);
        assert_eq!(cfg.debug_log_path, PathBuf::from("/var/log/gargo.log"));
        assert!(!cfg.show_line_number);
        assert_eq!(cfg.horizontal_scroll_margin, 5);
        assert_eq!(cfg.plugins.enabled, vec!["diff_ui"]);
        assert_eq!(cfg.lsp.servers.len(), 1);
        assert_eq!(cfg.lsp.servers[0].command, "/opt/marksman");
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.performance.file_index.mode, FileIndexMode::Eager);
        assert_eq!(cfg.performance.lsp.start_mode, LspStartMode::Eager);
        assert_eq!(cfg.theme.preset, "ansi_light");
        assert_eq!(cfg.theme.captures["keyword"].fg.as_deref(), Some("red"));
        assert_eq!(cfg.theme.captures["keyword"].bold, Some(false));
        assert_eq!(
            cfg.theme.ui.markdown_link_hover_bg.as_deref(),
            Some("dark_grey")
        );
        assert_eq!(
            cfg.theme.ui.markdown_link_hover_selected_bg.as_deref(),
            Some("grey")
        );
    }

    #[test]
    fn test_partial_toml() {
        let toml_str = r#"debug = true"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.debug);
        assert_eq!(cfg.debug_log_path, PathBuf::from("/tmp/gargo.log"));
        assert!(!cfg.plugins.enabled.is_empty());
        assert_eq!(cfg.lsp.servers[0].command, "marksman");
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.performance.file_index.mode, FileIndexMode::Lazy);
        assert_eq!(cfg.performance.lsp.start_mode, LspStartMode::OnDemand);
        assert_eq!(cfg.theme.preset, "ansi_dark");
        assert!(cfg.theme.captures.is_empty());
        assert_eq!(cfg.theme.ui.markdown_link_hover_bg, None);
        assert_eq!(cfg.theme.ui.markdown_link_hover_selected_bg, None);
    }

    #[test]
    fn test_empty_string() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.debug);
        assert_eq!(cfg.debug_log_path, PathBuf::from("/tmp/gargo.log"));
        assert!(!cfg.plugins.enabled.is_empty());
        assert_eq!(cfg.lsp.servers[0].command, "marksman");
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 96);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(cfg.performance.file_index.mode, FileIndexMode::Lazy);
        assert_eq!(cfg.performance.lsp.start_mode, LspStartMode::OnDemand);
        assert_eq!(cfg.theme.preset, "ansi_dark");
        assert!(cfg.theme.captures.is_empty());
        assert_eq!(cfg.theme.ui.markdown_link_hover_bg, None);
        assert_eq!(cfg.theme.ui.markdown_link_hover_selected_bg, None);
    }

    #[test]
    fn test_load_returns_config() {
        let cfg = Config::load();
        // Should not panic regardless of whether the file exists
        let _ = cfg.debug;
        let _ = cfg.debug_log_path;
    }

    #[test]
    fn test_default_config_serializes_to_toml() {
        let cfg = Config::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.tab_width, 4);
        assert_eq!(parsed.horizontal_scroll_margin, 5);
        assert!(parsed.show_line_number);
        assert_eq!(
            parsed.plugin.github_server.auto_open_browser,
            cfg.plugin.github_server.auto_open_browser
        );
        assert_eq!(parsed.git.gutter_debounce_high_priority_ms, 1);
        assert_eq!(parsed.performance.file_index.mode, FileIndexMode::Lazy);
        assert_eq!(parsed.performance.lsp.start_mode, LspStartMode::OnDemand);
        assert_eq!(parsed.git.gutter_debounce_normal_ms, 96);
        assert_eq!(parsed.git.git_view_diff_cache_max_entries, 64);
        assert_eq!(parsed.git.git_view_diff_prefetch_radius, 1);
        assert_eq!(parsed.theme.preset, "ansi_dark");
        assert!(parsed.theme.captures.is_empty());
        assert_eq!(parsed.theme.ui.markdown_link_hover_bg, None);
        assert_eq!(parsed.theme.ui.markdown_link_hover_selected_bg, None);
    }

    #[test]
    fn test_partial_config_roundtrip_fills_missing_with_defaults() {
        let toml_str = r#"
tab_width = 2
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&out).unwrap();

        assert_eq!(parsed.tab_width, 2);
        assert_eq!(parsed.horizontal_scroll_margin, 5);
        assert!(parsed.show_line_number);
        assert_eq!(parsed.plugins.enabled, Config::default().plugins.enabled);
        assert_eq!(
            parsed.git.gutter_debounce_high_priority_ms,
            Config::default().git.gutter_debounce_high_priority_ms
        );
        assert_eq!(
            parsed.git.gutter_debounce_normal_ms,
            Config::default().git.gutter_debounce_normal_ms
        );
        assert_eq!(
            parsed.git.git_view_diff_cache_max_entries,
            Config::default().git.git_view_diff_cache_max_entries
        );
        assert_eq!(
            parsed.git.git_view_diff_prefetch_radius,
            Config::default().git.git_view_diff_prefetch_radius
        );
        assert_eq!(parsed.theme.preset, "ansi_dark");
        assert!(parsed.theme.captures.is_empty());
        assert_eq!(parsed.theme.ui.markdown_link_hover_bg, None);
        assert_eq!(parsed.theme.ui.markdown_link_hover_selected_bg, None);
    }

    #[test]
    fn test_git_config_parses_custom_values() {
        let toml_str = r#"
[git]
gutter_debounce_high_priority_ms = 7
gutter_debounce_normal_ms = 150
git_view_diff_cache_max_entries = 99
git_view_diff_prefetch_radius = 3
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.git.gutter_debounce_high_priority_ms, 7);
        assert_eq!(cfg.git.gutter_debounce_normal_ms, 150);
        assert_eq!(cfg.git.git_view_diff_cache_max_entries, 99);
        assert_eq!(cfg.git.git_view_diff_prefetch_radius, 3);
    }

    #[test]
    fn test_theme_config_parses_custom_values() {
        let toml_str = r##"
[theme]
preset = "ansi_light"

[theme.captures]
"comment" = { fg = "#123456", italic = false }
"keyword" = { bold = true }

[theme.ui]
markdown_link_hover_bg = "#111111"
markdown_link_hover_selected_bg = "#222222"
"##;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.theme.preset, "ansi_light");
        assert_eq!(cfg.theme.captures["comment"].fg.as_deref(), Some("#123456"));
        assert_eq!(cfg.theme.captures["comment"].italic, Some(false));
        assert_eq!(cfg.theme.captures["keyword"].bold, Some(true));
        assert_eq!(
            cfg.theme.ui.markdown_link_hover_bg.as_deref(),
            Some("#111111")
        );
        assert_eq!(
            cfg.theme.ui.markdown_link_hover_selected_bg.as_deref(),
            Some("#222222")
        );
    }

    #[test]
    fn test_horizontal_scroll_margin_parses_custom_value() {
        let toml_str = r#"
horizontal_scroll_margin = 9
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.horizontal_scroll_margin, 9);
    }

    #[test]
    fn test_line_number_width_parses_custom_value() {
        let toml_str = r#"
line_number_width = 3
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.line_number_width, 3);
    }

    #[test]
    fn test_line_number_width_alias() {
        let toml_str = r#"
line_number_min_width = 7
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.line_number_width, 7);
    }

    #[test]
    fn test_ui_config_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.ui.popup_width_percent, 95);
        assert_eq!(cfg.ui.popup_height_percent, 90);
    }

    #[test]
    fn test_ui_config_parses_custom_values() {
        let toml_str = r#"
[ui]
popup_width_percent = 80
popup_height_percent = 75
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ui.popup_width_percent, 80);
        assert_eq!(cfg.ui.popup_height_percent, 75);
    }

    #[test]
    fn test_ui_config_partial_keeps_defaults() {
        let toml_str = r#"
[ui]
popup_width_percent = 92
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ui.popup_width_percent, 92);
        assert_eq!(cfg.ui.popup_height_percent, 90);
    }

    #[test]
    fn test_plugin_normalization_maps_old_server_plugins_to_unified_server() {
        let cfg: Config = toml::from_str(
            r#"
[plugins]
enabled = ["lsp", "diff_ui", "github_preview", "github_server"]
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.plugins.normalized_enabled(),
            vec!["lsp".to_string(), "github_server".to_string()]
        );
    }
}

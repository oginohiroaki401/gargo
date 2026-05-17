use std::path::Path;

use crate::config::Config;
use crate::plugin::diff_ui::DiffUiPlugin;
use crate::plugin::github_preview::GithubPreviewPlugin;
use crate::plugin::github_server::GithubServerPlugin;
use crate::plugin::host::PluginHost;
use crate::plugin::lsp::LspPlugin;
use crate::plugin::types::Plugin;

pub fn build_plugin_host(config: &Config, project_root: &Path) -> Result<PluginHost, String> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();

    for plugin_id in config.plugins.normalized_enabled() {
        match plugin_id.as_str() {
            "github_server" => {
                plugins.push(Box::new(GithubServerPlugin::new(config, project_root)));
            }
            "diff_ui" => plugins.push(Box::new(DiffUiPlugin::new(config))),
            "github_preview" => {
                plugins.push(Box::new(GithubPreviewPlugin::new(config, project_root)));
            }
            "lsp" => plugins.push(Box::new(LspPlugin::new(config, project_root))),
            other => {
                return Err(format!(
                    "Unknown plugin id in config.plugins.enabled: {}",
                    other
                ));
            }
        }
    }

    Ok(PluginHost::new(plugins))
}

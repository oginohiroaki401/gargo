use std::collections::{HashMap, HashSet};

use crate::plugin::types::{Plugin, PluginCommandSpec, PluginContext, PluginEvent, PluginOutput};

pub struct PluginHost {
    plugins: Vec<Box<dyn Plugin>>,
    command_specs: Vec<PluginCommandSpec>,
    command_to_plugin_index: HashMap<String, usize>,
}

impl PluginHost {
    pub fn new(plugins: Vec<Box<dyn Plugin>>) -> Self {
        let mut command_specs = Vec::new();
        let mut command_to_plugin_index = HashMap::new();

        for (idx, plugin) in plugins.iter().enumerate() {
            for spec in plugin.commands() {
                command_to_plugin_index.insert(spec.id.clone(), idx);
                command_specs.push(spec.clone());
            }
        }

        Self {
            plugins,
            command_specs,
            command_to_plugin_index,
        }
    }

    pub fn command_specs(&self) -> &[PluginCommandSpec] {
        &self.command_specs
    }

    /// Command ids that should currently be hidden from the command palette,
    /// aggregated across all plugins based on their live runtime state.
    pub fn hidden_command_ids(&self) -> HashSet<String> {
        self.plugins
            .iter()
            .flat_map(|plugin| plugin.hidden_command_ids())
            .collect()
    }

    pub fn run_command(&mut self, command_id: &str, ctx: &PluginContext) -> Vec<PluginOutput> {
        let Some(plugin_idx) = self.command_to_plugin_index.get(command_id).copied() else {
            return vec![PluginOutput::Message(format!(
                "Unknown plugin command: {}",
                command_id
            ))];
        };
        self.plugins[plugin_idx].on_command(command_id, ctx)
    }

    pub fn on_event(&mut self, event: &PluginEvent, ctx: &PluginContext) -> Vec<PluginOutput> {
        let mut all = Vec::new();
        for plugin in &mut self.plugins {
            all.extend(plugin.on_event(event, ctx));
        }
        all
    }

    pub fn poll(&mut self, ctx: &PluginContext) -> Vec<PluginOutput> {
        let mut all = Vec::new();
        for plugin in &mut self.plugins {
            all.extend(plugin.poll(ctx));
        }
        all
    }
}

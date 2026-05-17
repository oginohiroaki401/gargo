use std::io::Write;
use std::process::{Command as ProcessCommand, Stdio};

use crate::core::editor::Editor;
use crate::core::mode::Mode;
use crate::input::action::{
    Action, AppAction, BufferAction, CoreAction, IntegrationAction, LifecycleAction, ProjectAction,
    WorkspaceAction,
};

pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut child = if cfg!(target_os = "macos") {
        ProcessCommand::new("pbcopy").stdin(Stdio::piped()).spawn()
    } else {
        ProcessCommand::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
    }
    .map_err(|e| e.to_string())?;

    child
        .stdin
        .as_mut()
        .ok_or("failed to open stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;

    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn gargo_version_info() -> String {
    format!(
        "{} v{} ({}/{})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

pub type CommandId = String;

pub enum CommandEffect {
    None,
    Message(String),
    Action(Action),
}

pub struct CommandContext<'a> {
    editor: &'a Editor,
}

impl<'a> CommandContext<'a> {
    pub fn new(editor: &'a Editor) -> Self {
        Self { editor }
    }

    pub fn editor(&self) -> &Editor {
        self.editor
    }
}

#[allow(dead_code)]
pub struct CommandEntry {
    pub id: CommandId,
    pub label: String,
    pub category: Option<String>,
    pub action: Box<dyn Fn(&CommandContext) -> CommandEffect>,
}

pub struct CommandRegistry {
    commands: Vec<CommandEntry>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    pub fn register(&mut self, entry: CommandEntry) {
        self.commands.push(entry);
    }

    pub fn commands(&self) -> &[CommandEntry] {
        &self.commands
    }

    pub fn register_plugin_commands(
        &mut self,
        commands: &[crate::plugin::types::PluginCommandSpec],
    ) {
        for command in commands {
            let id = command.id.clone();
            self.register(CommandEntry {
                id: id.clone(),
                label: command.label.clone(),
                category: command.category.clone(),
                action: Box::new(move |_ctx| {
                    CommandEffect::Action(Action::App(AppAction::Integration(
                        IntegrationAction::RunPluginCommand { id: id.clone() },
                    )))
                }),
            });
        }
    }
}

pub fn register_builtins(registry: &mut CommandRegistry) {
    super::git::register(registry);
    super::in_editor_diff::register(registry);
    registry.register(CommandEntry {
        id: "core.save".into(),
        label: "Save File".into(),
        category: Some("Core".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Buffer(BufferAction::Save)))
        }),
    });

    registry.register(CommandEntry {
        id: "file.save_as".into(),
        label: "Save current buffer as ...".into(),
        category: Some("File".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenSaveBufferAsPopup,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "file.rename".into(),
        label: "Rename file in buffer".into(),
        category: Some("File".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Buffer(
                BufferAction::OpenRenameFilePopup,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "config.reload".into(),
        label: "Reload Config".into(),
        category: Some("Config".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::ReloadConfig,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "config.open_file".into(),
        label: "Open Config File".into(),
        category: Some("Config".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::OpenConfigFile,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "config.create_default".into(),
        label: "Create Default Config".into(),
        category: Some("Config".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::CreateDefaultConfig,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "config.toggle_debug".into(),
        label: "Show Debug".into(),
        category: Some("Config".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::ToggleDebug,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "config.toggle_line_numbers".into(),
        label: "Show Line Number".into(),
        category: Some("Config".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::ToggleLineNumber,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "core.quit".into(),
        label: "Quit Editor".into(),
        category: Some("Core".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(LifecycleAction::Quit)))
        }),
    });

    registry.register(CommandEntry {
        id: "core.force_quit".into(),
        label: "Force Quit Editor".into(),
        category: Some("Core".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Lifecycle(
                LifecycleAction::ForceQuit,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "edit.find_replace".into(),
        label: "Find and Replace".into(),
        category: Some("Edit".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenFindReplace,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "search.global".into(),
        label: "Global Search".into(),
        category: Some("Search".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenGlobalSearch,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "core.copy_branch_name".into(),
        label: "Copy Current Branch Name".into(),
        category: Some("Git".into()),
        action: Box::new(|ctx| {
            let mut cmd = ProcessCommand::new("git");
            cmd.args(["branch", "--show-current"]);
            if let Some(path) = &ctx.editor().active_buffer().file_path {
                let root = crate::project::find_project_root(Some(path));
                cmd.current_dir(root);
            }
            let result = cmd.output();
            match result {
                Ok(output) => {
                    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if branch.is_empty() {
                        CommandEffect::Message("Not in a git repository".into())
                    } else {
                        match copy_to_clipboard(&branch) {
                            Ok(()) => CommandEffect::Message(format!("Copied: {}", branch)),
                            Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
                        }
                    }
                }
                Err(e) => CommandEffect::Message(format!("git error: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "core.copy_gargo_version".into(),
        label: "Copy Gargo Version".into(),
        category: Some("Help".into()),
        action: Box::new(|_ctx| {
            let info = gargo_version_info();
            match copy_to_clipboard(&info) {
                Ok(()) => CommandEffect::Message(format!("Copied: {}", info)),
                Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
            }
        }),
    });

    registry.register(CommandEntry {
        id: "file.copy_absolute_path".into(),
        label: "Copy File's Absolute Path".into(),
        category: Some("File".into()),
        action: Box::new(|ctx| match &ctx.editor().active_buffer().file_path {
            Some(path) => {
                let text = path.display().to_string();
                match copy_to_clipboard(&text) {
                    Ok(()) => CommandEffect::Message(format!("Copied: {}", text)),
                    Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
                }
            }
            None => CommandEffect::Message("No file path (scratch buffer)".into()),
        }),
    });

    registry.register(CommandEntry {
        id: "file.copy_relative_path".into(),
        label: "Copy File's Relative Path".into(),
        category: Some("File".into()),
        action: Box::new(|ctx| match &ctx.editor().active_buffer().file_path {
            Some(path) => {
                let root = crate::project::find_project_root(Some(path));
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .display()
                    .to_string();
                match copy_to_clipboard(&rel) {
                    Ok(()) => CommandEffect::Message(format!("Copied: {}", rel)),
                    Err(e) => CommandEffect::Message(format!("Copy failed: {}", e)),
                }
            }
            None => CommandEffect::Message("No file path (scratch buffer)".into()),
        }),
    });

    registry.register(CommandEntry {
        id: "file.open_current_file".into(),
        label: "Open Current File (System)".into(),
        category: Some("File".into()),
        action: Box::new(|ctx| match &ctx.editor().active_buffer().file_path {
            Some(path) => {
                let path_str = path.display().to_string();
                let result = if cfg!(target_os = "macos") {
                    ProcessCommand::new("open").arg(&path_str).spawn()
                } else {
                    ProcessCommand::new("xdg-open").arg(&path_str).spawn()
                };
                match result {
                    Ok(_) => CommandEffect::Message(format!("Opened: {}", path_str)),
                    Err(e) => CommandEffect::Message(format!("Open failed: {}", e)),
                }
            }
            None => CommandEffect::Message("No file path (scratch buffer)".into()),
        }),
    });

    registry.register(CommandEntry {
        id: "file.reveal_in_finder".into(),
        label: "Reveal Current File in Finder".into(),
        category: Some("File".into()),
        action: Box::new(|ctx| match &ctx.editor().active_buffer().file_path {
            Some(path) => {
                let path_str = path.display().to_string();
                let result = if cfg!(target_os = "macos") {
                    ProcessCommand::new("open").args(["-R", &path_str]).spawn()
                } else {
                    let dir = path.parent().unwrap_or(path).display().to_string();
                    ProcessCommand::new("xdg-open").arg(&dir).spawn()
                };
                match result {
                    Ok(_) => CommandEffect::Message(format!("Revealed: {}", path_str)),
                    Err(e) => CommandEffect::Message(format!("Reveal failed: {}", e)),
                }
            }
            None => CommandEffect::Message("No file path (scratch buffer)".into()),
        }),
    });

    registry.register(CommandEntry {
        id: "core.change_mode_insert".into(),
        label: "Enter Insert Mode".into(),
        category: Some("Mode".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::ChangeMode(Mode::Insert)))
        }),
    });

    registry.register(CommandEntry {
        id: "core.change_mode_normal".into(),
        label: "Enter Normal Mode".into(),
        category: Some("Mode".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::ChangeMode(Mode::Normal)))
        }),
    });

    // Multi-cursor commands
    registry.register(CommandEntry {
        id: "cursor.add_above".into(),
        label: "Add Cursor Above".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::AddCursorAbove))),
    });

    registry.register(CommandEntry {
        id: "cursor.add_below".into(),
        label: "Add Cursor Below".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::AddCursorBelow))),
    });

    registry.register(CommandEntry {
        id: "cursor.add_next_match".into(),
        label: "Add Cursor to Next Match".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::AddCursorToNextMatch))
        }),
    });

    registry.register(CommandEntry {
        id: "cursor.add_prev_match".into(),
        label: "Add Cursor to Previous Match".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::AddCursorToPrevMatch))
        }),
    });

    registry.register(CommandEntry {
        id: "cursor.add_all_matches".into(),
        label: "Add Cursor to All Matches".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::AddCursorToAllMatches))
        }),
    });

    registry.register(CommandEntry {
        id: "cursor.add_to_top".into(),
        label: "Add Cursors to Top".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::AddCursorsToTop))),
    });

    registry.register(CommandEntry {
        id: "cursor.add_to_bottom".into(),
        label: "Add Cursors to Bottom".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::AddCursorsToBottom))
        }),
    });

    registry.register(CommandEntry {
        id: "cursor.remove_secondary".into(),
        label: "Remove Secondary Cursors".into(),
        category: Some("Cursor".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::Core(CoreAction::RemoveSecondaryCursors))
        }),
    });

    registry.register(CommandEntry {
        id: "buffer.next".into(),
        label: "Next Buffer".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::NextBuffer))),
    });

    registry.register(CommandEntry {
        id: "buffer.prev".into(),
        label: "Previous Buffer".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::PrevBuffer))),
    });

    registry.register(CommandEntry {
        id: "buffer.close".into(),
        label: "Close Buffer".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Buffer(BufferAction::CloseBuffer)))
        }),
    });

    registry.register(CommandEntry {
        id: "buffer.new".into(),
        label: "New Buffer".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_ctx| CommandEffect::Action(Action::Core(CoreAction::NewBuffer))),
    });

    registry.register(CommandEntry {
        id: "buffer.list".into(),
        label: "Buffer List".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenBufferPicker,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "jump.list".into(),
        label: "Jump List".into(),
        category: Some("Navigation".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenJumpListPicker,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "symbol.current_document".into(),
        label: "Go to Symbol (Current File)".into(),
        category: Some("Navigation".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenSymbolPicker,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "symbol.smart_copy".into(),
        label: "Smart Copy (Current File)".into(),
        category: Some("Navigation".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenSmartCopy,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "buffer.refresh".into(),
        label: "Refresh Buffer from Disk".into(),
        category: Some("Buffer".into()),
        action: Box::new(|_ctx| {
            CommandEffect::Action(Action::App(AppAction::Buffer(BufferAction::RefreshBuffer)))
        }),
    });

    registry.register(CommandEntry {
        id: "explorer.toggle".into(),
        label: "Open File Explorer".into(),
        category: Some("Explorer".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::ToggleExplorer,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "explorer.reveal".into(),
        label: "Reveal Current File in Explorer".into(),
        category: Some("Explorer".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::RevealInExplorer,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "explorer.popup".into(),
        label: "Open Explorer (Big)".into(),
        category: Some("Explorer".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Workspace(
                WorkspaceAction::OpenExplorerPopup,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "project.change_root".into(),
        label: "Change Project Root".into(),
        category: Some("Project".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Project(
                ProjectAction::OpenProjectRootPicker,
            )))
        }),
    });

    registry.register(CommandEntry {
        id: "project.switch_recent".into(),
        label: "Switch to Recent Project".into(),
        category: Some("Project".into()),
        action: Box::new(|_editor| {
            CommandEffect::Action(Action::App(AppAction::Project(
                ProjectAction::OpenRecentProjectPicker,
            )))
        }),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_include_config_commands() {
        let mut registry = CommandRegistry::new();
        register_builtins(&mut registry);

        let ids: Vec<&str> = registry.commands().iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"config.reload"));
        assert!(ids.contains(&"config.open_file"));
        assert!(ids.contains(&"config.create_default"));
        assert!(ids.contains(&"config.toggle_debug"));
        assert!(ids.contains(&"config.toggle_line_numbers"));
        assert!(ids.contains(&"core.force_quit"));
        assert!(ids.contains(&"symbol.current_document"));
        assert!(ids.contains(&"symbol.smart_copy"));
        assert!(ids.contains(&"diff.open_in_editor"));
        assert!(ids.contains(&"diff.refresh_in_editor"));
        assert!(ids.contains(&"project.change_root"));
        assert!(ids.contains(&"project.switch_recent"));
        assert!(ids.contains(&"file.save_as"));
        assert!(ids.contains(&"cursor.add_next_match"));
        assert!(ids.contains(&"cursor.add_prev_match"));
        assert!(ids.contains(&"cursor.add_all_matches"));
    }

    #[test]
    fn builtins_include_file_path_commands() {
        let mut registry = CommandRegistry::new();
        register_builtins(&mut registry);

        let ids: Vec<&str> = registry.commands().iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"file.copy_absolute_path"));
        assert!(ids.contains(&"file.copy_relative_path"));
        assert!(ids.contains(&"file.open_current_file"));
        assert!(ids.contains(&"file.reveal_in_finder"));
    }
}

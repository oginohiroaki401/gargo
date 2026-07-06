use gargo::command::registry::{CommandContext, CommandEffect, CommandRegistry, register_builtins};
use gargo::core::editor::Editor;
use gargo::input::action::{Action, AppAction, ProjectAction};

/// The "Git Push" command must route to the async `ProjectAction::GitPush`
/// rather than running `git push` inline — pushing blocks on the network and
/// would otherwise freeze the editor.
#[test]
fn git_push_command_dispatches_background_push_action() {
    let mut registry = CommandRegistry::new();
    register_builtins(&mut registry);

    let idx = registry
        .commands()
        .iter()
        .position(|command| command.id == "git.push")
        .expect("git.push command should be registered");

    let editor = Editor::new();
    let ctx = CommandContext::new(&editor);
    match (registry.commands()[idx].action)(&ctx) {
        CommandEffect::Action(Action::App(AppAction::Project(ProjectAction::GitPush))) => {}
        CommandEffect::Action(other) => panic!("expected GitPush action, got {:?}", other),
        CommandEffect::Message(message) => {
            panic!("expected GitPush action, got message {message:?}")
        }
        CommandEffect::None => panic!("expected GitPush action, got no effect"),
    }
}

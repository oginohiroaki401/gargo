// Modules shared by the terminal build and the wasm (browser) build.
pub mod core;
pub mod input;
pub mod ui; // gated internally: only `ui::text` is available on wasm

// Native-only: terminal UI, servers, git, plugins, CLI, updater.
#[cfg(not(target_arch = "wasm32"))]
pub mod app;
#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod diff_render;
#[cfg(not(target_arch = "wasm32"))]
pub mod log;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugin;
#[cfg(not(target_arch = "wasm32"))]
pub mod project;
#[cfg(not(target_arch = "wasm32"))]
pub mod split_render;
#[cfg(not(target_arch = "wasm32"))]
pub mod terminal;
#[cfg(not(target_arch = "wasm32"))]
pub mod upgrade;

// `command` and `syntax` are reached into by `core`; on wasm they are replaced
// by minimal stubs (no gix/tokio/axum, no tree-sitter).
#[cfg(not(target_arch = "wasm32"))]
pub mod command;
#[cfg(target_arch = "wasm32")]
#[path = "wasm_stubs/command.rs"]
pub mod command;

#[cfg(not(target_arch = "wasm32"))]
pub mod syntax;
#[cfg(target_arch = "wasm32")]
#[path = "wasm_stubs/syntax.rs"]
pub mod syntax;

// Browser editor bindings.
#[cfg(target_arch = "wasm32")]
pub mod wasm;

//! Externalized HTML page templates (loaded via `include_str!`).

/// Side-by-side ("split view") page template. Server-rendered: the body
/// is built in Rust and embedded inline, no XHR. Keyboard handling
/// (`j`/`k`/`n`/`p`/`gg`/`G`/`q`) lives in the shared `SHORTCUTS_JS`.
pub(crate) const SPLIT_HTML_TEMPLATE: &str = include_str!("../../../assets/diff_server/split.html");

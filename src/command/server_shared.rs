pub const SHARED_CSS: &str = include_str!("../../assets/server_shared.css");
pub const SHORTCUTS_JS: &str = include_str!("../../assets/server_shortcuts.js");

/// Cache-busting stamp for the externalized shared assets. It bumps with each
/// release — the only time these bytes can actually change — so the long-lived
/// `immutable` cache on `/assets/server-shared.*` is invalidated exactly then.
pub const ASSET_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `<link>` to the externalized shared stylesheet. Replaces inlining ~10KB of
/// CSS into every page; the browser fetches it once and reuses it across every
/// navigation instead of re-parsing it on each full page load.
pub fn shared_css_link() -> String {
    format!(r#"<link rel="stylesheet" href="/assets/server-shared.css?v={ASSET_VERSION}">"#)
}

/// `<script>` tag for the externalized shared keyboard-shortcuts module.
/// `defer` runs it after the document is parsed (it only attaches listeners and
/// reads `<body data-page>`), matching the old inline-in-`<body>` behaviour.
pub fn shortcuts_js_tag() -> String {
    format!(r#"<script src="/assets/server-shortcuts.js?v={ASSET_VERSION}" defer></script>"#)
}

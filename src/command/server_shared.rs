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

/// `<script>` that publishes `window.__GARGO_REPO_CTX__` for the page's inline
/// JS and the shared shortcuts module. Beyond owner/repo/branch it now carries
/// `githubBase` (the remote https URL, or null) and `defaultBranch` (main /
/// master, or null) so the open-actions pills can build "open on GitHub" and
/// "open on GitHub (default branch)" links entirely client-side.
pub fn repo_ctx_script(
    owner: &str,
    repo: &str,
    branch: &str,
    github_base: Option<&str>,
    default_branch: Option<&str>,
) -> String {
    use crate::command::gargo_preview_server::html_escape;
    let js_str = |v: Option<&str>| match v {
        Some(s) => format!(r#""{}""#, html_escape(s)),
        None => "null".to_string(),
    };
    format!(
        r#"<script>window.__GARGO_REPO_CTX__ = {{ owner: "{owner}", repo: "{repo}", branch: "{branch}", githubBase: {gh}, defaultBranch: {def} }};</script>"#,
        owner = html_escape(owner),
        repo = html_escape(repo),
        branch = html_escape(branch),
        gh = js_str(github_base),
        def = js_str(default_branch),
    )
}

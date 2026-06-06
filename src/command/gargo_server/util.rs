//! Small helpers + page-specific CSS for the gargo server pages.



use crate::command::gargo_preview_server::{
    self,
};

pub(crate) fn parse_commit_hash(hash: &str) -> Option<String> {
    if hash.is_empty() || hash.len() > 64 {
        return None;
    }
    if hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hash.to_string())
    } else {
        None
    }
}

pub(crate) fn normalize_api_path(path: &str) -> String {
    let normalized = gargo_preview_server::normalize_rel_path_for_compare(path);
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

pub(crate) fn app_css() -> String {
    // Shared chrome is linked as a cacheable asset; only the page-specific
    // rules stay inline. `APP_CSS_PAGE_SPECIFIC` carries its own `<style>` open
    // tag and trailing `</style>`.
    format!(
        "{}\n<style>{}",
        crate::command::server_shared::shared_css_link(),
        APP_CSS_PAGE_SPECIFIC,
    )
}

pub(crate) fn shortcuts_script() -> String {
    crate::command::server_shared::shortcuts_js_tag()
}

const APP_CSS_PAGE_SPECIFIC: &str = r#"
a { color: #0969da; text-decoration: none; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace; padding: 2px 6px; background: #f6f8fa; border: 1px solid #d0d7de; border-radius: 4px; }
.loading, .empty { padding: 16px; color: #57606a; font-size: 13px; }
.section { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; padding: 16px; margin-bottom: 16px; }
.section h2 { margin: 0 0 12px 0; font-size: 16px; }

/* Commits list */
.commits-main { max-width: 960px; }
.commits-section { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; overflow: hidden; }
.commits-title { margin: 0; padding: 12px 16px; font-size: 16px; border-bottom: 1px solid #d0d7de; background: #f6f8fa; }
.commit-list { list-style: none; padding: 0; margin: 0; }
.commit-item { display: flex; align-items: center; gap: 12px; padding: 12px 16px; border-bottom: 1px solid #d8dee4; }
.commit-item:last-child { border-bottom: 0; }
.commit-main { flex: 1 1 auto; min-width: 0; }
.commit-subject { display: block; color: #24292f; font-weight: 600; font-size: 14px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.commit-subject:hover { color: #0969da; text-decoration: underline; }
.commit-meta { display: flex; align-items: center; gap: 6px; margin-top: 4px; font-size: 12px; color: #57606a; }
.commit-author { color: #24292f; font-weight: 500; }
.commit-dot { color: #8c959f; }
.commit-hash { flex-shrink: 0; }
.commit-hash code { font-size: 12px; }

/* Commit detail summary */
.commit-summary .commit-title { margin: 0 0 8px 0; font-size: 20px; font-weight: 600; color: #24292f; }
.commit-summary .commit-body { margin: 0 0 12px 0; padding: 12px; background: #f6f8fa; border: 1px solid #d8dee4; border-radius: 6px; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 12px; white-space: pre-wrap; word-wrap: break-word; color: #1f2328; }
.commit-summary .commit-byline { display: flex; align-items: center; flex-wrap: wrap; gap: 6px; font-size: 13px; color: #57606a; }
.commit-summary .commit-byline .commit-author strong { color: #24292f; }
.commit-summary .commit-hash code { font-size: 12px; }

/* File list in sidebar */
.file-list { list-style: none; margin: 0; padding: 0; }
.file-list li { margin: 2px 0; }
.file-list a { display: flex; align-items: center; gap: 6px; color: #0969da; text-decoration: none; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 12px; padding: 4px 6px; border-radius: 4px; }
.file-list a:hover { background: #f6f8fa; text-decoration: underline; }
.file-list .file-path-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; min-width: 0; }
.file-status { display: inline-block; width: 1.2em; text-align: center; font-weight: 700; flex: 0 0 1.2em; font-size: 11px; }
.file-status.gr-status-added { color: #1a7f37; }
.file-status.gr-status-modified { color: #9a6700; }
.file-status.gr-status-deleted { color: #cf222e; }
.file-status.gr-status-renamed { color: #0969da; }
.file-status.gr-status-untracked { color: #57606a; }

/* Diff file cards (compatible with .gr-diff-body styles from render_diff_styles) */
.gr-file { background: #fff; border: 1px solid #d0d7de; border-radius: 6px; margin-bottom: 12px; scroll-margin-top: calc(var(--app-rail-height, 46px) + 12px); }
.gr-file-header { display: flex; align-items: center; gap: 8px; padding: 8px 12px; background: #f6f8fa; border-bottom: 1px solid #d0d7de; border-top-left-radius: 6px; border-top-right-radius: 6px; position: sticky; top: var(--app-rail-height, 46px); z-index: 5; }
.gr-file-name-wrapper { flex: 1 1 auto; min-width: 0; display: flex; align-items: center; gap: 8px; overflow: hidden; }
.gr-file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 13px; }
.gr-status-tag { flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; text-transform: lowercase; }
.gr-status-tag.gr-status-modified  { background: #fff8c5; color: #9a6700; }
.gr-status-tag.gr-status-added     { background: #dafbe1; color: #1a7f37; }
.gr-status-tag.gr-status-deleted   { background: #ffebe9; color: #cf222e; }
.gr-status-tag.gr-status-renamed   { background: #ddf4ff; color: #0969da; }
.gr-status-tag.gr-status-untracked { background: #eaeef2; color: #57606a; }
.gr-file-body { background: #fff; }
.gr-file-body .loading, .gr-file-body .empty { padding: 12px; color: #57606a; font-size: 12px; }
.gr-file-collapsed .gr-file-body { display: none; }
.gr-file-collapsed .gr-file-header { border-bottom: none; border-bottom-left-radius: 6px; border-bottom-right-radius: 6px; }
.diff-toggle-btn { flex-shrink: 0; cursor: pointer; width: 22px; height: 22px; padding: 0; line-height: 1; border: 1px solid #d0d7de; border-radius: 4px; background: #fff; color: #57606a; font-size: 11px; }
.diff-toggle-btn:hover { background: #eef2f7; }
.gr-file-stats { flex-shrink: 0; display: inline-flex; gap: 8px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }
.gr-additions { color: #1a7f37; }
.gr-deletions { color: #cf222e; }
.gr-large-tag { flex-shrink: 0; padding: 1px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; background: #fff1e5; color: #bc4c00; }
.gr-collapsed-note { padding: 12px; color: #57606a; font-size: 12px; display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.gr-collapsed-note button { cursor: pointer; border: 1px solid #d0d7de; border-radius: 4px; background: #f6f8fa; padding: 3px 10px; font-size: 12px; color: #24292f; }
.gr-collapsed-note button:hover { background: #eef2f7; }
#go-top-btn { position: fixed; right: 20px; bottom: 20px; z-index: 1000; padding: 8px 12px; border: 1px solid #ccc; border-radius: 8px; background: #fff; color: #24292f; font-size: 14px; cursor: pointer; opacity: 0; pointer-events: none; transform: translateY(8px); transition: opacity 0.15s ease, transform 0.15s ease; }
#go-top-btn.visible { opacity: 1; pointer-events: auto; transform: translateY(0); }
#go-top-btn:hover { background: #eef2f7; }
</style>"#;


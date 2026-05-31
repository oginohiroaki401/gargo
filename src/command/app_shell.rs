//! Shared chrome (left navigation rail) used by every gargo HTTP UI page.
//!
//! The rail replaces the old `.repo-header` card so the same component is
//! reused across status, compare, code, directory and commit-detail pages.
//! Callers render it into a `{{APP_RAIL}}` template slot that sits inside
//! `<div class="app-shell"> … <main class="app-main"> …`.

// Wired into the templates in the next commit; allow until then so this can
// land as its own reviewable change.
#![allow(dead_code)]

use crate::command::github_preview_server::{
    RepoUrlContext, commits_url, html_escape, repo_home_url,
};

/// Which tab in the rail to highlight as the current location.
///
/// The string maps directly onto the `id` checked in the rail template:
/// `"code"`, `"status"`, `"branches"`, or `"commits"`. Any other value
/// simply leaves no tab highlighted.
pub(crate) fn app_rail_html(
    ctx: &RepoUrlContext,
    repo_url: Option<&str>,
    active_tab: &str,
) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(r#"<aside class="app-rail">"#);

    // Repo identity. Owner stays muted so the eye lands on the repo name.
    out.push_str(r#"<div class="app-rail-repo">"#);
    out.push_str(&format!(
        r#"<span class="repo-owner">{owner}/</span><a href="{home}">{repo}</a>"#,
        owner = html_escape(&ctx.owner),
        repo = html_escape(&ctx.repo),
        home = html_escape(&repo_home_url(ctx)),
    ));
    out.push_str("</div>");

    if !ctx.branch.is_empty() {
        out.push_str(&format!(
            r#"<div><span class="app-rail-branch" title="{branch}">{branch}</span></div>"#,
            branch = html_escape(&ctx.branch),
        ));
    }

    out.push_str(r#"<nav class="app-rail-nav" aria-label="Repository views">"#);
    out.push_str(&rail_link("code", "Code", &repo_home_url(ctx), active_tab));
    out.push_str(&rail_link("status", "Status", "/status", active_tab));
    out.push_str(&rail_link("branches", "Branches", "/branches", active_tab));
    out.push_str(&rail_link(
        "commits",
        "Commits",
        &commits_url(ctx),
        active_tab,
    ));
    out.push_str("</nav>");

    out.push_str(r#"<div class="app-rail-spacer"></div>"#);

    if let Some(url) = repo_url {
        out.push_str(&format!(
            r#"<a class="app-rail-github" href="{url}" target="_blank" rel="noopener">↗ View on GitHub</a>"#,
            url = html_escape(url),
        ));
    }

    out.push_str("</aside>");
    out
}

fn rail_link(id: &str, label: &str, href: &str, active: &str) -> String {
    let class = if id == active {
        "app-rail-link app-rail-link-active"
    } else {
        "app-rail-link"
    };
    format!(
        r#"<a class="{class}" href="{href}">{label}</a>"#,
        class = class,
        href = html_escape(href),
        label = html_escape(label),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::github_preview_server::RepoUrlContext;

    fn ctx() -> RepoUrlContext {
        RepoUrlContext {
            owner: "aplio".to_string(),
            repo: "gargo".to_string(),
            branch: "master".to_string(),
        }
    }

    #[test]
    fn highlights_active_tab() {
        let html = app_rail_html(&ctx(), None, "status");
        assert!(html.contains(r#"href="/status""#));
        // The Status link should carry the active modifier, Code should not.
        assert!(html.contains(r#"app-rail-link app-rail-link-active" href="/status""#));
        assert!(html.contains(r#"class="app-rail-link" href="/aplio/gargo">Code</a>"#));
    }

    #[test]
    fn shows_branch_chip_when_branch_known() {
        let html = app_rail_html(&ctx(), None, "code");
        assert!(html.contains(r#"<span class="app-rail-branch" title="master">master</span>"#));
    }

    #[test]
    fn renders_github_link_only_when_remote_known() {
        let with_remote = app_rail_html(&ctx(), Some("https://github.com/aplio/gargo"), "code");
        assert!(with_remote.contains("View on GitHub"));
        let without = app_rail_html(&ctx(), None, "code");
        assert!(!without.contains("View on GitHub"));
    }
}

//! Commit-history HTML pages (commits list + commit detail).
//! Templates stay inline (they use `format!`, not `.replace()`).

use super::*;
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, State},
    response::{Html, IntoResponse},
};

use crate::command::gargo_preview_server::{
    self,
};
use crate::diff_render::render_diff_styles;

pub(crate) async fn handle_commits_html(State(state): State<Arc<GargoServerState>>) -> impl IntoResponse {
    // Memoized + concurrent: avoids two serial cold git spawns per page load.
    let (repo_url, default_branch) = tokio::join!(
        gargo_preview_server::cached_github_repo_url(&state.repo_root),
        gargo_preview_server::cached_default_branch_name(&state.repo_root),
    );
    let github_href = repo_url
        .as_deref()
        .map(|base| format!("{base}/commits/{}", state.url_ctx.branch));
    let rail =
        crate::command::app_shell::app_rail_html(&state.url_ctx, github_href.as_deref(), "commits");
    let commit_prefix = gargo_preview_server::commit_url(&state.url_ctx, "");
    let repo_ctx = crate::command::server_shared::repo_ctx_script(
        &state.url_ctx.owner,
        &state.url_ctx.repo,
        &state.url_ctx.branch,
        repo_url.as_deref(),
        default_branch.as_deref(),
    );
    Html(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Commits</title>{css}</head><body data-page="commits">{repo_ctx}{shortcuts}<div class="app-shell">{rail}<main class="app-main"><main class="commits-main"><section class="commits-section"><h1 class="commits-title">Commits</h1><div id="commits"><div class="loading">Loading commits...</div></div></section></main><script>
fetch('/api/commits', {{cache:'no-store'}}).then(r=>r.json()).then(data=>{{
 const list = data.commits || [];
 const root = document.getElementById('commits');
 if (!list.length) {{ root.innerHTML = '<div class="empty">No commits</div>'; return; }}
 root.innerHTML = '<ul class="commit-list">' + list.map(c => {{
   const subject = String(c.message || '').split('\n')[0];
   const detailHref = '{commit_prefix}' + c.full_hash;
   return `<li class="commit-item"><div class="commit-main"><a class="commit-subject" href="${{detailHref}}">${{escapeHtml(subject)}}</a><div class="commit-meta"><span class="commit-author">${{escapeHtml(c.author)}}</span><span class="commit-dot">·</span><span class="commit-date">${{escapeHtml(c.date)}}</span></div></div>${{window.gargoOpenCommitActionsHtml({{ fullHash: c.full_hash, detailHref: detailHref }})}}<a class="commit-hash" href="${{detailHref}}" title="${{escapeHtml(c.full_hash)}}"><code>${{escapeHtml(c.hash)}}</code></a></li>`;
 }}).join('') + '</ul>';
}});
function escapeHtml(s) {{ return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c])); }}
</script></main></div></body></html>"#,
        css = app_css(),
        rail = rail,
        commit_prefix = commit_prefix,
        repo_ctx = repo_ctx,
        shortcuts = shortcuts_script(),
    ))
}

pub(crate) async fn handle_commit_html(
    State(state): State<Arc<GargoServerState>>,
    AxumPath((_owner, _repo, hash)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    let hash = gargo_preview_server::html_escape(&hash);
    // Memoized + concurrent: avoids two serial cold git spawns per page load.
    let (repo_url, default_branch) = tokio::join!(
        gargo_preview_server::cached_github_repo_url(&state.repo_root),
        gargo_preview_server::cached_default_branch_name(&state.repo_root),
    );
    let github_href = repo_url
        .as_deref()
        .map(|base| format!("{base}/commit/{hash}"));
    let rail =
        crate::command::app_shell::app_rail_html(&state.url_ctx, github_href.as_deref(), "commits");
    let commit_prefix = gargo_preview_server::commit_url(&state.url_ctx, "");
    let repo_ctx = crate::command::server_shared::repo_ctx_script(
        &state.url_ctx.owner,
        &state.url_ctx.repo,
        &state.url_ctx.branch,
        repo_url.as_deref(),
        default_branch.as_deref(),
    );
    let diff_styles = render_diff_styles();
    Html(format!(
        r##"<!doctype html><html><head><meta charset="utf-8"><title>Commit {hash}</title>{css}<style>{diff_styles}</style></head><body data-page="commit-detail">{repo_ctx}{shortcuts}<div class="app-shell">{rail}<main class="app-main">
<section class="commit-summary section"><div id="commit-summary"><div class="loading">Loading commit...</div></div></section>
<div class="layout">
 <aside class="sidebar">
  <section class="section files-section"><h2 id="files-heading">Files</h2><div id="files-list"><div class="loading">Loading files...</div></div></section>
 </aside>
 <main class="content"><div id="files-main"><div class="loading">Loading files...</div></div></main>
</div>
<button id="go-top-btn" type="button" aria-label="Go to top">Go top</button>
<script>
const hash = "{hash}";
const summaryEl = document.getElementById('commit-summary');
const filesListEl = document.getElementById('files-list');
const filesMainEl = document.getElementById('files-main');
const filesHeadingEl = document.getElementById('files-heading');

function escapeHtml(s) {{ return String(s).replace(/[&<>"']/g, c => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c])); }}
function statusToText(s) {{
  switch ((s || '').toUpperCase()) {{
    case 'A': return 'added';
    case 'D': return 'deleted';
    case 'R': return 'renamed';
    case 'C': return 'renamed';
    case 'M': default: return 'modified';
  }}
}}
function statusBadgeChar(s) {{
  switch (s) {{
    case 'added': return 'A';
    case 'deleted': return 'D';
    case 'renamed': return 'R';
    case 'untracked': return '?';
    default: return 'M';
  }}
}}
function fileAnchorFor(path) {{ return 'f-' + path.replace(/[^A-Za-z0-9_-]/g, '_'); }}
// Diffs with at least this many changed lines are collapsed by default and
// not fetched until expanded, so a commit touching huge files stays light.
const HUGE_DIFF_LINES = 1000;

function renderSummary(data) {{
  const message = String(data.message || '');
  const lines = message.split('\n');
  const subject = lines[0] || hash;
  const body = lines.slice(1).join('\n').replace(/^\n+/, '');
  const author = data.author || '';
  const email = data.author_email || '';
  const date = data.date || '';
  const fullHash = data.full_hash || hash;
  summaryEl.innerHTML = `
    <h1 class="commit-title">${{escapeHtml(subject)}}</h1>
    ${{body ? `<pre class="commit-body">${{escapeHtml(body)}}</pre>` : ''}}
    <div class="commit-byline">
      <span class="commit-author"><strong>${{escapeHtml(author)}}</strong>${{email ? ` &lt;${{escapeHtml(email)}}&gt;` : ''}}</span>
      <span class="commit-dot">·</span>
      <span class="commit-date">committed ${{escapeHtml(date)}}</span>
      <span class="commit-dot">·</span>
      <a class="commit-hash" href="{commit_prefix}${{escapeHtml(fullHash)}}"><code>${{escapeHtml((fullHash || '').slice(0, 7))}}</code></a>
    </div>`;
}}

function renderSidebar(files) {{
  filesHeadingEl.textContent = `Files (${{files.length}})`;
  if (!files.length) {{ filesListEl.innerHTML = '<div class="empty">No files changed</div>'; return; }}
  filesListEl.innerHTML = '<ul class="file-list">' + files.map(f => {{
    const status = statusToText(f.status);
    const badge = statusBadgeChar(status);
    return `<li><a href="#${{fileAnchorFor(f.path)}}"><span class="file-status gr-status-${{status}}">${{badge}}</span><span class="file-path-text" title="${{escapeHtml(f.path)}}">${{escapeHtml(f.path)}}</span></a></li>`;
  }}).join('') + '</ul>';
}}

function renderMain(files, statsByPath) {{
  if (!files.length) {{ filesMainEl.innerHTML = '<div class="empty">No files changed</div>'; return; }}
  filesMainEl.innerHTML = files.map(f => {{
    const status = statusToText(f.status);
    const anchor = fileAnchorFor(f.path);
    const st = statsByPath[f.path] || {{}};
    const adds = st.additions || 0;
    const dels = st.deletions || 0;
    const changed = adds + dels;
    const huge = changed >= HUGE_DIFF_LINES;
    const sectionCls = huge ? 'gr-file gr-file-collapsed' : 'gr-file';
    const toggleChar = huge ? '▸' : '▾';
    const largeTag = huge ? '<span class="gr-large-tag" title="Large diff — collapsed by default to keep the page light">large diff</span>' : '';
    const bodyInner = huge
      ? `<div class="gr-collapsed-note"><span>Large diff (${{changed}} changed lines) collapsed to keep the page light.</span><button type="button" class="gr-load-btn">Show diff</button></div>`
      : '<div class="loading">Loading diff...</div>';
    return `<section class="${{sectionCls}}" id="${{anchor}}" data-path="${{escapeHtml(f.path)}}">`
      + `<div class="gr-file-header">`
      + `<button type="button" class="diff-toggle-btn" aria-label="Toggle diff" aria-expanded="${{huge ? 'false' : 'true'}}">${{toggleChar}}</button>`
      + `<div class="gr-file-name-wrapper"><span class="gr-status-tag gr-status-${{status}}">${{status}}</span><span class="gr-file-name" title="${{escapeHtml(f.path)}}">${{escapeHtml(f.path)}}</span>${{largeTag}}</div>`
      + `<span class="gr-file-stats"><span class="gr-additions">+${{adds}}</span><span class="gr-deletions">-${{dels}}</span></span>`
      + window.gargoOpenActionsHtml({{ path: f.path, ghRef: hash }})
      + `</div>`
      + `<div class="gr-file-body" data-path="${{escapeHtml(f.path)}}">${{bodyInner}}</div>`
      + `</section>`;
  }}).join('');
  for (const section of filesMainEl.querySelectorAll('section.gr-file')) {{
    const body = section.querySelector('.gr-file-body');
    const toggleBtn = section.querySelector('.diff-toggle-btn');
    if (!body || !toggleBtn) continue;
    const loadDiff = () => {{
      if (body.dataset.loaded || body.dataset.loading) return;
      body.dataset.loading = '1';
      body.innerHTML = '<div class="loading">Loading diff...</div>';
      fetch(`/api/commit/${{hash}}/file?path=${{encodeURIComponent(body.dataset.path)}}`, {{cache:'no-store'}})
        .then(r => r.json())
        .then(file => {{ body.innerHTML = file.html || ''; body.dataset.loaded = '1'; }})
        .catch(e => {{ body.innerHTML = `<div class="loading">Error: ${{escapeHtml(e.message)}}</div>`; }})
        .finally(() => {{ delete body.dataset.loading; }});
    }};
    const setCollapsed = (collapsed) => {{
      section.classList.toggle('gr-file-collapsed', collapsed);
      toggleBtn.textContent = collapsed ? '▸' : '▾';
      toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
      if (!collapsed) loadDiff();
    }};
    toggleBtn.addEventListener('click', () => {{
      setCollapsed(!section.classList.contains('gr-file-collapsed'));
    }});
    body.addEventListener('click', (e) => {{
      if (e.target && e.target.classList.contains('gr-load-btn')) setCollapsed(false);
    }});
    if (!section.classList.contains('gr-file-collapsed')) loadDiff();
  }}
}}

fetch(`/api/commit/${{hash}}`, {{cache:'no-store'}}).then(r=>r.json()).then(data=>{{
  renderSummary(data);
  const files = data.files || [];
  const statsByPath = {{}};
  for (const df of (data.diff_files || [])) {{ if (df && df.path) statsByPath[df.path] = df; }}
  renderSidebar(files);
  renderMain(files, statsByPath);
}}).catch(e=>{{ summaryEl.innerHTML = `<div class="loading">Error: ${{escapeHtml(e.message)}}</div>`; }});

const goTopButton = document.getElementById('go-top-btn');
const GO_TOP_SHOW_SCROLL_Y = 240;
function updateGoTopButtonVisibility() {{
  if (window.scrollY > GO_TOP_SHOW_SCROLL_Y) goTopButton.classList.add('visible');
  else goTopButton.classList.remove('visible');
}}
goTopButton.addEventListener('click', () => {{ window.scrollTo({{ top: 0, behavior: 'smooth' }}); }});
window.addEventListener('scroll', updateGoTopButtonVisibility, {{ passive: true }});
updateGoTopButtonVisibility();
</script></main></div></body></html>"##,
        css = app_css(),
        diff_styles = diff_styles,
        rail = rail,
        hash = hash,
        commit_prefix = commit_prefix,
        repo_ctx = repo_ctx,
        shortcuts = shortcuts_script(),
    ))
}

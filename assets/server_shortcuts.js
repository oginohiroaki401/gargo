/* Keyboard shortcuts shared by every gargo HTTP UI page.
 *
 * Loaded into each template via the {{SHORTCUTS_JS}} placeholder. Page identity
 * comes from <body data-page="..."> so this single module can serve status,
 * compare, code (tree/blob), commits, and commit-detail. The script never owns
 * application state — it dispatches clicks on the existing buttons / links so
 * the per-page handlers keep doing the persistence + UI updates they already do.
 */
(function () {
    if (window.__gargoShortcutsLoaded) return;
    window.__gargoShortcutsLoaded = true;

    const PAGE = (document.body && document.body.dataset && document.body.dataset.page) || "";
    const LEADER_TIMEOUT_MS = 1200;

    // --- open-actions pills (shared markup) -----------------------------
    //
    // One source of truth for the inline "open this file" pill cluster so the
    // JS-rendered diff rows (status / compare / commit) and the commits list
    // emit exactly the markup, classes and link targets that the server-rendered
    // toolbars (app_shell::open_actions_html) produce. Repo context — owner,
    // repo, branch, the GitHub remote base and the default branch — is published
    // by the page via window.__GARGO_REPO_CTX__.

    function encodePath(p) {
        return String(p).split("/").map(encodeURIComponent).join("/");
    }

    function pill(cls, href, newTab, title, label) {
        const tgt = newTab ? ' target="_blank" rel="noopener"' : "";
        return '<a class="oa ' + cls + '" href="' + escapeHtml(href) + '"' + tgt
            + ' title="' + escapeHtml(title) + '">' + escapeHtml(label) + "</a>";
    }

    // Pills to open a file: current tab + new tab (gargo file view), GitHub at a
    // ref (commit sha on the commit page, else the current branch), GitHub at the
    // default branch, and the gargo editor. `opts.path` is repo-relative;
    // `opts.ghRef` overrides the GitHub ref (the commit page passes its sha).
    window.gargoOpenActionsHtml = function (opts) {
        opts = opts || {};
        const ctx = window.__GARGO_REPO_CTX__ || {};
        const enc = encodePath(opts.path || "");
        const blob = "/" + encodeURIComponent(ctx.owner || "") + "/"
            + encodeURIComponent(ctx.repo || "") + "/blob/"
            + encodeURIComponent(ctx.branch || "") + "/" + enc;
        const editor = "/editor/" + enc;
        const ghBase = ctx.githubBase;
        const ghRef = encodePath(opts.ghRef || ctx.branch || "");
        let out = '<span class="open-actions">';
        out += pill("oa-tab", blob, false, "Open in current tab", "Tab");
        out += pill("oa-new", blob, true, "Open in new tab", "New");
        if (ghBase) {
            out += pill("oa-gh", ghBase + "/blob/" + ghRef + "/" + enc, true, "Open on GitHub", "GH");
            if (ctx.defaultBranch) {
                out += pill("oa-ghmain", ghBase + "/blob/" + encodePath(ctx.defaultBranch) + "/" + enc,
                    true, "Open on GitHub (" + ctx.defaultBranch + ")", "GH " + ctx.defaultBranch);
            }
        }
        out += pill("oa-editor open-in-editor", editor, true, "Open in editor", "✎");
        out += "</span>";
        return out;
    };

    // Same cluster as an element, with click-stop so a pill never toggles the
    // row's collapse state. Used by the DOM-built status / compare rows.
    window.gargoOpenActions = function (opts) {
        const tmp = document.createElement("span");
        tmp.innerHTML = window.gargoOpenActionsHtml(opts);
        const el = tmp.firstElementChild || document.createElement("span");
        el.addEventListener("click", (e) => e.stopPropagation());
        return el;
    };

    // Pills to open a commit (not a file): current/new tab → commit detail page,
    // GitHub → the commit on the remote. No editor / default-branch targets,
    // since a commit isn't a file. `.oa-*` classes match so j/k + t/r reuse them.
    window.gargoOpenCommitActionsHtml = function (opts) {
        opts = opts || {};
        const ctx = window.__GARGO_REPO_CTX__ || {};
        const detail = opts.detailHref || "#";
        const ghBase = ctx.githubBase;
        const full = opts.fullHash || "";
        let out = '<span class="open-actions">';
        out += pill("oa-tab", detail, false, "Open commit", "Tab");
        out += pill("oa-new", detail, true, "Open commit in new tab", "New");
        if (ghBase && full) {
            out += pill("oa-gh", ghBase + "/commit/" + encodeURIComponent(full),
                true, "Open commit on GitHub", "GH");
        }
        out += "</span>";
        return out;
    };

    // --- helpers --------------------------------------------------------

    function isEditable(el) {
        if (!el) return false;
        const tag = el.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return true;
        if (el.isContentEditable) return true;
        return false;
    }

    function railLinkFor(letter) {
        // Map letter → app-rail link. Reading the rail keeps URL conventions
        // server-side; this script never has to know that Status lives at /status.
        // `g e` opens the editor; it's inherently scoped out of the editor page
        // itself, which doesn't load this script.
        const map = { c: "code", s: "status", b: "branches", h: "commits", e: "editor" };
        const id = map[letter];
        if (!id) return null;
        return document.querySelector(`.app-rail-link[data-shortcut="${letter}"]`)
            || document.querySelector(`.app-rail-link[data-tab="${id}"]`);
    }

    function railHeight() {
        const rail = document.querySelector(".app-rail");
        return rail ? rail.getBoundingClientRect().height : 0;
    }

    function scrollItemIntoView(el) {
        if (!el) return;
        const rect = el.getBoundingClientRect();
        const top = rect.top + window.scrollY;
        const offset = railHeight() + 12;
        const viewportTop = window.scrollY + offset;
        const viewportBottom = window.scrollY + window.innerHeight - 24;
        if (top < viewportTop) {
            window.scrollTo({ top: top - offset, behavior: "smooth" });
        } else if (top + rect.height > viewportBottom) {
            const target = top - (window.innerHeight - rect.height) + 24;
            window.scrollTo({ top: Math.max(0, target), behavior: "smooth" });
        }
    }

    // --- focus model ----------------------------------------------------

    const ITEM_SELECTORS = {
        "status": ".gr-file",
        "compare": ".gr-file",
        "commit-detail": ".gr-file",
        "code-tree": ".file-list .file-item",
        "commits": ".commit-item",
    };

    function items() {
        const sel = ITEM_SELECTORS[PAGE];
        if (!sel) return [];
        return Array.from(document.querySelectorAll(sel));
    }

    function currentIndex(list) {
        for (let i = 0; i < list.length; i++) {
            if (list[i].classList.contains("is-focused")) return i;
        }
        return -1;
    }

    function focusAt(index) {
        const list = items();
        if (!list.length) return;
        const clamped = Math.max(0, Math.min(list.length - 1, index));
        for (const el of list) el.classList.remove("is-focused");
        list[clamped].classList.add("is-focused");
        scrollItemIntoView(list[clamped]);
    }

    function moveFocus(delta) {
        const list = items();
        if (!list.length) return;
        const cur = currentIndex(list);
        if (cur < 0) {
            focusAt(delta > 0 ? 0 : list.length - 1);
        } else {
            focusAt(cur + delta);
        }
    }

    function focusedItem() {
        const list = items();
        const cur = currentIndex(list);
        return cur >= 0 ? list[cur] : null;
    }

    // --- per-page actions ----------------------------------------------

    function toggleCollapse() {
        const el = focusedItem();
        if (!el) return;
        const btn = el.querySelector(".diff-toggle-btn");
        if (btn) btn.click();
    }

    function toggleViewed() {
        const el = focusedItem();
        if (!el) return;
        const cb = el.querySelector('.diff-viewed-label input[type="checkbox"]');
        if (!cb) return;
        cb.checked = !cb.checked;
        cb.dispatchEvent(new Event("change", { bubbles: true }));
    }

    // Stage / unstage the focused file (status page). The listing re-renders
    // after the POST resolves and the file hops sections, so we re-focus the
    // same path once its row reappears to keep `j u j u` flowing.
    function toggleStage() {
        const el = focusedItem();
        if (!el) return;
        const btn = el.querySelector(".stage-btn");
        if (!btn) return;
        const path = el.dataset.path;
        btn.click();
        // Staging changes the row's section, so its fileId changes and the
        // listing re-renders with a fresh row. Wait for the old row to detach
        // before re-focusing the same path (now in the other section).
        if (path) refocusByPathWhenReady(path, el, 40);
    }

    function refocusByPathWhenReady(path, oldEl, tries) {
        if (tries > 0 && oldEl && oldEl.isConnected) {
            setTimeout(() => refocusByPathWhenReady(path, oldEl, tries - 1), 40);
            return;
        }
        const list = items();
        const idx = list.findIndex((el) => el.dataset.path === path);
        if (idx >= 0) {
            focusAt(idx);
        } else if (tries > 0) {
            setTimeout(() => refocusByPathWhenReady(path, null, tries - 1), 40);
        }
    }

    function activateFocused() {
        const el = focusedItem();
        if (!el) return;
        const link = el.matches("a") ? el : el.querySelector("a");
        if (link) link.click();
    }

    // Open the focused file in the browser editor (new tab) by clicking its
    // existing ".open-in-editor" link, so URL/target conventions stay in one
    // place. No-op when the focused item has no such link (e.g. a directory).
    function openInEditor() {
        const el = focusedItem();
        if (!el) return;
        const link = el.querySelector(".open-in-editor");
        if (link) link.click();
    }

    // Click an open-actions pill (.oa-tab / .oa-new / .oa-gh / .oa-ghmain) inside
    // the focused item, so keybindings reuse the exact link the pill already
    // carries. Returns false when the focused item has no such pill (e.g. a
    // directory row, or .oa-ghmain when the repo has no GitHub remote).
    function clickPill(sel) {
        const el = focusedItem();
        if (!el) return false;
        const a = el.querySelector(sel);
        if (a) { a.click(); return true; }
        return false;
    }

    // --- split view actions --------------------------------------------

    // Open the focused diff in the side-by-side split view (new tab).
    // Each source page exposes the path / context we need on the focused
    // .gr-file element (status: data-section + data-path; compare: data-path
    // + base/compare from the URL; commit: data-path + hash from the URL).
    function openSplit() {
        const el = focusedItem();
        if (!el) return;
        const path = el.dataset.path
            || (el.querySelector(".gr-file-body") && el.querySelector(".gr-file-body").dataset.path);
        if (!path) return;
        let url = null;
        if (PAGE === "status") {
            const section = el.dataset.section;
            if (!section) return;
            url = `/split?source=status&section=${encodeURIComponent(section)}&path=${encodeURIComponent(path)}`;
        } else if (PAGE === "compare") {
            const params = new URLSearchParams(window.location.search);
            const base = params.get("base");
            const compare = params.get("compare");
            if (!base || !compare) return;
            url = `/split?source=compare&base=${encodeURIComponent(base)}&compare=${encodeURIComponent(compare)}&path=${encodeURIComponent(path)}`;
        } else if (PAGE === "commit-detail") {
            const m = window.location.pathname.match(/\/commit\/([0-9a-f]+)/i);
            if (!m) return;
            url = `/split?source=commit&hash=${encodeURIComponent(m[1])}&path=${encodeURIComponent(path)}`;
        }
        if (url) window.open(url, "_blank");
    }

    // Per-row scroll step for j/k on the split page. Measured once from the
    // first .sp-row; falls back to a sensible default before any row exists.
    let splitRowHeightCache = 0;
    function splitRowHeight() {
        if (splitRowHeightCache > 0) return splitRowHeightCache;
        const row = document.querySelector(".sp-row");
        if (row) {
            const h = row.getBoundingClientRect().height;
            if (h > 0) {
                splitRowHeightCache = h;
                return h;
            }
        }
        return 19; // matches font-size:12 * line-height:1.55, used until DOM is ready
    }

    // Jump to the next / previous non-context row. Picks the first row whose
    // top is strictly past (or before) the current viewport center so repeats
    // always make progress.
    function jumpToChangedRow(dir) {
        const rows = Array.from(document.querySelectorAll(".sp-row:not(.sp-context)"));
        if (!rows.length) return;
        const viewportCenter = window.scrollY + window.innerHeight / 2;
        const positions = rows.map(r => r.getBoundingClientRect().top + window.scrollY);
        let target = null;
        if (dir > 0) {
            for (let i = 0; i < positions.length; i++) {
                if (positions[i] > viewportCenter + 4) { target = rows[i]; break; }
            }
            if (!target) target = rows[rows.length - 1];
        } else {
            for (let i = positions.length - 1; i >= 0; i--) {
                if (positions[i] < viewportCenter - 4) { target = rows[i]; break; }
            }
            if (!target) target = rows[0];
        }
        if (target) target.scrollIntoView({ block: "center", behavior: "smooth" });
    }

    // --- help overlay ---------------------------------------------------

    const HELP_SECTIONS_GLOBAL = [
        { heading: "Navigation", rows: [
            ["g c", "Go to Code"],
            ["g s", "Go to Status"],
            ["g b", "Go to Branches"],
            ["g h", "Go to Commits"],
            ["g e", "Go to Editor"],
        ]},
        { heading: "General", rows: [
            ["?", "Show this help"],
            ["Esc", "Close help / cancel chord"],
        ]},
    ];

    // Shared "open the focused file" rows, appended to every file-diff page so
    // the pill keybindings are documented in one place.
    const OPEN_FILE_ROWS = [
        ["Enter", "Open focused file in current tab"],
        ["t", "Open focused file in a new tab"],
        ["r", "Open focused file on GitHub"],
        ["Shift+R", "Open focused file on GitHub (default branch)"],
        ["e", "Open focused file in editor (new tab)"],
    ];

    const HELP_SECTIONS_PAGE = {
        "status": [{ heading: "Diff (Status)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["u", "Stage / unstage focused file"],
            ["v", "Toggle Viewed"],
            ["g g / G", "Jump to first / last file"],
        ].concat(OPEN_FILE_ROWS) }],
        "compare": [{ heading: "Diff (Compare)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["v", "Toggle Viewed"],
            ["g g / G", "Jump to first / last file"],
        ].concat(OPEN_FILE_ROWS) }],
        "commit-detail": [{ heading: "Diff (Commit)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["g g / G", "Jump to first / last file"],
        ].concat(OPEN_FILE_ROWS) }],
        "split": [{ heading: "Split view", rows: [
            ["j / k", "Scroll down / up one row"],
            ["n / p", "Jump to next / previous changed row"],
            ["g g / G", "Scroll to top / bottom"],
            ["q", "Close this tab"],
        ]}],
        "code-tree": [{ heading: "Files", rows: [
            ["j / k", "Focus next / previous entry"],
            ["Enter", "Open focused entry (current tab)"],
            ["t", "Open focused file in a new tab"],
            ["r", "Open focused file on GitHub"],
            ["Shift+R", "Open focused file on GitHub (default branch)"],
            ["e", "Open focused file in editor (new tab)"],
            ["g g / G", "Jump to first / last entry"],
        ]}],
        "commits": [{ heading: "Commits", rows: [
            ["j / k", "Focus next / previous commit"],
            ["Enter", "Open focused commit (current tab)"],
            ["t", "Open focused commit in a new tab"],
            ["r", "Open focused commit on GitHub"],
            ["g g / G", "Jump to first / last commit"],
        ]}],
    };

    let overlayEl = null;

    function buildOverlay() {
        const wrap = document.createElement("div");
        wrap.className = "gargo-help-overlay";
        wrap.hidden = true;
        wrap.innerHTML = '<div class="gargo-help-panel" role="dialog" aria-label="Keyboard shortcuts">'
            + '<div class="gargo-help-header"><span>Keyboard shortcuts</span>'
            + '<button type="button" class="gargo-help-close" aria-label="Close">×</button></div>'
            + '<div class="gargo-help-body"></div></div>';
        wrap.addEventListener("click", (ev) => {
            if (ev.target === wrap) closeOverlay();
        });
        wrap.querySelector(".gargo-help-close").addEventListener("click", closeOverlay);
        document.body.appendChild(wrap);
        return wrap;
    }

    function renderOverlay() {
        if (!overlayEl) overlayEl = buildOverlay();
        const body = overlayEl.querySelector(".gargo-help-body");
        const sections = (HELP_SECTIONS_PAGE[PAGE] || []).concat(HELP_SECTIONS_GLOBAL);
        body.innerHTML = sections.map(sec => {
            const rows = sec.rows.map(([keys, desc]) =>
                `<tr><td class="gargo-help-keys"><kbd>${escapeHtml(keys)}</kbd></td><td>${escapeHtml(desc)}</td></tr>`
            ).join("");
            return `<section><h3>${escapeHtml(sec.heading)}</h3><table>${rows}</table></section>`;
        }).join("");
    }

    function openOverlay() {
        renderOverlay();
        overlayEl.hidden = false;
    }

    function closeOverlay() {
        if (overlayEl) overlayEl.hidden = true;
    }

    function overlayOpen() {
        return overlayEl && !overlayEl.hidden;
    }

    function escapeHtml(s) {
        return String(s).replace(/[&<>"']/g, c => ({
            "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
        }[c]));
    }

    // --- chord state ----------------------------------------------------

    let leader = null;          // 'g' if pending
    let leaderTimer = null;
    let lastNonChordWasG = false; // for gg double-tap

    function startLeader(letter) {
        leader = letter;
        clearTimeout(leaderTimer);
        leaderTimer = setTimeout(() => { leader = null; }, LEADER_TIMEOUT_MS);
    }

    function clearLeader() {
        leader = null;
        clearTimeout(leaderTimer);
    }

    // --- key dispatch ---------------------------------------------------

    function onKey(ev) {
        if (ev.defaultPrevented) return;
        if (ev.metaKey || ev.ctrlKey || ev.altKey) return;
        if (isEditable(ev.target)) return;

        const key = ev.key;

        if (overlayOpen()) {
            if (key === "Escape" || key === "?" ) {
                ev.preventDefault();
                closeOverlay();
            }
            return;
        }

        if (key === "Escape") {
            clearLeader();
            return;
        }

        if (key === "?") {
            ev.preventDefault();
            openOverlay();
            return;
        }

        // Leader continuation
        if (leader === "g") {
            clearLeader();
            if (key === "g") {
                ev.preventDefault();
                if (PAGE === "split") {
                    window.scrollTo({ top: 0, behavior: "smooth" });
                } else {
                    focusAt(0);
                }
                return;
            }
            const link = railLinkFor(key);
            if (link) {
                ev.preventDefault();
                link.click();
                return;
            }
            return;
        }

        if (key === "g") {
            ev.preventDefault();
            startLeader("g");
            return;
        }

        if (key === "G") {
            ev.preventDefault();
            if (PAGE === "split") {
                window.scrollTo({ top: document.documentElement.scrollHeight, behavior: "smooth" });
            } else {
                const list = items();
                if (list.length) focusAt(list.length - 1);
            }
            return;
        }

        // Page-local single keys
        if (key === "j") {
            if (PAGE === "split") {
                ev.preventDefault();
                window.scrollBy(0, splitRowHeight());
                return;
            }
            if (!ITEM_SELECTORS[PAGE]) return;
            ev.preventDefault();
            moveFocus(1);
            return;
        }
        if (key === "k") {
            if (PAGE === "split") {
                ev.preventDefault();
                window.scrollBy(0, -splitRowHeight());
                return;
            }
            if (!ITEM_SELECTORS[PAGE]) return;
            ev.preventDefault();
            moveFocus(-1);
            return;
        }

        if (PAGE === "status" || PAGE === "compare" || PAGE === "commit-detail") {
            if (key === "o") {
                ev.preventDefault();
                toggleCollapse();
                return;
            }
            if (key === "O") {
                ev.preventDefault();
                openSplit();
                return;
            }
            if (key === "v" && PAGE !== "commit-detail") {
                ev.preventDefault();
                toggleViewed();
                return;
            }
            if (key === "u" && PAGE === "status") {
                ev.preventDefault();
                toggleStage();
                return;
            }
        }

        // Open-target pills on the focused item. Works on every item-based page
        // (status / compare / commit / code tree / commits); each key clicks the
        // matching .oa-* pill, no-op when that pill isn't present.
        if (ITEM_SELECTORS[PAGE]) {
            if (key === "Enter") {
                ev.preventDefault();
                if (!clickPill(".oa-tab")) activateFocused();
                return;
            }
            if (key === "t") {
                ev.preventDefault();
                clickPill(".oa-new");
                return;
            }
            if (key === "r") {
                ev.preventDefault();
                clickPill(".oa-gh");
                return;
            }
            if (key === "R") {
                ev.preventDefault();
                clickPill(".oa-ghmain");
                return;
            }
            if (key === "e") {
                ev.preventDefault();
                openInEditor();
                return;
            }
        }

        if (PAGE === "split") {
            if (key === "n") {
                ev.preventDefault();
                jumpToChangedRow(1);
                return;
            }
            if (key === "p") {
                ev.preventDefault();
                jumpToChangedRow(-1);
                return;
            }
            if (key === "q") {
                ev.preventDefault();
                window.close();
                return;
            }
        }

    }

    window.addEventListener("keydown", onKey);

    // Publish the sticky header's height as a CSS variable so sticky sidebars
    // (status / compare) can offset themselves below it instead of being
    // hidden underneath. Re-measured on resize since the rail can wrap.
    function syncRailHeightVar() {
        const h = railHeight();
        if (h > 0) {
            document.documentElement.style.setProperty("--app-rail-height", `${Math.round(h)}px`);
        }
    }
    // This module is injected ahead of the rail markup on some pages, so the
    // rail may not exist yet at parse time — defer the first measurement until
    // the DOM is ready, then keep it in sync as the rail wraps on resize.
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", syncRailHeightVar);
    } else {
        syncRailHeightVar();
    }
    window.addEventListener("load", syncRailHeightVar);
    window.addEventListener("resize", syncRailHeightVar, { passive: true });
})();

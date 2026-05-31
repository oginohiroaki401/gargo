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
        const map = { c: "code", s: "status", b: "branches", h: "commits" };
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

    function activateFocused() {
        const el = focusedItem();
        if (!el) return;
        const link = el.matches("a") ? el : el.querySelector("a");
        if (link) link.click();
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
        ]},
        { heading: "General", rows: [
            ["?", "Show this help"],
            ["Esc", "Close help / cancel chord"],
        ]},
    ];

    const HELP_SECTIONS_PAGE = {
        "status": [{ heading: "Diff (Status)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["v", "Toggle Viewed"],
            ["g g / G", "Jump to first / last file"],
        ]}],
        "compare": [{ heading: "Diff (Compare)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["v", "Toggle Viewed"],
            ["g g / G", "Jump to first / last file"],
        ]}],
        "commit-detail": [{ heading: "Diff (Commit)", rows: [
            ["j / k", "Focus next / previous file"],
            ["o", "Expand / collapse focused file"],
            ["Shift+O", "Open focused file in split view (new tab)"],
            ["g g / G", "Jump to first / last file"],
        ]}],
        "split": [{ heading: "Split view", rows: [
            ["j / k", "Scroll down / up one row"],
            ["n / p", "Jump to next / previous changed row"],
            ["g g / G", "Scroll to top / bottom"],
            ["q", "Close this tab"],
        ]}],
        "code-tree": [{ heading: "Files", rows: [
            ["j / k", "Focus next / previous entry"],
            ["Enter", "Open focused entry"],
            ["g g / G", "Jump to first / last entry"],
        ]}],
        "commits": [{ heading: "Commits", rows: [
            ["j / k", "Focus next / previous commit"],
            ["Enter", "Open focused commit"],
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

        if (PAGE === "code-tree" || PAGE === "commits") {
            if (key === "Enter") {
                if (!focusedItem()) return;
                ev.preventDefault();
                activateFocused();
                return;
            }
        }
    }

    window.addEventListener("keydown", onKey);
})();

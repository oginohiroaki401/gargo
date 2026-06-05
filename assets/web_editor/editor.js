      // emacs/VSCode 風ブラウザエディタ PoC。
      //
      // 現行モーダルエディタ(editor.js)と同じ WASM コア・疎通(/api/file,
      // /api/save)を横引きしつつ、「常に Insert mode」「Ctrl+f/b/n/p/a/e と矢印で
      // 移動」「カーソル追従スクロール(縦+横)」だけを差し替えた単一ページ実装。
      import init, { WebEditor } from "/assets/gargo_wasm.js";

      const LINE_HEIGHT = 20;
      const OVERSCAN = 4;

      const els = {
        scroller: document.getElementById("scroller"),
        sizer: document.getElementById("sizer"),
        ime: document.getElementById("ime"),
        preedit: document.getElementById("preedit"),
        mode: document.getElementById("mode"),
        path: document.getElementById("path"),
        dirty: document.getElementById("dirty"),
        error: document.getElementById("error"),
        picker: document.getElementById("picker"),
        pickerInput: document.getElementById("picker-input"),
        pickerList: document.getElementById("picker-list"),
        gsearch: document.getElementById("gsearch"),
        gsearchInput: document.getElementById("gsearch-input"),
        gsearchStatus: document.getElementById("gsearch-status"),
        gsearchList: document.getElementById("gsearch-list"),
        gsearchClose: document.getElementById("gsearch-close"),
        main: document.getElementById("main"),
        sidebar: document.getElementById("sidebar"),
        sidebarResizer: document.getElementById("sidebar-resizer"),
        previewPane: document.getElementById("preview-pane"),
        previewFrame: document.getElementById("preview-frame"),
        previewResizer: document.getElementById("preview-resizer"),
        tree: document.getElementById("tree"),
        ctxmenu: document.getElementById("ctxmenu"),
        fsprompt: document.getElementById("fsprompt"),
        fspromptTitle: document.getElementById("fsprompt-title"),
        fspromptInput: document.getElementById("fsprompt-input"),
        find: document.getElementById("find"),
        findExpand: document.getElementById("find-expand"),
        findInput: document.getElementById("find-input"),
        findCase: document.getElementById("find-case"),
        findWord: document.getElementById("find-word"),
        findRegex: document.getElementById("find-regex"),
        findCount: document.getElementById("find-count"),
        findPrev: document.getElementById("find-prev"),
        findNext: document.getElementById("find-next"),
        findClose: document.getElementById("find-close"),
        replaceRow: document.getElementById("replace-row"),
        replaceInput: document.getElementById("replace-input"),
        replaceOne: document.getElementById("replace-one"),
        replaceAll: document.getElementById("replace-all"),
      };

      let editor = null;
      let filePath = "";
      let baseHash = ""; // hash of the content we loaded (for conflict detection)
      // Content as last loaded/saved, for the "● modified" indicator. The wasm
      // `dirty` flag stays true once you've made any edit, so it can't tell that
      // you've manually undone back to the original state — we compare the live
      // content against this baseline instead. Cached per editor version so we
      // only re-stringify the rope when the document actually changes.
      let baseContent = "";
      let modifiedVersion = -1;
      let modifiedValue = false;
      function isModified() {
        if (!editor) return false;
        const v = editor.version();
        if (v !== modifiedVersion) {
          modifiedValue = editor.content() !== baseContent;
          modifiedVersion = v;
        }
        return modifiedValue;
      }
      let charWidth = 8;
      let gutterWidth = 50;
      let lastVersion = -1;
      let composing = false;
      // The most recent render model, kept so IME composition can splice the
      // composing (pre-edit) text into the caret's row inline (see paintPreedit).
      let lastModel = null;
      let maxCols = 0; // widest row seen so far, for horizontal scroll sizing
      // Syntax highlight: spans per line (char offsets into the expanded row
      // text), computed server-side (tree-sitter) and refreshed on a debounce.
      let highlightSpans = new Map(); // lineIdx -> [{start, end, scope}]
      // Git change gutter: per-line status computed server-side (gix is
      // native-only, so the wasm core can't produce it), refreshed on a debounce.
      let gitGutter = new Map(); // lineIdx -> "added" | "modified" | "deleted"

      // Soft-wrap mode. Default from server config (window.__GARGO_WRAP__),
      // overridden per-tab via Alt+Z and remembered in localStorage. In wrap mode
      // the renderer switches to a flow layout and measures caret/selection rects
      // from the real DOM (see renderWrap); above WRAP_MAX_LINES wrap is ignored
      // (the virtualized horizontal-scroll renderer stays on) for performance.
      const WRAP_MAX_LINES = 4000;
      let wrapMode = (function () {
        let saved = null;
        try { saved = localStorage.getItem("gargo_wrap"); } catch (_) {}
        return saved !== null ? saved === "true" : window.__GARGO_WRAP__ === true;
      })();
      let renderedWrap = null; // the mode the surface DOM is currently built for
      const wrapLineEls = new Map(); // line -> {wline, wgutter, wrow}
      let wrapBuiltVersion = -1;
      let wrapRowsDirty = false; // force a row rebuild (highlight/gutter changed)
      let wrapPrimaryCaretTop = 0; // content-Y of the primary caret, for scrolling

      function showError(msg) {
        els.error.textContent = msg;
        els.error.style.display = "block";
        setTimeout(() => (els.error.style.display = "none"), 6000);
      }

      function measureCharWidth() {
        const probe = document.createElement("span");
        probe.style.cssText =
          "position:absolute;visibility:hidden;white-space:pre;font:var(--font)";
        probe.textContent = "M".repeat(100);
        els.sizer.appendChild(probe);
        charWidth = probe.getBoundingClientRect().width / 100 || 8;
        probe.remove();
        document.documentElement.style.setProperty("--char-w", charWidth + "px");
      }

      function setGutterWidth(totalLines) {
        const digits = Math.max(2, String(totalLines).length);
        gutterWidth = digits * charWidth + 16;
        document.documentElement.style.setProperty("--gutter-w", gutterWidth + "px");
      }

      function colToPx(col) {
        return gutterWidth + col * charWidth;
      }

      // Pixel width of the first `charCol` characters of a (tab-expanded) row,
      // measured with the real editor font. Columns alone can't position the
      // caret once a line mixes Latin and full-width (CJK) glyphs: the browser
      // renders e.g. Japanese narrower than 2× the Latin advance, so a
      // column×charWidth caret drifts away from the text. `char_col` from the
      // wasm model is a character offset into the expanded row, so measuring the
      // matching prefix lands the caret/selection exactly on the glyph boundary.
      let _measureCtx = null;
      function prefixPx(rowText, charCol) {
        if (!charCol) return 0;
        if (!_measureCtx) {
          _measureCtx = document.createElement("canvas").getContext("2d");
          const cs = getComputedStyle(els.scroller);
          _measureCtx.font = cs.fontSize + " " + cs.fontFamily;
        }
        const chars = Array.from(rowText || "");
        return _measureCtx.measureText(chars.slice(0, charCol).join("")).width;
      }

      // ---- rendering -------------------------------------------------------

      // Rows currently mounted in the DOM, keyed by line index.
      const mountedRows = new Map(); // line -> {gutter, row}
      const caretPool = [];
      const selPool = [];
      const matchPool = []; // in-file search match highlights

      function render() {
        if (!editor) return;
        const useWrap = wrapMode && editor.line_count() <= WRAP_MAX_LINES;
        if (useWrap !== renderedWrap) {
          resetSurface(useWrap);
          renderedWrap = useWrap;
        }
        if (useWrap) renderWrap();
        else renderVirtual();
      }

      // Tear down all dynamic surface DOM when switching wrap on/off so the two
      // renderers never collide. ensurePool/buildWrapRows recreate as needed.
      function resetSurface(useWrap) {
        for (const e of mountedRows.values()) {
          e.gutter.remove();
          e.row.remove();
        }
        mountedRows.clear();
        for (const e of wrapLineEls.values()) e.wline.remove();
        wrapLineEls.clear();
        for (const pool of [caretPool, selPool, matchPool]) {
          for (const el of pool) el.remove();
          pool.length = 0;
        }
        wrapBuiltVersion = -1;
        wrapRowsDirty = false;
        lastVersion = -1;
        maxCols = 0;
        els.sizer.classList.toggle("wrap", useWrap);
        if (useWrap) {
          els.sizer.style.height = "";
          els.sizer.style.width = "";
        }
      }

      function renderVirtual() {
        const version = editor.version();
        const total = editor.line_count();
        const scrollTop = els.scroller.scrollTop;
        const clientH = els.scroller.clientHeight;
        const top = Math.max(0, Math.floor(scrollTop / LINE_HEIGHT) - OVERSCAN);
        const visible = Math.ceil(clientH / LINE_HEIGHT) + OVERSCAN * 2;

        // Keep the sizer tall enough for the whole document.
        els.sizer.style.height = total * LINE_HEIGHT + "px";
        setGutterWidth(total);

        const model = editor.render(top, visible);
        lastModel = model;
        renderRows(model);
        renderSelections(model);
        renderMatches(model);
        renderCarets(model);
        syncStatus(model);
        positionIme(model);
        // Widen the sizer so the native horizontal scrollbar can reach the end
        // of the longest line we've laid out (plus the cursor column).
        const cursorCol = model.cursors[0] ? model.cursors[0].col : 0;
        maxCols = Math.max(maxCols, cursorCol);
        els.sizer.style.width = colToPx(maxCols + 4) + "px";
        lastVersion = version;
      }

      function renderRows(model) {
        const wanted = new Set();
        for (let i = 0; i < model.rows.length; i++) {
          const line = model.top + i;
          wanted.add(line);
          let entry = mountedRows.get(line);
          if (!entry) {
            const gutter = document.createElement("div");
            gutter.className = "gutter";
            const row = document.createElement("div");
            row.className = "row";
            els.sizer.appendChild(gutter);
            els.sizer.appendChild(row);
            entry = { gutter, row };
            mountedRows.set(line, entry);
          }
          const y = line * LINE_HEIGHT + "px";
          entry.gutter.style.top = y;
          entry.row.style.top = y;
          entry.gutter.textContent = String(line + 1);
          const gst = gitGutter.get(line);
          entry.gutter.className = "gutter" + (gst ? " git-" + gst : "");
          paintRow(entry.row, model.rows[i], highlightSpans.get(line));
          if (model.rows[i].length > maxCols) maxCols = model.rows[i].length;
        }
        // Unmount rows that scrolled out of view.
        for (const [line, entry] of mountedRows) {
          if (!wanted.has(line)) {
            entry.gutter.remove();
            entry.row.remove();
            mountedRows.delete(line);
          }
        }
      }

      function renderCarets(model) {
        ensurePool(caretPool, model.cursors.length, "caret");
        for (let i = 0; i < caretPool.length; i++) {
          const el = caretPool[i];
          if (i < model.cursors.length) {
            const c = model.cursors[i];
            el.style.display = "block";
            const cRowText = model.rows[c.row - model.top] || "";
            el.style.left = gutterWidth + prefixPx(cRowText, c.char_col) + "px";
            el.style.top = c.row * LINE_HEIGHT + "px";
            // Always-insert PoC: caret is a steady (non-blinking) line.
            el.className = "caret" + (c.primary ? " primary" : "");
          } else {
            el.style.display = "none";
          }
        }
      }

      function renderSelections(model) {
        // Expand each selection range into per-row rectangles, measured in
        // pixels (CJK-aware) the same way the caret is.
        const rects = [];
        for (const s of model.selections) {
          for (let row = s.start_row; row <= s.end_row; row++) {
            const rowText = model.rows[row - model.top] || "";
            const fromChar = row === s.start_row ? s.start_char : 0;
            const lineChars = Array.from(rowText).length;
            const toChar = row === s.end_row ? s.end_char : lineChars;
            let fromPx = prefixPx(rowText, fromChar);
            let toPx = prefixPx(rowText, toChar);
            // Selections that wrap a line end extend a little past the last
            // glyph to signal the included newline.
            if (row !== s.end_row) toPx += charWidth;
            if (toPx <= fromPx) toPx = fromPx + charWidth;
            rects.push({ row, leftPx: fromPx, widthPx: toPx - fromPx });
          }
        }
        ensurePool(selPool, rects.length, "sel");
        for (let i = 0; i < selPool.length; i++) {
          const el = selPool[i];
          if (i < rects.length) {
            const r = rects[i];
            el.style.display = "block";
            el.style.left = gutterWidth + r.leftPx + "px";
            el.style.top = r.row * LINE_HEIGHT + "px";
            el.style.width = r.widthPx + "px";
          } else {
            el.style.display = "none";
          }
        }
      }

      // Draw a translucent band over every visible search match except the
      // current one (which is the editor selection, .sel). Same CJK-aware pixel
      // measurement as renderSelections.
      function renderMatches(model) {
        const rects = [];
        if (findOpen) {
          for (let i = 0; i < searchMatches.length; i++) {
            if (i === searchIndex && findCurrentSelected) continue; // = editor selection
            const m = searchMatches[i];
            if (m.row < model.top || m.row >= model.top + model.rows.length) continue;
            const rowText = model.rows[m.row - model.top] || "";
            const fromPx = prefixPx(rowText, m.start_char);
            let toPx = prefixPx(rowText, m.end_char);
            if (toPx <= fromPx) toPx = fromPx + charWidth;
            rects.push({ row: m.row, leftPx: fromPx, widthPx: toPx - fromPx });
          }
        }
        ensurePool(matchPool, rects.length, "match-hl");
        for (let i = 0; i < matchPool.length; i++) {
          const el = matchPool[i];
          if (i < rects.length) {
            const r = rects[i];
            el.style.display = "block";
            el.style.left = gutterWidth + r.leftPx + "px";
            el.style.top = r.row * LINE_HEIGHT + "px";
            el.style.width = r.widthPx + "px";
          } else {
            el.style.display = "none";
          }
        }
      }

      function ensurePool(pool, n, cls) {
        while (pool.length < n) {
          const el = document.createElement("div");
          el.className = cls;
          els.sizer.appendChild(el);
          pool.push(el);
        }
      }

      // Find clickable links in a line: markdown `[label](target)` (any target,
      // url or relative path) and bare http(s) URLs. Works in any file type.
      // Indices are string offsets, which equal character offsets for non-astral
      // text (matches how syntax spans are indexed). Returns [{start,end,href}].
      function detectLinks(text) {
        const ranges = [];
        let m;
        const md = /\[[^\]]*\]\(([^)\s]+)[^)]*\)/g;
        while ((m = md.exec(text))) {
          ranges.push({ start: m.index, end: m.index + m[0].length, href: m[1] });
        }
        const url = /(https?:\/\/[^\s)>\]"'`]+)/g;
        while ((m = url.exec(text))) {
          const s = m.index;
          const e = s + m[1].length;
          if (ranges.some((r) => s >= r.start && s < r.end)) continue; // inside a md link
          ranges.push({ start: s, end: e, href: m[1] });
        }
        return ranges;
      }

      // The syntax scope covering segment [a,b), or "" — first span wins on
      // overlap, matching the old clamp behaviour.
      function scopeAt(spans, a, b) {
        if (!spans) return "";
        for (const s of spans) if (s.start <= a && s.end >= b) return s.scope;
        return "";
      }

      // The link href covering segment [a,b), or null.
      function hrefAt(links, a, b) {
        for (const l of links) if (l.start <= a && l.end >= b) return l.href;
        return null;
      }

      function escapeAttr(s) {
        return s.replace(/"/g, "&quot;").replace(/&/g, "&amp;");
      }

      // Paint one row: syntax coloring (`spans`: {start,end,scope} char ranges)
      // plus clickable link wrappers. Splits the line at every span/link boundary
      // and wraps each segment in the matching `tok-*` span and/or `.elink` anchor.
      function paintRow(rowEl, text, spans) {
        const links = detectLinks(text);
        if ((!spans || !spans.length) && !links.length) {
          rowEl.textContent = text; // fast path: nothing to wrap
          return;
        }
        const chars = Array.from(text);
        const n = chars.length;
        const bset = new Set([0, n]);
        if (spans) for (const s of spans) { bset.add(Math.min(s.start, n)); bset.add(Math.min(s.end, n)); }
        for (const l of links) { bset.add(Math.min(l.start, n)); bset.add(Math.min(l.end, n)); }
        const points = [...bset].filter((p) => p >= 0 && p <= n).sort((x, y) => x - y);
        let html = "";
        for (let i = 0; i < points.length - 1; i++) {
          const a = points[i];
          const b = points[i + 1];
          if (a >= b) continue;
          let piece = escapeHtml(chars.slice(a, b).join(""));
          const scope = scopeAt(spans, a, b);
          if (scope) piece = '<span class="tok-' + scope + '">' + piece + "</span>";
          const href = hrefAt(links, a, b);
          if (href) piece = '<a class="elink" data-href="' + escapeAttr(href) + '">' + piece + "</a>";
          html += piece;
        }
        rowEl.innerHTML = html;
      }

      // ---- syntax highlight (server-side tree-sitter, debounced) -----------

      let highlightTimer = null;
      let highlightedVersion = -1; // editor.version() the spans were computed for

      function scheduleHighlight() {
        if (!filePath || !editor) return;
        if (editor.version() === highlightedVersion) return; // content unchanged
        if (highlightTimer) clearTimeout(highlightTimer);
        highlightTimer = setTimeout(fetchHighlight, 200);
      }

      async function fetchHighlight() {
        if (!editor || !filePath) return;
        highlightedVersion = editor.version();
        const content = editor.content();
        try {
          const resp = await fetch("/api/highlight", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: filePath, content }),
          });
          if (!resp.ok) return;
          const data = await resp.json();
          const next = new Map();
          for (const [line, spans] of Object.entries(data.lines || {})) {
            next.set(Number(line), spans);
          }
          highlightSpans = next;
          wrapRowsDirty = true; // spans changed → repaint wrapped rows
          render();
        } catch (_) {
          // Highlight is best-effort; ignore network/parse errors.
        }
      }

      let gitGutterTimer = null;
      let gitGutterVersion = -1; // editor.version() the gutter was computed for

      function scheduleGitGutter() {
        if (!filePath || !editor) return;
        if (editor.version() === gitGutterVersion) return; // content unchanged
        if (gitGutterTimer) clearTimeout(gitGutterTimer);
        gitGutterTimer = setTimeout(fetchGitGutter, 200);
      }

      async function fetchGitGutter() {
        if (!editor || !filePath) return;
        gitGutterVersion = editor.version();
        const content = editor.content();
        try {
          const resp = await fetch("/api/git-gutter", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: filePath, content }),
          });
          if (!resp.ok) return;
          const data = await resp.json();
          const next = new Map();
          for (const [line, status] of Object.entries(data.lines || {})) {
            next.set(Number(line), status);
          }
          gitGutter = next;
          wrapRowsDirty = true; // gutter status changed → repaint wrapped rows
          render();
        } catch (_) {
          // Gutter is best-effort; ignore network/parse errors.
        }
      }

      // ---- Markdown / HTML preview (server-rendered, debounced) ------------

      // Which preview kind a path supports, or null if none. Drives both the
      // palette command's visibility and how refreshPreview() wraps the result.
      function previewableKind(p) {
        const ext = (p || "").split(".").pop().toLowerCase();
        if (ext === "md" || ext === "markdown") return "markdown";
        if (ext === "html" || ext === "htm") return "html";
        return null;
      }

      let previewOpen = false;
      let previewTimer = null;

      // Styles + mermaid bootstrap injected into the preview iframe. Mirrors the
      // GitHub preview server's markdown-body look so the rendered output is
      // consistent across the two surfaces.
      const PREVIEW_CSS = [
        "body { margin: 0; padding: 20px; color: #1f2328; background: #fff;",
        "  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif; line-height: 1.6; }",
        ".markdown-body { max-width: 980px; margin: 0 auto; }",
        ".markdown-body img { max-width: 100%; }",
        ".markdown-body pre { background: #f6f8fa; padding: 16px; border-radius: 6px; overflow: auto; line-height: 1.45; }",
        ".markdown-body code { background: rgba(175,184,193,0.2); padding: 0.2em 0.4em; border-radius: 6px;",
        "  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 85%; }",
        ".markdown-body pre code { background: transparent; padding: 0; border-radius: 0; font-size: 100%; }",
        ".markdown-body table { border-collapse: collapse; }",
        ".markdown-body th, .markdown-body td { border: 1px solid #d0d7de; padding: 6px 13px; }",
        "pre.mermaid { background: #fff; border: none; display: flex; justify-content: center; }",
      ].join("\n");

      const PREVIEW_MERMAID_BOOT =
        '<script src="/assets/mermaid.min.js"><\/script>' +
        "<script>(function(){if(!window.mermaid)return;" +
        "window.mermaid.initialize({startOnLoad:false,theme:'default'});" +
        "window.mermaid.run({querySelector:'pre.mermaid'}).catch(function(){});})();<\/script>";

      function schedulePreview() {
        if (!previewOpen || !editor || !filePath) return;
        if (previewTimer) clearTimeout(previewTimer);
        previewTimer = setTimeout(refreshPreview, 250);
      }

      async function refreshPreview() {
        if (!previewOpen || !editor || !filePath) return;
        const content = editor.content();
        try {
          const resp = await fetch("/api/preview", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: filePath, content }),
          });
          if (!resp.ok) return;
          const data = await resp.json();
          if (data.kind === "html") {
            els.previewFrame.srcdoc = data.html;
          } else if (data.kind === "markdown") {
            els.previewFrame.srcdoc =
              '<!DOCTYPE html><html><head><meta charset="utf-8"><style>' +
              PREVIEW_CSS +
              '</style></head><body><div class="markdown-body">' +
              data.html +
              "</div>" +
              PREVIEW_MERMAID_BOOT +
              "</body></html>";
          } else {
            els.previewFrame.srcdoc = "";
          }
        } catch (_) {
          // Preview is best-effort; ignore network/parse errors.
        }
      }

      // Toggle the split preview pane (palette command). Only meaningful for
      // Markdown/HTML files; the editor re-measures its narrower width on render.
      function togglePreview() {
        if (!previewableKind(filePath)) {
          showError("Preview is only available for Markdown and HTML files.");
          return;
        }
        previewOpen = !previewOpen;
        // Persist per browser tab so the choice survives full-page navigation
        // to another file within the same tab (sessionStorage, not localStorage:
        // each tab/window keeps its own setting).
        try { sessionStorage.setItem("gargo_preview", String(previewOpen)); } catch (_) {}
        els.previewPane.hidden = !previewOpen;
        els.previewResizer.hidden = !previewOpen;
        render();
        if (previewOpen) refreshPreview();
      }

      // Reconcile the split preview with the current file on every load/swap:
      // keep it open if the file supports preview and this tab wants it (either
      // it's already open, or sessionStorage remembers "on"), otherwise hide the
      // pane. A persisted "on" for a non-previewable file is kept (not cleared),
      // so navigating back to a Markdown/HTML file in the same tab re-opens it.
      // The caller renders afterwards, so this doesn't render itself.
      function syncPreview() {
        let want = previewOpen;
        if (!want) {
          try { want = sessionStorage.getItem("gargo_preview") === "true"; } catch (_) {}
        }
        const ok = want && !!previewableKind(filePath);
        previewOpen = ok;
        els.previewPane.hidden = !ok;
        els.previewResizer.hidden = !ok;
        if (ok) refreshPreview();
      }

      function syncStatus(model) {
        els.mode.textContent = model.mode;
        els.dirty.textContent = isModified() ? "● modified" : "";
      }

      function positionIme(model) {
        const c = model.cursors[0] || { row: 0, col: 0, char_col: 0 };
        const rowText = model.rows[c.row - model.top] || "";
        const x = gutterWidth + prefixPx(rowText, c.char_col);
        const y = c.row * LINE_HEIGHT;
        els.ime.style.left = x + "px";
        els.ime.style.top = y + "px";
        els.preedit.style.left = x + "px";
        els.preedit.style.top = y + "px";
      }

      // ---- soft-wrap renderer ----------------------------------------------
      //
      // In wrap mode lines flow normally (the browser wraps them); we read the
      // exact caret/selection geometry back from the DOM via Range rects, so
      // overlays follow the wrapping precisely. Rows are rebuilt only when the
      // content (version) or highlight/gutter spans change — not on cursor moves.

      function renderWrap() {
        const total = editor.line_count();
        setGutterWidth(total);
        els.sizer.style.height = "";
        els.sizer.style.width = "";
        const model = editor.render(0, total);
        lastModel = model;
        const v = editor.version();
        if (v !== wrapBuiltVersion || wrapRowsDirty) {
          buildWrapRows(model);
          wrapBuiltVersion = v;
          wrapRowsDirty = false;
        }
        positionWrapOverlays(model);
        syncStatus(model);
        positionImeWrap(model);
        lastVersion = v;
      }

      function buildWrapRows(model) {
        for (const e of wrapLineEls.values()) e.wline.remove();
        wrapLineEls.clear();
        const frag = document.createDocumentFragment();
        for (let line = 0; line < model.rows.length; line++) {
          const wline = document.createElement("div");
          wline.className = "wline";
          wline.dataset.line = String(line);
          const wgutter = document.createElement("div");
          const gst = gitGutter.get(line);
          wgutter.className = "wgutter" + (gst ? " git-" + gst : "");
          wgutter.textContent = String(line + 1);
          const wrow = document.createElement("div");
          wrow.className = "wrow";
          paintRow(wrow, model.rows[line], highlightSpans.get(line));
          wline.appendChild(wgutter);
          wline.appendChild(wrow);
          frag.appendChild(wline);
          wrapLineEls.set(line, { wline, wgutter, wrow });
        }
        els.sizer.appendChild(frag);
      }

      function positionWrapOverlays(model) {
        const base = els.sizer.getBoundingClientRect();

        ensurePool(caretPool, model.cursors.length, "caret");
        for (let i = 0; i < caretPool.length; i++) {
          const el = caretPool[i];
          const c = i < model.cursors.length ? model.cursors[i] : null;
          const e = c ? wrapLineEls.get(c.row) : null;
          const rect = e ? caretRectInRow(e.wrow, c.char_col) : null;
          if (rect) {
            el.style.display = "block";
            el.style.left = rect.left - base.left + "px";
            el.style.top = rect.top - base.top + "px";
            el.className = "caret" + (c.primary ? " primary" : "");
            if (c.primary) wrapPrimaryCaretTop = rect.top - base.top;
          } else {
            el.style.display = "none";
          }
        }

        // One rectangle per wrapped segment, courtesy of Range.getClientRects().
        const selRects = [];
        for (const s of model.selections) {
          for (let row = s.start_row; row <= s.end_row; row++) {
            const e = wrapLineEls.get(row);
            if (!e) continue;
            const text = model.rows[row] || "";
            const lineChars = Array.from(text).length;
            const fromChar = row === s.start_row ? s.start_char : 0;
            const toChar = row === s.end_row ? s.end_char : lineChars;
            for (const r of rangeRectsInRow(e.wrow, fromChar, toChar)) selRects.push(r);
          }
        }
        placeWrapRects(selPool, "sel", selRects, base);

        const matchRects = [];
        if (findOpen) {
          for (let i = 0; i < searchMatches.length; i++) {
            if (i === searchIndex && findCurrentSelected) continue;
            const m = searchMatches[i];
            const e = wrapLineEls.get(m.row);
            if (!e) continue;
            for (const r of rangeRectsInRow(e.wrow, m.start_char, m.end_char)) matchRects.push(r);
          }
        }
        placeWrapRects(matchPool, "match-hl", matchRects, base);
      }

      function placeWrapRects(pool, cls, rects, base) {
        ensurePool(pool, rects.length, cls);
        for (let i = 0; i < pool.length; i++) {
          const el = pool[i];
          if (i < rects.length) {
            const r = rects[i];
            el.style.display = "block";
            el.style.left = r.left - base.left + "px";
            el.style.top = r.top - base.top + "px";
            el.style.width = r.width + "px";
            el.style.height = r.height + "px";
          } else {
            el.style.display = "none";
          }
        }
      }

      // Locate the text node + UTF-16 offset for character index `charIdx` inside
      // a wrapped row (whose descendants may be syntax <span>s). For all non-astral
      // text (incl. CJK) a char index equals a code-unit offset.
      function locateChar(wrow, charIdx) {
        const walk = document.createTreeWalker(wrow, NodeFilter.SHOW_TEXT);
        let node = walk.nextNode();
        let acc = 0;
        let last = null;
        while (node) {
          const len = node.nodeValue.length;
          last = node;
          if (charIdx <= acc + len) return { node, offset: charIdx - acc };
          acc += len;
          node = walk.nextNode();
        }
        return last ? { node: last, offset: last.nodeValue.length } : null;
      }

      // Viewport-coord rect for the caret at `charIdx` within a wrapped row.
      function caretRectInRow(wrow, charIdx) {
        const loc = locateChar(wrow, charIdx);
        if (!loc) {
          const r = wrow.getBoundingClientRect();
          return { left: r.left, top: r.top, width: 0, height: r.height };
        }
        const range = document.createRange();
        range.setStart(loc.node, loc.offset);
        range.setEnd(loc.node, loc.offset);
        let rect = range.getBoundingClientRect();
        if (!rect.height && loc.offset < loc.node.nodeValue.length) {
          range.setEnd(loc.node, loc.offset + 1);
          rect = range.getBoundingClientRect();
        }
        if (!rect.height) {
          const r = wrow.getBoundingClientRect();
          return { left: rect.left || r.left, top: r.top, width: 0, height: r.height };
        }
        return { left: rect.left, top: rect.top, width: 0, height: rect.height };
      }

      // Per-visual-row rects covering [fromChar, toChar) within a wrapped row.
      function rangeRectsInRow(wrow, fromChar, toChar) {
        if (toChar <= fromChar) return [];
        const a = locateChar(wrow, fromChar);
        const b = locateChar(wrow, toChar);
        if (!a || !b) return [];
        const range = document.createRange();
        range.setStart(a.node, a.offset);
        range.setEnd(b.node, b.offset);
        return Array.from(range.getClientRects());
      }

      // Character offset within `wrow` for a DOM (node, offset) hit-test result.
      function charOffsetWithin(wrow, node, offset) {
        const walk = document.createTreeWalker(wrow, NodeFilter.SHOW_TEXT);
        let n = walk.nextNode();
        let acc = 0;
        while (n) {
          if (n === node) return acc + offset;
          acc += n.nodeValue.length;
          n = walk.nextNode();
        }
        return acc;
      }

      // Map a mouse event to a (row, col) using the browser's hit-testing, which
      // is wrap-aware. `col` is a character index (treated as a display column by
      // set_cursor, exact for ASCII, approximate for tabs/CJK — like the
      // non-wrap path).
      function wrapEventToRowCol(e) {
        let node = null;
        let offset = 0;
        if (document.caretPositionFromPoint) {
          const p = document.caretPositionFromPoint(e.clientX, e.clientY);
          if (p) {
            node = p.offsetNode;
            offset = p.offset;
          }
        } else if (document.caretRangeFromPoint) {
          const r = document.caretRangeFromPoint(e.clientX, e.clientY);
          if (r) {
            node = r.startContainer;
            offset = r.startOffset;
          }
        }
        if (!node) return null;
        const startEl = node.nodeType === 3 ? node.parentNode : node;
        const wline = startEl && startEl.closest ? startEl.closest(".wline") : null;
        if (!wline) return null;
        const row = Number(wline.dataset.line);
        const wrow = wline.querySelector(".wrow");
        const col = wrow && node.nodeType === 3 ? charOffsetWithin(wrow, node, offset) : 0;
        return { row, col };
      }

      function positionImeWrap(model) {
        const c = model.cursors[0];
        const e = c ? wrapLineEls.get(c.row) : null;
        const base = els.sizer.getBoundingClientRect();
        let x = 0;
        let y = 0;
        if (e) {
          const r = caretRectInRow(e.wrow, c.char_col);
          x = r.left - base.left;
          y = r.top - base.top;
        }
        els.ime.style.left = x + "px";
        els.ime.style.top = y + "px";
        els.preedit.style.left = x + "px";
        els.preedit.style.top = y + "px";
      }

      // Toggle soft-wrap (Alt+Z / palette), remembering the choice per browser.
      function toggleWrap() {
        wrapMode = !wrapMode;
        try { localStorage.setItem("gargo_wrap", String(wrapMode)); } catch (_) {}
        render();
        ensureCursorVisible();
      }

      // ---- cursor-follow scrolling (vertical + horizontal) -----------------

      function ensureCursorVisible() {
        if (!editor) return;
        if (renderedWrap) {
          ensureCursorVisibleWrap();
          return;
        }
        const row = editor.cursor_row();
        const col = editor.cursor_col();

        // Vertical: keep a couple of context lines above/below the caret.
        const m = LINE_HEIGHT * 2;
        const top = els.scroller.scrollTop;
        const h = els.scroller.clientHeight;
        const cy = row * LINE_HEIGHT;
        if (cy < top + m) {
          els.scroller.scrollTop = Math.max(0, cy - m);
        } else if (cy + LINE_HEIGHT > top + h - m) {
          els.scroller.scrollTop = cy + LINE_HEIGHT - h + m;
        }

        // Horizontal: keep a few columns of context left/right of the caret.
        const mx = charWidth * 4;
        const left = els.scroller.scrollLeft;
        const w = els.scroller.clientWidth;
        const cx = colToPx(col);
        if (cx < left + gutterWidth + mx) {
          els.scroller.scrollLeft = Math.max(0, cx - gutterWidth - mx);
        } else if (cx + charWidth > left + w - mx) {
          els.scroller.scrollLeft = cx + charWidth - w + mx;
        }
      }

      // Wrap mode: scroll the primary caret into view. renderWrap() refreshes the
      // rows/overlays first so the measured caret rect reflects the latest edit,
      // regardless of whether callers run render() before or after this.
      function ensureCursorVisibleWrap() {
        renderWrap();
        const top = els.scroller.scrollTop;
        const h = els.scroller.clientHeight;
        const m = LINE_HEIGHT * 2;
        const cy = wrapPrimaryCaretTop;
        if (cy < top + m) {
          els.scroller.scrollTop = Math.max(0, cy - m);
        } else if (cy + LINE_HEIGHT > top + h - m) {
          els.scroller.scrollTop = cy + LINE_HEIGHT - h + m;
        }
      }

      // Apply an edit/motion, then follow the caret and repaint.
      function afterEdit() {
        ensureCursorVisible();
        render();
        scheduleHighlight();
        scheduleGitGutter();
        if (previewOpen) schedulePreview();
        // Edits shift match offsets; recompute so the find box stays accurate.
        if (findOpen) runFind(false);
      }

      // ---- input -----------------------------------------------------------

      // Browser KeyboardEvent.key → name understood by WebEditor.key().
      function keyName(e) {
        const k = e.key;
        if (k.length === 1) return k; // single printable char (includes space)
        const named = new Set([
          "Enter", "Backspace", "Delete", "Tab",
          "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
          "Home", "End", "PageUp", "PageDown", "Insert",
        ]);
        if (named.has(k)) return k;
        if (/^F([1-9]|1[0-2])$/.test(k)) return k;
        return null; // Shift/Control/Alt/Meta/CapsLock/Escape/... — ignore
      }

      function onKeyDown(e) {
        // While composing, let the IME consume everything.
        if (composing || e.isComposing || e.keyCode === 229) return;
        if (pickerOpen) return; // picker has its own input + handler

        // Cmd+L / Ctrl+L: leave untouched so the browser handles it (focus the
        // address bar). Don't preventDefault and don't route it to the core.
        if ((e.metaKey || e.ctrlKey) && (e.key === "l" || e.key === "L")) return;

        // Cmd+R / Cmd+Shift+R: page reload — hand straight to the browser.
        // (Falling through would preventDefault it and route "r" into the core.)
        if (e.metaKey && (e.key === "r" || e.key === "R")) return;

        // Clipboard bridge. The modal core's selection lives in wasm, not the
        // DOM, so the browser has nothing to copy on its own:
        //   Cmd+C / Cmd+X — copy (and cut) the selection via the async API.
        //   Cmd+V         — leave it for the textarea's native `paste` event
        //                   (onPaste); preventDefault here would suppress it.
        if (e.metaKey && !e.shiftKey && (e.key === "c" || e.key === "C")) {
          const sel = editor.selection_text();
          if (!sel) return; // nothing selected → let the browser have the key
          e.preventDefault();
          navigator.clipboard.writeText(sel).catch(() => {});
          return;
        }
        if (e.metaKey && !e.shiftKey && (e.key === "x" || e.key === "X")) {
          const sel = editor.selection_text();
          if (!sel) return;
          e.preventDefault();
          navigator.clipboard.writeText(sel).catch(() => {});
          editor.delete_selection();
          els.ime.value = "";
          afterEdit();
          return;
        }
        if (e.metaKey && !e.shiftKey && (e.key === "v" || e.key === "V")) return;

        // Cmd+Shift+P: command palette (opens the picker in "> " command mode).
        // Checked before Cmd+P so the Shift variant doesn't fall through.
        if (e.metaKey && e.shiftKey && (e.key === "p" || e.key === "P")) {
          e.preventDefault();
          openCommandPalette();
          return;
        }

        // Cmd+P: fuzzy file picker (VSCode-style). Ctrl+P stays emacs "move up".
        if (e.metaKey && (e.key === "p" || e.key === "P")) {
          e.preventDefault();
          openPicker();
          return;
        }

        // Cmd+Shift+F: project-wide search (Zed-style). Checked before Cmd+F so
        // the Shift variant doesn't fall through to the in-file find.
        if ((e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "f" || e.key === "F")) {
          e.preventDefault();
          openGlobalSearch();
          return;
        }

        // Cmd+F: find; Cmd+Alt+F: find & replace. Ctrl+F stays emacs move-right.
        if (e.metaKey && (e.key === "f" || e.key === "F")) {
          e.preventDefault();
          openFind(e.altKey);
          return;
        }

        // Undo / redo (VSCode/Mac-style). The Insert-mode keymap has no Ctrl+Z
        // binding, so drive the core undo/redo directly and stay in Insert.
        if ((e.ctrlKey || e.metaKey) && (e.key === "z" || e.key === "Z")) {
          e.preventDefault();
          if (e.shiftKey) editor.redo();
          else editor.undo();
          els.ime.value = "";
          afterEdit();
          return;
        }
        if ((e.ctrlKey || e.metaKey) && !e.shiftKey && (e.key === "y" || e.key === "Y")) {
          e.preventDefault();
          editor.redo();
          els.ime.value = "";
          afterEdit();
          return;
        }

        // Save shortcut.
        if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
          e.preventDefault();
          save();
          return;
        }

        // Whole-line delete (VSCode-style), independent of selection.
        if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "k" || e.key === "K")) {
          e.preventDefault();
          editor.delete_line();
          els.ime.value = "";
          afterEdit();
          return;
        }

        // Cmd+D: select the word under the caret, or (with a selection) add a
        // cursor at the next occurrence (VSCode-style). Cmd only — Ctrl+D stays
        // the emacs delete-forward in the core keymap.
        if (e.metaKey && !e.shiftKey && (e.key === "d" || e.key === "D")) {
          e.preventDefault();
          editor.select_word_or_add_next_match();
          els.ime.value = "";
          afterEdit();
          return;
        }

        // Cmd+A: select the whole buffer. Cmd only — Ctrl+A stays the emacs
        // move-to-line-start in the core keymap. set_selection takes display
        // columns and clamps to the line end internally, so a huge head column
        // lands at true EOF regardless of tabs/CJK on the last line.
        if (e.metaKey && !e.shiftKey && (e.key === "a" || e.key === "A")) {
          e.preventDefault();
          const last = Math.max(0, editor.line_count() - 1);
          editor.set_selection(0, 0, last, Number.MAX_SAFE_INTEGER);
          els.ime.value = "";
          afterEdit();
          return;
        }

        // Cmd+Shift+E: move keyboard focus into the file explorer (VSCode
        // "Focus on Explorer"). The explorer then handles arrows/Enter/e/Esc.
        if (e.metaKey && e.shiftKey && (e.key === "e" || e.key === "E")) {
          e.preventDefault();
          focusExplorer();
          return;
        }

        // Cmd+Shift+L: select every occurrence of the word/selection and place a
        // cursor on each (VSCode "Select All Occurrences"). Plain Cmd+L is left
        // to the browser (focus address bar).
        if (e.metaKey && e.shiftKey && (e.key === "l" || e.key === "L")) {
          e.preventDefault();
          editor.add_cursors_to_all_matches();
          els.ime.value = "";
          afterEdit();
          return;
        }

        // Cmd+Shift+O: go to symbol in file (opens the picker in "@" mode).
        if (e.metaKey && e.shiftKey && (e.key === "o" || e.key === "O")) {
          e.preventDefault();
          openSymbolPicker();
          return;
        }

        // Alt+Z: toggle soft word-wrap. Use e.code since Alt rewrites e.key on
        // some layouts (Mac Option produces e.g. "Ω").
        if (e.altKey && !e.metaKey && !e.ctrlKey && e.code === "KeyZ") {
          e.preventDefault();
          toggleWrap();
          return;
        }

        // Always-insert PoC: swallow Escape so we never leave Insert mode.
        if (e.key === "Escape") {
          e.preventDefault();
          return;
        }

        const name = keyName(e);
        if (name === null) return; // pure modifier press

        e.preventDefault();

        // Mac/VSCode-style modified deletes. These would otherwise reach the
        // core keymap as Ctrl+Backspace/Delete (Cmd is sent as Ctrl) and resolve
        // to Noop, so handle them here by extending a selection then deleting it:
        //   Cmd+Backspace → to line start   Cmd+Delete → to line end
        //   Alt+Backspace → previous word    Alt+Delete → next word
        if ((e.metaKey || e.altKey) && (name === "Backspace" || name === "Delete")) {
          const back = name === "Backspace";
          let ext;
          if (e.metaKey) ext = back ? ["a", true, true, false] : ["e", true, true, false];
          else ext = back ? ["ArrowLeft", true, true, false] : ["ArrowRight", true, true, false];
          if (!editor.has_selection()) editor.key(...ext); // extend; no-op selects nothing
          if (editor.has_selection()) {
            editor.delete_selection();
          } else {
            // Empty line (caret already at line start): a plain delete removes
            // the adjacent newline, joining with the previous/next line.
            editor.key(name, false, false, false);
          }
          els.ime.value = "";
          afterEdit();
          return;
        }

        // VSCode-style selection editing: Backspace/Delete removes the
        // selection; typing a printable char / Enter / Tab replaces it.
        const noMod = !e.ctrlKey && !e.metaKey && !e.altKey;
        const isDelete = name === "Backspace" || name === "Delete";
        const isTextInput = noMod && (name.length === 1 || name === "Enter" || name === "Tab");

        // Auto-surround: typing an opening bracket/quote with text selected wraps
        // the selection instead of replacing it (VSCode behaviour).
        if (noMod && SURROUND_PAIRS[name] && editor.has_selection()) {
          editor.wrap_selection(name, SURROUND_PAIRS[name]);
          els.ime.value = "";
          afterEdit();
          return;
        }

        if ((isDelete || isTextInput) && editor.has_selection()) {
          editor.delete_selection();
          if (isDelete) {
            els.ime.value = "";
            afterEdit();
            return; // selection consumed; don't also delete a stray char
          }
          // fall through: insert the typed key on top of the now-empty range
        }

        editor.key(name, e.ctrlKey || e.metaKey, e.shiftKey, e.altKey);
        // Keep the IME textarea empty so command keystrokes never accumulate.
        els.ime.value = "";
        afterEdit();
      }

      function onCompositionStart() {
        composing = true;
        els.preedit.textContent = "";
      }

      function onCompositionUpdate(e) {
        paintPreedit(e.data || "");
      }

      // Inline IME pre-edit: splice the composing `text` into the caret's row at
      // the caret column so the caret and the trailing text on that line follow
      // the composition (instead of a static overlay that stays at the start
      // position and overlaps the following text). The row's syntax highlight is
      // dropped for the duration of the composition; the next full render (on
      // commit/cancel) restores it. No `text` → restore the row to its committed
      // state. Falls back to a full render when the caret row isn't mounted.
      function paintPreedit(text) {
        if (!editor || !lastModel) return;
        if (!text) {
          clearPreedit();
          return;
        }
        const cursors = lastModel.cursors || [];
        const c = cursors.find((x) => x.primary) || cursors[0];
        if (!c) {
          clearPreedit();
          return;
        }
        const span = '<span class="preedit-inline">' + escapeHtml(text) + "</span>";
        if (renderedWrap) {
          const e = wrapLineEls.get(c.row);
          if (!e) {
            clearPreedit();
            return;
          }
          const rowText = (lastModel.rows[c.row] || "");
          const chars = Array.from(rowText);
          const before = chars.slice(0, c.char_col).join("");
          const after = chars.slice(c.char_col).join("");
          e.wrow.innerHTML = escapeHtml(before) + span + escapeHtml(after);
          const base = els.sizer.getBoundingClientRect();
          const r = caretRectInRow(e.wrow, c.char_col + Array.from(text).length);
          const x = r.left - base.left;
          const y = r.top - base.top;
          movePrimaryCaret(x, y);
          els.ime.style.left = x + "px";
          els.ime.style.top = y + "px";
        } else {
          const entry = mountedRows.get(c.row);
          if (!entry) {
            clearPreedit();
            return;
          }
          const rowText = (lastModel.rows[c.row - lastModel.top] || "");
          const chars = Array.from(rowText);
          const before = chars.slice(0, c.char_col).join("");
          const after = chars.slice(c.char_col).join("");
          entry.row.innerHTML = escapeHtml(before) + span + escapeHtml(after);
          const headChars = Array.from(before + text);
          const x = gutterWidth + prefixPx(before + text, headChars.length);
          const y = c.row * LINE_HEIGHT;
          movePrimaryCaret(x, y);
          els.ime.style.left = x + "px";
          els.ime.style.top = y + "px";
        }
      }

      // Move the primary caret element to a content-pixel position (used while an
      // inline IME pre-edit is shown, since no full render runs during composition).
      function movePrimaryCaret(x, y) {
        const cursors = (lastModel && lastModel.cursors) || [];
        let idx = cursors.findIndex((x2) => x2.primary);
        if (idx < 0) idx = 0;
        const el = caretPool[idx];
        if (el) {
          el.style.display = "block";
          el.style.left = x + "px";
          el.style.top = y + "px";
        }
      }

      // Drop any inline pre-edit and restore the committed view. In wrap mode the
      // row is only rebuilt when the version changes, so force a rebuild.
      function clearPreedit() {
        if (renderedWrap) wrapRowsDirty = true;
        render();
      }

      // Insert text, first replacing the active selection if any (VSCode-style).
      function insertReplacing(text) {
        if (editor.has_selection()) editor.delete_selection();
        editor.insert_text(text);
        afterEdit();
      }

      function onCompositionEnd(e) {
        composing = false;
        els.preedit.textContent = "";
        const text = e.data || "";
        els.ime.value = "";
        if (text) insertReplacing(text); // afterEdit() re-renders, clearing pre-edit
        else clearPreedit(); // cancelled composition: restore the committed row
      }

      // Fallback for IMEs/dead-keys that commit via `input` without composition.
      function onInput() {
        if (composing) return;
        const text = els.ime.value;
        els.ime.value = "";
        if (text) insertReplacing(text);
      }

      function onPaste(e) {
        e.preventDefault();
        const text = (e.clipboardData || window.clipboardData).getData("text");
        if (text) insertReplacing(text);
      }

      // ---- mouse (click = move caret, drag = select) -----------------------

      let dragging = false;
      let dragAnchor = null; // { row, col } captured at mousedown

      // Map a mouse event to a display (row, col). #sizer holds the absolutely
      // positioned rows; its bounding rect already accounts for scroll, so
      // clientX/Y minus the rect maps straight into document coordinates.
      function eventToRowCol(e) {
        if (renderedWrap) {
          const rc = wrapEventToRowCol(e);
          if (rc) return rc;
        }
        const rect = els.sizer.getBoundingClientRect();
        const x = e.clientX - rect.left;
        const y = e.clientY - rect.top;
        const row = Math.max(0, Math.floor(y / LINE_HEIGHT));
        const col = Math.max(0, Math.round((x - gutterWidth) / charWidth));
        return { row, col };
      }

      function onMouseDown(e) {
        if (e.button !== 0) return; // left button only
        const { row, col } = eventToRowCol(e);

        // Cmd/Ctrl+click a rendered link (.elink) → open it in another tab:
        // http(s) in a new browser tab, a repo/relative path in a new editor tab.
        // Falls through to normal caret placement when not on a link.
        if (e.metaKey || e.ctrlKey) {
          const a = e.target && e.target.closest ? e.target.closest(".elink") : null;
          if (a) {
            e.preventDefault();
            openLink(a.dataset.href);
            return;
          }
        }

        e.preventDefault(); // suppress native text selection
        els.ime.focus();

        // Double-click: select the word under the cursor.
        if (e.detail === 2) {
          dragging = false;
          editor.select_word_at(row, col);
          render();
          return;
        }

        // Shift-click: extend the selection from the existing anchor (or the
        // current caret if there's no selection) to the clicked point.
        if (e.shiftKey) {
          let ar = editor.anchor_row();
          let ac = editor.anchor_col();
          if (ar < 0) {
            ar = editor.cursor_row();
            ac = editor.cursor_col();
          }
          dragAnchor = { row: ar, col: ac };
          dragging = true;
          editor.set_selection(ar, ac, row, col);
          render();
          return;
        }

        dragging = true;
        dragAnchor = { row, col };
        editor.set_cursor(row, col);
        render();
      }

      function onMouseMove(e) {
        if (!dragging) return;
        const { row, col } = eventToRowCol(e);
        editor.set_selection(dragAnchor.row, dragAnchor.col, row, col);
        ensureCursorVisible(); // follow the drag head past the viewport edge
        render();
      }

      function onMouseUp() {
        dragging = false;
      }

      // ---- Cmd+P fuzzy picker (files + command palette) --------------------
      //
      // One widget, prefix-driven like the terminal palette: a bare query (or
      // empty) searches files, "> " searches commands (Cmd+Shift+P), and ":<n>"
      // jumps to a line. Each result is generic: { text, positions, hint?,
      // onChoose(event) } so files and commands share rendering and navigation.

      let pickerOpen = false;
      let allFiles = null; // cached list from /api/files
      let allSymbols = []; // symbols for the current file, refreshed when @ opens
      let pickerResults = [];
      let pickerSel = 0;
      const PICKER_LIMIT = 50;

      // Opening bracket/quote → its closing partner, for auto-surround and the
      // "Wrap Selection" palette commands.
      const SURROUND_PAIRS = { "(": ")", "[": "]", "{": "}", '"': '"', "'": "'", "`": "`" };

      // Command palette entries. `run` is the action; `hint` is the keybinding
      // shown muted on the right. Commands that open another overlay close the
      // picker first so the overlay can take focus; editing commands run and let
      // afterEdit() re-render and refocus the editor.
      const COMMANDS = [
        { label: "Save File", hint: "⌘S", run: () => save() },
        { label: "Search in Project", hint: "⇧⌘F", run: () => openGlobalSearch() },
        { label: "Find", hint: "⌘F", run: () => openFind(false) },
        { label: "Find and Replace", hint: "⌥⌘F", run: () => openFind(true) },
        { label: "Go to File", hint: "⌘P", run: () => reopenPicker("") },
        { label: "Go to Line…", hint: "", run: () => reopenPicker(":") },
        { label: "Go to Symbol in File…", hint: "⇧⌘O", run: () => openSymbolPicker() },
        { label: "Undo", hint: "⌘Z", run: () => { editor.undo(); afterEdit(); } },
        { label: "Redo", hint: "⇧⌘Z", run: () => { editor.redo(); afterEdit(); } },
        { label: "Delete Line", hint: "⇧⌘K", run: () => { editor.delete_line(); afterEdit(); } },
        { label: "Add Cursor Above", hint: "", run: () => { editor.add_cursor_above(); afterEdit(); } },
        { label: "Add Cursor Below", hint: "", run: () => { editor.add_cursor_below(); afterEdit(); } },
        { label: "Add Cursors to Top", hint: "", run: () => { editor.add_cursors_to_top(); afterEdit(); } },
        { label: "Add Cursors to Bottom", hint: "", run: () => { editor.add_cursors_to_bottom(); afterEdit(); } },
        { label: "Select Next Occurrence", hint: "⌘D", run: () => { editor.select_word_or_add_next_match(); afterEdit(); } },
        { label: "Select All Occurrences", hint: "⇧⌘L", run: () => { editor.add_cursors_to_all_matches(); afterEdit(); } },
        { label: "Clear Other Cursors", hint: "", run: () => { editor.remove_secondary_cursors(); afterEdit(); } },
        { label: "Toggle Word Wrap", hint: "⌥Z", run: () => toggleWrap() },
        { label: "Toggle Preview", hint: "", when: () => previewableKind(filePath), run: () => togglePreview() },
        { label: "Wrap Selection in ( )", hint: "", run: () => { editor.wrap_selection("(", ")"); afterEdit(); } },
        { label: "Wrap Selection in [ ]", hint: "", run: () => { editor.wrap_selection("[", "]"); afterEdit(); } },
        { label: "Wrap Selection in { }", hint: "", run: () => { editor.wrap_selection("{", "}"); afterEdit(); } },
        { label: "Wrap Selection in \" \"", hint: "", run: () => { editor.wrap_selection('"', '"'); afterEdit(); } },
        { label: "Wrap Selection in ' '", hint: "", run: () => { editor.wrap_selection("'", "'"); afterEdit(); } },
        { label: "Wrap Selection in ` `", hint: "", run: () => { editor.wrap_selection("`", "`"); afterEdit(); } },
        { label: "Copy File Path", hint: "", run: () => copyText(absPath(filePath)) },
        { label: "Copy Relative Path", hint: "", run: () => copyText(filePath) },
        { label: "Reveal in Finder", hint: "", run: () => { if (filePath) revealInFinder(filePath); } },
      ];

      // ---- keyboard-shortcuts help overlay ---------------------------------
      //
      // The editor page doesn't load server_shortcuts.js (which owns the `?`
      // overlay on the other pages), so it builds its own overlay reusing the
      // shared .gargo-help-* styles from server_shared.css. Opened by the
      // top-right "?" rail button; closed by Esc, the × button, or a backdrop click.
      const HELP_SECTIONS = [
        { heading: "Files & Search", rows: [
          ["⌘P", "Go to file"],
          ["⌘F", "Find"],
          ["⌥⌘F", "Find and replace"],
          ["⇧⌘F", "Search in project"],
          ["⇧⌘O", "Go to symbol in file"],
          ["⇧⌘P", "Command palette"],
        ]},
        { heading: "Editing", rows: [
          ["⌘S", "Save"],
          ["⌘Z / ⇧⌘Z", "Undo / redo"],
          ["⌘D", "Select next occurrence"],
          ["⇧⌘L", "Select all occurrences"],
          ["⇧⌘K", "Delete line"],
          ["⌘⌫ / ⌥⌫", "Delete to line start / previous word"],
          ["⌥Z", "Toggle word wrap"],
        ]},
        { heading: "Movement (emacs)", rows: [
          ["Ctrl+f / Ctrl+b", "Forward / back one char"],
          ["Ctrl+n / Ctrl+p", "Next / previous line"],
          ["Ctrl+a / Ctrl+e", "Line start / end"],
        ]},
        { heading: "Links", rows: [
          ["⌘/Ctrl + Click", "Open link under the cursor"],
        ]},
      ];

      let helpOverlayEl = null;

      function buildHelpOverlay() {
        const wrap = document.createElement("div");
        wrap.className = "gargo-help-overlay";
        wrap.hidden = true;
        const sections = HELP_SECTIONS.map((sec) => {
          const rows = sec.rows.map(([keys, desc]) =>
            `<tr><td class="gargo-help-keys"><kbd>${escapeHtml(keys)}</kbd></td><td>${escapeHtml(desc)}</td></tr>`
          ).join("");
          return `<section><h3>${escapeHtml(sec.heading)}</h3><table>${rows}</table></section>`;
        }).join("");
        wrap.innerHTML = '<div class="gargo-help-panel" role="dialog" aria-label="Keyboard shortcuts">'
          + '<div class="gargo-help-header"><span>Keyboard shortcuts</span>'
          + '<button type="button" class="gargo-help-close" aria-label="Close">×</button></div>'
          + '<div class="gargo-help-body">' + sections + '</div></div>';
        wrap.addEventListener("click", (ev) => { if (ev.target === wrap) closeHelp(); });
        wrap.querySelector(".gargo-help-close").addEventListener("click", closeHelp);
        document.body.appendChild(wrap);
        return wrap;
      }

      function helpOpen() { return helpOverlayEl && !helpOverlayEl.hidden; }
      function openHelp() {
        if (!helpOverlayEl) helpOverlayEl = buildHelpOverlay();
        helpOverlayEl.hidden = false;
      }
      function closeHelp() {
        if (helpOverlayEl) helpOverlayEl.hidden = true;
        els.ime.focus();
      }

      // Fetch the repository file list once; shared by the Cmd+P picker and the
      // sidebar file tree.
      async function loadFiles() {
        if (allFiles !== null) return allFiles;
        try {
          const resp = await fetch("/api/files");
          const data = await resp.json();
          allFiles = data.files || [];
        } catch (err) {
          allFiles = [];
          showError("File list failed: " + err);
        }
        return allFiles;
      }

      // Cmd+P: open in file mode. Cmd+Shift+P: open in command mode ("> ").
      async function openPicker() { await showPicker(""); }
      async function openCommandPalette() { await showPicker("> "); }

      async function showPicker(initial) {
        if (pickerOpen) return;
        pickerOpen = true;
        els.picker.hidden = false;
        els.pickerInput.value = initial;
        if (allFiles === null) {
          els.pickerList.innerHTML = "<li>Loading…</li>";
          await loadFiles();
        }
        updatePicker(initial);
        els.pickerInput.focus();
        // Caret after the prefix so the user types straight into the query.
        const n = els.pickerInput.value.length;
        els.pickerInput.setSelectionRange(n, n);
      }

      // Switch the open picker to another mode (used by the "Go to File" /
      // "Go to Line…" commands) without closing it.
      function reopenPicker(prefix) {
        els.pickerInput.value = prefix;
        updatePicker(prefix);
        els.pickerInput.focus();
        const n = prefix.length;
        els.pickerInput.setSelectionRange(n, n);
      }

      function closePicker() {
        if (!pickerOpen) return;
        pickerOpen = false;
        els.picker.hidden = true;
        els.ime.focus();
      }

      function updatePicker(query) {
        if (query.startsWith(">")) pickerResults = commandResults(query.slice(1).trim());
        else if (query.startsWith(":")) pickerResults = gotoLineResults(query.slice(1).trim());
        else if (query.startsWith("@")) pickerResults = symbolResults(query.slice(1).trim());
        else pickerResults = fileResults(query.trim());
        pickerSel = 0;
        renderPickerList();
      }

      // File mode: fuzzy-match the repo file list; Enter/click opens (Cmd/Alt for
      // a new window), mirroring the file explorer.
      function fileResults(q) {
        const toResult = (p, positions) => ({
          text: p,
          positions,
          onChoose: (e) => {
            // Cmd/Alt opens in a new window — keep this picker open so the user
            // can fan out several files; plain Enter/click closes it.
            if (e && (e.metaKey || e.altKey)) openFileInNewWindow(p);
            else { closePicker(); openFile(p); }
          },
        });
        if (!q) return (allFiles || []).slice(0, PICKER_LIMIT).map((p) => toResult(p, []));
        const scored = [];
        for (const p of allFiles || []) {
          const m = fuzzyMatch(p, q);
          if (m) scored.push({ p, score: m.score, positions: m.positions });
        }
        scored.sort((a, b) => b.score - a.score);
        return scored.slice(0, PICKER_LIMIT).map((s) => toResult(s.p, s.positions));
      }

      // Command mode ("> "): fuzzy-match command labels.
      function commandResults(q) {
        const toResult = (cmd, positions) => ({
          text: cmd.label,
          positions,
          hint: cmd.hint,
          onChoose: () => { closePicker(); cmd.run(); },
        });
        // Some commands are context-sensitive (e.g. "Toggle Preview" only for
        // Markdown/HTML files); hide those whose `when` predicate is false.
        const available = COMMANDS.filter((c) => !c.when || c.when());
        if (!q) return available.map((c) => toResult(c, []));
        const scored = [];
        for (const c of available) {
          const m = fuzzyMatch(c.label, q);
          if (m) scored.push({ c, score: m.score, positions: m.positions });
        }
        scored.sort((a, b) => b.score - a.score);
        return scored.map((s) => toResult(s.c, s.positions));
      }

      // Symbol mode ("@"): fuzzy-match the current file's symbol outline
      // (fetched into `allSymbols` when the picker opens); choosing one jumps the
      // caret to its definition. The kind ("function", "class", …) is the hint.
      function symbolResults(q) {
        if (!allSymbols.length) {
          return [{ text: "No symbols in this file", positions: [] }];
        }
        const toResult = (s, positions) => ({
          text: s.name,
          positions,
          hint: s.kind,
          onChoose: () => {
            closePicker();
            editor.set_cursor(s.line, s.col);
            ensureCursorVisible();
            render();
          },
        });
        if (!q) return allSymbols.slice(0, PICKER_LIMIT).map((s) => toResult(s, []));
        const scored = [];
        for (const s of allSymbols) {
          const m = fuzzyMatch(s.name, q);
          if (m) scored.push({ s, score: m.score, positions: m.positions });
        }
        scored.sort((a, b) => b.score - a.score);
        return scored.slice(0, PICKER_LIMIT).map((r) => toResult(r.s, r.positions));
      }

      // Cmd+Shift+O: fetch the current file's symbols, then open the picker in
      // "@" mode. Symbols are recomputed each open so they reflect live edits.
      async function openSymbolPicker() {
        if (!editor || !filePath) return;
        allSymbols = [];
        try {
          const resp = await fetch("/api/symbols", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: filePath, content: editor.content() }),
          });
          if (resp.ok) allSymbols = (await resp.json()).symbols || [];
        } catch (_) {
          // Symbol outline is best-effort; an empty list shows a placeholder.
        }
        await showPicker("@");
      }

      // Goto-line mode (":<n>"): a single synthetic result that jumps the caret.
      function gotoLineResults(q) {
        const n = parseInt(q, 10);
        if (!q || isNaN(n) || n < 1) {
          return [{ text: "Go to line… (type a number)", positions: [] }];
        }
        return [{
          text: "Go to line " + n,
          positions: [],
          onChoose: () => {
            closePicker();
            editor.set_cursor(n - 1, 0);
            ensureCursorVisible();
            render();
          },
        }];
      }

      function renderPickerList() {
        els.pickerList.innerHTML = "";
        pickerResults.forEach((r, i) => {
          const li = document.createElement("li");
          if (i === pickerSel) li.className = "picked";
          const label = document.createElement("span");
          label.className = "picker-label";
          label.innerHTML = highlightPath(r.text, r.positions);
          li.appendChild(label);
          if (r.hint) {
            const hint = document.createElement("span");
            hint.className = "picker-hint";
            hint.textContent = r.hint;
            li.appendChild(hint);
          }
          if (r.onChoose) {
            li.addEventListener("mousedown", (e) => {
              e.preventDefault();
              r.onChoose(e);
            });
          }
          els.pickerList.appendChild(li);
        });
      }

      function highlightPath(path, positions) {
        if (!positions || !positions.length) return escapeHtml(path);
        const set = new Set(positions);
        let out = "";
        const chars = Array.from(path);
        for (let i = 0; i < chars.length; i++) {
          const c = escapeHtml(chars[i]);
          out += set.has(i) ? "<b>" + c + "</b>" : c;
        }
        return out;
      }

      function escapeHtml(s) {
        return s.replace(/[&<>"']/g, (c) =>
          ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
        );
      }

      // URL for a file's editor page, preserving "/" path separators.
      function editorUrl(path) {
        return "/editor/" + path.split("/").map(encodeURIComponent).join("/");
      }

      // Remember the last file opened in this repo so a bare /editor reopens it.
      // Persisted server-side (keyed by repo root) rather than in localStorage:
      // the server binds a fresh random port on every start, and localStorage is
      // per-origin, so a new port would be a new origin that can't see the prior
      // session's record. The server endpoint stores it under the data dir.
      function recordLastFile(path) {
        try {
          fetch("/api/last-file", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: path }),
            keepalive: true,
          }).catch(() => {});
        } catch (_) {}
      }
      async function readLastFile() {
        try {
          const resp = await fetch("/api/last-file", { cache: "no-store" });
          if (!resp.ok) return null;
          const data = await resp.json();
          return data && typeof data.path === "string" && data.path ? data.path : null;
        } catch (_) {
          return null;
        }
      }
      function clearLastFile() {
        // Forget the record (null path) — e.g. an auto-reopened file is gone.
        try {
          fetch("/api/last-file", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: null }),
            keepalive: true,
          }).catch(() => {});
        } catch (_) {}
      }

      // Switch the editor to `path` (optionally with a #L hash) in place — no full
      // page reload. The old path called window.location.assign, which re-fetched
      // and re-instantiated the wasm module and rebuilt every listener on each
      // open; loadFile() reuses all of that and only swaps the buffer.
      async function navigateToFile(path, hash) {
        if (path === filePath && !hash) return; // already here
        // beforeunload can't guard an in-page swap, so confirm here instead.
        if (
          isModified() &&
          !window.confirm("You have unsaved changes. Discard them and switch files?")
        ) {
          return;
        }
        history.pushState({ path }, "", editorUrl(path) + (hash || ""));
        await loadFile(path, hash || "");
      }

      function openFile(path) {
        navigateToFile(path, null);
      }

      // Open a file and land the caret on a specific location. The target is
      // carried in the URL hash (#L<line>:<col>, 1-based); loadFile()/jumpToHash()
      // read it back and position the cursor once the new core is in place.
      function openFileAt(path, line, col) {
        navigateToFile(path, "#L" + (line + 1) + ":" + (col + 1));
      }

      // Back/forward between files visited in this tab. The address bar has
      // already changed, so a cancelled unsaved-changes prompt re-pushes the
      // current file to keep the URL honest; a same-file pop just re-jumps to the
      // (possibly new) #L hash.
      async function onPopState() {
        const path = pathFromLocation();
        if (path === filePath) {
          jumpToHash();
          return;
        }
        if (
          isModified() &&
          !window.confirm("You have unsaved changes. Discard them and switch files?")
        ) {
          history.pushState({ path: filePath }, "", editorUrl(filePath));
          return;
        }
        await loadFile(path, window.location.hash || "");
      }

      // Parse #L<line>:<col> (1-based) from the URL and place the caret there.
      // `col` is treated as a display column; for lines with leading tabs/CJK it
      // may be slightly off, but the line is exact. set_cursor clamps internally.
      function jumpToHash() {
        if (!editor) return;
        const m = /^#L(\d+)(?::(\d+))?$/.exec(window.location.hash);
        if (!m) return;
        const row = Math.max(0, parseInt(m[1], 10) - 1);
        const col = m[2] ? Math.max(0, parseInt(m[2], 10) - 1) : 0;
        editor.set_cursor(row, col);
        ensureCursorVisible();
        render();
      }

      // Open a file in a separate browser window/tab (Cmd+click or Alt+click in
      // the file explorer), leaving the current editor untouched.
      function openFileInNewWindow(path) {
        window.open(editorUrl(path), "_blank");
      }

      // Collapse "." / ".." segments in a repo-relative path.
      function normalizePath(p) {
        const out = [];
        for (const seg of p.split("/")) {
          if (seg === "" || seg === ".") continue;
          if (seg === "..") out.pop();
          else out.push(seg);
        }
        return out.join("/");
      }

      // Open a link target found under a Cmd+click. URLs/mailto open in a new
      // browser tab; an in-page anchor is ignored; anything else is treated as a
      // path (root-relative if it starts with "/", else relative to the current
      // file's directory) and opened in a new editor tab.
      function openLink(target) {
        if (/^(https?:)?\/\//i.test(target) || /^mailto:/i.test(target)) {
          window.open(target, "_blank");
          return;
        }
        if (target.startsWith("#")) return;
        let path = target.replace(/[#?].*$/, "");
        if (path.startsWith("/")) path = path.slice(1);
        else {
          const dir = parentDir(filePath);
          path = dir ? dir + "/" + path : path;
        }
        path = normalizePath(path);
        if (path) openFileInNewWindow(path);
      }

      // Same, but landing on a specific location (for global-search results
      // opened with Cmd/Alt+click or Alt+Enter).
      function openFileAtInNewWindow(path, line, col) {
        const hash = "#L" + (line + 1) + ":" + (col + 1);
        window.open(editorUrl(path) + hash, "_blank");
      }

      function onPickerKeyDown(e) {
        if (e.key === "Escape") {
          e.preventDefault();
          closePicker();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          const r = pickerResults[pickerSel];
          // onChoose handles the mode-specific action (open file / run command /
          // jump to line); Alt+Enter is forwarded so files can open a new window.
          if (r && r.onChoose) r.onChoose(e);
          return;
        }
        const down = e.key === "ArrowDown" || (e.ctrlKey && (e.key === "n" || e.key === "N"));
        const up = e.key === "ArrowUp" || (e.ctrlKey && (e.key === "p" || e.key === "P"));
        if (down) {
          e.preventDefault();
          movePicker(1);
        } else if (up) {
          e.preventDefault();
          movePicker(-1);
        }
      }

      function movePicker(delta) {
        if (!pickerResults.length) return;
        pickerSel = (pickerSel + delta + pickerResults.length) % pickerResults.length;
        renderPickerList();
        const sel = els.pickerList.children[pickerSel];
        if (sel) sel.scrollIntoView({ block: "nearest" });
      }

      // Fuzzy match, ported from src/ui/shared/filtering.rs so the browser
      // picker ranks files the same way the terminal file picker does.
      function fuzzyMatch(haystack, needle) {
        if (!needle) return { score: 0, positions: [] };
        return fuzzyStrict(haystack, needle) || fuzzyTokens(haystack, needle);
      }

      function fuzzyStrict(haystack, needle) {
        const hay = Array.from(haystack);
        const ndl = Array.from(needle);
        const positions = [];
        let hi = 0;
        for (const nch of ndl) {
          const nl = nch.toLowerCase();
          let found = false;
          while (hi < hay.length) {
            if (hay[hi].toLowerCase() === nl) {
              positions.push(hi);
              hi++;
              found = true;
              break;
            }
            hi++;
          }
          if (!found) return null;
        }
        return { score: fuzzyScore(hay, ndl, positions), positions };
      }

      const TOKEN_FALLBACK_PENALTY = 50;

      function fuzzyTokens(haystack, needle) {
        const tokens = needle.split(/\s+/).filter(Boolean);
        if (tokens.length < 2) return null;
        let total = 0;
        let all = [];
        for (const t of tokens) {
          const m = fuzzyStrict(haystack, t);
          if (!m) return null;
          total += m.score;
          all.push(...m.positions);
        }
        all = [...new Set(all)].sort((a, b) => a - b);
        return { score: total - TOKEN_FALLBACK_PENALTY, positions: all };
      }

      function fuzzyScore(hay, ndl, positions) {
        let score = 0;
        for (let i = 0; i < positions.length; i++) {
          const pos = positions[i];
          if (pos === 0) score += 8;
          if (pos > 0) {
            const prev = hay[pos - 1];
            if (prev === " " || prev === "_" || prev === "-" || prev === "." || prev === "/") {
              score += 8;
            }
          }
          if (i > 0 && pos === positions[i - 1] + 1) score += 12;
          if (hay[pos] === ndl[i]) score += 4; // exact-case bonus
          if (i > 0) score -= pos - positions[i - 1] - 1;
        }
        score -= Math.floor(hay.length / 4);
        return score;
      }

      // ---- Cmd+Shift+F project search --------------------------------------

      let gsearchOpen = false;
      let gsearchDebounce = null;
      let gsearchSeq = 0; // request id, to drop out-of-order responses
      // Raw hits from the server (sorted by path), the flat list of *visible*
      // selectable rows, and the set of collapsed file paths. Each row is a file
      // header {kind:'file',path} or a match {kind:'hit',path,line,col}; rows
      // under a collapsed file are omitted, so the keyboard selection skips them.
      let gsearchHits = [];
      let gsearchRows = [];
      let gsearchSel = 0;
      let gsearchQuery = "";
      let gsearchTruncated = false;
      const gsearchCollapsed = new Set();
      // Files the user hid with Backspace on their header. Temporary: cleared on
      // every new query (re-running the search brings them back).
      const gsearchRemoved = new Set();
      // True once the user has moved the selection with the arrow keys. While
      // false (right after typing) Backspace edits the query as usual; once
      // navigating the result list, Backspace on a file header hides that file.
      let gsearchNavigated = false;
      const GSEARCH_MIN = 3; // matches the server-side trigram minimum
      const GSEARCH_MAX = 500; // server caps per file, so this spans many files

      // Focus + select the project-search query. Done now and again on the next
      // frame: the immediate call covers the common path, while the deferred one
      // wins when the panel was only just unhidden or another handler grabbed
      // focus in the same tick (e.g. the command palette's close → els.ime.focus()
      // runs right before this when launched from "Search in Project").
      function focusGsearchInput() {
        els.gsearchInput.focus();
        els.gsearchInput.select();
        requestAnimationFrame(() => {
          if (!gsearchOpen) return;
          els.gsearchInput.focus();
          els.gsearchInput.select();
        });
      }

      function openGlobalSearch() {
        // Already open → just re-focus and select the query (Cmd+Shift+F again).
        if (gsearchOpen) {
          focusGsearchInput();
          return;
        }
        gsearchOpen = true;
        els.gsearch.hidden = false;
        // Seed with the current selection, if any (VSCode/Zed-style).
        const seed = editor ? editor.selection_text() : "";
        if (seed && !seed.includes("\n")) els.gsearchInput.value = seed;
        runGlobalSearch(els.gsearchInput.value);
        focusGsearchInput();
      }

      function closeGlobalSearch() {
        if (!gsearchOpen) return;
        gsearchOpen = false;
        els.gsearch.hidden = true;
        els.ime.focus();
      }

      function runGlobalSearch(query) {
        if (gsearchDebounce) clearTimeout(gsearchDebounce);
        // Editing the query means we're no longer navigating the result list, so
        // Backspace goes back to its normal text-editing behavior.
        gsearchNavigated = false;
        const q = query.trim();
        if (q.length < GSEARCH_MIN) {
          gsearchHits = [];
          els.gsearchList.innerHTML = "";
          els.gsearchStatus.textContent = "Type " + GSEARCH_MIN + "+ characters";
          return;
        }
        els.gsearchStatus.textContent = "Searching…";
        const seq = ++gsearchSeq;
        gsearchDebounce = setTimeout(async () => {
          try {
            const resp = await fetch(
              "/api/search?q=" + encodeURIComponent(q) + "&max=" + GSEARCH_MAX
            );
            if (seq !== gsearchSeq) return; // a newer query superseded this one
            if (!resp.ok) {
              const data = await resp.json().catch(() => ({}));
              els.gsearchStatus.textContent = "Search failed: " + (data.error || resp.status);
              return;
            }
            const data = await resp.json();
            if (seq !== gsearchSeq) return;
            renderGlobalSearch(q, data.hits || [], !!data.truncated);
          } catch (err) {
            if (seq === gsearchSeq) els.gsearchStatus.textContent = "Search failed: " + err;
          }
        }, 120);
      }

      // New result set from the server: reset collapse/selection, then paint.
      function renderGlobalSearch(query, hits, truncated) {
        gsearchHits = hits;
        gsearchQuery = query;
        gsearchTruncated = truncated;
        gsearchCollapsed.clear();
        gsearchRemoved.clear();
        gsearchSel = 0;

        if (!hits.length) {
          gsearchRows = [];
          els.gsearchList.innerHTML = "";
          els.gsearchStatus.textContent = "No results";
          return;
        }
        paintGsearch(); // sets the status line from the (visible) hits
      }

      // Rebuild the visible row list + DOM from `gsearchHits` and the collapse
      // set. Hits arrive sorted by path, so a run-length pass yields one header
      // per file followed by its (visible) match rows. Cheap enough to call on
      // every collapse/expand.
      function paintGsearch() {
        // Hits from files the user hasn't hidden with Backspace.
        const visible = gsearchHits.filter((h) => !gsearchRemoved.has(h.path));
        const counts = new Map();
        for (const h of visible) counts.set(h.path, (counts.get(h.path) || 0) + 1);

        const fileCount = counts.size;
        if (!visible.length) {
          els.gsearchStatus.textContent = gsearchRemoved.size
            ? "All matching files hidden"
            : "No results";
        } else {
          const hidden = gsearchRemoved.size
            ? " (" + gsearchRemoved.size + " hidden)"
            : "";
          els.gsearchStatus.textContent =
            visible.length + (gsearchTruncated ? "+" : "") +
            " results in " + fileCount + " files" + hidden;
        }

        gsearchRows = [];
        els.gsearchList.innerHTML = "";
        let curPath = null;
        visible.forEach((h) => {
          if (h.path !== curPath) {
            curPath = h.path;
            const collapsed = gsearchCollapsed.has(h.path);
            const rowIdx = gsearchRows.length;
            gsearchRows.push({ kind: "file", path: h.path });
            const head = document.createElement("li");
            head.className = "gfile";
            head.dataset.row = String(rowIdx);
            head.innerHTML =
              '<span class="gchevron">' + (collapsed ? "▸" : "▾") + "</span>" +
              escapeHtml(h.path) +
              '<span class="gcount">(' + (counts.get(h.path) || 0) + ")</span>";
            head.addEventListener("mousedown", (e) => {
              e.preventDefault();
              gsearchSel = Number(head.dataset.row);
              toggleCollapse(h.path);
            });
            els.gsearchList.appendChild(head);
          }
          if (gsearchCollapsed.has(h.path)) return; // hidden under collapsed file
          const rowIdx = gsearchRows.length;
          gsearchRows.push({ kind: "hit", path: h.path, line: h.line, col: h.col });
          const li = document.createElement("li");
          li.className = "ghit";
          li.dataset.row = String(rowIdx);
          li.innerHTML =
            '<span class="gline">' + (h.line + 1) + "</span>" +
            '<span class="gtext">' + highlightExcerpt(h.excerpt, h.col, gsearchQuery.length) + "</span>";
          li.addEventListener("mousedown", (e) => {
            e.preventDefault();
            // Cmd/Alt+click: open this match in a separate window.
            if (e.metaKey || e.altKey) openFileAtInNewWindow(h.path, h.line, h.col);
            else openFileAt(h.path, h.line, h.col);
          });
          els.gsearchList.appendChild(li);
        });
        if (gsearchSel >= gsearchRows.length) gsearchSel = gsearchRows.length - 1;
        if (gsearchSel < 0) gsearchSel = 0;
        paintGsearchSelection();
      }

      function paintGsearchSelection() {
        els.gsearchList.querySelectorAll("li").forEach((li) => {
          const on = Number(li.dataset.row) === gsearchSel;
          li.classList.toggle("picked", on);
          if (on) li.scrollIntoView({ block: "nearest" });
        });
      }

      // Bold the matched substring within an excerpt. The server gives a 0-based
      // character column; bold `qlen` chars from there (the match is literal).
      function highlightExcerpt(excerpt, col, qlen) {
        const chars = Array.from(excerpt);
        const start = Math.max(0, Math.min(col, chars.length));
        const end = Math.max(start, Math.min(col + qlen, chars.length));
        const pre = escapeHtml(chars.slice(0, start).join(""));
        const mid = escapeHtml(chars.slice(start, end).join(""));
        const post = escapeHtml(chars.slice(end).join(""));
        return pre + (mid ? "<b>" + mid + "</b>" : "") + post;
      }

      function onGsearchKeyDown(e) {
        // Cmd+Shift+F while the search is already focused: re-select the query
        // instead of falling through to any browser/editor default.
        if ((e.metaKey || e.ctrlKey) && e.shiftKey && (e.key === "f" || e.key === "F")) {
          e.preventDefault();
          els.gsearchInput.focus();
          els.gsearchInput.select();
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          closeGlobalSearch();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          const row = gsearchRows[gsearchSel];
          if (!row) return;
          if (row.kind === "hit") {
            // Alt+Enter: open this match in a separate window.
            if (e.altKey) openFileAtInNewWindow(row.path, row.line, row.col);
            else openFileAt(row.path, row.line, row.col);
          } else {
            toggleCollapse(row.path);
          }
          return;
        }
        // Backspace on a file header (while navigating the list) hides that file
        // from the results; otherwise it edits the query text as usual.
        if (e.key === "Backspace" && gsearchNavigated) {
          const row = gsearchRows[gsearchSel];
          if (row && row.kind === "file") {
            e.preventDefault();
            removeGsearchFile(row.path);
            return;
          }
        }
        // Alt+Up/Down jumps between file headers (next/previous file).
        if (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
          e.preventDefault();
          gsearchNavigated = true;
          moveGsearchFile(e.key === "ArrowDown" ? 1 : -1);
          return;
        }
        const down = e.key === "ArrowDown" || (e.ctrlKey && (e.key === "n" || e.key === "N"));
        const up = e.key === "ArrowUp" || (e.ctrlKey && (e.key === "p" || e.key === "P"));
        if (down) {
          e.preventDefault();
          gsearchNavigated = true;
          moveGsearch(1);
        } else if (up) {
          e.preventDefault();
          gsearchNavigated = true;
          moveGsearch(-1);
        } else if (e.key === "ArrowLeft") {
          e.preventDefault();
          gsearchNavigated = true;
          collapseSelection();
        } else if (e.key === "ArrowRight") {
          e.preventDefault();
          gsearchNavigated = true;
          expandSelection();
        }
      }

      function moveGsearch(delta) {
        if (!gsearchRows.length) return;
        gsearchSel = (gsearchSel + delta + gsearchRows.length) % gsearchRows.length;
        paintGsearchSelection();
      }

      // Alt+Up/Down: move the selection to the next/previous file header. Down
      // lands on the next file's header; Up lands on the nearest header at or
      // above the selection (so from a match it first jumps to that file's own
      // header, then to the previous file). Clamps at the ends.
      function moveGsearchFile(delta) {
        if (!gsearchRows.length) return;
        if (delta > 0) {
          for (let i = gsearchSel + 1; i < gsearchRows.length; i++) {
            if (gsearchRows[i].kind === "file") { gsearchSel = i; paintGsearchSelection(); return; }
          }
        } else {
          for (let i = gsearchSel - 1; i >= 0; i--) {
            if (gsearchRows[i].kind === "file") { gsearchSel = i; paintGsearchSelection(); return; }
          }
        }
      }

      // Backspace on a file header: temporarily drop that file's hits from the
      // results (until the next query re-runs the search). The selection stays
      // at the same index, which now points at the following file's header.
      function removeGsearchFile(path) {
        gsearchRemoved.add(path);
        paintGsearch();
      }

      // Left arrow: on a match, collapse its parent file and select the header;
      // on an expanded file header, collapse it.
      function collapseSelection() {
        const row = gsearchRows[gsearchSel];
        if (!row) return;
        if (row.kind === "hit") {
          selectFileHeader(row.path);
          gsearchCollapsed.add(row.path);
          paintGsearch();
        } else if (!gsearchCollapsed.has(row.path)) {
          gsearchCollapsed.add(row.path);
          paintGsearch();
        }
      }

      // Right arrow: on a collapsed header, expand it; on an expanded header,
      // step into its first match.
      function expandSelection() {
        const row = gsearchRows[gsearchSel];
        if (!row || row.kind !== "file") return;
        if (gsearchCollapsed.has(row.path)) {
          gsearchCollapsed.delete(row.path);
          paintGsearch();
        } else {
          moveGsearch(1); // first child hit sits right after the header
        }
      }

      function toggleCollapse(path) {
        if (gsearchCollapsed.has(path)) {
          gsearchCollapsed.delete(path);
        } else {
          gsearchCollapsed.add(path);
          selectFileHeader(path); // keep selection on the now-collapsed header
        }
        paintGsearch();
      }

      // Point the selection at a file's header row (so collapsing doesn't leave
      // the selection on a row that's about to be hidden).
      function selectFileHeader(path) {
        const idx = gsearchRows.findIndex((r) => r.kind === "file" && r.path === path);
        if (idx >= 0) gsearchSel = idx;
      }

      // ---- sidebar file tree -----------------------------------------------

      let treeRoot = null;
      const expandedDirs = new Set();
      // Directories created this session. The file list (`/api/files`) is built
      // from `git ls-files`, which never lists empty directories, so a freshly
      // created folder would vanish on the next rebuild unless we track it here.
      let extraDirs = new Set();
      // Repo-relative path of the keyboard-focused explorer row, or null. Drives
      // the `.focused` ring (distinct from `.active`, the currently-open file).
      let focusedPath = null;

      // Working-tree git status for the sidebar decorations: repo-relative path
      // -> "modified" | "added" | "untracked" | "deleted" | "conflict". Only
      // changed paths appear; everything else is treated as clean. `dirtyDirs`
      // holds every ancestor directory of a changed path so folders can show a
      // roll-up marker. Both are recomputed from `/api/git-status`.
      let gitStatus = new Map();
      let dirtyDirs = new Set();

      // Single-letter badge shown on a sidebar row, matching the terminal's
      // status indicators (dirs roll up to a dot).
      function statusChar(st) {
        switch (st) {
          case "modified": return "M";
          case "added": return "A";
          case "untracked": return "?";
          case "deleted": return "D";
          case "conflict": return "U";
          case "dir": return "•";
          default: return "";
        }
      }

      // Recompute `dirtyDirs` from the current `gitStatus` keys: a directory is
      // "dirty" if any file under it has a status.
      function computeDirtyDirs() {
        dirtyDirs = new Set();
        for (const path of gitStatus.keys()) {
          const parts = path.split("/");
          let acc = "";
          for (let i = 0; i < parts.length - 1; i++) {
            acc = acc ? acc + "/" + parts[i] : parts[i];
            dirtyDirs.add(acc);
          }
        }
      }

      // Fetch working-tree git status and repaint the sidebar decorations.
      async function refreshGitStatus() {
        try {
          const resp = await fetch("/api/git-status");
          const data = await resp.json();
          gitStatus = new Map(Object.entries(data.statuses || {}));
        } catch (_) {
          gitStatus = new Map();
        }
        computeDirtyDirs();
        if (treeRoot) renderTree();
      }

      // Build a nested tree from the flat file list (same set as the picker),
      // plus any session-created empty directories.
      function buildTree(files) {
        const root = { name: "", path: "", isDir: true, children: new Map() };
        // `leaf` is true for a file path (its last segment is a file), false
        // for a directory path (every segment, including the last, is a dir).
        const addPath = (p, leaf) => {
          const parts = p.split("/");
          let node = root;
          let acc = "";
          for (let i = 0; i < parts.length; i++) {
            const part = parts[i];
            acc = acc ? acc + "/" + part : part;
            const isDir = leaf ? i < parts.length - 1 : true;
            let child = node.children.get(part);
            if (!child) {
              child = { name: part, path: acc, isDir, children: new Map() };
              node.children.set(part, child);
            }
            node = child;
          }
        };
        for (const f of files) addPath(f, true);
        for (const d of extraDirs) addPath(d, false);
        return root;
      }

      // Rebuild the tree from the current file list + extra dirs, keeping the
      // expansion state, and re-render. Used after filesystem operations.
      function rebuildTree() {
        treeRoot = buildTree(allFiles || []);
        renderTree();
      }

      // Directories first, then files, each alphabetically.
      function sortedChildren(node) {
        return [...node.children.values()].sort((a, b) => {
          if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
          return a.name.localeCompare(b.name);
        });
      }

      function expandToFile(path) {
        if (!path) return;
        const parts = path.split("/");
        let acc = "";
        for (let i = 0; i < parts.length - 1; i++) {
          acc = acc ? acc + "/" + parts[i] : parts[i];
          expandedDirs.add(acc);
        }
      }

      function renderTree() {
        els.tree.innerHTML = "";
        if (!treeRoot) return;
        const frag = document.createDocumentFragment();
        renderTreeNodes(treeRoot, 0, frag);
        els.tree.appendChild(frag);
      }

      function renderTreeNodes(node, depth, parent) {
        for (const child of sortedChildren(node)) {
          const open = expandedDirs.has(child.path);
          // File rows are real anchors so native Cmd/middle-click opens a new
          // browser tab pointing at the file (a scripted window.open from a
          // mousedown gets popup-blocked and lands on root). Dirs stay <div>.
          const row = document.createElement(child.isDir ? "div" : "a");
          row.className = "tnode " + (child.isDir ? "dir" : "file");
          if (!child.isDir) {
            row.href = editorUrl(child.path);
            row.draggable = false;
          }
          if (!child.isDir && child.path === filePath) row.classList.add("active");
          if (child.path === focusedPath) row.classList.add("focused");
          row.dataset.path = child.path;
          row.dataset.dir = child.isDir ? "1" : "";
          row.style.paddingLeft = depth * 12 + 6 + "px";

          // Git decoration: files carry their own status; dirs roll up to a dot
          // when anything under them changed. The class colors the label; the
          // badge shows the one-letter status (M/A/?/D/U) on the right.
          const st = child.isDir
            ? (dirtyDirs.has(child.path) ? "dir" : null)
            : gitStatus.get(child.path) || null;
          if (st) row.classList.add("git-" + st);

          const twist = document.createElement("span");
          twist.className = "twist";
          twist.textContent = child.isDir ? (open ? "▾" : "▸") : "";
          const label = document.createElement("span");
          label.className = "tlabel";
          label.textContent = child.name;
          row.appendChild(twist);
          row.appendChild(label);
          if (st) {
            const badge = document.createElement("span");
            badge.className = "tstatus";
            badge.textContent = statusChar(st);
            row.appendChild(badge);
          }

          row.addEventListener("mousedown", (e) => {
            if (e.button !== 0) return; // let right-click open the context menu
            if (child.isDir) {
              e.preventDefault();
              if (open) expandedDirs.delete(child.path);
              else expandedDirs.add(child.path);
              focusedPath = child.path;
              renderTree();
              els.sidebar.focus(); // enter keyboard nav (preventDefault blocks the implicit focus)
              return;
            }
            // File: Cmd/Ctrl/Alt or middle click → let the anchor's href open a
            // new tab natively (don't preventDefault). Plain left click → keep
            // single-page navigation and move keyboard focus into the explorer.
            if (e.metaKey || e.ctrlKey || e.altKey || e.button === 1) return;
            e.preventDefault();
            focusedPath = child.path;
            openFile(child.path);
          });

          row.addEventListener("contextmenu", (e) => {
            e.preventDefault();
            e.stopPropagation(); // empty-area handler shouldn't also fire
            showContextMenu(e.clientX, e.clientY, child);
          });

          parent.appendChild(row);
          if (child.isDir && open) renderTreeNodes(child, depth + 1, parent);
        }
      }

      async function initSidebar() {
        const files = await loadFiles();
        treeRoot = buildTree(files);
        expandToFile(filePath);
        renderTree();
        // Decorations are best-effort and async; paint the tree first, then
        // overlay git status when it arrives (re-renders the tree).
        refreshGitStatus();
      }

      // ---- explorer keyboard navigation ------------------------------------

      // Currently-rendered rows, in visual order. Collapsed dirs' children are
      // not in the DOM, so this is exactly the navigable set.
      function explorerRows() {
        return [...els.tree.querySelectorAll(".tnode")];
      }

      // Move the keyboard focus to `path`, repaint the ring, scroll into view.
      function setExplorerFocus(path) {
        if (!path) return;
        focusedPath = path;
        renderTree();
        const row = els.tree.querySelector('.tnode[data-path="' + cssEscape(path) + '"]');
        if (row) row.scrollIntoView({ block: "nearest" });
      }

      // Minimal CSS attribute-selector escaping for repo paths (quotes/backslash).
      function cssEscape(s) {
        return s.replace(/["\\]/g, "\\$&");
      }

      // Move keyboard focus into the explorer (Cmd+Shift+E, or click). Seeds the
      // focus on the open file, else the first row.
      function focusExplorer() {
        const rows = explorerRows();
        if (!focusedPath || !rows.some((r) => r.dataset.path === focusedPath)) {
          focusedPath = filePath && rows.some((r) => r.dataset.path === filePath)
            ? filePath
            : rows[0] && rows[0].dataset.path;
        }
        renderTree();
        els.sidebar.focus();
        const row = els.tree.querySelector(".tnode.focused");
        if (row) row.scrollIntoView({ block: "nearest" });
      }

      // Enter on a row: render-preview-and-open for Markdown/HTML, else open.
      function explorerActivate(path) {
        if (!path) return;
        // Previewable → pre-arm the per-tab preview flag so the destination page
        // boots with the rendered split pane already open (reuses fix-3 restore).
        if (previewableKind(path)) {
          try { sessionStorage.setItem("gargo_preview", "true"); } catch (_) {}
        }
        openFile(path);
      }

      function onExplorerKeyDown(e) {
        if (e.metaKey || e.ctrlKey || e.altKey) return; // leave combos to global
        const rows = explorerRows();
        if (!rows.length) return;
        let idx = rows.findIndex((r) => r.dataset.path === focusedPath);
        if (idx < 0) idx = 0;
        const row = rows[idx];
        const path = row.dataset.path;
        const isDir = row.dataset.dir === "1";
        switch (e.key) {
          case "ArrowDown":
            e.preventDefault();
            setExplorerFocus(rows[Math.min(rows.length - 1, idx + 1)].dataset.path);
            break;
          case "ArrowUp":
            e.preventDefault();
            setExplorerFocus(rows[Math.max(0, idx - 1)].dataset.path);
            break;
          case "ArrowRight":
            e.preventDefault();
            if (isDir && !expandedDirs.has(path)) {
              expandedDirs.add(path);
              renderTree();
            } else if (rows[idx + 1]) {
              setExplorerFocus(rows[idx + 1].dataset.path);
            }
            break;
          case "ArrowLeft":
            e.preventDefault();
            if (isDir && expandedDirs.has(path)) {
              expandedDirs.delete(path);
              renderTree();
            } else {
              const parent = parentDir(path);
              if (parent) setExplorerFocus(parent);
            }
            break;
          case "Enter":
            e.preventDefault();
            if (isDir) {
              if (expandedDirs.has(path)) expandedDirs.delete(path);
              else expandedDirs.add(path);
              renderTree();
            } else {
              explorerActivate(path);
            }
            break;
          case "e":
          case "E":
            e.preventDefault();
            if (!isDir) openFile(path); // open in the editor (edit mode)
            break;
          case "Escape":
            e.preventDefault();
            els.ime.focus();
            break;
        }
      }

      // Draggable sidebar width.
      let resizing = false;
      function onResizerDown(e) {
        resizing = true;
        e.preventDefault();
      }
      function onResizerMove(e) {
        if (resizing) {
          const rect = els.main.getBoundingClientRect();
          let w = Math.max(120, Math.min(e.clientX - rect.left, rect.width - 200));
          document.documentElement.style.setProperty("--sidebar-w", w + "px");
          render();
        } else if (previewResizing) {
          // Preview pane is on the right: width measured from main's right edge.
          const rect = els.main.getBoundingClientRect();
          let w = Math.max(200, Math.min(rect.right - e.clientX, rect.width - 300));
          document.documentElement.style.setProperty("--preview-w", w + "px");
          render();
        }
      }
      function onResizerUp() {
        if (resizing || previewResizing) {
          resizing = false;
          previewResizing = false;
          render();
        }
      }

      // Draggable preview-pane width (only present when the preview is open).
      let previewResizing = false;
      function onPreviewResizerDown(e) {
        previewResizing = true;
        e.preventDefault();
      }

      // ---- sidebar context menu + filesystem ops ---------------------------

      // Absolute repo root injected by the server; "" if unknown (then absolute
      // == relative). Used to build absolute paths for "Copy Path".
      const repoRoot =
        typeof window.__GARGO_REPO_ROOT__ === "string"
          ? window.__GARGO_REPO_ROOT__
          : "";

      function parentDir(path) {
        const i = path.lastIndexOf("/");
        return i >= 0 ? path.slice(0, i) : "";
      }

      function absPath(rel) {
        if (!rel) return repoRoot;
        return repoRoot ? repoRoot.replace(/\/+$/, "") + "/" + rel : rel;
      }

      async function copyText(text) {
        try {
          await navigator.clipboard.writeText(text);
        } catch (err) {
          showError("Copy failed: " + err);
        }
      }

      // Items of the currently-open menu, for single-letter shortcut lookup.
      let ctxItems = [];

      // Build the menu rows for a right-clicked tree node (null = empty area /
      // repo root). New entries land in the node's own dir (for a directory),
      // its parent dir (for a file), or the root (empty area). Each item's
      // `key` is the single-letter shortcut shown on the right and accepted
      // while the menu is open.
      function buildContextItems(node) {
        const baseDir = !node ? "" : node.isDir ? node.path : parentDir(node.path);
        const rel = node ? node.path : "";
        const items = [
          { key: "a", label: "New File", action: () => createEntry(baseDir) },
          { sep: true },
          { key: "f", label: "Reveal in Finder", action: () => revealInFinder(rel) },
          { sep: true },
          { key: "p", label: "Copy Path", action: () => copyText(absPath(rel)) },
        ];
        if (node) {
          items.push({ key: "c", label: "Copy Relative Path", action: () => copyText(rel) });
          items.push({ sep: true });
          items.push({ key: "r", label: "Rename", action: () => renameEntry(node) });
          items.push({ key: "d", label: "Delete", action: () => deleteEntry(node) });
        }
        return items;
      }

      function showContextMenu(x, y, node) {
        closeContextMenu();
        const menu = els.ctxmenu;
        menu.innerHTML = "";
        ctxItems = buildContextItems(node);
        for (const item of ctxItems) {
          if (item.sep) {
            const sep = document.createElement("div");
            sep.className = "ctxsep";
            menu.appendChild(sep);
            continue;
          }
          const el = document.createElement("div");
          el.className = "ctxitem";
          const label = document.createElement("span");
          label.textContent = item.label;
          el.appendChild(label);
          const hintText = item.hint || (item.key ? item.key.toUpperCase() : "");
          if (hintText) {
            const hint = document.createElement("span");
            hint.className = "ctxhint";
            hint.textContent = hintText;
            el.appendChild(hint);
          }
          // preventDefault on mousedown keeps the editor from stealing focus
          // before the click fires.
          el.addEventListener("mousedown", (ev) => ev.preventDefault());
          el.addEventListener("click", () => {
            closeContextMenu();
            item.action();
          });
          menu.appendChild(el);
        }
        menu.hidden = false;
        // Position, clamped so the menu stays inside the viewport.
        const rect = menu.getBoundingClientRect();
        const mx = Math.min(x, window.innerWidth - rect.width - 8);
        const my = Math.min(y, window.innerHeight - rect.height - 8);
        menu.style.left = Math.max(4, mx) + "px";
        menu.style.top = Math.max(4, my) + "px";
      }

      function closeContextMenu() {
        els.ctxmenu.hidden = true;
      }

      // A small modal text prompt; resolves to the entered string (trimmed) or
      // null if cancelled. `selectBasename` pre-selects the name before its
      // extension (for Rename).
      let fspromptResolve = null;
      function promptInput({ title, initial, selectBasename }) {
        return new Promise((resolve) => {
          fspromptResolve = resolve;
          els.fspromptTitle.textContent = title || "";
          els.fsprompt.hidden = false;
          const input = els.fspromptInput;
          input.value = initial || "";
          input.focus();
          if (selectBasename && initial) {
            const dot = initial.lastIndexOf(".");
            input.setSelectionRange(0, dot > 0 ? dot : initial.length);
          } else {
            const n = input.value.length;
            input.setSelectionRange(n, n);
          }
        });
      }

      function closeFsPrompt(value) {
        if (!fspromptResolve) return;
        els.fsprompt.hidden = true;
        const resolve = fspromptResolve;
        fspromptResolve = null;
        resolve(value);
        els.ime.focus();
      }

      function onFsPromptKeyDown(e) {
        if (e.key === "Enter") {
          e.preventDefault();
          closeFsPrompt(els.fspromptInput.value.trim());
        } else if (e.key === "Escape") {
          e.preventDefault();
          closeFsPrompt(null);
        }
        e.stopPropagation();
      }

      async function createEntry(baseDir) {
        const where = baseDir ? baseDir + "/" : "project root";
        const raw = await promptInput({
          title: "New file in " + where + " — end the name with / to make a folder",
          initial: "",
        });
        await createFromRaw(baseDir, raw);
      }

      // Create a file/dir from a raw, possibly-nested name typed by the user.
      // A trailing slash means "directory"; otherwise it's a file. Nested names
      // (e.g. `some/path/here.md`) create the intermediate dirs server-side.
      // Shared by the context-menu prompt and the inline double-click input.
      async function createFromRaw(baseDir, raw) {
        if (!raw) return;
        const kind = raw.endsWith("/") ? "dir" : "file";
        const name = raw.replace(/\/+$/, "");
        if (!name) return;
        const path = baseDir ? baseDir + "/" + name : name;
        try {
          const resp = await fetch("/api/fs/create", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path, kind }),
          });
          if (!resp.ok) {
            const data = await resp.json().catch(() => ({}));
            showError("Create failed: " + (data.error || resp.status));
            return;
          }
        } catch (err) {
          showError("Create failed: " + err);
          return;
        }
        // Reflect the new entry locally: new files come back from /api/files on
        // a reload, but empty dirs never do, so track them in extraDirs.
        expandToFile(path); // expand ancestor dirs so the entry is visible
        if (kind === "dir") {
          extraDirs.add(path);
          expandedDirs.add(path);
          rebuildTree();
          refreshGitStatus();
        } else {
          if (allFiles && !allFiles.includes(path)) allFiles.push(path);
          // Opening the file is a full navigation; it'll rebuild on load.
          openFile(path);
        }
      }

      // Inline new-entry input: a temporary textbox appended to the tree (used
      // on double-click of empty sidebar space). Enter creates, Esc/blur cancels.
      let inlineActive = false;
      function startInlineCreate(baseDir) {
        if (inlineActive) return;
        inlineActive = true;
        const row = document.createElement("div");
        row.className = "tnode";
        row.style.paddingLeft = "6px";
        const input = document.createElement("input");
        input.className = "tinput";
        input.placeholder = "name… (end with / for a folder)";
        row.appendChild(input);
        els.tree.appendChild(row);
        input.focus();

        let done = false;
        const cleanup = () => {
          if (done) return;
          done = true;
          inlineActive = false;
          row.remove();
        };
        input.addEventListener("keydown", (e) => {
          e.stopPropagation(); // keep keys out of the editor core
          if (e.key === "Enter") {
            e.preventDefault();
            const raw = input.value.trim();
            cleanup();
            createFromRaw(baseDir, raw);
          } else if (e.key === "Escape") {
            e.preventDefault();
            cleanup();
            els.ime.focus();
          }
        });
        // Clicking away cancels (runs after a possible Enter, which is fine —
        // cleanup is idempotent).
        input.addEventListener("blur", cleanup);
      }

      async function renameEntry(node) {
        const newName = await promptInput({
          title: "Rename " + node.path,
          initial: node.name,
          selectBasename: !node.isDir,
        });
        if (!newName || newName === node.name) return;
        const base = parentDir(node.path);
        const to = base ? base + "/" + newName : newName;
        try {
          const resp = await fetch("/api/fs/rename", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ from: node.path, to }),
          });
          if (!resp.ok) {
            const data = await resp.json().catch(() => ({}));
            showError("Rename failed: " + (data.error || resp.status));
            return;
          }
        } catch (err) {
          showError("Rename failed: " + err);
          return;
        }
        // If the renamed entry contains the file currently open, follow it to
        // its new path (a full navigation that reloads the tree).
        if (filePath === node.path || filePath.startsWith(node.path + "/")) {
          openFile(to + filePath.slice(node.path.length));
          return;
        }
        applyRenameLocal(node.path, to, node.isDir);
        rebuildTree();
        refreshGitStatus();
      }

      // Rewrite the cached file list / extra dirs / expansion state to reflect a
      // rename of `from` → `to` (a file, or a directory and everything under it).
      function applyRenameLocal(from, to, isDir) {
        const remap = (p) => {
          if (p === from) return to;
          if (isDir && p.startsWith(from + "/")) return to + p.slice(from.length);
          return p;
        };
        if (allFiles) allFiles = allFiles.map(remap);
        extraDirs = new Set([...extraDirs].map(remap));
        if (expandedDirs.has(from)) {
          expandedDirs.delete(from);
          expandedDirs.add(to);
        }
      }

      async function revealInFinder(path) {
        try {
          const resp = await fetch("/api/fs/reveal", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path }),
          });
          if (!resp.ok) {
            const data = await resp.json().catch(() => ({}));
            showError("Reveal failed: " + (data.error || resp.status));
          }
        } catch (err) {
          showError("Reveal failed: " + err);
        }
      }

      async function deleteEntry(node) {
        const msg =
          "Delete " + node.path + (node.isDir ? "/ and everything in it?" : "?");
        if (!window.confirm(msg)) return;
        try {
          const resp = await fetch("/api/fs/delete", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: node.path }),
          });
          if (!resp.ok) {
            const data = await resp.json().catch(() => ({}));
            showError("Delete failed: " + (data.error || resp.status));
            return;
          }
        } catch (err) {
          showError("Delete failed: " + err);
          return;
        }
        // If the open file was deleted (or lived under a deleted dir), leave it
        // by returning to the editor's no-file state (which opens the picker).
        if (filePath === node.path || filePath.startsWith(node.path + "/")) {
          window.location.assign("/editor");
          return;
        }
        applyDeleteLocal(node.path, node.isDir);
        rebuildTree();
        refreshGitStatus();
      }

      // Drop a deleted file/dir (and anything under it) from the cached file
      // list, extra dirs, and expansion state, then the caller rebuilds.
      function applyDeleteLocal(path, isDir) {
        const under = (p) => p === path || (isDir && p.startsWith(path + "/"));
        if (allFiles) allFiles = allFiles.filter((f) => !under(f));
        extraDirs = new Set([...extraDirs].filter((d) => !under(d)));
        for (const d of [...expandedDirs]) if (under(d)) expandedDirs.delete(d);
      }

      // ---- Cmd+F in-file search --------------------------------------------

      let findOpen = false;
      let searchMatches = []; // [{start, end, row, start_char, end_char}]
      let searchIndex = -1; // index into searchMatches of the current match
      let findCurrentSelected = false; // is the current match the live selection?
      let caseSensitive = false;
      let wholeWord = false;
      let useRegex = false;

      function openFind(withReplace) {
        // VSCode prefills with the current selection (single-line only).
        const sel = editor.selection_text();
        if (sel && !sel.includes("\n")) els.findInput.value = sel;
        findOpen = true;
        els.find.hidden = false;
        if (withReplace) els.find.classList.add("with-replace");
        runFind(true);
        els.findInput.focus();
        els.findInput.select();
      }

      function closeFind() {
        if (!findOpen) return;
        findOpen = false;
        els.find.hidden = true;
        searchMatches = [];
        searchIndex = -1;
        findCurrentSelected = false;
        render();
        els.ime.focus();
      }

      function toggleReplaceRow() {
        els.find.classList.toggle("with-replace");
        els.findInput.focus();
      }

      // Recompute matches for the current query. When `jump`, also move to the
      // match at/after the caret; otherwise keep the current index (used after
      // edits that shift offsets around).
      function runFind(jump) {
        const q = els.findInput.value;
        if (!q) {
          searchMatches = [];
          searchIndex = -1;
          findCurrentSelected = false;
          updateFindCount();
          render();
          return;
        }
        try {
          searchMatches = editor.find(q, caseSensitive, wholeWord, useRegex) || [];
        } catch (_) {
          searchMatches = [];
        }
        if (!searchMatches.length) {
          searchIndex = -1;
          findCurrentSelected = false;
          updateFindCount();
          render();
          return;
        }
        if (jump) {
          const off = editor.cursor_offset();
          let idx = searchMatches.findIndex((m) => m.start >= off);
          if (idx < 0) idx = 0;
          gotoMatch(idx);
        } else {
          if (searchIndex < 0 || searchIndex >= searchMatches.length) searchIndex = 0;
          // Offsets shifted under us; don't claim the stale selection is current.
          findCurrentSelected = false;
          updateFindCount();
          render();
        }
      }

      // Select (highlight + scroll to) the match at `idx`, wrapping around.
      function gotoMatch(idx) {
        if (!searchMatches.length) return;
        searchIndex = (idx + searchMatches.length) % searchMatches.length;
        const m = searchMatches[searchIndex];
        editor.select_range(m.start, m.end);
        findCurrentSelected = true;
        ensureCursorVisible();
        updateFindCount();
        render();
      }

      function findNext() {
        if (searchMatches.length) gotoMatch(searchIndex + 1);
      }

      function findPrev() {
        if (searchMatches.length) gotoMatch(searchIndex - 1);
      }

      function updateFindCount() {
        const n = searchMatches.length;
        if (!els.findInput.value) els.findCount.textContent = "";
        else if (!n) els.findCount.textContent = "No results";
        else els.findCount.textContent = searchIndex + 1 + " of " + n;
      }

      function toggleCase() {
        caseSensitive = !caseSensitive;
        els.findCase.classList.toggle("on", caseSensitive);
        runFind(true);
        els.findInput.focus();
      }

      function toggleWord() {
        wholeWord = !wholeWord;
        els.findWord.classList.toggle("on", wholeWord);
        runFind(true);
        els.findInput.focus();
      }

      function toggleRegex() {
        useRegex = !useRegex;
        els.findRegex.classList.toggle("on", useRegex);
        runFind(true);
        els.findInput.focus();
      }

      // Replace the current match, then advance to the next one (VSCode-style).
      function replaceOne() {
        if (searchIndex < 0 || searchIndex >= searchMatches.length) return;
        const m = searchMatches[searchIndex];
        editor.replace_range(m.start, m.end, els.replaceInput.value);
        // The edit shifted offsets; recompute and jump to the next match at/
        // after the caret (which now sits just past the replacement).
        ensureCursorVisible();
        render();
        scheduleHighlight();
        scheduleGitGutter();
        runFind(true);
      }

      function replaceAll() {
        const q = els.findInput.value;
        if (!q) return;
        const n = editor.replace_all(
          q,
          els.replaceInput.value,
          caseSensitive,
          wholeWord,
          useRegex
        );
        ensureCursorVisible();
        render();
        scheduleHighlight();
        scheduleGitGutter();
        runFind(false);
        if (n) els.findCount.textContent = "Replaced " + n;
      }

      function onReplaceKeyDown(e) {
        if (e.key === "Escape") {
          e.preventDefault();
          closeFind();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          if (e.metaKey || e.altKey) replaceAll();
          else replaceOne();
        }
      }

      function onFindKeyDown(e) {
        if (e.key === "Escape") {
          e.preventDefault();
          closeFind();
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          if (e.shiftKey) findPrev();
          else findNext();
          return;
        }
        // Cmd+F while open re-selects the query (VSCode focus+select behaviour).
        if (e.metaKey && (e.key === "f" || e.key === "F")) {
          e.preventDefault();
          els.findInput.select();
        }
      }

      // ---- save ------------------------------------------------------------

      async function save() {
        const content = editor.content();
        try {
          const resp = await fetch("/api/save", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: filePath, base_hash: baseHash, content }),
          });
          if (resp.status === 409) {
            const data = await resp.json();
            const overwrite = window.confirm(
              "This file changed on disk since you opened it.\n\n" +
                "OK = overwrite with your version\nCancel = keep editing (don't save)"
            );
            if (overwrite) {
              baseHash = data.current_hash;
              await save();
            }
            return;
          }
          if (!resp.ok) {
            const data = await resp.json().catch(() => ({}));
            showError("Save failed: " + (data.error || resp.status));
            return;
          }
          const data = await resp.json();
          baseHash = data.hash;
          // New baseline: the content we just persisted is now the "original",
          // so an edit-then-undo back to it reads as unmodified again.
          baseContent = content;
          modifiedVersion = -1;
          els.dirty.textContent = "✓ saved";
          // Saving changes the working tree, so refresh the sidebar git decorations.
          refreshGitStatus();
          setTimeout(() => render(), 800);
        } catch (err) {
          showError("Save failed: " + err);
        }
      }

      // ---- boot ------------------------------------------------------------

      // Repo-relative path the URL currently points at.
      function pathFromLocation() {
        return decodeURIComponent(
          window.location.pathname.replace(/^\/editor\/?/, "")
        );
      }

      // Load (or swap to) a file in place — no full page reload. Re-instantiating
      // the wasm module and attaching listeners is boot()'s one-time job; this
      // runs on every file switch and rebuilds only what's file-specific. Returns
      // true if the file failed to load.
      async function loadFile(path, hash) {
        filePath = path;
        els.path.textContent = filePath || "(no file — Cmd+P to open)";
        document.title =
          (filePath ? filePath.split("/").pop() : "(no file)") + " — gargo";

        let content = "";
        let loadFailed = false;
        if (filePath) {
          try {
            const resp = await fetch("/api/file?path=" + encodeURIComponent(filePath));
            if (!resp.ok) {
              loadFailed = true;
              const data = await resp.json().catch(() => ({}));
              showError("Open failed: " + (data.error || resp.status));
            } else {
              const data = await resp.json();
              content = data.content;
              baseHash = data.hash;
              recordLastFile(filePath); // remember for the next bare /editor visit
            }
          } catch (err) {
            loadFailed = true;
            showError("Open failed: " + err);
          }
        }

        // Drop the previous core so its wasm allocation is freed now rather than
        // whenever the FinalizationRegistry happens to run.
        if (editor) {
          try { editor.free(); } catch (_) {}
        }
        editor = new WebEditor(filePath, content);
        // Baseline for the "● modified" indicator. Read it back from the editor
        // (not the raw server string) so any normalization the core applies on
        // load doesn't make a freshly-opened file look modified.
        baseContent = editor.content();
        modifiedVersion = -1;
        modifiedValue = false;
        // Always-insert: switch into Insert mode once on load (Normal 'i' →
        // ChangeMode(Insert); also begins the undo transaction). Escape is
        // swallowed in onKeyDown so we stay here.
        editor.key("i", false, false, false);

        // Reset everything scoped to the file we just left. renderedWrap = null
        // makes the next render() tear down and rebuild the surface DOM for the
        // new document (resetSurface), so stale rows/carets/spans can't survive.
        renderedWrap = null;
        lastModel = null;
        lastVersion = -1;
        maxCols = 0;
        highlightSpans = new Map();
        highlightedVersion = -1;
        gitGutter = new Map();
        gitGutterVersion = -1;
        if (highlightTimer) { clearTimeout(highlightTimer); highlightTimer = null; }
        if (gitGutterTimer) { clearTimeout(gitGutterTimer); gitGutterTimer = null; }

        // A find/preview tied to the previous buffer is meaningless now: drop the
        // find overlay and reconcile the preview pane against the new file. The
        // project-search pane is a full-area overlay (position:absolute; inset:0),
        // so leaving it open after jumping to a result would both hide the file we
        // just opened and cover the Cmd+F find widget; close it on every in-tab
        // navigation. Opening a result in a new window goes through window.open()
        // and never reaches loadFile, so that tab's pane correctly stays open.
        if (findOpen) closeFind();
        if (gsearchOpen) closeGlobalSearch();
        syncPreview();

        els.ime.focus();
        render();

        // Caret target carried in #L<line>:<col> (needs the new core in place).
        if (filePath) jumpToHash();

        // Reuse the in-memory sidebar tree: expand to and re-mark the active row
        // (renderTree reads filePath), with no /api/files refetch.
        expandToFile(filePath);
        renderTree();

        // Populate syntax highlight + git gutter for the new file.
        if (filePath) fetchHighlight();
        if (filePath) fetchGitGutter();

        return loadFailed;
      }

      async function boot() {
        await init();
        measureCharWidth();

        // Warn before leaving (tab close or reload) while there are unsaved edits.
        // In-page file switches are guarded separately (navigateToFile/onPopState)
        // since beforeunload doesn't fire for them. Native browsers ignore custom
        // text and show their own prompt; setting returnValue is what triggers it.
        window.addEventListener("beforeunload", (e) => {
          if (isModified()) {
            e.preventDefault();
            e.returnValue = "";
          }
        });

        els.ime.addEventListener("keydown", onKeyDown);
        els.ime.addEventListener("compositionstart", onCompositionStart);
        els.ime.addEventListener("compositionupdate", onCompositionUpdate);
        els.ime.addEventListener("compositionend", onCompositionEnd);
        els.ime.addEventListener("input", onInput);
        els.ime.addEventListener("paste", onPaste);
        // In wrap mode rows flow and overlays scroll with the content, so a
        // scroll needs no re-render; the virtualized renderer does need one.
        els.scroller.addEventListener("scroll", () => {
          if (!renderedWrap) render();
        });
        els.scroller.addEventListener("mousedown", onMouseDown);
        window.addEventListener("mousemove", onMouseMove);
        window.addEventListener("mouseup", onMouseUp);

        // Links underline + show a pointer on hover only (see .elink:hover in
        // editor.css); Cmd/Ctrl+click opens them (see onMouseDown). A previous
        // Cmd/Ctrl-held toggle flickered while modifier keys were pressed.
        window.addEventListener("resize", () => render());

        // Top-right "?" rail button opens the shortcuts overlay. Esc closes it
        // (capture phase so it runs before the ime keydown handler, which
        // otherwise swallows Escape to stay in insert mode).
        const helpBtn = document.querySelector(".app-rail-help");
        if (helpBtn) helpBtn.addEventListener("click", openHelp);
        window.addEventListener("keydown", (e) => {
          if (helpOpen() && e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            closeHelp();
          }
        }, true);
        els.pickerInput.addEventListener("input", (e) => updatePicker(e.target.value));
        els.pickerInput.addEventListener("keydown", onPickerKeyDown);
        els.gsearchInput.addEventListener("input", (e) => runGlobalSearch(e.target.value));
        els.gsearchInput.addEventListener("keydown", onGsearchKeyDown);
        els.gsearchClose.addEventListener("mousedown", (e) => e.preventDefault());
        els.gsearchClose.addEventListener("click", closeGlobalSearch);
        els.findInput.addEventListener("input", () => runFind(true));
        els.findInput.addEventListener("keydown", onFindKeyDown);
        els.replaceInput.addEventListener("keydown", onReplaceKeyDown);
        // mousedown preventDefault keeps focus in the find/replace input when
        // clicking the widget's buttons (so the editor doesn't steal it).
        for (const [el, fn] of [
          [els.findExpand, toggleReplaceRow],
          [els.findCase, toggleCase],
          [els.findWord, toggleWord],
          [els.findRegex, toggleRegex],
          [els.findPrev, findPrev],
          [els.findNext, findNext],
          [els.findClose, closeFind],
          [els.replaceOne, replaceOne],
          [els.replaceAll, replaceAll],
        ]) {
          el.addEventListener("mousedown", (e) => e.preventDefault());
          el.addEventListener("click", fn);
        }
        els.sidebarResizer.addEventListener("mousedown", onResizerDown);
        els.previewResizer.addEventListener("mousedown", onPreviewResizerDown);
        window.addEventListener("mousemove", onResizerMove);
        window.addEventListener("mouseup", onResizerUp);

        // Right-click on empty sidebar space → root-level context menu. Bound
        // to the whole sidebar so the empty area below a short file list counts;
        // rows stop propagation, so this only fires on truly empty space.
        els.sidebar.addEventListener("contextmenu", (e) => {
          if (e.target.closest(".tnode") || e.target.closest("#tree-head")) return;
          e.preventDefault();
          showContextMenu(e.clientX, e.clientY, null);
        });
        // Double-click empty sidebar space → inline new-entry textbox at root.
        els.sidebar.addEventListener("dblclick", (e) => {
          if (e.target.closest(".tnode") || e.target.closest("#tree-head")) return;
          startInlineCreate("");
        });
        // Dismiss the context menu on any outside click, scroll, or blur.
        window.addEventListener("mousedown", (e) => {
          if (!els.ctxmenu.hidden && !els.ctxmenu.contains(e.target)) {
            closeContextMenu();
          }
        });
        window.addEventListener("blur", closeContextMenu);
        document.addEventListener("scroll", closeContextMenu, true);
        // While the menu is open, Esc closes it and single letters fire the
        // matching item (capture phase, so the editor never sees them).
        document.addEventListener(
          "keydown",
          (e) => {
            if (els.ctxmenu.hidden) return;
            if (e.key === "Escape") {
              e.preventDefault();
              e.stopPropagation();
              closeContextMenu();
              return;
            }
            if (e.metaKey || e.ctrlKey || e.altKey) return; // leave combos alone
            const item = ctxItems.find((it) => it.key === e.key.toLowerCase());
            if (item) {
              e.preventDefault();
              e.stopPropagation();
              closeContextMenu();
              item.action();
            }
          },
          true,
        );
        // Modal name prompt: keys, and click-on-backdrop to cancel.
        els.fspromptInput.addEventListener("keydown", onFsPromptKeyDown);
        els.fsprompt.addEventListener("mousedown", (e) => {
          if (e.target === els.fsprompt) closeFsPrompt(null);
        });

        // Keyboard navigation in the file explorer. Bound to the sidebar so it
        // only fires when the sidebar (not the editor textarea) holds focus —
        // the two never collide because DOM focus is mutually exclusive.
        els.sidebar.setAttribute("tabindex", "0");
        els.sidebar.addEventListener("keydown", onExplorerKeyDown);

        // Back/forward between files visited in this tab (in-page swaps).
        window.addEventListener("popstate", onPopState);

        // Populate the sidebar file tree once; it's reused across file switches.
        initSidebar();

        // One-shot flag distinguishing an auto-reopened file from a directly
        // navigated one, so a since-deleted auto file falls back to the picker
        // (instead of looping).
        let autoOpened = false;
        try {
          autoOpened = sessionStorage.getItem("gargo_autoopen") === "1";
          sessionStorage.removeItem("gargo_autoopen");
        } catch (_) {}

        // Load the file named in the URL. loadFile handles render, jump-to-hash,
        // preview, highlight, and the sidebar active row.
        const initialPath = pathFromLocation();
        const loadFailed = await loadFile(initialPath, window.location.hash || "");

        // Opened without a file (the rail's "Editor" link): reopen the last file
        // for this repo if we have one, else jump straight to the file picker.
        if (!initialPath) {
          const last = await readLastFile();
          if (last) {
            // replaceState so the bare /editor entry doesn't pollute history.
            history.replaceState({ path: last }, "", editorUrl(last));
            const failed = await loadFile(last, "");
            if (failed) {
              // The remembered file is gone (deleted/renamed) → forget it.
              clearLastFile();
              openPicker();
            }
            return;
          }
          openPicker();
        } else if (loadFailed && autoOpened) {
          // The auto-reopened file is gone (deleted/renamed) → forget it and let
          // the user pick, rather than sitting on a broken buffer.
          clearLastFile();
          openPicker();
        }
      }

      boot();

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
      let charWidth = 8;
      let gutterWidth = 50;
      let lastVersion = -1;
      let composing = false;
      let maxCols = 0; // widest row seen so far, for horizontal scroll sizing
      // Syntax highlight: spans per line (char offsets into the expanded row
      // text), computed server-side (tree-sitter) and refreshed on a debounce.
      let highlightSpans = new Map(); // lineIdx -> [{start, end, scope}]
      // Git change gutter: per-line status computed server-side (gix is
      // native-only, so the wasm core can't produce it), refreshed on a debounce.
      let gitGutter = new Map(); // lineIdx -> "added" | "modified" | "deleted"

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

      // Paint one row, coloring it with syntax spans when available. `spans` are
      // {start, end, scope} char ranges into `text` (the expanded line). Falls
      // back to plain text (no highlight / no spans for this line).
      function paintRow(rowEl, text, spans) {
        if (!spans || !spans.length) {
          rowEl.textContent = text;
          return;
        }
        const chars = Array.from(text);
        const len = chars.length;
        let html = "";
        let pos = 0;
        for (const s of spans) {
          // Clamp each span's start to `pos` so overlapping/nested captures
          // (e.g. a token tagged both "type" and "constructor") never re-emit
          // text already painted — that produced doubled words like "SomeSome".
          let from = Math.max(pos, Math.min(s.start, len));
          const to = Math.max(from, Math.min(s.end, len));
          if (to <= from) continue; // fully covered by an earlier span
          if (from > pos) html += escapeHtml(chars.slice(pos, from).join(""));
          html +=
            '<span class="tok-' +
            s.scope +
            '">' +
            escapeHtml(chars.slice(from, to).join("")) +
            "</span>";
          pos = to;
        }
        if (pos < len) html += escapeHtml(chars.slice(pos).join(""));
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
          render();
        } catch (_) {
          // Gutter is best-effort; ignore network/parse errors.
        }
      }

      function syncStatus(model) {
        els.mode.textContent = model.mode;
        els.dirty.textContent = editor.is_dirty() ? "● modified" : "";
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

      // ---- cursor-follow scrolling (vertical + horizontal) -----------------

      function ensureCursorVisible() {
        if (!editor) return;
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

      // Apply an edit/motion, then follow the caret and repaint.
      function afterEdit() {
        ensureCursorVisible();
        render();
        scheduleHighlight();
        scheduleGitGutter();
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
          if (!editor.has_selection()) editor.key(...ext); // extend; no-op replaces nothing
          editor.delete_selection();
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
        els.preedit.textContent = e.data || "";
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
        if (text) insertReplacing(text);
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

        // Cmd+click a link (markdown [text](target) or a bare URL) → open it in
        // another tab: http(s) in a new browser tab, a repo/relative path in a
        // new editor tab. Falls through to normal caret placement if no link.
        if (e.metaKey) {
          const entry = mountedRows.get(row);
          const target = entry ? linkAt(entry.row.textContent, col) : null;
          if (target) {
            e.preventDefault();
            openLink(target);
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
          onChoose: (e) => (e && (e.metaKey || e.altKey) ? openFileInNewWindow(p) : openFile(p)),
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
        if (!q) return COMMANDS.map((c) => toResult(c, []));
        const scored = [];
        for (const c of COMMANDS) {
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

      function openFile(path) {
        // Full navigation; boot() loads the file.
        window.location.assign(editorUrl(path));
      }

      // Open a file and land the caret on a specific location. The target is
      // carried in the URL hash (#L<line>:<col>, 1-based) so it survives the
      // full-page navigation; boot() parses it and positions the cursor.
      function openFileAt(path, line, col) {
        const hash = "#L" + (line + 1) + ":" + (col + 1);
        window.location.assign(editorUrl(path) + hash);
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

      // Find a link covering display column `col` in `text`: a markdown
      // `[label](target)` (returns target) or a bare/autolinked http(s) URL.
      // Returns the target string, or null when the click isn't on a link.
      function linkAt(text, col) {
        let m;
        const md = /\[[^\]]*\]\(([^)\s]+)[^)]*\)/g;
        while ((m = md.exec(text))) {
          if (col >= m.index && col <= m.index + m[0].length) return m[1];
        }
        const url = /<?(https?:\/\/[^\s)>\]]+)>?/g;
        while ((m = url.exec(text))) {
          if (col >= m.index && col <= m.index + m[0].length) return m[1];
        }
        return null;
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

      function openGlobalSearch() {
        // Already open → just re-focus and select the query (Cmd+Shift+F again).
        if (gsearchOpen) {
          els.gsearchInput.focus();
          els.gsearchInput.select();
          return;
        }
        gsearchOpen = true;
        els.gsearch.hidden = false;
        // Seed with the current selection, if any (VSCode/Zed-style).
        const seed = editor ? editor.selection_text() : "";
        if (seed && !seed.includes("\n")) els.gsearchInput.value = seed;
        runGlobalSearch(els.gsearchInput.value);
        els.gsearchInput.focus();
        els.gsearchInput.select();
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
          const row = document.createElement("div");
          row.className = "tnode " + (child.isDir ? "dir" : "file");
          if (!child.isDir && child.path === filePath) row.classList.add("active");
          row.style.paddingLeft = depth * 12 + 6 + "px";

          const twist = document.createElement("span");
          twist.className = "twist";
          twist.textContent = child.isDir ? (open ? "▾" : "▸") : "";
          const label = document.createElement("span");
          label.className = "tlabel";
          label.textContent = child.name;
          row.appendChild(twist);
          row.appendChild(label);

          row.addEventListener("mousedown", (e) => {
            if (e.button !== 0) return; // let right-click open the context menu
            e.preventDefault();
            if (child.isDir) {
              if (open) expandedDirs.delete(child.path);
              else expandedDirs.add(child.path);
              renderTree();
            } else if (e.metaKey || e.altKey) {
              // Cmd/Alt+click: open in a separate window.
              openFileInNewWindow(child.path);
            } else {
              openFile(child.path);
            }
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
      }

      // Draggable sidebar width.
      let resizing = false;
      function onResizerDown(e) {
        resizing = true;
        e.preventDefault();
      }
      function onResizerMove(e) {
        if (!resizing) return;
        const rect = els.main.getBoundingClientRect();
        let w = Math.max(120, Math.min(e.clientX - rect.left, rect.width - 200));
        document.documentElement.style.setProperty("--sidebar-w", w + "px");
        render();
      }
      function onResizerUp() {
        if (resizing) {
          resizing = false;
          render();
        }
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
          els.dirty.textContent = "✓ saved";
          setTimeout(() => render(), 800);
        } catch (err) {
          showError("Save failed: " + err);
        }
      }

      // ---- boot ------------------------------------------------------------

      async function boot() {
        await init();
        measureCharWidth();

        filePath = decodeURIComponent(
          window.location.pathname.replace(/^\/editor\/?/, "")
        );
        els.path.textContent = filePath || "(no file — Cmd+P to open)";

        let content = "";
        if (filePath) {
          try {
            const resp = await fetch("/api/file?path=" + encodeURIComponent(filePath));
            if (!resp.ok) {
              const data = await resp.json().catch(() => ({}));
              showError("Open failed: " + (data.error || resp.status));
            } else {
              const data = await resp.json();
              content = data.content;
              baseHash = data.hash;
            }
          } catch (err) {
            showError("Open failed: " + err);
          }
        }

        editor = new WebEditor(filePath, content);
        // Always-insert: switch into Insert mode once at boot (Normal 'i' →
        // ChangeMode(Insert); also begins the undo transaction). Escape is
        // swallowed in onKeyDown so we stay here.
        editor.key("i", false, false, false);

        els.ime.addEventListener("keydown", onKeyDown);
        els.ime.addEventListener("compositionstart", onCompositionStart);
        els.ime.addEventListener("compositionupdate", onCompositionUpdate);
        els.ime.addEventListener("compositionend", onCompositionEnd);
        els.ime.addEventListener("input", onInput);
        els.ime.addEventListener("paste", onPaste);
        els.scroller.addEventListener("scroll", () => render());
        els.scroller.addEventListener("mousedown", onMouseDown);
        window.addEventListener("mousemove", onMouseMove);
        window.addEventListener("mouseup", onMouseUp);
        window.addEventListener("resize", () => render());
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

        els.ime.focus();
        render();

        // Jump to a location passed in the URL hash (#L<line>:<col>, 1-based),
        // e.g. when opened from a global-search result. Clamp defensively.
        if (filePath) jumpToHash();

        // Populate the sidebar file tree and the initial syntax highlight.
        initSidebar();
        if (filePath) fetchHighlight();
        if (filePath) fetchGitGutter();

        // Opened without a file (the rail's "Editor" link) → jump straight to
        // the file picker.
        if (!filePath) openPicker();
      }

      boot();

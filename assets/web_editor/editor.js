// Browser front-end for the gargo editor.
//
// Architecture (mirrors CodeMirror/Monaco): the text is rendered as plain DOM
// rows, the caret/selection are drawn as overlays, and a hidden <textarea>
// captures keystrokes and IME composition. All editing runs locally in the
// wasm `WebEditor`; the server only reads the file on open and saves it (with
// conflict detection) on Ctrl/Cmd+S.

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
};

let editor = null;
let filePath = "";
let baseHash = ""; // hash of the content we loaded (for conflict detection)
let charWidth = 8;
let gutterWidth = 50;
let lastVersion = -1;
let composing = false;
let imeIgnore = false; // composition started outside Insert mode → discard

// Rows currently mounted in the DOM, keyed by line index.
const mountedRows = new Map(); // line -> {gutter, row}
const caretPool = [];
const selPool = [];

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

// ---- rendering -----------------------------------------------------------

function render(force = false) {
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
  renderCarets(model);
  syncStatus(model);
  positionIme(model);
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
    entry.row.textContent = model.rows[i];
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
      el.style.left = colToPx(c.col) + "px";
      el.style.top = c.row * LINE_HEIGHT + "px";
      el.className =
        "caret" +
        (model.mode === "insert" ? " blink" : " block") +
        (c.primary ? " primary" : "");
    } else {
      el.style.display = "none";
    }
  }
}

function renderSelections(model) {
  // Expand each selection range into per-row rectangles.
  const rects = [];
  for (const s of model.selections) {
    for (let row = s.start_row; row <= s.end_row; row++) {
      const from = row === s.start_row ? s.start_col : 0;
      // For intermediate/last rows we don't know the line's display width
      // cheaply; approximate the end of multi-row selections with the row text.
      let to;
      if (row === s.end_row) {
        to = s.end_col;
      } else {
        const entry = mountedRows.get(row);
        to = entry ? entry.row.textContent.length + 1 : from + 1;
      }
      if (to <= from) to = from + 1;
      rects.push({ row, from, to });
    }
  }
  ensurePool(selPool, rects.length, "sel");
  for (let i = 0; i < selPool.length; i++) {
    const el = selPool[i];
    if (i < rects.length) {
      const r = rects[i];
      el.style.display = "block";
      el.style.left = colToPx(r.from) + "px";
      el.style.top = r.row * LINE_HEIGHT + "px";
      el.style.width = (r.to - r.from) * charWidth + "px";
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

function syncStatus(model) {
  els.mode.textContent = model.mode;
  els.dirty.textContent = editor.is_dirty() ? "● modified" : "";
}

function positionIme(model) {
  const c = model.cursors[0] || { row: 0, col: 0 };
  const x = colToPx(c.col);
  const y = c.row * LINE_HEIGHT;
  els.ime.style.left = x + "px";
  els.ime.style.top = y + "px";
  els.preedit.style.left = x + "px";
  els.preedit.style.top = y + "px";
}

// ---- input ---------------------------------------------------------------

// Browser KeyboardEvent.key → name understood by WebEditor.key().
function keyName(e) {
  const k = e.key;
  // Single printable character (includes space). Use as-is.
  if (k.length === 1) return k;
  // Named keys we map directly.
  const named = new Set([
    "Enter", "Backspace", "Delete", "Escape", "Tab",
    "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
    "Home", "End", "PageUp", "PageDown", "Insert",
  ]);
  if (named.has(k)) return k;
  if (/^F([1-9]|1[0-2])$/.test(k)) return k;
  return null; // Shift/Control/Alt/Meta/CapsLock/... — ignore
}

function onKeyDown(e) {
  // While composing, let the IME consume everything.
  if (composing || e.isComposing || e.keyCode === 229) return;

  // Save shortcut.
  if ((e.ctrlKey || e.metaKey) && (e.key === "s" || e.key === "S")) {
    e.preventDefault();
    save();
    return;
  }

  const name = keyName(e);
  if (name === null) return; // pure modifier press

  e.preventDefault();
  editor.key(name, e.ctrlKey || e.metaKey, e.shiftKey, e.altKey);
  // Keep the IME textarea empty so command keystrokes never accumulate there.
  els.ime.value = "";
  render();
}

function onCompositionStart() {
  composing = true;
  // Only Insert mode accepts IME text; otherwise discard the result.
  imeIgnore = editor.mode() !== "insert";
  els.preedit.textContent = "";
}

function onCompositionUpdate(e) {
  els.preedit.textContent = imeIgnore ? "" : e.data || "";
}

function onCompositionEnd(e) {
  composing = false;
  els.preedit.textContent = "";
  const text = e.data || "";
  els.ime.value = "";
  if (!imeIgnore && text) {
    editor.insert_text(text);
    render();
  }
  imeIgnore = false;
}

// Fallback for IMEs/dead-keys that commit via `input` without composition.
function onInput(e) {
  if (composing) return;
  const text = els.ime.value;
  els.ime.value = "";
  if (text && editor.mode() === "insert") {
    editor.insert_text(text);
    render();
  }
}

function onPaste(e) {
  e.preventDefault();
  const text = (e.clipboardData || window.clipboardData).getData("text");
  if (text) {
    editor.insert_text(text);
    render();
  }
}

// ---- save ----------------------------------------------------------------

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
        // Re-save adopting the current on-disk hash as the new base.
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

// ---- boot ----------------------------------------------------------------

async function boot() {
  await init();
  measureCharWidth();

  filePath = decodeURIComponent(
    window.location.pathname.replace(/^\/edit\/?/, "")
  );
  els.path.textContent = filePath || "(no file)";

  let content = "";
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

  editor = new WebEditor(filePath, content);

  els.ime.addEventListener("keydown", onKeyDown);
  els.ime.addEventListener("compositionstart", onCompositionStart);
  els.ime.addEventListener("compositionupdate", onCompositionUpdate);
  els.ime.addEventListener("compositionend", onCompositionEnd);
  els.ime.addEventListener("input", onInput);
  els.ime.addEventListener("paste", onPaste);
  els.scroller.addEventListener("scroll", () => render());
  els.scroller.addEventListener("mousedown", () => setTimeout(() => els.ime.focus(), 0));
  window.addEventListener("resize", () => render());

  els.ime.focus();
  render(true);
}

boot();

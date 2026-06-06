const app = document.getElementById("app");
const header = document.getElementById("header");
const focusPath = document.getElementById("focus-path");
const popupBackdrop = document.getElementById("popup-backdrop");
const popupTitle = document.getElementById("popup-title");
const popupInput = document.getElementById("popup-input");
const popupList = document.getElementById("popup-list");
const popup = document.getElementById("popup");
const popupPreview = document.getElementById("popup-preview");
const popupHint = document.getElementById("popup-hint");
const toast = document.getElementById("toast");
const helpBackdrop = document.getElementById("help-backdrop");
const helpBody = document.getElementById("help-body");
const repoLink = document.getElementById("repo-link");
const repoSep = document.getElementById("repo-sep");

const COMPONENTS = ["explorer", "history", "compare", "status"];
const state = {
  component: "explorer",
  focusLevel: "app",
  pane: 0,
  gPending: false,
  files: [],
  fileEntries: [],
  currentFile: "",
  fileContent: "",
  fileBaseContent: "",
  fileHash: "",
  editorMode: "readonly",
  previewMode: false,
  gitGutter: {},
  multiRanges: [],
  multiWord: "",
  multiGoalCol: 0,
  editorHistory: null,
  highlightLines: {},
  commits: [],
  historyCommit: 0,
  historyFile: 0,
  historyData: null,
  historySignature: "",
  historyPollTimer: null,
  refs: [],
  compareBase: "",
  compareTarget: "",
  refPickerWhich: "base",
  compareFiles: [],
  compareFile: 0,
  statusFiles: [],
  statusFile: 0,
  statusSignature: "",
  statusPollTimer: null,
  popup: null,
  popupItems: [],
  popupFiltered: [],
  popupIndex: 0,
  treeRoot: null,
  treeExpanded: new Set(),
  treePreviewToken: 0,
  help: false,
  searchToken: 0,
  lastSearchQuery: null,
  searchHits: [],
  searchQuery: "",
  searchCollapsed: new Set(),
  repoInfo: null,
  quickFiles: [],
  quickCommands: [],
  quickSymbols: [],
  quickSymbolsLoaded: false,
  quickMode: "files",
  menuActions: [],
};

const HELP_SECTIONS = [
  {
    title: "Global", keys: [
      ["g e / g h / g c / g s", "Explorer / History / Compare / Status"],
      ["⌘P / ⌘⇧P", "File picker / Command picker"],
      ["⌘@", "Symbol picker"],
      ["⌘⇧F", "Global search"],
      ["⌘S", "Save current file"],
      ["r", "Refresh component"],
      ["?", "Toggle this help"],
    ],
  },
  {
    title: "Explorer / Editor", keys: [
      ["t", "Open file tree"],
      ["i / Enter", "Edit (insert) mode"],
      ["Esc", "Back to app focus"],
      ["⌘D", "Add cursor: word / next match (multi-cursor)"],
      ["⌥⌘↓ / ⌥⌘↑", "Add a cursor below / above"],
      ["⌥⌘⇧↓ / ⌥⌘⇧↑", "Add cursors to bottom / top"],
      ["⌘⌫ / ⌥⌫", "Multi-cursor: delete to line start / word"],
      ["⌘Z / ⌘⇧Z", "Undo / redo"],
      ["j / k", "Scroll"],
      ["g g", "Jump to head of file"],
      ["G", "Jump to tail of file"],
      ["p", "Toggle Markdown/HTML preview"],
    ],
  },
  {
    title: "Diff views", keys: [
      ["j / k", "Move selection · scroll preview when focused"],
      ["l / Tab", "Focus next pane"],
      ["h / Esc", "Focus previous pane / component"],
      ["Ctrl-d / Ctrl-u", "Scroll preview"],
      ["o", "Open selected file in editor"],
      ["O", "Open menu: GitHub · copy path · copy content"],
      ["v", "Toggle viewed"],
    ],
  },
  {
    title: "History / Compare", keys: [
      ["J / K", "History: prev/next changed file · Compare: scroll preview"],
      ["B / C", "Compare: pick base / compare ref (fuzzy)"],
    ],
  },
  {
    title: "File tree", keys: [
      ["j / k", "Move"],
      ["h / l", "Collapse / expand"],
      ["Enter", "Open"],
      ["⌥Enter / ⌘Enter", "Open in new tab"],
      ["/", "Filter"],
      ["J / K", "Scroll preview"],
    ],
  },
];

const COMMANDS = [
  { label: "Switch to Explorer", hint: "g e", run: () => switchComponent("explorer") },
  { label: "Switch to History", hint: "g h", run: () => switchComponent("history") },
  { label: "Switch to Compare", hint: "g c", run: () => switchComponent("compare") },
  { label: "Switch to Status", hint: "g s", run: () => switchComponent("status") },
  { label: "Open file tree", hint: "t", run: () => openTreePicker() },
  { label: "Save current file", hint: "Cmd+S", run: () => saveCurrentFile() },
  { label: "Refresh current component", hint: "r", run: () => refreshComponent() },
  { label: "Search project", hint: "Cmd+Shift+F", run: () => openSearchPopup() },
  { label: "Show keybindings", hint: "?", run: () => toggleHelp() },
];

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, c => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

async function api(url, options) {
  const response = await fetch(url, { cache: "no-store", ...options });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `${response.status} ${response.statusText}`);
  return data;
}

function notify(message) {
  toast.textContent = message;
  toast.hidden = false;
  clearTimeout(notify.timer);
  notify.timer = setTimeout(() => { toast.hidden = true; }, 2800);
}

async function loadRepoInfo() {
  try {
    state.repoInfo = await api("/api/repo-info");
  } catch (_) {
    state.repoInfo = null;
  }
  renderRepoLink();
  updateTitle();
}

function renderRepoLink() {
  const info = state.repoInfo;
  if (!info || (!info.owner && !info.repo)) {
    repoLink.hidden = true;
    repoSep.hidden = true;
    return;
  }
  repoLink.textContent = `${info.owner}/${info.repo}`;
  if (info.remote_url) repoLink.href = info.remote_url;
  else repoLink.removeAttribute("href");
  repoLink.title = info.remote_url || info.root || "";
  repoLink.hidden = false;
  repoSep.hidden = false;
}

function updateTitle() {
  const repo = state.repoInfo?.repo || "gargo";
  const detail = state.component === "explorer" && state.currentFile
    ? state.currentFile
    : state.component.charAt(0).toUpperCase() + state.component.slice(1);
  document.title = `${repo}/${detail}`;
}

function activePane() {
  return app.querySelector(`.pane[data-pane="${state.pane}"]`);
}

function setFocus(level, pane = state.pane) {
  state.focusLevel = level;
  state.pane = Math.max(0, pane);
  document.querySelectorAll(".pane.focused").forEach(el => el.classList.remove("focused"));
  document.querySelectorAll("#header button.focused").forEach(el => el.classList.remove("focused"));
  if (level === "editor") {
    const input = app.querySelector(".editor-input");
    if (input) {
      input.readOnly = false;
      state.editorMode = "insert";
      input.focus({ preventScroll: true });
    }
  } else if (level === "pane") {
    const paneEl = activePane();
    paneEl?.classList.add("focused");
    paneEl?.focus({ preventScroll: true });
  } else if (level === "component") {
    header.querySelector(`[data-component="${state.component}"]`)?.classList.add("focused");
    app.focus({ preventScroll: true });
  } else {
    app.focus({ preventScroll: true });
  }
  updateEditorModeIndicator();
  updateFocusChrome();
}

function updateFocusChrome() {
  header.querySelectorAll("button").forEach(button => {
    button.classList.toggle("active", button.dataset.component === state.component);
  });
  const paneName = activePane()?.dataset.name || "";
  focusPath.textContent = [state.focusLevel, state.component, state.focusLevel === "pane" ? paneName : ""]
    .filter(Boolean).join(" › ");
  updateTitle();
}

async function switchComponent(component) {
  if (!COMPONENTS.includes(component)) return;
  stopStatusPolling();
  stopHistoryPolling();
  state.component = component;
  state.pane = 0;
  state.focusLevel = component === "explorer" ? "app" : "pane";
  state.editorMode = "readonly";
  location.hash = component;
  if (component === "explorer") await renderExplorer();
  if (component === "history") await renderHistory();
  if (component === "compare") await renderCompare();
  if (component === "status") await renderStatus();
  setFocus(component === "explorer" ? "app" : "pane", 0);
  if (component === "status") startStatusPolling();
  if (component === "history") startHistoryPolling();
}

function componentBar(title, hint) {
  return `<div class="component-bar"><strong>${title}</strong><span class="grow"></span>${hint || ""}</div>`;
}

function pane(title, name, index, body, extra = "") {
  return `<section class="pane" tabindex="-1" data-pane="${index}" data-name="${name}">
    <div class="pane-title">${title}<span class="grow"></span>${extra}</div>${body}</section>`;
}

function listHtml(items, selected, row) {
  if (!items.length) return `<div class="empty">No items</div>`;
  return `<ol class="list">${items.map((item, index) =>
    `<li data-index="${index}" class="${index === selected ? "selected" : ""}">${row(item, index)}</li>`
  ).join("")}</ol>`;
}

async function ensureFiles() {
  if (state.files.length) return;
  const data = await api("/api/files");
  state.files = data.files || [];
  state.fileEntries = data.entries || state.files.map(path => ({ path, mtime: 0, changed: false }));
}

async function renderExplorer() {
  app.innerHTML = `<section class="component">
    ${componentBar("Explorer", `<span><span class="key">t</span> tree · <span class="key">⌘P</span> files · <span class="key">⌘⇧P</span> commands · <span class="key">⌘@</span> symbols · <span class="key">⌘⇧F</span> search · <span class="key">p</span> preview · <span class="key">?</span> help</span>`)}
    <div id="explorer-surface" class="pane focused" tabindex="-1" data-pane="0" data-name="editor"></div>
  </section>`;
  if (!state.currentFile) {
    app.querySelector("#explorer-surface").innerHTML =
      `<div class="empty">No file open. Press <span class="key">t</span> for the tree or <span class="key">Cmd+P</span> for files.</div>`;
  } else if (state.previewMode && previewableKind(state.currentFile)) {
    await renderPreviewSurface(app.querySelector("#explorer-surface"));
  } else {
    await renderCodeSurface(app.querySelector("#explorer-surface"), {
      path: state.currentFile,
      content: state.fileContent,
      editable: true,
    });
  }
}

// Markdown/HTML preview: `p` toggles a rendered view of the current
// file. The server renders markdown (GFM) and passes HTML through; mermaid code
// blocks come back as `<pre class="mermaid">` which the injected bootstrap runs.
function previewableKind(path) {
  const ext = (path || "").split(".").pop().toLowerCase();
  if (ext === "md" || ext === "markdown") return "markdown";
  if (ext === "html" || ext === "htm") return "html";
  return null;
}

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

// The escaped `<\/script>` keeps the parent page's <script> from closing early
// when editor.js is inlined into editor.html.
const PREVIEW_MERMAID_BOOT =
  '<script src="/assets/mermaid.min.js"><\/script>'
  + "<script>(function(){if(!window.mermaid)return;"
  + "window.mermaid.initialize({startOnLoad:false,theme:'default'});"
  + "window.mermaid.run({querySelector:'pre.mermaid'}).catch(function(){});})();<\/script>";

function previewDocument(data) {
  if (data.kind === "html") return data.html || "";
  if (data.kind === "markdown") {
    return `<!DOCTYPE html><html><head><meta charset="utf-8"><style>${PREVIEW_CSS}</style></head>`
      + `<body><div class="markdown-body">${data.html || ""}</div>${PREVIEW_MERMAID_BOOT}</body></html>`;
  }
  return "";
}

async function renderPreviewSurface(container) {
  const path = state.currentFile;
  container.innerHTML = `<div class="code-surface preview-surface">
    <div class="code-toolbar"><span class="path">${escapeHtml(path)}</span>
      <span class="grow"></span><span class="editor-mode">preview</span><span>p for code</span></div>
    <div class="code-body"><iframe class="preview-frame" title="Preview"></iframe></div>
  </div>`;
  const frame = container.querySelector(".preview-frame");
  try {
    const data = await api("/api/preview", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ path, content: state.fileContent }),
    });
    frame.srcdoc = previewDocument(data);
  } catch (error) {
    frame.srcdoc = `<pre style="color:#b00020;padding:16px">${escapeHtml(error.message)}</pre>`;
  }
}

async function togglePreview() {
  if (state.component !== "explorer") return;
  if (!state.currentFile || !previewableKind(state.currentFile)) {
    notify("Preview is only available for Markdown and HTML files");
    return;
  }
  state.previewMode = !state.previewMode;
  await renderExplorer();
  setFocus("app", 0);
}

async function openFile(path, line = null, col = 0) {
  const data = await api(`/api/file?path=${encodeURIComponent(path)}`);
  state.currentFile = data.path;
  state.fileContent = data.content;
  state.fileBaseContent = data.content;
  state.fileHash = data.hash;
  state.editorMode = "readonly";
  state.gitGutter = {};
  state.multiRanges = [];
  state.multiWord = "";
  stopStatusPolling();
  stopHistoryPolling();
  await api("/api/last-file", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ path }),
  }).catch(() => {});
  state.component = "explorer";
  location.hash = "explorer";
  await renderExplorer();
  const input = app.querySelector(".editor-input");
  if (input && line !== null) {
    const lines = input.value.split("\n");
    const offset = lines.slice(0, line).reduce((n, value) => n + value.length + 1, 0) + col;
    input.setSelectionRange(offset, offset);
    setFocus("editor", 0);
    scrollEditorToCursor("auto");
  } else {
    setFocus("app", 0);
  }
}

async function saveCurrentFile() {
  if (!state.currentFile) return;
  try {
    const data = await api("/api/save", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({
        path: state.currentFile,
        base_hash: state.fileHash,
        content: state.fileContent,
      }),
    });
    state.fileHash = data.hash;
    state.fileBaseContent = state.fileContent;
    updateDirtyIndicator();
    notify(`Saved ${state.currentFile}`);
  } catch (error) {
    notify(`Save failed: ${error.message}`);
  }
}

async function renderCodeSurface(container, options) {
  container.innerHTML = `<div class="code-surface">
    <div class="code-toolbar"><span class="path">${escapeHtml(options.path || "Preview")}</span>
      <span class="grow"></span><span class="dirty"></span>
      ${options.editable ? `<span class="editor-mode"></span><span>i/Enter edit · Esc app focus · Cmd+S save</span>` : `<span>read only</span>`}
    </div><div class="code-body"></div></div>`;
  const body = container.querySelector(".code-body");
  if (options.diffHtml !== undefined) {
    body.innerHTML = `<div class="diff-preview">${options.diffHtml || `<div class="empty">No diff</div>`}</div>`;
    return;
  }
  if (!options.editable) {
    body.innerHTML = `<pre class="highlight-layer">${numberedPlainText(options.content || "")}</pre>`;
    return;
  }
  body.innerHTML = `<div class="editor-wrap"><pre class="highlight-layer"></pre>
    <textarea class="editor-input" spellcheck="false" aria-label="Editor"></textarea>
    <div class="multi-overlay"></div></div>`;
  const input = body.querySelector(".editor-input");
  const layer = body.querySelector(".highlight-layer");
  state.multiRanges = [];
  state.multiWord = "";
  input.value = options.content || "";
  input.tabIndex = -1;
  input.readOnly = state.editorMode !== "insert";
  await updateHighlightLayer(layer, options.path, input.value);
  input.style.height = `${Math.max(input.scrollHeight, body.clientHeight)}px`;
  input.dataset.lines = String(input.value.split("\n").length);
  fetchGitGutter(options.path, input.value);
  editorHistoryInit(input.value);
  input.addEventListener("keydown", onEditorKeyDown);
  input.addEventListener("input", async () => {
    if (state.multiRanges.length) clearMultiCursors(); // a native edit exits multi-cursor
    state.fileContent = input.value;
    editorHistoryPush(true);
    // The visible glyphs are the highlight layer (the textarea text is
    // transparent), so paint plain text synchronously for zero-lag feedback;
    // the server syntax pass refines it a beat later.
    layer.innerHTML = numberedPlainText(input.value);
    applyGitGutter(layer);
    // Re-measure height only when the line count changes — `scrollHeight`
    // forces a reflow, so skipping it on intra-line edits keeps typing snappy.
    const lines = input.value.split("\n").length;
    if (String(lines) !== input.dataset.lines) {
      // Resetting the height to `auto` collapses the surface and clamps its
      // scrollTop, which yanks the viewport on every line added/removed. Pin the
      // scroll position across the reflow so the view only moves when the caret
      // actually leaves the viewport (handled by scrollEditorToCursor below).
      const surface = editorScroller();
      const keepTop = surface ? surface.scrollTop : 0;
      input.dataset.lines = String(lines);
      input.style.height = "auto";
      input.style.height = `${Math.max(input.scrollHeight, body.clientHeight)}px`;
      if (surface) surface.scrollTop = keepTop;
      scrollEditorToCursor("auto");
    }
    updateDirtyIndicator();
    clearTimeout(input.highlightTimer);
    input.highlightTimer = setTimeout(() => updateHighlightLayer(layer, options.path, input.value), 90);
    scheduleGitGutter(options.path, input.value);
  });
  input.addEventListener("mousedown", () => {
    if (state.editorMode === "readonly") {
      // Click into a read-only editor → enter insert mode with the caret where
      // the click lands. Don't preventDefault: the native mousedown positions
      // the caret, and setFocus("editor") flips readOnly off and focuses.
      setFocus("editor", 0);
    } else {
      clearMultiCursors();
    }
  });
  input.addEventListener("blur", updateEditorModeIndicator);
  updateDirtyIndicator();
  updateEditorModeIndicator();
}

function updateDirtyIndicator() {
  const dirty = app.querySelector(".code-toolbar .dirty");
  if (dirty) dirty.textContent = state.fileContent !== state.fileBaseContent ? "modified" : "";
}

function updateEditorModeIndicator() {
  const indicator = app.querySelector(".code-toolbar .editor-mode");
  if (indicator) indicator.textContent = state.editorMode;
}

function enterEditorInsertMode() {
  if (state.component !== "explorer" || state.focusLevel !== "app") return false;
  const input = app.querySelector(".editor-input");
  if (!input) return false;
  input.readOnly = false;
  // Drop the caret on the first visible line so entering edit mode keeps the
  // viewport where the user was reading instead of jumping to the old caret.
  // Line height is 20px with 12px top padding (see editor.css).
  const surface = editorScroller();
  if (surface) {
    const lines = input.value.split("\n");
    const topLine = Math.max(0, Math.min(Math.round((surface.scrollTop - 12) / 20), lines.length - 1));
    const offset = lines.slice(0, topLine).reduce((sum, line) => sum + line.length + 1, 0);
    input.setSelectionRange(offset, offset);
  }
  updateEditorModeIndicator();
  setFocus("editor", 0);
  // No scroll-to-cursor here: the caret was just placed on a visible line, so
  // scrolling would re-center it and undo that.
  return true;
}

function leaveEditorInsertMode() {
  clearMultiCursors();
  const input = app.querySelector(".editor-input");
  if (input) input.readOnly = true;
  state.editorMode = "readonly";
  setFocus("app", 0);
}

// Accumulating smooth scroll: repeated key presses extend a single target and
// ease toward it on rAF, instead of each press starting a fresh `behavior:
// "smooth"` animation from a mid-flight position (the source of the "rattle").
function smoothScrollBy(el, delta) {
  if (!el) return;
  if (!el._scrollRAF) el._scrollTarget = el.scrollTop;
  const max = el.scrollHeight - el.clientHeight;
  el._scrollTarget = Math.max(0, Math.min((el._scrollTarget ?? el.scrollTop) + delta, max));
  if (el._scrollRAF) return;
  const step = () => {
    const diff = el._scrollTarget - el.scrollTop;
    if (Math.abs(diff) < 1) { el.scrollTop = el._scrollTarget; el._scrollRAF = null; return; }
    el.scrollTop += diff * 0.28;
    el._scrollRAF = requestAnimationFrame(step);
  };
  el._scrollRAF = requestAnimationFrame(step);
}

// The scrollable element behind the explorer keys: the preview iframe's
// document when previewing, otherwise the code surface.
function explorerScrollTarget() {
  if (state.previewMode) {
    const doc = app.querySelector(".preview-frame")?.contentDocument;
    const el = doc?.scrollingElement || doc?.documentElement;
    if (el) return el;
  }
  return editorScroller();
}

function scrollExplorer(direction) {
  smoothScrollBy(explorerScrollTarget(), direction * 80);
}

function editorScroller() {
  return app.querySelector("#explorer-surface .code-surface")
    || app.querySelector("#explorer-surface");
}

// Bring the caret line into view inside the scrollable code surface. The
// textarea is fully expanded (overflow hidden) so it never scrolls itself;
// the surrounding `.code-surface` does. Line height is 20px and the editor
// has 12px top padding (see editor.css), so caret pixel = 12 + line * 20.
function scrollEditorToCursor(behavior = "smooth") {
  const input = app.querySelector(".editor-input");
  const surface = editorScroller();
  if (!input || !surface) return;
  const line = input.value.slice(0, input.selectionStart).split("\n").length - 1;
  const caretY = 12 + line * 20;
  const top = surface.scrollTop;
  const view = surface.clientHeight - 31; // subtract sticky toolbar height
  if (caretY < top + 40 || caretY > top + view - 40) {
    surface.scrollTo({ top: Math.max(0, caretY - surface.clientHeight / 2), behavior });
  }
}

// ---- Multi-cursor ----------------------------------------------------------
//
// A <textarea> has a single native caret, so multi-cursor is emulated: a set of
// ranges in `state.multiRanges`, an overlay drawing the extra carets/selection
// bands, and a keydown interceptor that applies typing/Backspace/Enter to every
// range at once. Cmd+D seeds the word under the caret, then adds the next match
// on each press (the textarea-native single-selection case stays native).
const WORD_CHAR = /[A-Za-z0-9_]/;
const EDITOR_PAD_LEFT = 58;
const EDITOR_PAD_TOP = 12;
const EDITOR_LINE_H = 20;
const EDITOR_TAB = 4;
let editorCharW = 0;

function editorCharWidth() {
  if (editorCharW) return editorCharW;
  const probe = document.createElement("span");
  probe.style.cssText = "position:absolute;visibility:hidden;white-space:pre;font:13px/20px var(--font)";
  probe.textContent = "0".repeat(80);
  document.body.appendChild(probe);
  editorCharW = probe.getBoundingClientRect().width / 80 || 7.8;
  probe.remove();
  return editorCharW;
}

// Visual line + column for a string offset, expanding tabs so overlay carets
// line up with the rendered (tab-size 4) text.
function offsetToVisual(value, offset) {
  const before = value.slice(0, offset);
  const nl = before.lastIndexOf("\n");
  const line = (before.match(/\n/g) || []).length;
  let vcol = 0;
  for (let i = nl + 1; i < offset; i++) {
    vcol += value[i] === "\t" ? EDITOR_TAB - (vcol % EDITOR_TAB) : 1;
  }
  return { line, vcol };
}

function renderMultiCursors() {
  const overlay = app.querySelector(".multi-overlay");
  if (!overlay) return;
  const input = app.querySelector(".editor-input");
  if (!input || !state.multiRanges.length) { overlay.innerHTML = ""; return; }
  const value = input.value;
  const cw = editorCharWidth();
  overlay.innerHTML = state.multiRanges.map(range => {
    const a = offsetToVisual(value, range.start);
    const top = EDITOR_PAD_TOP + a.line * EDITOR_LINE_H;
    const left = EDITOR_PAD_LEFT + a.vcol * cw;
    if (range.start === range.end) {
      return `<div class="multi-caret" style="top:${top}px;left:${left}px"></div>`;
    }
    const b = offsetToVisual(value, range.end);
    const width = Math.max(2, (b.vcol - a.vcol) * cw);
    return `<div class="multi-band" style="top:${top}px;left:${left}px;width:${width}px"></div>`;
  }).join("");
}

function clearMultiCursors() {
  if (!state.multiRanges.length) return;
  state.multiRanges = [];
  state.multiWord = "";
  state.multiGoalCol = 0;
  renderMultiCursors();
}

// Character line/column for a string offset (vs. offsetToVisual's tab-expanded
// column used for drawing) — these map to/from real caret offsets.
function offsetLineCol(value, offset) {
  const nl = value.lastIndexOf("\n", offset - 1);
  return { line: (value.slice(0, offset).match(/\n/g) || []).length, col: offset - (nl + 1) };
}

function lineColToOffset(lines, line, col) {
  let offset = 0;
  for (let i = 0; i < line; i++) offset += lines[i].length + 1;
  return offset + Math.min(col, lines[line].length);
}

function lineStartOffset(value, offset) {
  return value.lastIndexOf("\n", offset - 1) + 1;
}

// Start of the word/whitespace run before `offset` — the span Alt+Backspace eats.
function prevWordStart(value, offset) {
  let i = offset;
  while (i > 0 && /\s/.test(value[i - 1]) && value[i - 1] !== "\n") i--;
  while (i > 0 && WORD_CHAR.test(value[i - 1])) i--;
  if (i === offset && i > 0) i--; // always remove at least one char (punctuation)
  return i;
}

// Seed multi-cursor from the native caret if it isn't active yet, remembering the
// caret's column as the "goal" so vertical adds keep their column across short lines.
function seedMultiFromCaret() {
  const input = app.querySelector(".editor-input");
  if (state.multiRanges.length) return;
  state.multiRanges = [{ start: input.selectionStart, end: input.selectionStart }];
  state.multiWord = "";
  state.multiGoalCol = offsetLineCol(input.value, input.selectionStart).col;
}

// Add a single caret one line above/below the current extreme caret (VSCode
// ⌥⌘↑ / ⌥⌘↓), kept at the goal column.
function addCursorVertical(dir) {
  const input = app.querySelector(".editor-input");
  if (!input || input.readOnly) return;
  seedMultiFromCaret();
  const value = input.value;
  const lines = value.split("\n");
  const ranges = state.multiRanges;
  const ref = dir > 0 ? ranges[ranges.length - 1] : ranges[0];
  const target = offsetLineCol(value, ref.end).line + dir;
  if (target < 0 || target >= lines.length) { notify("No more lines"); return; }
  const offset = lineColToOffset(lines, target, state.multiGoalCol);
  if (ranges.some(r => r.start === r.end && r.start === offset)) return;
  ranges.push({ start: offset, end: offset });
  ranges.sort((a, b) => a.start - b.start);
  input.setSelectionRange(offset, offset);
  scrollEditorToCursor("auto");
  renderMultiCursors();
}

// Drop a caret on every line from the current one to the top/bottom of the file
// (⌥⌘⇧↑ / ⌥⌘⇧↓), all at the goal column.
function addCursorsToEdge(dir) {
  const input = app.querySelector(".editor-input");
  if (!input || input.readOnly) return;
  seedMultiFromCaret();
  const value = input.value;
  const lines = value.split("\n");
  const ranges = state.multiRanges;
  const refLine = offsetLineCol(value, (dir > 0 ? ranges[ranges.length - 1] : ranges[0]).end).line;
  const edge = dir > 0 ? lines.length - 1 : 0;
  for (let line = refLine + dir; dir > 0 ? line <= edge : line >= edge; line += dir) {
    const offset = lineColToOffset(lines, line, state.multiGoalCol);
    if (!ranges.some(r => r.start === r.end && r.start === offset)) ranges.push({ start: offset, end: offset });
  }
  ranges.sort((a, b) => a.start - b.start);
  input.setSelectionRange(lineColToOffset(lines, edge, state.multiGoalCol), lineColToOffset(lines, edge, state.multiGoalCol));
  scrollEditorToCursor("auto");
  renderMultiCursors();
}

// Cmd+D: seed from the current selection/word, or add the next occurrence of the
// seeded word as another cursor.
function multiCursorAddNext() {
  const input = app.querySelector(".editor-input");
  if (!input || input.readOnly) return;
  const value = input.value;
  if (!state.multiRanges.length) {
    let { selectionStart: s, selectionEnd: e } = input;
    if (s === e) {
      while (s > 0 && WORD_CHAR.test(value[s - 1])) s--;
      while (e < value.length && WORD_CHAR.test(value[e])) e++;
      if (e <= s) return;
    }
    state.multiWord = value.slice(s, e);
    state.multiRanges = [{ start: s, end: e }];
    input.setSelectionRange(s, e);
    renderMultiCursors();
    return;
  }
  const word = state.multiWord;
  if (!word) return;
  const maxEnd = Math.max(...state.multiRanges.map(r => r.end));
  let idx = value.indexOf(word, maxEnd);
  if (idx < 0) idx = value.indexOf(word); // wrap
  if (idx < 0 || state.multiRanges.some(r => r.start === idx)) { notify("No more matches"); return; }
  state.multiRanges.push({ start: idx, end: idx + word.length });
  state.multiRanges.sort((a, b) => a.start - b.start);
  input.setSelectionRange(idx, idx + word.length);
  scrollEditorToCursor("auto");
  renderMultiCursors();
}

// Apply one edit per cursor. `op(selText, range)` returns the absolute span
// `{from,to}` to replace with `text`; ranges collapse to carets after the edit.
function applyMultiEdit(op) {
  const input = app.querySelector(".editor-input");
  if (!input) return;
  const value = input.value;
  const edits = state.multiRanges.map(range => op(value.slice(range.start, range.end), range))
    .sort((a, b) => a.from - b.from);
  let out = "", cursor = 0;
  const next = [];
  for (const edit of edits) {
    if (edit.from < cursor) continue; // skip overlaps defensively
    out += value.slice(cursor, edit.from) + edit.text;
    next.push({ start: out.length, end: out.length });
    cursor = edit.to;
  }
  out += value.slice(cursor);
  input.value = out;
  state.multiRanges = next;
  const last = next[next.length - 1];
  if (last) input.setSelectionRange(last.start, last.start);
  repaintEditorAfterEdit();
  editorHistoryPush(false); // structural multi-cursor edit → its own undo step
}

// ---- Undo / redo -----------------------------------------------------------
//
// Setting `textarea.value` directly (multi-cursor edits, programmatic inserts)
// wipes the browser's native undo stack, so the editor keeps its own. Every edit
// pushes a snapshot; rapid typing coalesces into one entry so a single Cmd+Z
// drops a burst rather than one character.
function editorHistoryInit(value) {
  const input = app.querySelector(".editor-input");
  const sel = input ? input.selectionStart : 0;
  state.editorHistory = { entries: [{ value, selStart: sel, selEnd: sel }], index: 0, lastEdit: 0 };
}

function editorHistoryPush(coalesce) {
  const history = state.editorHistory;
  const input = app.querySelector(".editor-input");
  if (!history || !input) return;
  const snap = { value: input.value, selStart: input.selectionStart, selEnd: input.selectionEnd };
  if (snap.value === history.entries[history.index]?.value) return; // no textual change
  history.entries.length = history.index + 1; // discard the redo branch
  const now = Date.now();
  if (coalesce && now - history.lastEdit < 500 && history.index > 0) {
    history.entries[history.index] = snap; // fold this keystroke into the current burst
  } else {
    history.entries.push(snap);
    history.index = history.entries.length - 1;
  }
  history.lastEdit = coalesce ? now : 0;
  if (history.entries.length > 500) { history.entries.shift(); history.index--; }
}

function editorApplyHistory(snap) {
  const input = app.querySelector(".editor-input");
  if (!input || !snap) return;
  clearMultiCursors();
  input.value = snap.value;
  input.setSelectionRange(snap.selStart, snap.selEnd);
  repaintEditorAfterEdit();
  scrollEditorToCursor("auto");
}

function editorUndo() {
  const history = state.editorHistory;
  if (!history || history.index <= 0) return;
  history.index--;
  history.lastEdit = 0;
  editorApplyHistory(history.entries[history.index]);
}

function editorRedo() {
  const history = state.editorHistory;
  if (!history || history.index >= history.entries.length - 1) return;
  history.index++;
  history.lastEdit = 0;
  editorApplyHistory(history.entries[history.index]);
}

// Mirror the input-listener side effects for programmatic (multi-cursor) edits,
// which don't fire the textarea's `input` event.
function repaintEditorAfterEdit() {
  const input = app.querySelector(".editor-input");
  const layer = app.querySelector("#explorer-surface .highlight-layer");
  if (!input || !layer) return;
  state.fileContent = input.value;
  layer.innerHTML = numberedPlainText(input.value);
  applyGitGutter(layer);
  const body = layer.closest(".code-body");
  input.dataset.lines = String(input.value.split("\n").length);
  input.style.height = "auto";
  input.style.height = `${Math.max(input.scrollHeight, body?.clientHeight || 0)}px`;
  updateDirtyIndicator();
  clearTimeout(input.highlightTimer);
  input.highlightTimer = setTimeout(() => updateHighlightLayer(layer, state.currentFile, input.value), 90);
  scheduleGitGutter(state.currentFile, input.value);
  renderMultiCursors();
}

// Intercept keys on the textarea. Add-cursor chords (⌥⌘ + arrows) work whether or
// not multi-cursor is active yet; everything else only matters once it is. Runs
// before the window handler (which it stops for Escape so the editor isn't exited).
function onEditorKeyDown(event) {
  // Multi-cursor add actions — seed from the native caret on first use.
  if ((event.metaKey || event.ctrlKey) && event.altKey
      && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
    event.preventDefault();
    event.stopPropagation();
    if (event.shiftKey) addCursorsToEdge(event.key === "ArrowDown" ? 1 : -1);
    else addCursorVertical(event.key === "ArrowDown" ? 1 : -1);
    return;
  }
  if (!state.multiRanges.length) return;
  if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    clearMultiCursors();
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "d") return; // global adds next match
  // Cmd/Alt+Backspace delete-to-line-start / delete-word at every cursor. Handle
  // before the generic modifier-clear below, which would otherwise drop multi-cursor.
  if (state.multiRanges.length >= 2 && event.key === "Backspace" && (event.metaKey || event.altKey)) {
    event.preventDefault();
    const value = app.querySelector(".editor-input").value;
    applyMultiEdit((sel, r) => {
      if (sel.length) return { from: r.start, to: r.end, text: "" };
      let from = event.metaKey ? lineStartOffset(value, r.start) : prevWordStart(value, r.start);
      if (event.metaKey && from === r.start && from > 0) from -= 1; // at line start → eat newline
      return { from, to: r.start, text: "" };
    });
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "z") return; // global undo/redo
  if (event.metaKey || event.ctrlKey || event.altKey
      || event.key.startsWith("Arrow") || ["Home", "End", "PageUp", "PageDown"].includes(event.key)) {
    clearMultiCursors();
    return;
  }
  if (state.multiRanges.length < 2) return; // single range → let native editing run
  if (event.key === "Backspace") {
    event.preventDefault();
    applyMultiEdit((sel, r) => sel.length
      ? { from: r.start, to: r.end, text: "" }
      : { from: Math.max(0, r.start - 1), to: r.start, text: "" });
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    applyMultiEdit((sel, r) => ({ from: r.start, to: r.end, text: "\n" }));
    return;
  }
  if (event.key.length === 1) {
    event.preventDefault();
    const ch = event.key;
    applyMultiEdit((sel, r) => ({ from: r.start, to: r.end, text: ch }));
  }
}

// Jump the explorer surface (and caret, if present) to the top or bottom of
// the file — `gg` goes to the head, `G` to the tail.
function gotoEditorEdge(edge) {
  const surface = explorerScrollTarget();
  if (!surface) return;
  const input = app.querySelector(".editor-input");
  if (input && !state.previewMode) {
    const offset = edge === "top" ? 0 : input.value.length;
    input.setSelectionRange(offset, offset);
  }
  surface.scrollTo({ top: edge === "top" ? 0 : surface.scrollHeight, behavior: "smooth" });
}

function numberedPlainText(content) {
  return content.split("\n").map((line, index) =>
    `<span class="line-number">${index + 1}</span>${escapeHtml(line)}`
  ).join("\n");
}

async function updateHighlightLayer(layer, path, content) {
  // Very large files: skip the server round-trip + per-token markup and render
  // plain numbered text, so editing stays responsive (master virtualizes; this
  // is the lightweight equivalent for the textarea surface).
  if (content.length > 200000) {
    layer.innerHTML = numberedPlainText(content);
    applyGitGutter(layer);
    return;
  }
  try {
    layer.innerHTML = await highlightedTextHtml(path, content);
  } catch (_) {
    layer.innerHTML = numberedPlainText(content);
  }
  applyGitGutter(layer);
}

// Git change gutter: per-line added/modified/deleted status from
// `/api/git-gutter`, painted as colored bars on the line-number column. Only the
// editable code view renders line numbers, so the gutter is scoped to it.
function scheduleGitGutter(path, content) {
  clearTimeout(scheduleGitGutter.timer);
  scheduleGitGutter.timer = setTimeout(() => fetchGitGutter(path, content), 280);
}

async function fetchGitGutter(path, content) {
  try {
    const data = await api("/api/git-gutter", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ path, content }),
    });
    const next = {};
    for (const [line, status] of Object.entries(data.lines || {})) next[Number(line)] = status;
    state.gitGutter = next;
  } catch (_) {
    state.gitGutter = {};
  }
  applyGitGutter(app.querySelector("#explorer-surface .highlight-layer"));
}

function applyGitGutter(layer) {
  if (!layer) return;
  layer.querySelectorAll(".line-number").forEach((el, index) => {
    el.classList.remove("gutter-added", "gutter-modified", "gutter-deleted");
    const status = state.gitGutter[index];
    if (status) el.classList.add(`gutter-${status}`);
  });
}

async function highlightedTextHtml(path, content) {
  const data = await api("/api/highlight", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, content }),
  });
  return content.split("\n").map((line, index) => {
    const spans = [...(data.lines?.[String(index)] || [])]
      .sort((a, b) => a.start - b.start || a.end - b.end);
    const chars = Array.from(line);
    let out = "";
    let cursor = 0;
    for (const span of spans) {
      const start = Math.max(cursor, span.start);
      const end = Math.max(start, span.end);
      if (end <= cursor) continue;
      out += escapeHtml(chars.slice(cursor, start).join(""));
      out += `<span class="gr-hl-${escapeHtml(span.scope)}">${escapeHtml(chars.slice(start, end).join(""))}</span>`;
      cursor = end;
    }
    out += escapeHtml(chars.slice(cursor).join(""));
    return `<span class="line-number">${index + 1}</span>${out}`;
  }).join("\n");
}

async function renderHistory() {
  if (!state.commits.length) {
    const data = await api("/api/commits");
    state.commits = data.commits || [];
    state.historySignature = historySignature(state.commits);
  }
  if (state.commits.length && !state.historyData) await loadHistoryCommit();
  const files = state.historyData?.files || [];
  await renderDiffView({
    kind: "history",
    title: "History",
    hint: `<span><span class="key">j/k</span> select · <span class="key">J/K</span> changed files · <span class="key">l/Tab</span> right · <span class="key">h/Esc</span> left</span>`,
    panes: [
      {
        title: "Commit log", name: "commit log",
        body: listHtml(state.commits, state.historyCommit, commit =>
        `<div class="stack"><div class="primary">${escapeHtml(String(commit.message || "").split("\n")[0])}</div>
        <span class="secondary">${escapeHtml(commit.hash)} · ${escapeHtml(commit.author)} · ${escapeHtml(commit.date)}</span></div>`),
      },
      { title: "Changed files", name: "changed files", body: fileList(files, state.historyFile) },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
  });
}

async function loadHistoryCommit() {
  const commit = state.commits[state.historyCommit];
  if (!commit) { state.historyData = null; return; }
  state.historyData = await api(`/api/commit/${encodeURIComponent(commit.full_hash)}`);
  state.historyFile = Math.min(state.historyFile, Math.max(0, (state.historyData.files || []).length - 1));
}

function fileList(files, selected, options = {}) {
  return listHtml(files, selected, file =>
    `${options.viewed ? `<span class="viewed-box ${file.viewed ? "checked" : ""}" title="${file.viewed ? "Viewed" : "Not viewed"}">${file.viewed ? "[x]" : "[ ]"}</span>` : ""}
     <span class="status-badge status-${statusName(file.status)}">${statusLetter(file.status)}</span>
     <span class="primary">${escapeHtml(file.path)}</span>
     <span class="secondary">${stats(file)}</span>`);
}

function statusName(status) {
  const value = String(status || "").toLowerCase();
  return ({ a: "added", d: "deleted", r: "renamed", m: "modified", "?": "untracked" })[value] || value || "modified";
}
function statusLetter(status) {
  const name = statusName(status);
  return ({ added: "A", deleted: "D", renamed: "R", untracked: "?", modified: "M" })[name] || "M";
}
function stats(file) {
  const add = Number(file.additions || 0), del = Number(file.deletions || 0);
  return add || del ? `+${add} -${del}` : "";
}

function previewScroller() {
  // The diff preview nests a `.code-surface` inside `#diff-surface` (itself a
  // `.code-surface`); only the innermost one actually overflows and scrolls,
  // so prefer the innermost scrollable surface over the first match.
  const surfaces = [...app.querySelectorAll(".pane:last-child .code-surface")];
  return surfaces.reverse().find(el => el.scrollHeight > el.clientHeight)
    || surfaces[0]
    || app.querySelector(".pane:last-child");
}

function scrollPreview(direction, amount = 0.7) {
  const preview = previewScroller();
  if (preview) smoothScrollBy(preview, direction * preview.clientHeight * amount);
}

// True when the focused pane is the rightmost (preview) pane of a diff view,
// where there is no list to move through so j/k should scroll the preview.
function isPreviewPaneFocused() {
  if (state.focusLevel !== "pane") return false;
  if (!["compare", "status", "history"].includes(state.component)) return false;
  const count = app.querySelectorAll(".pane").length;
  return count > 0 && state.pane === count - 1;
}

async function ensureRefs() {
  if (state.refs.length) return;
  const data = await api("/api/branches");
  state.refs = data.branches || [];
  state.compareBase ||= data.default || data.current || state.refs[0] || "HEAD";
  state.compareTarget ||= data.current || state.refs[1] || "HEAD";
}

async function renderCompare() {
  await ensureRefs();
  if (!state.compareFiles.length && state.compareBase && state.compareTarget) {
    await loadCompare().catch(error => notify(error.message));
  }
  await renderDiffView({
    kind: "compare",
    title: "Compare",
    hint: `<span>Any branch, tag, or commit ref · <span class="key">B/C</span> base/compare · <span class="key">j/k</span> files · <span class="key">J/K</span> preview · <span class="key">v</span> viewed · <span class="key">o</span> edit · <span class="key">O</span> menu</span>`,
    panes: [
      {
        title: "Source · ref pair", name: "ref pair and changed files",
        body: `<div class="ref-form" id="ref-form">
          <label class="ref-label">Base</label>
          <button type="button" class="ref-button" id="ref-base" aria-label="Base ref">${escapeHtml(state.compareBase) || "—"}</button>
          <label class="ref-label">Compare</label>
          <button type="button" class="ref-button" id="ref-target" aria-label="Compare ref">${escapeHtml(state.compareTarget) || "—"}</button>
        </div>
        ${fileList(state.compareFiles, state.compareFile, { viewed: true })}`,
      },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
    bind: () => {
      document.getElementById("ref-base").addEventListener("click", () => openRefPicker("base"));
      document.getElementById("ref-target").addEventListener("click", () => openRefPicker("target"));
    },
  });
}

// Fuzzy picker for the Compare base/compare refs (replaces the raw text inputs).
// Lists known branches/tags/refs; a non-matching query offers a "use verbatim"
// row so arbitrary commit refs still work.
async function openRefPicker(which) {
  await ensureRefs();
  state.refPickerWhich = which;
  const current = which === "base" ? state.compareBase : state.compareTarget;
  const items = state.refs.map(ref => ({
    label: ref, search: ref,
    hint: ref === current ? "current" : "",
    run: () => applyRef(which, ref),
  }));
  showPopup("ref", which === "base" ? "Select base ref" : "Select compare ref",
    items, "Filter branches, tags, refs…");
}

async function applyRef(which, ref) {
  ref = String(ref || "").trim();
  if (!ref) return;
  if (which === "base") state.compareBase = ref;
  else state.compareTarget = ref;
  state.compareFiles = [];
  state.compareFile = 0;
  await loadCompare().catch(error => notify(error.message));
  await renderCompare();
  setFocus("pane", 0);
}

async function renderDiffView(source) {
  app.innerHTML = `<section class="component" data-diff-source="${source.kind}">
    ${componentBar(source.title, source.hint)}
    <div class="panes ${source.kind}">
      ${source.panes.map((item, index) => pane(item.title, item.name, index, item.body)).join("")}
    </div></section>`;
  source.bind?.();
  bindListClicks();
  await loadCurrentDiffPreview();
}

function diffSurfaceHtml() {
  return `<div id="diff-surface" class="code-surface"><div class="loading">Loading preview…</div></div>`;
}

async function loadCompare() {
  const query = new URLSearchParams({ base: state.compareBase, compare: state.compareTarget });
  const data = await api(`/api/compare?${query}`);
  state.compareFiles = data.files || [];
  state.compareFile = Math.min(state.compareFile, Math.max(0, state.compareFiles.length - 1));
}

function statusFilesFrom(data) {
  return ["unstaged", "staged", "untracked"].flatMap(section =>
    (data[section] || []).map(file => ({ ...file, section }))
  );
}

function statusSignature(files) {
  return files.map(file =>
    `${file.section}:${file.path}:${file.status}:${file.additions || 0}:${file.deletions || 0}:${file.viewed ? 1 : 0}`
  ).join("|");
}

async function renderStatus() {
  const data = await api("/api/status");
  state.statusFiles = statusFilesFrom(data);
  state.statusFile = Math.min(state.statusFile, Math.max(0, state.statusFiles.length - 1));
  state.statusSignature = statusSignature(state.statusFiles);
  await renderStatusView();
}

async function renderStatusView() {
  await renderDiffView({
    kind: "status",
    title: "Status",
    hint: `<span>Worktree vs HEAD · live · <span class="key">j/k</span> files · <span class="key">v</span> viewed · <span class="key">o</span> edit · <span class="key">O</span> menu · <span class="key">Ctrl-d/u</span> preview</span>`,
    panes: [
      { title: "Changed files", name: "changed files", body: fileList(state.statusFiles, state.statusFile, { viewed: true }) },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
  });
}

// Item 36: keep Status live. While it is the active component, poll the
// worktree and re-render only when the file set actually changes (signature
// diff) so navigation/focus isn't disturbed on every tick.
function startStatusPolling() {
  stopStatusPolling();
  state.statusPollTimer = setInterval(refreshStatusIfChanged, 1500);
}

function stopStatusPolling() {
  if (state.statusPollTimer) {
    clearInterval(state.statusPollTimer);
    state.statusPollTimer = null;
  }
}

async function refreshStatusIfChanged() {
  if (state.component !== "status" || state.popup || state.help || document.hidden) return;
  let data;
  try { data = await api("/api/status"); } catch (_) { return; }
  if (state.component !== "status" || state.popup || state.help) return;
  const files = statusFilesFrom(data);
  const signature = statusSignature(files);
  if (signature === state.statusSignature) return;
  const focusLevel = state.focusLevel, pane = state.pane;
  state.statusFiles = files;
  state.statusFile = Math.min(state.statusFile, Math.max(0, files.length - 1));
  state.statusSignature = signature;
  await renderStatusView();
  setFocus(focusLevel, pane);
}

// Item 37: keep History live — poll the commit log and re-render only when it
// changes (new commit, amend, rebase). Selection is preserved by commit hash so
// the user stays on the commit they were inspecting when it still exists.
function historySignature(commits) {
  return `${commits.length}:${commits[0]?.full_hash || ""}:${commits[commits.length - 1]?.full_hash || ""}`;
}

function startHistoryPolling() {
  stopHistoryPolling();
  state.historyPollTimer = setInterval(refreshHistoryIfChanged, 2500);
}

function stopHistoryPolling() {
  if (state.historyPollTimer) {
    clearInterval(state.historyPollTimer);
    state.historyPollTimer = null;
  }
}

async function refreshHistoryIfChanged() {
  if (state.component !== "history" || state.popup || state.help || document.hidden) return;
  let data;
  try { data = await api("/api/commits"); } catch (_) { return; }
  if (state.component !== "history" || state.popup || state.help) return;
  const commits = data.commits || [];
  const signature = historySignature(commits);
  if (signature === state.historySignature) return;
  const prevHash = state.commits[state.historyCommit]?.full_hash;
  state.commits = commits;
  state.historySignature = signature;
  const idx = prevHash ? commits.findIndex(commit => commit.full_hash === prevHash) : -1;
  state.historyCommit = idx >= 0 ? idx : 0;
  state.historyData = null;
  await loadHistoryCommit();
  const focusLevel = state.focusLevel, pane = state.pane;
  await renderHistory();
  setFocus(focusLevel, pane);
}

async function loadCurrentDiffPreview() {
  const surface = document.getElementById("diff-surface");
  if (!surface) return;
  let file, url;
  if (state.component === "history") {
    file = state.historyData?.files?.[state.historyFile];
    const commit = state.commits[state.historyCommit];
    if (file && commit) url = `/api/commit/${encodeURIComponent(commit.full_hash)}/file?path=${encodeURIComponent(file.path)}`;
  } else if (state.component === "compare") {
    file = state.compareFiles[state.compareFile];
    if (file) url = `/api/compare/file?${new URLSearchParams({
      base: state.compareBase, compare: state.compareTarget, path: file.path,
    })}`;
  } else if (state.component === "status") {
    file = state.statusFiles[state.statusFile];
    if (file) url = `/api/status/file?${new URLSearchParams({ section: file.section, path: file.path })}`;
  }
  if (!file || !url) {
    surface.innerHTML = `<div class="empty">No file selected</div>`;
    return;
  }
  try {
    const data = await api(url);
    await renderCodeSurface(surface, { path: file.path, diffHtml: data.html || "", editable: false });
  } catch (error) {
    surface.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
  }
}

function bindListClicks() {
  app.querySelectorAll(".pane .list li").forEach(li => li.addEventListener("click", async () => {
    const paneIndex = Number(li.closest(".pane").dataset.pane);
    state.pane = paneIndex;
    await moveSelectionTo(Number(li.dataset.index));
    setFocus("pane", paneIndex);
  }));
}

async function moveSelectionTo(index) {
  if (state.component === "history" && state.pane === 0) {
    state.historyCommit = Math.max(0, Math.min(index, state.commits.length - 1));
    state.historyFile = 0;
    await loadHistoryCommit();
    await renderHistory();
  } else if (state.component === "history" && state.pane === 1) {
    state.historyFile = Math.max(0, Math.min(index, (state.historyData?.files || []).length - 1));
    await renderHistory();
  } else if (state.component === "compare" && state.pane === 0) {
    state.compareFile = Math.max(0, Math.min(index, state.compareFiles.length - 1));
    await renderCompare();
  } else if (state.component === "status" && state.pane === 0) {
    state.statusFile = Math.max(0, Math.min(index, state.statusFiles.length - 1));
    await renderStatus();
  }
}

function currentSelection() {
  if (state.component === "history" && state.pane === 0) return [state.historyCommit, state.commits.length];
  if (state.component === "history" && state.pane === 1) return [state.historyFile, state.historyData?.files?.length || 0];
  if (state.component === "compare" && state.pane === 0) return [state.compareFile, state.compareFiles.length];
  if (state.component === "status" && state.pane === 0) return [state.statusFile, state.statusFiles.length];
  return [0, 0];
}

async function moveSelection(delta) {
  const [index, length] = currentSelection();
  if (!length) return;
  await moveSelectionTo((index + delta + length) % length);
  setFocus("pane", state.pane);
  app.querySelector(".list li.selected")?.scrollIntoView({ block: "nearest" });
}

async function moveHistoryFile(delta) {
  const files = state.historyData?.files || [];
  if (!files.length) return;
  const focusLevel = state.focusLevel;
  const pane = state.pane;
  state.historyFile = (state.historyFile + delta + files.length) % files.length;
  await renderHistory();
  setFocus(focusLevel, pane);
  app.querySelector('.pane[data-pane="1"] .list li.selected')
    ?.scrollIntoView({ block: "nearest" });
}

async function refreshComponent() {
  if (state.component === "history") { state.commits = []; state.historyData = null; }
  if (state.component === "compare") state.compareFiles = [];
  await switchComponent(state.component);
}

async function toggleStatusViewed() {
  if (state.component !== "status" || state.pane !== 0) return;
  const file = state.statusFiles[state.statusFile];
  if (!file) return;
  try {
    const data = await api("/api/status/viewed", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        section: file.section,
        path: file.path,
        viewed: !file.viewed,
      }),
    });
    file.viewed = Boolean(data.viewed);
    await renderStatus();
    setFocus("pane", 0);
  } catch (error) {
    notify(`Viewed toggle failed: ${error.message}`);
  }
}

async function toggleCompareViewed() {
  if (state.component !== "compare" || state.pane !== 0) return;
  const file = state.compareFiles[state.compareFile];
  if (!file) return;
  try {
    const data = await api("/api/compare/viewed", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        base: state.compareBase,
        compare: state.compareTarget,
        path: file.path,
        viewed: !file.viewed,
      }),
    });
    file.viewed = Boolean(data.viewed);
    await renderCompare();
    setFocus("pane", 0);
  } catch (error) {
    notify(`Viewed toggle failed: ${error.message}`);
  }
}

// Open a file in a fresh browser tab. boot() reads the `?path=` query param
// and loads that file, so the new tab lands directly on it.
function openFileInNewTab(path) {
  window.open(`/editor?path=${encodeURIComponent(path)}`, "_blank");
}

function openTreeSelectionInNewTab() {
  const node = state.popupFiltered[state.popupIndex]?.node;
  if (!node || node.type !== "file") return;
  openFileInNewTab(node.path);
}

async function openSelectedDiffFileInEditor() {
  let file = null;
  if (state.component === "status" && state.pane === 0) {
    file = state.statusFiles[state.statusFile];
  } else if (state.component === "compare" && state.pane === 0) {
    file = state.compareFiles[state.compareFile];
  }
  if (!file) return;
  try {
    await openFile(file.path);
  } catch (error) {
    notify(`Cannot open ${file.path}: ${error.message}`);
  }
}

// The file `O` (open menu) acts on: the selected file in status/compare lists,
// else the file open in the explorer.
function openMenuTarget() {
  if (state.component === "status" && state.pane === 0) return state.statusFiles[state.statusFile]?.path || "";
  if (state.component === "compare" && state.pane === 0) return state.compareFiles[state.compareFile]?.path || "";
  if (state.component === "explorer") return state.currentFile || "";
  return "";
}

function githubBlobUrl(remote, branch, path) {
  return `${remote}/blob/${encodeURIComponent(branch)}/${path.split("/").map(encodeURIComponent).join("/")}`;
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    notify("Copied");
  } catch (_) {
    const area = document.createElement("textarea");
    area.value = text;
    document.body.appendChild(area);
    area.select();
    let ok = false;
    try { ok = document.execCommand("copy"); } catch (_) {}
    area.remove();
    notify(ok ? "Copied" : "Copy failed");
  }
}

async function copyFileContent(path) {
  try {
    const content = path === state.currentFile
      ? state.fileContent
      : (await api(`/api/file?path=${encodeURIComponent(path)}`)).content;
    await copyText(content);
  } catch (error) {
    notify(`Copy failed: ${error.message}`);
  }
}

// `O` — file actions menu (open on GitHub, copy paths/content) for the file the
// editor would open with `o`.
function openOpenMenu() {
  const path = openMenuTarget();
  if (!path) { notify("No file to act on"); return; }
  const info = state.repoInfo || {};
  const actions = [];
  if (info.remote_url) {
    const def = info.default_branch || "main";
    actions.push({ key: "g", label: `Open on GitHub (${def})`, run: () => window.open(githubBlobUrl(info.remote_url, def, path), "_blank") });
    if (info.branch && info.branch !== def) {
      actions.push({ key: "G", label: `Open on GitHub (${info.branch})`, run: () => window.open(githubBlobUrl(info.remote_url, info.branch, path), "_blank") });
    }
  }
  actions.push({ key: "r", label: "Copy relative path", run: () => copyText(path) });
  actions.push({ key: "a", label: "Copy absolute path", run: () => copyText(info.root ? `${info.root.replace(/\/$/, "")}/${path}` : path) });
  actions.push({ key: "y", label: "Copy whole content", run: () => copyFileContent(path) });
  showMenuPopup(`Open · ${path}`, actions);
}

function showMenuPopup(title, actions) {
  state.popup = "menu";
  state.menuActions = actions;
  state.popupIndex = 0;
  popupTitle.textContent = title;
  popupInput.hidden = true;
  popup.classList.remove("tree-popup");
  popupPreview.hidden = true;
  popupPreview.innerHTML = "";
  popupHint.textContent = "↑↓/jk move · Enter select · shortcut keys shown · Esc close";
  popupBackdrop.hidden = false;
  renderMenu();
  popup.focus();
}

function renderMenu() {
  popupList.innerHTML = state.menuActions.map((action, index) =>
    `<li data-index="${index}" class="${index === state.popupIndex ? "selected" : ""}">${escapeHtml(action.label)}<span class="hint">${escapeHtml(action.key)}</span></li>`
  ).join("");
  popupList.querySelectorAll("[data-index]").forEach(li => li.addEventListener("click", () => chooseMenu(Number(li.dataset.index))));
  popupList.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
}

async function chooseMenu(index = state.popupIndex) {
  const action = state.menuActions[index];
  closePopup();
  if (action) await action.run();
}

function handleMenuKey(event) {
  if (event.key === "Escape") { event.preventDefault(); closePopup(); return; }
  if (event.key === "ArrowDown" || event.key === "j") {
    event.preventDefault();
    state.popupIndex = Math.min(state.popupIndex + 1, state.menuActions.length - 1);
    renderMenu();
    return;
  }
  if (event.key === "ArrowUp" || event.key === "k") {
    event.preventDefault();
    state.popupIndex = Math.max(0, state.popupIndex - 1);
    renderMenu();
    return;
  }
  if (event.key === "Enter") { event.preventDefault(); chooseMenu(); return; }
  const idx = state.menuActions.findIndex(action => action.key === event.key);
  if (idx >= 0) { event.preventDefault(); chooseMenu(idx); }
}

function fuzzyScore(text, query) {
  text = text.toLowerCase();
  query = query.toLowerCase();
  let cursor = 0, score = 0;
  for (const char of query) {
    const found = text.indexOf(char, cursor);
    if (found < 0) return -1;
    score += found === cursor ? 4 : Math.max(1, 3 - (found - cursor));
    cursor = found + 1;
  }
  return score - text.length * .001;
}

function toggleHelp() {
  if (state.help) { closeHelp(); return; }
  helpBody.innerHTML = HELP_SECTIONS.map(section =>
    `<div class="help-group"><h3>${escapeHtml(section.title)}</h3><dl>${section.keys.map(
      ([key, desc]) => `<dt>${escapeHtml(key)}</dt><dd>${escapeHtml(desc)}</dd>`
    ).join("")}</dl></div>`).join("");
  state.help = true;
  helpBackdrop.hidden = false;
  document.getElementById("help").focus({ preventScroll: true });
}

function closeHelp() {
  state.help = false;
  helpBackdrop.hidden = true;
  setFocus(state.focusLevel, state.pane);
}

function showPopup(kind, title, items, placeholder = "") {
  state.popup = kind;
  state.popupItems = items;
  state.popupIndex = 0;
  popupTitle.textContent = title;
  popupInput.placeholder = placeholder;
  popupInput.value = "";
  popupInput.hidden = kind === "tree";
  popup.classList.toggle("tree-popup", kind === "tree");
  popupPreview.hidden = kind !== "tree";
  popupHint.textContent = kind === "tree"
    ? "j/k move · h/l collapse/expand · Enter open · ⌥/⌘Enter new tab · / filter · J/K preview · Esc close"
    : kind === "search"
    ? "type to search project · ↑↓ move · Enter open · Esc close"
    : kind === "quick"
    ? "↑↓ move · Enter select · > commands · @ symbols · Esc close"
    : kind === "ref"
    ? "type to filter · Enter select · any commit/tag ref works · Esc close"
    : "arrows move · Enter select · Esc close";
  if (kind !== "tree") popupPreview.innerHTML = "";
  popupBackdrop.hidden = false;
  filterPopup();
  if (kind === "tree") popup.focus();
  else popupInput.focus();
}

function closePopup() {
  state.popup = null;
  state.treePreviewToken++;
  popupBackdrop.hidden = true;
  popupInput.hidden = false;
  setFocus(state.focusLevel, state.pane);
}

function renderPopupList(emptyText = "No matches") {
  popupList.innerHTML = state.popupFiltered.map((item, index) =>
    `<li data-index="${index}" class="${index === state.popupIndex ? "selected " : ""}${item.cls || ""}">
      ${item.html || escapeHtml(item.label)}${item.hint && !item.html ? `<span class="hint">${escapeHtml(item.hint)}</span>` : ""}
    </li>`).join("") || `<li>${escapeHtml(emptyText)}</li>`;
  popupList.querySelectorAll("[data-index]").forEach(li => li.addEventListener("click", () => choosePopup(Number(li.dataset.index))));
  popupList.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
}

function filterPopup() {
  const raw = popupInput.value;
  if (state.popup === "search") {
    if (raw.trim() === state.lastSearchQuery) renderPopupList();
    else scheduleSearch();
    return;
  }
  let query = raw.trim();
  if (state.popup === "tree") {
    state.popupItems = treePopupItems(query);
  } else if (state.popup === "quick") {
    const resolved = resolveQuickMode(raw);
    if (resolved.mode !== state.quickMode) { state.quickMode = resolved.mode; state.popupIndex = 0; }
    state.popupItems = resolved.items;
    query = resolved.query;
    popupTitle.textContent = resolved.title;
    if (resolved.mode === "symbols" && !state.quickSymbolsLoaded) loadQuickSymbols();
  }
  state.popupFiltered = query
    ? state.popupItems
      .map(item => ({ ...item, score: fuzzyScore(item.search || item.label, query) }))
      .filter(item => item.score >= 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, 300)
    : state.popupItems.slice(0, 300);
  // Ref picker: a query that doesn't exactly name a known ref still resolves —
  // offer it verbatim so arbitrary commit/tag refs can be entered.
  if (state.popup === "ref" && query
      && !state.popupFiltered.some(item => item.label === query)) {
    const which = state.refPickerWhich;
    state.popupFiltered.unshift({
      label: query, search: query, hint: "use verbatim",
      run: () => applyRef(which, query),
    });
  }
  state.popupIndex = Math.min(state.popupIndex, Math.max(0, state.popupFiltered.length - 1));
  renderPopupList();
  if (state.popup === "tree") updateTreePreview();
}

async function choosePopup(index = state.popupIndex) {
  const item = state.popupFiltered[index];
  if (!item) return;
  if (!item.keepOpen) closePopup();
  await item.run();
}

// Unified quick-open (VSCode / CLI style): one picker whose mode is driven by a
// leading sigil — `>` runs commands, `@` jumps to symbols, anything else is the
// fuzzy file picker. `initial` seeds the input so the entry-point shortcuts
// (⌘P / ⌘⇧P / ⌘@) land directly in the right mode.
async function openQuickPicker(initial = "") {
  await ensureFiles();
  const entries = [...state.fileEntries].sort((a, b) =>
    Number(b.changed) - Number(a.changed)
    || Number(b.mtime || 0) - Number(a.mtime || 0)
    || a.path.localeCompare(b.path)
  );
  state.quickFiles = entries.map(entry => ({
    label: entry.path,
    hint: entry.changed ? "changed" : "",
    run: () => openFile(entry.path),
  }));
  state.quickCommands = [...COMMANDS].sort((a, b) => a.label.localeCompare(b.label)).map(command => ({
    label: command.label, hint: command.hint, run: command.run,
  }));
  state.quickSymbols = [];
  state.quickSymbolsLoaded = false;
  state.quickMode = "files";
  showPopup("quick", "Files", [], "Search files · > commands · @ symbols");
  if (initial) { popupInput.value = initial; filterPopup(); }
}

function resolveQuickMode(raw) {
  if (raw.startsWith(">")) {
    return { mode: "commands", items: state.quickCommands, query: raw.slice(1).trim(), title: "Commands · type to filter" };
  }
  if (raw.startsWith("@")) {
    return { mode: "symbols", items: state.quickSymbols, query: raw.slice(1).trim(), title: "Symbols · type to filter" };
  }
  return { mode: "files", items: state.quickFiles, query: raw.trim(), title: "Files · > commands · @ symbols" };
}

async function loadQuickSymbols() {
  state.quickSymbolsLoaded = true;
  if (!state.currentFile) { state.quickSymbols = []; return; }
  try {
    const data = await api("/api/symbols", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ path: state.currentFile, content: state.fileContent }),
    });
    state.quickSymbols = (data.symbols || []).map(symbol => ({
      label: symbol.name, hint: `${symbol.kind} · ${symbol.line + 1}`,
      run: () => openFile(state.currentFile, symbol.line, symbol.col),
    }));
  } catch (_) {
    state.quickSymbols = [];
  }
  if (state.popup === "quick") filterPopup();
}

async function openTreePicker() {
  await ensureFiles();
  state.treeRoot = buildTree(state.fileEntries);
  // Reveal and select the currently open file so the tree lands focused on it.
  if (state.currentFile) {
    const parts = state.currentFile.split("/");
    for (let i = 1; i < parts.length; i++) state.treeExpanded.add(parts.slice(0, i).join("/"));
  }
  showPopup("tree", "Explorer", treePopupItems(""), "Filter tree");
  if (state.currentFile) {
    const index = state.popupFiltered.findIndex(item => item.node?.path === state.currentFile);
    if (index >= 0) {
      state.popupIndex = index;
      renderPopupList();
      popupList.querySelector(".selected")?.scrollIntoView({ block: "center" });
      updateTreePreview();
    }
  }
}

function buildTree(entries) {
  const root = { name: "", path: "", type: "dir", children: new Map(), changed: false, depth: -1 };
  for (const entry of entries) {
    let current = root;
    const parts = entry.path.split("/");
    if (parts.some(part => part.startsWith("."))) continue;
    parts.forEach((name, index) => {
      const path = parts.slice(0, index + 1).join("/");
      const type = index === parts.length - 1 ? "file" : "dir";
      if (!current.children.has(name)) {
        current.children.set(name, {
          name, path, type, children: new Map(), changed: false, depth: index,
        });
      }
      current = current.children.get(name);
      current.changed ||= Boolean(entry.changed);
    });
  }
  return root;
}

function sortedTreeChildren(node) {
  return [...node.children.values()].sort((a, b) =>
    Number(a.type === "file") - Number(b.type === "file")
    || a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
  );
}

function allTreeNodes(node, output = []) {
  for (const child of sortedTreeChildren(node)) {
    output.push(child);
    if (child.type === "dir") allTreeNodes(child, output);
  }
  return output;
}

function visibleTreeNodes(node, output = []) {
  for (const child of sortedTreeChildren(node)) {
    output.push(child);
    if (child.type === "dir" && state.treeExpanded.has(child.path)) {
      visibleTreeNodes(child, output);
    }
  }
  return output;
}

function treePopupItems(query) {
  if (!state.treeRoot) return [];
  const nodes = query ? allTreeNodes(state.treeRoot) : visibleTreeNodes(state.treeRoot);
  return nodes.map(node => {
    const expanded = node.type === "dir" && state.treeExpanded.has(node.path);
    const indent = query ? 0 : node.depth;
    const icon = node.type === "dir" ? (expanded ? "▾" : "▸") : "·";
    return {
      label: node.path,
      search: node.path,
      node,
      keepOpen: node.type === "dir",
      html: `<span class="tree-indent" style="width:${indent * 16}px"></span>
        <span class="tree-icon">${icon}</span>
        <span class="tree-name ${node.changed ? "status-modified" : ""}">${escapeHtml(node.name)}</span>
        ${query ? `<span class="tree-path">${escapeHtml(node.path)}</span>` : ""}`,
      run: async () => {
        if (node.type === "dir") {
          if (state.treeExpanded.has(node.path)) state.treeExpanded.delete(node.path);
          else state.treeExpanded.add(node.path);
          filterPopup();
        } else {
          await openFile(node.path);
        }
      },
    };
  });
}

async function updateTreePreview() {
  const item = state.popupFiltered[state.popupIndex];
  const node = item?.node;
  const token = ++state.treePreviewToken;
  if (!node) {
    popupPreview.innerHTML = `<div class="empty">No selection</div>`;
    return;
  }
  if (node.type === "dir") {
    popupPreview.innerHTML = `<div class="preview-title">${escapeHtml(node.path)}/</div>
      <div class="empty">${node.children.size} entries · ${state.treeExpanded.has(node.path) ? "expanded" : "collapsed"}</div>`;
    return;
  }
  popupPreview.innerHTML = `<div class="preview-title">${escapeHtml(node.path)}</div><div class="loading">Loading preview…</div>`;
  try {
    const data = await api(`/api/file?path=${encodeURIComponent(node.path)}`);
    const previewContent = data.content.split("\n").slice(0, 200).join("\n");
    const html = await highlightedTextHtml(node.path, previewContent);
    if (token !== state.treePreviewToken || state.popup !== "tree") return;
    popupPreview.innerHTML = `<div class="preview-title">${escapeHtml(node.path)}</div><pre>${html}</pre>`;
  } catch (error) {
    if (token !== state.treePreviewToken || state.popup !== "tree") return;
    popupPreview.innerHTML = `<div class="preview-title">${escapeHtml(node.path)}</div><div class="error">${escapeHtml(error.message)}</div>`;
  }
}

// Project-wide text search (Cmd+Shift+F). Backed by /api/search, which is a
// case-insensitive literal substring search with a 3-character minimum.
function openSearchPopup() {
  state.lastSearchQuery = null;
  showPopup("search", "Search project", [], "Search across files");
}

function scheduleSearch() {
  clearTimeout(scheduleSearch.timer);
  scheduleSearch.timer = setTimeout(runGlobalSearch, 150);
}

async function runGlobalSearch() {
  const query = popupInput.value.trim();
  const token = ++state.searchToken;
  state.lastSearchQuery = query;
  if (query.length < 3) {
    state.popupFiltered = [];
    popupList.innerHTML = `<li>${query ? "Type at least 3 characters…" : "Search across the project"}</li>`;
    return;
  }
  popupList.innerHTML = `<li>Searching…</li>`;
  try {
    const data = await api(`/api/search?${new URLSearchParams({ q: query, max: "500" })}`);
    if (token !== state.searchToken || state.popup !== "search") return;
    state.searchHits = data.hits || [];
    state.searchQuery = query;
    state.searchCollapsed = new Set();
    state.popupIndex = 0;
    if (!state.searchHits.length) {
      state.popupFiltered = [];
      popupList.innerHTML = `<li>No matches</li>`;
      popupTitle.textContent = "Search project · no matches";
      return;
    }
    state.popupFiltered = buildSearchRows();
    renderPopupList("No matches");
    const files = new Set(state.searchHits.map(hit => hit.path)).size;
    popupTitle.textContent =
      `Search project · ${state.searchHits.length}${data.truncated ? "+" : ""} matches in ${files} file${files === 1 ? "" : "s"}`;
  } catch (error) {
    if (token !== state.searchToken || state.popup !== "search") return;
    popupList.innerHTML = `<li>${escapeHtml(error.message)}</li>`;
  }
}

// Flatten the path-sorted hits into a collapsible per-file tree: one file header
// row (chevron · path · count) followed by its match rows (line · excerpt).
// Rows under a collapsed file are dropped so keyboard nav skips them.
function buildSearchRows() {
  const counts = new Map();
  for (const hit of state.searchHits) counts.set(hit.path, (counts.get(hit.path) || 0) + 1);
  const rows = [];
  let curPath = null;
  for (const hit of state.searchHits) {
    if (hit.path !== curPath) {
      curPath = hit.path;
      const path = hit.path;
      const collapsed = state.searchCollapsed.has(path);
      rows.push({
        kind: "file", path, cls: "gfile", keepOpen: true,
        html: `<span class="gchevron">${collapsed ? "▸" : "▾"}</span>`
          + `<span class="gfile-path">${escapeHtml(path)}</span>`
          + `<span class="gcount">${counts.get(path)}</span>`,
        run: () => toggleSearchFile(path),
      });
    }
    if (state.searchCollapsed.has(hit.path)) continue;
    rows.push({
      kind: "hit", cls: "ghit",
      html: `<span class="gline">${hit.line + 1}</span>`
        + `<span class="gtext">${highlightExcerpt(hit.excerpt, hit.col, state.searchQuery.length)}</span>`,
      run: () => openFile(hit.path, hit.line, hit.col),
    });
  }
  return rows;
}

function toggleSearchFile(path) {
  if (state.searchCollapsed.has(path)) state.searchCollapsed.delete(path);
  else state.searchCollapsed.add(path);
  state.popupFiltered = buildSearchRows();
  state.popupIndex = Math.min(state.popupIndex, state.popupFiltered.length - 1);
  renderPopupList("No matches");
}

// Bold `qlen` characters of the excerpt from the server-provided 0-based column.
function highlightExcerpt(excerpt, col, qlen) {
  const chars = Array.from(String(excerpt || ""));
  const start = Math.max(0, Math.min(col, chars.length));
  const end = Math.max(start, Math.min(col + qlen, chars.length));
  return escapeHtml(chars.slice(0, start).join(""))
    + (end > start ? `<span class="match">${escapeHtml(chars.slice(start, end).join(""))}</span>` : "")
    + escapeHtml(chars.slice(end).join(""));
}

popupInput.addEventListener("input", filterPopup);
popupInput.addEventListener("keydown", event => {
  if (event.key === "Escape") {
    event.preventDefault();
    if (state.popup === "tree") {
      popupInput.value = "";
      popupInput.hidden = true;
      filterPopup();
      popup.focus();
    } else {
      closePopup();
    }
  } else if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) {
    event.preventDefault();
    state.popupIndex = Math.min(state.popupIndex + 1, state.popupFiltered.length - 1);
    filterPopup();
  } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
    event.preventDefault();
    state.popupIndex = Math.max(0, state.popupIndex - 1);
    filterPopup();
  } else if (state.popup === "search" && (event.key === "ArrowRight" || event.key === "ArrowLeft")) {
    const item = state.popupFiltered[state.popupIndex];
    if (item?.kind === "file" && (event.key === "ArrowRight") === state.searchCollapsed.has(item.path)) {
      event.preventDefault();
      toggleSearchFile(item.path);
    }
  } else if (event.key === "Enter" && state.popup === "tree" && (event.altKey || event.metaKey)) {
    event.preventDefault();
    openTreeSelectionInNewTab();
  } else if (event.key === "Enter") {
    event.preventDefault();
    choosePopup();
  }
});

popup.addEventListener("keydown", event => {
  if (state.popup === "menu") { handleMenuKey(event); return; }
  if (state.popup !== "tree" || !popupInput.hidden) return;
  if (event.key === "Escape") {
    event.preventDefault();
    closePopup();
  } else if (event.key === "/" || event.key === "f") {
    event.preventDefault();
    popupInput.hidden = false;
    popupInput.focus();
  } else if (event.key === "j" || event.key === "ArrowDown") {
    event.preventDefault();
    state.popupIndex = Math.min(state.popupIndex + 1, state.popupFiltered.length - 1);
    filterPopup();
  } else if (event.key === "k" || event.key === "ArrowUp") {
    event.preventDefault();
    state.popupIndex = Math.max(0, state.popupIndex - 1);
    filterPopup();
  } else if (event.key === "Enter" && (event.altKey || event.metaKey)) {
    event.preventDefault();
    openTreeSelectionInNewTab();
  } else if (event.key === "l" || event.key === "ArrowRight" || event.key === "Enter") {
    event.preventDefault();
    choosePopup();
  } else if (event.key === "h" || event.key === "ArrowLeft") {
    event.preventDefault();
    collapseTreeSelection();
  } else if (event.shiftKey && event.key === "J") {
    event.preventDefault();
    popupPreview.scrollBy({ top: popupPreview.clientHeight * 0.25, behavior: "smooth" });
  } else if (event.shiftKey && event.key === "K") {
    event.preventDefault();
    popupPreview.scrollBy({ top: -popupPreview.clientHeight * 0.25, behavior: "smooth" });
  }
});

function collapseTreeSelection() {
  const item = state.popupFiltered[state.popupIndex];
  const node = item?.node;
  if (!node) return;
  if (node.type === "dir" && state.treeExpanded.has(node.path)) {
    state.treeExpanded.delete(node.path);
    filterPopup();
    return;
  }
  const parentPath = node.path.includes("/") ? node.path.slice(0, node.path.lastIndexOf("/")) : "";
  if (!parentPath) return;
  const parentIndex = state.popupFiltered.findIndex(candidate => candidate.node?.path === parentPath);
  if (parentIndex >= 0) {
    state.popupIndex = parentIndex;
    filterPopup();
  }
}

header.addEventListener("click", event => {
  const button = event.target.closest("[data-component]");
  if (button) switchComponent(button.dataset.component);
});

// Click the dimmed area outside a picker / help dialog → dismiss it. The dialog
// itself (`#popup` / `#help`) stops the event from reaching the backdrop.
popupBackdrop.addEventListener("mousedown", event => {
  if (event.target === popupBackdrop) closePopup();
});
helpBackdrop.addEventListener("mousedown", event => {
  if (event.target === helpBackdrop) closeHelp();
});

// Click anywhere outside the editor while in insert mode → leave insert mode.
// Clicks inside the editor, or inside a popup/help dialog (which manage their
// own focus), are ignored.
document.addEventListener("mousedown", event => {
  if (state.editorMode !== "insert") return;
  if (event.target.closest(".editor-input, #popup-backdrop, #help-backdrop")) return;
  leaveEditorInsertMode();
});

window.addEventListener("keydown", async event => {
  const isText = event.target.matches("textarea, input");
  if (state.help) {
    if (event.key === "Escape" || event.key === "?") {
      event.preventDefault();
      closeHelp();
    }
    return;
  }
  if (state.popup) return;

  // Preserve native browser focus-location and reload shortcuts.
  if (event.metaKey && ["l", "r"].includes(event.key.toLowerCase())) return;

  if (event.metaKey && event.shiftKey && event.key.toLowerCase() === "f") {
    event.preventDefault();
    openSearchPopup();
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "p") {
    event.preventDefault();
    openQuickPicker(event.shiftKey ? ">" : "");
    return;
  }
  if (event.metaKey && (event.key === "@" || (event.shiftKey && event.key === "2"))) {
    event.preventDefault();
    openQuickPicker("@");
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
    event.preventDefault();
    // Only persist while actively editing — Cmd+S in a read-only view (diff
    // panes, or a file opened but not being edited) shouldn't write anything.
    if (state.editorMode === "insert") await saveCurrentFile();
    return;
  }
  if (event.metaKey && !event.shiftKey && event.key.toLowerCase() === "d"
      && event.target.classList?.contains("editor-input")) {
    event.preventDefault();
    multiCursorAddNext();
    return;
  }
  // Undo/redo run off the editor's own history stack (the textarea's native one
  // is wiped by programmatic edits): Cmd+Z undo, Cmd+Shift+Z / Cmd+Y redo.
  if (event.metaKey && event.key.toLowerCase() === "z"
      && event.target.classList?.contains("editor-input")) {
    event.preventDefault();
    if (event.shiftKey) editorRedo(); else editorUndo();
    return;
  }
  if (event.metaKey && event.key.toLowerCase() === "y"
      && event.target.classList?.contains("editor-input")) {
    event.preventDefault();
    editorRedo();
    return;
  }
  if (event.key === "Escape") {
    if (isText && event.target.classList.contains("editor-input")) {
      event.preventDefault();
      // Esc collapses an active selection first (staying in insert mode);
      // only a second Esc (no selection) returns to app focus.
      const input = event.target;
      if (input.selectionStart !== input.selectionEnd) {
        input.setSelectionRange(input.selectionEnd, input.selectionEnd);
      } else {
        leaveEditorInsertMode();
      }
    } else if (state.component === "explorer" && state.focusLevel === "app") {
      return;
    } else if (state.focusLevel === "pane" && state.pane > 0) {
      event.preventDefault();
      setFocus("pane", state.pane - 1);
    } else if (state.focusLevel === "pane") {
      event.preventDefault();
      setFocus("component", 0);
    } else {
      event.preventDefault();
      setFocus("app", 0);
    }
    return;
  }
  if (isText) return;

  if (event.key === "?") {
    event.preventDefault();
    toggleHelp();
    return;
  }
  if (event.key === "O") {
    event.preventDefault();
    openOpenMenu();
    return;
  }

  if (state.gPending) {
    state.gPending = false;
    if (event.key === "g" && state.component === "explorer") {
      event.preventDefault();
      gotoEditorEdge("top");
      return;
    }
    const target = ({ e: "explorer", h: "history", c: "compare", s: "status" })[event.key];
    if (target) { event.preventDefault(); await switchComponent(target); }
    return;
  }
  if (event.key === "g") {
    event.preventDefault();
    state.gPending = true;
    focusPath.textContent = "g …";
    setTimeout(() => { state.gPending = false; updateFocusChrome(); }, 10000);
    return;
  }
  if (state.focusLevel === "app" && event.key === "t") {
    event.preventDefault(); openTreePicker(); return;
  }
  if ((event.key === "i" || event.key === "Enter") && enterEditorInsertMode()) {
    event.preventDefault();
    return;
  }
  if (state.component === "explorer" && state.focusLevel === "app"
      && (event.key === "j" || event.key === "k")) {
    event.preventDefault();
    scrollExplorer(event.key === "j" ? 1 : -1);
    return;
  }
  if (state.component === "explorer" && state.focusLevel === "app" && event.key === "G") {
    event.preventDefault();
    gotoEditorEdge("bottom");
    return;
  }
  if (state.component === "explorer" && state.focusLevel === "app" && event.key === "p") {
    event.preventDefault();
    await togglePreview();
    return;
  }
  if (state.component === "compare" && event.shiftKey && event.key === "J") {
    event.preventDefault();
    scrollPreview(1, 0.25);
    return;
  }
  if (state.component === "compare" && event.shiftKey && event.key === "K") {
    event.preventDefault();
    scrollPreview(-1, 0.25);
    return;
  }
  if (state.component === "history" && event.shiftKey && event.key === "J") {
    event.preventDefault();
    await moveHistoryFile(1);
    return;
  }
  if (state.component === "history" && event.shiftKey && event.key === "K") {
    event.preventDefault();
    await moveHistoryFile(-1);
    return;
  }
  if (state.component === "compare" && (event.key === "B" || event.key === "C")) {
    event.preventDefault();
    await openRefPicker(event.key === "B" ? "base" : "target");
    return;
  }
  if (state.component === "compare" && state.pane === 0 && event.key === "v") {
    event.preventDefault();
    await toggleCompareViewed();
    return;
  }
  if (state.component === "compare" && state.pane === 0 && event.key === "o") {
    event.preventDefault();
    await openSelectedDiffFileInEditor();
    return;
  }
  if (state.component === "status" && state.pane === 0 && event.key === "v") {
    event.preventDefault();
    await toggleStatusViewed();
    return;
  }
  if (state.component === "status" && state.pane === 0 && event.key === "o") {
    event.preventDefault();
    await openSelectedDiffFileInEditor();
    return;
  }
  if (event.key === "r") {
    event.preventDefault(); await refreshComponent(); return;
  }
  if (isPreviewPaneFocused() && (event.key === "j" || event.key === "k")) {
    event.preventDefault();
    scrollPreview(event.key === "j" ? 1 : -1, 0.25);
    return;
  }
  if (event.key === "j") { event.preventDefault(); await moveSelection(1); return; }
  if (event.key === "k") { event.preventDefault(); await moveSelection(-1); return; }
  if (event.key === "l" || event.key === "Tab") {
    event.preventDefault();
    const count = app.querySelectorAll(".pane").length;
    setFocus("pane", Math.min(state.pane + 1, count - 1));
    return;
  }
  if (event.key === "h") {
    event.preventDefault();
    if (state.pane > 0) setFocus("pane", state.pane - 1);
    else setFocus("component", 0);
    return;
  }
  if (event.ctrlKey && (event.key === "d" || event.key === "u")) {
    event.preventDefault();
    scrollPreview(event.key === "d" ? 1 : -1);
  }
});

// Warn before closing/reloading the tab while the open file has unsaved edits.
// The browser shows its own native confirm dialog when returnValue is set.
window.addEventListener("beforeunload", event => {
  if (state.currentFile && state.fileContent !== state.fileBaseContent) {
    event.preventDefault();
    event.returnValue = "";
  }
});

async function boot() {
  try {
    loadRepoInfo();
    const last = await api("/api/last-file").catch(() => ({ path: null }));
    if (last.path) {
      const data = await api(`/api/file?path=${encodeURIComponent(last.path)}`);
      state.currentFile = data.path;
      state.fileContent = data.content;
      state.fileBaseContent = data.content;
      state.fileHash = data.hash;
    }
    const pathComponent = location.pathname === "/status" || location.pathname === "/changes"
      || location.pathname === "/diff" ? "status"
      : location.pathname === "/compare" || location.pathname === "/branches" ? "compare"
      : location.pathname.includes("/commits") || location.pathname.includes("/commit/") ? "history"
      : "explorer";
    const requested = location.hash.slice(1) || pathComponent;
    await switchComponent(COMPONENTS.includes(requested) ? requested : "explorer");
    const fileParam = new URLSearchParams(location.search).get("path");
    if (fileParam) {
      await openFile(fileParam)
        .catch(error => notify(`Cannot open ${fileParam}: ${error.message}`));
    }
  } catch (error) {
    app.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
  }
}

boot();

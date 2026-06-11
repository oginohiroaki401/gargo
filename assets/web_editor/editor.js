const app = document.getElementById("app");
const header = document.getElementById("header");
const versionText = document.getElementById("version-text");
const versionUpdate = document.getElementById("version-update");
const helpToggle = document.getElementById("help-toggle");
const popupBackdrop = document.getElementById("popup-backdrop");
const popupTitle = document.getElementById("popup-title");
const popupInput = document.getElementById("popup-input");
const popupList = document.getElementById("popup-list");
const popup = document.getElementById("popup");
const popupPreview = document.getElementById("popup-preview");
const popupHint = document.getElementById("popup-hint");
const toast = document.getElementById("toast");
const connBanner = document.getElementById("conn-banner");
const helpBackdrop = document.getElementById("help-backdrop");
const helpBody = document.getElementById("help-body");
const repoLink = document.getElementById("repo-link");
const repoSep = document.getElementById("repo-sep");
const commitBackdrop = document.getElementById("commit-backdrop");
const commitBranch = document.getElementById("commit-branch");
const commitSummary = document.getElementById("commit-summary");
const commitMessage = document.getElementById("commit-message");
const commitAmendRow = document.getElementById("commit-amend-row");
const commitAmend = document.getElementById("commit-amend");
const commitSubmit = document.getElementById("commit-submit");
const commitCancel = document.getElementById("commit-cancel");

const COMPONENTS = ["explorer", "history", "compare", "status", "search"];
const state = {
  component: "explorer",
  connected: true,
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
  commits: [],
  historyCommit: 0,
  historyFile: 0,
  historyData: null,
  historySignature: "",
  historyPollTimer: null,
  refs: [],
  refInfo: {},
  commit: null,
  compareBase: "",
  compareTarget: "",
  refPickerWhich: "base",
  compareFiles: [],
  compareFile: 0,
  statusFiles: [],
  statusFile: 0,
  statusBranch: "",
  statusSignature: "",
  statusPollTimer: null,
  previewToken: 0,
  paneSizes: {},        // kind -> [fr, …] column widths for the diff-view panes
  popup: null,
  popupItems: [],
  popupFiltered: [],
  popupIndex: 0,
  treeRoot: null,
  treeExpanded: new Set(),
  treePreviewToken: 0,
  showHidden: false,       // tree: `.` toggles dot-prefixed files/dirs into view
  promptResolve: null,     // resolver for the active promptText()/confirmAction() overlay
  help: false,
  searchToken: 0,
  searchHits: [],          // flat, path-sorted match hits from /api/search
  searchRows: [],          // visible rows (file headers + hits), honoring collapse
  searchQuery: "",
  searchSelected: 0,       // index into searchRows (selected row in the Search tab)
  searchCollapsed: new Set(), // collapsed file paths
  searchTruncated: false,
  repoInfo: null,
  quickFiles: [],
  quickCommands: [],
  quickSymbols: [],
  quickSymbolsLoaded: false,
  quickMode: "files",
  menuActions: [],
  find: {
    open: false,
    replace: false,
    matches: [],   // [{start, end}] offsets into the textarea value
    index: -1,     // current match
    caseSensitive: false,
    wholeWord: false,
    regex: false,
    pendingCaret: null, // where the next i/Enter should drop the caret (last match)
    kept: null,         // {start,end} of a match kept highlighted after the bar closes
    cache: null,        // last computed match set for the same buffer/query/options
  },
};

const HELP_SECTIONS = [
  {
    title: "Global", keys: [
      ["g e / g h / g c / g s / g f", "Explorer / History / Compare / Status / Search"],
      ["⌘P / ⌘⇧P", "File picker / Command picker"],
      ["⌘@", "Symbol picker"],
      ["⌘⇧F", "Global search (Search tab)"],
      ["⌘S", "Save current file"],
      ["r", "Refresh component"],
      ["?", "Toggle this help"],
    ],
  },
  {
    title: "Explorer / Editor", keys: [
      ["t", "Open file tree"],
      ["a / r / d", "Tree: add / rename / delete entry"],
      ["c / y", "Tree: copy absolute / relative path"],
      [".", "Tree: toggle hidden files"],
      ["right-click", "Tree: context menu"],
      ["i / Enter", "Edit (insert) mode"],
      ["⌘F / ⌘⌥F", "Find / Find & replace in file"],
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
    title: "Status", keys: [
      ["u", "Stage / unstage selected file"],
      ["C", "Open commit dialog (message + staged summary)"],
      ["v", "Toggle viewed"],
      ["Cmd+Enter / Esc", "Commit dialog: commit / cancel"],
    ],
  },
  {
    title: "Search", keys: [
      ["⌘⇧F / g f", "Open the Search tab / focus the query box"],
      ["type", "Query (≥ 3 chars), live results"],
      ["Enter (in box)", "Move focus to the results list"],
      ["j / k", "Prev / next row (k at top → query box)"],
      ["h / l (← / →)", "Collapse / expand the file group"],
      ["J / K · Ctrl-f/b · Ctrl-d/u", "Scroll preview"],
      ["o / Enter", "Open file at the matched line"],
      ["e", "Open file in a new browser tab"],
      ["O", "Open menu: GitHub · copy path · copy content"],
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
  { label: "Switch to Search", hint: "g f", run: () => switchComponent("search") },
  { label: "Open file tree", hint: "t", run: () => openTreePicker() },
  { label: "Save current file", hint: "Cmd+S", run: () => saveCurrentFile() },
  { label: "Refresh current component", hint: "r", run: () => refreshComponent() },
  { label: "Search project", hint: "Cmd+Shift+F", run: () => switchComponent("search") },
  { label: "Show keybindings", hint: "?", run: () => toggleHelp() },
];

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, c => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

// Reflect whether the backend is reachable. `fetch` only rejects on a
// network-level failure (server gone, connection refused) — an HTTP error
// status still means the server is alive — so flipping to disconnected here is
// a reliable "the CLI died / was killed" signal. Recovers automatically once a
// later request (or the heartbeat) succeeds.
function setConnected(ok) {
  if (state.connected === ok) return;
  state.connected = ok;
  connBanner.hidden = ok;
  updateTitle();
}

async function api(url, options) {
  let response;
  try {
    response = await fetch(url, { cache: "no-store", ...options });
  } catch (error) {
    setConnected(false);
    throw error;
  }
  setConnected(true);
  const data = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(data.error || `${response.status} ${response.statusText}`);
  return data;
}

// Cheap poll so an idle tab still notices the server dying even when no
// component is actively refreshing. setConnected (via api) does the work.
async function heartbeat() {
  try {
    await api("/api/repo-info");
  } catch (_) {
    // Connection state already updated in api(); nothing else to do.
  }
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
  renderVersion();
  updateTitle();
}

function renderVersion() {
  const version = state.repoInfo?.version;
  versionText.textContent = version ? `gargo v${version}` : "gargo";
}

// Probe for a newer release (same check as `gargo --check`) and reveal the ↑
// badge if one exists. Fire-and-forget: failures (offline, rate-limited) leave
// the badge hidden. Clicking it points the user at the upgrade command.
async function checkForUpdate() {
  let data;
  try {
    data = await api("/api/update-check");
  } catch (_) {
    return;
  }
  if (!data?.has_update) return;
  versionUpdate.hidden = false;
  versionUpdate.title = data.latest
    ? `Update available: v${data.latest} — run \`gargo --update\``
    : "Update available — run `gargo --update`";
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
  document.title = state.connected
    ? `${repo}/${detail}`
    : `⚠ Disconnected · ${repo}`;
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
    if (!button.dataset.component) return;
    button.classList.toggle("active", button.dataset.component === state.component);
  });
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
  if (component === "search") await renderSearch();
  setFocus(component === "explorer" ? "app" : "pane", 0);
  if (component === "status") startStatusPolling();
  if (component === "history") startHistoryPolling();
  if (component === "search") focusSearchInput();
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
  state.fileEntries = data.entries || state.files.map(path => ({ path, mtime: 0, opened: 0, changed: false }));
}

async function renderExplorer() {
  app.innerHTML = `<section class="component">
    ${componentBar("Explorer", `<span><span class="key">t</span> tree · <span class="key">⌘P</span> files · <span class="key">⌘⇧P</span> commands · <span class="key">⌘@</span> symbols · <span class="key">⌘F</span> find · <span class="key">⌘⇧F</span> search · <span class="key">p</span> preview · <span class="key">?</span> help</span>`)}
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
  // Clicks on rendered links navigate: relative links open the target in this
  // same preview pane, external URLs open a new tab. In-page #anchors fall
  // through so the iframe scrolls them natively. The srcdoc frame is
  // same-origin, so we can reach into its document to intercept clicks.
  frame.addEventListener("load", () => {
    let doc;
    try { doc = frame.contentDocument; } catch (_) { return; }
    if (!doc) return;
    doc.addEventListener("click", event => {
      const anchor = event.target.closest && event.target.closest("a");
      const href = anchor && anchor.getAttribute("href");
      if (!href || href.startsWith("#")) return;
      event.preventDefault();
      navigateEditorLink(href);
    });
  });
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

// ---- Link navigation -------------------------------------------------------
//
// Cmd/Ctrl-click in the editor (insert or read-only) and clicks in the rendered
// preview follow links: relative links open the target file (staying in the
// current preview/code mode), external URLs open in a new tab.

// True for links that point outside the repo — a scheme like http:/mailto: or a
// protocol-relative //host. These open in a browser tab, not the editor.
function isExternalLink(href) {
  return /^[a-z][a-z0-9+.-]*:/i.test(href) || href.startsWith("//");
}

// Resolve a relative (or repo-root `/foo`) link against the current file's
// directory into a clean repo-relative path, collapsing `.`/`..` and dropping
// the leading slash (the server rejects `..`, so we normalize it away here).
function resolveRelativePath(currentFile, href) {
  let combined;
  if (href.startsWith("/")) {
    combined = href.slice(1); // repo-root relative
  } else {
    const slash = (currentFile || "").lastIndexOf("/");
    const baseDir = slash >= 0 ? currentFile.slice(0, slash) : "";
    combined = baseDir ? `${baseDir}/${href}` : href;
  }
  const parts = [];
  for (const seg of combined.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") { if (parts.length) parts.pop(); continue; }
    parts.push(seg);
  }
  return parts.join("/");
}

// The link target covering `offset` in `value`, or null. Matches an inline
// markdown link `[text](target)` first (a click anywhere in it counts), then a
// bare http(s) URL.
function linkTargetAt(value, offset) {
  let m;
  const linkRe = /\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g;
  while ((m = linkRe.exec(value))) {
    if (offset >= m.index && offset <= m.index + m[0].length) return m[1];
  }
  const urlRe = /https?:\/\/[^\s)<>"']+/g;
  while ((m = urlRe.exec(value))) {
    if (offset >= m.index && offset <= m.index + m[0].length) return m[0];
  }
  return null;
}

// Follow a link href from the editor: external → new tab; in-repo → open the
// target file, keeping the current preview/code mode (openFile leaves
// state.previewMode untouched). In-page `#anchors` are the caller's business.
async function navigateEditorLink(href) {
  if (!href || href.startsWith("#")) return;
  if (isExternalLink(href)) { window.open(href, "_blank", "noopener"); return; }
  let clean = href.split("#")[0].split("?")[0];
  try { clean = decodeURIComponent(clean); } catch (_) { /* keep raw */ }
  const target = resolveRelativePath(state.currentFile, clean);
  if (!target) return;
  try {
    await openFile(target);
  } catch (error) {
    notify(`Can't open ${target}: ${error.message}`);
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
    <div class="find-overlay"></div>
    <div class="multi-overlay"></div></div>`;
  const input = body.querySelector(".editor-input");
  const layer = body.querySelector(".highlight-layer");
  state.multiRanges = [];
  state.multiWord = "";
  resetFindState(); // the find bar is recreated hidden below
  input.value = options.content || "";
  input.tabIndex = -1;
  input.readOnly = state.editorMode !== "insert";
  await updateHighlightLayer(layer, options.path, input.value, input);
  input.style.height = `${Math.max(input.scrollHeight, body.clientHeight)}px`;
  input.dataset.lines = String(input.value.split("\n").length);
  fetchGitGutter(options.path, input.value);
  editorHistoryInit(input.value);
  input.addEventListener("keydown", onEditorKeyDown);
  // Track IME composition: while composing (notably Japanese/Chinese), the buffer
  // churns through many intermediate states. We still repaint plain glyphs each
  // step (the textarea text is transparent, so the layer is the only thing the
  // user sees), but we hold off on history snapshots and the async syntax pass —
  // painting a stale highlight response over the composing text is what made it
  // flash/disappear. `compositionend` does the catch-up once the text commits.
  input.addEventListener("compositionstart", () => { input.composing = true; });
  input.addEventListener("compositionend", () => {
    input.composing = false;
    paintEditorPlain(input, layer);
    reflowEditorHeight(input, layer, body);
    updateDirtyIndicator();
    if (state.find.open) runFind(false);
    editorHistoryPush(false); // a finished composition is one undo step
    scheduleHighlight(input, layer, options.path);
    scheduleGitGutter(options.path, input.value);
  });
  input.addEventListener("input", event => {
    if (state.multiRanges.length) clearMultiCursors(); // a native edit exits multi-cursor
    state.fileContent = input.value;
    // The visible glyphs are the highlight layer (the textarea text is
    // transparent), so paint plain text synchronously for zero-lag feedback;
    // the server syntax pass refines it a beat later.
    paintEditorPlain(input, layer);
    reflowEditorHeight(input, layer, body);
    updateDirtyIndicator();
    if (state.find.open) runFind(false);
    if (event.isComposing || input.composing) return; // IME → defer to compositionend
    editorHistoryPush(true);
    scheduleHighlight(input, layer, options.path);
    scheduleGitGutter(options.path, input.value);
  });
  container.insertAdjacentHTML("beforeend", FIND_BAR_HTML);
  wireFindBar();
  input.addEventListener("mousedown", event => {
    clearFindKept(); // a click moves the caret → drop any kept find highlight
    // Cmd/Ctrl-click is a link jump (handled on `click`, after the caret lands)
    // — don't enter insert mode or clear multi-cursor for it.
    if (event.metaKey || event.ctrlKey) return;
    if (state.editorMode === "readonly") {
      // Click into a read-only editor → enter insert mode with the caret where
      // the click lands. Don't preventDefault: the native mousedown positions
      // the caret, and setFocus("editor") flips readOnly off and focuses.
      setFocus("editor", 0);
    } else {
      clearMultiCursors();
    }
  });
  // Cmd/Ctrl-click a URL or markdown link under the caret to follow it.
  input.addEventListener("click", event => {
    if (!(event.metaKey || event.ctrlKey)) return;
    const href = linkTargetAt(input.value, input.selectionStart);
    if (!href) return;
    event.preventDefault();
    navigateEditorLink(href);
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
  // Just searched? Drop the caret at the start of the match the find bar left us
  // on, instead of the first visible line.
  const pending = state.find.pendingCaret;
  clearFindKept(); // editing begins → drop the kept highlight
  if (pending != null) {
    input.setSelectionRange(pending, pending);
    updateEditorModeIndicator();
    setFocus("editor", 0);
    scrollEditorToCursor("auto");
    return true;
  }
  // Otherwise drop the caret on the first visible line so entering edit mode keeps
  // the viewport where the user was reading instead of jumping to the old caret.
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

// A cursor-motion key: arrows, or the macOS emacs motions Ctrl+N/P/F/B/A/E.
// (Plain Cmd/Alt combos are not motions.)
function isCursorMotionKey(event) {
  if (event.metaKey || event.altKey) return false;
  const k = event.key.toLowerCase();
  if (k === "arrowleft" || k === "arrowright" || k === "arrowup" || k === "arrowdown") return true;
  return event.ctrlKey && ["n", "p", "f", "b", "a", "e"].includes(k);
}

// New caret offset for a motion key applied at the current caret. Vertical moves
// keep the column; Ctrl+A/E go to line start/end.
function motionCaretTarget(input, event) {
  const value = input.value;
  const lines = value.split("\n");
  const at = input.selectionStart;
  const k = event.key.toLowerCase();
  const ctrl = event.ctrlKey;
  if (k === "arrowleft" || (ctrl && k === "b")) return Math.max(0, at - 1);
  if (k === "arrowright" || (ctrl && k === "f")) return Math.min(value.length, at + 1);
  if (ctrl && k === "a") return value.lastIndexOf("\n", at - 1) + 1;
  if (ctrl && k === "e") { const nl = value.indexOf("\n", at); return nl < 0 ? value.length : nl; }
  const { line, col } = offsetLineCol(value, at);
  if (k === "arrowup" || (ctrl && k === "p")) return line === 0 ? at : lineColToOffset(lines, line - 1, col);
  return line >= lines.length - 1 ? at : lineColToOffset(lines, line + 1, col); // ArrowDown / Ctrl+N
}

// When the editor view is showing (explorer, app focus, not preview) but the
// textarea isn't focused, a cursor-motion key wakes the editor: it focuses and
// applies that one motion, so e.g. `p` out of preview then Ctrl+N just works.
// Later motions land on the now-focused textarea (native on macOS). Returns
// true when it handled the key.
function wakeEditorWithMotion(event) {
  if (state.component !== "explorer" || state.focusLevel !== "app" || state.previewMode) return false;
  if (!isCursorMotionKey(event)) return false;
  const input = app.querySelector(".editor-input");
  if (!input || !enterEditorInsertMode()) return false;
  const target = motionCaretTarget(input, event); // from the caret enterEditorInsertMode placed
  input.setSelectionRange(target, target);
  scrollEditorToCursor("auto");
  return true;
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
  clearFindKept(); // moved away from the match → drop the kept highlight
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

// New offset for one caret after an arrow press. Horizontal clamps to the buffer
// ends; vertical keeps the column and stays put at the first/last line.
function moveCaretOffset(value, lines, offset, key, dir) {
  if (key === "ArrowLeft") return Math.max(0, offset - 1);
  if (key === "ArrowRight") return Math.min(value.length, offset + 1);
  const { line, col } = offsetLineCol(value, offset);
  const target = line + dir;
  if (target < 0 || target >= lines.length) return offset;
  return lineColToOffset(lines, target, col);
}

// Plain (or Shift-extended) arrow press while multi-cursor is active: move every
// cursor instead of dropping the secondary ones. A horizontal arrow with live
// selections collapses each to its leading edge first (VSCode-style); Shift moves
// only the head so selections grow/shrink. Cursors that land together merge.
function moveMultiCursors(event) {
  const input = app.querySelector(".editor-input");
  if (!input || input.readOnly) return;
  const value = input.value;
  const lines = value.split("\n");
  const dir = (event.key === "ArrowRight" || event.key === "ArrowDown") ? 1 : -1;
  const horizontal = event.key === "ArrowLeft" || event.key === "ArrowRight";
  const hasSelection = state.multiRanges.some(r => r.start !== r.end);
  const next = state.multiRanges.map(r => {
    if (event.shiftKey) {
      return { start: r.start, end: moveCaretOffset(value, lines, r.end, event.key, dir) };
    }
    if (hasSelection && horizontal) {
      const at = dir > 0 ? r.end : r.start; // collapse selection to its edge
      return { start: at, end: at };
    }
    const at = moveCaretOffset(value, lines, dir > 0 ? r.end : r.start, event.key, dir);
    return { start: at, end: at };
  });
  next.sort((a, b) => a.start - b.start || a.end - b.end);
  state.multiRanges = next.filter((r, i) =>
    i === 0 || r.start !== next[i - 1].start || r.end !== next[i - 1].end);
  const primary = dir > 0 ? state.multiRanges[state.multiRanges.length - 1] : state.multiRanges[0];
  input.setSelectionRange(primary.start, primary.end);
  state.multiGoalCol = offsetLineCol(value, primary.end).col;
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
  patchPlainChangedLines(layer, input.value);
  applyGitGutter(layer);
  const body = layer.closest(".code-body");
  input.dataset.lines = String(input.value.split("\n").length);
  input.style.height = "auto";
  input.style.height = `${Math.max(input.scrollHeight, body?.clientHeight || 0)}px`;
  updateDirtyIndicator();
  scheduleHighlight(input, layer, state.currentFile);
  scheduleGitGutter(state.currentFile, input.value);
  renderMultiCursors();
  if (state.find.open) runFind(false);
}

// ---- In-file find / replace ------------------------------------------------
//
// Cmd+F (Cmd+Alt+F for the replace row). Matches are computed against the
// textarea value and drawn as translucent bands in `.find-overlay`; the current
// match is also set as the textarea selection so Enter/Esc leave the caret on it
// (the "move cursor to the match" behaviour). Search options (case / whole word /
// regex) persist across opens in `state.find`.
const FIND_BAR_HTML = `<div id="find" hidden>
  <button id="find-expand" title="Toggle Replace (Cmd+Alt+F)">▸</button>
  <div class="find-rows">
    <div class="find-row">
      <input id="find-input" type="text" placeholder="Find"
        autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" />
      <button id="find-case" class="toggle" title="Match Case">Aa</button>
      <button id="find-word" class="toggle" title="Match Whole Word"><u>ab</u></button>
      <button id="find-regex" class="toggle" title="Use Regular Expression">.*</button>
      <span id="find-count"></span>
      <button id="find-prev" title="Previous match (Shift+Enter)">↑</button>
      <button id="find-next" title="Next match (Enter)">↓</button>
      <button id="find-close" title="Close (Esc)">✕</button>
    </div>
    <div class="find-row" id="replace-row">
      <input id="replace-input" type="text" placeholder="Replace"
        autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" />
      <button id="replace-one" title="Replace (Enter)">↪</button>
      <button id="replace-all" title="Replace All (Cmd+Enter)">⇉</button>
    </div>
  </div>
</div>`;

function resetFindState() {
  state.find.open = false;
  state.find.matches = [];
  state.find.index = -1;
  state.find.pendingCaret = null;
  state.find.kept = null;
  state.find.cache = null;
}

function findBar() { return app.querySelector("#find"); }

function wireFindBar() {
  const bar = findBar();
  if (!bar) return;
  const f = state.find;
  bar.classList.toggle("with-replace", f.replace);
  bar.querySelector("#find-case").classList.toggle("on", f.caseSensitive);
  bar.querySelector("#find-word").classList.toggle("on", f.wholeWord);
  bar.querySelector("#find-regex").classList.toggle("on", f.regex);
  bar.querySelector("#find-input").addEventListener("input", () => runFind(true));
  bar.querySelector("#find-input").addEventListener("keydown", onFindKeyDown);
  bar.querySelector("#replace-input").addEventListener("keydown", onReplaceKeyDown);
  bar.querySelector("#find-expand").addEventListener("click", () => toggleReplaceRow());
  bar.querySelector("#find-case").addEventListener("click", () => toggleFindOption("caseSensitive"));
  bar.querySelector("#find-word").addEventListener("click", () => toggleFindOption("wholeWord"));
  bar.querySelector("#find-regex").addEventListener("click", () => toggleFindOption("regex"));
  bar.querySelector("#find-prev").addEventListener("click", () => findPrev());
  bar.querySelector("#find-next").addEventListener("click", () => findNext());
  bar.querySelector("#find-close").addEventListener("click", () => closeFind());
  bar.querySelector("#replace-one").addEventListener("click", () => replaceOne());
  bar.querySelector("#replace-all").addEventListener("click", () => replaceAll());
}

function openFind(withReplace) {
  const input = app.querySelector(".editor-input");
  const bar = findBar();
  if (!input || !bar) return;
  const f = state.find;
  f.open = true;
  if (withReplace) f.replace = true;
  bar.hidden = false;
  bar.classList.toggle("with-replace", f.replace);
  const findInput = bar.querySelector("#find-input");
  // Prefill with the current single-line selection (VSCode behaviour).
  const sel = input.value.slice(input.selectionStart, input.selectionEnd);
  if (sel && !sel.includes("\n")) findInput.value = sel;
  runFind(true);
  findInput.focus();
  findInput.select();
}

function closeFind() {
  const f = state.find;
  if (!f.open) return;
  f.open = false;
  const bar = findBar();
  if (bar) bar.hidden = true;
  const input = app.querySelector(".editor-input");
  // Closing over a read-only editor with a live match: keep the match highlighted
  // (a persisted band) and remember it, so j/k still scroll, Cmd+C copies the
  // match, and i/Enter enters insert mode at its start. In insert mode we instead
  // keep the textarea focused so Enter types a newline.
  const hasMatch = input && f.matches.length && state.editorMode !== "insert";
  f.pendingCaret = hasMatch ? input.selectionStart : null;
  f.kept = hasMatch ? { start: input.selectionStart, end: input.selectionEnd } : null;
  f.matches = [];
  f.index = -1;
  if (!input) { renderFindMatches(); return; }
  if (state.editorMode === "insert") {
    input.focus({ preventScroll: true });
  } else {
    setFocus("app", 0); // editor's resting state — j/k / Enter / Cmd+C all work
  }
  renderFindMatches(); // draw the kept band (or clear, if there's no match)
}

// Drop the post-find kept highlight (and its caret), e.g. once the user scrolls
// away, starts editing, or dismisses it.
function clearFindKept() {
  if (state.find.pendingCaret == null && !state.find.kept) return;
  state.find.pendingCaret = null;
  state.find.kept = null;
  renderFindMatches();
}

function toggleReplaceRow() {
  const bar = findBar();
  if (!bar) return;
  state.find.replace = !state.find.replace;
  bar.classList.toggle("with-replace", state.find.replace);
  bar.querySelector("#find-input").focus();
}

function toggleFindOption(key) {
  state.find[key] = !state.find[key];
  const bar = findBar();
  if (!bar) return;
  const map = { caseSensitive: "#find-case", wholeWord: "#find-word", regex: "#find-regex" };
  bar.querySelector(map[key]).classList.toggle("on", state.find[key]);
  runFind(true);
  bar.querySelector("#find-input").focus();
}

// Build the search regex. Non-regex queries are escaped to a literal; whole-word
// wraps the pattern in \b…\b. Always global so we can scan for every match.
function findPattern(query) {
  const f = state.find;
  let pattern = f.regex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  if (f.wholeWord) pattern = `\\b(?:${pattern})\\b`;
  return new RegExp(pattern, f.caseSensitive ? "g" : "gi");
}

function computeFindMatches(value, query) {
  const re = findPattern(query);
  const out = [];
  let m;
  while ((m = re.exec(value))) {
    if (m[0] === "") { re.lastIndex++; continue; } // skip zero-width matches
    out.push({ start: m.index, end: m.index + m[0].length });
    if (out.length >= 5000) break; // sanity cap
  }
  return out;
}

function computeFindMatchesCached(value, query) {
  const f = state.find;
  const cache = f.cache;
  if (cache
      && cache.value === value
      && cache.query === query
      && cache.caseSensitive === f.caseSensitive
      && cache.wholeWord === f.wholeWord
      && cache.regex === f.regex) {
    return cache.matches;
  }
  const matches = computeFindMatches(value, query);
  f.cache = {
    value,
    query,
    caseSensitive: f.caseSensitive,
    wholeWord: f.wholeWord,
    regex: f.regex,
    matches,
  };
  return matches;
}

// Recompute matches for the current query. When `jump`, also move to the match
// at/after the caret; otherwise keep the current index (used after edits that
// shift offsets around).
function runFind(jump) {
  const input = app.querySelector(".editor-input");
  const bar = findBar();
  if (!input || !bar) return;
  const f = state.find;
  const query = bar.querySelector("#find-input").value;
  if (!query) {
    f.matches = [];
    f.index = -1;
    updateFindCount();
    renderFindMatches();
    return;
  }
  try { f.matches = computeFindMatchesCached(input.value, query); }
  catch (_) { f.matches = []; } // invalid regex while typing
  if (!f.matches.length) {
    f.index = -1;
    updateFindCount();
    renderFindMatches();
    return;
  }
  if (jump) {
    const off = input.selectionStart;
    let idx = f.matches.findIndex(m => m.start >= off);
    if (idx < 0) idx = 0;
    gotoMatch(idx);
  } else {
    if (f.index < 0 || f.index >= f.matches.length) f.index = 0;
    updateFindCount();
    renderFindMatches();
  }
}

// Select + scroll to the match at `idx`, wrapping around.
function gotoMatch(idx) {
  const f = state.find;
  const input = app.querySelector(".editor-input");
  if (!input || !f.matches.length) return;
  f.index = (idx + f.matches.length) % f.matches.length;
  const m = f.matches[f.index];
  input.setSelectionRange(m.start, m.end);
  scrollEditorToCursor("auto");
  updateFindCount();
  renderFindMatches();
}

function findNext() { if (state.find.matches.length) gotoMatch(state.find.index + 1); }
function findPrev() { if (state.find.matches.length) gotoMatch(state.find.index - 1); }

function updateFindCount() {
  const bar = findBar();
  if (!bar) return;
  const f = state.find;
  const el = bar.querySelector("#find-count");
  if (!bar.querySelector("#find-input").value) el.textContent = "";
  else if (!f.matches.length) el.textContent = "No results";
  else el.textContent = `${f.index + 1} of ${f.matches.length}`;
}

// Draw a band over every match; the current one gets a stronger tint. Mirrors the
// multi-cursor overlay's tab-expanded positioning so bands line up with glyphs.
function renderFindMatches() {
  const overlay = app.querySelector(".find-overlay");
  if (!overlay) return;
  const input = app.querySelector(".editor-input");
  const f = state.find;
  // While the bar is open: a band per match (current emphasized). Once closed: just
  // the single "kept" match left highlighted after Esc. Otherwise nothing.
  const ranges = f.open ? f.matches : (f.kept ? [f.kept] : []);
  const current = f.open ? f.index : 0;
  if (!input || !ranges.length) { overlay.innerHTML = ""; return; }
  const value = input.value;
  const cw = editorCharWidth();
  overlay.innerHTML = ranges.map((m, i) => {
    const a = offsetToVisual(value, m.start);
    const b = offsetToVisual(value, m.end);
    if (a.line !== b.line) return ""; // skip the rare match that spans lines
    const top = EDITOR_PAD_TOP + a.line * EDITOR_LINE_H;
    const left = EDITOR_PAD_LEFT + a.vcol * cw;
    const width = Math.max(2, (b.vcol - a.vcol) * cw);
    const cls = i === current ? "find-band current" : "find-band";
    return `<div class="${cls}" style="top:${top}px;left:${left}px;width:${width}px"></div>`;
  }).join("");
}

// Make sure the editor is writable before a replace (find can be opened over a
// read-only view) so the edit sticks and Cmd+S can save it.
function ensureEditableForReplace(input) {
  if (state.editorMode === "insert") return;
  input.readOnly = false;
  state.editorMode = "insert";
  updateEditorModeIndicator();
}

function replaceOne() {
  const f = state.find;
  const input = app.querySelector(".editor-input");
  const bar = findBar();
  if (!input || !bar || f.index < 0 || f.index >= f.matches.length) return;
  ensureEditableForReplace(input);
  const m = f.matches[f.index];
  const repl = bar.querySelector("#replace-input").value;
  input.value = input.value.slice(0, m.start) + repl + input.value.slice(m.end);
  const caret = m.start + repl.length;
  input.setSelectionRange(caret, caret);
  editorHistoryPush(false);
  repaintEditorAfterEdit();
  // The edit shifted offsets; recompute and jump to the next match at/after the
  // caret (which now sits just past the replacement).
  runFind(true);
  bar.querySelector("#replace-input").focus();
}

function replaceAll() {
  const f = state.find;
  const input = app.querySelector(".editor-input");
  const bar = findBar();
  if (!input || !bar) return;
  const query = bar.querySelector("#find-input").value;
  if (!query) return;
  ensureEditableForReplace(input);
  const repl = bar.querySelector("#replace-input").value;
  let re;
  try { re = findPattern(query); } catch (_) { return; }
  let count = 0;
  const next = input.value.replace(re, match => {
    if (match === "") return match; // never substitute a zero-width match
    count++;
    return repl;
  });
  if (!count) { updateFindCount(); return; }
  input.value = next;
  input.setSelectionRange(input.selectionStart, input.selectionStart);
  editorHistoryPush(false);
  repaintEditorAfterEdit();
  runFind(false);
  bar.querySelector("#find-count").textContent = `Replaced ${count}`;
}

function onFindKeyDown(e) {
  if (e.key === "Escape") { e.preventDefault(); closeFind(); return; }
  if (e.key === "Enter") {
    e.preventDefault();
    if (e.shiftKey) findPrev(); else findNext();
    return;
  }
  // Cmd+Alt+F toggles the replace row; Cmd+F re-selects the query (VSCode-style).
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
    e.preventDefault();
    if (e.altKey) toggleReplaceRow(); else e.target.select();
  }
}

function onReplaceKeyDown(e) {
  if (e.key === "Escape") { e.preventDefault(); closeFind(); return; }
  if (e.key === "Enter") {
    e.preventDefault();
    if (e.metaKey || e.altKey) replaceAll(); else replaceOne();
  }
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
  // Plain/Shift arrows adjust every cursor instead of dropping the extras.
  if (event.key.startsWith("Arrow") && state.multiRanges.length >= 2
      && !event.metaKey && !event.ctrlKey && !event.altKey) {
    event.preventDefault();
    moveMultiCursors(event);
    return;
  }
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
  clearFindKept(); // jumped to head/tail → drop the kept highlight
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
  return plainTextLines(contentLines(content)).join("\n");
}

function editorLineHtml(index, inner) {
  return `<span class="editor-line" data-line="${index}"><span class="line-number">${index + 1}</span>${inner}</span>`;
}

function contentLines(content) {
  return content.split("\n");
}

function plainTextLine(line, index) {
  return editorLineHtml(index, escapeHtml(line));
}

function plainTextLines(lineTexts) {
  return lineTexts.map((line, index) => plainTextLine(line, index));
}

function setLayerLines(layer, lines, lineTexts = null) {
  layer.innerHTML = lines.join("\n");
  layer._lineHtml = lines.slice();
  layer._lineText = lineTexts ? lineTexts.slice() : null;
}

function patchLayerLines(layer, lines, lineTexts = null) {
  const previous = layer._lineHtml;
  const nodes = layer.querySelectorAll(":scope > .editor-line");
  if (!previous || previous.length !== lines.length || nodes.length !== lines.length) {
    setLayerLines(layer, lines, lineTexts);
    return;
  }
  for (let i = 0; i < lines.length; i++) {
    if (previous[i] === lines[i]) continue;
    nodes[i].outerHTML = lines[i];
    previous[i] = lines[i];
  }
  if (lineTexts) layer._lineText = lineTexts.slice();
}

function patchPlainChangedLines(layer, content) {
  const lineTexts = contentLines(content);
  const previousText = layer._lineText;
  const previousHtml = layer._lineHtml;
  const nodes = layer.querySelectorAll(":scope > .editor-line");
  if (!previousText
      || !previousHtml
      || previousText.length !== lineTexts.length
      || previousHtml.length !== lineTexts.length
      || nodes.length !== lineTexts.length) {
    patchLayerLines(layer, plainTextLines(lineTexts), lineTexts);
    return;
  }
  for (let i = 0; i < lineTexts.length; i++) {
    if (previousText[i] === lineTexts[i]) continue;
    const html = plainTextLine(lineTexts[i], i);
    nodes[i].outerHTML = html;
    previousHtml[i] = html;
    previousText[i] = lineTexts[i];
  }
}

// Paint plain (un-highlighted) glyphs into the layer immediately — the textarea
// itself is transparent, so this is what the user actually sees while typing.
function paintEditorPlain(input, layer) {
  patchPlainChangedLines(layer, input.value);
  applyGitGutter(layer);
  if (state.find.open) renderFindMatches();
}

// Re-measure the textarea height only when the line count changes — `scrollHeight`
// forces a reflow, so skipping it on intra-line edits keeps typing snappy.
function reflowEditorHeight(input, layer, body) {
  const lines = input.value.split("\n").length;
  if (String(lines) === input.dataset.lines) return;
  // Resetting the height to `auto` collapses the surface and clamps its
  // scrollTop, which yanks the viewport on every line added/removed. Pin the
  // scroll position across the reflow so the view only moves when the caret
  // actually leaves the viewport (handled by scrollEditorToCursor below).
  const surface = editorScroller();
  const keepTop = surface ? surface.scrollTop : 0;
  const measure = body || layer.closest(".code-body");
  input.dataset.lines = String(lines);
  input.style.height = "auto";
  input.style.height = `${Math.max(input.scrollHeight, measure?.clientHeight || 0)}px`;
  if (surface) surface.scrollTop = keepTop;
  scrollEditorToCursor("auto");
}

function scheduleHighlight(input, layer, path) {
  clearTimeout(input.highlightTimer);
  input.highlightTimer = setTimeout(() => updateHighlightLayer(layer, path, input.value, input), 90);
}

async function updateHighlightLayer(layer, path, content, input) {
  // Very large files: skip the server round-trip + per-token markup and render
  // plain numbered text, so editing stays responsive (master virtualizes; this
  // is the lightweight equivalent for the textarea surface).
  if (content.length > 200000) {
    const lineTexts = contentLines(content);
    patchLayerLines(layer, plainTextLines(lineTexts), lineTexts);
    applyGitGutter(layer);
    if (state.find.open) renderFindMatches();
    return;
  }
  try {
    const { htmlLines, lineTexts } = await highlightedTextLines(path, content);
    // Discard a stale pass: if the buffer moved on (or an IME composition is in
    // flight) since the request went out, painting this would flash older text
    // into view — the root of the "Japanese text briefly disappears" bug.
    if (input && (input.value !== content || input.composing)) return;
    patchLayerLines(layer, htmlLines, lineTexts);
  } catch (_) {
    if (input && input.value !== content) return;
    const lineTexts = contentLines(content);
    patchLayerLines(layer, plainTextLines(lineTexts), lineTexts);
  }
  applyGitGutter(layer);
  if (state.find.open) renderFindMatches();
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
  return (await highlightedTextLines(path, content)).htmlLines.join("\n");
}

async function highlightedTextLines(path, content) {
  const data = await api("/api/highlight", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ path, content }),
  });
  const lineTexts = contentLines(content);
  const htmlLines = lineTexts.map((line, index) => {
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
    return editorLineHtml(index, out);
  });
  return { htmlLines, lineTexts };
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
  if (!["compare", "status", "history", "search"].includes(state.component)) return false;
  const count = app.querySelectorAll(".pane").length;
  return count > 0 && state.pane === count - 1;
}

async function ensureRefs() {
  if (state.refs.length) return;
  const data = await api("/api/branches");
  state.refs = data.branches || [];
  state.refInfo = data.info || {};
  state.compareBase ||= data.default || data.current || state.refs[0] || "HEAD";
  state.compareTarget ||= data.current || state.refs[1] || "HEAD";
}

// Compact "time ago" for branch tips: seconds is a unix timestamp (author
// time). Returns "" for a missing/zero stamp so the picker can omit it.
function relativeTime(seconds) {
  const value = Number(seconds);
  if (!value) return "";
  const delta = Math.max(0, Math.floor(Date.now() / 1000 - value));
  const units = [
    [31536000, "y"], [2592000, "mo"], [604800, "w"],
    [86400, "d"], [3600, "h"], [60, "m"],
  ];
  for (const [size, label] of units) {
    if (delta >= size) return `${Math.floor(delta / size)}${label} ago`;
  }
  return "just now";
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
  const items = state.refs.map(ref => {
    const tip = state.refInfo[ref];
    const meta = tip && (tip.hash || tip.message || tip.time)
      ? `${escapeHtml(tip.hash || "")}${tip.message ? ` · ${escapeHtml(String(tip.message).split("\n")[0])}` : ""}${tip.time ? ` · ${escapeHtml(relativeTime(tip.time))}` : ""}`
      : "";
    return {
      label: ref, search: ref, cls: "ref-row",
      run: () => applyRef(which, ref),
      html: `<div class="stack"><div class="primary">${escapeHtml(ref)}${ref === current ? ` <span class="hint">current</span>` : ""}</div>`
        + (meta ? `<span class="secondary">${meta}</span>` : "") + `</div>`,
    };
  });
  showPopup("ref", which === "base" ? "Select base ref" : "Select compare ref",
    items, "Filter branches, tags, refs…");
}

async function applyRef(which, ref) {
  ref = String(ref || "").trim();
  if (!ref) return;
  // Picking a ref equal to the other one would diff a ref against itself; swap
  // instead so the old value of the picked side moves to the other side.
  if (which === "base") {
    if (ref === state.compareTarget) state.compareTarget = state.compareBase;
    state.compareBase = ref;
  } else {
    if (ref === state.compareBase) state.compareBase = state.compareTarget;
    state.compareTarget = ref;
  }
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
  installPaneResizers(app.querySelector(".panes"), source.kind);
  source.bind?.();
  bindListClicks();
  await loadCurrentDiffPreview();
}

// Default column proportions (arbitrary `fr` units; only the ratio matters) that
// mirror the CSS fallbacks in editor.css. Used on first run and on dbl-click
// reset of a divider.
const PANE_DEFAULTS = {
  history: [25, 25, 50],
  compare: [34, 66],
  status: [34, 66],
  search: [34, 66],
};

function loadPaneSizes(kind, count) {
  if (state.paneSizes[kind]?.length === count) return state.paneSizes[kind].slice();
  let sizes;
  try {
    const raw = localStorage.getItem(`gargo:panes:${kind}`);
    const parsed = raw && JSON.parse(raw);
    if (Array.isArray(parsed) && parsed.length === count
        && parsed.every(n => typeof n === "number" && n > 0)) {
      sizes = parsed;
    }
  } catch (_) { /* localStorage unavailable or malformed — fall through */ }
  sizes = sizes || (PANE_DEFAULTS[kind] || Array(count).fill(1)).slice();
  state.paneSizes[kind] = sizes.slice();
  return sizes.slice();
}

function savePaneSizes(kind, sizes) {
  state.paneSizes[kind] = sizes.slice();
  try { localStorage.setItem(`gargo:panes:${kind}`, JSON.stringify(sizes)); } catch (_) {}
}

function applyPaneTemplate(panesEl, sizes) {
  panesEl.style.gridTemplateColumns =
    sizes.map(fr => `minmax(120px, ${fr.toFixed(4)}fr)`).join(" 6px ");
}

// Insert draggable dividers between the panes of a diff view. The drag uses the
// Pointer Events API with setPointerCapture, which behaves identically across
// Chromium, WebKit (Safari) and Gecko (Firefox) — no per-browser branching. The
// chosen widths persist per layout in localStorage; double-clicking a divider
// resets that boundary to the default split.
function installPaneResizers(panesEl, kind) {
  if (!panesEl) return;
  const panes = [...panesEl.querySelectorAll(":scope > .pane")];
  if (panes.length < 2) return;
  // Below this width the CSS switches to a stacked/scrolling layout; leave it.
  if (window.matchMedia("(max-width: 800px)").matches) return;

  panesEl.classList.add("resizable");
  const sizes = loadPaneSizes(kind, panes.length);
  applyPaneTemplate(panesEl, sizes);

  for (let h = 0; h < panes.length - 1; h++) {
    const handle = document.createElement("div");
    handle.className = "pane-resizer";
    handle.setAttribute("role", "separator");
    handle.setAttribute("aria-orientation", "vertical");
    handle.title = "Drag to resize · double-click to reset";
    panes[h].after(handle);
    attachResizer(handle, panesEl, panes, sizes, kind, h);
  }
}

function attachResizer(handle, panesEl, panes, sizes, kind, h) {
  const MIN = 140; // px: keep both neighbouring panes usable
  let startX = 0, leftPx = 0, sumPx = 0, sumFr = 0, dragging = false;

  const onMove = (e) => {
    if (!dragging) return;
    const newLeft = Math.max(MIN, Math.min(sumPx - MIN, leftPx + (e.clientX - startX)));
    sizes[h] = sumFr * (newLeft / sumPx);
    sizes[h + 1] = sumFr - sizes[h];
    applyPaneTemplate(panesEl, sizes);
  };
  const onUp = (e) => {
    if (!dragging) return;
    dragging = false;
    handle.classList.remove("dragging");
    document.body.classList.remove("col-resizing");
    try { handle.releasePointerCapture(e.pointerId); } catch (_) {}
    savePaneSizes(kind, sizes);
  };

  handle.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    const lr = panes[h].getBoundingClientRect();
    const rr = panes[h + 1].getBoundingClientRect();
    sumPx = lr.width + rr.width;
    if (sumPx < MIN * 2) return; // too narrow to split meaningfully
    e.preventDefault();
    leftPx = lr.width;
    sumFr = sizes[h] + sizes[h + 1];
    startX = e.clientX;
    dragging = true;
    handle.classList.add("dragging");
    document.body.classList.add("col-resizing");
    try { handle.setPointerCapture(e.pointerId); } catch (_) {}
  });
  handle.addEventListener("pointermove", onMove);
  handle.addEventListener("pointerup", onUp);
  handle.addEventListener("pointercancel", onUp);
  handle.addEventListener("dblclick", () => {
    const def = PANE_DEFAULTS[kind];
    if (!def) return;
    const sumFr2 = sizes[h] + sizes[h + 1];
    sizes[h] = sumFr2 * def[h] / (def[h] + def[h + 1]);
    sizes[h + 1] = sumFr2 - sizes[h];
    applyPaneTemplate(panesEl, sizes);
    savePaneSizes(kind, sizes);
  });
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

// Flatten the status payload into one selectable list, staged first so the
// "Changes to be committed" section sits at the top (the grouped renderer
// relies on this section order to map each row back to its global index).
function statusFilesFrom(data) {
  return ["staged", "unstaged", "untracked"].flatMap(section =>
    (data[section] || []).map(file => ({ ...file, section }))
  );
}

function statusSignature(files, branch) {
  return `${branch || ""}\n` + files.map(file =>
    `${file.section}:${file.path}:${file.status}:${file.additions || 0}:${file.deletions || 0}:${file.viewed ? 1 : 0}`
  ).join("|");
}

async function renderStatus() {
  const data = await api("/api/status");
  state.statusFiles = statusFilesFrom(data);
  state.statusBranch = data.branch || "";
  state.statusFile = Math.min(state.statusFile, Math.max(0, state.statusFiles.length - 1));
  state.statusSignature = statusSignature(state.statusFiles, state.statusBranch);
  await renderStatusView();
}

// Render the status list as git-status-style sections (branch header + staged /
// unstaged / untracked groups). Each file row keeps its global index into
// `state.statusFiles` so j/k navigation, clicks and the preview stay in sync.
function statusFileListHtml() {
  const branch = state.statusBranch
    ? `On branch ${escapeHtml(state.statusBranch)}`
    : "Detached HEAD";
  const SECTIONS = [
    { key: "staged", title: "Changes to be committed:" },
    { key: "unstaged", title: "Changes not staged for commit:" },
    { key: "untracked", title: "Untracked files:" },
  ];
  const parts = [`<div class="status-branch">${branch}</div>`];
  let index = 0;
  for (const section of SECTIONS) {
    const files = state.statusFiles.filter(file => file.section === section.key);
    parts.push(`<div class="status-section-head status-section-${section.key}">${section.title}</div>`);
    if (!files.length) {
      parts.push(`<div class="status-section-empty">(no files)</div>`);
      continue;
    }
    parts.push(`<ol class="list">` + files.map(file => {
      const i = index++;
      return `<li data-index="${i}" class="${i === state.statusFile ? "selected" : ""}">`
        + `<span class="viewed-box ${file.viewed ? "checked" : ""}" title="${file.viewed ? "Viewed" : "Not viewed"}">${file.viewed ? "[x]" : "[ ]"}</span>`
        + `<span class="status-badge status-${statusName(file.status)}">${statusLetter(file.status)}</span>`
        + `<span class="primary">${escapeHtml(file.path)}</span>`
        + `<span class="secondary">${stats(file)}</span></li>`;
    }).join("") + `</ol>`);
  }
  return parts.join("");
}

async function renderStatusView() {
  await renderDiffView({
    kind: "status",
    title: "Status",
    hint: `<span>Worktree vs HEAD · live · <span class="key">j/k</span> files · <span class="key">u</span> stage · <span class="key">C</span> commit · <span class="key">v</span> viewed · <span class="key">o</span> edit · <span class="key">O</span> menu · <span class="key">Ctrl-d/u</span> preview</span>`,
    panes: [
      { title: "Changed files", name: "changed files", body: statusFileListHtml() },
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
  const branch = data.branch || "";
  const signature = statusSignature(files, branch);
  if (signature === state.statusSignature) return;
  const focusLevel = state.focusLevel, pane = state.pane;
  state.statusFiles = files;
  state.statusBranch = branch;
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
  if (state.component === "search") { await loadSearchPreview(surface); return; }
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
  // Guard against out-of-order responses: while holding j/k, several preview
  // fetches are in flight at once, and a slow earlier one must not clobber the
  // diff for the row the user has since landed on.
  const token = ++state.previewToken;
  try {
    const data = await api(url);
    if (token !== state.previewToken) return;
    await renderCodeSurface(surface, { path: file.path, diffHtml: data.html || "", editable: false });
  } catch (error) {
    if (token !== state.previewToken) return;
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
    // j/k between files: the file set is already in state (and a 1.5s poller
    // keeps it fresh), so don't re-fetch /api/status or rebuild the whole view.
    // Just move the selected row and load the new file's preview — re-rendering
    // both panes on every keystroke is what made navigation feel sluggish.
    state.statusFile = Math.max(0, Math.min(index, state.statusFiles.length - 1));
    const pane0 = app.querySelector('.pane[data-pane="0"]');
    pane0?.querySelector("li.selected")?.classList.remove("selected");
    pane0?.querySelector(`li[data-index="${state.statusFile}"]`)?.classList.add("selected");
    await loadCurrentDiffPreview();
  } else if (state.component === "search" && state.pane === 0) {
    await selectSearchRow(index);
  }
}

function currentSelection() {
  if (state.component === "history" && state.pane === 0) return [state.historyCommit, state.commits.length];
  if (state.component === "history" && state.pane === 1) return [state.historyFile, state.historyData?.files?.length || 0];
  if (state.component === "compare" && state.pane === 0) return [state.compareFile, state.compareFiles.length];
  if (state.component === "status" && state.pane === 0) return [state.statusFile, state.statusFiles.length];
  if (state.component === "search" && state.pane === 0) return [state.searchSelected, state.searchRows.length];
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

// Stage (git add) or unstage (git reset) the selected status file, then reload
// the list so it hops between the Changes / Staged sections. The selection
// follows the same path across the move so `j u j u` keeps flowing.
async function toggleStatusStage() {
  if (state.component !== "status" || state.pane !== 0) return;
  const file = state.statusFiles[state.statusFile];
  if (!file) return;
  const staged = file.section === "staged";
  const endpoint = staged ? "/api/status/unstage" : "/api/status/stage";
  try {
    await api(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ path: file.path }),
    });
  } catch (error) {
    notify(`${staged ? "Unstage" : "Stage"} failed: ${error.message}`);
    return;
  }
  const keepPath = file.path;
  await renderStatus();
  const idx = state.statusFiles.findIndex(other => other.path === keepPath);
  if (idx >= 0 && idx !== state.statusFile) {
    state.statusFile = idx;
    await renderStatusView();
  }
  setFocus("pane", 0);
}

// Commit dialog: a staged-file summary plus a message form. Opened with `C`
// from the Status view. Backed by /api/status/commit-prepare (summary + the
// HEAD message for amend) and /api/status/commit (the actual commit).
async function openCommitDialog() {
  let data;
  try {
    data = await api("/api/status/commit-prepare");
  } catch (error) {
    notify(`Commit unavailable: ${error.message}`);
    return;
  }
  const staged = data.staged || [];
  state.commit = {
    open: true,
    lastMessage: data.last_message || "",
    hasHead: Boolean(data.has_head),
    stagedCount: staged.length,
  };
  commitBranch.textContent = data.branch ? `on ${data.branch}` : "";
  commitSummary.innerHTML = staged.length
    ? `<div class="commit-summary-head">${staged.length} staged file${staged.length === 1 ? "" : "s"}</div>`
      + `<ol class="list commit-files">` + staged.map(file =>
        `<li><span class="status-badge status-${statusName(file.status)}">${statusLetter(file.status)}</span>`
        + `<span class="primary">${escapeHtml(file.path)}</span>`
        + `<span class="secondary">${stats(file)}</span></li>`).join("")
      + `</ol>`
    : `<div class="empty">No staged changes — stage files with <span class="key">u</span> first.</div>`;
  commitMessage.value = "";
  commitAmendRow.hidden = !state.commit.hasHead;
  commitAmend.checked = false;
  updateCommitSubmitState();
  commitBackdrop.hidden = false;
  commitMessage.focus();
}

// The commit button is enabled when there's a message and either staged
// changes or an amend in progress (amend can rewrite the message alone).
function updateCommitSubmitState() {
  if (!state.commit) return;
  const hasMessage = commitMessage.value.trim().length > 0;
  const hasWork = state.commit.stagedCount > 0 || commitAmend.checked;
  commitSubmit.disabled = !(hasMessage && hasWork);
}

function closeCommitDialog() {
  state.commit = null;
  commitBackdrop.hidden = true;
  setFocus(state.focusLevel, state.pane);
}

async function submitCommit() {
  if (!state.commit) return;
  const message = commitMessage.value.trim();
  const amend = commitAmend.checked;
  if (!message) {
    notify("Commit message must not be empty");
    commitMessage.focus();
    return;
  }
  if (state.commit.stagedCount === 0 && !amend) {
    notify("No staged changes to commit");
    return;
  }
  commitSubmit.disabled = true;
  try {
    await api("/api/status/commit", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message, amend }),
    });
  } catch (error) {
    notify(`Commit failed: ${error.message}`);
    updateCommitSubmitState();
    return;
  }
  closeCommitDialog();
  notify(amend ? "Amended commit" : "Committed");
  // The history log is now stale; drop it so the next visit refetches.
  state.commits = [];
  state.historyData = null;
  await renderStatus();
  setFocus("pane", 0);
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
  if (state.component === "search") return searchRowTarget(state.searchRows[state.searchSelected])?.path || "";
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
    ? "j/k move · h/l fold · Enter open · a add · r rename · d delete · c/y copy path · . hidden · right-click menu · / filter · Esc close"
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
  // A prompt/confirm overlay dismissed via backdrop-click resolves to "cancelled"
  // so the awaiting action doesn't hang.
  if (state.promptResolve) {
    const resolve = state.promptResolve;
    state.promptResolve = null;
    resolve(null);
  }
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

// Move the selection highlight without re-rendering the list. A full
// renderPopupList() rebuilds every <li> and re-binds its click handler, which is
// O(n) per keystroke — janky for large result sets (e.g. global search with
// hundreds of hits). Arrow navigation only changes which row is selected, so
// shift the `selected` class between two rows and scroll the new one into view.
function movePopupSelection(delta) {
  const max = state.popupFiltered.length - 1;
  if (max < 0) return;
  const next = Math.max(0, Math.min(state.popupIndex + delta, max));
  if (next !== state.popupIndex) {
    popupList.children[state.popupIndex]?.classList.remove("selected");
    state.popupIndex = next;
    popupList.children[next]?.classList.add("selected");
  }
  popupList.children[state.popupIndex]?.scrollIntoView({ block: "nearest" });
  if (state.popup === "tree") updateTreePreview();
}

function filterPopup() {
  const raw = popupInput.value;
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
  // Empty-query order: changed files first, then by recency — the more recent of
  // the file's mtime and the last time it was opened in gargo (CLI or web editor).
  const recency = entry => Math.max(Number(entry.mtime || 0), Number(entry.opened || 0));
  const entries = [...state.fileEntries].sort((a, b) =>
    Number(b.changed) - Number(a.changed)
    || recency(b) - recency(a)
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
    if (!state.showHidden && parts.some(part => part.startsWith("."))) continue;
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

// Lightweight text-prompt overlay (rename / new-entry path). Reuses the popup
// backdrop + filter input rather than a bespoke modal: shows the title, prefills
// and focuses the input, and resolves on Enter (trimmed value, or null if empty)
// / null on Esc/backdrop-click. The resolver lives on state.promptResolve so the
// keydown handler and closePopup() can settle it.
function promptText(title, initial = "", hint = "Enter confirm · Esc cancel") {
  return new Promise(resolve => {
    state.promptResolve = resolve;
    state.popup = "prompt";
    popupTitle.textContent = title;
    popup.classList.remove("tree-popup");
    popupList.innerHTML = "";
    popupPreview.hidden = true;
    popupPreview.innerHTML = "";
    popupInput.hidden = false;
    popupInput.placeholder = "";
    popupInput.value = initial;
    popupHint.textContent = hint;
    popupBackdrop.hidden = false;
    popupInput.focus();
    popupInput.setSelectionRange(initial.length, initial.length);
  });
}

// Yes/no confirmation (delete). Same overlay as promptText but no text entry;
// resolves true on Enter, false on Esc/backdrop-click.
function confirmAction(title) {
  return new Promise(resolve => {
    state.promptResolve = value => resolve(value !== null);
    state.popup = "confirm";
    popupTitle.textContent = title;
    popup.classList.remove("tree-popup");
    popupList.innerHTML = "";
    popupPreview.hidden = true;
    popupPreview.innerHTML = "";
    popupInput.hidden = true;
    popupHint.textContent = "Enter confirm · Esc cancel";
    popupBackdrop.hidden = false;
    popup.focus();
  });
}

// Settle the active prompt/confirm overlay with `value` (null = cancelled) and
// tear the overlay down. Called from the keydown handlers.
function resolvePrompt(value) {
  const resolve = state.promptResolve;
  state.promptResolve = null;
  closePopup();
  if (resolve) resolve(value);
}

// POST a JSON body to one of the /api/fs/* mutation endpoints, then refresh the
// file listing and rebuild the tree so the change shows immediately. `ensureFiles`
// caches on `state.files`, so clear it to force a refetch.
async function fsMutate(endpoint, body) {
  await api(endpoint, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  state.files = [];
  await ensureFiles();
  state.treeRoot = buildTree(state.fileEntries);
  if (state.popup === "tree") filterPopup();
}

// The tree node the selection currently points at (null when the list is empty).
function selectedTreeNode() {
  return state.popupFiltered[state.popupIndex]?.node || null;
}

// Repo-relative directory a new entry should land in, derived from the selection:
// inside the selected dir, beside the selected file, else the repo root.
function treeBaseDir(node) {
  if (!node) return "";
  if (node.type === "dir") return `${node.path}/`;
  return node.path.includes("/") ? `${node.path.slice(0, node.path.lastIndexOf("/") + 1)}` : "";
}

// `a` — create a file or directory. A trailing `/` means directory; otherwise a
// file (missing parent dirs are created server-side). `some/path/memo.md` works.
async function treeAddEntry() {
  const base = treeBaseDir(selectedTreeNode());
  const input = await promptText("New file or dir (end with / for a directory)", base);
  if (!input) return;
  const kind = input.endsWith("/") ? "dir" : "file";
  const path = input.replace(/\/+$/, "");
  if (!path) return;
  try {
    await fsMutate("/api/fs/create", { path, kind });
    // Reveal the new entry: expand its ancestors, and open it if it's a file.
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) state.treeExpanded.add(parts.slice(0, i).join("/"));
    if (state.popup === "tree") filterPopup();
    if (kind === "file") await openFile(path);
    notify(`Created ${path}`);
  } catch (error) {
    notify(`Create failed: ${error.message}`);
  }
}

// `r` — rename/move the selected entry; prompt is prefilled with its current path.
async function treeRenameEntry(node) {
  if (!node) { notify("No selection"); return; }
  const to = await promptText("Rename", node.path);
  if (!to || to === node.path) return;
  try {
    const wasOpen = state.currentFile === node.path;
    await fsMutate("/api/fs/rename", { from: node.path, to });
    if (wasOpen) await openFile(to);
    notify(`Renamed to ${to}`);
  } catch (error) {
    notify(`Rename failed: ${error.message}`);
  }
}

// `d` — delete the selected entry after a confirmation.
async function treeDeleteEntry(node) {
  if (!node) { notify("No selection"); return; }
  if (!await confirmAction(`Delete ${node.path}?`)) return;
  try {
    await fsMutate("/api/fs/delete", { path: node.path });
    notify(`Deleted ${node.path}`);
  } catch (error) {
    notify(`Delete failed: ${error.message}`);
  }
}

// `c` / `y` — copy the selected entry's absolute / repo-relative path.
function treeCopyAbsPath(node) {
  if (!node) { notify("No selection"); return; }
  const root = state.repoInfo?.root ? state.repoInfo.root.replace(/\/$/, "") : "";
  copyText(root ? `${root}/${node.path}` : node.path);
}
function treeCopyRelPath(node) {
  if (!node) { notify("No selection"); return; }
  copyText(node.path);
}

// `.` — toggle dot-prefixed files/dirs in the tree (gitignored paths stay hidden;
// those aren't in gargo's file listing). Rebuilds the tree from the same entries.
function toggleTreeHidden() {
  state.showHidden = !state.showHidden;
  state.treeRoot = buildTree(state.fileEntries);
  filterPopup();
  notify(state.showHidden ? "Showing hidden files" : "Hiding hidden files");
}

// Floating right-click context menu over the tree: the same actions as the direct
// keys, anchored at the cursor. Created once, reused, clamped to the viewport.
let treeContextMenuEl = null;
function showTreeContextMenu(x, y, node) {
  const root = state.repoInfo?.root ? state.repoInfo.root.replace(/\/$/, "") : "";
  const actions = [{ key: "a", label: "New file / dir…", run: () => treeAddEntry() }];
  if (node) {
    actions.push(
      { key: "r", label: "Rename…", run: () => treeRenameEntry(node) },
      { key: "c", label: "Copy absolute path", run: () => copyText(root ? `${root}/${node.path}` : node.path) },
      { key: "y", label: "Copy relative path", run: () => copyText(node.path) },
      { key: "d", label: "Delete…", run: () => treeDeleteEntry(node) },
    );
  }
  if (!treeContextMenuEl) {
    treeContextMenuEl = document.createElement("div");
    treeContextMenuEl.id = "tree-context-menu";
    document.body.appendChild(treeContextMenuEl);
  }
  const menu = treeContextMenuEl;
  menu.innerHTML = actions.map((action, index) =>
    `<div class="ctx-item" data-index="${index}">${escapeHtml(action.label)}<span class="hint">${escapeHtml(action.key)}</span></div>`
  ).join("");
  menu.hidden = false;
  menu.querySelectorAll("[data-index]").forEach(el => el.addEventListener("click", () => {
    hideTreeContextMenu();
    actions[Number(el.dataset.index)].run();
  }));
  // Position, then clamp so the menu stays on-screen.
  menu.style.left = `${x}px`;
  menu.style.top = `${y}px`;
  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth) menu.style.left = `${Math.max(0, window.innerWidth - rect.width - 4)}px`;
  if (rect.bottom > window.innerHeight) menu.style.top = `${Math.max(0, window.innerHeight - rect.height - 4)}px`;
}

function hideTreeContextMenu() {
  if (treeContextMenuEl) treeContextMenuEl.hidden = true;
}

// Project-wide text search (⌘⇧F) as a dedicated tab, styled like Compare: the
// left pane is a typed query + per-file list of matched lines, the right pane
// previews the selected match's file with the matched line highlighted. Backed by
// /api/search (case-insensitive literal substring, 3-character minimum).
async function renderSearch() {
  await renderDiffView({
    kind: "search",
    title: "Search",
    hint: `<span>Project-wide · <span class="key">j/k</span> rows · <span class="key">h/l</span> collapse/expand · <span class="key">J/K</span> preview · <span class="key">o</span> open · <span class="key">e</span> new tab · <span class="key">O</span> menu</span>`,
    panes: [
      {
        title: "Matches", name: "matches",
        body: `<div class="search-form">
          <input id="search-input" type="text" placeholder="Search project…" autocomplete="off"
            autocorrect="off" autocapitalize="off" spellcheck="false" value="${escapeHtml(state.searchQuery)}">
        </div>
        <div id="search-results">${searchResultsHtml()}</div>`,
      },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
    bind: () => {
      const input = document.getElementById("search-input");
      input.addEventListener("input", scheduleSearch);
      input.addEventListener("keydown", onSearchInputKey);
    },
  });
}

// Focus (and select-to-end) the query box: the tab opens ready to type, and a
// second ⌘⇧F while already on the tab just refocuses it.
function focusSearchInput() {
  const input = document.getElementById("search-input");
  if (!input) return;
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
}

// While the query box owns focus: arrows move the selection (and preview), Enter
// hands focus to the results list so the vim keys (j/k/J/K/o/e/O/h/l) take over,
// and Esc does the same. `stopPropagation` keeps the global handler out.
function onSearchInputKey(event) {
  if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) {
    event.preventDefault(); event.stopPropagation();
    moveSearchSelection(1);
  } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
    event.preventDefault(); event.stopPropagation();
    moveSearchSelection(-1);
  } else if (event.key === "Enter" || event.key === "Escape") {
    event.preventDefault(); event.stopPropagation();
    event.target.blur();
    setFocus("pane", 0);
    app.querySelector(".list li.selected")?.scrollIntoView({ block: "nearest" });
  }
}

function scheduleSearch() {
  clearTimeout(scheduleSearch.timer);
  scheduleSearch.timer = setTimeout(runGlobalSearch, 150);
}

async function runGlobalSearch() {
  const input = document.getElementById("search-input");
  if (!input) return;
  const query = input.value.trim();
  const token = ++state.searchToken;
  state.searchQuery = query;
  const results = document.getElementById("search-results");
  if (query.length < 3) {
    state.searchHits = [];
    state.searchRows = [];
    state.searchCollapsed = new Set();
    state.searchSelected = 0;
    if (results) results.innerHTML = `<div class="empty">${query ? "Type at least 3 characters…" : "Search across the project"}</div>`;
    clearSearchPreview();
    return;
  }
  if (results) results.innerHTML = `<div class="loading">Searching…</div>`;
  try {
    const data = await api(`/api/search?${new URLSearchParams({ q: query, max: "500" })}`);
    if (token !== state.searchToken || state.component !== "search") return;
    state.searchHits = data.hits || [];
    state.searchCollapsed = new Set();
    state.searchRows = buildSearchRows();
    state.searchSelected = 0;
    state.searchTruncated = Boolean(data.truncated);
    renderSearchResults();
    if (state.searchRows.length) await loadCurrentDiffPreview();
    else clearSearchPreview();
  } catch (error) {
    if (token !== state.searchToken || state.component !== "search") return;
    if (results) results.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
  }
}

// Flatten the path-sorted hits into the visible rows: one file-header row per
// path followed by its match rows, with a collapsed file's matches omitted so
// keyboard nav skips them. `state.searchRows` is the single source of truth for
// selection (its index is the row's `data-index`).
function buildSearchRows() {
  const rows = [];
  let curPath = null;
  state.searchHits.forEach((hit, hitIndex) => {
    if (hit.path !== curPath) {
      curPath = hit.path;
      rows.push({ kind: "file", path: hit.path });
    }
    if (!state.searchCollapsed.has(hit.path)) rows.push({ kind: "hit", path: hit.path, hitIndex });
  });
  return rows;
}

// The hit a row resolves to for preview / open: the hit itself, or a file
// header's first match.
function searchRowTarget(row) {
  if (!row) return null;
  if (row.kind === "hit") return state.searchHits[row.hitIndex];
  return state.searchHits.find(hit => hit.path === row.path) || null;
}

function searchResultsHtml() {
  if (!state.searchHits.length) {
    return `<div class="empty">${state.searchQuery ? "No matches" : "Search across the project"}</div>`;
  }
  const counts = new Map();
  for (const hit of state.searchHits) counts.set(hit.path, (counts.get(hit.path) || 0) + 1);
  const rows = state.searchRows.map((row, index) => {
    const sel = index === state.searchSelected ? " selected" : "";
    if (row.kind === "file") {
      const chevron = state.searchCollapsed.has(row.path) ? "▸" : "▾";
      return `<li data-index="${index}" class="gfile${sel}"><span class="gchevron">${chevron}</span>`
        + `<span class="gfile-path">${escapeHtml(row.path)}</span>`
        + `<span class="gcount">${counts.get(row.path)}</span></li>`;
    }
    const hit = state.searchHits[row.hitIndex];
    return `<li data-index="${index}" class="ghit${sel}"><span class="gline">${hit.line + 1}</span>`
      + `<span class="gtext">${highlightExcerpt(hit.excerpt, hit.col, state.searchQuery.length)}</span></li>`;
  });
  return `<ol class="list">${rows.join("")}</ol>`;
}

function renderSearchResults() {
  const results = document.getElementById("search-results");
  if (!results) return;
  results.innerHTML = searchResultsHtml();
  bindListClicks();
}

function clearSearchPreview() {
  const surface = document.getElementById("diff-surface");
  if (surface) surface.innerHTML = `<div class="empty">No file selected</div>`;
}

// Move the selected row (wrapping), update the highlighted row and the preview.
async function moveSearchSelection(delta) {
  const length = state.searchRows.length;
  if (!length) return;
  await selectSearchRow((state.searchSelected + delta + length) % length);
  app.querySelector(".list li.selected")?.scrollIntoView({ block: "nearest" });
}

// Re-select without rebuilding the list (it can be hundreds of rows): shift the
// `selected` class and reload the preview for the new row.
async function selectSearchRow(index) {
  state.searchSelected = Math.max(0, Math.min(index, state.searchRows.length - 1));
  const pane0 = app.querySelector('.pane[data-pane="0"]');
  pane0?.querySelector("li.selected")?.classList.remove("selected");
  pane0?.querySelector(`li[data-index="${state.searchSelected}"]`)?.classList.add("selected");
  await loadCurrentDiffPreview();
}

// Collapse / expand the selected file group (h / ← collapse, l / → expand). On a
// hit, collapsing folds its parent file and lands the selection on the header.
async function setSearchCollapsed(path, collapsed) {
  if (collapsed) state.searchCollapsed.add(path);
  else state.searchCollapsed.delete(path);
  state.searchRows = buildSearchRows();
  const idx = state.searchRows.findIndex(row => row.kind === "file" && row.path === path);
  state.searchSelected = idx < 0 ? 0 : idx;
  renderSearchResults();
  app.querySelector(".list li.selected")?.scrollIntoView({ block: "nearest" });
  await loadCurrentDiffPreview();
}

async function searchCollapse() {
  const row = state.searchRows[state.searchSelected];
  if (row && !state.searchCollapsed.has(row.path)) await setSearchCollapsed(row.path, true);
}

async function searchExpand() {
  const row = state.searchRows[state.searchSelected];
  if (row && row.kind === "file" && state.searchCollapsed.has(row.path)) await setSearchCollapsed(row.path, false);
}

function openSearchHit() {
  const hit = searchRowTarget(state.searchRows[state.searchSelected]);
  if (hit) openFile(hit.path, hit.line, hit.col);
}

// Preview the selected row's file (full content, syntax-highlighted) with the
// matched line highlighted and centred. Shares state.previewToken with the diff
// previews so a slow earlier fetch can't clobber the row the user landed on.
async function loadSearchPreview(surface) {
  const hit = searchRowTarget(state.searchRows[state.searchSelected]);
  if (!hit) { surface.innerHTML = `<div class="empty">No file selected</div>`; return; }
  const token = ++state.previewToken;
  try {
    const data = await api(`/api/file?path=${encodeURIComponent(hit.path)}`);
    if (token !== state.previewToken || state.component !== "search") return;
    const body = await highlightedTextHtml(hit.path, data.content || "");
    if (token !== state.previewToken || state.component !== "search") return;
    // A normal-flow <pre> (not the absolute .highlight-layer) so the .code-surface
    // actually overflows and J/K / Ctrl-f-b can scroll it.
    surface.innerHTML = `<div class="code-surface">
      <div class="code-toolbar"><span class="path">${escapeHtml(hit.path)}</span>
        <span class="grow"></span><span>read only</span></div>
      <div class="code-body"><pre class="search-preview">${body}</pre></div></div>`;
    const lineEl = surface.querySelector(`.editor-line[data-line="${hit.line}"]`);
    if (lineEl) {
      lineEl.classList.add("search-match-line");
      lineEl.scrollIntoView({ block: "center" });
    }
  } catch (error) {
    if (token !== state.previewToken || state.component !== "search") return;
    surface.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
  }
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
  if (state.popup === "prompt") {
    if (event.key === "Enter") { event.preventDefault(); resolvePrompt(popupInput.value.trim() || null); }
    else if (event.key === "Escape") { event.preventDefault(); resolvePrompt(null); }
    return;
  }
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
    movePopupSelection(1);
  } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
    event.preventDefault();
    movePopupSelection(-1);
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
  if (state.popup === "confirm") {
    if (event.key === "Enter") { event.preventDefault(); resolvePrompt(true); }
    else if (event.key === "Escape") { event.preventDefault(); resolvePrompt(null); }
    return;
  }
  if (state.popup !== "tree" || !popupInput.hidden) return;
  if (event.key === "Escape") {
    event.preventDefault();
    closePopup();
  } else if (event.key === "a") {
    event.preventDefault();
    treeAddEntry();
  } else if (event.key === "r") {
    event.preventDefault();
    treeRenameEntry(selectedTreeNode());
  } else if (event.key === "d") {
    event.preventDefault();
    treeDeleteEntry(selectedTreeNode());
  } else if (event.key === "c") {
    event.preventDefault();
    treeCopyAbsPath(selectedTreeNode());
  } else if (event.key === "y") {
    event.preventDefault();
    treeCopyRelPath(selectedTreeNode());
  } else if (event.key === ".") {
    event.preventDefault();
    toggleTreeHidden();
  } else if (event.key === "/" || event.key === "f") {
    event.preventDefault();
    popupInput.hidden = false;
    popupInput.focus();
  } else if (event.key === "j" || event.key === "ArrowDown") {
    event.preventDefault();
    movePopupSelection(1);
  } else if (event.key === "k" || event.key === "ArrowUp") {
    event.preventDefault();
    movePopupSelection(-1);
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

// Right-click a tree row → floating context menu at the cursor. Selecting the row
// first keeps the keyboard actions (which read the selection) consistent with the
// menu. Right-clicking empty space still offers "New file / dir".
popupList.addEventListener("contextmenu", event => {
  if (state.popup !== "tree") return;
  event.preventDefault();
  const row = event.target.closest("[data-index]");
  if (row) {
    const index = Number(row.dataset.index);
    if (index !== state.popupIndex) { state.popupIndex = index; renderPopupList(); updateTreePreview(); }
  }
  showTreeContextMenu(event.clientX, event.clientY, selectedTreeNode());
});

// Dismiss the context menu on any outside interaction.
document.addEventListener("mousedown", event => {
  if (treeContextMenuEl && !treeContextMenuEl.hidden && !event.target.closest("#tree-context-menu")) {
    hideTreeContextMenu();
  }
}, true);
document.addEventListener("scroll", () => hideTreeContextMenu(), true);
window.addEventListener("keydown", event => { if (event.key === "Escape") hideTreeContextMenu(); }, true);

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

// Header `?` button mirrors the `?` key: toggle the keybindings popup.
helpToggle.addEventListener("click", () => toggleHelp());

// Update badge: surface the upgrade command rather than navigating.
versionUpdate.addEventListener("click", event => {
  event.preventDefault();
  notify(versionUpdate.title || "Update available — run `gargo --update`");
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
  if (event.target.closest(".editor-input, #find, #popup-backdrop, #help-backdrop")) return;
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

  // The find bar owns every key while one of its inputs is focused (Esc / Enter /
  // Cmd+F / Cmd+Alt+F are handled there); don't let the global shortcuts fire.
  if (event.target.closest?.("#find")) return;

  // Commit dialog: Cmd/Ctrl+Enter commits, Esc cancels; every other key falls
  // through to the textarea so the message types normally.
  if (state.commit && state.commit.open) {
    if (event.key === "Escape") {
      event.preventDefault();
      closeCommitDialog();
    } else if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
      event.preventDefault();
      await submitCommit();
    }
    return;
  }

  // Preserve native browser focus-location and reload shortcuts.
  if (event.metaKey && ["l", "r"].includes(event.key.toLowerCase())) return;

  if (event.metaKey && event.shiftKey && event.key.toLowerCase() === "f") {
    event.preventDefault();
    if (state.component === "search") focusSearchInput();
    else switchComponent("search");
    return;
  }
  // Cmd+F: in-file find; Cmd+Alt+F: find & replace. Only over the editable editor;
  // elsewhere it falls through to the browser's native find.
  if (event.metaKey && !event.shiftKey && event.key.toLowerCase() === "f"
      && state.component === "explorer" && app.querySelector(".editor-input")) {
    event.preventDefault();
    openFind(event.altKey);
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
  // Cmd+C with a match still highlighted from a just-closed find bar → copy it.
  if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey
      && event.key.toLowerCase() === "c" && state.find.kept
      && state.component === "explorer" && state.focusLevel === "app") {
    event.preventDefault();
    const input = app.querySelector(".editor-input");
    if (input) copyText(input.value.slice(state.find.kept.start, state.find.kept.end));
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
      // A kept find highlight? First Esc dismisses it; otherwise nothing to do.
      if (state.find.kept) { event.preventDefault(); clearFindKept(); }
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
    const target = ({ e: "explorer", h: "history", c: "compare", s: "status", f: "search" })[event.key];
    if (target) { event.preventDefault(); await switchComponent(target); }
    return;
  }
  if (event.key === "g") {
    event.preventDefault();
    state.gPending = true;
    setTimeout(() => { state.gPending = false; }, 10000);
    return;
  }
  if (state.focusLevel === "app" && event.key === "t") {
    event.preventDefault(); openTreePicker(); return;
  }
  if ((event.key === "i" || event.key === "Enter") && enterEditorInsertMode()) {
    event.preventDefault();
    return;
  }
  if (wakeEditorWithMotion(event)) {
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
  if (state.component === "search") {
    if (event.shiftKey && (event.key === "J" || event.key === "K")) {
      event.preventDefault();
      scrollPreview(event.key === "J" ? 1 : -1, 0.25);
      return;
    }
    if (event.ctrlKey && (event.key === "f" || event.key === "b")) {
      event.preventDefault();
      scrollPreview(event.key === "f" ? 1 : -1, 0.9);
      return;
    }
    if (state.pane === 0) {
      const up = event.key === "ArrowUp" || (event.ctrlKey && event.key === "p");
      const down = event.key === "ArrowDown" || (event.ctrlKey && event.key === "n");
      // Up off the top row of the list (k / ↑ / Ctrl-p) jumps back to the query box.
      if ((event.key === "k" || up) && state.searchSelected === 0) {
        event.preventDefault();
        focusSearchInput();
        return;
      }
      if (up) { event.preventDefault(); await moveSelection(-1); return; }
      if (down) { event.preventDefault(); await moveSelection(1); return; }
      if (event.key === "h" || event.key === "ArrowLeft") { event.preventDefault(); await searchCollapse(); return; }
      if (event.key === "l" || event.key === "ArrowRight") { event.preventDefault(); await searchExpand(); return; }
      if (event.key === "o" || event.key === "Enter") { event.preventDefault(); openSearchHit(); return; }
      if (event.key === "e") {
        event.preventDefault();
        const hit = searchRowTarget(state.searchRows[state.searchSelected]);
        if (hit) openFileInNewTab(hit.path);
        return;
      }
    }
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
  if (state.component === "status" && state.pane === 0 && event.key === "u") {
    event.preventDefault();
    await toggleStatusStage();
    return;
  }
  if (state.component === "status" && event.key === "C") {
    event.preventDefault();
    await openCommitDialog();
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

// Commit dialog wiring: backdrop click cancels, the buttons commit/cancel, and
// the message + amend toggle keep the submit button's enabled state in sync.
// Amend prefills the previous message when the box is still empty.
commitBackdrop.addEventListener("click", event => {
  if (event.target === commitBackdrop) closeCommitDialog();
});
commitCancel.addEventListener("click", () => closeCommitDialog());
commitSubmit.addEventListener("click", () => { submitCommit(); });
commitMessage.addEventListener("input", updateCommitSubmitState);
commitAmend.addEventListener("change", () => {
  if (commitAmend.checked && state.commit && !commitMessage.value.trim()) {
    commitMessage.value = state.commit.lastMessage;
  }
  updateCommitSubmitState();
});

// Warn before closing/reloading the tab while the open file has unsaved edits.
// The browser shows its own native confirm dialog when returnValue is set.
window.addEventListener("beforeunload", event => {
  if (state.currentFile && state.fileContent !== state.fileBaseContent) {
    event.preventDefault();
    event.returnValue = "";
  }
});

// Re-render the active diff view when crossing the 800px breakpoint so the
// resizable desktop layout and the stacked mobile layout swap cleanly (an
// inline grid-template would otherwise pin the desktop columns on a narrow tab).
window.matchMedia("(max-width: 800px)").addEventListener("change", () => {
  if (["history", "compare", "status", "search"].includes(state.component)) {
    switchComponent(state.component);
  }
});

async function boot() {
  try {
    loadRepoInfo();
    checkForUpdate();
    setInterval(heartbeat, 4000);
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

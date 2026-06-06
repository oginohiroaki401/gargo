const app = document.getElementById("app");
const header = document.getElementById("header");
const focusPath = document.getElementById("focus-path");
const popupBackdrop = document.getElementById("popup-backdrop");
const popupTitle = document.getElementById("popup-title");
const popupInput = document.getElementById("popup-input");
const popupList = document.getElementById("popup-list");
const toast = document.getElementById("toast");

const COMPONENTS = ["explorer", "history", "compare", "status"];
const state = {
  component: "explorer",
  focusLevel: "pane",
  pane: 0,
  gPending: false,
  files: [],
  currentFile: "",
  fileContent: "",
  fileBaseContent: "",
  fileHash: "",
  highlightLines: {},
  commits: [],
  historyCommit: 0,
  historyFile: 0,
  historyData: null,
  refs: [],
  compareBase: "",
  compareTarget: "",
  compareFiles: [],
  compareFile: 0,
  statusFiles: [],
  statusFile: 0,
  popup: null,
  popupItems: [],
  popupFiltered: [],
  popupIndex: 0,
};

const COMMANDS = [
  { label: "Switch to Explorer", hint: "g e", run: () => switchComponent("explorer") },
  { label: "Switch to History", hint: "g h", run: () => switchComponent("history") },
  { label: "Switch to Compare", hint: "g c", run: () => switchComponent("compare") },
  { label: "Switch to Status", hint: "g s", run: () => switchComponent("status") },
  { label: "Open file tree", hint: "t", run: () => openTreePicker() },
  { label: "Save current file", hint: "Cmd+S", run: () => saveCurrentFile() },
  { label: "Refresh current component", hint: "r", run: () => refreshComponent() },
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

function activePane() {
  return app.querySelector(`.pane[data-pane="${state.pane}"]`);
}

function setFocus(level, pane = state.pane) {
  state.focusLevel = level;
  state.pane = Math.max(0, pane);
  document.querySelectorAll(".pane.focused").forEach(el => el.classList.remove("focused"));
  document.querySelectorAll("#header button.focused").forEach(el => el.classList.remove("focused"));
  if (level === "pane") {
    const paneEl = activePane();
    paneEl?.classList.add("focused");
    paneEl?.focus({ preventScroll: true });
  } else if (level === "component") {
    header.querySelector(`[data-component="${state.component}"]`)?.classList.add("focused");
    app.focus({ preventScroll: true });
  } else {
    app.focus({ preventScroll: true });
  }
  updateFocusChrome();
}

function updateFocusChrome() {
  header.querySelectorAll("button").forEach(button => {
    button.classList.toggle("active", button.dataset.component === state.component);
  });
  const paneName = activePane()?.dataset.name || "";
  focusPath.textContent = [state.focusLevel, state.component, state.focusLevel === "pane" ? paneName : ""]
    .filter(Boolean).join(" › ");
}

async function switchComponent(component) {
  if (!COMPONENTS.includes(component)) return;
  state.component = component;
  state.pane = 0;
  state.focusLevel = "pane";
  location.hash = component;
  if (component === "explorer") await renderExplorer();
  if (component === "history") await renderHistory();
  if (component === "compare") await renderCompare();
  if (component === "status") await renderStatus();
  setFocus("pane", 0);
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
}

async function renderExplorer() {
  app.innerHTML = `<section class="component">
    ${componentBar("Explorer", `<span><span class="key">t</span> tree · <span class="key">⌘P</span> files · <span class="key">⌘⇧P</span> commands · <span class="key">⌘@</span> symbols</span>`)}
    <div id="explorer-surface" class="pane focused" tabindex="-1" data-pane="0" data-name="editor"></div>
  </section>`;
  if (!state.currentFile) {
    app.querySelector("#explorer-surface").innerHTML =
      `<div class="empty">No file open. Press <span class="key">t</span> for the tree or <span class="key">Cmd+P</span> for files.</div>`;
  } else {
    await renderCodeSurface(app.querySelector("#explorer-surface"), {
      path: state.currentFile,
      content: state.fileContent,
      editable: true,
    });
  }
}

async function openFile(path, line = null, col = 0) {
  const data = await api(`/api/file?path=${encodeURIComponent(path)}`);
  state.currentFile = data.path;
  state.fileContent = data.content;
  state.fileBaseContent = data.content;
  state.fileHash = data.hash;
  await api("/api/last-file", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ path }),
  }).catch(() => {});
  state.component = "explorer";
  await renderExplorer();
  const input = app.querySelector(".editor-input");
  if (input && line !== null) {
    const lines = input.value.split("\n");
    const offset = lines.slice(0, line).reduce((n, value) => n + value.length + 1, 0) + col;
    input.setSelectionRange(offset, offset);
    input.focus();
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
      ${options.editable ? `<span>Esc app focus · Cmd+S save</span>` : `<span>read only</span>`}
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
    <textarea class="editor-input" spellcheck="false" aria-label="Editor"></textarea></div>`;
  const input = body.querySelector(".editor-input");
  const layer = body.querySelector(".highlight-layer");
  input.value = options.content || "";
  await updateHighlightLayer(layer, options.path, input.value);
  input.style.height = `${Math.max(input.scrollHeight, body.clientHeight)}px`;
  input.addEventListener("input", async () => {
    state.fileContent = input.value;
    input.style.height = "auto";
    input.style.height = `${Math.max(input.scrollHeight, body.clientHeight)}px`;
    updateDirtyIndicator();
    clearTimeout(input.highlightTimer);
    input.highlightTimer = setTimeout(() => updateHighlightLayer(layer, options.path, input.value), 120);
  });
  input.addEventListener("scroll", () => {
    layer.style.transform = `translate(${-input.scrollLeft}px, ${-input.scrollTop}px)`;
  });
  updateDirtyIndicator();
}

function updateDirtyIndicator() {
  const dirty = app.querySelector(".code-toolbar .dirty");
  if (dirty) dirty.textContent = state.fileContent !== state.fileBaseContent ? "modified" : "";
}

function numberedPlainText(content) {
  return content.split("\n").map((line, index) =>
    `<span class="line-number">${index + 1}</span>${escapeHtml(line)}`
  ).join("\n");
}

async function updateHighlightLayer(layer, path, content) {
  try {
    const data = await api("/api/highlight", {
      method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ path, content }),
    });
    layer.innerHTML = content.split("\n").map((line, index) => {
      const spans = data.lines?.[String(index)] || [];
      let out = "";
      let cursor = 0;
      for (const span of spans) {
        out += escapeHtml(Array.from(line).slice(cursor, span.start).join(""));
        out += `<span class="gr-hl-${escapeHtml(span.scope)}">${escapeHtml(Array.from(line).slice(span.start, span.end).join(""))}</span>`;
        cursor = span.end;
      }
      out += escapeHtml(Array.from(line).slice(cursor).join(""));
      return `<span class="line-number">${index + 1}</span>${out}`;
    }).join("\n");
  } catch (_) {
    layer.innerHTML = numberedPlainText(content);
  }
}

async function renderHistory() {
  if (!state.commits.length) {
    const data = await api("/api/commits");
    state.commits = data.commits || [];
  }
  if (state.commits.length && !state.historyData) await loadHistoryCommit();
  const files = state.historyData?.files || [];
  await renderDiffView({
    kind: "history",
    title: "History",
    hint: `<span><span class="key">j/k</span> select · <span class="key">l/Tab</span> right · <span class="key">h/Esc</span> left</span>`,
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

function fileList(files, selected) {
  return listHtml(files, selected, file =>
    `<span class="status-badge status-${statusName(file.status)}">${statusLetter(file.status)}</span>
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
  const options = state.refs.map(ref => `<option value="${escapeHtml(ref)}"></option>`).join("");
  await renderDiffView({
    kind: "compare",
    title: "Compare",
    hint: `<span>Any branch, tag, or commit ref · <span class="key">j/k</span> files</span>`,
    panes: [
      {
        title: "Source · ref pair", name: "ref pair and changed files",
        body: `<form class="ref-form" id="ref-form">
          <input name="base" value="${escapeHtml(state.compareBase)}" list="refs" aria-label="Base ref">
          <span>…</span>
          <input name="target" value="${escapeHtml(state.compareTarget)}" list="refs" aria-label="Target ref">
          <button class="small-button" type="submit">Load</button>
          <datalist id="refs">${options}</datalist>
        </form>
        ${fileList(state.compareFiles, state.compareFile)}`,
      },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
    bind: () => {
      document.getElementById("ref-form").addEventListener("submit", async event => {
        event.preventDefault();
        const form = new FormData(event.currentTarget);
        state.compareBase = String(form.get("base") || "").trim();
        state.compareTarget = String(form.get("target") || "").trim();
        await loadCompare();
        await renderCompare();
        setFocus("pane", 0);
      });
    },
  });
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

async function renderStatus() {
  const data = await api("/api/status");
  state.statusFiles = ["unstaged", "staged", "untracked"].flatMap(section =>
    (data[section] || []).map(file => ({ ...file, section }))
  );
  state.statusFile = Math.min(state.statusFile, Math.max(0, state.statusFiles.length - 1));
  await renderDiffView({
    kind: "status",
    title: "Status",
    hint: `<span>Worktree vs HEAD · <span class="key">j/k</span> files · <span class="key">Ctrl-d/u</span> preview page</span>`,
    panes: [
      { title: "Changed files", name: "changed files", body: fileList(state.statusFiles, state.statusFile) },
      { title: "Preview", name: "preview", body: diffSurfaceHtml() },
    ],
  });
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

async function refreshComponent() {
  if (state.component === "history") { state.commits = []; state.historyData = null; }
  if (state.component === "compare") state.compareFiles = [];
  await switchComponent(state.component);
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

function showPopup(kind, title, items, placeholder = "") {
  state.popup = kind;
  state.popupItems = items;
  state.popupIndex = 0;
  popupTitle.textContent = title;
  popupInput.placeholder = placeholder;
  popupInput.value = "";
  popupBackdrop.hidden = false;
  filterPopup();
  popupInput.focus();
}

function closePopup() {
  state.popup = null;
  popupBackdrop.hidden = true;
  setFocus(state.focusLevel, state.pane);
}

function filterPopup() {
  const query = popupInput.value.trim();
  state.popupFiltered = state.popupItems
    .map(item => ({ ...item, score: fuzzyScore(item.search || item.label, query) }))
    .filter(item => item.score >= 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, 300);
  state.popupIndex = Math.min(state.popupIndex, Math.max(0, state.popupFiltered.length - 1));
  popupList.innerHTML = state.popupFiltered.map((item, index) =>
    `<li data-index="${index}" class="${index === state.popupIndex ? "selected" : ""}">
      ${escapeHtml(item.label)}${item.hint ? `<span class="hint">${escapeHtml(item.hint)}</span>` : ""}
    </li>`).join("") || `<li>No matches</li>`;
  popupList.querySelectorAll("[data-index]").forEach(li => li.addEventListener("click", () => choosePopup(Number(li.dataset.index))));
}

async function choosePopup(index = state.popupIndex) {
  const item = state.popupFiltered[index];
  if (!item) return;
  closePopup();
  await item.run();
}

async function openFilePicker() {
  await ensureFiles();
  showPopup("files", "File picker", state.files.map(path => ({
    label: path, run: () => openFile(path),
  })), "Search files");
}

async function openTreePicker() {
  await ensureFiles();
  const items = state.files.map(path => {
    const parts = path.split("/");
    return {
      label: `${"  ".repeat(parts.length - 1)}${parts.length > 1 ? "└ " : ""}${parts.at(-1)}`,
      hint: path,
      search: path,
      run: () => openFile(path),
    };
  });
  showPopup("tree", "Explorer · file tree", items, "Filter tree");
}

function openCommandPicker() {
  showPopup("commands", "Command picker", COMMANDS.map(command => ({
    label: command.label, hint: command.hint, run: command.run,
  })), "Run command");
}

async function openSymbolPicker() {
  if (!state.currentFile) { notify("Open a file before using the symbol picker"); return; }
  const data = await api("/api/symbols", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ path: state.currentFile, content: state.fileContent }),
  });
  showPopup("symbols", `Symbols · ${state.currentFile}`, (data.symbols || []).map(symbol => ({
    label: symbol.name, hint: `${symbol.kind} · ${symbol.line + 1}`,
    run: () => openFile(state.currentFile, symbol.line, symbol.col),
  })), "Search symbols");
}

popupInput.addEventListener("input", filterPopup);
popupInput.addEventListener("keydown", event => {
  if (event.key === "Escape") { event.preventDefault(); closePopup(); }
  else if (event.key === "ArrowDown" || (event.key === "j" && !event.metaKey && !event.ctrlKey)) {
    event.preventDefault();
    state.popupIndex = Math.min(state.popupIndex + 1, state.popupFiltered.length - 1);
    filterPopup();
  } else if (event.key === "ArrowUp" || (event.key === "k" && !event.metaKey && !event.ctrlKey)) {
    event.preventDefault();
    state.popupIndex = Math.max(0, state.popupIndex - 1);
    filterPopup();
  } else if (event.key === "Enter") {
    event.preventDefault();
    choosePopup();
  }
});

header.addEventListener("click", event => {
  const button = event.target.closest("[data-component]");
  if (button) switchComponent(button.dataset.component);
});

window.addEventListener("keydown", async event => {
  const isText = event.target.matches("textarea, input");
  if (state.popup) return;

  if (event.metaKey && event.key.toLowerCase() === "p") {
    event.preventDefault();
    if (event.shiftKey) openCommandPicker(); else openFilePicker();
    return;
  }
  if (event.metaKey && (event.key === "@" || (event.shiftKey && event.key === "2"))) {
    event.preventDefault();
    openSymbolPicker();
    return;
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
    event.preventDefault();
    await saveCurrentFile();
    return;
  }
  if (event.key === "Escape") {
    event.preventDefault();
    if (isText && event.target.classList.contains("editor-input")) {
      setFocus("app", 0);
    } else if (state.focusLevel === "pane" && state.pane > 0) {
      setFocus("pane", state.pane - 1);
    } else if (state.focusLevel === "pane") {
      setFocus("component", 0);
    } else {
      setFocus("app", 0);
    }
    return;
  }
  if (isText) return;

  if (state.gPending) {
    state.gPending = false;
    const target = ({ e: "explorer", h: "history", c: "compare", s: "status" })[event.key];
    if (target) { event.preventDefault(); await switchComponent(target); }
    return;
  }
  if (event.key === "g") {
    event.preventDefault();
    state.gPending = true;
    focusPath.textContent = "g …";
    setTimeout(() => { state.gPending = false; updateFocusChrome(); }, 900);
    return;
  }
  if (state.component === "explorer" && event.key === "t") {
    event.preventDefault(); openTreePicker(); return;
  }
  if (event.key === "r") {
    event.preventDefault(); await refreshComponent(); return;
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
    const preview = app.querySelector(".pane:last-child .code-surface")
      || app.querySelector(".pane:last-child");
    preview?.scrollBy({ top: (event.key === "d" ? 1 : -1) * preview.clientHeight * .7, behavior: "smooth" });
  }
});

async function boot() {
  try {
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
  } catch (error) {
    app.innerHTML = `<div class="error">${escapeHtml(error.message)}</div>`;
  }
}

boot();

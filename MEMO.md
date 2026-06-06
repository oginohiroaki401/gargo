# gargo UI Spec (minimal)

gargo is a code/Git browser with a simple embedded editor. It runs as a **web server**; the UI is a browser app driven by keyboard navigation.
Abandon current gargo server FE code and create new. 

## 1. Focus model

Focus is a 3-level hierarchy:

```
app  ->  component  ->  pane
```

- `Esc` moves focus **up one level**. If a popup is open, `Esc` closes it first.
- Keybinds of the **currently focused element** are the active ones.
- A component is entered at its **primary (leftmost) pane**.

### Switching components

- From **app focus** (or any non-text pane), `g` then one of `e` / `h` / `c` / `s` switches the active component and focuses it:
  - `g e` -> Explorer
  - `g h` -> History
  - `g c` -> Compare
  - `g s` -> Status
- The editor uses `g` as its own prefix (Helix-style goto: `gg`, `ge`, `gh`, ...). So **inside the editor's text-editing focus, `g` belongs to the editor**. To switch components from the editor, press `Esc` to reach app focus first, then `g <x>`.

## 2. Header

The header shows 4 separate entries, always:

```
Explorer | History | Compare | Status
```

## 3. Components

### Explorer (+ editor)

- The **editor** is the main surface.
- The **file tree is a popup**, not a permanent sidebar. Selecting an entry in the tree opens that file in the editor.
- Pickers:
  - `Cmd+P` — file picker
  - `Cmd+Shift+P` — command picker
  - `Cmd+@` — symbol picker (for the current file)

### History

Commit log browser. 3 panes:

```
[ commit log ] [ changed files ] [ preview ]
```

### Compare

Compare two refs. 2 panes:

```
[ ref selector + changed files ] [ preview ]
```

- The "source" is a **ref pair**. Keep the source abstraction general (branch / commit / tag), not branch-only, so commit-to-commit compare can be added later without a new component.

### Status

Working-tree `git status`. 2 panes:

```
[ changed files ] [ preview ]
```

- `g s` must land directly on uncommitted changes with no extra selection step (this is the hot path).

## 4. Shared internals (important)

History, Compare, and Status are the **same view with a different diff source**. They all reduce to `diff(source)`:

| Component | Source | Git equivalent |
|-----------|--------|----------------|
| Status    | worktree vs HEAD | `git diff` / `git diff --staged` |
| Compare   | refA vs refB | `git diff main...feature` |
| History   | a commit | `git show` = `git diff <commit>^ <commit>` |

All three share the shape:

```
[ source selector ] + [ changed files ] + [ preview ]
```

Implementation rules:

- Build **one `DiffView` component** with a **pluggable source**. History / Compare / Status are this component with the source preset (and Status has no explicit selector).
- The **preview pane is the Explorer editor reused in read-only mode**. Syntax highlighting and diff rendering must be a single code path, not duplicated per component.

## 5. Pane focus model (master-detail)

Panes are **not equal peers**. Left pane = driver, right pane = follower.

- Default focus is the **leftmost pane** (file list / commit log).
- `j` / `k` move the selection in the focused pane; downstream panes **auto-follow** the selection.
  - History chain: commit log -> changed files -> preview.
- `l` (or `Tab`) moves focus **rightward** (e.g. into the preview to scroll, search, or do hunk-level actions).
- `h` (or `Esc`) moves focus **leftward** / up.
- While focused on a list pane, `Ctrl-d` / `Ctrl-u` page the preview **without** switching focus (avoids stepping right for short diffs).

## 6. Picker scope

- **File picker** (`Cmd+P`) and **command picker** (`Cmd+Shift+P`) are **global** — usable from any component.
- **Symbol picker** (`Cmd+@`) is **context-dependent** — operates on the current file / diff target.

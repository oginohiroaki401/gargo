- [x] explorer(editor)'s tree : fix it to make it look like cli's spc e finder.
- [x] explorer(editor) : g->e to enter to focus on editor. i to insert(edit) mode. esc to app focus(so that g-> e -> enter -> insert & edit and save -> esc -> g -> s to status works
- [x] explorer(editor) : `t` in app focus to open tree.
- [x] leave cmd l and cmd r untouched(want browser to handle that so that focus on url or reload works)
- [x] in compare, want shift j and shift k to scroll in preview.
- [x] in status, implement v for viwed checkbox toggle, `o` to open in editor
- [x] in file picker and command picker, if nothing was in input field, show candidate like what cli's picker does
- [x] compare : in compare, want shift j and shift k to scroll in preview.
- [x] compare : in compare, want o to open that file in editor
- [x] compare : also v checkbox
- [x] history : shift j and shift k to up/down in changed files
- [x] explorer(editor) : see memo1

## memo1
- have app to have two focus in explorer tab. app(with editor READONLY) or editor(writable)
- esc in editor to focus to app. esc in app does nothing
- `t` in app to file tree(explorer)
- `i` or `Enter` in app make editor writable, focus to editor with cursor
- `j` or `k` in app make editor scroll

# PLAN_MEMO — ブラウザ版 emacs スタイルエディタ

gargo のブラウザエディタを **emacs/VSCode 風(常時 input mode)** で本実装していくための作業メモ。
現状は `/editor` ルートのエディタが動いており、その感触で良さそうなので、この方向で進める。

- 旧モーダル版 PoC(`/edit`, hjkl / Normal-Insert-Visual)は**削除済み**(`index.html` / `editor.js` / 関連ルート)。
- gargo server の共通ヘッダ(app-rail)に **Editor リンク**を追加済み。エディタページでも同じヘッダを表示。
- 本実装も「WASM がエディタの権威モデル、サーバは file の read/save・file 列挙・(将来)highlight 計算」という横引き構成を踏襲する。

---

## 1. 現状の仕様(`/editor`)

### アーキテクチャ
- 編集ロジックはすべて **タブ内の WASM (`WebEditor`)** で完結。`gargo server` は
  - ファイルを開くとき read(`GET /api/file?path=`)
  - 保存するとき write(`POST /api/save`、VSCode 風の **409 競合検知**:ロード時 hash と不一致なら拒否)
  のみを担う。
- モーダルコア(rope / cursors / keymap / undo)を WASM 化したものをそのまま流用。`/emacs` は
  **常に Insert mode に固定**し、emacs 風キーと追従スクロール・マウスだけを上に被せた薄い層。
- syntax highlight は **未実装**(tree-sitter の grammar は native C で wasm32 にできないため。→ §2 で対応方針)。

### ファイル構成
| 役割 | パス |
|---|---|
| ペライチ本体(HTML+CSS+inline module JS) | `assets/web_editor/editor.html`(`{{APP_CSS}}` / `{{APP_RAIL}}` スロットあり) |
| サーバ(ページ配信 + file API) | `src/command/web_editor_server.rs`(`EDITOR_HTML` / `handle_editor_page` / `handle_api_files`) |
| ルート登録 | `src/command/github_server.rs`(`/editor`, `/editor/{*path}`, `/api/files`) |
| 共通ヘッダ(Editor リンク追加) | `src/command/app_shell.rs`(`app_rail_html`) |
| WASM バインディング | `src/wasm/mod.rs`(`WebEditor`) |
| Core 編集プリミティブ | `src/core/document/editing.rs`, `src/core/editor/web_dispatch.rs` |
| ファイル列挙(Cmd+P) | `src/project.rs`(`collect_files`)/ fuzzy は `src/ui/shared/filtering.rs` を JS へ移植 |
| キーマップ(流用) | `src/input/keymap.rs` |

WASM バンドル(`assets/web_editor/pkg/`)は生成物。再生成手順:
```
cargo build --lib --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
  --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web
```
`emacs.html` は `include_str!` でバイナリに埋め込まれるため、編集後は **ネイティブ `cargo build` も必要**。

### WASM API(`WebEditor`)
- 既存: `key(code, ctrl, shift, alt)` / `insert_text` / `content` / `version` / `is_dirty` /
  `line_count` / `mode` / `cursor_row` / `cursor_col` / `render(top, height)`
- PoC で追加:
  - `has_selection()` → bool
  - `delete_selection()` → bool(選択削除。`DeleteSelection` と違い **Normal に戻さず Insert 維持**)
  - `delete_line()`(現在行を改行ごと削除)
  - `set_cursor(row, col)`(クリックでカーソル移動。col は表示桁、tab/CJK 幅対応)
  - `set_selection(aRow, aCol, hRow, hCol)`(ドラッグ選択)
  - ヘルパ `line_col_to_offset`(表示座標→char offset。`offset_to_display_col` の逆)
- Core 追加: `Document::delete_current_line()` / `Document::delete_active_selection()`
  (native ユニットテスト 4 本あり:`src/core/document/tests.rs`)

### 入力仕様(`/emacs`)
- **常時 Insert**:起動時に `editor.key("i")` を一回送って Insert へ。`Escape` は転送せず無視。
- **カーソル移動**:`Ctrl+f/b/n/p`(←/→/↓/↑)、`Ctrl+a/e`(行頭/行末)、矢印キー
  (insert mode でも `keymap` の `ctrl_motion_action` が効くため、コア無改造で成立)
- **編集**:文字入力、`Backspace`、`Enter`、`Tab`、`Ctrl+k`(行末まで kill)、`Ctrl+d`(前方削除)、
  `Ctrl+h`(後方削除)、`Ctrl+j`(改行)
- **選択**:`Shift+矢印`、`Ctrl+Shift+A/E`(行頭/行末)、`Ctrl+Shift+←/→`(単語)
- **選択編集(VSCode 風)**:選択して `Backspace/Delete` で削除、印字キー/`Enter`/`Tab`/ペースト/IME 確定で**置換**
- **行全体削除**:`Ctrl+Shift+K`
- **Mac 風の修飾削除**(JS 層で実装):`Cmd+Backspace`=行頭まで / `Cmd+Delete`=行末まで /
  `Alt+Backspace`=前の単語 / `Alt+Delete`=次の単語
- **保存**:`Ctrl/Cmd+S`(409 で上書き確認ダイアログ)
- **ファイルピッカー**:`Cmd+P` で fuzzy ファイルピッカー(`/api/files` = `collect_files`、CLI と同じ列挙・
  あいまい一致を JS 移植)。`↑↓` / `Ctrl+n/p` で移動、`Enter` で `/editor/<path>` へ遷移、`Esc` で閉じる。
  `/editor`(ファイル無し)で開くと自動でピッカーを表示。
- **マウス**:クリックでカーソル移動、ドラッグで範囲選択(枠外ドラッグはビュー追従)
- **IME**:隠し `<textarea>` + composition イベントで日本語入力対応
- **ヘッダ**:gargo server 共通の app-rail を上部に表示(Code/Status/Branches/Commits/Editor)。
  ※ エディタページでは server のショートカット JS は読み込まない(タイピングと衝突するため)。

### 表示・スクロール
- 仮想行レンダリング(可視範囲 + overscan のみ DOM 化)、caret / selection はオーバーレイ div。
- **カーソル追従スクロール(縦+横)**:キー/編集/ドラッグ後に `ensureCursorVisible()`。
- gutter に行番号。テーマはダーク、status バーは紫(モーダル版の青と区別)。

### 既知の制約(現状)
- syntax highlight:実装済み(§2.2)。ただし編集中は debounce(200ms)まで前回 span のまま
  描画されるため、編集直後の数百 ms はずれた色になりうる(致命的でない)。
- `Cmd+Z` / `Cmd+Shift+Z`(undo/redo):実装済み(WASM `undo()/redo()`)。`Cmd+Y` も redo。
- ダブルクリック単語選択・シフトクリック選択拡張:実装済み(§2.1)。
- サイドバー file tree:実装済み(§2.3)。ただしファイルを開くのはフルページ遷移。タブ/バッファは未実装。
- 選択中の `Ctrl+h`/`Ctrl+d` は選択削除にならず 1 文字削除(`Backspace/Delete` のみ選択対応)。
- `Ctrl+x` は chord 状態に入り次キーを 1 つ消費(App アクション扱いで実害小、放置)。

---

## 2. 本実装の方針 / やりたいこと

### 2.1 入力まわりの追加(優先・小)
- [x] **ダブルクリックで単語選択**:`mousedown` の `detail===2` を検出し、WASM
      `select_word_at(row, col)`(`word_range_at` 流用)で単語境界を選択。
- [x] **シフトクリックで選択拡張**:`mousedown` の `e.shiftKey` で、既存選択アンカー
      (WASM `anchor_row()/anchor_col()`、無ければ現カーソル)を固定し head をクリック位置へ
      (`set_selection` 流用)。以降のドラッグも同アンカーから拡張継続。
- [x] **`Cmd+Z` / `Cmd+Shift+Z`(undo/redo)**:WASM に `undo()/redo()` を追加
      (`dispatch_core(Undo/Redo)`、insert 維持)。JS 層で `Cmd/Ctrl+Z`→undo、
      `Cmd+Shift+Z` / `Cmd+Y`→redo にバインド。

### 2.2 syntax highlight(中・サーバ連携) — ✅ 実装済み
実装サマリ:`POST /api/highlight { path, content }` → `{ lines: { "<行>": [{start, end, scope}] } }`。
サーバは拡張子で言語判定(`LanguageRegistry::detect_by_extension`)し `syntax::highlight::highlight_text`
を呼ぶ。span の byte offset は **タブ展開後の char offset**(`byte_to_expanded_col`、tab→4)に変換して返すので、
クライアントは wasm が出す展開済み行文字列の部分文字列をそのまま色付けできる。`scope` は capture 名の先頭成分
(`keyword`/`string`/`comment`/…)。クライアントは編集後 **debounce 200ms** で再取得し(`scheduleHighlight`、
`editor.version()` で content 不変時はスキップ)、`paintRow` が `<span class="tok-...">` で塗り分け。
色は CSS の `.tok-*`(VSCode Dark+ 系パレット)。未対応言語は空マップ。

(旧方針メモ)tree-sitter はバンドルせず、ローカルサーバ側で計算して highlight span をブラウザへ返す。
  - 理由:tree-sitter grammar は native C で wasm32 ターゲットにできない。gargo 本体(native server)は
    既に tree-sitter を持っているはずなので、そこを再利用する。
- API 案:`POST /api/highlight { path, content | version, lang }` → `{ spans: [{start, end, scope}] }`
  (char offset か (row,col) ベース)。
- フロー:
  1. 編集が落ち着いたら(**debounce** 150〜300ms)現在の `content` と `version` をサーバへ送る。
  2. サーバが tree-sitter で解析し、token scope の span を返す。
  3. レンダラが span を行単位に割り、`.row` 内を `<span class="tok-...">` で塗り分け。
  4. `version` で古いレスポンスを破棄(送信時の version と現在を比較)。
- 注意:大ファイルは増分(可視範囲優先 or tree-sitter の incremental parse)を検討。まずは全体 + debounce で可。
- 参考:memory `web-editor.md` の future work(server-side tree-sitter highlight を debounce 配信)。

### 2.3 UI レイアウト(中〜大)
- [x] **サイドバーに file tree、メインにエディタ**(VSCode 風 2 ペイン)。
  - 新規 API は作らず **`/api/files`(`collect_files` = git ls-files 相当)を流用**し、JS でフラットな
    パス一覧から入れ子ツリーを構築(`buildTree`)。`.gitignore` 等は collect_files が考慮済み。
  - ディレクトリは折りたたみ可(`expandedDirs`)、起動時は現在ファイルまでのパスを自動展開。
    ファイルクリックは `/editor/<path>` へフルページ遷移(`openFile`)で開く。
  - レイアウト:`#main` を `flex-direction:row` で `sidebar | resizer | scroller`。
    サイドバー幅は `#sidebar-resizer` ドラッグでリサイズ(`--sidebar-w`)。
- [ ] **(未)バッファ/タブ**:複数ファイルの保持・切替・dirty 表示。現状はクリックでフルページ遷移。
- [ ] **(未)ツリーのインライン読み込み**:遷移せず `/api/file` で流し込み(URL 同期)。

### 2.4 実装構成の見直し(本実装移行時)
- PoC は inline JS のペライチ。本実装では JS をモジュール分割(`input` / `render` / `mouse` /
  `highlight` / `tree` / `net`)し、ビルドパイプラインに載せるか検討。
- `emacs.html` の埋め込み(`include_str!`)方針は据え置きで可。assets が増えるならまとめて配信を整理。
- WASM API は安定化(命名・粒度)。`set_*` 系・`undo/redo`・`select_word_at` を整理。

---

## 3. 検証
- ネイティブ:`cargo run -- --server --no-open` → 出力ホストの `…/emacs/<相対パス>` を開く。
- WASM 変更時は §1 の 2 コマンドで再生成 → `cargo build`。
- Core ロジックは `cargo test --lib`(`document::tests` に選択削除/行削除テストあり。今後の追加分も同様に）。

## 4. リリース
- 残す判断になったら `release-workflow`(fmt / clippy / test / version bump / tag)に沿って
  コミット → PR。現ブランチ `feat/web-editor-mvp`。

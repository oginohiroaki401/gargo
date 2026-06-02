# Gargo

Gargo is a terminal text editor written in Rust.

## Requirements

- Rust stable toolchain
- A terminal with true color support

## Quick start

Run with an empty scratch buffer:

```bash
cargo run
```

Open a file or directory:

```bash
cargo run -- path/to/file_or_directory
```

Run optimized build:

```bash
cargo run --release -- path/to/file
```

## Installation

Quick install:

```bash
curl -fsSL https://github.com/aplio/gargo/raw/refs/heads/master/install.sh | sh
```

Install a specific version:

```bash
curl -fsSL https://github.com/aplio/gargo/raw/refs/heads/master/install.sh | GARGO_VERSION=v0.1.13 sh
```

Install to a custom directory:

```bash
curl -fsSL https://github.com/aplio/gargo/raw/refs/heads/master/install.sh | GARGO_BIN_DIR=$HOME/.bin sh
```

Checksum verification is enabled when a release includes `checksums.txt`. Set `GARGO_SKIP_VERIFY=1` to skip verification.

- Legacy/manual install still works by downloading a release tarball from [GitHub Releases](https://github.com/aplio/gargo/releases) and placing `gargo` on your `PATH`.

Supported assets:
- `gargo-v<version>-x86_64-apple-darwin.tar.gz`
- `gargo-v<version>-aarch64-apple-darwin.tar.gz`
- `gargo-v<version>-x86_64-unknown-linux-gnu.tar.gz`
- `gargo-v<version>-aarch64-unknown-linux-gnu.tar.gz`

## Source install

```bash
cargo install --path .
```

### Web editor (browser)

`gargo --server` includes a browser-based editor whose modal core runs in-tab as
WebAssembly. The wasm bundle is **embedded into the binary at build time**, so
release binaries (and `gargo --update`) ship it automatically — no extra setup
for end users.

When building from source, generate the bundle before `cargo build` so it gets
embedded (it lives in `assets/web_editor/pkg/`, which is gitignored):

```bash
cargo build --lib --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
  --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web
```

This needs the `wasm32-unknown-unknown` target (`rustup target add
wasm32-unknown-unknown`) and `wasm-bindgen-cli` at the exact version of the
`wasm-bindgen` crate in `Cargo.lock`. If the bundle is missing, `cargo build`
still succeeds and the editor's asset routes report "wasm not built".

## Basic keys

- `i`: enter insert mode
- `Esc`: return to normal mode
- `Ctrl+S`: save current buffer
- `Ctrl+Q`: close current buffer, or quit when it is the last one
- `SPC f`: open file picker
- `SPC p`: open command palette
- `SPC g`: open flat changed-files sidebar with status badges
- `SPC G`: open Git view
- Mouse: left-drag an editor split border to resize pane widths/heights

## More docs

- `docs/README.md` for architecture
- `docs/CONTRIBUTING.md` for development workflow

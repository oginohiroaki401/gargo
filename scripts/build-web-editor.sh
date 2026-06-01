#!/usr/bin/env bash
# Build the browser-editor wasm bundle (assets/web_editor/pkg/).
# Run this once, and again after editing any Rust the wasm build touches
# (core/, input/, src/wasm/, src/wasm_stubs/).
#
# Requires wasm-bindgen-cli matching the `wasm-bindgen` crate version:
#   cargo install wasm-bindgen-cli --version 0.2.114
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> building lib for wasm32 (release)"
cargo build --lib --release --target wasm32-unknown-unknown

echo "==> generating JS bindings into assets/web_editor/pkg"
wasm-bindgen target/wasm32-unknown-unknown/release/gargo.wasm \
  --out-dir assets/web_editor/pkg --out-name gargo_wasm --target web

echo
echo "Done. Now run the server (from the repo you want to edit):"
echo "    cargo run -- --server --no-open"
echo "It prints a browse URL like http://127.0.0.1:PORT/owner/repo —"
echo "for the EDITOR open:   http://127.0.0.1:PORT/edit/<relative/path>"

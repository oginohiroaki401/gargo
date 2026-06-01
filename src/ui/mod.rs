// `text` is dependency-free (unicode-width only) and is used by `core`, so it
// is the one UI submodule available on wasm. Everything else is terminal-only.
pub mod text;

#[cfg(not(target_arch = "wasm32"))]
pub mod diff;
#[cfg(not(target_arch = "wasm32"))]
pub mod framework;
#[cfg(not(target_arch = "wasm32"))]
pub mod image;
#[cfg(not(target_arch = "wasm32"))]
pub mod overlays;
#[cfg(not(target_arch = "wasm32"))]
pub mod popup_layout;
#[cfg(not(target_arch = "wasm32"))]
pub mod shared;
#[cfg(not(target_arch = "wasm32"))]
pub mod text_input;
#[cfg(not(target_arch = "wasm32"))]
pub mod url;
#[cfg(not(target_arch = "wasm32"))]
pub mod views;

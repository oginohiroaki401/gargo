use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use base64::Engine;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use image::imageops::FilterType;

#[derive(Debug, Clone)]
pub struct ImageRenderRequest {
    pub key: PathBuf,
    pub col: u16,
    pub row: u16,
    pub cell_cols: u16,
    pub cell_rows: u16,
    pub data: Arc<EncodedImage>,
}

/// Compute cell dimensions for an image so it fits within `(max_cols, max_rows)`
/// while preserving aspect ratio. Assumes a terminal cell is roughly twice as
/// tall as it is wide (typical for monospace fonts).
pub fn fit_cells(img_w: u32, img_h: u32, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if img_w == 0 || img_h == 0 || max_cols == 0 || max_rows == 0 {
        return (max_cols.max(1), max_rows.max(1));
    }
    const CELL_HEIGHT_RATIO: f64 = 2.0;
    let avail_w = max_cols as f64;
    let avail_h = max_rows as f64 * CELL_HEIGHT_RATIO;
    let scale = (avail_w / img_w as f64).min(avail_h / img_h as f64);
    let cols = (img_w as f64 * scale).floor().max(1.0) as u16;
    let rows = (img_h as f64 * scale / CELL_HEIGHT_RATIO).floor().max(1.0) as u16;
    (cols.min(max_cols), rows.min(max_rows))
}

pub fn is_image_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    )
}

pub fn supports_kitty_graphics() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        if let Ok(v) = std::env::var("GARGO_DISABLE_IMAGES")
            && (v == "1" || v.eq_ignore_ascii_case("true"))
        {
            return false;
        }
        if std::env::var("GARGO_FORCE_IMAGES")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        {
            return true;
        }
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        std::env::var("KITTY_WINDOW_ID").is_ok()
            || term == "xterm-kitty"
            || term == "xterm-ghostty"
            || term.contains("kitty")
            || term.contains("ghostty")
            || term_program.eq_ignore_ascii_case("ghostty")
            || term_program.eq_ignore_ascii_case("wezterm")
            || std::env::var("WEZTERM_PANE").is_ok()
            || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok()
            || std::env::var("GHOSTTY_BIN_DIR").is_ok()
    })
}

fn in_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

pub fn debug_log(msg: &str) {
    if std::env::var("GARGO_LOG_IMAGES").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/gargo-images.log")
    {
        let _ = writeln!(f, "{msg}");
    }
}

#[derive(Debug, Clone)]
pub struct EncodedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub fn load_and_encode(path: &Path, max_dim: u32) -> Option<EncodedImage> {
    let reader = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(r) => r,
        Err(e) => {
            debug_log(&format!("load_and_encode: open {:?} failed: {}", path, e));
            return None;
        }
    };
    let img = match reader.decode() {
        Ok(i) => i,
        Err(e) => {
            debug_log(&format!("load_and_encode: decode {:?} failed: {}", path, e));
            return None;
        }
    };
    let (w, h) = (img.width(), img.height());
    let resized = if w > max_dim || h > max_dim {
        img.resize(max_dim, max_dim, FilterType::Triangle)
    } else {
        img
    };
    let (rw, rh) = (resized.width(), resized.height());
    let mut buf = Vec::new();
    if let Err(e) = resized.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png) {
        debug_log(&format!("load_and_encode: encode {:?} failed: {}", path, e));
        return None;
    }
    debug_log(&format!(
        "load_and_encode: {:?} -> {}x{} ({} png bytes)",
        path,
        rw,
        rh,
        buf.len()
    ));
    Some(EncodedImage {
        png: buf,
        width: rw,
        height: rh,
    })
}

/// Wrap a Kitty graphics protocol payload (the bytes between `\x1b_G…\x1b\\`)
/// in a tmux DCS passthrough envelope when running inside tmux. The caller
/// provides the inner sequence including its leading ESC and trailing ESC `\`.
fn wrap_for_tmux(inner: &str) -> String {
    if !in_tmux() {
        return inner.to_string();
    }
    // tmux passthrough: \x1bPtmux;<doubled ESC>...<doubled ESC>\x1b\\
    let doubled = inner.replace('\x1b', "\x1b\x1b");
    format!("\x1bPtmux;{doubled}\x1b\\")
}

/// Emit a Kitty graphics protocol APC sequence rendering `image` at the
/// given cell position, sized to fit `cell_cols` x `cell_rows` cells.
/// `image_id` is used for later deletion or replacement.
pub fn emit_kitty_image<W: Write>(
    stdout: &mut W,
    image_id: u32,
    col: u16,
    row: u16,
    cell_cols: u16,
    cell_rows: u16,
    image: &EncodedImage,
) -> io::Result<()> {
    queue!(stdout, MoveTo(col, row))?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&image.png);
    let bytes = b64.as_bytes();
    const CHUNK: usize = 4096;
    let total = bytes.len();
    let mut offset = 0;
    let mut first = true;

    debug_log(&format!(
        "emit_kitty_image id={} col={} row={} cells={}x{} png_bytes={} b64_bytes={} tmux={}",
        image_id,
        col,
        row,
        cell_cols,
        cell_rows,
        image.png.len(),
        total,
        in_tmux()
    ));

    while offset < total {
        let end = (offset + CHUNK).min(total);
        let is_last = end == total;
        let m = if is_last { 0 } else { 1 };
        let chunk_data = std::str::from_utf8(&bytes[offset..end]).unwrap_or("");

        let inner = if first {
            first = false;
            format!(
                "\x1b_Gf=100,a=T,i={},c={},r={},q=2,m={};{}\x1b\\",
                image_id, cell_cols, cell_rows, m, chunk_data
            )
        } else {
            format!("\x1b_Gm={};{}\x1b\\", m, chunk_data)
        };
        let payload = wrap_for_tmux(&inner);
        stdout.write_all(payload.as_bytes())?;
        offset = end;
    }

    Ok(())
}

pub fn clear_kitty_images<W: Write>(stdout: &mut W) -> io::Result<()> {
    let inner = "\x1b_Ga=d,d=A,q=2\x1b\\";
    let payload = wrap_for_tmux(inner);
    stdout.write_all(payload.as_bytes())
}

pub fn delete_kitty_image_by_id<W: Write>(stdout: &mut W, image_id: u32) -> io::Result<()> {
    let inner = format!("\x1b_Ga=d,d=i,i={image_id},q=2\x1b\\");
    let payload = wrap_for_tmux(&inner);
    stdout.write_all(payload.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detects_image_extensions() {
        assert!(is_image_path(Path::new("a.png")));
        assert!(is_image_path(Path::new("a.PNG")));
        assert!(is_image_path(Path::new("a.jpg")));
        assert!(is_image_path(Path::new("a.jpeg")));
        assert!(is_image_path(Path::new("a.webp")));
        assert!(!is_image_path(Path::new("a.txt")));
        assert!(!is_image_path(Path::new("noext")));
    }
}

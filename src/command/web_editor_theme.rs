//! Theming for the browser editor.
//!
//! The terminal editor reads `[theme]` (`preset` + `captures`) and renders with
//! ANSI/crossterm colors. The browser editor needs real CSS colors and some
//! web-only chrome (background, gutter, selection, …) that the terminal gets for
//! free from the host terminal. This module turns the user's `[theme.editor]`
//! config into the CSS injected into `editor.html`'s `{{THEME_CSS}}` slot.
//!
//! Two built-in palettes ship: `light` (the default, a clean light theme) and
//! `dark` (the VSCode-dark colors the editor originally hard-coded). Users pick
//! one via `preset` and override individual colors via `[theme.editor]` chrome
//! keys and `[theme.editor.tokens]` per-scope entries.

use crate::config::{ThemeConfig, ThemeEditorConfig};

/// Chrome (non-syntax) colors of the editor surface.
struct Chrome {
    bg: &'static str,
    fg: &'static str,
    gutter: &'static str,
    caret: &'static str,
    selection: &'static str,
    status_bg: &'static str,
    git_added: &'static str,
    git_modified: &'static str,
    git_deleted: &'static str,
    // Overlay/panel chrome (find widget, pickers, sidebar).
    panel_bg: &'static str,
    panel_border: &'static str,
    input_bg: &'static str,
    hover_bg: &'static str,
    muted: &'static str,
    accent: &'static str,
}

/// One syntax scope's styling. `scope` is the `tok-*` class suffix the server
/// emits (see `web_editor_server::capture_to_scope`).
struct Token {
    scope: &'static str,
    color: &'static str,
    bold: bool,
    italic: bool,
    underline: bool,
}

const fn tok(scope: &'static str, color: &'static str) -> Token {
    Token {
        scope,
        color,
        bold: false,
        italic: false,
        underline: false,
    }
}

const fn tok_italic(scope: &'static str, color: &'static str) -> Token {
    Token {
        italic: true,
        ..tok(scope, color)
    }
}

const fn tok_bold(scope: &'static str, color: &'static str) -> Token {
    Token {
        bold: true,
        ..tok(scope, color)
    }
}

const fn tok_underline(scope: &'static str, color: &'static str) -> Token {
    Token {
        underline: true,
        ..tok(scope, color)
    }
}

const LIGHT_CHROME: Chrome = Chrome {
    bg: "#ffffff",
    fg: "#1f2328",
    gutter: "#8c959f",
    caret: "#1f2328",
    selection: "#cfe3ff",
    status_bg: "#6a2c91",
    git_added: "#1a7f37",
    git_modified: "#9a6700",
    git_deleted: "#cf222e",
    panel_bg: "#f3f3f3",
    panel_border: "#d0d7de",
    input_bg: "#ffffff",
    hover_bg: "#e8eaed",
    muted: "#6e7781",
    accent: "#0969da",
};

const DARK_CHROME: Chrome = Chrome {
    bg: "#1e1e1e",
    fg: "#d4d4d4",
    gutter: "#858585",
    caret: "#d4d4d4",
    selection: "#264f78",
    status_bg: "#6a2c91",
    git_added: "#587c0c",
    git_modified: "#bb8009",
    git_deleted: "#94151b",
    panel_bg: "#252526",
    panel_border: "#454545",
    input_bg: "#3c3c3c",
    hover_bg: "#2a2d2e",
    muted: "#8a8a8a",
    accent: "#4daafc",
};

/// Light palette — purple keywords, blue functions, green strings, red markup.
const LIGHT_TOKENS: &[Token] = &[
    tok("keyword", "#8250df"),
    tok("string", "#098658"),
    tok_italic("comment", "#6e7781"),
    tok("function", "#0969da"),
    tok("type", "#0a7a6e"),
    tok("constructor", "#0a7a6e"),
    tok("number", "#0550ae"),
    tok("constant", "#0550ae"),
    tok("boolean", "#0550ae"),
    tok("operator", "#1f2328"),
    tok("variable", "#1f2328"),
    tok("parameter", "#1f2328"),
    tok("property", "#1f2328"),
    tok("attribute", "#0550ae"),
    tok("namespace", "#0a7a6e"),
    tok("module", "#0a7a6e"),
    tok("label", "#0550ae"),
    tok("tag", "#a31515"),
    tok("punctuation", "#1f2328"),
    tok("escape", "#cf6a00"),
    tok("embedded", "#1f2328"),
    tok_bold("title", "#a31515"),
    tok_underline("link", "#0a7a6e"),
    tok_italic("emphasis", "inherit"),
    tok_bold("strong", "inherit"),
];

/// Dark palette — the VSCode-dark colors the editor originally shipped.
const DARK_TOKENS: &[Token] = &[
    tok("keyword", "#569cd6"),
    tok("string", "#ce9178"),
    tok_italic("comment", "#6a9955"),
    tok("function", "#dcdcaa"),
    tok("type", "#4ec9b0"),
    tok("constructor", "#4ec9b0"),
    tok("number", "#b5cea8"),
    tok("constant", "#569cd6"),
    tok("boolean", "#569cd6"),
    tok("operator", "#d4d4d4"),
    tok("variable", "#9cdcfe"),
    tok("parameter", "#9cdcfe"),
    tok("property", "#9cdcfe"),
    tok("attribute", "#9cdcfe"),
    tok("namespace", "#4ec9b0"),
    tok("module", "#4ec9b0"),
    tok("label", "#c8c8c8"),
    tok("tag", "#569cd6"),
    tok("punctuation", "#d4d4d4"),
    tok("escape", "#d7ba7d"),
    tok("embedded", "#d4d4d4"),
    tok_bold("title", "#569cd6"),
    tok_underline("link", "#4ec9b0"),
    tok_italic("emphasis", "inherit"),
    tok_bold("strong", "inherit"),
];

/// Build the CSS injected into the editor page's `{{THEME_CSS}}` slot: a `:root`
/// block overriding the chrome color variables and one `.tok-*` rule per syntax
/// scope. Because this block follows the static defaults in `editor.html`, it
/// wins — the statics serve only as a fallback if injection is ever skipped.
pub fn editor_theme_css(theme: &ThemeConfig) -> String {
    let cfg = &theme.editor;
    let dark = matches!(
        cfg.preset.as_deref().map(str::trim).map(str::to_ascii_lowercase),
        Some(ref p) if p == "dark" || p == "ansi_dark"
    );

    let (base_chrome, base_tokens) = if dark {
        (&DARK_CHROME, DARK_TOKENS)
    } else {
        (&LIGHT_CHROME, LIGHT_TOKENS)
    };

    let mut css = String::new();
    css.push_str(":root {\n");
    push_var(&mut css, "--bg", &cfg.bg, base_chrome.bg);
    push_var(&mut css, "--fg", &cfg.fg, base_chrome.fg);
    push_var(&mut css, "--gutter", &cfg.gutter, base_chrome.gutter);
    push_var(&mut css, "--caret", &cfg.caret, base_chrome.caret);
    push_var(&mut css, "--sel", &cfg.selection, base_chrome.selection);
    push_var(
        &mut css,
        "--status-bg",
        &cfg.status_bg,
        base_chrome.status_bg,
    );
    push_var(
        &mut css,
        "--git-added",
        &cfg.git_added,
        base_chrome.git_added,
    );
    push_var(
        &mut css,
        "--git-modified",
        &cfg.git_modified,
        base_chrome.git_modified,
    );
    push_var(
        &mut css,
        "--git-deleted",
        &cfg.git_deleted,
        base_chrome.git_deleted,
    );
    // Panel chrome follows the preset (no per-key overrides — overrideable chrome
    // is kept to the surface colors users actually want to tweak).
    push_var(&mut css, "--panel-bg", &None, base_chrome.panel_bg);
    push_var(&mut css, "--panel-border", &None, base_chrome.panel_border);
    push_var(&mut css, "--input-bg", &None, base_chrome.input_bg);
    push_var(&mut css, "--hover-bg", &None, base_chrome.hover_bg);
    push_var(&mut css, "--muted", &None, base_chrome.muted);
    push_var(&mut css, "--accent", &None, base_chrome.accent);
    css.push_str("}\n");

    for token in base_tokens {
        push_token_rule(&mut css, token, cfg);
    }

    css
}

/// Emit a `--var: value;` line, preferring the user override when it resolves to
/// a valid CSS color, otherwise the palette default.
fn push_var(css: &mut String, name: &str, override_color: &Option<String>, default: &str) {
    let value = override_color
        .as_deref()
        .and_then(to_css_color)
        .unwrap_or_else(|| default.to_string());
    css.push_str("  ");
    css.push_str(name);
    css.push_str(": ");
    css.push_str(&value);
    css.push_str(";\n");
}

/// Emit a `.tok-<scope> { … }` rule, applying any `[theme.editor.tokens]`
/// override for that scope (color / bold / italic) on top of the palette value.
fn push_token_rule(css: &mut String, token: &Token, cfg: &ThemeEditorConfig) {
    let override_cfg = cfg.tokens.get(token.scope);

    let color = override_cfg
        .and_then(|o| o.fg.as_deref())
        .and_then(to_css_color)
        .unwrap_or_else(|| token.color.to_string());
    let bold = override_cfg.and_then(|o| o.bold).unwrap_or(token.bold);
    let italic = override_cfg.and_then(|o| o.italic).unwrap_or(token.italic);

    css.push_str(".tok-");
    css.push_str(token.scope);
    css.push_str(" { ");
    if color != "inherit" {
        css.push_str("color: ");
        css.push_str(&color);
        css.push_str("; ");
    }
    if bold {
        css.push_str("font-weight: 600; ");
    }
    if italic {
        css.push_str("font-style: italic; ");
    }
    if token.underline {
        css.push_str("text-decoration: underline; ");
    }
    css.push_str("}\n");
}

/// Resolve a user-supplied color to a CSS color string, or `None` if it can't be
/// interpreted. Accepts `#rgb`/`#rrggbb` hex, the crossterm-style snake_case
/// names used by the terminal theme (`dark_grey` → `#808080`), and otherwise
/// passes through plain alphanumeric tokens (any valid CSS named color works).
fn to_css_color(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        let ok = (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit());
        return ok.then(|| format!("#{hex}"));
    }
    let normalized = s.to_ascii_lowercase().replace('-', "_");
    let mapped = match normalized.as_str() {
        "black" => "#000000",
        "dark_grey" | "dark_gray" => "#808080",
        "grey" | "gray" => "#c0c0c0",
        "white" => "#ffffff",
        "red" => "#cd3131",
        "dark_red" => "#a31515",
        "green" => "#0a7d00",
        "dark_green" => "#067d00",
        "yellow" => "#b58900",
        "dark_yellow" => "#9a6700",
        "blue" => "#0969da",
        "dark_blue" => "#0033b3",
        "magenta" => "#a626a4",
        "dark_magenta" => "#8250df",
        "cyan" => "#0a7a6e",
        "dark_cyan" => "#067a6e",
        _ => {
            // Pass through anything that looks like a bare CSS keyword.
            if s.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some(s.to_string());
            }
            return None;
        }
    };
    Some(mapped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeCaptureConfig;
    use std::collections::HashMap;

    fn light() -> ThemeConfig {
        ThemeConfig::default()
    }

    #[test]
    fn default_is_light_palette() {
        let css = editor_theme_css(&light());
        assert!(css.contains("--bg: #ffffff;"));
        assert!(css.contains("--fg: #1f2328;"));
        // Light keyword color.
        assert!(css.contains(".tok-keyword { color: #8250df; }"));
    }

    #[test]
    fn dark_preset_selects_dark_palette() {
        let mut cfg = light();
        cfg.editor.preset = Some("dark".to_string());
        let css = editor_theme_css(&cfg);
        assert!(css.contains("--bg: #1e1e1e;"));
        assert!(css.contains(".tok-keyword { color: #569cd6; }"));
    }

    #[test]
    fn chrome_override_applies() {
        let mut cfg = light();
        cfg.editor.bg = Some("#101010".to_string());
        cfg.editor.selection = Some("dark_blue".to_string());
        let css = editor_theme_css(&cfg);
        assert!(css.contains("--bg: #101010;"));
        assert!(css.contains("--sel: #0033b3;"));
    }

    #[test]
    fn invalid_chrome_override_falls_back_to_default() {
        let mut cfg = light();
        cfg.editor.bg = Some("not a color!".to_string());
        let css = editor_theme_css(&cfg);
        assert!(css.contains("--bg: #ffffff;"));
    }

    #[test]
    fn token_override_changes_color_and_weight() {
        let mut cfg = light();
        let mut tokens = HashMap::new();
        tokens.insert(
            "keyword".to_string(),
            ThemeCaptureConfig {
                fg: Some("#ff0000".to_string()),
                bold: Some(true),
                italic: None,
            },
        );
        cfg.editor.tokens = tokens;
        let css = editor_theme_css(&cfg);
        assert!(css.contains(".tok-keyword { color: #ff0000; font-weight: 600; }"));
    }

    #[test]
    fn comment_keeps_italic_in_light_palette() {
        let css = editor_theme_css(&light());
        assert!(css.contains(".tok-comment { color: #6e7781; font-style: italic; }"));
    }

    #[test]
    fn emphasis_has_no_color_declaration() {
        let css = editor_theme_css(&light());
        // `inherit` sentinel means: keep surrounding fg, only toggle style.
        assert!(css.contains(".tok-emphasis { font-style: italic; }"));
    }

    #[test]
    fn short_hex_is_accepted() {
        assert_eq!(to_css_color("#abc"), Some("#abc".to_string()));
        assert_eq!(to_css_color("#abcdef"), Some("#abcdef".to_string()));
        assert_eq!(to_css_color("#xyz"), None);
    }
}

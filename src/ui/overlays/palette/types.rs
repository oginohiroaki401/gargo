use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Command,
    FileFinder,
    BufferPicker,
    JumpPicker,
    ReferencePicker,
    GitBranchPicker,
    GitBranchComparePicker,
    SymbolPicker,
    GlobalSearch,
    GotoLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateKind {
    Command(usize),
    Buffer(BufferId),
    Jump(usize),
    Reference(usize),
    GitBranch(usize),
    Symbol(usize),
    File(usize),
    SearchResult(usize),
}

pub struct ScoredCandidate {
    pub kind: CandidateKind,
    pub label: String,
    pub score: i32,
    pub match_positions: Vec<usize>,
    pub preview_lines: Vec<String>,
}

pub(super) struct PreviewRequest {
    pub(super) rel_path: String,
}

pub(super) struct PreviewResult {
    pub(super) rel_path: String,
    pub(super) lines: Vec<String>,
    pub(super) spans: HashMap<usize, Vec<HighlightSpan>>,
}

#[derive(Debug, Clone)]
pub(super) struct GlobalSearchResultEntry {
    pub(super) rel_path: String,
    pub(super) line: usize,
    pub(super) char_col: usize,
    pub(super) preview_lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct JumpEntry {
    pub(super) jump_index: usize,
    pub(super) label: String,
    pub(super) preview_lines: Vec<String>,
    pub(super) source_path: Option<String>,
    pub(super) target_preview_line: Option<usize>,
    pub(super) target_char_col: usize,
}

#[derive(Debug, Clone)]
pub struct JumpPickerEntry {
    pub jump_index: usize,
    pub label: String,
    pub preview_lines: Vec<String>,
    pub source_path: Option<String>,
    pub target_preview_line: Option<usize>,
    pub target_char_col: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ReferenceEntry {
    pub(super) label: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) character_utf16: usize,
    pub(super) preview_lines: Vec<String>,
    pub(super) source_path: Option<String>,
    pub(super) target_preview_line: Option<usize>,
    pub(super) target_char_col: usize,
}

#[derive(Debug, Clone)]
pub struct ReferencePickerEntry {
    pub label: String,
    pub path: PathBuf,
    pub line: usize,
    pub character_utf16: usize,
    pub preview_lines: Vec<String>,
    pub source_path: Option<String>,
    pub target_preview_line: Option<usize>,
    pub target_char_col: usize,
}

#[derive(Debug, Clone)]
pub(super) struct SymbolEntry {
    pub(super) label: String,
    pub(super) line: usize,
    pub(super) char_col: usize,
    pub(super) preview_lines: Vec<String>,
    pub(super) copy_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SmartCopyPickerEntry {
    pub label: String,
    pub line: usize,
    pub char_col: usize,
    pub preview_lines: Vec<String>,
    pub copy_text: String,
}

#[derive(Debug, Clone)]
pub(super) struct GitBranchEntry {
    pub(super) branch_name: String,
    pub(super) label: String,
    pub(super) preview_lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GitBranchPickerEntry {
    pub branch_name: String,
    pub label: String,
    pub preview_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SymbolSubmitBehavior {
    JumpToLocation,
    CopyToClipboard,
}

pub(super) struct GlobalSearchRequest {
    pub(super) query: String,
    pub(super) generation: u64,
}

pub(super) struct GlobalSearchBatch {
    pub(super) generation: u64,
    pub(super) results: Vec<GlobalSearchResultEntry>,
    pub(super) error: Option<String>,
}

pub(super) struct PreviewHorizontalWindow<'a> {
    pub(super) visible: &'a str,
    pub(super) start_byte: usize,
    pub(super) end_byte: usize,
    pub(super) start_col: usize,
    pub(super) used_width: usize,
}

pub(super) fn split_numbered_preview_line(line: &str) -> Option<(&str, &str)> {
    let (prefix, right) = line.split_once('|')?;
    let code = right.strip_prefix(' ').unwrap_or(right);
    Some((prefix, code))
}

pub(super) fn jump_marker_column(line: &str, target_char_col: usize) -> Option<(usize, usize)> {
    let (prefix, code) = split_numbered_preview_line(line)?;
    let prefix_display_width = display_width(prefix) + 2; // "| "
    let chars: Vec<char> = code.chars().collect();
    if chars.is_empty() {
        return Some((prefix_display_width, 1));
    }
    let clamped = target_char_col.min(chars.len().saturating_sub(1));
    let char_byte = code
        .char_indices()
        .nth(clamped)
        .map(|(idx, _)| idx)
        .unwrap_or(code.len());
    let code_display_width = display_width(&code[..char_byte]);
    let ch_width = UnicodeWidthChar::width(chars[clamped]).unwrap_or(1).max(1);
    Some((prefix_display_width + code_display_width, ch_width))
}

pub(super) fn slice_preview_display_window(
    display: &str,
    start_col: usize,
    max_width: usize,
) -> PreviewHorizontalWindow<'_> {
    let mut col = 0usize;
    let mut start_byte = display.len();
    let mut effective_start_col = 0usize;

    for (i, ch) in display.char_indices() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col >= start_col {
            start_byte = i;
            effective_start_col = col;
            break;
        }
        if col + ch_w > start_col {
            // Never render half of a wide character.
            col += ch_w;
            continue;
        }
        col += ch_w;
    }

    if start_byte == display.len() {
        effective_start_col = col;
    }

    if max_width == 0 || start_byte == display.len() {
        return PreviewHorizontalWindow {
            visible: "",
            start_byte,
            end_byte: start_byte,
            start_col: effective_start_col,
            used_width: 0,
        };
    }

    let mut used_width = 0usize;
    let mut end_byte = display.len();
    for (i, ch) in display[start_byte..].char_indices() {
        let ch_w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used_width + ch_w > max_width {
            end_byte = start_byte + i;
            break;
        }
        used_width += ch_w;
    }

    PreviewHorizontalWindow {
        visible: &display[start_byte..end_byte],
        start_byte,
        end_byte,
        start_col: effective_start_col,
        used_width,
    }
}

pub(super) fn command_display_label(
    entry: &crate::command::registry::CommandEntry,
    config: &Config,
) -> String {
    match entry.id.as_str() {
        "config.toggle_debug" => {
            if config.debug {
                "Hide Debug".to_string()
            } else {
                "Show Debug".to_string()
            }
        }
        "config.toggle_line_numbers" => {
            if config.show_line_number {
                "Hide Line Number".to_string()
            } else {
                "Show Line Number".to_string()
            }
        }
        _ => entry.label.clone(),
    }
}

pub(super) fn command_preview_lines(
    entry: &crate::command::registry::CommandEntry,
    display_label: &str,
) -> Vec<String> {
    let mut lines = vec![format!("Command: {}", display_label)];
    if let Some(category) = &entry.category {
        lines.push(format!("Category: {}", category));
    }
    lines.push(format!("ID: {}", entry.id));

    if entry.id == "core.copy_gargo_version" {
        lines.push(String::new());
        lines.push("Version Preview:".to_string());
        lines.push(crate::command::registry::gargo_version_info());
        lines.push(
            "Gargo is a Rust terminal text editor with modal editing, multi-buffer, and Tree-sitter highlighting."
                .to_string(),
        );
        lines.push("Executes: copy version info to system clipboard.".to_string());
    }

    lines
}

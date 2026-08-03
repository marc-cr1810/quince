//! Document formatting provider.

use lsp_types::{DocumentFormattingParams, Position, Range, TextEdit};
use crate::lsp::DocumentState;

pub(crate) fn format_document(
    state: Option<&DocumentState>,
    _params: DocumentFormattingParams,
) -> Vec<TextEdit> {
    let state = match state {
        Some(s) => s,
        None => return Vec::new(),
    };

    let source = state.text();
    let mut formatted_lines = Vec::new();
    let mut indent_level: usize = 0;


    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            formatted_lines.push(String::new());
            continue;
        }

        if trimmed.starts_with('}') {
            indent_level = indent_level.saturating_sub(1);
        }

        let indent = "    ".repeat(indent_level);
        formatted_lines.push(format!("{indent}{trimmed}"));

        if trimmed.ends_with('{') {
            indent_level += 1;
        }
    }

    let mut new_text = formatted_lines.join("\n");
    if source.ends_with('\n') && !new_text.ends_with('\n') {
        new_text.push('\n');
    }
    if new_text == source {
        return Vec::new();
    }


    let total_lines = source.lines().count() as u32;
    let last_line_len = source.lines().last().map_or(0, |l| l.len() as u32);

    vec![TextEdit {
        range: Range {
            start: Position { line: 0, character: 0 },
            end: Position {
                line: total_lines,
                character: last_line_len,
            },
        },
        new_text,
    }]
}

//! Offsets and positions.
//!
//! LSP counts UTF-16 code units from a line start; a `Span` counts bytes from the
//! file start. Every conversion between the two is here so there is one place to
//! be wrong.


use lsp_types::{Position, Range};

use quince::syntax::token::Span;

pub(crate) fn get_word_at_position(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    if col > line.len() {
        return None;
    }

    let is_ident_char = |c: char| c == '_' || c.is_alphanumeric();

    let start = line[..col].rfind(|c| !is_ident_char(c)).map_or(0, |i| i + 1);
    let end = line[col..]
        .find(|c| !is_ident_char(c))
        .map_or(line.len(), |i| col + i);

    if start < end {
        Some(line[start..end].to_string())
    } else {
        None
    }
}

/// Converts Quince byte-offset Span into an LSP Range (0-indexed line and character).
pub(crate) fn span_to_range(source: &str, span: Span) -> Range {
    let start = offset_to_position(source, span.start as usize);
    let end = offset_to_position(source, span.end as usize);
    if start == end {
        let next_char = offset_to_position(source, (span.end as usize + 1).min(source.len()));
        Range { start, end: next_char }
    } else {
        Range { start, end }
    }
}

/// The byte offset an LSP position names, which is what a `Span` is measured in.
pub(crate) fn position_to_offset(source: &str, pos: Position) -> u32 {
    let mut offset = 0usize;
    for (index, line) in source.split('\n').enumerate() {
        if index == pos.line as usize {
            let col = line
                .char_indices()
                .nth(pos.character as usize)
                .map_or(line.len(), |(index, _)| index);
            return (offset + col) as u32;
        }
        offset += line.len() + 1;
    }
    source.len() as u32
}

pub(crate) fn offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let line = source[..offset].matches('\n').count() as u32;
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = source[line_start..offset].chars().count() as u32;
    Position {
        line,
        character: col,
    }
}

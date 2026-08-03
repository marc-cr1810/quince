//! Go-to-definition, find references, symbol rename, and document/workspace outlines.

use std::collections::HashMap;
use lsp_types::{
    GotoDefinitionResponse, Location, Position,
    Range, SymbolInformation, SymbolKind, Url,
    WorkspaceEdit, TextEdit,
};

use quince::syntax::ast::{Stmt, StmtKind};
use quince::syntax::token::Span;
use crate::lsp::DocumentState;
use crate::lsp::position::{get_word_at_position, offset_to_position, span_to_range};

pub(crate) fn get_definition(
    uri: &Url,
    state: Option<&DocumentState>,
    documents: &HashMap<Url, DocumentState>,
    pos: Position,
) -> Option<GotoDefinitionResponse> {
    let state = state?;
    let word = get_word_at_position(&state.text, pos)?;

    // Search current file's AST first
    if let Some(ast) = state.ast.as_ref()
        && let Some(span) = find_decl_span(ast, &word)
    {
        let range = span_to_range(&state.text, span);
        let location = Location {
            uri: uri.clone(),
            range,
        };
        return Some(GotoDefinitionResponse::Scalar(location));
    }

    // Search other open documents in the workspace (for imported definitions)
    for (doc_uri, doc_state) in documents {
        if doc_uri == uri {
            continue;
        }
        if let Some(ast) = doc_state.ast.as_ref()
            && let Some(span) = find_decl_span(ast, &word)
        {
            let range = span_to_range(&doc_state.text, span);
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: doc_uri.clone(),
                range,
            }));
        }
    }

    None
}

pub(crate) fn find_decl_span(stmts: &[Stmt], target: &str) -> Option<Span> {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } if decl.name == target => return Some(stmt.span),
            StmtKind::Class { name, .. } if name == target => return Some(stmt.span),
            StmtKind::Let { name, .. } if name == target => return Some(stmt.span),
            StmtKind::Class { methods, .. } => {
                for m in methods {
                    if m.name == target {
                        return Some(m.body.span);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn find_name_range(source: &str, span: Span, name: &str) -> Option<Range> {
    let start_idx = (span.start as usize).min(source.len());
    let end_idx = (span.end as usize).min(source.len());
    if start_idx >= end_idx {
        return None;
    }
    let text = &source[start_idx..end_idx];
    let rel_offset = text.find(name)?;
    let abs_start = start_idx + rel_offset;
    let abs_end = abs_start + name.len();
    Some(Range {
        start: offset_to_position(source, abs_start),
        end: offset_to_position(source, abs_end),
    })
}

pub(crate) fn get_references(uri: &Url, state: Option<&DocumentState>, pos: Position) -> Vec<Location> {
    let state = match state {
        Some(s) => s,
        None => return Vec::new(),
    };
    let word = match get_word_at_position(&state.text, pos) {
        Some(w) => w,
        None => return Vec::new(),
    };

    let mut locations = Vec::new();
    let text = &state.text;
    let len = word.len();

    for (line_idx, line) in text.lines().enumerate() {
        let mut start_col = 0;
        while let Some(found_idx) = line[start_col..].find(&word) {
            let col = start_col + found_idx;
            let prev_char = if col > 0 { line[..col].chars().last() } else { None };
            let next_char = line[col + len..].chars().next();

            let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
            let left_ok = prev_char.map_or(true, |c| !is_ident_char(c));
            let right_ok = next_char.map_or(true, |c| !is_ident_char(c));

            if left_ok && right_ok {
                locations.push(Location {
                    uri: uri.clone(),
                    range: Range {
                        start: Position { line: line_idx as u32, character: col as u32 },
                        end: Position { line: line_idx as u32, character: (col + len) as u32 },
                    },
                });
            }
            start_col = col + len.max(1);
        }
    }

    locations
}

pub(crate) fn rename_symbol(
    uri: &Url,
    state: Option<&DocumentState>,
    pos: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let refs = get_references(uri, state, pos);
    if refs.is_empty() {
        return None;
    }
    let edits: Vec<TextEdit> = refs
        .into_iter()
        .map(|loc| TextEdit {
            range: loc.range,
            new_text: new_name.to_string(),
        })
        .collect();

    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);

    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

pub(crate) fn get_document_symbols(uri: &Url, state: Option<&DocumentState>) -> Vec<SymbolInformation> {
    let mut symbols = Vec::new();
    let state = match state {
        Some(s) => s,
        None => return symbols,
    };
    let ast = match &state.ast {
        Some(a) => a,
        None => return symbols,
    };

    for stmt in ast {
        let stmt_range = span_to_range(&state.text, stmt.span);
        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                #[allow(deprecated)]
                symbols.push(SymbolInformation {
                    name: decl.name.clone(),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: stmt_range,
                    },
                    container_name: None,
                });
            }
            StmtKind::Class { name, methods, .. } => {
                #[allow(deprecated)]
                symbols.push(SymbolInformation {
                    name: name.clone(),
                    kind: SymbolKind::CLASS,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: stmt_range,
                    },
                    container_name: None,
                });

                for m in methods {
                    let m_range = span_to_range(&state.text, m.body.span);
                    #[allow(deprecated)]
                    symbols.push(SymbolInformation {
                        name: m.name.clone(),
                        kind: SymbolKind::METHOD,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: uri.clone(),
                            range: m_range,
                        },
                        container_name: Some(name.clone()),
                    });
                }
            }
            StmtKind::Let { name, .. } => {
                #[allow(deprecated)]
                symbols.push(SymbolInformation {
                    name: name.clone(),
                    kind: SymbolKind::VARIABLE,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: stmt_range,
                    },
                    container_name: None,
                });
            }
            _ => {}
        }
    }

    symbols
}

pub(crate) fn get_workspace_symbols(
    documents: &HashMap<Url, DocumentState>,
    query: &str,
) -> Vec<SymbolInformation> {
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    for (uri, state) in documents {
        let syms = get_document_symbols(uri, Some(state));
        for sym in syms {
            if query.is_empty() || sym.name.to_lowercase().contains(&query_lower) {
                results.push(sym);
            }
        }
    }

    results
}



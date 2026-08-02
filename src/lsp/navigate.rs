//! Go-to-definition, and the document outline.


use lsp_types::{
    GotoDefinitionResponse, Location, Position,
    Range, SymbolInformation,
    SymbolKind, Url,
};

use quince::syntax::ast::{Stmt, StmtKind};
use quince::syntax::token::Span;
use crate::lsp::DocumentState;
use crate::lsp::position::{get_word_at_position, offset_to_position, span_to_range};

pub(crate) fn get_definition(uri: &Url, state: Option<&DocumentState>, pos: Position) -> Option<GotoDefinitionResponse> {
    let state = state?;
    let word = get_word_at_position(&state.text, pos)?;
    let ast = state.ast.as_ref()?;

    if let Some(span) = find_decl_span(ast, &word) {
        let range = span_to_range(&state.text, span);
        let location = Location {
            uri: uri.clone(),
            range,
        };
        return Some(GotoDefinitionResponse::Scalar(location));
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


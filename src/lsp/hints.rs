//! Inlay hints: what the pass worked out and the program did not say.
//!
//! `let x` ⟨`: int`⟩ `= 8`. Only where no annotation was written — a hint over a
//! type the program already stated is noise, and v0.7 §6 says so explicitly.
//!
//! Only where the pass is *certain*, too. `Unknown` is most of a dynamically
//! typed program, and a hint reading `: _` on every other line would be an
//! editor filling the margin with the news that it does not know.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Range};

use quince::sema::infer::Types;
use quince::sema::types::Type;
use quince::syntax::ast::{Block, Stmt, StmtKind};

use crate::lsp::DocumentState;
use crate::lsp::position::offset_to_position;

/// Every hint for the part of the document `range` covers.
///
/// The editor asks per visible range rather than per document, so a long file
/// costs what is on screen.
pub(crate) fn get_inlay_hints(state: Option<&DocumentState>, range: Range) -> Vec<InlayHint> {
    let Some(state) = state else {
        return Vec::new();
    };
    let (Some(ast), Some(types)) = (state.ast(), state.types()) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    stmts(ast, types, &mut found);

    found
        .into_iter()
        .filter_map(|(at, label)| {
            let position = offset_to_position(state.text(), at as usize);
            // The editor asked about a window; anything outside it is work it
            // did not ask for.
            (position.line >= range.start.line && position.line <= range.end.line).then_some(
                InlayHint {
                    position,
                    label: InlayHintLabel::String(label),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    // Rendered tight against the name it describes, as a written
                    // annotation would be.
                    padding_left: Some(false),
                    padding_right: Some(false),
                    data: None,
                },
            )
        })
        .collect()
}

fn stmts(stmts: &[Stmt], types: &Types, found: &mut Vec<(u32, String)>) {
    for stmt in stmts {
        one(stmt, types, found);
    }
}

fn one(stmt: &Stmt, types: &Types, found: &mut Vec<(u32, String)>) {
    match &stmt.kind {
        StmtKind::Let {
            name_span,
            ty,
            value,
            ..
        } => {
            // The program said it. Repeating it back is the noise §6 rules out.
            if ty.is_some() {
                return;
            }
            if let Some(label) = hint(&types.of_expr(value.span)) {
                found.push((name_span.end, label));
            }
        }
        StmtKind::Class { methods, fields, .. } => {
            for field in fields {
                if field.ty.is_some() {
                    continue;
                }
                if let Some(label) = hint(&types.of_expr(field.value.span)) {
                    found.push((field.name_span.end, label));
                }
            }
            for decl in methods {
                stmts(&decl.body.stmts, types, found);
            }
        }
        StmtKind::Fn { decl, .. } => stmts(&decl.body.stmts, types, found),
        StmtKind::Extend { methods, .. } => {
            for decl in methods {
                stmts(&decl.body.stmts, types, found);
            }
        }
        StmtKind::If { then, otherwise, .. } => {
            block(then, types, found);
            if let Some(other) = otherwise {
                one(other, types, found);
            }
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => block(body, types, found),
        StmtKind::Try { body, handler, .. } => {
            block(body, types, found);
            block(handler, types, found);
        }
        StmtKind::Block(inner) => block(inner, types, found),
        StmtKind::Expr(_)
        | StmtKind::Return(_)
        | StmtKind::Throw(_)
        | StmtKind::Alias { .. }
        | StmtKind::Import { .. } => {}
    }
}

fn block(block: &Block, types: &Types, found: &mut Vec<(u32, String)>) {
    stmts(&block.stmts, types, found);
}

/// The text of a hint, or `None` where there is nothing worth saying.
fn hint(ty: &Type) -> Option<String> {
    match ty {
        // The pass has not been told and could not work it out. An editor
        // filling the margin with that is an editor saying nothing, loudly.
        Type::Unknown => None,
        // A module is bound by `import`, which names it on the same line.
        Type::Module(_) => None,
        _ => Some(format!(": {ty}")),
    }
}

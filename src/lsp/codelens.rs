//! CodeLens provider.

use lsp_types::{Command, CodeLens, Position, Uri as Url};
use quince::syntax::ast::StmtKind;
use crate::lsp::DocumentState;
use crate::lsp::navigate::{find_name_range, get_references};
use crate::lsp::position::span_to_range;

pub(crate) fn get_code_lenses(
    uri: &Url,
    state: Option<&DocumentState>,
) -> Vec<CodeLens> {
    let mut lenses = Vec::new();
    let Some(state) = state else {
        return lenses;
    };
    let Some(ast) = state.ast() else {
        return lenses;
    };

    for stmt in ast {
        let name_info = match &stmt.kind {
            StmtKind::Fn { decl, .. } => Some((decl.name.clone(), stmt.span)),
            StmtKind::Class { name, .. } => Some((name.clone(), stmt.span)),
            _ => None,
        };

        if let Some((name, span)) = name_info {
            let stmt_range = span_to_range(&state.text, span);
            let name_range = find_name_range(&state.text, span, &name).unwrap_or(stmt_range);
            let pos = Position {
                line: name_range.start.line,
                character: name_range.start.character,
            };

            let refs = get_references(uri, Some(state), pos);
            let ref_count = refs.iter().filter(|loc| loc.range != name_range).count();

            let title = if ref_count == 1 {
                "1 reference".to_string()
            } else {
                format!("{ref_count} references")
            };

            lenses.push(CodeLens {
                range: name_range,
                command: Some(Command {
                    title,
                    command: "editor.action.showReferences".to_string(),
                    arguments: None,
                }),
                data: None,
            });
        }
    }

    lenses
}

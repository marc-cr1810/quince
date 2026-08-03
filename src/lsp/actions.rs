//! Code action provider (quick fixes).

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    Range, TextEdit, WorkspaceEdit,
};
use std::collections::HashMap;

use crate::lsp::DocumentState;

pub(crate) fn get_code_actions(
    _state: Option<&DocumentState>,
    params: CodeActionParams,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    if _state.is_none() {
        return actions;
    }


    for diagnostic in &params.context.diagnostics {
        // Quick fix for missing type annotation or type mismatch suggestion
        if diagnostic.message.contains("write") && diagnostic.message.contains("if it may be absent") {
            // Suggest adding '?' to make type nullable
            let action = CodeAction {
                title: "Make type nullable with '?'".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some({
                        let mut changes = HashMap::new();
                        changes.insert(
                            params.text_document.uri.clone(),
                            vec![TextEdit {
                                range: Range {
                                    start: diagnostic.range.end,
                                    end: diagnostic.range.end,
                                },
                                new_text: "?".to_string(),
                            }],
                        );
                        changes
                    }),
                    document_changes: None,
                    change_annotations: None,
                }),
                is_preferred: Some(true),
                disabled: None,
                data: None,
                command: None,
            };
            actions.push(CodeActionOrCommand::CodeAction(action));
        }
    }

    actions
}

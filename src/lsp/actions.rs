//! Code action provider (quick fixes).

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    Position, Range, TextEdit, WorkspaceEdit,
};
use std::collections::HashMap;

use quince::builtins::stdlib::MODULES;
use crate::lsp::DocumentState;
use crate::lsp::position::{get_word_at_position, position_to_offset};

pub(crate) fn get_code_actions(
    state: Option<&DocumentState>,
    params: CodeActionParams,
) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    let Some(state) = state else {
        return actions;
    };

    // 1. Diagnostic-triggered Quick Fixes
    for diagnostic in &params.context.diagnostics {
        // Quick fix: Suggest adding '?' for nullable types
        if diagnostic.message.contains("write") && diagnostic.message.contains("if it may be absent") {
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

        // Quick fix: Offer auto-import if unknown name matches a stdlib module
        for module in MODULES {
            if diagnostic.message.contains(module.name) && !state.text().contains(&format!("import {}", module.name)) {
                let action = CodeAction {
                    title: format!("Import module '{}'", module.name),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![diagnostic.clone()]),
                    edit: Some(WorkspaceEdit {
                        changes: Some({
                            let mut changes = HashMap::new();
                            changes.insert(
                                params.text_document.uri.clone(),
                                vec![TextEdit {
                                    range: Range {
                                        start: Position { line: 0, character: 0 },
                                        end: Position { line: 0, character: 0 },
                                    },
                                    new_text: format!("import {}\n", module.name),
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
    }

    // 2. Selection/Position-based Code Actions (e.g. Add type annotation hint)
    if let Some(word) = get_word_at_position(state.text(), params.range.start) {
        let offset = position_to_offset(state.text(), params.range.start);
        let inferred = state.type_of(&word, offset);
        if inferred.is_known() {
            let type_str = format!("{inferred}");
            let action = CodeAction {
                title: format!("Add type annotation ': {type_str}'"),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: None,
                edit: Some(WorkspaceEdit {
                    changes: Some({
                        let mut changes = HashMap::new();
                        changes.insert(
                            params.text_document.uri.clone(),
                            vec![TextEdit {
                                range: Range {
                                    start: Position {
                                        line: params.range.start.line,
                                        character: params.range.start.character + word.len() as u32,
                                    },
                                    end: Position {
                                        line: params.range.start.line,
                                        character: params.range.start.character + word.len() as u32,
                                    },
                                },
                                new_text: format!(": {type_str}"),
                            }],
                        );
                        changes
                    }),
                    document_changes: None,
                    change_annotations: None,
                }),
                is_preferred: Some(false),
                disabled: None,
                data: None,
                command: None,
            };
            actions.push(CodeActionOrCommand::CodeAction(action));
        }
    }

    actions
}

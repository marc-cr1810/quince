//! The language server.

pub mod actions;
pub mod codelens;
pub mod completion;
pub mod diagnostics;
pub mod folding;
pub mod format;
pub mod highlight;
pub mod hints;
pub mod hover;
pub mod navigate;
pub mod position;
pub mod selection;
pub mod semantic;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},
    request::{
        CodeActionRequest, CodeLensRequest, Completion, DocumentHighlightRequest, FoldingRangeRequest,
        Formatting, GotoDefinition, HoverRequest, InlayHintRequest, References, Rename, Request as _,
        SelectionRangeRequest, SignatureHelpRequest, WorkspaceSymbolRequest,
    },
    CodeActionParams, CodeActionProviderCapability, CodeActionResponse, CodeLensOptions, CodeLensParams,
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentHighlightParams, DocumentSymbolParams, DocumentSymbolResponse,
    FoldingRangeParams, FoldingRangeProviderCapability, GotoDefinitionParams, HoverParams,
    HoverProviderCapability, OneOf, PublishDiagnosticsParams, ReferenceParams, RenameParams,
    SelectionRangeParams, SelectionRangeProviderCapability, SemanticTokenModifier, SemanticTokenType,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelpOptions,
    SignatureHelpParams, TextDocumentSyncCapability, TextDocumentSyncKind, Url, WorkspaceSymbolParams,
};

use quince::sema::infer::{self, Types};
use quince::sema::symbols::{Kind, Symbol};
use quince::sema::types::Type;
use quince::syntax::ast::Stmt;
use crate::cursor::{path_ending_at, trailing_literal_type};
use crate::lsp::actions::get_code_actions;
use crate::lsp::codelens::get_code_lenses;
use crate::lsp::completion::get_completions;
use crate::lsp::diagnostics::publish_diagnostics;
use crate::lsp::folding::get_folding_ranges;
use crate::lsp::format::format_document;
use crate::lsp::highlight::get_document_highlights;
use crate::lsp::hints::get_inlay_hints;
use crate::lsp::hover::{get_hover, get_signature_help};
use crate::lsp::navigate::{
    get_definition, get_hierarchical_document_symbols, get_references, get_workspace_symbols, rename_symbol,
};
use crate::lsp::position::position_to_offset;
use crate::lsp::selection::get_selection_ranges;
use crate::lsp::semantic::get_semantic_tokens;

pub(crate) struct DocumentState {
    text: String,
    ast: Option<Vec<Stmt>>,
    types: Option<Types>,
}

impl DocumentState {
    pub(crate) fn new(text: String, previous: Option<DocumentState>) -> DocumentState {
        let (stmts, errors) = quince::compile_recovering(&text);
        let ast = if !stmts.is_empty() { Some(stmts) } else { None };
        let types = match (&ast, previous) {
            (Some(ast), Some(prev)) => {
                let inferred = infer::infer(ast);
                if !errors.is_empty() && prev.types.is_some() {
                    prev.types
                } else {
                    Some(inferred)
                }
            }
            (Some(ast), None) => Some(infer::infer(ast)),
            (None, Some(prev)) => prev.types,
            (None, None) => None,
        };
        DocumentState { text, ast, types }
    }


    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn ast(&self) -> Option<&[Stmt]> {
        self.ast.as_deref()
    }

    pub(crate) fn types(&self) -> Option<&Types> {
        self.types.as_ref()
    }

    pub(crate) fn members_before(&self, before: &str, offset: u32) -> Vec<Symbol> {
        let on_class_object = path_ending_at(before).is_some_and(|path| {
            self.types
                .as_ref()
                .is_some_and(|types| types.names_a_class(&path, offset))
        });

        match self.type_of(before, offset) {
            Type::Class(class) => match &self.types {
                Some(types) => {
                    let inside = types.class_at(offset);
                    types
                        .members_of(&class.name)
                        .into_iter()
                        .filter(|symbol| !(on_class_object && symbol.kind == Kind::Field))
                        .filter(|symbol| types.may_offer(symbol.visibility, &class.name, inside))
                        .collect()
                }
                None => Vec::new(),
            },
            Type::Module(module) => quince::sema::symbols::module_symbols(&module),
            Type::Unknown => Vec::new(),
        }
    }

    pub(crate) fn type_of(&self, before: &str, offset: u32) -> Type {
        if let Some(path) = path_ending_at(before)
            && let Some(types) = &self.types
        {
            let found = types.of_path(&path, offset);
            if found.is_known() {
                return found;
            }
        }
        trailing_literal_type(before)
    }
}

pub(crate) const LEGEND_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::CLASS,     // 0
    SemanticTokenType::FUNCTION,  // 1
    SemanticTokenType::METHOD,    // 2
    SemanticTokenType::VARIABLE,  // 3
    SemanticTokenType::PARAMETER, // 4
    SemanticTokenType::PROPERTY,  // 5
    SemanticTokenType::TYPE,      // 6
    SemanticTokenType::KEYWORD,   // 7
];

pub fn run_lsp_server() -> anyhow::Result<()> {
    let (connection, io_threads) = Connection::stdio();

    let server_capabilities = serde_json::to_value(&ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        completion_provider: Some(CompletionOptions {
            resolve_provider: Some(false),
            trigger_characters: Some(vec![".".to_string()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: Default::default(),
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        code_lens_provider: Some(CodeLensOptions {
            resolve_provider: Some(false),
        }),

        inlay_hint_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: LEGEND_TYPES.to_vec(),
                    token_modifiers: vec![
                        SemanticTokenModifier::DECLARATION,
                        SemanticTokenModifier::DEFAULT_LIBRARY,
                    ],
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                range: Some(false),
                work_done_progress_options: Default::default(),
            }),
        ),
        ..Default::default()
    })?;

    connection.initialize(server_capabilities)?;

    let mut documents: HashMap<Url, DocumentState> = HashMap::new();

    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(());
                }
                if let Err(err) = handle_request(&connection, &documents, req) {
                    eprintln!("Error handling LSP request: {err}");
                }
            }
            Message::Notification(notif) => {
                if let Err(err) = handle_notification(&connection, &mut documents, notif) {
                    eprintln!("Error handling LSP notification: {err}");
                }
            }
            Message::Response(_) => {}
        }
    }

    io_threads.join()?;
    Ok(())
}


pub(crate) fn handle_request(
    connection: &Connection,
    documents: &HashMap<Url, DocumentState>,
    req: Request,
) -> anyhow::Result<()> {
    let id = req.id.clone();
    match req.method.as_str() {
        Completion::METHOD => {
            let params: CompletionParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            let items = get_completions(documents.get(uri), pos);
            let resp = Response::new_ok(id, CompletionResponse::Array(items));
            connection.sender.send(Message::Response(resp))?;
        }
        InlayHintRequest::METHOD => {
            let params: lsp_types::InlayHintParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let hints = get_inlay_hints(documents.get(uri), params.range);
            let resp = Response::new_ok(id, hints);
            connection.sender.send(Message::Response(resp))?;
        }
        HoverRequest::METHOD => {
            let params: HoverParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let hover = get_hover(documents.get(uri), pos);
            let resp = Response::new_ok(id, hover);
            connection.sender.send(Message::Response(resp))?;
        }
        SignatureHelpRequest::METHOD => {
            let params: SignatureHelpParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let help = get_signature_help(documents.get(uri), pos);
            let resp = Response::new_ok(id, help);
            connection.sender.send(Message::Response(resp))?;
        }
        GotoDefinition::METHOD => {
            let params: GotoDefinitionParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let location = get_definition(uri, documents.get(uri), documents, pos);
            let resp = Response::new_ok(id, location);
            connection.sender.send(Message::Response(resp))?;
        }
        References::METHOD => {
            let params: ReferenceParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            let locations = get_references(uri, documents.get(uri), pos);
            let resp = Response::new_ok(id, locations);
            connection.sender.send(Message::Response(resp))?;
        }
        Rename::METHOD => {
            let params: RenameParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position.text_document.uri;
            let pos = params.text_document_position.position;
            let edit = rename_symbol(uri, documents.get(uri), pos, &params.new_name);
            let resp = Response::new_ok(id, edit);
            connection.sender.send(Message::Response(resp))?;
        }
        WorkspaceSymbolRequest::METHOD => {
            let params: WorkspaceSymbolParams = serde_json::from_value(req.params)?;
            let symbols = get_workspace_symbols(documents, &params.query);
            let resp = Response::new_ok(id, symbols);
            connection.sender.send(Message::Response(resp))?;
        }
        CodeActionRequest::METHOD => {
            let params: CodeActionParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let actions = get_code_actions(documents.get(uri), params);
            let resp = Response::new_ok(id, CodeActionResponse::from(actions));
            connection.sender.send(Message::Response(resp))?;
        }
        Formatting::METHOD => {
            let params: DocumentFormattingParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let edits = format_document(documents.get(uri), params);
            let resp = Response::new_ok(id, edits);
            connection.sender.send(Message::Response(resp))?;
        }
        lsp_types::request::DocumentSymbolRequest::METHOD => {
            let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let symbols = get_hierarchical_document_symbols(uri, documents.get(uri));
            let resp = Response::new_ok(id, DocumentSymbolResponse::Nested(symbols));
            connection.sender.send(Message::Response(resp))?;
        }
        FoldingRangeRequest::METHOD => {
            let params: FoldingRangeParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let ranges = get_folding_ranges(documents.get(uri));
            let resp = Response::new_ok(id, ranges);
            connection.sender.send(Message::Response(resp))?;
        }
        SelectionRangeRequest::METHOD => {
            let params: SelectionRangeParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let ranges = get_selection_ranges(documents.get(uri), params.positions);
            let resp = Response::new_ok(id, ranges);
            connection.sender.send(Message::Response(resp))?;
        }
        DocumentHighlightRequest::METHOD => {
            let params: DocumentHighlightParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document_position_params.text_document.uri;
            let pos = params.text_document_position_params.position;
            let highlights = get_document_highlights(uri, documents.get(uri), pos);
            let resp = Response::new_ok(id, highlights);
            connection.sender.send(Message::Response(resp))?;
        }
        CodeLensRequest::METHOD => {
            let params: CodeLensParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let lenses = get_code_lenses(uri, documents.get(uri));
            let resp = Response::new_ok(id, lenses);
            connection.sender.send(Message::Response(resp))?;
        }
        "textDocument/semanticTokens/full" => {
            let params: SemanticTokensParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let tokens = get_semantic_tokens(documents.get(uri));
            let resp = Response::new_ok(id, SemanticTokensResult::Tokens(tokens));
            connection.sender.send(Message::Response(resp))?;
        }
        _ => {
            let resp = Response::new_ok(id, serde_json::Value::Null);
            connection.sender.send(Message::Response(resp))?;
        }
    }
    Ok(())
}

pub(crate) fn handle_notification(
    connection: &Connection,
    documents: &mut HashMap<Url, DocumentState>,
    notif: Notification,
) -> anyhow::Result<()> {
    match notif.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri;
            let text = params.text_document.text;
            let previous = documents.remove(&uri);
            documents.insert(uri.clone(), DocumentState::new(text.clone(), previous));
            publish_diagnostics(connection, uri, &text)?;
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri;

            let current_text = documents.get(&uri).map(|d| d.text.clone()).unwrap_or_default();
            let mut updated_text = current_text;

            for change in params.content_changes {
                if let Some(range) = change.range {
                    let start_offset = position_to_offset(&updated_text, range.start) as usize;
                    let end_offset = position_to_offset(&updated_text, range.end) as usize;
                    if start_offset <= end_offset && end_offset <= updated_text.len() {
                        updated_text.replace_range(start_offset..end_offset, &change.text);
                    }
                } else {
                    updated_text = change.text;
                }
            }

            let previous = documents.remove(&uri);
            let state = DocumentState::new(updated_text.clone(), previous);
            documents.insert(uri.clone(), state);
            publish_diagnostics(connection, uri, &updated_text)?;
        }
        "textDocument/didClose" => {
            let params: lsp_types::DidCloseTextDocumentParams = serde_json::from_value(notif.params)?;
            documents.remove(&params.text_document.uri);
            let notif = Notification {
                method: PublishDiagnostics::METHOD.to_string(),
                params: serde_json::to_value(PublishDiagnosticsParams {
                    uri: params.text_document.uri,
                    diagnostics: Vec::new(),
                    version: None,
                })?,
            };
            connection.sender.send(Message::Notification(notif))?;
        }
        _ => {}
    }
    Ok(())
}

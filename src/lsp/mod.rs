//! The language server.
//!
//! One file per request the editor makes, because that is how the protocol is
//! shaped and how the work arrives: v0.7 adds inlay hints (a new file), type
//! completion after `:` (an arm in [`completion`]), and visibility- and
//! smart-cast-aware filtering (two more).
//!
//! This file is the loop, the document cache, and the dispatch. Everything it
//! dispatches to reads [`Types`] — the inference pass — rather than the source
//! text, and [`crate::cursor`] is the one place either surface still touches raw
//! characters.

pub mod completion;
pub mod diagnostics;
pub mod hover;
pub mod navigate;
pub mod position;
pub mod semantic;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},
    request::{Completion, GotoDefinition, HoverRequest, Request as _, SignatureHelpRequest}, CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, HoverParams, HoverProviderCapability, OneOf, PublishDiagnosticsParams, SemanticTokenModifier, SemanticTokenType,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelpOptions, SignatureHelpParams, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use quince::sema::infer::{Types, self};
use quince::sema::symbols::{Kind, Symbol};
use quince::sema::types::{Type};
use quince::syntax::ast::Stmt;
use crate::cursor::{path_ending_at, trailing_literal_type};
use crate::lsp::completion::get_completions;
use crate::lsp::diagnostics::publish_diagnostics;
use crate::lsp::hover::{get_hover, get_signature_help};
use crate::lsp::navigate::{get_definition, get_document_symbols};
use crate::lsp::semantic::get_semantic_tokens;

pub(crate) struct DocumentState {
    text: String,
    ast: Option<Vec<Stmt>>,
    /// What the inference pass last made of this document.
    ///
    /// Held beside the tree rather than computed per request, because every
    /// request over one document would compute the same thing and the answer
    /// only changes when the text does.
    ///
    /// *Last*, not *current*, and that word is doing the work. Typing the `.`
    /// in `p.` is what makes a document stop parsing, so the moment the pass is
    /// most wanted is the moment there is no tree to run it over. Keeping the
    /// previous answer is what makes it useful at all: the text before the
    /// cursor is unchanged, so the offsets the lookup cares about still point
    /// where they did, and a scope that contained the cursor still contains it.
    /// What goes stale is everything after the edit, which is not what anyone
    /// is asking about.
    types: Option<Types>,
}

impl DocumentState {
    /// Reads a document, keeping what was known about the one it replaces.
    pub(crate) fn new(text: String, previous: Option<DocumentState>) -> DocumentState {
        let ast = parse_ast_lenient(&text);
        let types = match &ast {
            Some(ast) => Some(infer::infer(ast)),
            None => previous.and_then(|state| state.types),
        };
        DocumentState { text, ast, types }
    }

    /// Everything reachable through a dot on whatever `before` evaluates to.
    pub(crate) fn members_before(&self, before: &str, offset: u32) -> Vec<Symbol> {
        // A class object gets methods and no fields — see `names_a_class`.
        let on_class_object = path_ending_at(before).is_some_and(|path| {
            self.types
                .as_ref()
                .is_some_and(|types| types.names_a_class(&path, offset))
        });

        match self.type_of(before, offset) {
            Type::Class(class) => match &self.types {
                Some(types) => types
                    .members_of(&class)
                    .into_iter()
                    .filter(|symbol| !(on_class_object && symbol.kind == Kind::Field))
                    .collect(),
                None => Vec::new(),
            },
            Type::Module(module) => quince::sema::symbols::module_symbols(&module),
            Type::Unknown => Vec::new(),
        }
    }

    /// What the text ending at `before` evaluates to.
    ///
    /// A dotted path first, since a name needs the scope it was written in;
    /// then a literal, which needs only the lexer. Both are answers the
    /// language gives, and neither is a guess about what a line looks like.
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

/// Runs the Quince LSP server event loop over stdio.
pub fn run_lsp_server() -> anyhow::Result<()> {
    let (connection, io_threads) = Connection::stdio();

    let server_capabilities = serde_json::to_value(&ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL,
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

pub(crate) fn parse_ast_lenient(source: &str) -> Option<Vec<Stmt>> {
    if let Ok(stmts) = quince::compile(source) {
        return Some(stmts);
    }
    if let Ok(tokens) = quince::syntax::lexer::Lexer::new(source).tokenize()
        && let Ok(stmts) = quince::syntax::parser::Parser::new(tokens).parse()
    {
        return Some(stmts);
    }
    None
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
            let location = get_definition(uri, documents.get(uri), pos);
            let resp = Response::new_ok(id, location);
            connection.sender.send(Message::Response(resp))?;
        }
        lsp_types::request::DocumentSymbolRequest::METHOD => {
            let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
            let uri = &params.text_document.uri;
            let symbols = get_document_symbols(uri, documents.get(uri));
            let resp = Response::new_ok(id, DocumentSymbolResponse::Flat(symbols));
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
            if let Some(change) = params.content_changes.into_iter().last() {
                let previous = documents.remove(&uri);
                let state = DocumentState::new(change.text.clone(), previous);
                documents.insert(uri.clone(), state);
                publish_diagnostics(connection, uri, &change.text)?;
            }
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

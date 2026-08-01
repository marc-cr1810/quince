use std::collections::HashMap;

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    notification::{Notification as _, PublishDiagnostics},
    request::{Completion, GotoDefinition, HoverRequest, Request as _, SignatureHelpRequest},
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, HoverProviderCapability, LanguageString, Location,
    MarkedString, OneOf, ParameterInformation, ParameterLabel, Position, PublishDiagnosticsParams,
    Range, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensParams, SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities,
    SignatureHelp, SignatureHelpOptions, SignatureHelpParams, SignatureInformation, SymbolInformation,
    SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use quince::ast::{Expr, ExprKind, Stmt, StmtKind};
use quince::error::QuinceError;
use crate::cursor::{path_ending_at, trailing_literal_type};
use quince::doc::Doc;
use quince::infer::{self, Kind, Symbol, Type, Types};
use quince::token::{Span, KEYWORDS};

struct DocumentState {
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
    fn new(text: String, previous: Option<DocumentState>) -> DocumentState {
        let ast = parse_ast_lenient(&text);
        let types = match &ast {
            Some(ast) => Some(infer::infer(ast)),
            None => previous.and_then(|state| state.types),
        };
        DocumentState { text, ast, types }
    }

    /// Everything reachable through a dot on whatever `before` evaluates to.
    fn members_before(&self, before: &str, offset: u32) -> Vec<Symbol> {
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
            Type::Module(module) => quince::infer::module_symbols(&module),
            Type::Unknown => Vec::new(),
        }
    }

    /// What the text ending at `before` evaluates to.
    ///
    /// A dotted path first, since a name needs the scope it was written in;
    /// then a literal, which needs only the lexer. Both are answers the
    /// language gives, and neither is a guess about what a line looks like.
    fn type_of(&self, before: &str, offset: u32) -> Type {
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

const LEGEND_TYPES: &[SemanticTokenType] = &[
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

fn parse_ast_lenient(source: &str) -> Option<Vec<Stmt>> {
    if let Ok(stmts) = quince::compile(source) {
        return Some(stmts);
    }
    if let Ok(tokens) = quince::lexer::Lexer::new(source).tokenize()
        && let Ok(stmts) = quince::parser::Parser::new(tokens).parse()
    {
        return Some(stmts);
    }
    None
}

fn handle_request(
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

fn handle_notification(
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

fn publish_diagnostics(connection: &Connection, uri: Url, source: &str) -> anyhow::Result<()> {
    let mut diagnostics = Vec::new();

    if let Err(err) = quince::compile(source) {
        diagnostics.push(quince_error_to_diagnostic(source, &err));
    }

    let params = PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    };

    let notif = Notification {
        method: PublishDiagnostics::METHOD.to_string(),
        params: serde_json::to_value(params)?,
    };

    connection.sender.send(Message::Notification(notif))?;
    Ok(())
}

fn quince_error_to_diagnostic(source: &str, err: &QuinceError) -> Diagnostic {
    let range = span_to_range(source, err.span);
    let mut message = err.message.clone();
    if let Some(help) = &err.help {
        message.push_str(&format!("\nhelp: {help}"));
    }

    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        code: None,
        code_description: None,
        source: Some("quince".to_string()),
        message,
        related_information: None,
        tags: None,
        data: None,
    }
}

// --- LSP IDE Capabilities ---

/// How a symbol is drawn in a completion list.
fn kind_of(kind: Kind) -> CompletionItemKind {
    match kind {
        Kind::Class => CompletionItemKind::CLASS,
        Kind::Function => CompletionItemKind::FUNCTION,
        Kind::Method => CompletionItemKind::METHOD,
        Kind::Field => CompletionItemKind::FIELD,
        Kind::Variable => CompletionItemKind::VARIABLE,
        Kind::Parameter => CompletionItemKind::VARIABLE,
        Kind::Module => CompletionItemKind::MODULE,
    }
}

/// One completion, built from what the pass and the tables know.
///
/// The detail line is the symbol's own signature and the documentation is its
/// own `##` block. Neither is written here, which is the point: this file used
/// to carry sentences about `print` and a guess about everything else.
fn item_of(symbol: &Symbol) -> CompletionItem {
    CompletionItem {
        label: symbol.name.clone(),
        kind: Some(kind_of(symbol.kind)),
        detail: Some(symbol.signature()),
        documentation: symbol.doc.as_ref().map(|doc| {
            lsp_types::Documentation::MarkupContent(lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: rendered_doc(doc),
            })
        }),
        ..Default::default()
    }
}

/// A doc block as markdown: the summary, then what it says about each part.
fn rendered_doc(doc: &Doc) -> String {
    let mut out = doc.summary.clone();
    for param in &doc.params {
        out.push_str(&format!("\n\n`{}` — {}", param.name, param.text));
    }
    if let Some(returns) = &doc.returns {
        out.push_str(&format!("\n\nReturns {}", returns.text));
    }
    for thrown in &doc.throws {
        out.push_str(&format!("\n\nRaises `{}` {}", thrown.name, thrown.text));
    }
    out.trim().to_string()
}

fn get_completions(state: Option<&DocumentState>, pos: Position) -> Vec<CompletionItem> {
    let Some(state) = state else {
        return Vec::new();
    };

    // After a dot, the receiver decides the whole list. What the pass does not
    // know, nobody offers: the text heuristics that used to answer here read
    // the first character of the line and told a list it was a string, and an
    // empty list is a better answer than a confident wrong one.
    if is_preceded_by_dot(&state.text, pos) {
        let Some(before) = text_before_dot(&state.text, pos) else {
            return Vec::new();
        };
        let offset = position_to_offset(&state.text, pos);
        return state
            .members_before(&before, offset)
            .iter()
            .map(item_of)
            .collect();
    }

    // After `import` or `from`, the only thing that can follow is a module, so
    // that is the only thing offered. Offering the stdlib names everywhere would
    // suggest `math` to a file that never imported it, where the name means
    // nothing — the point of `import` is that a module is not there until asked
    // for, and a completion list that forgets it undoes exactly that.
    if at_import(&state.text, pos) {
        return quince::stdlib::MODULES
            .iter()
            .map(|module| CompletionItem {
                label: module.name.to_string(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!(
                    "{} — {}",
                    module.name,
                    module
                        .members
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                ..Default::default()
            })
            .collect();
    }

    // The keywords, the globals, and everything the pass found in scope here.
    // All three are read off the language rather than written down: a keyword
    // comes with its own explanation, a global with its own signature, and a
    // name in scope with whatever the program said about it.
    let mut items: Vec<CompletionItem> = KEYWORDS
        .iter()
        .map(|keyword| CompletionItem {
            label: (*keyword).to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: quince::token::TokenKind::keyword(keyword)
                .and_then(|kind| kind.doc())
                .map(str::to_string),
            ..Default::default()
        })
        .collect();
    items.extend(quince::infer::globals().iter().map(item_of));
    if let Some(types) = &state.types {
        let offset = position_to_offset(&state.text, pos);
        items.extend(types.in_scope(offset).iter().map(item_of));
    }
    items
}

/// Whether the cursor sits where a module name goes.
///
/// The text before it on this line is `import`, or `from`, or a `from` and the
/// module being imported from — which is where a member name goes rather than a
/// module, but the members are what the module's own completion lists anyway.
fn at_import(source: &str, pos: Position) -> bool {
    let Some(line) = source.lines().nth(pos.line as usize) else {
        return false;
    };
    let before = &line[..(pos.character as usize).min(line.len())];
    let mut words = before.split_whitespace();
    match (words.next(), words.next()) {
        (Some("import"), None) => true,
        (Some("from"), None) => false,
        (Some("from"), Some(_)) => true,
        _ => false,
    }
}

fn is_preceded_by_dot(source: &str, pos: Position) -> bool {
    let line = match source.lines().nth(pos.line as usize) {
        Some(l) => l,
        None => return false,
    };
    let col = (pos.character as usize).min(line.len());
    line[..col].trim_end().ends_with('.')
}

/// The text the cursor sits immediately after the dot of.
fn text_before_dot(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = (pos.character as usize).min(line.len());
    Some(line[..col].trim_end().strip_suffix('.')?.to_string())
}

/// The text a member access sits after, if the word under the cursor is one.
///
/// Hovering the `magnitude` in `p.magnitude()` asks about a method, and which
/// method depends on what `p` is. The word itself is not enough.
fn text_before_word(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = (pos.character as usize).min(line.len());
    let start = line[..col]
        .rfind(|c: char| !(c == '_' || c.is_alphanumeric()))
        .map_or(0, |index| index + 1);
    Some(line[..start].trim_end().strip_suffix('.')?.to_string())
}

/// What is being called at `pos`, and which argument the cursor is in.
///
/// Text, and unavoidably so: the call has not been written yet, so there is no
/// node in any tree to ask. What it reads is bracket depth and commas — where
/// the cursor *is*, never what anything *means*. Deciding the callee's type is
/// the pass's, and this hands the name over for it to answer.
fn find_call_context(source: &str, pos: Position) -> Option<(String, Option<String>, u32)> {
    let before = &source[..(position_to_offset(source, pos) as usize).min(source.len())];

    let chars: Vec<(usize, char)> = before.char_indices().collect();
    let mut depth = 0;
    let mut commas = 0;
    let mut open = None;
    for index in (0..chars.len()).rev() {
        match chars[index].1 {
            ')' => depth += 1,
            '(' if depth == 0 => {
                open = Some(chars[index].0);
                break;
            }
            '(' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }

    let before_paren = before[..open?].trim_end();
    let start = before_paren
        .rfind(|c: char| !(c == '_' || c.is_alphanumeric()))
        .map_or(0, |index| index + 1);
    if start >= before_paren.len() {
        return None;
    }
    let callee = before_paren[start..].to_string();
    let receiver = before_paren[..start]
        .trim_end()
        .strip_suffix('.')
        .map(str::to_string);

    Some((callee, receiver, commas))
}

fn symbol_at(state: &DocumentState, word: &str, pos: Position) -> Option<Symbol> {
    let offset = position_to_offset(&state.text, pos);

    // `p.magnitude` — the word is a member, and the receiver decides which one.
    if let Some(before) = text_before_word(&state.text, pos)
        && let Some(found) = state
            .members_before(&before, offset)
            .into_iter()
            .find(|symbol| symbol.name == word)
    {
        return Some(found);
    }

    if let Some(types) = &state.types
        && let Some(found) = types.symbol(word, offset)
    {
        return Some(found);
    }

    quince::infer::globals()
        .into_iter()
        .find(|symbol| symbol.name == word)
}

fn get_hover(state: Option<&DocumentState>, pos: Position) -> Option<Hover> {
    let state = state?;
    let word = get_word_at_position(&state.text, pos)?;

    // A keyword explains itself, from beside the list that reserves it.
    if let Some(doc) = quince::token::TokenKind::keyword(&word).and_then(|kind| kind.doc()) {
        return Some(Hover {
            contents: HoverContents::Array(vec![
                MarkedString::LanguageString(LanguageString {
                    language: "quince".to_string(),
                    value: word,
                }),
                MarkedString::String(doc.to_string()),
            ]),
            range: None,
        });
    }

    let symbol = symbol_at(state, &word, pos)?;
    let signature = symbol.signature();
    // A hover that would repeat the word under the cursor and add nothing says
    // nothing. A parameter with no type and no `@param` is the ordinary case,
    // and an empty tooltip is worse than none at all.
    if symbol.doc.is_none() && signature == word {
        return None;
    }
    let mut contents = vec![MarkedString::LanguageString(LanguageString {
        language: "quince".to_string(),
        value: signature,
    })];
    if let Some(doc) = &symbol.doc {
        contents.push(MarkedString::String(rendered_doc(doc)));
    }

    Some(Hover {
        contents: HoverContents::Array(contents),
        range: None,
    })
}

fn get_definition(uri: &Url, state: Option<&DocumentState>, pos: Position) -> Option<GotoDefinitionResponse> {
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

fn find_decl_span(stmts: &[Stmt], target: &str) -> Option<Span> {
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

fn find_name_range(source: &str, span: Span, name: &str) -> Option<Range> {
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

fn get_document_symbols(uri: &Url, state: Option<&DocumentState>) -> Vec<SymbolInformation> {
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

// --- Semantic Tokens Highlighting ---

#[derive(Clone, Copy)]
struct RawSemanticToken {
    line: u32,
    col: u32,
    len: u32,
    token_type: u32,
    modifiers: u32,
}

fn get_semantic_tokens(state: Option<&DocumentState>) -> SemanticTokens {
    let mut raw_tokens = Vec::new();
    let state = match state {
        Some(s) => s,
        None => return SemanticTokens { result_id: None, data: Vec::new() },
    };
    let ast = match &state.ast {
        Some(a) => a,
        None => return SemanticTokens { result_id: None, data: Vec::new() },
    };

    collect_stmt_semantic_tokens(&state.text, ast, &mut raw_tokens);

    // Sort tokens by line and column
    raw_tokens.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.col.cmp(&b.col)));

    let mut data = Vec::new();
    let mut prev_line = 0;
    let mut prev_col = 0;

    for t in raw_tokens {
        let delta_line = t.line - prev_line;
        let delta_start = if delta_line == 0 {
            t.col - prev_col
        } else {
            t.col
        };

        data.push(SemanticToken {
            delta_line,
            delta_start,
            length: t.len,
            token_type: t.token_type,
            token_modifiers_bitset: t.modifiers,
        });

        prev_line = t.line;
        prev_col = t.col;
    }

    SemanticTokens {
        result_id: None,
        data,
    }
}

fn push_raw_token(
    source: &str,
    span: Span,
    name: &str,
    token_type: u32,
    modifiers: u32,
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    if let Some(range) = find_name_range(source, span, name) {
        raw_tokens.push(RawSemanticToken {
            line: range.start.line,
            col: range.start.character,
            len: name.len() as u32,
            token_type,
            modifiers,
        });
    }
}

fn collect_stmt_semantic_tokens(
    source: &str,
    stmts: &[Stmt],
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                // Function declaration (1), declaration modifier (1)
                push_raw_token(source, stmt.span, &decl.name, 1, 1, raw_tokens);
                for param in &decl.params {
                    // Parameter (4), declaration modifier (1)
                    push_raw_token(source, stmt.span, &param.name, 4, 1, raw_tokens);
                }
                collect_stmt_semantic_tokens(source, &decl.body.stmts, raw_tokens);
            }
            StmtKind::Class { name, parent, methods, .. } => {
                // Class declaration (0), declaration modifier (1)
                push_raw_token(source, stmt.span, name, 0, 1, raw_tokens);
                if let Some(p) = parent {
                    // Parent class reference (0)
                    push_raw_token(source, stmt.span, &p.name, 0, 0, raw_tokens);
                }
                for m in methods {
                    let m_span = Span { start: m.body.span.start.saturating_sub(40), end: m.body.span.end };
                    // Method declaration (2), declaration modifier (1)
                    push_raw_token(source, m_span, &m.name, 2, 1, raw_tokens);
                    for param in &m.params {
                        push_raw_token(source, m_span, &param.name, 4, 1, raw_tokens);
                    }
                    collect_stmt_semantic_tokens(source, &m.body.stmts, raw_tokens);
                }
            }
            StmtKind::Let { name, value, .. } => {
                // Variable declaration (3), declaration modifier (1)
                push_raw_token(source, stmt.span, name, 3, 1, raw_tokens);
                collect_expr_semantic_tokens(source, value, raw_tokens);
            }
            StmtKind::Expr(expr) => collect_expr_semantic_tokens(source, expr, raw_tokens),
            StmtKind::If { cond, then, otherwise, .. } => {
                collect_expr_semantic_tokens(source, cond, raw_tokens);
                collect_stmt_semantic_tokens(source, &then.stmts, raw_tokens);
                if let Some(other) = otherwise {
                    collect_stmt_semantic_tokens(source, std::slice::from_ref(other.as_ref()), raw_tokens);
                }
            }
            StmtKind::While { cond, body, .. } => {
                collect_expr_semantic_tokens(source, cond, raw_tokens);
                collect_stmt_semantic_tokens(source, &body.stmts, raw_tokens);
            }
            StmtKind::For { var, iter, body, .. } => {
                push_raw_token(source, stmt.span, var, 3, 1, raw_tokens);
                collect_expr_semantic_tokens(source, iter, raw_tokens);
                collect_stmt_semantic_tokens(source, &body.stmts, raw_tokens);
            }
            StmtKind::Try { body, handler, binding, .. } => {
                push_raw_token(source, stmt.span, binding, 3, 1, raw_tokens);
                collect_stmt_semantic_tokens(source, &body.stmts, raw_tokens);
                collect_stmt_semantic_tokens(source, &handler.stmts, raw_tokens);
            }
            StmtKind::Block(block) => collect_stmt_semantic_tokens(source, &block.stmts, raw_tokens),
            _ => {}
        }
    }
}

fn collect_expr_semantic_tokens(
    source: &str,
    expr: &Expr,
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Var(var) = &callee.kind {
                // Call (Function / Class constructor)
                push_raw_token(source, callee.span, &var.name, 1, 0, raw_tokens);
            } else {
                collect_expr_semantic_tokens(source, callee, raw_tokens);
            }
            for arg in args {
                collect_expr_semantic_tokens(source, arg, raw_tokens);
            }
        }
        ExprKind::Field { target, name } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            // Member property / method (5)
            push_raw_token(source, expr.span, name, 5, 0, raw_tokens);
        }
        ExprKind::Var(var) => {
            if var.name == "self" || var.name == "super" {
                push_raw_token(source, expr.span, &var.name, 7, 0, raw_tokens); // Keyword (7)
            } else {
                push_raw_token(source, expr.span, &var.name, 3, 0, raw_tokens); // Variable (3)
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr_semantic_tokens(source, lhs, raw_tokens);
            collect_expr_semantic_tokens(source, rhs, raw_tokens);
        }
        ExprKind::Unary { rhs, .. } => collect_expr_semantic_tokens(source, rhs, raw_tokens),
        ExprKind::List(items) => {
            for item in items {
                collect_expr_semantic_tokens(source, item, raw_tokens);
            }
        }
        ExprKind::Assign { target, value } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            collect_expr_semantic_tokens(source, value, raw_tokens);
        }
        _ => {}
    }
}

fn get_word_at_position(source: &str, pos: Position) -> Option<String> {
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
fn span_to_range(source: &str, span: Span) -> Range {
    let start = offset_to_position(source, span.start as usize);
    let end = offset_to_position(source, span.end as usize);
    if start == end {
        let next_char = offset_to_position(source, (span.end as usize + 1).min(source.len()));
        Range { start, end: next_char }
    } else {
        Range { start, end }
    }
}

fn get_signature_help(state: Option<&DocumentState>, pos: Position) -> Option<SignatureHelp> {
    let state = state?;
    let (callee, receiver, active) = find_call_context(&state.text, pos)?;
    let offset = position_to_offset(&state.text, pos);

    // Where to look for the name depends on what is in front of it, exactly as
    // it does for a completion. A method is found on its receiver's class; a
    // bare name is found in scope, and failing that among the globals.
    let symbol = match &receiver {
        Some(before) => state
            .members_before(before, offset)
            .into_iter()
            .find(|symbol| symbol.name == callee),
        None => state
            .types
            .as_ref()
            .and_then(|types| types.symbol(&callee, offset))
            .or_else(|| {
                quince::infer::globals()
                    .into_iter()
                    .find(|symbol| symbol.name == callee)
            }),
    }?;

    // A name that is not callable has no signature to show. A class is: calling
    // one runs its `init`, and the parameters worth showing are that method's.
    let parameters: Vec<ParameterInformation> = match symbol.kind {
        Kind::Class => match &state.types {
            Some(types) => types
                .members_of(&symbol.name)
                .into_iter()
                .find(|member| member.name == "init")
                .map(|init| init.params)
                .unwrap_or(symbol.params.clone()),
            None => symbol.params.clone(),
        },
        _ => symbol.params.clone(),
    }
    .into_iter()
    .map(|name| ParameterInformation {
        // Documented from the `@param` that named it, when there is one, which
        // is what makes writing them worth the trouble.
        documentation: symbol
            .doc
            .as_ref()
            .and_then(|doc| doc.params.iter().find(|param| param.name == name))
            .map(|param| lsp_types::Documentation::String(param.text.clone())),
        label: ParameterLabel::Simple(name),
    })
    .collect();

    let label = match symbol.kind {
        Kind::Class => format!(
            "{}({})",
            symbol.name,
            parameters
                .iter()
                .map(|param| match &param.label {
                    ParameterLabel::Simple(name) => name.clone(),
                    ParameterLabel::LabelOffsets(_) => String::new(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => symbol.signature(),
    };

    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: symbol
                .doc
                .as_ref()
                .map(|doc| lsp_types::Documentation::String(doc.summary.clone())),
            parameters: Some(parameters),
            active_parameter: Some(active),
        }],
        active_signature: Some(0),
        active_parameter: Some(active),
    })
}

/// The byte offset an LSP position names, which is what a `Span` is measured in.
fn position_to_offset(source: &str, pos: Position) -> u32 {
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

fn offset_to_position(source: &str, offset: usize) -> Position {
    let offset = offset.min(source.len());
    let line = source[..offset].matches('\n').count() as u32;
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = source[line_start..offset].chars().count() as u32;
    Position {
        line,
        character: col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_help_extracts_class_constructor_params() {
        let before = "class Point {\n    op init(x, y) {\n        self.x = x\n        self.y = y\n    }\n}\n";
        let code = &format!("{before}\nlet p = Point(3, ");
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("a constructor helps");
        assert_eq!(help.signatures[0].label, "Point(x, y)");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn signature_help_names_what_a_method_returns() {
        let before = "class Point {\n    op init(x, y) {\n        self.x = x\n        self.y = y\n    }\n    fn scaled(k) {\n        return Point(self.x * k, self.y * k)\n    }\n}\nlet p = Point(3, 4)\n";
        let code = &format!("{before}p.scaled(");
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("a method helps");
        // The return type comes from the pass, and the parameter name from the
        // declaration. Neither was available when this said `fn scaled(k)`.
        assert_eq!(help.signatures[0].label, "fn scaled(k): Point");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_names_a_builtins_parameters() {
        let before = "let text = \"hello world\"\n";
        let code = &format!("{before}text.split(");
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("a builtin helps");
        // `fn split(arg1, arg2)` before the natives knew their own parameters.
        assert_eq!(help.signatures[0].label, "fn split(separator): list");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_reaches_a_method_on_a_literal() {
        let before = "\"test\"";
        let code = &format!("{before}.split(");
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("a literal helps");
        assert_eq!(help.signatures[0].label, "fn split(separator): list");
    }

    #[test]
    fn signature_help_reads_an_error_class_from_the_prelude() {
        // `Error` and its `op init(message)` are written in Quince, so the
        // editor learns the parameter by inferring over the same source the
        // interpreter runs rather than from a second copy of it.
        let code = "TypeError(";
        let state = typed(&["", code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("an error class helps");
        assert_eq!(help.signatures[0].label, "TypeError(message)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn completion_excludes_symbols_defined_after_cursor() {
        let code = "let defined_above = 1\n\nlet defined_below = 2";
        let state = DocumentState::new(code.to_string(), None);
        let pos = Position { line: 1, character: 0 };
        let labels: Vec<String> = get_completions(Some(&state), pos)
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert!(labels.contains(&"defined_above".to_string()));
        assert!(!labels.contains(&"defined_below".to_string()));
    }

    #[test]
    fn signature_help_extracts_inherited_constructor_params() {
        let before = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Cat extends Animal {}\n";
        let code = &format!("{before}Cat(");
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("an inherited init helps");
        assert_eq!(help.signatures[0].label, "Cat(name)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_extracts_super_method_params() {
        let before = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Dog extends Animal {\n    op init(name, breed) {\n        self.breed = breed\n    }\n}\n";
        let code = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Dog extends Animal {\n    op init(name, breed) {\n        super.init(";
        let state = typed(&[before, code]);
        let help = get_signature_help(Some(&state), end_of(code)).expect("`super.init` helps");
        assert_eq!(help.signatures[0].label, "fn init(name)");
        assert_eq!(help.active_parameter, Some(0));
    }

    /// The position just past the end of `code`, which is where the cursor is
    /// when someone has just typed the last character of it.
    fn end_of(code: &str) -> Position {
        let line = code.matches('\n').count() as u32;
        let start = code.rfind('\n').map_or(0, |index| index + 1);
        Position {
            line,
            character: code[start..].chars().count() as u32,
        }
    }

    fn path_at(code: &str) -> Option<String> {
        path_ending_at(&text_before_dot(code, end_of(code))?)
    }

    /// A document typed rather than opened: each string is the whole text after
    /// one edit.
    ///
    /// Which is the only honest way to test a completion after a `.`, because
    /// the `.` is what stops the document parsing — a state built from the
    /// final text alone would be testing a document nobody ever has.
    fn typed(edits: &[&str]) -> DocumentState {
        let mut state = None;
        for text in edits {
            state = Some(DocumentState::new(text.to_string(), state));
        }
        state.expect("at least one edit")
    }

    /// The labels a completion request comes back with, after typing the `.`.
    fn completions_after(before: &str) -> Vec<String> {
        let code = format!("{before}.");
        let state = typed(&[before, &code]);
        let mut labels: Vec<String> = get_completions(Some(&state), end_of(&code))
            .into_iter()
            .map(|item| item.label)
            .collect();
        labels.sort();
        labels.dedup();
        labels
    }

    #[test]
    fn a_receiver_path_keeps_the_parentheses_the_heuristics_throw_away() {
        assert_eq!(path_at("p.").as_deref(), Some("p"));
        assert_eq!(path_at("o.inner.").as_deref(), Some("o.inner"));
        assert_eq!(path_at("Point().").as_deref(), Some("Point()"));
        // The arguments say nothing about the type, so they are normalised
        // away rather than parsed.
        assert_eq!(path_at("Point(3, 4).").as_deref(), Some("Point()"));
        assert_eq!(path_at("f(g(x)).").as_deref(), Some("f()"));
        assert_eq!(path_at("let q = b.twin().").as_deref(), Some("b.twin()"));

        // Not a path, and not approximated into one — the heuristics read
        // literals, and this is not a parser.
        assert_eq!(path_at("\"abc\"."), None);
        assert_eq!(path_at("xs[0]."), None);
        assert_eq!(path_at("p"), None);
    }

    #[test]
    fn a_position_and_an_offset_name_the_same_byte() {
        let code = "let a = 1\nfn f() {\n  return 2\n}\n";
        for offset in 0..=code.len() {
            let position = offset_to_position(code, offset);
            assert_eq!(
                position_to_offset(code, position) as usize,
                offset,
                "offset {offset} did not survive the round trip"
            );
        }
    }

    #[test]
    fn dot_completion_offers_the_class_the_pass_worked_out() {
        // The receiver is a constructor call, which the capital-letter
        // heuristic gets right by accident and the pass gets right by reading
        // the declaration. What the heuristic cannot do is the second half:
        // `made` holds a `Box` because that is what `make` returns.
        let labels = completions_after("class Box {\n    op init() {\n        self.n = 1\n    }\n    fn only_on_box() {\n        return 1\n    }\n}\nfn make() {\n    return Box()\n}\nlet made = make()\nmade");
        assert!(labels.contains(&"only_on_box".to_string()), "{labels:?}");
        assert!(labels.contains(&"n".to_string()), "{labels:?}");
    }

    #[test]
    fn dot_completion_offers_a_modules_members() {
        // Nothing offered these before: a module is not a class, so the
        // class-shaped heuristic had nothing to say about `math.` at all.
        let labels = completions_after("import math\nmath");
        assert!(labels.contains(&"floor".to_string()), "{labels:?}");
        assert!(labels.contains(&"pi".to_string()), "{labels:?}");
        // And only that module's, which is the point of `import` — `read` is
        // `io`'s and has no business here.
        assert!(!labels.contains(&"read".to_string()), "{labels:?}");
    }

    /// The code block a hover leads with, which is the signature.
    fn hovered(code: &str, pos: Position) -> Option<String> {
        let state = DocumentState::new(code.to_string(), None);
        let hover = get_hover(Some(&state), pos)?;
        let HoverContents::Array(parts) = hover.contents else {
            panic!("expected an array of hover contents");
        };
        match parts.first() {
            Some(MarkedString::LanguageString(shown)) => Some(shown.value.clone()),
            _ => panic!("expected a quince code block first"),
        }
    }

    /// Everything a hover says after its signature.
    fn hovered_doc(code: &str, pos: Position) -> Option<String> {
        let state = DocumentState::new(code.to_string(), None);
        let HoverContents::Array(parts) = get_hover(Some(&state), pos)?.contents else {
            panic!("expected an array of hover contents");
        };
        match parts.get(1) {
            Some(MarkedString::String(text)) => Some(text.clone()),
            _ => None,
        }
    }

    #[test]
    fn hover_over_a_binding_says_how_it_was_bound_and_what_it_holds() {
        let pos = Position { line: 0, character: 5 };
        assert_eq!(
            hovered("let total = 1 + 2\n", pos).as_deref(),
            Some("let total: int")
        );
        // `final` and `const` are a real distinction and the editor shows it.
        assert_eq!(
            hovered("const LIMIT = 10\n", Position { line: 0, character: 7 }).as_deref(),
            Some("const LIMIT: int")
        );
    }

    #[test]
    fn hover_over_a_function_names_what_it_returns() {
        let code = "fn make() {\n    return 1\n}\n";
        let pos = Position { line: 0, character: 4 };
        assert_eq!(hovered(code, pos).as_deref(), Some("fn make(): int"));
    }

    #[test]
    fn hover_over_a_keyword_explains_it_from_beside_the_list() {
        let code = "let total = 1\n";
        let doc = hovered_doc(code, Position { line: 0, character: 1 })
            .expect("a keyword explains itself");
        assert!(doc.contains("reassigned"), "{doc}");
    }

    #[test]
    fn hover_over_a_builtin_reads_the_natives_own_documentation() {
        // This used to be a sentence written in `lsp.rs` for ten builtins and
        // nothing for the other forty-two.
        let code = "let n = len(\"ab\")\n";
        let pos = Position { line: 0, character: 9 };
        assert_eq!(hovered(code, pos).as_deref(), Some("fn len(value): int"));
        let doc = hovered_doc(code, pos).expect("`len` documents itself");
        assert!(doc.contains("How many characters"), "{doc}");
    }

    #[test]
    fn hover_over_a_parameter_says_what_its_function_said_about_it() {
        // The only thing anyone can be told about a parameter until v0.7, and
        // the reason writing an `@param` is worth the trouble.
        let code = "## Scales it.\n## @param k how much to scale by\nfn scale(k) {\n    return k\n}\n";
        let pos = Position { line: 3, character: 11 };
        assert_eq!(
            hovered_doc(code, pos).as_deref(),
            Some("how much to scale by")
        );
    }

    #[test]
    fn hover_says_nothing_where_nothing_is_known() {
        // An undocumented parameter carries no type and no prose. Repeating the
        // word under the cursor back at the reader is worse than silence.
        let code = "fn f(x) {\n    return x\n}\n";
        assert!(hovered(code, Position { line: 1, character: 11 }).is_none());
    }

    /// A native's result is what the native says it is.
    ///
    /// This asserted the opposite until `Native` grew a `returns` field: the
    /// pass had nothing to read, the heuristics answered, and they read the
    /// literal at the front of the line and called a list a string. `words` is
    /// a list, and the editor now offers a list's methods on it.
    #[test]
    fn dot_completion_types_a_literal_by_lexing_it() {
        // `[1, 2]` is a list and `xs[0]` is an item out of one. The heuristic
        // that used to answer here read the trailing bracket and called both
        // lists; this reads the token in front of the opening one.
        let labels = completions_after("[1, 2]");
        assert!(labels.contains(&"push".to_string()), "{labels:?}");

        let labels = completions_after("let xs = [1, 2]\nxs[0]");
        assert!(labels.is_empty(), "an item of unknown type offers nothing: {labels:?}");

        let labels = completions_after("\"abc\"");
        assert!(labels.contains(&"upper".to_string()), "{labels:?}");
    }

    #[test]
    fn dot_completion_on_an_unknown_receiver_offers_nothing() {
        // The heuristics used to fill this in by guessing. An empty list is the
        // honest answer, and a wrong one is worse than none: it is indexed,
        // scrolled, and believed.
        let labels = completions_after("fn f(thing) {\n    thing");
        assert!(labels.is_empty(), "{labels:?}");
    }

    #[test]
    fn a_natives_result_is_what_the_native_says_it_is() {
        let labels = completions_after("let words = \"a,b,c\".split(\",\")\nwords");
        assert!(labels.contains(&"push".to_string()), "{labels:?}");
        assert!(!labels.contains(&"lower".to_string()), "{labels:?}");

        // And the other direction. `join` is a string's method taking the list,
        // so this crosses from a list back to a string — which the heuristics
        // got wrong the same way, by reading the `[` and stopping.
        let labels = completions_after("let sep = \", \"\nlet line = sep.join([\"a\", \"b\"])\nline");
        assert!(labels.contains(&"upper".to_string()), "{labels:?}");
        assert!(!labels.contains(&"push".to_string()), "{labels:?}");
    }
}

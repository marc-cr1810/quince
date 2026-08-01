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
use quince::token::{Span, KEYWORDS};

struct DocumentState {
    text: String,
    ast: Option<Vec<Stmt>>,
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
    if let Ok(tokens) = quince::lexer::Lexer::new(source).tokenize() {
        if let Ok(stmts) = quince::parser::Parser::new(tokens).parse() {
            return Some(stmts);
        }
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
            let ast = parse_ast_lenient(&text);
            documents.insert(uri.clone(), DocumentState { text: text.clone(), ast });
            publish_diagnostics(connection, uri, &text)?;
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri;
            if let Some(change) = params.content_changes.into_iter().last() {
                let ast = parse_ast_lenient(&change.text);
                documents.insert(uri.clone(), DocumentState { text: change.text.clone(), ast });
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

fn get_completions(state: Option<&DocumentState>, pos: Position) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    let state = match state {
        Some(s) => s,
        None => return items,
    };

    // Check if user is typing a dot (e.g. `self.` or `p.`)
    let is_dot_trigger = is_preceded_by_dot(&state.text, pos);

    // 1. User-defined classes and type constructors
    if !is_dot_trigger {
        // Language Keywords
        for &kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some("Quince keyword".to_string()),
                ..Default::default()
            });
        }

        // Builtin Functions & Types
        let builtins = &[
            ("print", "fn print(...) - Prints values to stdout", CompletionItemKind::FUNCTION),
            ("type", "fn type(value) - Returns the type name of a value", CompletionItemKind::FUNCTION),
            ("len", "fn len(collection) - Returns the length of a collection", CompletionItemKind::FUNCTION),
            ("int", "Built-in integer type constructor", CompletionItemKind::TYPE_PARAMETER),
            ("float", "Built-in float type constructor", CompletionItemKind::TYPE_PARAMETER),
            ("string", "Built-in string type constructor", CompletionItemKind::TYPE_PARAMETER),
            ("list", "Built-in list type constructor", CompletionItemKind::TYPE_PARAMETER),
            ("dict", "Built-in dict type constructor", CompletionItemKind::TYPE_PARAMETER),
        ];

        for &(name, doc, kind) in builtins {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(kind),
                detail: Some(doc.to_string()),
                ..Default::default()
            });
        }

        // Builtin Error Classes
        for kind in quince::error::ERROR_KINDS {
            let class_name = kind.class_name();
            items.push(CompletionItem {
                label: class_name.to_string(),
                kind: Some(CompletionItemKind::CLASS),
                detail: Some(format!("Built-in error class {class_name}")),
                ..Default::default()
            });
        }
    }

    // 2. Traversal for AST & Text Symbols (Classes, Methods, Functions, Variables)
    if is_dot_trigger {
        if let Some(receiver) = get_receiver_before_dot(&state.text, pos) {
            let target_class = infer_receiver_class(&state.text, &receiver, pos);
            collect_dot_completions_for_class(&state.text, state.ast.as_deref(), &receiver, target_class.as_deref(), &mut items);
        }
    } else {
        if let Some(ast) = &state.ast {
            collect_ast_completions(&state.text, ast, pos, &mut items);
        }
        collect_text_variable_completions(&state.text, pos, &mut items);
    }

    items
}


fn is_preceded_by_dot(source: &str, pos: Position) -> bool {
    let line = match source.lines().nth(pos.line as usize) {
        Some(l) => l,
        None => return false,
    };
    let col = (pos.character as usize).min(line.len());
    line[..col].trim_end().ends_with('.')
}

fn get_receiver_before_dot(source: &str, pos: Position) -> Option<String> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = (pos.character as usize).min(line.len());
    let trimmed = line[..col].trim_end();
    if !trimmed.ends_with('.') {
        return None;
    }
    let before_dot = trimmed[..trimmed.len() - 1].trim_end();
    if before_dot.is_empty() {
        return None;
    }

    let mut cleaned = before_dot;
    if cleaned.ends_with(')') {
        if let Some(open_paren) = cleaned.rfind('(') {
            cleaned = cleaned[..open_paren].trim_end();
        }
    }

    if cleaned.ends_with('"') || cleaned.ends_with(']') || cleaned.ends_with('}') {
        return Some(cleaned.to_string());
    }

    let start = cleaned
        .rfind(|c: char| !(c == '_' || c == '.' || c.is_alphanumeric()))
        .map_or(0, |i| i + 1);

    if start < cleaned.len() {
        Some(cleaned[start..].to_string())
    } else {
        Some(cleaned.to_string())
    }
}

fn infer_receiver_class(source: &str, receiver: &str, pos: Position) -> Option<String> {
    let clean_recv = receiver.split('(').next().unwrap_or(receiver).trim();

    // String literal e.g. `"test".`
    if clean_recv.starts_with('"') || clean_recv.ends_with('"') {
        return Some("string".to_string());
    }
    // List literal e.g. `[1, 2, 3, 4].`
    if clean_recv.starts_with('[') || clean_recv.ends_with(']') {
        return Some("list".to_string());
    }
    // Dict literal e.g. `{"a": 1}.`
    if clean_recv.starts_with('{') || clean_recv.ends_with('}') {
        return Some("dict".to_string());
    }
    // Int or Float literal e.g. `5.` or `5.0.`
    if !clean_recv.is_empty() && clean_recv.chars().all(|c| c.is_ascii_digit() || c == '.') {
        if clean_recv.contains('.') {
            return Some("float".to_string());
        } else {
            return Some("int".to_string());
        }
    }

    // Direct Class Constructor Call e.g. `Shadow().` or `Point(3, 4).`
    if clean_recv.chars().next().map_or(false, |c| c.is_uppercase()) {
        return Some(clean_recv.to_string());
    }

    if clean_recv == "self" {
        let mut current_class = None;
        let mut brace_depth = 0;
        let mut class_brace_depth = 0;

        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("class ") {
                let parts: Vec<_> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let name = parts[1].split('{').next().unwrap_or("").trim();
                    if !name.is_empty() {
                        current_class = Some(name.to_string());
                        class_brace_depth = brace_depth;
                    }
                }
            }

            if line_idx == pos.line as usize {
                return current_class;
            }

            for c in line.chars() {
                if c == '{' {
                    brace_depth += 1;
                } else if c == '}' {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    }
                    if current_class.is_some() && brace_depth <= class_brace_depth {
                        current_class = None;
                    }
                }
            }
        }
        return current_class;
    }

    if clean_recv == "super" {
        let mut current_parent = None;
        let mut brace_depth = 0;
        let mut class_brace_depth = 0;

        for (line_idx, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("class ") {
                let parts: Vec<_> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Some(extends_idx) = parts.iter().position(|&p| p == "extends") {
                        if extends_idx + 1 < parts.len() {
                            let pname = parts[extends_idx + 1].split('{').next().unwrap_or("").trim();
                            if !pname.is_empty() {
                                current_parent = Some(pname.to_string());
                                class_brace_depth = brace_depth;
                            }
                        }
                    }
                }
            }

            if line_idx == pos.line as usize {
                return current_parent;
            }

            for c in line.chars() {
                if c == '{' {
                    brace_depth += 1;
                } else if c == '}' {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    }
                    if current_parent.is_some() && brace_depth <= class_brace_depth {
                        current_parent = None;
                    }
                }
            }
        }
        return current_parent;
    }


    let max_line = (pos.line as usize).min(source.lines().count().saturating_sub(1));
    for line_idx in (0..=max_line).rev() {
        if let Some(line) = source.lines().nth(line_idx) {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some(eq_idx) = trimmed.find('=') {
                let lhs = trimmed[..eq_idx].trim();
                let var_name = lhs.split_whitespace().last().unwrap_or("");
                if var_name == receiver {
                    let rhs = trimmed[eq_idx + 1..].trim();
                    let first_word = rhs.split(&['(', ' ', ';'][..]).next().unwrap_or("").trim();
                    if !first_word.is_empty()
                        && first_word.chars().next().map_or(false, |c| c.is_uppercase())
                    {
                        return Some(first_word.to_string());
                    }

                    if rhs.starts_with('"') {
                        return Some("string".to_string());
                    }
                    if rhs.starts_with('[') {
                        return Some("list".to_string());
                    }
                    if rhs.starts_with('{') {
                        return Some("dict".to_string());
                    }

                    if let Some(dot_idx) = rhs.find('.') {
                        let obj_name = rhs[..dot_idx].trim();
                        let rest = rhs[dot_idx + 1..].trim();
                        let method_name = rest.split('(').next().unwrap_or("").trim();
                        if let Some(parent_class) = infer_receiver_class(source, obj_name, Position { line: line_idx as u32, character: 0 }) {
                            if let Some(ret_class) = infer_method_return_class(source, &parent_class, method_name) {
                                return Some(ret_class);
                            }
                            return Some(parent_class);
                        }
                    }
                }
            }
        }
    }

    None
}

fn infer_method_return_class(source: &str, class_name: &str, method_name: &str) -> Option<String> {
    let mut inside_target_class = false;
    let mut inside_target_method = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("class ") {
            let parts: Vec<_> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[1].split('{').next().unwrap_or("").trim();
                inside_target_class = name == class_name;
            }
        }

        if inside_target_class {
            if trimmed.starts_with("fn ") || trimmed.starts_with("op ") {
                let parts: Vec<_> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mname = parts[1].split('(').next().unwrap_or("").trim();
                    inside_target_method = mname == method_name;
                }
            }

            if inside_target_method && trimmed.contains("return ") {
                if let Some(ret_idx) = trimmed.find("return ") {
                    let expr = trimmed[ret_idx + "return ".len()..].trim();
                    let first_word = expr.split('(').next().unwrap_or("").trim();
                    if !first_word.is_empty() && first_word.chars().next().map_or(false, |c| c.is_uppercase()) {
                        return Some(first_word.to_string());
                    }
                    if expr.starts_with('"') {
                        return Some("string".to_string());
                    }
                    if expr.starts_with('[') {
                        return Some("list".to_string());
                    }
                    if expr.starts_with('{') {
                        return Some("dict".to_string());
                    }
                }
            }

            if trimmed.starts_with('}') {
                inside_target_method = false;
            }
        }
    }
    None
}

fn collect_ast_dot_completions_for_class(
    stmts: &[Stmt],
    target_class: &str,
    seen: &mut std::collections::HashSet<String>,
    items: &mut Vec<CompletionItem>,
) {
    for stmt in stmts {
        if let StmtKind::Class { name, parent, methods, .. } = &stmt.kind {
            if name == target_class {
                for m in methods {
                    if !seen.contains(&m.name) {
                        seen.insert(m.name.clone());
                        items.push(CompletionItem {
                            label: m.name.clone(),
                            kind: Some(CompletionItemKind::METHOD),
                            detail: Some(format!("method {name}.{}()", m.name)),
                            ..Default::default()
                        });
                    }
                }
                if let Some(pvar) = parent {
                    collect_ast_dot_completions_for_class(stmts, &pvar.name, seen, items);
                }
            }
        }
    }
}

fn collect_dot_completions_for_class(
    source: &str,
    ast: Option<&[Stmt]>,
    receiver: &str,
    target_class: Option<&str>,
    items: &mut Vec<CompletionItem>,
) {

    let mut seen = std::collections::HashSet::new();

    // 0. Dynamically populate built-in type methods directly from engine runtime tables (crate::class::BUILTINS)
    if let Some(tc) = target_class {
        let tc_lower = tc.to_lowercase();
        for &builtin in quince::class::BUILTINS {
            let seed = builtin.seed();
            if seed.name == tc_lower || (tc_lower == "str" && seed.name == "string") {
                for &(m_name, _) in seed.methods {
                    if !seen.contains(m_name) {
                        seen.insert(m_name.to_string());
                        items.push(CompletionItem {
                            label: m_name.to_string(),
                            kind: Some(CompletionItemKind::METHOD),
                            detail: Some(format!("builtin method {}.{}()", seed.name, m_name)),
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }

    // 1. Try AST-based class methods if AST is present
    if let Some(stmts) = ast {
        if let Some(tc) = target_class {
            collect_ast_dot_completions_for_class(stmts, tc, &mut seen, items);
        } else {
            for stmt in stmts {
                if let StmtKind::Class { name, methods, .. } = &stmt.kind {
                    for m in methods {
                        if !seen.contains(&m.name) {
                            seen.insert(m.name.clone());
                            items.push(CompletionItem {
                                label: m.name.clone(),
                                kind: Some(CompletionItemKind::METHOD),
                                detail: Some(format!("method {name}.{}()", m.name)),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }


    // 2. Text-based scan for class methods & fields matching target_class
    let mut inside_class = false;
    let mut current_class_name = String::new();
    let mut brace_depth = 0;
    let mut class_brace_depth = 0;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with("class ") {
            let parts: Vec<_> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[1].split('{').next().unwrap_or("").trim().to_string();
                current_class_name = name;
                inside_class = true;
                class_brace_depth = brace_depth;
            }
        }

        let is_matching_class = target_class.map_or(true, |tc| current_class_name == tc);

        if inside_class && is_matching_class {
            // Class methods
            if trimmed.starts_with("fn ") || trimmed.starts_with("op ") {
                let parts: Vec<_> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mname = parts[1].split('(').next().unwrap_or("").trim();
                    if !mname.is_empty() && !seen.contains(mname) && !KEYWORDS.contains(&mname) {
                        seen.insert(mname.to_string());
                        items.push(CompletionItem {
                            label: mname.to_string(),
                            kind: Some(CompletionItemKind::METHOD),
                            detail: Some(format!("method {current_class_name}.{mname}()")),
                            ..Default::default()
                        });
                    }
                }
            }

            // Fields inside class body (e.g. self.x = x)
            for part in line.split(|c: char| !(c == '.' || c == '_' || c.is_alphanumeric())) {
                if let Some(idx) = part.find('.') {
                    let recv = part[..idx].trim();
                    let field = part[idx + 1..].trim();
                    if (recv == "self" || recv == receiver)
                        && !field.is_empty()
                        && field.chars().all(|c| c == '_' || c.is_alphanumeric())
                    {
                        if !seen.contains(field) && !KEYWORDS.contains(&field) {
                            seen.insert(field.to_string());
                            items.push(CompletionItem {
                                label: field.to_string(),
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(format!("property {current_class_name}.{field}")),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        // Fields assigned on receiver explicitly (e.g. p.z = 99)
        for part in line.split(|c: char| !(c == '.' || c == '_' || c.is_alphanumeric())) {
            if let Some(idx) = part.find('.') {
                let recv = part[..idx].trim();
                let field = part[idx + 1..].trim();
                if recv == receiver
                    && !field.is_empty()
                    && field.chars().all(|c| c == '_' || c.is_alphanumeric())
                {
                    if !seen.contains(field) && !KEYWORDS.contains(&field) {
                        seen.insert(field.to_string());
                        items.push(CompletionItem {
                            label: field.to_string(),
                            kind: Some(CompletionItemKind::FIELD),
                            detail: Some(format!("property {field}")),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        for c in line.chars() {
            if c == '{' {
                brace_depth += 1;
            } else if c == '}' {
                if brace_depth > 0 {
                    brace_depth -= 1;
                }
                if inside_class && brace_depth <= class_brace_depth {
                    inside_class = false;
                }
            }
        }
    }
}

fn collect_ast_completions(source: &str, stmts: &[Stmt], pos: Position, items: &mut Vec<CompletionItem>) {
    for stmt in stmts {
        let stmt_pos = offset_to_position(source, stmt.span.start as usize);
        if stmt_pos.line > pos.line {
            continue;
        }

        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                items.push(CompletionItem {
                    label: decl.name.clone(),
                    kind: Some(CompletionItemKind::FUNCTION),
                    detail: Some(format!("fn {}(...)", decl.name)),
                    ..Default::default()
                });
                for p in &decl.params {
                    items.push(CompletionItem {
                        label: p.name.clone(),
                        kind: Some(CompletionItemKind::VARIABLE),
                        detail: Some("parameter".to_string()),
                        ..Default::default()
                    });
                }
                collect_ast_completions(source, &decl.body.stmts, pos, items);
            }
            StmtKind::Class { name, methods, .. } => {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    detail: Some(format!("class {name}")),
                    ..Default::default()
                });
                for m in methods {
                    items.push(CompletionItem {
                        label: m.name.clone(),
                        kind: Some(CompletionItemKind::METHOD),
                        detail: Some(format!("method {}.{}()", name, m.name)),
                        ..Default::default()
                    });
                    for p in &m.params {
                        items.push(CompletionItem {
                            label: p.name.clone(),
                            kind: Some(CompletionItemKind::VARIABLE),
                            detail: Some("parameter".to_string()),
                            ..Default::default()
                        });
                    }
                    collect_ast_completions(source, &m.body.stmts, pos, items);
                }
            }
            StmtKind::Let { name, bind, .. } => {
                items.push(CompletionItem {
                    label: name.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some(format!("{} variable", bind.word())),
                    ..Default::default()
                });
            }
            StmtKind::If { then, otherwise, .. } => {
                collect_ast_completions(source, &then.stmts, pos, items);
                if let Some(other) = otherwise {
                    collect_ast_completions(source, std::slice::from_ref(other.as_ref()), pos, items);
                }
            }
            StmtKind::While { body, .. } => collect_ast_completions(source, &body.stmts, pos, items),
            StmtKind::For { var, body, .. } => {
                items.push(CompletionItem {
                    label: var.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("loop variable".to_string()),
                    ..Default::default()
                });
                collect_ast_completions(source, &body.stmts, pos, items);
            }
            StmtKind::Try { body, handler, binding, .. } => {
                items.push(CompletionItem {
                    label: binding.clone(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("caught error variable".to_string()),
                    ..Default::default()
                });
                collect_ast_completions(source, &body.stmts, pos, items);
                collect_ast_completions(source, &handler.stmts, pos, items);
            }
            StmtKind::Block(block) => collect_ast_completions(source, &block.stmts, pos, items),
            _ => {}
        }
    }
}

fn collect_text_variable_completions(source: &str, pos: Position, items: &mut Vec<CompletionItem>) {
    let mut seen: std::collections::HashSet<String> = items.iter().map(|i| i.label.clone()).collect();
    let is_ident_start = |c: char| c == '_' || c.is_alphabetic();

    for (line_idx, line) in source.lines().enumerate() {
        if line_idx > pos.line as usize {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        for word in line.split(|c: char| !(c == '_' || c.is_alphanumeric())) {
            if word.len() > 1
                && is_ident_start(word.chars().next().unwrap())
                && !KEYWORDS.contains(&word)
                && !seen.contains(word)
            {
                seen.insert(word.to_string());
                items.push(CompletionItem {
                    label: word.to_string(),
                    kind: Some(CompletionItemKind::VARIABLE),
                    detail: Some("identifier".to_string()),
                    ..Default::default()
                });
            }
        }
    }
}


fn get_hover(state: Option<&DocumentState>, pos: Position) -> Option<Hover> {
    let state = state?;
    let word = get_word_at_position(&state.text, pos)?;

    // Builtin Hover Docs
    let doc = match word.as_str() {
        "print" => Some("**print(...)**\n\nPrints one or more values to standard output."),
        "type" => Some("**type(val)**\n\nReturns the type representation or type name of a value."),
        "len" => Some("**len(val)**\n\nReturns the length of a string, list, or dict."),
        "int" => Some("**int**\n\nBuilt-in integer type and converter function."),
        "float" => Some("**float**\n\nBuilt-in floating-point number type and converter function."),
        "string" => Some("**string**\n\nBuilt-in text string type and converter function."),
        "list" => Some("**list**\n\nBuilt-in dynamic array type and converter function."),
        "dict" => Some("**dict**\n\nBuilt-in key-value dictionary type."),
        "self" => Some("**self**\n\nReference to the current class instance inside a method."),
        "super" => Some("**super**\n\nReference to the parent class inside a method."),
        _ => None,
    };

    if let Some(content) = doc {
        return Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(content.to_string())),
            range: None,
        });
    }

    // Check user declarations in AST
    if let Some(ast) = &state.ast {
        if let Some(info) = find_decl_hover(ast, &word) {
            return Some(Hover {
                contents: HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
                    language: "quince".to_string(),
                    value: info,
                })),
                range: None,
            });
        }
    }

    None
}

fn find_decl_hover(stmts: &[Stmt], target: &str) -> Option<String> {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } if decl.name == target => {
                let params: Vec<_> = decl.params.iter().map(|p| p.name.as_str()).collect();
                return Some(format!("fn {}({})", decl.name, params.join(", ")));
            }
            StmtKind::Class { name, parent, .. } if name == target => {
                if let Some(p) = parent {
                    return Some(format!("class {name} extends {}", p.name));
                } else {
                    return Some(format!("class {name}"));
                }
            }
            StmtKind::Let { name, bind, .. } if name == target => {
                return Some(format!("{} {name}", bind.word()));
            }
            _ => {}
        }
    }
    None
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
    let (callee, receiver, active_param) = find_call_context(&state.text, pos)?;

    if callee == "print" {
        return Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: "print(*args)".to_string(),
                documentation: Some(lsp_types::Documentation::String(
                    "Prints values to standard output.".to_string(),
                )),
                parameters: Some(vec![ParameterInformation {
                    label: ParameterLabel::Simple("*args".to_string()),
                    documentation: None,
                }]),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        });
    }

    if callee == "type" {
        return Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: "type(value)".to_string(),
                documentation: Some(lsp_types::Documentation::String(
                    "Returns the type of a value.".to_string(),
                )),
                parameters: Some(vec![ParameterInformation {
                    label: ParameterLabel::Simple("value".to_string()),
                    documentation: None,
                }]),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        });
    }

    if callee == "Error" || quince::error::ERROR_KINDS.iter().any(|k| k.class_name() == callee) {
        return Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: format!("{callee}(message)"),
                documentation: Some(lsp_types::Documentation::String(
                    format!("Constructs a `{callee}` instance with a message string."),
                )),
                parameters: Some(vec![ParameterInformation {
                    label: ParameterLabel::Simple("message".to_string()),
                    documentation: None,
                }]),
                active_parameter: Some(active_param),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_param),
        });
    }


    let target_class = if let Some(recv) = &receiver {
        infer_receiver_class(&state.text, recv, pos)
    } else if callee.chars().next().map_or(false, |c| c.is_uppercase()) {
        Some(callee.clone())
    } else {
        None
    };

    let sig_info = find_function_signature(&state.text, state.ast.as_deref(), &callee, target_class.as_deref())?;

    Some(SignatureHelp {
        signatures: vec![sig_info],
        active_signature: Some(0),
        active_parameter: Some(active_param),
    })
}

fn find_call_context(source: &str, pos: Position) -> Option<(String, Option<String>, u32)> {
    let mut total_offset = 0;
    for (idx, line) in source.lines().enumerate() {
        if idx == pos.line as usize {
            total_offset += (pos.character as usize).min(line.len());
            break;
        }
        total_offset += line.len() + 1;
    }
    let text_before = &source[..total_offset.min(source.len())];

    let mut depth = 0;
    let mut comma_count = 0;
    let mut open_paren_idx = None;

    let chars: Vec<(usize, char)> = text_before.char_indices().collect();
    for i in (0..chars.len()).rev() {
        let (_, c) = chars[i];
        if c == ')' {
            depth += 1;
        } else if c == '(' {
            if depth == 0 {
                open_paren_idx = Some(chars[i].0);
                break;
            } else {
                depth -= 1;
            }
        } else if c == ',' && depth == 0 {
            comma_count += 1;
        }
    }

    let open_idx = open_paren_idx?;
    let text_before_paren = text_before[..open_idx].trim_end();

    let end_ident = text_before_paren.len();
    let start_ident = text_before_paren
        .rfind(|c: char| !(c == '_' || c.is_alphanumeric()))
        .map_or(0, |i| i + 1);

    if start_ident >= end_ident {
        return None;
    }
    let callee = text_before_paren[start_ident..end_ident].to_string();

    let mut receiver = None;
    let before_ident = text_before_paren[..start_ident].trim_end();
    if before_ident.ends_with('.') {
        let recv_part = before_ident[..before_ident.len() - 1].trim_end();
        if !recv_part.is_empty() {
            let mut cleaned = recv_part;
            if cleaned.ends_with(')') {
                if let Some(open_paren) = cleaned.rfind('(') {
                    cleaned = cleaned[..open_paren].trim_end();
                }
            }

            if cleaned.ends_with('"') || cleaned.ends_with(']') || cleaned.ends_with('}') {
                receiver = Some(cleaned.to_string());
            } else {
                let rstart = cleaned
                    .rfind(|c: char| !(c == '_' || c == '.' || c.is_alphanumeric()))
                    .map_or(0, |i| i + 1);
                receiver = Some(cleaned[rstart..].to_string());
            }
        }
    }


    Some((callee, receiver, comma_count))
}

fn find_function_signature(
    source: &str,
    ast: Option<&[Stmt]>,
    callee: &str,
    target_class: Option<&str>,
) -> Option<SignatureInformation> {
    // 0. Search Built-in class methods dynamically from engine runtime tables (quince::class::BUILTINS)
    if let Some(tc) = target_class {
        let tc_lower = tc.to_lowercase();
        for &builtin in quince::class::BUILTINS {
            let seed = builtin.seed();
            if seed.name == tc_lower || (tc_lower == "str" && seed.name == "string") {
                for &(m_name, native) in seed.methods {
                    if m_name == callee {
                        let params = match native.arity {
                            Some(0) => vec![],
                            Some(1) => vec!["arg".to_string()],
                            Some(n) => (1..=n).map(|i| format!("arg{i}")).collect(),
                            None => vec!["*args".to_string()],
                        };
                        let label = format!("fn {}({})", m_name, params.join(", "));
                        let param_infos = params
                            .into_iter()
                            .map(|p| ParameterInformation {
                                label: ParameterLabel::Simple(p),
                                documentation: None,
                            })
                            .collect();
                        return Some(SignatureInformation {
                            label,
                            documentation: Some(lsp_types::Documentation::String(
                                format!("Builtin method `{}.{}()`", seed.name, m_name),
                            )),
                            parameters: Some(param_infos),
                            active_parameter: None,
                        });
                    }
                }
            }
        }
    }

    if let Some(stmts) = ast {
        if let Some(sig) = find_ast_signature(stmts, callee, target_class) {
            return Some(sig);
        }
    }

    find_text_signature(source, callee, target_class)
}



fn find_ast_signature(
    stmts: &[Stmt],
    callee: &str,
    target_class: Option<&str>,
) -> Option<SignatureInformation> {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                if target_class.is_none() && decl.name == callee {
                    let params: Vec<String> = decl.params.iter().map(|p| p.name.clone()).collect();
                    let label = format!("fn {}({})", decl.name, params.join(", "));
                    let param_infos = params
                        .into_iter()
                        .map(|p| ParameterInformation {
                            label: ParameterLabel::Simple(p),
                            documentation: None,
                        })
                        .collect();
                    return Some(SignatureInformation {
                        label,
                        documentation: None,
                        parameters: Some(param_infos),
                        active_parameter: None,
                    });
                }
            }
            StmtKind::Class { name, parent, methods, .. } => {
                let is_constructor_call = target_class.map_or(name == callee, |tc| tc == name && callee == name);

                if is_constructor_call {
                    for m in methods {
                        if m.name == "init" {
                            let params: Vec<String> = m.params.iter().filter(|p| !p.receiver && p.name != "self").map(|p| p.name.clone()).collect();
                            let label = format!("{}({})", name, params.join(", "));
                            let param_infos = params
                                .into_iter()
                                .map(|p| ParameterInformation {
                                    label: ParameterLabel::Simple(p),
                                    documentation: None,
                                })
                                .collect();
                            return Some(SignatureInformation {
                                label,
                                documentation: None,
                                parameters: Some(param_infos),
                                active_parameter: None,
                            });
                        }
                    }
                    if let Some(pvar) = parent {
                        if let Some(mut parent_sig) = find_ast_signature(stmts, &pvar.name, Some(&pvar.name)) {
                            let params_str = parent_sig.label.split('(').nth(1).unwrap_or(")");
                            parent_sig.label = format!("{name}({params_str}");
                            return Some(parent_sig);
                        }
                    }
                    return Some(SignatureInformation {
                        label: format!("{}()", name),
                        documentation: None,
                        parameters: Some(vec![]),
                        active_parameter: None,
                    });
                }

                if target_class.map_or(true, |tc| tc == name.as_str()) {
                    for m in methods {
                        if m.name == callee {
                            let params: Vec<String> = m.params.iter().filter(|p| !p.receiver && p.name != "self").map(|p| p.name.clone()).collect();
                            let label = format!("fn {}({})", m.name, params.join(", "));
                            let param_infos = params
                                .into_iter()
                                .map(|p| ParameterInformation {
                                    label: ParameterLabel::Simple(p),
                                    documentation: None,
                                })
                                .collect();
                            return Some(SignatureInformation {
                                label,
                                documentation: None,
                                parameters: Some(param_infos),
                                active_parameter: None,
                            });
                        }
                    }
                    if let Some(pvar) = parent {
                        if let Some(parent_sig) = find_ast_signature(stmts, callee, Some(&pvar.name)) {
                            return Some(parent_sig);
                        }
                    }
                }
            }


            _ => {}
        }
    }
    None
}

fn find_text_signature(
    source: &str,
    callee: &str,
    target_class: Option<&str>,
) -> Option<SignatureInformation> {
    let mut inside_class = false;
    let mut current_class = String::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("class ") {
            let parts: Vec<_> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                current_class = parts[1].split('{').next().unwrap_or("").trim().to_string();
                inside_class = true;
            }
        }

        if target_class.is_some() && current_class == callee && (trimmed.starts_with("op init") || trimmed.starts_with("init")) {
            let params = extract_params_from_line(trimmed);
            let filtered_params: Vec<String> = params.into_iter().filter(|p| p != "self").collect();
            let label = format!("{}({})", callee, filtered_params.join(", "));
            let param_infos = filtered_params
                .into_iter()
                .map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p),
                    documentation: None,
                })
                .collect();
            return Some(SignatureInformation {
                label,
                documentation: None,
                parameters: Some(param_infos),
                active_parameter: None,
            });
        }

        if trimmed.starts_with("fn ") || trimmed.starts_with("op ") {
            let parts: Vec<_> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[1].split('(').next().unwrap_or("").trim();
                if name == callee {
                    let is_matching = if inside_class {
                        target_class.map_or(true, |tc| tc == current_class)
                    } else {
                        target_class.is_none()
                    };

                    if is_matching {
                        let params = extract_params_from_line(trimmed);
                        let filtered_params: Vec<String> = params.into_iter().filter(|p| p != "self").collect();
                        let label = format!("fn {}({})", callee, filtered_params.join(", "));
                        let param_infos = filtered_params
                            .into_iter()
                            .map(|p| ParameterInformation {
                                label: ParameterLabel::Simple(p),
                                documentation: None,
                            })
                            .collect();
                        return Some(SignatureInformation {
                            label,
                            documentation: None,
                            parameters: Some(param_infos),
                            active_parameter: None,
                        });
                    }
                }
            }
        }

        if trimmed.starts_with('}') {
            inside_class = false;
        }
    }

    if let Some(tc) = target_class {
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("class ") {
                let parts: Vec<_> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 && parts[1].split('{').next().unwrap_or("").trim() == tc {
                    if let Some(ext_idx) = parts.iter().position(|&p| p == "extends") {
                        if ext_idx + 1 < parts.len() {
                            let parent_name = parts[ext_idx + 1].split('{').next().unwrap_or("").trim();
                            if callee == tc {
                                if let Some(mut parent_sig) = find_text_signature(source, parent_name, Some(parent_name)) {
                                    let params_str = parent_sig.label.split('(').nth(1).unwrap_or(")");
                                    parent_sig.label = format!("{tc}({params_str}");
                                    return Some(parent_sig);
                                }
                            } else {
                                if let Some(parent_sig) = find_text_signature(source, callee, Some(parent_name)) {
                                    return Some(parent_sig);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if callee.chars().next().map_or(false, |c| c.is_uppercase()) {
        return Some(SignatureInformation {
            label: format!("{}()", callee),
            documentation: None,
            parameters: Some(vec![]),
            active_parameter: None,
        });
    }


    None
}

fn extract_params_from_line(line: &str) -> Vec<String> {
    let open = match line.find('(') {
        Some(i) => i,
        None => return vec![],
    };
    let close = match line.find(')') {
        Some(i) => i,
        None => line.len(),
    };
    if open >= close {
        return vec![];
    }
    let param_str = &line[open + 1..close];
    param_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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
        let code = "class Point {\n    op init(x, y) {\n        self.x = x\n        self.y = y\n    }\n}\n\nlet p = Point(3, ";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 7, character: 17 }; // Cursor right after `, `
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "Point(x, y)");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn signature_help_extracts_method_params() {
        let code = "class Point {\n    fn scaled(k) {\n        return Point(self.x * k, self.y * k)\n    }\n}\n\nlet p = Point(3, 4)\np.scaled(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 7, character: 9 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "fn scaled(k)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_extracts_builtin_method_params() {
        let code = "let text = \"hello world\"\ntext.split(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 1, character: 11 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "fn split(arg1, arg2)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_extracts_literal_string_method_params() {
        let code = "\"test\".split(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 0, character: 13 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help for literal string method");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "fn split(arg1, arg2)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_extracts_error_class_params() {
        let code = "TypeError(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 0, character: 10 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help for TypeError");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "TypeError(message)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn completion_excludes_symbols_defined_after_cursor() {
        let code = "let defined_above = 1\n\nlet defined_below = 2";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 1, character: 0 };
        let items = get_completions(Some(&state), pos);
        let labels: Vec<String> = items.into_iter().map(|i| i.label).collect();
        assert!(labels.contains(&"defined_above".to_string()));
        assert!(!labels.contains(&"defined_below".to_string()));
    }

    #[test]
    fn signature_help_extracts_inherited_constructor_params() {
        let code = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Cat extends Animal {}\nCat(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 6, character: 4 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help for inherited constructor");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "Cat(name)");
        assert_eq!(help.active_parameter, Some(0));
    }

    #[test]
    fn signature_help_extracts_super_method_params() {
        let code = "class Animal {\n    op init(name) {\n        self.name = name\n    }\n}\nclass Dog extends Animal {\n    op init(name, breed) {\n        super.init(";
        let state = DocumentState {
            text: code.to_string(),
            ast: parse_ast_lenient(code),
        };
        let pos = Position { line: 7, character: 19 };
        let help = get_signature_help(Some(&state), pos).expect("Expected signature help for super.init");
        assert_eq!(help.signatures.len(), 1);
        assert_eq!(help.signatures[0].label, "fn init(name)");
        assert_eq!(help.active_parameter, Some(0));
    }
}

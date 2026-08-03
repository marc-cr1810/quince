//! Go-to-definition, AST-aware references, symbol rename, and document/workspace outlines.

use std::collections::HashMap;
use lsp_types::{
    DocumentSymbol, GotoDefinitionResponse, Location, Position,
    Range, SymbolInformation, SymbolKind, Uri as Url, WorkspaceEdit, TextEdit,
};

use quince::syntax::ast::{Expr, ExprKind, FnDecl, Stmt, StmtKind};
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

    // 1. Search current file's AST first
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

    // 2. Search other open documents in the workspace
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

    // 3. Search non-open files in the workspace directory (Cross-file lookup)
    if let Ok(parsed_url) = url::Url::parse(uri.as_str())
        && let Ok(file_path) = parsed_url.to_file_path()
        && let Some(parent_dir) = file_path.parent()
    {
        let candidates = [
            parent_dir.join(format!("{word}.qn")),
            parent_dir.join(word.clone()).join("mod.qn"),
        ];

        for cand in candidates {
            if cand.exists() && cand.is_file() {
                if let Ok(source) = std::fs::read_to_string(&cand) {
                    let (ast, _) = quince::compile_recovering(&source);
                    if !ast.is_empty() && let Some(span) = find_decl_span(&ast, &word) {
                        let range = span_to_range(&source, span);
                        if let Ok(target_url) = url::Url::from_file_path(&cand)
                            && let Ok(target_uri) = target_url.as_str().parse()
                        {
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: target_uri,
                                range,
                            }));
                        }
                    }
                }
            }
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
            StmtKind::Alias { name, .. } if name == target => return Some(stmt.span),
            StmtKind::Class { methods, fields, .. } => {
                for m in methods {
                    if m.name == target {
                        return Some(m.body.span);
                    }
                }
                for f in fields {
                    if f.name == target {
                        return Some(f.name_span);
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

    let mut ranges = Vec::new();
    if let Some(ast) = state.ast() {
        collect_ast_references(&state.text, ast, &word, &mut ranges);
    } else {
        // Fallback to text matching if AST recovery failed completely
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
                    ranges.push(Range {
                        start: Position { line: line_idx as u32, character: col as u32 },
                        end: Position { line: line_idx as u32, character: (col + len) as u32 },
                    });
                }
                start_col = col + len.max(1);
            }
        }
    }

    ranges
        .into_iter()
        .map(|range| Location {
            uri: uri.clone(),
            range,
        })
        .collect()
}

fn collect_ast_references(source: &str, stmts: &[Stmt], target: &str, ranges: &mut Vec<Range>) {
    for stmt in stmts {
        visit_stmt(source, stmt, target, ranges);
    }
}

fn visit_stmt(source: &str, stmt: &Stmt, target: &str, ranges: &mut Vec<Range>) {
    match &stmt.kind {
        StmtKind::Let { name, name_span, value, .. } => {
            if name == target {
                if let Some(r) = find_name_range(source, *name_span, target) {
                    ranges.push(r);
                }
            }
            visit_expr(source, value, target, ranges);
        }
        StmtKind::Fn { decl, .. } => {
            visit_fndecl(source, decl, stmt.span, target, ranges);
        }
        StmtKind::Class { name, parent, methods, fields, .. } => {
            if name == target {
                if let Some(r) = find_name_range(source, stmt.span, target) {
                    ranges.push(r);
                }
            }
            if let Some(p) = parent && p.name == target {
                if let Some(r) = find_name_range(source, stmt.span, target) {
                    ranges.push(r);
                }
            }
            for field in fields {
                if field.name == target {
                    if let Some(r) = find_name_range(source, field.name_span, target) {
                        ranges.push(r);
                    }
                }
                visit_expr(source, &field.value, target, ranges);
            }
            for m in methods {
                visit_fndecl(source, m, m.body.span, target, ranges);
            }
        }
        StmtKind::Alias { name, name_span, .. } => {
            if name == target {
                if let Some(r) = find_name_range(source, *name_span, target) {
                    ranges.push(r);
                }
            }
        }
        StmtKind::Extend { target: var, methods, .. } => {
            if var.name == target {
                if let Some(r) = find_name_range(source, stmt.span, target) {
                    ranges.push(r);
                }
            }
            for m in methods {
                visit_fndecl(source, m, m.body.span, target, ranges);
            }
        }
        StmtKind::Import { module, module_span, names } => {
            if module == target {
                if let Some(r) = find_name_range(source, *module_span, target) {
                    ranges.push(r);
                }
            }
            match names {
                quince::syntax::ast::ImportNames::Names(list) => {
                    for imported in list {
                        if imported.name == target {
                            if let Some(r) = find_name_range(source, imported.span, target) {
                                ranges.push(r);
                            }
                        }
                    }
                }
                quince::syntax::ast::ImportNames::Module => {}
            }
        }
        StmtKind::If { cond, then, otherwise } => {
            visit_expr(source, cond, target, ranges);
            collect_ast_references(source, &then.stmts, target, ranges);
            if let Some(other) = otherwise {
                visit_stmt(source, other, target, ranges);
            }
        }
        StmtKind::While { cond, body } => {
            visit_expr(source, cond, target, ranges);
            collect_ast_references(source, &body.stmts, target, ranges);
        }
        StmtKind::For { var, iter, body, .. } => {
            if var == target {
                if let Some(r) = find_name_range(source, stmt.span, target) {
                    ranges.push(r);
                }
            }
            visit_expr(source, iter, target, ranges);
            collect_ast_references(source, &body.stmts, target, ranges);
        }
        StmtKind::Try { body, binding, handler, .. } => {
            collect_ast_references(source, &body.stmts, target, ranges);
            if binding == target {
                if let Some(r) = find_name_range(source, stmt.span, target) {
                    ranges.push(r);
                }
            }
            collect_ast_references(source, &handler.stmts, target, ranges);
        }
        StmtKind::Return(Some(expr)) | StmtKind::Throw(expr) | StmtKind::Expr(expr) => {
            visit_expr(source, expr, target, ranges);
        }
        StmtKind::Block(block) => {
            collect_ast_references(source, &block.stmts, target, ranges);
        }
        StmtKind::Return(None) => {}
    }
}

fn visit_fndecl(source: &str, decl: &FnDecl, span: Span, target: &str, ranges: &mut Vec<Range>) {
    if decl.name == target {
        if let Some(r) = find_name_range(source, span, target) {
            ranges.push(r);
        }
    }
    for param in &decl.params {
        if param.name == target {
            if let Some(r) = find_name_range(source, span, target) {
                ranges.push(r);
            }
        }
    }
    collect_ast_references(source, &decl.body.stmts, target, ranges);
}

fn visit_expr(source: &str, expr: &Expr, target: &str, ranges: &mut Vec<Range>) {
    match &expr.kind {
        ExprKind::Var(var) => {
            if var.name == target {
                if let Some(r) = find_name_range(source, expr.span, target) {
                    ranges.push(r);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            visit_expr(source, callee, target, ranges);
            for arg in args {
                visit_expr(source, arg, target, ranges);
            }
        }
        ExprKind::Field { target: recv, name, .. } => {
            visit_expr(source, recv, target, ranges);
            if name == target {
                if let Some(r) = find_name_range(source, expr.span, target) {
                    ranges.push(r);
                }
            }
        }
        ExprKind::Unary { rhs, .. } => visit_expr(source, rhs, target, ranges),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Logical { lhs, rhs, .. }
        | ExprKind::Coalesce { lhs, rhs }
        | ExprKind::Assign { target: lhs, value: rhs } => {
            visit_expr(source, lhs, target, ranges);
            visit_expr(source, rhs, target, ranges);
        }
        ExprKind::List(items) => {
            for item in items {
                visit_expr(source, item, target, ranges);
            }
        }
        ExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                visit_expr(source, k, target, ranges);
                visit_expr(source, v, target, ranges);
            }
        }
        ExprKind::Index { target: recv, index } => {
            visit_expr(source, recv, target, ranges);
            visit_expr(source, index, target, ranges);
        }
        ExprKind::Slice { target: recv, start, end } => {
            visit_expr(source, recv, target, ranges);
            if let Some(s) = start { visit_expr(source, s, target, ranges); }
            if let Some(e) = end { visit_expr(source, e, target, ranges); }
        }
        ExprKind::Is { value, .. } => visit_expr(source, value, target, ranges),
        ExprKind::Chain(inner) => visit_expr(source, inner, target, ranges),
        ExprKind::Super { name, parent, receiver } => {
            if parent.name == target || receiver.name == target || name == target {
                if let Some(r) = find_name_range(source, expr.span, target) {
                    ranges.push(r);
                }
            }
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Nil => {}
    }
}

fn fn_decl_signature(decl: &FnDecl) -> String {
    let params = decl
        .params
        .iter()
        .map(|p| {
            if let Some(ty) = &p.ty {
                format!("{}: {}", p.name, ty.written())
            } else {
                p.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = decl
        .returns
        .as_ref()
        .map(|r| format!(": {}", r.written()))
        .unwrap_or_default();
    format!("fn {}({}){}", decl.name, params, ret)
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

pub(crate) fn get_hierarchical_document_symbols(_uri: &Url, state: Option<&DocumentState>) -> Vec<DocumentSymbol> {
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
                let name_range = find_name_range(&state.text, stmt.span, &decl.name).unwrap_or(stmt_range);
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: decl.name.clone(),
                    detail: Some(fn_decl_signature(decl)),
                    kind: SymbolKind::FUNCTION,
                    tags: None,
                    deprecated: None,
                    range: stmt_range,
                    selection_range: name_range,
                    children: None,
                });
            }
            StmtKind::Class { name, methods, fields, parent, .. } => {
                let name_range = find_name_range(&state.text, stmt.span, name).unwrap_or(stmt_range);
                let mut children = Vec::new();

                for field in fields {
                    let field_range = span_to_range(&state.text, field.value.span);
                    let field_name_range = find_name_range(&state.text, field.name_span, &field.name).unwrap_or(field_range);
                    #[allow(deprecated)]
                    children.push(DocumentSymbol {
                        name: field.name.clone(),
                        detail: field.ty.as_ref().map(|t| t.written()),
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: field_name_range,
                        selection_range: field_name_range,
                        children: None,
                    });
                }

                for m in methods {
                    let m_range = span_to_range(&state.text, m.body.span);
                    let m_name_range = find_name_range(&state.text, m.body.span, &m.name).unwrap_or(m_range);
                    #[allow(deprecated)]
                    children.push(DocumentSymbol {
                        name: m.name.clone(),
                        detail: Some(fn_decl_signature(m)),
                        kind: SymbolKind::METHOD,
                        tags: None,
                        deprecated: None,
                        range: m_range,
                        selection_range: m_name_range,
                        children: None,
                    });
                }

                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: name.clone(),
                    detail: parent.as_ref().map(|p| format!("extends {}", p.name)),
                    kind: SymbolKind::CLASS,
                    tags: None,
                    deprecated: None,
                    range: stmt_range,
                    selection_range: name_range,
                    children: if children.is_empty() { None } else { Some(children) },
                });
            }
            StmtKind::Let { name, name_span, bind, ty, .. } => {
                let name_range = find_name_range(&state.text, *name_span, name).unwrap_or(stmt_range);
                let kind = if *bind == quince::syntax::ast::BindKind::Const {
                    SymbolKind::CONSTANT
                } else {
                    SymbolKind::VARIABLE
                };
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: name.clone(),
                    detail: ty.as_ref().map(|t| t.written()),
                    kind,
                    tags: None,
                    deprecated: None,
                    range: stmt_range,
                    selection_range: name_range,
                    children: None,
                });
            }
            StmtKind::Alias { name, name_span, ty, .. } => {
                let name_range = find_name_range(&state.text, *name_span, name).unwrap_or(stmt_range);
                #[allow(deprecated)]
                symbols.push(DocumentSymbol {
                    name: name.clone(),
                    detail: Some(format!("alias = {}", ty.written())),
                    kind: SymbolKind::STRUCT,
                    tags: None,
                    deprecated: None,
                    range: stmt_range,
                    selection_range: name_range,
                    children: None,
                });
            }
            _ => {}
        }
    }

    symbols
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

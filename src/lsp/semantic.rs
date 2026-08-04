//! Semantic tokens — the highlighting the editor cannot get from a grammar.
//!
//! The TextMate grammar in `editors/` colours what it can see lexically. This is
//! for the rest: a name that is a class because the pass says so, not because it
//! starts with a capital.


use lsp_types::{SemanticToken, SemanticTokens};

use quince::syntax::ast::{Expr, ExprKind, Stmt, StmtKind, TypeExpr, TypeName};
use quince::syntax::token::Span;
use crate::lsp::DocumentState;
use crate::lsp::navigate::find_name_range;

#[derive(Clone, Copy)]
pub(crate) struct RawSemanticToken {
    line: u32,
    col: u32,
    len: u32,
    token_type: u32,
    modifiers: u32,
}

pub(crate) fn get_semantic_tokens(state: Option<&DocumentState>) -> SemanticTokens {
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

pub(crate) fn push_raw_token(
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

pub(crate) fn collect_type_expr_semantic_tokens(
    source: &str,
    ty: &TypeExpr,
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    match &ty.name {
        TypeName::Named(name) => {
            push_raw_token(source, ty.span, name, 6, 0, raw_tokens);
        }
        TypeName::Any => {
            push_raw_token(source, ty.span, "any", 6, 0, raw_tokens);
        }
    }
    for arg in &ty.args {
        collect_type_expr_semantic_tokens(source, arg, raw_tokens);
    }
}

pub(crate) fn collect_stmt_semantic_tokens(
    source: &str,
    stmts: &[Stmt],
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                // Function declaration (1), declaration modifier (1)
                push_raw_token(source, decl.name_span, &decl.name, 1, 1, raw_tokens);
                for param in &decl.params {
                    if param.receiver {
                        continue;
                    }
                    // Parameter (4), declaration modifier (1)
                    push_raw_token(source, param.span, &param.name, 4, 1, raw_tokens);
                    if let Some(param_ty) = &param.ty {
                        collect_type_expr_semantic_tokens(source, param_ty, raw_tokens);
                    }
                }
                if let Some(ret_ty) = &decl.returns {
                    collect_type_expr_semantic_tokens(source, ret_ty, raw_tokens);
                }
                collect_stmt_semantic_tokens(source, &decl.body.stmts, raw_tokens);
            }
            StmtKind::Class { name, parent, fields, methods, .. } => {
                // Class declaration (0), declaration modifier (1)
                push_raw_token(source, stmt.span, name, 0, 1, raw_tokens);
                if let Some(p) = parent {
                    // Parent class reference (0)
                    push_raw_token(source, stmt.span, &p.name, 0, 0, raw_tokens);
                }
                for field in fields {
                    push_raw_token(source, field.name_span, &field.name, 5, 1, raw_tokens);
                    if let Some(field_ty) = &field.ty {
                        collect_type_expr_semantic_tokens(source, field_ty, raw_tokens);
                    }
                    collect_expr_semantic_tokens(source, &field.value, raw_tokens);
                }
                for m in methods {
                    // Method declaration (2), declaration modifier (1)
                    push_raw_token(source, m.name_span, &m.name, 2, 1, raw_tokens);
                    for param in &m.params {
                        if param.receiver {
                            continue;
                        }
                        push_raw_token(source, param.span, &param.name, 4, 1, raw_tokens);
                        if let Some(param_ty) = &param.ty {
                            collect_type_expr_semantic_tokens(source, param_ty, raw_tokens);
                        }
                    }
                    if let Some(ret_ty) = &m.returns {
                        collect_type_expr_semantic_tokens(source, ret_ty, raw_tokens);
                    }
                    collect_stmt_semantic_tokens(source, &m.body.stmts, raw_tokens);
                }
            }
            StmtKind::Let { name, name_span, value, ty, .. } => {
                // Variable declaration (3), declaration modifier (1)
                push_raw_token(source, *name_span, name, 3, 1, raw_tokens);
                if let Some(t) = ty {
                    collect_type_expr_semantic_tokens(source, t, raw_tokens);
                }
                collect_expr_semantic_tokens(source, value, raw_tokens);
            }
            StmtKind::Alias { name, name_span, ty, .. } => {
                push_raw_token(source, *name_span, name, 6, 1, raw_tokens);
                collect_type_expr_semantic_tokens(source, ty, raw_tokens);
            }
            StmtKind::Extend { target, methods, .. } => {
                push_raw_token(source, stmt.span, &target.name, 0, 0, raw_tokens);
                for m in methods {
                    push_raw_token(source, m.name_span, &m.name, 2, 1, raw_tokens);
                    for param in &m.params {
                        if param.receiver {
                            continue;
                        }
                        push_raw_token(source, param.span, &param.name, 4, 1, raw_tokens);
                        if let Some(param_ty) = &param.ty {
                            collect_type_expr_semantic_tokens(source, param_ty, raw_tokens);
                        }
                    }
                    if let Some(ret_ty) = &m.returns {
                        collect_type_expr_semantic_tokens(source, ret_ty, raw_tokens);
                    }
                    collect_stmt_semantic_tokens(source, &m.body.stmts, raw_tokens);
                }
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
            StmtKind::Return(value) => {
                if let Some(v) = value {
                    collect_expr_semantic_tokens(source, v, raw_tokens);
                }
            }
            StmtKind::Throw(value) => collect_expr_semantic_tokens(source, value, raw_tokens),
            StmtKind::Block(block) => collect_stmt_semantic_tokens(source, &block.stmts, raw_tokens),
            _ => {}
        }
    }
}

pub(crate) fn collect_expr_semantic_tokens(
    source: &str,
    expr: &Expr,
    raw_tokens: &mut Vec<RawSemanticToken>,
) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Var(var) = &callee.kind {
                let is_class = var.name.chars().next().map_or(false, |c| c.is_ascii_uppercase());
                let token_type = if is_class { 0 } else { 1 };
                push_raw_token(source, callee.span, &var.name, token_type, 0, raw_tokens);
            } else {
                collect_expr_semantic_tokens(source, callee, raw_tokens);
            }
            for arg in args {
                collect_expr_semantic_tokens(source, arg, raw_tokens);
            }
        }
        ExprKind::Field { target, name, .. } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            // Member property / method (5)
            push_raw_token(source, expr.span, name, 5, 0, raw_tokens);
        }
        ExprKind::Var(var) => {
            if var.name == "self" || var.name == "super" {
                push_raw_token(source, expr.span, &var.name, 7, 0, raw_tokens); // Keyword (7)
            } else if var.name.chars().next().map_or(false, |c| c.is_ascii_uppercase()) {
                push_raw_token(source, expr.span, &var.name, 0, 0, raw_tokens); // Class reference (0)
            } else {
                push_raw_token(source, expr.span, &var.name, 3, 0, raw_tokens); // Variable (3)
            }
        }
        ExprKind::Is { value, ty } => {
            collect_expr_semantic_tokens(source, value, raw_tokens);
            collect_type_expr_semantic_tokens(source, ty, raw_tokens);
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Logical { lhs, rhs, .. }
        | ExprKind::Coalesce { lhs, rhs } => {
            collect_expr_semantic_tokens(source, lhs, raw_tokens);
            collect_expr_semantic_tokens(source, rhs, raw_tokens);
        }
        ExprKind::Unary { rhs, .. } => collect_expr_semantic_tokens(source, rhs, raw_tokens),
        ExprKind::List(items) => {
            for item in items {
                collect_expr_semantic_tokens(source, item, raw_tokens);
            }
        }
        ExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                collect_expr_semantic_tokens(source, k, raw_tokens);
                collect_expr_semantic_tokens(source, v, raw_tokens);
            }
        }
        ExprKind::Index { target, index } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            collect_expr_semantic_tokens(source, index, raw_tokens);
        }
        ExprKind::Slice { target, start, end } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            if let Some(s) = start { collect_expr_semantic_tokens(source, s, raw_tokens); }
            if let Some(e) = end { collect_expr_semantic_tokens(source, e, raw_tokens); }
        }
        ExprKind::Chain(inner) => collect_expr_semantic_tokens(source, inner, raw_tokens),
        ExprKind::Assign { target, value } => {
            collect_expr_semantic_tokens(source, target, raw_tokens);
            collect_expr_semantic_tokens(source, value, raw_tokens);
        }
        ExprKind::Super { name, .. } => {
            push_raw_token(source, expr.span, "super", 7, 0, raw_tokens); // Keyword (7)
            push_raw_token(source, expr.span, name, 2, 0, raw_tokens); // Method (2)
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Nil => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_annotations_produce_type_semantic_tokens() {
        let code = "let x: dict[string, any?] = nil";
        let state = DocumentState::new(code.to_string(), None);
        let tokens = get_semantic_tokens(Some(&state));
        // Verify type tokens are emitted (type token is index 6)
        let type_tokens_count = tokens
            .data
            .iter()
            .filter(|t| t.token_type == 6)
            .count();
        // dict, string, any -> 3 type tokens
        assert_eq!(type_tokens_count, 3);
    }

    #[test]
    fn return_expressions_produce_semantic_tokens() {
        let code = "class Point { op string() { return self.x } }";
        let state = DocumentState::new(code.to_string(), None);
        let tokens = get_semantic_tokens(Some(&state));
        // Verify self inside return statement gets KEYWORD token (7)
        let keyword_tokens_count = tokens
            .data
            .iter()
            .filter(|t| t.token_type == 7)
            .count();
        assert_eq!(keyword_tokens_count, 1);
    }

    #[test]
    fn parameter_matching_keyword_substring_does_not_override_fn_keyword() {
        let code = "fn rebindable(n: int) {\n    n = n + 1\n}";
        let state = DocumentState::new(code.to_string(), None);
        let tokens = get_semantic_tokens(Some(&state));
        // The first token should NOT be at col 1 (which would be 'n' in 'fn')
        for token in &tokens.data {
            if token.delta_line == 0 {
                assert_ne!(token.delta_start, 1, "Should not emit semantic token at 'n' in 'fn'");
            }
        }
    }

    #[test]
    fn class_constructors_emit_class_semantic_tokens() {
        let code = "let p = Point(5, 8)";
        let state = DocumentState::new(code.to_string(), None);
        let tokens = get_semantic_tokens(Some(&state));
        // Point in Point(5, 8) should emit a class token (0), not a function token (1)
        let class_tokens = tokens.data.iter().filter(|t| t.token_type == 0).count();
        assert_eq!(class_tokens, 1);
    }

    #[test]
    fn super_keyword_emits_keyword_semantic_token() {
        let code = "class Point3D extends Point { op init(x: int, y: int, z: int) { super.init(x, y) } }";
        let state = DocumentState::new(code.to_string(), None);
        let tokens = get_semantic_tokens(Some(&state));
        // super should emit KEYWORD token (7)
        let super_tokens = tokens.data.iter().filter(|t| t.token_type == 7).count();
        assert!(super_tokens >= 1, "super keyword should receive a Keyword (7) semantic token");
    }
}

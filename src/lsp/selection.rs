//! Selection range provider for AST expansion/shrinkage.

use lsp_types::{Position, SelectionRange};
use quince::syntax::ast::{Expr, ExprKind, Stmt, StmtKind};
use quince::syntax::token::Span;
use crate::lsp::DocumentState;
use crate::lsp::position::{position_to_offset, span_to_range};

pub(crate) fn get_selection_ranges(
    state: Option<&DocumentState>,
    positions: Vec<Position>,
) -> Vec<SelectionRange> {
    let mut results = Vec::new();
    let Some(state) = state else {
        return results;
    };
    let Some(ast) = state.ast() else {
        return results;
    };

    for pos in positions {
        let offset = position_to_offset(&state.text, pos) as u32;
        let mut spans = Vec::new();

        // Always include document span
        spans.push(Span { start: 0, end: state.text.len() as u32 });

        collect_containing_spans(&state.text, ast, offset, &mut spans);

        // Sort spans by length descending (largest outer node to smallest inner node)
        spans.sort_by_key(|s| std::cmp::Reverse(s.end.saturating_sub(s.start)));
        spans.dedup();

        let mut current_range: Option<SelectionRange> = None;
        for span in spans {
            let range = span_to_range(&state.text, span);
            current_range = Some(SelectionRange {
                range,
                parent: current_range.map(Box::new),
            });
        }

        if let Some(sr) = current_range {
            results.push(sr);
        }
    }

    results
}

fn collect_containing_spans(source: &str, stmts: &[Stmt], offset: u32, spans: &mut Vec<Span>) {
    for stmt in stmts {
        if stmt.span.start <= offset && offset <= stmt.span.end {
            spans.push(stmt.span);
            visit_stmt_selection(source, stmt, offset, spans);
        }
    }
}

fn visit_stmt_selection(source: &str, stmt: &Stmt, offset: u32, spans: &mut Vec<Span>) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            if value.span.start <= offset && offset <= value.span.end {
                visit_expr_selection(source, value, offset, spans);
            }
        }
        StmtKind::Fn { decl, .. } => {
            collect_containing_spans(source, &decl.body.stmts, offset, spans);
        }
        StmtKind::Class { methods, fields, .. } => {
            for field in fields {
                if field.value.span.start <= offset && offset <= field.value.span.end {
                    visit_expr_selection(source, &field.value, offset, spans);
                }
            }
            for m in methods {
                collect_containing_spans(source, &m.body.stmts, offset, spans);
            }
        }
        StmtKind::If { cond, then, otherwise } => {
            if cond.span.start <= offset && offset <= cond.span.end {
                visit_expr_selection(source, cond, offset, spans);
            }
            collect_containing_spans(source, &then.stmts, offset, spans);
            if let Some(other) = otherwise {
                visit_stmt_selection(source, other, offset, spans);
            }
        }
        StmtKind::While { cond, body } => {
            if cond.span.start <= offset && offset <= cond.span.end {
                visit_expr_selection(source, cond, offset, spans);
            }
            collect_containing_spans(source, &body.stmts, offset, spans);
        }
        StmtKind::For { iter, body, .. } => {
            if iter.span.start <= offset && offset <= iter.span.end {
                visit_expr_selection(source, iter, offset, spans);
            }
            collect_containing_spans(source, &body.stmts, offset, spans);
        }
        StmtKind::Try { body, handler, .. } => {
            collect_containing_spans(source, &body.stmts, offset, spans);
            collect_containing_spans(source, &handler.stmts, offset, spans);
        }
        StmtKind::Return(Some(expr)) | StmtKind::Throw(expr) | StmtKind::Expr(expr) => {
            if expr.span.start <= offset && offset <= expr.span.end {
                visit_expr_selection(source, expr, offset, spans);
            }
        }
        StmtKind::Block(block) => {
            collect_containing_spans(source, &block.stmts, offset, spans);
        }
        StmtKind::Alias { .. } | StmtKind::Extend { .. } | StmtKind::Import { .. } | StmtKind::Return(None) => {}
    }
}

fn visit_expr_selection(source: &str, expr: &Expr, offset: u32, spans: &mut Vec<Span>) {
    if expr.span.start <= offset && offset <= expr.span.end {
        spans.push(expr.span);
    }
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            visit_expr_selection(source, callee, offset, spans);
            for arg in args {
                visit_expr_selection(source, &arg.value, offset, spans);
            }
        }
        ExprKind::Field { target, .. } => visit_expr_selection(source, target, offset, spans),
        ExprKind::Unary { rhs, .. } => visit_expr_selection(source, rhs, offset, spans),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Logical { lhs, rhs, .. }
        | ExprKind::Coalesce { lhs, rhs }
        | ExprKind::Assign { target: lhs, value: rhs }
        | ExprKind::AssignOp { target: lhs, value: rhs, .. }
        | ExprKind::AssignShort { target: lhs, value: rhs, .. } => {
            visit_expr_selection(source, lhs, offset, spans);
            visit_expr_selection(source, rhs, offset, spans);
        }
        ExprKind::List(items) => {
            for item in items {
                visit_expr_selection(source, item, offset, spans);
            }
        }
        ExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                visit_expr_selection(source, k, offset, spans);
                visit_expr_selection(source, v, offset, spans);
            }
        }
        ExprKind::Index { target, index } => {
            visit_expr_selection(source, target, offset, spans);
            visit_expr_selection(source, index, offset, spans);
        }
        ExprKind::Slice { target, start, end } => {
            visit_expr_selection(source, target, offset, spans);
            if let Some(s) = start { visit_expr_selection(source, s, offset, spans); }
            if let Some(e) = end { visit_expr_selection(source, e, offset, spans); }
        }
        ExprKind::Is { value, .. } => visit_expr_selection(source, value, offset, spans),
        ExprKind::Chain(inner) => visit_expr_selection(source, inner, offset, spans),
        _ => {}
    }
}

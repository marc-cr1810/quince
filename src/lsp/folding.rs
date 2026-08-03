//! Code folding range provider.

use lsp_types::{FoldingRange, FoldingRangeKind};
use quince::syntax::ast::{Stmt, StmtKind};
use crate::lsp::DocumentState;
use crate::lsp::position::offset_to_position;

pub(crate) fn get_folding_ranges(state: Option<&DocumentState>) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    let Some(state) = state else {
        return ranges;
    };

    // 1. Fold AST Blocks, Functions, Classes, Control structures
    if let Some(ast) = state.ast() {
        collect_ast_folding_ranges(&state.text, ast, &mut ranges);
    }

    // 2. Fold contiguous `##` doc comments
    collect_doc_comment_folding_ranges(&state.text, &mut ranges);

    ranges
}

fn collect_ast_folding_ranges(source: &str, stmts: &[Stmt], ranges: &mut Vec<FoldingRange>) {
    for stmt in stmts {
        visit_stmt_folding(source, stmt, ranges);
    }
}

fn visit_stmt_folding(source: &str, stmt: &Stmt, ranges: &mut Vec<FoldingRange>) {
    let start_pos = offset_to_position(source, stmt.span.start as usize);
    let end_pos = offset_to_position(source, stmt.span.end as usize);

    if start_pos.line < end_pos.line {
        match &stmt.kind {
            StmtKind::Fn { decl, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some(format!("fn {}()", decl.name)),
                });
                collect_ast_folding_ranges(source, &decl.body.stmts, ranges);
            }
            StmtKind::Class { name, methods, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some(format!("class {name}")),
                });
                for m in methods {
                    let m_start = offset_to_position(source, m.body.span.start as usize);
                    let m_end = offset_to_position(source, m.body.span.end as usize);
                    if m_start.line < m_end.line {
                        ranges.push(FoldingRange {
                            start_line: m_start.line,
                            start_character: Some(m_start.character),
                            end_line: m_end.line,
                            end_character: Some(m_end.character),
                            kind: Some(FoldingRangeKind::Region),
                            collapsed_text: Some(format!("fn {}()", m.name)),
                        });
                    }
                    collect_ast_folding_ranges(source, &m.body.stmts, ranges);
                }
            }
            StmtKind::If { then, otherwise, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some("if ...".to_string()),
                });
                collect_ast_folding_ranges(source, &then.stmts, ranges);
                if let Some(other) = otherwise {
                    visit_stmt_folding(source, other, ranges);
                }
            }
            StmtKind::While { body, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some("while ...".to_string()),
                });
                collect_ast_folding_ranges(source, &body.stmts, ranges);
            }
            StmtKind::For { body, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some("for ...".to_string()),
                });
                collect_ast_folding_ranges(source, &body.stmts, ranges);
            }
            StmtKind::Try { body, handler, .. } => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: Some("try ...".to_string()),
                });
                collect_ast_folding_ranges(source, &body.stmts, ranges);
                collect_ast_folding_ranges(source, &handler.stmts, ranges);
            }
            StmtKind::Block(block) => {
                ranges.push(FoldingRange {
                    start_line: start_pos.line,
                    start_character: Some(start_pos.character),
                    end_line: end_pos.line,
                    end_character: Some(end_pos.character),
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: None,
                });
                collect_ast_folding_ranges(source, &block.stmts, ranges);
            }
            _ => {}
        }
    }
}

fn collect_doc_comment_folding_ranges(source: &str, ranges: &mut Vec<FoldingRange>) {
    let mut comment_start: Option<u32> = None;
    let mut comment_end: Option<u32> = None;

    for (line_idx, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("##") {
            if comment_start.is_none() {
                comment_start = Some(line_idx as u32);
            }
            comment_end = Some(line_idx as u32);
        } else {
            if let (Some(start), Some(end)) = (comment_start, comment_end) {
                if start < end {
                    ranges.push(FoldingRange {
                        start_line: start,
                        start_character: None,
                        end_line: end,
                        end_character: None,
                        kind: Some(FoldingRangeKind::Comment),
                        collapsed_text: Some("## doc comment".to_string()),
                    });
                }
            }
            comment_start = None;
            comment_end = None;
        }
    }

    if let (Some(start), Some(end)) = (comment_start, comment_end) {
        if start < end {
            ranges.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line: end,
                end_character: None,
                kind: Some(FoldingRangeKind::Comment),
                collapsed_text: Some("## doc comment".to_string()),
            });
        }
    }
}

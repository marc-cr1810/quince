//! The statement forms that are not declarations.
//!
//! Bindings, control flow, `try`/`throw`, and a bare expression. v0.10's `match`
//! and `if let` are statement-position forms and belong here beside `if`.

use crate::error::{Raised, Result};
use crate::syntax::ast::{BinaryOp, BindKind, Expr, ExprKind, Stmt, StmtKind};
use crate::syntax::parser::{Parser, syntax};
use crate::syntax::token::{Span, Token, TokenKind};

/// The refusal for a `++` or `--` written where a value is wanted.
///
/// Shared with the expression parser, which meets the same token from the other
/// side — `x = i++` gets here through [`Parser::incr_suffix`] and `f(i++)`
/// through `unary`, and both mistakes have one answer.
pub(super) fn incr_outside_statement(token: &Token) -> Raised {
    let op = &token.kind;
    let long = match op {
        TokenKind::PlusPlus => "+= 1",
        _ => "-= 1",
    };
    syntax(
        format!("`{op}` is a statement on its own, not an operator inside an expression"),
        token.span,
    )
    .with_help(format!(
        "put it on a line of its own, or write `i {long}` where a value is wanted — \
         `{op}` produces no value, so the two spellings mean the same thing"
    ))
}

impl Parser {
    pub(super) fn let_stmt(&mut self) -> Result<Stmt> {
        let start = self.peek().span;
        let modifiers = self.modifiers("a binding")?;
        let keyword = self.advance();
        let bind = match keyword.kind {
            TokenKind::Let => BindKind::Let,
            TokenKind::Final => BindKind::Final,
            _ => BindKind::Const,
        };
        let word = format!("`{}`", bind.word());

        let (name, name_span) = self.expect_ident(&format!("after {word}"))?;
        let ty = self.annotation()?;
        // A declaration with no `= value` takes the one its type answers with:
        // `nil` when there is no annotation, and the type's zero-argument
        // constructor when there is. Which types can answer is a question about
        // classes and so is the resolver's — v0.8 §3.4.
        let (value, defaulted) = match self.eat(&TokenKind::Assign) {
            true => (self.expression()?, false),
            false => (Self::default_for(ty.as_ref(), name_span), true),
        };
        let span = start.to(value.span);
        self.end_of_statement()?;

        Ok(Stmt {
            kind: StmtKind::Let {
                slot: None,
                name,
                name_span,
                value,
                defaulted,
                bind,
                ty,
                visibility: modifiers.visibility,
                doc: modifiers.doc,
            },
            span,
        })
    }
    pub(super) fn if_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let cond = self.expression()?;
        let then = self.block()?;
        let mut span = start.to(then.span);

        let otherwise = if self.eat(&TokenKind::Else) {
            let stmt = if self.check(&TokenKind::If) {
                self.if_stmt()?
            } else {
                let block = self.block()?;
                Stmt {
                    span: block.span,
                    kind: StmtKind::Block(block),
                }
            };
            span = span.to(stmt.span);
            Some(Box::new(stmt))
        } else {
            None
        };

        Ok(Stmt {
            kind: StmtKind::If {
                cond,
                then,
                otherwise,
            },
            span,
        })
    }

    pub(super) fn while_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let cond = self.expression()?;
        let body = self.block()?;
        let span = start.to(body.span);
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            span,
        })
    }

    pub(super) fn for_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let (var, _) = self.expect_ident("after `for`")?;
        self.expect(TokenKind::In, "after the loop variable")?;
        let iter = self.expression()?;
        let body = self.block()?;
        let span = start.to(body.span);
        Ok(Stmt {
            kind: StmtKind::For {
                var,
                iter,
                body,
                slot: None,
            },
            span,
        })
    }

    pub(super) fn return_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let value = if self.at_statement_end() {
            None
        } else {
            Some(self.expression()?)
        };
        let span = value.as_ref().map_or(start, |v| start.to(v.span));
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Return(value),
            span,
        })
    }

    /// Parses `try { … } catch e { … }`.
    ///
    /// `catch e` takes no parentheses, matching `if cond {` and `for x in xs {` —
    /// nothing else in the grammar parenthesises a header and this does not start.
    pub(super) fn try_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let body = self.block()?;
        self.expect(TokenKind::Catch, "after the `try` block")?;
        let (binding, _) = self.expect_ident("after `catch`")?;
        let handler = self.block()?;
        let span = start.to(handler.span);
        Ok(Stmt {
            kind: StmtKind::Try {
                body,
                binding,
                handler,
                slot: None,
            },
            span,
        })
    }

    pub(super) fn throw_stmt(&mut self) -> Result<Stmt> {
        let start = self.advance().span;
        let value = self.expression()?;
        let span = start.to(value.span);
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Throw(value),
            span,
        })
    }

    pub(super) fn expr_stmt(&mut self) -> Result<Stmt> {
        let expr = self.expression()?;
        let expr = self.incr_suffix(expr)?;
        let span = expr.span;
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span,
        })
    }

    /// `i++` and `i--`, the postfix half of the increment forms.
    ///
    /// Applied to a whole statement's expression rather than inside the
    /// precedence climb, which is what keeps them statements: a nested `f(i++)`
    /// never reaches here, so the `++` is still sitting there when the argument
    /// list asks for its `,`.
    fn incr_suffix(&mut self, expr: Expr) -> Result<Expr> {
        let Some(op) = incr_op(&self.peek().kind) else {
            return Ok(expr);
        };
        // A `++` on the next line belongs to that line's statement, exactly as a
        // `(` on a fresh line starts a new one rather than calling what came
        // before it.
        if self.peek().newline_before {
            return Ok(expr);
        }
        let token = self.advance();
        self.incr(expr, op, token.span)
    }

    /// `++i` and `--i`.
    ///
    /// Dispatched from `statement` on the operator itself, which is the only
    /// place a program can write one: anywhere an operand is expected, `unary`
    /// refuses it instead.
    pub(super) fn incr_stmt(&mut self) -> Result<Stmt> {
        let token = self.advance();
        let op = incr_op(&token.kind).expect("dispatched on `++` or `--`");
        let target = self.postfix()?;
        let expr = self.incr(target, op, token.span)?;
        let span = expr.span;
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span,
        })
    }

    /// Builds `target += 1` for either spelling and either position.
    ///
    /// One function because there is one meaning. `++i` and `i++` differ in C by
    /// what they *evaluate to*, and neither evaluates to anything here — so the
    /// distinction has nothing left to be about, and both become the compound
    /// assignment that already exists. That also means the target is evaluated
    /// once for free: `d[f()]++` calls `f` a single time, because
    /// [`ExprKind::AssignOp`] is what carries that rule.
    fn incr(&mut self, target: Expr, op: BinaryOp, op_span: Span) -> Result<Expr> {
        if !super::expr::is_assignable(&target) {
            let word = match op {
                BinaryOp::Add => "++",
                _ => "--",
            };
            return Err(syntax(
                format!("cannot apply `{word}` to this expression"),
                target.span.to(op_span),
            )
            .with_help(
                "it counts a name, an index, or a field up or down, and assigns the result back",
            ));
        }
        let span = target.span.to(op_span);
        Ok(Expr {
            kind: ExprKind::AssignOp {
                target: Box::new(target),
                op,
                // Spanned at the operator, which is the only thing in the source
                // that stands for it. Nothing points at this node, but a span of
                // zero would sort before the whole file in the maps keyed by one.
                value: Box::new(Expr {
                    kind: ExprKind::Int(1),
                    span: op_span,
                }),
            },
            span,
        })
    }
}

/// Which compound assignment an increment token stands for, if it is one.
fn incr_op(kind: &TokenKind) -> Option<BinaryOp> {
    match kind {
        TokenKind::PlusPlus => Some(BinaryOp::Add),
        TokenKind::MinusMinus => Some(BinaryOp::Sub),
        _ => None,
    }
}

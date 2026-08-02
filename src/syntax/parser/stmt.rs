//! The statement forms that are not declarations.
//!
//! Bindings, control flow, `try`/`throw`, and a bare expression. v0.10's `match`
//! and `if let` are statement-position forms and belong here beside `if`.

use crate::error::Result;
use crate::syntax::ast::{BindKind, Stmt, StmtKind};
use crate::syntax::parser::{Parser, declaration};
use crate::syntax::token::TokenKind;

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
        // An annotated binding is initialized or it is an error: there is no
        // `int` to synthesize and no honest default. The rule that lets
        // `let items: list[int]` mean `[]` is a constructor rule and waits for
        // v0.8 with the rest of them — v0.7 §3.2.
        if ty.is_some() && !self.check(&TokenKind::Assign) {
            return Err(declaration(
                format!("`{name}` is annotated but not initialized"),
                start.to(name_span),
            )
            .with_help(
                "an annotation says what the name will hold, not what to put there — give it a value",
            ));
        }
        self.expect(TokenKind::Assign, &format!("in a {word} binding"))?;
        let value = self.expression()?;
        let span = start.to(value.span);
        self.end_of_statement()?;

        Ok(Stmt {
            kind: StmtKind::Let {
                slot: None,
                name,
                name_span,
                value,
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
        let span = expr.span;
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span,
        })
    }
}

//! Expression parsing: a Pratt climb over the infix operators, then unary,
//! postfix, and the primary forms.
//!
//! [`infix_op`] is the whole precedence table. v0.7 adds `??`, `is`, and `?.`
//! to it and v0.10 adds `..`; each is a row here rather than a new production.

use crate::error::Result;
use crate::syntax::ast::{self, BinaryOp, Expr, ExprKind, LogicalOp, UnaryOp, Var};
use crate::syntax::parser::{Parser, syntax};
use crate::syntax::token::TokenKind;

/// Binding power of unary operators, above every infix operator so `-a * b`
/// groups as `(-a) * b`.
const UNARY_BP: u8 = 21;

/// Binding power of `??`.
///
/// Tighter than a comparison and looser than arithmetic, which is the pair of
/// choices that makes both ordinary readings come out right: `d[k] ?? 0 == 5` is
/// `(d[k] ?? 0) == 5`, because the coalesce produces the value being compared;
/// and `d[k] ?? 0 + 1` is `d[k] ?? (0 + 1)`, because the right side is a default
/// *value* rather than an operand of the `+`.
const COALESCE_BP: u8 = 9;

/// Binding power of `is`, which is a comparison and sits with the others.
const IS_BP: u8 = 7;

enum InfixOp {
    Binary(BinaryOp),
    Logical(LogicalOp),
}

/// Left and right binding powers for an infix operator. Every operator here is
/// left-associative, so the right power is always one higher.
fn infix_op(kind: &TokenKind) -> Option<(InfixOp, u8, u8)> {
    let (op, lbp) = match kind {
        TokenKind::OrOr => (InfixOp::Logical(LogicalOp::Or), 1),
        TokenKind::AndAnd => (InfixOp::Logical(LogicalOp::And), 3),
        // The three bitwise operators sit between the logical operators and the
        // comparisons, in C's order — `|` loosest, then `^`, then `&`. Quince
        // does not inherit C's mistake of putting them *looser* than `==`,
        // which is the one every C style guide tells you to parenthesise
        // around: here `a & b == c` groups as `(a & b) == c`, which is what it
        // looks like.
        TokenKind::Eq => (InfixOp::Binary(BinaryOp::Eq), 5),
        TokenKind::Ne => (InfixOp::Binary(BinaryOp::Ne), 5),
        TokenKind::In => (InfixOp::Binary(BinaryOp::In), 7),
        TokenKind::Lt => (InfixOp::Binary(BinaryOp::Lt), 7),
        TokenKind::Le => (InfixOp::Binary(BinaryOp::Le), 7),
        TokenKind::Gt => (InfixOp::Binary(BinaryOp::Gt), 7),
        TokenKind::Ge => (InfixOp::Binary(BinaryOp::Ge), 7),
        // Tighter than every comparison, which is where Quince parts company
        // with C. C puts these *looser* than `==`, so `a & b == c` means
        // `a & (b == c)` — the one precedence every C style guide tells you to
        // parenthesise around. Here it groups as `(a & b) == c`, which is what
        // it looks like. Among themselves they keep C's order: `|` loosest,
        // then `^`, then `&`.
        TokenKind::Pipe => (InfixOp::Binary(BinaryOp::BitOr), 11),
        TokenKind::Caret => (InfixOp::Binary(BinaryOp::BitXor), 12),
        TokenKind::Amp => (InfixOp::Binary(BinaryOp::BitAnd), 13),
        // A shift is arithmetic, so it binds looser than `+` — `a << 1 + 2` is
        // `a << (1 + 2)`, as in C.
        TokenKind::Shl => (InfixOp::Binary(BinaryOp::Shl), 15),
        TokenKind::Shr => (InfixOp::Binary(BinaryOp::Shr), 15),
        TokenKind::Plus => (InfixOp::Binary(BinaryOp::Add), 17),
        TokenKind::Minus => (InfixOp::Binary(BinaryOp::Sub), 17),
        TokenKind::Star => (InfixOp::Binary(BinaryOp::Mul), 19),
        TokenKind::Slash => (InfixOp::Binary(BinaryOp::Div), 19),
        TokenKind::SlashSlash => (InfixOp::Binary(BinaryOp::FloorDiv), 19),
        TokenKind::Percent => (InfixOp::Binary(BinaryOp::Rem), 19),
        _ => return None,
    };
    Some((op, lbp, lbp + 1))
}

impl Parser {
    pub(super) fn expression(&mut self) -> Result<Expr> {
        self.assignment()
    }

    /// Assignment binds loosest and associates rightwards, so `a = b = c` is
    /// `a = (b = c)`.
    pub(super) fn assignment(&mut self) -> Result<Expr> {
        let lhs = self.binary(0)?;
        if !self.eat(&TokenKind::Assign) {
            return Ok(lhs);
        }

        if !is_assignable(&lhs) {
            return Err(syntax(
                "cannot assign to this expression",
                lhs.span,
            ));
        }

        let value = self.assignment()?;
        let span = lhs.span.to(value.span);
        Ok(Expr {
            kind: ExprKind::Assign {
                target: Box::new(lhs),
                value: Box::new(value),
            },
            span,
        })
    }

    pub(super) fn binary(&mut self, min_bp: u8) -> Result<Expr> {
        let mut lhs = self.unary()?;

        loop {
            // The two that do not fit [`infix_op`]: `??` short-circuits, so it
            // builds its own node rather than a `Binary`, and `is` takes a type
            // on the right rather than an expression.
            if self.check(&TokenKind::QuestionQuestion) {
                if COALESCE_BP < min_bp {
                    break;
                }
                self.advance();
                // Right-associative — recursing at its own power rather than one
                // above it — so `a ?? b ?? c` is `a ?? (b ?? c)`. A chain of
                // fallbacks is read left to right and the first one that answers
                // wins, which is what that grouping gives.
                let rhs = self.binary(COALESCE_BP)?;
                let span = lhs.span.to(rhs.span);
                lhs = Expr {
                    kind: ExprKind::Coalesce {
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    span,
                };
                continue;
            }
            if self.check(&TokenKind::Is) {
                if IS_BP < min_bp {
                    break;
                }
                self.advance();
                let ty = self.type_expr()?;
                let span = lhs.span.to(ty.span);
                lhs = Expr {
                    kind: ExprKind::Is {
                        value: Box::new(lhs),
                        ty,
                    },
                    span,
                };
                continue;
            }

            let Some((op, lbp, rbp)) = infix_op(&self.peek().kind) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.binary(rbp)?;
            let span = lhs.span.to(rhs.span);
            let kind = match op {
                InfixOp::Binary(op) => ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                InfixOp::Logical(op) => ExprKind::Logical {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
            lhs = Expr { kind, span };
        }

        Ok(lhs)
    }

    pub(super) fn unary(&mut self) -> Result<Expr> {
        let op = match self.peek().kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Not => UnaryOp::Not,
            TokenKind::Tilde => UnaryOp::BitNot,
            _ => return self.postfix(),
        };
        let start = self.advance().span;
        let rhs = self.binary(UNARY_BP)?;
        let span = start.to(rhs.span);
        Ok(Expr {
            kind: ExprKind::Unary {
                op,
                rhs: Box::new(rhs),
            },
            span,
        })
    }

    pub(super) fn postfix(&mut self) -> Result<Expr> {
        let mut expr = self.primary()?;
        // Whether this chain contains a `?.`, and so whether it needs the
        // wrapper that bounds where short-circuiting stops.
        let mut optional_seen = false;

        loop {
            // A `(` or `[` on a fresh line starts a new statement rather than
            // continuing this expression. `.` is exempt so method chains can be
            // broken across lines.
            let newline = self.peek().newline_before;
            expr = match self.peek().kind {
                TokenKind::Dot | TokenKind::QuestionDot => {
                    let optional = self.check(&TokenKind::QuestionDot);
                    optional_seen |= optional;
                    let after = if optional { "after `?.`" } else { "after `.`" };
                    self.advance();
                    let (name, name_span) = self.expect_ident(after)?;
                    Expr {
                        span: expr.span.to(name_span),
                        kind: ExprKind::Field {
                            target: Box::new(expr),
                            name,
                            optional,
                        },
                    }
                }
                TokenKind::LParen if !newline => {
                    self.advance();
                    let mut args = Vec::new();
                    while !self.check(&TokenKind::RParen) {
                        args.push(self.expression()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    let close = self.expect(TokenKind::RParen, "after the arguments")?;
                    Expr {
                        span: expr.span.to(close.span),
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                    }
                }
                TokenKind::LBracket if !newline => {
                    self.advance();

                    // An empty lower bound is only legal in a slice, so a `:`
                    // here settles which form this is before anything is parsed.
                    let start = match self.check(&TokenKind::Colon) {
                        true => None,
                        false => Some(self.expression()?),
                    };

                    if !self.eat(&TokenKind::Colon) {
                        let close = self.expect(TokenKind::RBracket, "after the index")?;
                        Expr {
                            span: expr.span.to(close.span),
                            kind: ExprKind::Index {
                                target: Box::new(expr),
                                index: Box::new(start.expect("a non-slice index has a value")),
                            },
                        }
                    } else {
                        let end = match self.check(&TokenKind::RBracket) {
                            true => None,
                            false => Some(self.expression()?),
                        };
                        let close = self.expect(TokenKind::RBracket, "after the slice")?;
                        Expr {
                            span: expr.span.to(close.span),
                            kind: ExprKind::Slice {
                                target: Box::new(expr),
                                start: start.map(Box::new),
                                end: end.map(Box::new),
                            },
                        }
                    }
                }
                // The chain is over. A `?.` anywhere in it means the whole
                // thing short-circuits together, and this is the node that says
                // where "the whole thing" ends.
                _ => {
                    return Ok(match optional_seen {
                        true => Expr {
                            span: expr.span,
                            kind: ExprKind::Chain(Box::new(expr)),
                        },
                        false => expr,
                    });
                }
            };
        }
    }

    pub(super) fn primary(&mut self) -> Result<Expr> {
        let token = self.advance();
        let kind = match token.kind {
            TokenKind::Int(n) => ExprKind::Int(n),
            TokenKind::Float(n) => ExprKind::Float(n),
            TokenKind::Str(s) => ExprKind::Str(s),
            TokenKind::True => ExprKind::Bool(true),
            TokenKind::False => ExprKind::Bool(false),
            TokenKind::Nil => ExprKind::Nil,
            TokenKind::Ident(name) => ExprKind::Var(Var::new(name)),
            // An ordinary variable reference from here on. The parser put the
            // binding in place as a parameter, so nothing else has to know that
            // this name arrived as a keyword.
            TokenKind::SelfKw => ExprKind::Var(Var::new(ast::SELF)),

            // `super` is only ever a lookup — there is nothing useful to do
            // with the parent class as a bare value that naming it would not
            // do better, and requiring the `.name` here means the error lands
            // on the `super` rather than somewhere downstream.
            TokenKind::Super => {
                self.expect(TokenKind::Dot, "after `super`")?;
                let (name, end) = self.expect_ident("after `super.`")?;
                return Ok(Expr {
                    kind: ExprKind::Super {
                        name,
                        parent: Var::new(ast::SUPER),
                        receiver: Var::new(ast::SELF),
                    },
                    span: token.span.to(end),
                });
            }

            TokenKind::LParen => {
                let inner = self.expression()?;
                let close = self.expect(TokenKind::RParen, "after the expression")?;
                // Reuse the inner node but widen its span to include the parens,
                // so errors underline what the reader sees.
                return Ok(Expr {
                    kind: inner.kind,
                    span: token.span.to(close.span),
                });
            }

            TokenKind::LBracket => {
                let mut items = Vec::new();
                while !self.check(&TokenKind::RBracket) {
                    items.push(self.expression()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                let close = self.expect(TokenKind::RBracket, "after the list items")?;
                return Ok(Expr {
                    kind: ExprKind::List(items),
                    span: token.span.to(close.span),
                });
            }

            // Only reachable where an operand is expected. A `{` at the start of
            // a statement is dispatched to `block` long before this, so the two
            // uses of the brace never compete — see `end_of_statement`.
            TokenKind::LBrace => {
                let mut entries = Vec::new();
                while !self.check(&TokenKind::RBrace) {
                    let key = self.expression()?;
                    self.expect(TokenKind::Colon, "between a dict key and its value")?;
                    entries.push((key, self.expression()?));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                let close = self.expect(TokenKind::RBrace, "after the dict entries")?;
                return Ok(Expr {
                    kind: ExprKind::Dict(entries),
                    span: token.span.to(close.span),
                });
            }

            _ => {
                return Err(syntax(
                    format!("expected an expression, found `{}`", token.kind),
                    token.span,
                ));
            }
        };
        Ok(Expr {
            kind,
            span: token.span,
        })
    }

    // -- token helpers -----------------------------------------------------
}

fn is_assignable(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Var(_) | ExprKind::Index { .. } | ExprKind::Field { .. }
    )
}


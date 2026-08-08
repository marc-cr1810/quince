//! Expression parsing: a Pratt climb over the infix operators, then unary,
//! postfix, and the primary forms.
//!
//! [`infix_op`] is the whole precedence table. v0.7 adds `??`, `is`, and `?.`
//! to it and v0.10 adds `..`; each is a row here rather than a new production.

use crate::error::Result;
use crate::syntax::ast::{self, BinaryOp, CallArg, Expr, ExprKind, LogicalOp, UnaryOp, Var};
use crate::syntax::parser::stmt::incr_outside_statement;
use crate::syntax::parser::{Parser, declaration, syntax};
use crate::syntax::token::TokenKind;

/// Binding power of unary operators, above every infix operator so `-a * b`
/// groups as `(-a) * b`.
const UNARY_BP: u8 = 21;

/// Binding power of `not`, which is the one unary operator that binds *looser*
/// than the comparisons.
///
/// Above `and` and `or` and below everything else, which is where Python puts
/// it and is the only placement that makes the word read as the word: `not a in
/// b` is `not (a in b)`, `not a == b` is `not (a == b)`, and `not a and b` is
/// still `(not a) and b`.
///
/// It sat at [`UNARY_BP`] with the others while it was spelled `!`, where C puts
/// it and where `!a == b` means `(!a) == b`. That is defensible for a symbol and
/// indefensible for a word — nobody reads "not a in b" as asking whether the
/// negation of `a` is in `b`. `-` and `~` stay where they were, being symbols.
const NOT_BP: u8 = 4;

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

/// Binding power of `in`, named because `not in` reaches it from outside the
/// table and the two spellings must bind identically.
const IN_BP: u8 = 7;

enum InfixOp {
    Binary(BinaryOp),
    Logical(LogicalOp),
}

/// Binding power of `**`.
///
/// Above [`UNARY_BP`], which is what makes `-2 ** 2` mean `-(2 ** 2)`: the
/// operand of a unary operator is parsed at `UNARY_BP`, so an operator that
/// binds tighter is pulled into it. Python and ordinary mathematical notation
/// both read it that way.
const POW_BP: u8 = 23;

/// Left and right binding powers for an infix operator.
///
/// Every operator here is left-associative except `**`, so the right power is
/// one higher than the left — and one *lower* for the exception, which is what
/// makes `2 ** 3 ** 2` group as `2 ** (3 ** 2)`. It differs because left
/// association would make the operator useless for what it is for.
fn infix_op(kind: &TokenKind) -> Option<(InfixOp, u8, u8)> {
    if let TokenKind::StarStar = kind {
        return Some((InfixOp::Binary(BinaryOp::Pow), POW_BP, POW_BP - 1));
    }
    let (op, lbp) = match kind {
        TokenKind::Or => (InfixOp::Logical(LogicalOp::Or), 1),
        TokenKind::And => (InfixOp::Logical(LogicalOp::And), 3),
        // The three bitwise operators sit between the logical operators and the
        // comparisons, in C's order — `|` loosest, then `^`, then `&`. Quince
        // does not inherit C's mistake of putting them *looser* than `==`,
        // which is the one every C style guide tells you to parenthesise
        // around: here `a & b == c` groups as `(a & b) == c`, which is what it
        // looks like.
        TokenKind::Eq => (InfixOp::Binary(BinaryOp::Eq), 5),
        TokenKind::Ne => (InfixOp::Binary(BinaryOp::Ne), 5),
        TokenKind::In => (InfixOp::Binary(BinaryOp::In), IN_BP),
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

        // `a op= b` is `a = a op b` with the target evaluated once, which is the
        // whole reason it is a language form rather than something a program
        // writes out — `d[f()] += 1` calls `f` a single time. The node is what
        // carries "once"; a desugaring here could not.
        if let TokenKind::AssignOp(op) = self.peek().kind {
            self.advance();
            if !is_assignable(&lhs) {
                return Err(syntax("cannot assign to this expression", lhs.span));
            }
            let value = self.assignment()?;
            let span = lhs.span.to(value.span);
            return Ok(Expr {
                kind: ExprKind::AssignOp {
                    target: Box::new(lhs),
                    op,
                    value: Box::new(value),
                },
                span,
            });
        }

        // `and=`, `or=`, and `??=`. Parsed beside the compound assignments
        // because they are written like one and bind like one; a separate node
        // because the right side may not run — see [`ExprKind::AssignShort`].
        if let TokenKind::AssignShort(op) = self.peek().kind {
            self.advance();
            if !is_assignable(&lhs) {
                return Err(syntax("cannot assign to this expression", lhs.span));
            }
            let value = self.assignment()?;
            let span = lhs.span.to(value.span);
            return Ok(Expr {
                kind: ExprKind::AssignShort {
                    target: Box::new(lhs),
                    op,
                    value: Box::new(value),
                },
                span,
            });
        }

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
                // `x is not string` is the negation of `x is string`, and is
                // built as one: a `Not` over the ordinary node rather than a
                // second node meaning the opposite. Every pass that already
                // knows what `is` means therefore needs no change — including
                // the narrowing in `sema::infer`, which correctly declines to
                // narrow here, since what a *failed* type test proves is not
                // something that pass can express.
                let negated = self.eat(&TokenKind::Not);
                let ty = self.type_expr()?;
                let span = lhs.span.to(ty.span);
                lhs = Expr {
                    kind: ExprKind::Is {
                        value: Box::new(lhs),
                        ty,
                    },
                    span,
                };
                if negated {
                    lhs = Expr {
                        kind: ExprKind::Unary {
                            op: UnaryOp::Not,
                            rhs: Box::new(lhs),
                        },
                        span,
                    };
                }
                continue;
            }
            // `a not in b`, the other operator written as two words. `not` is a
            // prefix operator everywhere else, so it is only this form when an
            // `in` follows it — and reaching here at all means an operand is
            // already in hand, where a prefix `not` could not be.
            if self.check(&TokenKind::Not) && matches!(self.peek_ahead(), TokenKind::In) {
                if IN_BP < min_bp {
                    break;
                }
                self.advance();
                self.advance();
                let rhs = self.binary(IN_BP + 1)?;
                let span = lhs.span.to(rhs.span);
                lhs = Expr {
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        rhs: Box::new(Expr {
                            kind: ExprKind::Binary {
                                op: BinaryOp::In,
                                lhs: Box::new(lhs),
                                rhs: Box::new(rhs),
                            },
                            span,
                        }),
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
        let (op, bp) = match self.peek().kind {
            TokenKind::Minus => (UnaryOp::Neg, UNARY_BP),
            TokenKind::Not => (UnaryOp::Not, NOT_BP),
            TokenKind::Tilde => (UnaryOp::BitNot, UNARY_BP),
            // Reaching here means an operand was expected, and `++` is not one:
            // it is a statement, and `statement` has already taken every `++`
            // that opens one. Refusing it by name beats letting it fall through
            // to "expected an expression", which names the token and not the
            // rule. `- -x` still works; `--x` in operand position lands here.
            TokenKind::PlusPlus | TokenKind::MinusMinus => {
                return Err(incr_outside_statement(self.peek()));
            }
            _ => return self.postfix(),
        };
        let start = self.advance().span;
        let rhs = self.binary(bp)?;
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
                    let args = self.arguments()?;
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
                        // A comma means a second argument, and nothing indexable
                        // takes two — so this is a generic class being supplied
                        // with its arguments, `Pair[int, string]`. The one-argument
                        // form is not decided here and cannot be: `Stack[int]` and
                        // `xs[i]` are the same three tokens. See `ExprKind::TypeArgs`.
                        let mut args = vec![start.expect("a non-slice index has a value")];
                        let mut many = false;
                        while self.eat(&TokenKind::Comma) {
                            many = true;
                            // A trailing comma, as every other bracketed list allows.
                            if self.check(&TokenKind::RBracket) {
                                break;
                            }
                            args.push(self.expression()?);
                        }
                        let close = self.expect(
                            TokenKind::RBracket,
                            match many {
                                true => "after the type arguments",
                                false => "after the index",
                            },
                        )?;
                        Expr {
                            span: expr.span.to(close.span),
                            kind: match many {
                                true => ExprKind::TypeArgs {
                                    target: Box::new(expr),
                                    args,
                                },
                                false => ExprKind::Index {
                                    target: Box::new(expr),
                                    index: Box::new(args.pop().expect("one argument")),
                                },
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

    /// The arguments between a call's parentheses, positional then named.
    ///
    /// `name: expr` is a keyword argument and `expr` is a positional one, told
    /// apart by one token of lookahead — an identifier followed by a `:`. There
    /// is nothing else that shape can be inside a call: a dict literal starts
    /// with `{`, and a bare `:` here was a syntax error before v0.8.
    ///
    /// A positional argument after a named one is refused, because that ordering
    /// has no reading that is not a guess: the named one already took a
    /// parameter out of the sequence, and which one the next value continues
    /// from would be a rule nobody could hold in their head.
    pub(super) fn arguments(&mut self) -> Result<Vec<CallArg>> {
        let mut args: Vec<CallArg> = Vec::new();
        while !self.check(&TokenKind::RParen) {
            let named = match &self.peek().kind {
                TokenKind::Ident(name) => self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|token| token.kind == TokenKind::Colon)
                    .then(|| (name.clone(), self.peek().span)),
                _ => None,
            };
            match named {
                Some(name) => {
                    self.advance();
                    self.advance();
                    args.push(CallArg {
                        name: Some(name),
                        value: self.expression()?,
                    });
                }
                None => {
                    if let Some(earlier) = args.iter().find_map(|arg| arg.name.as_ref()) {
                        return Err(declaration(
                            format!(
                                "this argument has no name, and `{}:` before it does",
                                earlier.0
                            ),
                            self.peek().span,
                        )
                        .with_help(
                            "a named argument takes a parameter out of the sequence, so what \
                             this one would continue from is a guess — name it too, or move it \
                             in front",
                        ));
                    }
                    args.push(CallArg::positional(self.expression()?));
                }
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(args)
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

pub(super) fn is_assignable(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Var(_) | ExprKind::Index { .. } | ExprKind::Field { .. }
    )
}


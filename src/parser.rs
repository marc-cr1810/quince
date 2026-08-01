use crate::ast::{
    self, BinaryOp, BindKind, Block, Expr, ExprKind, FnDecl, LogicalOp, Op, Openness, Param, Stmt,
    StmtKind, UnaryOp, Var,
};
use crate::error::QuinceError;
use crate::token::{Span, Token, TokenKind};

/// Binding power of unary operators, above every infix operator so `-a * b`
/// groups as `(-a) * b`.
const UNARY_BP: u8 = 13;

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
        TokenKind::Eq => (InfixOp::Binary(BinaryOp::Eq), 5),
        TokenKind::Ne => (InfixOp::Binary(BinaryOp::Ne), 5),
        TokenKind::In => (InfixOp::Binary(BinaryOp::In), 7),
        TokenKind::Lt => (InfixOp::Binary(BinaryOp::Lt), 7),
        TokenKind::Le => (InfixOp::Binary(BinaryOp::Le), 7),
        TokenKind::Gt => (InfixOp::Binary(BinaryOp::Gt), 7),
        TokenKind::Ge => (InfixOp::Binary(BinaryOp::Ge), 7),
        TokenKind::Plus => (InfixOp::Binary(BinaryOp::Add), 9),
        TokenKind::Minus => (InfixOp::Binary(BinaryOp::Sub), 9),
        TokenKind::Star => (InfixOp::Binary(BinaryOp::Mul), 11),
        TokenKind::Slash => (InfixOp::Binary(BinaryOp::Div), 11),
        TokenKind::SlashSlash => (InfixOp::Binary(BinaryOp::FloorDiv), 11),
        TokenKind::Percent => (InfixOp::Binary(BinaryOp::Rem), 11),
        _ => return None,
    };
    Some((op, lbp, lbp + 1))
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    /// Parses a whole program.
    ///
    /// Stops at the first error rather than recovering; reporting several errors
    /// per run needs synchronisation points and can come once the grammar settles.
    pub fn parse(mut self) -> Result<Vec<Stmt>, QuinceError> {
        let mut stmts = Vec::new();
        while !self.at_end() {
            stmts.push(self.statement()?);
        }
        Ok(stmts)
    }

    // -- statements --------------------------------------------------------

    fn statement(&mut self) -> Result<Stmt, QuinceError> {
        match self.peek().kind {
            // One token of lookahead, and only here: `final` is the one modifier
            // that is also a binding form, so the class case has to be recognised
            // before `let_stmt` consumes the keyword and demands a name.
            // `complete` and `sealed` need none — they introduce nothing else.
            TokenKind::Final if self.next_is(&TokenKind::Class) => self.class_stmt(),
            TokenKind::Let | TokenKind::Final | TokenKind::Const => self.let_stmt(),
            TokenKind::Complete | TokenKind::Sealed => self.class_stmt(),
            TokenKind::Fn => self.fn_stmt(),
            // An `op` is a method the language calls on an instance, so there is
            // nothing for one to belong to out here.
            TokenKind::Op => Err(QuinceError::new(
                "`op` is only valid inside a class body",
                self.peek().span,
            )
            .with_help("use `fn` for a function that is called by name")),
            TokenKind::Class => self.class_stmt(),
            TokenKind::Extend => self.extend_stmt(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::For => self.for_stmt(),
            TokenKind::Return => self.return_stmt(),
            TokenKind::Try => self.try_stmt(),
            TokenKind::Throw => self.throw_stmt(),
            TokenKind::LBrace => {
                let block = self.block()?;
                Ok(Stmt {
                    span: block.span,
                    kind: StmtKind::Block(block),
                })
            }
            _ => self.expr_stmt(),
        }
    }

    fn let_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let keyword = self.advance();
        let bind = match keyword.kind {
            TokenKind::Let => BindKind::Let,
            TokenKind::Final => BindKind::Final,
            _ => BindKind::Const,
        };
        let word = format!("`{}`", bind.word());

        let (name, _) = self.expect_ident(&format!("after {word}"))?;
        self.expect(TokenKind::Assign, &format!("in a {word} binding"))?;
        let value = self.expression()?;
        let span = keyword.span.to(value.span);
        self.end_of_statement()?;

        Ok(Stmt {
            kind: StmtKind::Let {
                slot: None,
                name,
                value,
                bind,
            },
            span,
        })
    }

    fn fn_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.peek().span;
        let decl = self.fn_decl(false)?;
        Ok(Stmt {
            span: start.to(decl.body.span),
            kind: StmtKind::Fn {
                decl: std::rc::Rc::new(decl),
                slot: None,
            },
        })
    }

    /// Parses `fn name(params) { … }` or `op name(params) { … }`, starting at
    /// the keyword.
    ///
    /// A method gets `self` inserted as its first parameter, which is the whole
    /// of what makes the receiver implicit — see [`ast::SELF`]. Its span is the
    /// method's name, since there is no source text to point at and an error
    /// about `self` should land on the method that has one.
    ///
    /// An `op` is validated here rather than in the resolver because everything
    /// the check needs is local: the name, the span, and the parameters, all in
    /// hand before the body is parsed.
    fn fn_decl(&mut self, method: bool) -> Result<FnDecl, QuinceError> {
        let keyword = self.advance().kind.clone();
        let is_op = keyword == TokenKind::Op;
        let after = if is_op { "after `op`" } else { "after `fn`" };
        let (name, name_span) = self.expect_ident(after)?;

        let op = if is_op {
            // Listing the set beats naming it: the list is short, it is the
            // answer to "then what can I write", and it grows with the language.
            Some(Op::from_name(&name).ok_or_else(|| {
                let names: Vec<&str> = ast::OPS.iter().map(|op| op.name()).collect();
                QuinceError::new(
                    format!("`{name}` is not an operation a class can define"),
                    name_span,
                )
                .with_help(format!("`op` can define: {}", names.join(", ")))
            })?)
        } else {
            None
        };

        let lparen = self.expect(TokenKind::LParen, "after the function name")?;

        let mut params = Vec::new();
        if method {
            params.push(Param {
                name: ast::SELF.to_string(),
                span: name_span,
                receiver: true,
            });
        }
        while !self.check(&TokenKind::RParen) {
            let (name, span) = self.expect_ident("in the parameter list")?;
            params.push(Param {
                name,
                span,
                receiver: false,
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let rparen = self.expect(TokenKind::RParen, "after the parameter list")?;

        // An op takes the parameters the language will pass it, and nothing about
        // the declaration is free to differ. Refused here, where the parameter
        // list is what the message can point at — by the time `if x` calls a
        // three-parameter `op bool`, the source that got it wrong is elsewhere.
        if let Some(op) = op
            && let Some(arity) = op.arity()
        {
            // `self` is in `params` already, and is not the program's to count.
            let found = params.len() - usize::from(method);
            if found != arity {
                let plural = if arity == 1 { "" } else { "s" };
                return Err(QuinceError::new(
                    format!(
                        "`op {}` takes {arity} parameter{plural}, but {found} were declared",
                        op.name()
                    ),
                    lparen.span.to(rparen.span),
                )
                .with_help(match arity {
                    0 => format!("`op {}` answers about `self` alone", op.name()),
                    _ => format!(
                        "`op {}` is passed {arity} value{plural} by the language",
                        op.name()
                    ),
                }));
            }
        }

        let body = self.block()?;
        Ok(FnDecl {
            name,
            name_span,
            params,
            body,
            op,
        })
    }

    /// Refuses a name a body has already declared.
    ///
    /// A class body is a table keyed by name, so a second `fn a` overwrites the
    /// first — silently, with the one it replaced still sitting on the page above
    /// it. The resolver already refuses two `fn`s in a *function* for the same
    /// reason and in almost the same words; a class body simply is not a scope,
    /// so there was nowhere for that check to live until here.
    ///
    /// `fn` and `op` share the table, so `fn string` beside `op string` is the
    /// same collision and gets the same answer.
    fn refuse_duplicate(
        declared: &[std::rc::Rc<FnDecl>],
        decl: &FnDecl,
        whose: &str,
    ) -> Result<(), QuinceError> {
        if !declared.iter().any(|seen| seen.name == decl.name) {
            return Ok(());
        }
        Err(QuinceError::new(
            format!("{whose} already declares `{}`", decl.name),
            decl.name_span,
        )
        .with_help(
            "the second would replace the first without a word — rename it, or delete the \
             one you meant to be rid of",
        ))
    }

    /// `class Point { … }`, with an optional modifier in front of it.
    ///
    /// The span starts at the modifier when there is one, so a report about the
    /// declaration underlines the header the program wrote rather than the half
    /// of it after the word.
    fn class_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.peek().span;
        let openness = match self.peek().kind {
            TokenKind::Final => Openness::Final,
            TokenKind::Complete => Openness::Complete,
            TokenKind::Sealed => Openness::Sealed,
            _ => Openness::Open,
        };
        // `class` is the current token when there was no modifier, and the next
        // one when there was — so the two cases differ only in what has to be
        // eaten first, and only the second can fail.
        match openness.word() {
            Some(word) => {
                self.advance();
                self.expect(TokenKind::Class, &format!("after `{word}`"))?;
            }
            None => {
                self.advance();
            }
        }
        let (name, _) = self.expect_ident("after `class`")?;

        let (parent, parent_span) = if self.eat(&TokenKind::Extends) {
            let (parent, span) = self.expect_ident("after `extends`")?;
            (Some(Var::new(parent)), Some(span))
        } else {
            (None, None)
        };

        self.expect(TokenKind::LBrace, "after the class name")?;

        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            if !self.check(&TokenKind::Fn) && !self.check(&TokenKind::Op) {
                return Err(QuinceError::new(
                    format!("expected a method, found {}", self.peek().kind),
                    self.peek().span,
                ));
            }
            let decl = self.fn_decl(true)?;
            Self::refuse_duplicate(&methods, &decl, &name)?;
            methods.push(std::rc::Rc::new(decl));
        }
        let end = self.expect(TokenKind::RBrace, "after the class body")?.span;

        Ok(Stmt {
            kind: StmtKind::Class {
                name,
                parent,
                parent_span,
                methods,
                openness,
                slot: None,
            },
            span: start.to(end),
        })
    }

    /// `extend int { fn double() { … } }`.
    ///
    /// Shaped like a class body with the two halves a class has and an extension
    /// does not: no name to bind, and no `extends` clause, because an extension
    /// declares no type for anything to descend from.
    fn extend_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.advance().span;
        let (target, target_span) = self.expect_ident("after `extend`")?;

        self.expect(TokenKind::LBrace, "after the type being extended")?;

        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            // Refused here rather than at the class, because everything the check
            // needs is in hand: the keyword and its span, before a body is parsed.
            // The same reason `op` at the top level is caught in this file.
            if self.check(&TokenKind::Op) {
                return Err(QuinceError::new(
                    format!("`{target}` cannot be given an `op` by an extension"),
                    self.peek().span,
                )
                .with_help(
                    "an extension adds methods a program calls by name — an `op` decides what \
                     the language itself does with every value of the type, everywhere",
                ));
            }
            if !self.check(&TokenKind::Fn) {
                return Err(QuinceError::new(
                    format!("expected a method, found {}", self.peek().kind),
                    self.peek().span,
                ));
            }
            let decl = self.fn_decl(true)?;
            Self::refuse_duplicate(&methods, &decl, &target)?;
            methods.push(std::rc::Rc::new(decl));
        }
        let end = self
            .expect(TokenKind::RBrace, "after the extension body")?
            .span;

        Ok(Stmt {
            kind: StmtKind::Extend {
                target: Var::new(target),
                target_span,
                methods,
            },
            span: start.to(end),
        })
    }

    fn if_stmt(&mut self) -> Result<Stmt, QuinceError> {
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

    fn while_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.advance().span;
        let cond = self.expression()?;
        let body = self.block()?;
        let span = start.to(body.span);
        Ok(Stmt {
            kind: StmtKind::While { cond, body },
            span,
        })
    }

    fn for_stmt(&mut self) -> Result<Stmt, QuinceError> {
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

    fn return_stmt(&mut self) -> Result<Stmt, QuinceError> {
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
    fn try_stmt(&mut self) -> Result<Stmt, QuinceError> {
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

    fn throw_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.advance().span;
        let value = self.expression()?;
        let span = start.to(value.span);
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Throw(value),
            span,
        })
    }

    fn expr_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let expr = self.expression()?;
        let span = expr.span;
        self.end_of_statement()?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            span,
        })
    }

    fn block(&mut self) -> Result<Block, QuinceError> {
        let open = self.expect(TokenKind::LBrace, "to open a block")?;
        let mut stmts = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            stmts.push(self.statement()?);
        }
        let close = self.expect(TokenKind::RBrace, "to close the block")?;
        Ok(Block {
            stmts,
            span: open.span.to(close.span),
            slot_count: 0,
        })
    }

    // -- expressions -------------------------------------------------------

    fn expression(&mut self) -> Result<Expr, QuinceError> {
        self.assignment()
    }

    /// Assignment binds loosest and associates rightwards, so `a = b = c` is
    /// `a = (b = c)`.
    fn assignment(&mut self) -> Result<Expr, QuinceError> {
        let lhs = self.binary(0)?;
        if !self.eat(&TokenKind::Assign) {
            return Ok(lhs);
        }

        if !is_assignable(&lhs) {
            return Err(QuinceError::new(
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

    fn binary(&mut self, min_bp: u8) -> Result<Expr, QuinceError> {
        let mut lhs = self.unary()?;

        while let Some((op, lbp, rbp)) = infix_op(&self.peek().kind) {
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

    fn unary(&mut self) -> Result<Expr, QuinceError> {
        let op = match self.peek().kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Not => UnaryOp::Not,
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

    fn postfix(&mut self) -> Result<Expr, QuinceError> {
        let mut expr = self.primary()?;

        loop {
            // A `(` or `[` on a fresh line starts a new statement rather than
            // continuing this expression. `.` is exempt so method chains can be
            // broken across lines.
            let newline = self.peek().newline_before;
            expr = match self.peek().kind {
                TokenKind::Dot => {
                    self.advance();
                    let (name, name_span) = self.expect_ident("after `.`")?;
                    Expr {
                        span: expr.span.to(name_span),
                        kind: ExprKind::Field {
                            target: Box::new(expr),
                            name,
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
                _ => return Ok(expr),
            };
        }
    }

    fn primary(&mut self) -> Result<Expr, QuinceError> {
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
                return Err(QuinceError::new(
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

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn at_end(&self) -> bool {
        self.peek().kind == TokenKind::Eof
    }

    /// Whether the token *after* the current one is `kind`.
    ///
    /// The only lookahead in the parser, and it exists for one word: `final`
    /// introduces a binding unless a `class` follows it. Past the end is `false`
    /// rather than a panic, since the token after `Eof` is nothing at all.
    fn next_is(&self, kind: &TokenKind) -> bool {
        self.tokens
            .get(self.pos + 1)
            .is_some_and(|token| std::mem::discriminant(&token.kind) == std::mem::discriminant(kind))
    }

    /// Consumes and returns the current token, parking on `Eof` at the end.
    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if !self.at_end() {
            self.pos += 1;
        }
        token
    }

    /// Compares only the variant, so `Ident("a")` matches any `Ident`.
    fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, context: &str) -> Result<Token, QuinceError> {
        if self.check(&kind) {
            return Ok(self.advance());
        }
        let found = self.peek();
        Err(QuinceError::new(
            format!("expected `{kind}` {context}, found `{}`", found.kind),
            found.span,
        ))
    }

    fn expect_ident(&mut self, context: &str) -> Result<(String, Span), QuinceError> {
        let token = self.peek();
        if let TokenKind::Ident(name) = &token.kind {
            let result = (name.clone(), token.span);
            self.advance();
            return Ok(result);
        }
        Err(QuinceError::new(
            format!("expected a name {context}, found `{}`", token.kind),
            token.span,
        ))
    }

    /// Whether the current position already looks like the end of a statement,
    /// used to tell a bare `return` from one with a value.
    fn at_statement_end(&self) -> bool {
        let token = self.peek();
        token.newline_before
            || matches!(
                token.kind,
                TokenKind::Eof | TokenKind::RBrace | TokenKind::Semi
            )
    }

    /// Statements end at a newline, a `;`, or the end of the enclosing block.
    fn end_of_statement(&mut self) -> Result<(), QuinceError> {
        if self.eat(&TokenKind::Semi) || self.at_statement_end() {
            return Ok(());
        }
        let token = self.peek();
        // A `{` at the start of a statement always opens a block, so a bare dict
        // literal gets parsed as one and fails here on its first `:`. Saying so
        // is far more use than naming the token.
        if token.kind == TokenKind::Colon {
            return Err(QuinceError::new(
                "unexpected `:` — a `{` at the start of a statement opens a block, \
                 so a dict literal there needs parentheses around it",
                token.span,
            ));
        }
        Err(QuinceError::new(
            format!(
                "expected a newline or `;` after this statement, found `{}`",
                token.kind
            ),
            token.span,
        ))
    }
}

fn is_assignable(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Var(_) | ExprKind::Index { .. } | ExprKind::Field { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Result<Vec<Stmt>, QuinceError> {
        let tokens = Lexer::new(src).tokenize().expect("should lex");
        Parser::new(tokens).parse()
    }

    fn parse_ok(src: &str) -> Vec<Stmt> {
        parse(src).unwrap_or_else(|e| panic!("should parse `{src}`: {}", e.message))
    }

    fn parse_err(src: &str) -> QuinceError {
        parse(src).expect_err("should fail to parse")
    }

    /// Renders an expression as an s-expression, so precedence and associativity
    /// are visible in the assertion itself.
    fn sexpr(expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Int(n) => n.to_string(),
            ExprKind::Float(n) => n.to_string(),
            ExprKind::Str(s) => format!("{s:?}"),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Nil => "nil".into(),
            ExprKind::Var(var) => var.name.clone(),
            ExprKind::List(items) => format!("[{}]", joined(items)),
            ExprKind::Dict(entries) => {
                let pairs: Vec<_> = entries
                    .iter()
                    .map(|(key, value)| format!("{}: {}", sexpr(key), sexpr(value)))
                    .collect();
                format!("{{{}}}", pairs.join(" "))
            }
            ExprKind::Unary { op, rhs } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                format!("({op} {})", sexpr(rhs))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let op = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    BinaryOp::FloorDiv => "//",
                    BinaryOp::Rem => "%",
                    BinaryOp::Eq => "==",
                    BinaryOp::Ne => "!=",
                    BinaryOp::Lt => "<",
                    BinaryOp::Le => "<=",
                    BinaryOp::Gt => ">",
                    BinaryOp::Ge => ">=",
                    BinaryOp::In => "in",
                };
                format!("({op} {} {})", sexpr(lhs), sexpr(rhs))
            }
            ExprKind::Logical { op, lhs, rhs } => {
                let op = match op {
                    LogicalOp::And => "&&",
                    LogicalOp::Or => "||",
                };
                format!("({op} {} {})", sexpr(lhs), sexpr(rhs))
            }
            ExprKind::Call { callee, args } => {
                format!("(call {} {})", sexpr(callee), joined(args))
            }
            ExprKind::Index { target, index } => {
                format!("(index {} {})", sexpr(target), sexpr(index))
            }
            ExprKind::Slice { target, start, end } => {
                let bound = |b: &Option<Box<Expr>>| b.as_deref().map_or(String::new(), sexpr);
                format!("(slice {} {} {})", sexpr(target), bound(start), bound(end))
            }
            ExprKind::Field { target, name } => format!("(. {} {name})", sexpr(target)),
            ExprKind::Assign { target, value } => {
                format!("(= {} {})", sexpr(target), sexpr(value))
            }
            ExprKind::Super { name, .. } => format!("(super {name})"),
        }
    }

    fn joined(exprs: &[Expr]) -> String {
        exprs.iter().map(sexpr).collect::<Vec<_>>().join(" ")
    }

    /// Parses a single expression statement and renders it.
    fn expr_of(src: &str) -> String {
        let stmts = parse_ok(src);
        assert_eq!(stmts.len(), 1, "expected one statement from `{src}`");
        match &stmts[0].kind {
            StmtKind::Expr(expr) => sexpr(expr),
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    #[test]
    fn precedence_follows_arithmetic() {
        assert_eq!(expr_of("1 + 2 * 3"), "(+ 1 (* 2 3))");
        assert_eq!(expr_of("1 * 2 + 3"), "(+ (* 1 2) 3)");
        assert_eq!(expr_of("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }

    #[test]
    fn arithmetic_is_left_associative() {
        assert_eq!(expr_of("1 - 2 - 3"), "(- (- 1 2) 3)");
        assert_eq!(expr_of("8 / 4 / 2"), "(/ (/ 8 4) 2)");
    }

    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        assert_eq!(expr_of("a + 1 < b * 2"), "(< (+ a 1) (* b 2))");
    }

    #[test]
    fn logical_operators_bind_loosest() {
        assert_eq!(expr_of("a || b && c"), "(|| a (&& b c))");
        assert_eq!(expr_of("a == 1 && b != 2"), "(&& (== a 1) (!= b 2))");
    }

    #[test]
    fn unary_binds_tighter_than_arithmetic() {
        assert_eq!(expr_of("-a * b"), "(* (- a) b)");
        assert_eq!(expr_of("!a && b"), "(&& (! a) b)");
        assert_eq!(expr_of("--a"), "(- (- a))");
    }

    #[test]
    fn assignment_is_right_associative() {
        assert_eq!(expr_of("a = b = c"), "(= a (= b c))");
        assert_eq!(expr_of("a = 1 + 2"), "(= a (+ 1 2))");
    }

    #[test]
    fn assignment_targets_are_restricted() {
        let err = parse_err("1 = 2");
        assert!(err.message.contains("cannot assign"), "{}", err.message);
        // Index and field targets stay legal.
        assert_eq!(expr_of("a[0] = 1"), "(= (index a 0) 1)");
        assert_eq!(expr_of("a.b = 1"), "(= (. a b) 1)");
    }

    #[test]
    fn postfix_operators_chain() {
        assert_eq!(expr_of("f(1)(2)"), "(call (call f 1) 2)");
        assert_eq!(expr_of("a.b.c"), "(. (. a b) c)");
        assert_eq!(expr_of("a[0][1]"), "(index (index a 0) 1)");
        assert_eq!(expr_of("a.b(1)[2]"), "(index (call (. a b) 1) 2)");
    }

    #[test]
    fn a_colon_in_a_subscript_makes_it_a_slice() {
        // Which form it is turns on the `:`, and either bound may be missing,
        // so all four shapes have to be distinguished from a plain index.
        assert_eq!(expr_of("a[1:2]"), "(slice a 1 2)");
        assert_eq!(expr_of("a[1:]"), "(slice a 1 )");
        assert_eq!(expr_of("a[:2]"), "(slice a  2)");
        assert_eq!(expr_of("a[:]"), "(slice a  )");
        assert_eq!(expr_of("a[1]"), "(index a 1)");
    }

    #[test]
    fn slice_bounds_are_full_expressions() {
        // The bounds parse with `expression`, so a `:` cannot be mistaken for
        // the start of one and arithmetic in a bound needs no parentheses.
        assert_eq!(
            expr_of("a[i + 1:len(a) - 1]"),
            "(slice a (+ i 1) (- (call len a) 1))"
        );
        assert_eq!(expr_of("a[:2][0]"), "(index (slice a  2) 0)");
    }

    #[test]
    fn calls_take_arguments_with_optional_trailing_comma() {
        assert_eq!(expr_of("f()"), "(call f )");
        assert_eq!(expr_of("f(1, 2)"), "(call f 1 2)");
        assert_eq!(expr_of("f(1, 2,)"), "(call f 1 2)");
    }

    #[test]
    fn lists_parse_with_optional_trailing_comma() {
        assert_eq!(expr_of("[]"), "[]");
        assert_eq!(expr_of("[1, 2, 3]"), "[1 2 3]");
        assert_eq!(expr_of("[1, 2,]"), "[1 2]");
        assert_eq!(expr_of("[[1], 2]"), "[[1] 2]");
    }

    #[test]
    fn dicts_parse_with_optional_trailing_comma() {
        assert_eq!(expr_of("({})"), "{}");
        assert_eq!(expr_of(r#"({"a": 1, "b": 2})"#), r#"{"a": 1 "b": 2}"#);
        assert_eq!(expr_of(r#"({"a": 1,})"#), r#"{"a": 1}"#);
        assert_eq!(expr_of("(({1 + 1: [2]}))"), "{(+ 1 1): [2]}");
    }

    #[test]
    fn a_brace_in_condition_position_still_opens_a_block() {
        // The ambiguity Rust has with struct literals does not arise here: a
        // dict literal is not a postfix form, so once a condition has parsed,
        // `{` can only be the block.
        let stmts = parse_ok("if a { }");
        let StmtKind::If { cond, then, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert_eq!(sexpr(cond), "a");
        assert!(then.stmts.is_empty());

        // And a dict literal *inside* the condition is still reachable.
        let stmts = parse_ok(r#"if a == {"k": 1} { }"#);
        let StmtKind::If { cond, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert_eq!(sexpr(cond), r#"(== a {"k": 1})"#);
    }

    #[test]
    fn a_statement_beginning_with_a_brace_is_a_block_not_a_dict() {
        assert!(matches!(parse_ok("{ }")[0].kind, StmtKind::Block(_)));
        let err = parse_err(r#"{ "a": 1 }"#);
        assert!(err.message.contains("needs parentheses"), "{}", err.message);
    }

    #[test]
    fn in_parses_as_a_comparison_level_operator() {
        assert_eq!(expr_of("a in b"), "(in a b)");
        assert_eq!(expr_of("a + 1 in b"), "(in (+ a 1) b)");
        assert_eq!(expr_of("a in b && c"), "(&& (in a b) c)");
        // The loop form takes `in` before any expression is parsed, so the two
        // uses cannot collide.
        let stmts = parse_ok("for k in d { }");
        let StmtKind::For { var, iter, .. } = &stmts[0].kind else {
            panic!("expected a for loop");
        };
        assert_eq!((var.as_str(), sexpr(iter)), ("k", "d".to_string()));
    }

    #[test]
    fn a_call_on_the_next_line_is_a_separate_statement() {
        // Without this rule `let a = b` followed by `(c)` would silently become
        // a call to `b`.
        let stmts = parse_ok("let a = b\n(c)");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0].kind, StmtKind::Let { .. }));
        assert_eq!(expr_of("(c)"), "c");
    }

    #[test]
    fn method_chains_may_break_across_lines() {
        assert_eq!(
            expr_of("a\n  .b()\n  .c()"),
            "(call (. (call (. a b) ) c) )"
        );
    }

    #[test]
    fn statements_may_be_separated_by_newline_or_semicolon() {
        assert_eq!(parse_ok("let a = 1\nlet b = 2").len(), 2);
        assert_eq!(parse_ok("let a = 1; let b = 2").len(), 2);
        assert_eq!(parse_ok("let a = 1;").len(), 1);
    }

    #[test]
    fn run_on_statements_are_rejected() {
        let err = parse_err("let a = 1 let b = 2");
        assert!(
            err.message.contains("expected a newline"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_three_binding_keywords_share_a_node() {
        let stmts = parse_ok("let a = 1\nfinal b = 2\nconst c = 3");
        let kinds: Vec<_> = stmts
            .iter()
            .map(|stmt| match &stmt.kind {
                StmtKind::Let { bind, .. } => *bind,
                other => panic!("unexpected statement: {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [BindKind::Let, BindKind::Final, BindKind::Const],
            "the keyword should be the only difference"
        );
    }

    #[test]
    fn if_else_if_chains_nest() {
        let stmts = parse_ok("if a { } else if b { } else { }");
        let StmtKind::If { otherwise, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        let inner = otherwise.as_ref().expect("expected an else branch");
        let StmtKind::If { otherwise, .. } = &inner.kind else {
            panic!("expected `else if` to nest an if");
        };
        assert!(matches!(
            otherwise.as_ref().map(|s| &s.kind),
            Some(StmtKind::Block(_))
        ));
    }

    #[test]
    fn else_may_sit_on_its_own_line() {
        let stmts = parse_ok("if a {\n}\nelse {\n}");
        let StmtKind::If { otherwise, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert!(otherwise.is_some());
    }

    #[test]
    fn loops_parse() {
        assert!(matches!(
            parse_ok("while a < 10 { a = a + 1 }")[0].kind,
            StmtKind::While { .. }
        ));
        let stmts = parse_ok("for item in [1, 2] { print(item) }");
        let StmtKind::For { var, .. } = &stmts[0].kind else {
            panic!("expected a for loop");
        };
        assert_eq!(var, "item");
    }

    #[test]
    fn return_may_omit_its_value() {
        let stmts = parse_ok("fn f() {\n  return\n}");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        assert!(matches!(decl.body.stmts[0].kind, StmtKind::Return(None)));
    }

    #[test]
    fn return_takes_a_value_on_the_same_line() {
        let stmts = parse_ok("fn f() { return 1 + 2 }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        let StmtKind::Return(Some(value)) = &decl.body.stmts[0].kind else {
            panic!("expected a returned value");
        };
        assert_eq!(sexpr(value), "(+ 1 2)");
    }

    #[test]
    fn functions_declare_parameters() {
        let stmts = parse_ok("fn add(a, b,) { return a + b }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        assert_eq!(decl.name, "add");
        let names: Vec<_> = decl.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    /// The methods of the one class in `src`.
    fn methods_of(src: &str) -> Vec<std::rc::Rc<FnDecl>> {
        let stmts = parse_ok(src);
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        methods.clone()
    }

    #[test]
    fn op_marks_a_method_the_language_calls() {
        let methods = methods_of("class C { op init(x) { self.x = x } }");
        assert_eq!(methods[0].name, "init");
        assert_eq!(methods[0].op, Some(Op::Init));
    }

    #[test]
    fn fn_leaves_a_method_ordinary_even_when_named_after_an_op() {
        // The whole point of marking: the name alone decides nothing, so this is
        // a method called as `c.init()` and nothing more.
        let methods = methods_of("class C { fn init(x) { self.x = x } }");
        assert_eq!(methods[0].name, "init");
        assert_eq!(methods[0].op, None);
    }

    #[test]
    fn a_misspelled_op_is_rejected_where_it_is_written() {
        let err = parse_err("class C { op innit(x) { self.x = x } }");
        assert!(
            err.message.contains("`innit` is not an operation"),
            "{}",
            err.message
        );
        // The list is the suggestion, so it has to be there.
        let help = err.help.expect("should say what op can define");
        assert!(help.contains("init"), "{help}");
        // Pointing at the name, not at the `op`.
        assert_eq!(&"class C { op innit(x) { self.x = x } }"[13..18], "innit");
        assert_eq!(err.span.start, 13);
    }

    #[test]
    fn an_op_declaring_the_wrong_number_of_parameters_is_rejected() {
        let src = "class C { op bool(a, b) { return true } }";
        let err = parse_err(src);
        assert!(
            err.message.contains("`op bool` takes 0 parameters, but 2"),
            "{}",
            err.message
        );
        // Pointing at the parameter list, which is the part to change.
        assert_eq!(&src[17..23], "(a, b)");
        assert_eq!(err.span.start, 17);
        assert_eq!(err.span.end, 23);

        // The count excludes `self`, so the one-parameter ops want exactly one
        // besides it — and the message says "parameter", not "parameters".
        let err = parse_err("class C { op add() { return 1 } }");
        assert!(
            err.message.contains("`op add` takes 1 parameter, but 0"),
            "{}",
            err.message
        );
    }

    /// `init` is the exception, and the only one.
    ///
    /// A constructor's parameters belong to the class. Checking them here would
    /// mean deciding how many arguments `Point(1, 2)` may pass, which is not the
    /// parser's to decide.
    #[test]
    fn an_op_init_may_declare_any_parameters() {
        for src in [
            "class C { op init() { } }",
            "class C { op init(a) { } }",
            "class C { op init(a, b, c) { } }",
        ] {
            let methods = methods_of(src);
            assert_eq!(methods[0].op, Some(Op::Init), "{src}");
        }
    }

    /// Every op is declarable at the arity the table gives it.
    ///
    /// Deliberately tautological about the *number* — it reads `arity()` to build
    /// the source, so it cannot tell a wrong number from a right one. What it
    /// catches is an op the check refuses at its own arity, and `self` being
    /// miscounted. `arity_is_what_the_language_passes` pins the numbers, and
    /// nothing can confirm them for real until each op is wired.
    #[test]
    fn every_op_can_be_declared_at_its_own_arity() {
        for op in crate::ast::OPS {
            let Some(arity) = op.arity() else { continue };
            let params: Vec<String> = (0..arity).map(|i| format!("p{i}")).collect();
            let src = format!(
                "class C {{ op {}({}) {{ }} }}",
                op.name(),
                params.join(", ")
            );
            let methods = methods_of(&src);
            assert_eq!(methods[0].op, Some(*op), "{src}");
            // `self` plus what the language passes.
            assert_eq!(methods[0].params.len(), arity + 1, "{src}");
        }
    }

    #[test]
    fn op_outside_a_class_is_rejected() {
        let err = parse_err("op init(x) { }");
        assert!(
            err.message.contains("only valid inside a class body"),
            "{}",
            err.message
        );
        assert!(err.help.is_some(), "should point at `fn`");
    }

    #[test]
    fn missing_brace_reports_where_it_was_expected() {
        let err = parse_err("if a  print(1) }");
        assert!(err.message.contains("expected `{`"), "{}", err.message);
    }

    #[test]
    fn spans_cover_the_whole_expression() {
        let src = "let x = 1 + 2 * 3";
        let stmts = parse_ok(src);
        let StmtKind::Let { value, .. } = &stmts[0].kind else {
            panic!("expected a let");
        };
        assert_eq!(
            &src[value.span.start as usize..value.span.end as usize],
            "1 + 2 * 3"
        );
        assert_eq!(
            &src[stmts[0].span.start as usize..stmts[0].span.end as usize],
            src
        );
    }

    #[test]
    fn parenthesised_spans_include_the_parens() {
        let src = "(1 + 2)";
        let stmts = parse_ok(src);
        assert_eq!(
            &src[stmts[0].span.start as usize..stmts[0].span.end as usize],
            src
        );
    }

    #[test]
    fn a_method_gets_self_as_its_first_parameter() {
        // The whole of what makes the receiver implicit: everything downstream
        // sees an ordinary parameter list.
        let stmts = parse_ok("class C {\n fn m(a, b) { return a }\n}");
        let StmtKind::Class { name, methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(name, "C");
        assert_eq!(methods.len(), 1);

        let params: Vec<_> = methods[0].params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["self", "a", "b"]);
    }

    #[test]
    fn a_plain_function_gets_no_self() {
        let stmts = parse_ok("fn f(a) { return a }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a function");
        };
        let params: Vec<_> = decl.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["a"]);
    }

    #[test]
    fn self_parses_as_an_ordinary_variable() {
        let stmts = parse_ok("class C {\n fn m() { return self.x }\n}");
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(sexpr(expr), "(. self x)");
    }

    #[test]
    fn a_superclass_is_a_name_like_any_other() {
        let stmts = parse_ok("class Dog extends Animal {}");
        let StmtKind::Class { name, parent, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(name, "Dog");
        assert_eq!(parent.as_ref().map(|p| p.name.as_str()), Some("Animal"));
    }

    #[test]
    fn super_must_be_followed_by_a_name() {
        // `super` alone has no useful value — the parent class is better named
        // directly — so requiring the `.name` puts the error on the `super`.
        assert_eq!(
            parse_err("class B extends A {\n fn m() { return super }\n}").message,
            "expected `.` after `super`, found `}`"
        );
    }

    #[test]
    fn every_modifier_marks_a_class_and_none_forbids_a_parent() {
        // A modifier says what may attach to the class from below and beside it,
        // never what it may descend from — so all three allow `extends`.
        for (src, expected) in [
            ("class Dog extends Animal {}", Openness::Open),
            ("final class Dog extends Animal {}", Openness::Final),
            ("complete class Dog extends Animal {}", Openness::Complete),
            ("sealed class Dog extends Animal {}", Openness::Sealed),
        ] {
            let stmts = parse_ok(src);
            let StmtKind::Class {
                name,
                parent,
                openness,
                ..
            } = &stmts[0].kind
            else {
                panic!("expected a class from `{src}`");
            };
            assert_eq!(name, "Dog");
            assert_eq!(*openness, expected, "`{src}`");
            assert_eq!(parent.as_ref().map(|p| p.name.as_str()), Some("Animal"));
        }
    }

    #[test]
    fn final_still_introduces_a_binding() {
        // `final` is the one modifier that is also a binding form, so it is the
        // one the parser has to look past `class` to tell apart. The lookahead is
        // a single token, and every other `final` goes where it always did.
        let stmts = parse_ok("final x = 1");
        let StmtKind::Let { name, bind, .. } = &stmts[0].kind else {
            panic!("expected a binding");
        };
        assert_eq!(name, "x");
        assert_eq!(*bind, BindKind::Final);

        // And a `final` in front of anything else is still a binding missing its
        // name, rather than a modifier the parser invented a meaning for.
        assert_eq!(
            parse_err("final extend int {}").message,
            "expected a name after `final`, found `extend`"
        );
    }

    #[test]
    fn a_modifier_has_nothing_to_say_without_a_class() {
        // `complete` and `sealed` introduce nothing else, so they need no
        // lookahead — and the error lands on what is missing rather than on a
        // binding the program never wrote.
        assert_eq!(
            parse_err("sealed x = 1").message,
            "expected `class` after `sealed`, found `x`"
        );
        assert_eq!(
            parse_err("complete fn f() {}").message,
            "expected `class` after `complete`, found `fn`"
        );
    }

    #[test]
    fn an_empty_class_is_allowed() {
        let stmts = parse_ok("class C {}");
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert!(methods.is_empty());
    }

    #[test]
    fn a_class_body_holds_only_methods() {
        // A `let` in a class body would otherwise parse as a statement and then
        // silently do nothing, since there is nowhere for it to go.
        assert_eq!(
            parse_err("class C {\n let x = 1\n}").message,
            "expected a method, found let"
        );
    }

    #[test]
    fn parses_the_hello_example() {
        let src = include_str!("../examples/hello.qn");
        let stmts = parse_ok(src);
        assert_eq!(stmts.len(), 3, "fn, let, if");
        assert!(matches!(stmts[0].kind, StmtKind::Fn { .. }));
        assert!(matches!(stmts[1].kind, StmtKind::Let { .. }));
        assert!(matches!(stmts[2].kind, StmtKind::If { .. }));
    }
}

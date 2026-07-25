use crate::ast::{
    self, BinaryOp, Block, Expr, ExprKind, FnDecl, LogicalOp, Param, Stmt, StmtKind, UnaryOp, Var,
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
            TokenKind::Let | TokenKind::Const => self.let_stmt(),
            TokenKind::Fn => self.fn_stmt(),
            TokenKind::Class => self.class_stmt(),
            TokenKind::If => self.if_stmt(),
            TokenKind::While => self.while_stmt(),
            TokenKind::For => self.for_stmt(),
            TokenKind::Return => self.return_stmt(),
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
        let mutable = keyword.kind == TokenKind::Let;
        let word = if mutable { "`let`" } else { "`const`" };

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
                mutable,
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

    /// Parses `fn name(params) { … }`, starting at the `fn`.
    ///
    /// A method gets `self` inserted as its first parameter, which is the whole
    /// of what makes the receiver implicit — see [`ast::SELF`]. Its span is the
    /// method's name, since there is no source text to point at and an error
    /// about `self` should land on the method that has one.
    fn fn_decl(&mut self, method: bool) -> Result<FnDecl, QuinceError> {
        self.advance();
        let (name, name_span) = self.expect_ident("after `fn`")?;
        self.expect(TokenKind::LParen, "after the function name")?;

        let mut params = Vec::new();
        if method {
            params.push(Param {
                name: ast::SELF.to_string(),
                span: name_span,
            });
        }
        while !self.check(&TokenKind::RParen) {
            let (name, span) = self.expect_ident("in the parameter list")?;
            params.push(Param { name, span });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "after the parameter list")?;

        let body = self.block()?;
        Ok(FnDecl { name, params, body })
    }

    fn class_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.advance().span;
        let (name, _) = self.expect_ident("after `class`")?;

        let parent = if self.eat(&TokenKind::Extends) {
            let (parent, _) = self.expect_ident("after `extends`")?;
            Some(Var::new(parent))
        } else {
            None
        };

        self.expect(TokenKind::LBrace, "after the class name")?;

        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            if !self.check(&TokenKind::Fn) {
                return Err(QuinceError::new(
                    format!("expected a method, found {}", self.peek().kind),
                    self.peek().span,
                ));
            }
            methods.push(std::rc::Rc::new(self.fn_decl(true)?));
        }
        let end = self.expect(TokenKind::RBrace, "after the class body")?.span;

        Ok(Stmt {
            kind: StmtKind::Class {
                name,
                parent,
                methods,
                slot: None,
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
    fn let_and_const_differ_only_in_mutability() {
        let stmts = parse_ok("let a = 1\nconst b = 2");
        match (&stmts[0].kind, &stmts[1].kind) {
            (
                StmtKind::Let { mutable: true, .. },
                StmtKind::Let {
                    mutable: false,
                    name,
                    ..
                },
            ) => assert_eq!(name, "b"),
            other => panic!("unexpected bindings: {other:?}"),
        }
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

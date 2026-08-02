//! Recursive descent for statements, Pratt for expression precedence.
//!
//! Four files, split along the grammar's own seams. This one holds the parser
//! itself, the token helpers every production uses, and the statement dispatch;
//! `stmt` holds the control-flow forms, `decl` the things that declare a
//! name, and `expr` the precedence climb.
//!
//! The split follows where the milestones after v0.6 add grammar. v0.7 puts type
//! annotations on declarations and two new operators in the climb; v0.9 puts a
//! type-parameter list on `class`; v0.10 adds `match`, `if let`, and the range
//! operator. Each of those has one file to go in.

mod decl;
mod expr;
mod stmt;

#[cfg(test)]
mod tests;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::syntax::ast::{Block, Stmt, StmtKind};
use crate::syntax::token::{Span, Token, TokenKind};

/// An error for text that does not parse.
fn syntax(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Syntax)
}

/// An error for text that parses and still is not a program.
///
/// The parser raises both, which is why these are two functions rather than one
/// applied at the stage boundary. A handful of rules are checked here and not in
/// the resolver because everything they need is already in hand — the keyword
/// and its span, before a body is parsed — and being caught early does not make
/// them grammar. Reading which function a site calls is how you tell.
fn declaration(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Declaration)
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
    pub fn parse(mut self) -> Result<Vec<Stmt>> {
        let mut stmts = Vec::new();
        while !self.at_end() {
            stmts.push(self.statement()?);
        }
        Ok(stmts)
    }

    // -- statements --------------------------------------------------------
    fn statement(&mut self) -> Result<Stmt> {
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
            TokenKind::Op => Err(declaration(
                "`op` is only valid inside a class body",
                self.peek().span,
            )
            .with_help("use `fn` for a function that is called by name")),
            TokenKind::Class => self.class_stmt(),
            TokenKind::Extend => self.extend_stmt(),
            TokenKind::Import => self.import_stmt(),
            // `from` is not reserved — see `KEYWORDS`. It introduces an import
            // only here, at the start of a statement with an `import` two tokens
            // along, and `from` is an ordinary name everywhere else including in
            // the very next line of this file.
            TokenKind::Ident(_) if self.at_from_import() => self.import_stmt(),
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
    // -- blocks ------------------------------------------------------------

    fn block(&mut self) -> Result<Block> {
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

    /// Whether the statement starting here is `from <module> import …`.
    ///
    /// The deeper of the parser's two lookaheads, and it exists for one word.
    /// `from` is not reserved, so nothing about the first token says this is an
    /// import; the `import` two along is what says it. Deciding any earlier would
    /// mean taking the word, and `op init(from, to)` is how anyone writes a
    /// range.
    fn at_from_import(&self) -> bool {
        if !matches!(&self.peek().kind, TokenKind::Ident(name) if name == "from") {
            return false;
        }
        if !matches!(
            self.tokens.get(self.pos + 1).map(|token| &token.kind),
            Some(TokenKind::Ident(_))
        ) {
            return false;
        }
        // A `.` or `/` counts as well as the `import` itself, so that
        // `from a.b import c` is recognised as the import it was meant to be and
        // refused by `refuse_path` — which says what is wrong with it — rather
        // than falling through to be parsed as an expression and reported as a
        // missing newline. Neither can be anything else after `from <name>`.
        matches!(
            self.tokens.get(self.pos + 2).map(|token| &token.kind),
            Some(TokenKind::Import | TokenKind::Dot | TokenKind::Slash)
        )
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

    fn expect(&mut self, kind: TokenKind, context: &str) -> Result<Token> {
        if self.check(&kind) {
            return Ok(self.advance());
        }
        let found = self.peek();
        Err(syntax(
            format!("expected `{kind}` {context}, found `{}`", found.kind),
            found.span,
        ))
    }

    fn expect_ident(&mut self, context: &str) -> Result<(String, Span)> {
        let token = self.peek();
        if let TokenKind::Ident(name) = &token.kind {
            let result = (name.clone(), token.span);
            self.advance();
            return Ok(result);
        }
        Err(syntax(
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
    fn end_of_statement(&mut self) -> Result<()> {
        if self.eat(&TokenKind::Semi) || self.at_statement_end() {
            return Ok(());
        }
        let token = self.peek();
        // A `{` at the start of a statement always opens a block, so a bare dict
        // literal gets parsed as one and fails here on its first `:`. Saying so
        // is far more use than naming the token.
        if token.kind == TokenKind::Colon {
            return Err(syntax(
                "unexpected `:` — a `{` at the start of a statement opens a block, \
                 so a dict literal there needs parentheses around it",
                token.span,
            ));
        }
        Err(syntax(
            format!(
                "expected a newline or `;` after this statement, found `{}`",
                token.kind
            ),
            token.span,
        ))
    }
}

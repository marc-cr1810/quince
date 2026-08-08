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
use crate::syntax::ast::{Block, Stmt, StmtKind, Visibility};
use crate::syntax::doc::Doc;
use crate::syntax::parser::decl::Member;
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
/// The words written in front of a declaration, before the keyword that says
/// which kind of declaration it is.
///
/// One bundle rather than three parameters because this is the list the
/// milestones grow: v0.8 adds `const`, `override`, and `explicit` here, and each
/// of those would otherwise be another argument threaded through `fn_decl`.
pub(super) struct Modifiers {
    pub doc: Option<Doc>,
    pub visibility: Visibility,
    /// Where the visibility word was written, for the reports that refuse one.
    /// `None` when none was written, which is the same reach and not the same
    /// thing to point at.
    pub vis_span: Option<Span>,
    /// The v0.8 words, each `Some` at the span it was written at.
    ///
    /// Spans rather than bools because every one of them can be refused —
    /// `explicit` on something that is not a constructor, `override` at the top
    /// level — and a refusal has to underline the word rather than the
    /// declaration under it. Whether one was written is `is_some()`.
    pub constant: Option<Span>,
    pub overrides: Option<Span>,
    pub guarded: Option<Span>,
    pub explicit: Option<Span>,
}

impl Modifiers {
    /// The bundle a declaration form that takes none of them hands on.
    fn plain(doc: Option<Doc>, visibility: Visibility, vis_span: Option<Span>) -> Modifiers {
        Modifiers {
            doc,
            visibility,
            vis_span,
            constant: None,
            overrides: None,
            guarded: None,
            explicit: None,
        }
    }
}

/// Where a `fn` or an `op` is being declared, which is what decides whether its
/// modifiers mean anything.
///
/// The three differ in exactly two properties — whether there is a receiver, and
/// whether there is something above the declaration for `override` and `final`
/// to be about — so it is one parameter rather than two bools that can disagree.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Site {
    /// A plain `fn`, at the top level or inside a block: no receiver, and
    /// nothing above it.
    Free,
    /// A member of a class body.
    Member,
    /// A method an `extend` block adds. It takes a receiver like a member and
    /// still has nothing above it: an extension may not shadow, so what it adds
    /// is never an override, and no subclass reaches it through the class table
    /// for `final` to guard.
    Extension,
}

impl Site {
    /// Whether the parser inserts `self` as the first parameter.
    fn takes_receiver(self) -> bool {
        !matches!(self, Site::Free)
    }

    /// Whether `override` and `final` can be true here.
    fn inherits(self) -> bool {
        matches!(self, Site::Member)
    }

    /// What to call it in the report that refuses a modifier.
    fn what(self) -> &'static str {
        match self {
            Site::Free => "a plain function",
            Site::Member => "a class member",
            Site::Extension => "a method added by `extend`",
        }
    }
}

/// Whether `kind` is one of the words that may precede `fn` or `op`.
///
/// Read by [`Parser::declaration_modifiers`] to know when to stop, and by the
/// class body to look *past* them: `final` and `const` introduce a field as well
/// as qualifying a method, so which of the two a member is cannot be decided
/// until the first word that is neither.
fn is_member_modifier(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Const | TokenKind::Override | TokenKind::Final | TokenKind::Explicit
    )
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// How many blocks deep the parser is, so a form that means something only
    /// at the top level can say so.
    ///
    /// Visibility is the one such form today: a `private let` inside a function
    /// has no importer to hide from, so it is a word that would do nothing, and
    /// a modifier that does nothing is worse than one that is refused.
    depth: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            depth: 0,
        }
    }

    /// Parses a bare type, for the tests that assert about one.
    ///
    /// A type is not a statement, so there is no entry point that reaches
    /// `type_expr` from a string — and a test that wrapped one in a `let` would
    /// be asserting about the binding's checks as much as about the annotation.
    #[cfg(test)]
    pub fn parse_type_for_test(mut self) -> Result<crate::syntax::ast::TypeExpr> {
        self.type_expr()
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

    /// Parses a whole program, recovering from syntax errors where possible.
    pub fn parse_recovering(mut self) -> (Vec<Stmt>, Vec<Raised>) {
        let mut stmts = Vec::new();
        let mut errors = Vec::new();
        while !self.at_end() {
            match self.statement() {
                Ok(stmt) => stmts.push(stmt),
                Err(err) => {
                    errors.push(err);
                    self.synchronize();
                }
            }
        }
        (stmts, errors)
    }

    fn synchronize(&mut self) {
        self.advance();
        while !self.at_end() {
            if self.pos > 0 && self.tokens[self.pos - 1].kind == TokenKind::Semi {
                return;
            }
            match self.peek().kind {
                TokenKind::Class
                | TokenKind::Fn
                | TokenKind::Let
                | TokenKind::Final
                | TokenKind::Const
                | TokenKind::Override
                | TokenKind::For
                | TokenKind::If
                | TokenKind::While
                | TokenKind::Return
                | TokenKind::Import
                | TokenKind::Try
                | TokenKind::Alias
                | TokenKind::Public
                | TokenKind::Private
                | TokenKind::Protected
                | TokenKind::Extend => return,
                _ => {}
            }
            if self.peek().newline_before {
                return;
            }
            self.advance();
        }
    }

    // -- statements --------------------------------------------------------
    fn statement(&mut self) -> Result<Stmt> {
        match self.peek().kind {
            // Four words that open more than one form. `final` is a binding, a
            // class modifier, and a member guard; `const` is a binding and a
            // purity marker; `override` and `explicit` only ever precede a
            // method, and are dispatched here so that one written anywhere else
            // is refused by the form that knows why rather than falling through
            // to be parsed as an expression.
            TokenKind::Let
            | TokenKind::Final
            | TokenKind::Const
            | TokenKind::Override
            | TokenKind::Explicit => match self.member_kind() {
                Member::Method => self.fn_stmt(),
                Member::Class => self.class_stmt(),
                Member::Field => self.let_stmt(),
                Member::Neither => {
                    let (word, span) = (self.peek().kind.clone(), self.peek().span);
                    Err(declaration(
                        format!("`{word}` does not declare anything on its own"),
                        span,
                    )
                    .with_help(
                        "it is a word in front of a declaration — a `let`, a `fn`, an `op`, or \
                         a `class`",
                    ))
                }
            },
            TokenKind::Complete | TokenKind::Sealed => self.class_stmt(),
            TokenKind::Fn => self.fn_stmt(),
            // A visibility word says what an importing module sees, so it is a
            // word about the top level and is refused anywhere else.
            TokenKind::Public | TokenKind::Private | TokenKind::Protected => self.exported_stmt(),
            // An `op` is a method the language calls on an instance, so there is
            // nothing for one to belong to out here.
            TokenKind::Op => Err(declaration(
                "`op` is only valid inside a class body",
                self.peek().span,
            )
            .with_help("use `fn` for a function that is called by name")),
            TokenKind::Alias => self.alias_stmt(),
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
            // `++i` and `--i`. Dispatched here rather than in the precedence
            // climb because that is what makes them statements: the climb never
            // accepts one, so `f(++i)` is refused with the reason rather than
            // parsed into something with a value.
            TokenKind::PlusPlus | TokenKind::MinusMinus => self.incr_stmt(),
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
    /// A top-level declaration with a visibility word in front of it.
    ///
    /// Only three forms can carry one — a binding, a function, and a class —
    /// because those are the three a module exports. `import` is not among them:
    /// a name a module imported is not a name it declared, and re-export is a
    /// question this milestone does not answer.
    fn exported_stmt(&mut self) -> Result<Stmt> {
        let (word, span) = (self.peek().kind.clone(), self.peek().span);

        if self.depth > 0 {
            return Err(declaration(format!("`{word}` means nothing here"), span).with_help(
                "visibility says what an importing module sees, so it belongs on a top-level \
                 declaration — inside a function there is nobody it could hide the name from",
            ));
        }

        // The word is left where it is: each declaration form reads its own
        // modifiers, so dispatching is all this has to do. `member_kind` reads
        // past the whole run of words to whatever they qualify, which is the
        // same ambiguity `statement` resolves for an unexported declaration and
        // is resolved the same way.
        let after = self.tokens[self.pos + 1].kind.clone();
        match after {
            TokenKind::Alias => return self.alias_stmt(),
            TokenKind::Op => {
                return Err(declaration(
                    "`op` is only valid inside a class body",
                    self.tokens[self.pos + 1].span,
                )
                .with_help("use `fn` for a function that is called by name"));
            }
            _ => {}
        }
        match self.member_kind() {
            Member::Method => self.fn_stmt(),
            Member::Class => self.class_stmt(),
            Member::Field => self.let_stmt(),
            Member::Neither => Err(declaration(
                format!("expected a declaration after `{word}`, found {after}"),
                span.to(self.tokens[self.pos + 1].span),
            )
            .with_help(
                "`public`, `private`, and `protected` say what an importing module sees, so \
                 one goes in front of a `let`, `fn`, or `class`",
            )),
        }
    }

    /// Reads the words in front of a declaration: its `##` block, and its
    /// visibility if one is written.
    ///
    /// `what` names the thing being declared, for [`Self::doc_of`]. The doc is
    /// taken from the *first* token of the header, which is the visibility word
    /// when there is one — a `##` block sits above what it documents, and what
    /// it documents starts at the first word the program wrote.
    pub(super) fn modifiers(&mut self, what: &str) -> Result<Modifiers> {
        let header = self.peek().clone();
        let (visibility, vis_span) = self.visibility_word();
        Ok(Modifiers::plain(
            Self::doc_of(&header, what)?,
            visibility,
            vis_span,
        ))
    }

    /// The same, plus the v0.8 words a `fn` or an `op` may carry.
    ///
    /// The canonical order is visibility first — `public const fn` — and every
    /// other order is accepted and normalized, because there is nothing an
    /// ordering rule would catch that is a mistake rather than a habit. What is
    /// refused is writing one *twice*, which is a copy-and-paste and reads as
    /// though it meant something the second time.
    pub(super) fn declaration_modifiers(&mut self, what: &str) -> Result<Modifiers> {
        let mut modifiers = self.modifiers(what)?;
        loop {
            let token = self.peek();
            if !is_member_modifier(&token.kind) {
                break;
            }
            let (word, span) = (token.kind.clone(), token.span);
            let slot = match word {
                TokenKind::Const => &mut modifiers.constant,
                TokenKind::Override => &mut modifiers.overrides,
                TokenKind::Final => &mut modifiers.guarded,
                _ => &mut modifiers.explicit,
            };
            if slot.is_some() {
                return Err(declaration(format!("`{word}` is written twice"), span)
                    .with_help("one of them says nothing the other did not — delete it"));
            }
            *slot = Some(span);
            self.advance();
            // A visibility word after one of these is still a visibility word.
            // Accepting it here is what makes `const public fn` parse, and the
            // documentation still comes off the first token of the header.
            if modifiers.vis_span.is_none() {
                let (visibility, vis_span) = self.visibility_word();
                if vis_span.is_some() {
                    modifiers.visibility = visibility;
                    modifiers.vis_span = vis_span;
                }
            }
        }
        Ok(modifiers)
    }

    /// Eats a visibility word if the next token is one.
    ///
    /// Writing `public` and writing nothing are the same reach, and the span is
    /// what tells them apart for a report that has to quote the word back.
    pub(super) fn visibility_word(&mut self) -> (Visibility, Option<Span>) {
        let visibility = match self.peek().kind {
            TokenKind::Public => Visibility::Public,
            TokenKind::Private => Visibility::Private,
            TokenKind::Protected => Visibility::Protected,
            _ => return (Visibility::Public, None),
        };
        (visibility, Some(self.advance().span))
    }

    // -- blocks ------------------------------------------------------------

    fn block(&mut self) -> Result<Block> {
        let open = self.expect(TokenKind::LBrace, "to open a block")?;
        let mut stmts = Vec::new();
        self.depth += 1;
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            stmts.push(self.statement()?);
        }
        self.depth -= 1;
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

    /// The token after the current one.
    ///
    /// Wanted by the two operators written as two words — `not in` and `is not`
    /// — whose first word means something else on its own, so which form is
    /// being read cannot be decided without the second in hand.
    fn peek_ahead(&self) -> &TokenKind {
        self.tokens
            .get(self.pos + 1)
            .map_or(&TokenKind::Eof, |token| &token.kind)
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
        // A `++` still sitting here is always the same mistake: `expr_stmt` took
        // every one that ends a statement, so this one is inside an expression —
        // `f(i++)`, `if i++ > 3`. Naming the rule beats naming the token, and
        // the token is what the generic message below would name.
        if matches!(found.kind, TokenKind::PlusPlus | TokenKind::MinusMinus) {
            return Err(stmt::incr_outside_statement(found));
        }
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
        // `let x = i++`: the `++` reached the end of a statement it is not the
        // end *of*, because the assignment already claimed the expression.
        if matches!(token.kind, TokenKind::PlusPlus | TokenKind::MinusMinus) {
            return Err(stmt::incr_outside_statement(token));
        }
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

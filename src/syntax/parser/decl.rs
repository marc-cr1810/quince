//! The forms that declare a name: `fn`, `op`, `class`, `extend`, `import`.
//!
//! Where every modifier keyword the milestones add gets read — v0.7's
//! `public`/`private`/`protected` and its `: T` annotations, v0.8's `const`,
//! `override`, and `explicit`, v0.9's `[T]` parameter lists, v0.10's `enum`.


use crate::error::QuinceError;
use crate::syntax::ast::{
    self, FnDecl, ImportName, ImportNames, Op, Openness, Param, Stmt, StmtKind, Var,
};
use crate::syntax::doc::Doc;
use crate::syntax::parser::{Parser, declaration, syntax};
use crate::syntax::token::{Token, TokenKind};

impl Parser {
    pub(super) fn fn_stmt(&mut self) -> Result<Stmt, QuinceError> {
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
    pub(super) fn fn_decl(&mut self, method: bool) -> Result<FnDecl, QuinceError> {
        let token = self.advance();
        let doc = Self::doc_of(&token, "a function")?;
        let keyword = token.kind.clone();
        let is_op = keyword == TokenKind::Op;
        let after = if is_op { "after `op`" } else { "after `fn`" };
        let (name, name_span) = self.expect_ident(after)?;

        let op = if is_op {
            // Listing the set beats naming it: the list is short, it is the
            // answer to "then what can I write", and it grows with the language.
            Some(Op::from_name(&name).ok_or_else(|| {
                let names: Vec<&str> = ast::OPS.iter().map(|op| op.name()).collect();
                declaration(
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
                return Err(declaration(
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

        // Checked here because here is where the parameter list is in hand, so
        // the report can say what the function *does* take. A `@param` that
        // named nothing would otherwise be documentation nobody could find the
        // mistake in.
        if let Some(doc) = &doc {
            doc.check(&params)?;
        }

        let body = self.block()?;
        Ok(FnDecl {
            name,
            name_span,
            params,
            body,
            op,
            doc,
        })
    }

    /// The documentation attached to a token, parsed and checked for shape.
    ///
    /// `what` names the thing being declared, for the report when a tag is
    /// written that the declaration has no room for — `@return` above a `let`
    /// describes nothing, and there is no reading of it that is right.
    ///
    /// A `fn` is the one form with a signature, so it is the one form that
    /// checks nothing here and everything later: its `@param`s are checked
    /// against the parameter list once that has been read.
    pub(super) fn doc_of(token: &Token, what: &str) -> Result<Option<Doc>, QuinceError> {
        let Some(block) = &token.doc else {
            return Ok(None);
        };
        let doc = Doc::parse(block)?;
        if what != "a function" {
            doc.check_has_no_signature(what)?;
        }
        // A block of empty `##` lines documents nothing, and carrying it would
        // make an editor render a heading with no text under it.
        Ok((!doc.is_empty()).then_some(doc))
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
    pub(super) fn refuse_duplicate(
        declared: &[std::rc::Rc<FnDecl>],
        decl: &FnDecl,
        whose: &str,
    ) -> Result<(), QuinceError> {
        if !declared.iter().any(|seen| seen.name == decl.name) {
            return Ok(());
        }
        Err(declaration(
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
    pub(super) fn class_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.peek().span;
        // The modifier when there is one, since that is the first token of the
        // header and so the one the documentation attached to.
        let doc = Self::doc_of(self.peek(), "a class")?;
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
                return Err(syntax(
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
                doc,
            },
            span: start.to(end),
        })
    }

    /// `extend int { fn double() { … } }`.
    ///
    /// Shaped like a class body with the two halves a class has and an extension
    /// does not: no name to bind, and no `extends` clause, because an extension
    /// declares no type for anything to descend from.
    pub(super) fn extend_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.advance().span;
        let (target, target_span) = self.expect_ident("after `extend`")?;

        self.expect(TokenKind::LBrace, "after the type being extended")?;

        let mut methods = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            // Refused here rather than at the class, because everything the check
            // needs is in hand: the keyword and its span, before a body is parsed.
            // The same reason `op` at the top level is caught in this file.
            if !self.check(&TokenKind::Fn) && !self.check(&TokenKind::Op) {
                return Err(syntax(
                    format!("expected a method or op, found {}", self.peek().kind),
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

    /// `import math` and `from math import floor, ceil`.
    ///
    /// One function for both, because they are one statement written two ways —
    /// the module is named either side of the `import`, and which side it lands
    /// on decides only what gets bound.
    pub(super) fn import_stmt(&mut self) -> Result<Stmt, QuinceError> {
        let start = self.peek().span;
        let from = self.advance().kind != TokenKind::Import;

        if !from {
            let (module, module_span) = self.expect_ident("after `import`")?;
            self.refuse_path(&module)?;
            self.end_of_statement()?;
            return Ok(Stmt {
                kind: StmtKind::Import {
                    module,
                    module_span,
                    names: ImportNames::Module,
                },
                span: start.to(module_span),
            });
        }

        let (module, module_span) = self.expect_ident("after `from`")?;
        self.refuse_path(&module)?;
        self.expect(TokenKind::Import, "after the module being imported from")?;

        let mut names = Vec::new();
        loop {
            let (name, span) = self.expect_ident("in an import list")?;
            names.push(ImportName { name, span });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        // Unreachable — the loop above runs at least once — but the span below
        // is built from the last name and the compiler cannot know that.
        let end = names.last().map_or(module_span, |last| last.span);
        self.end_of_statement()?;

        Ok(Stmt {
            kind: StmtKind::Import {
                module,
                module_span,
                names: ImportNames::Names(names),
            },
            span: start.to(end),
        })
    }

    /// Refuses `import utils/strings` and `import utils.strings`.
    ///
    /// Caught here because here is where the `/` or the `.` still exists: by the
    /// time the evaluator has a module name it has only the identifier, and the
    /// generic "expected a newline after this statement" that the statement
    /// terminator would otherwise produce sends someone looking at their line
    /// endings rather than at the shape of what they asked for.
    ///
    /// The rule it enforces is that an import names a file beside the importer.
    /// A path syntax has to answer what it is relative to, what a package is,
    /// and how a search order works — all decisions that want a language with
    /// modules already in use.
    pub(super) fn refuse_path(&mut self, module: &str) -> Result<(), QuinceError> {
        if !self.check(&TokenKind::Slash) && !self.check(&TokenKind::Dot) {
            return Ok(());
        }
        Err(syntax(
            format!("`{module}` is a module name, and cannot be part of a path"),
            self.peek().span,
        )
        .with_help(
            "an import names a file beside this one, written without a directory or an extension",
        ))
    }
}

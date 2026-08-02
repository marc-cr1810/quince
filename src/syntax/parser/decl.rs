//! The forms that declare a name: `fn`, `op`, `class`, `extend`, `import`.
//!
//! Where every modifier keyword the milestones add gets read — v0.7's
//! `public`/`private`/`protected` and its `: T` annotations, v0.8's `const`,
//! `override`, and `explicit`, v0.9's `[T]` parameter lists, v0.10's `enum`.


use crate::error::Result;
use crate::syntax::ast::{
    self, BindKind, FieldDecl, FnDecl, ImportName, ImportNames, Op, Openness, Param, Stmt, StmtKind,
    TypeExpr, TypeName, Var,
};
use crate::syntax::doc::Doc;
use crate::syntax::parser::{Modifiers, Parser, declaration, syntax};
use crate::syntax::token::{Token, TokenKind};

impl Parser {
    pub(super) fn fn_stmt(&mut self) -> Result<Stmt> {
        let start = self.peek().span;
        let modifiers = self.modifiers("a function")?;
        let decl = self.fn_decl(false, modifiers)?;
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
    pub(super) fn fn_decl(&mut self, method: bool, modifiers: Modifiers) -> Result<FnDecl> {
        let Modifiers {
            doc,
            visibility,
            vis_span,
        } = modifiers;
        let token = self.advance();
        let keyword = token.kind.clone();
        let is_op = keyword == TokenKind::Op;
        let after = if is_op { "after `op`" } else { "after `fn`" };

        // The language calls an `op` on the program's behalf, from outside the
        // class — so a private one would be a method `print` is entitled to call
        // and forbidden from calling. Refused at the declaration rather than at
        // the call, because the call is in the evaluator and has no word to point
        // at. `public op` is allowed and says nothing new, as it does anywhere.
        if is_op && visibility.closes_outside() {
            let word = visibility.word().expect("a restricting word is written");
            return Err(declaration(
                format!("an `op` may not be {word}"),
                vis_span.expect("a restricting word was written, so it has a span"),
            )
            .with_help(
                "the language calls an `op` itself, from outside the class — one it is not \
                 allowed to reach could never run",
            ));
        }

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
                // The receiver's type is the class, and the class is not in hand
                // here — nor would writing it help, since nobody passes `self`.
                ty: None,
                // Rebindable, as it always was. Refusing `self = x` is the
                // resolver's, and it refuses it by name rather than by kind.
                bind: BindKind::Let,
                receiver: true,
            });
        }
        while !self.check(&TokenKind::RParen) {
            // The binding word first, as it is on a `let` — a parameter is a
            // binding the caller fills in, so the two forms are spelled alike.
            let bind = match self.peek().kind {
                TokenKind::Final => {
                    self.advance();
                    BindKind::Final
                }
                TokenKind::Const => {
                    self.advance();
                    BindKind::Const
                }
                _ => BindKind::Let,
            };
            let (name, span) = self.expect_ident("in the parameter list")?;
            let ty = self.annotation()?;
            params.push(Param {
                name,
                span,
                ty,
                bind,
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

        let returns = self.annotation()?;

        // An `op` with a fixed contract may not declare a return that disagrees
        // with it. This is a check on the *annotation*, not new enforcement:
        // the language already refuses a wrong value at run time, at nine sites
        // reading the same table this does. What it buys is catching the
        // declaration before the op is ever called.
        if let Some(op) = op
            && let Some(contract) = op.answers()
            && let Some(declared) = &returns
        {
            let disagrees = match &declared.name {
                // `any` is wider than the contract, so it is a claim that the op
                // may answer with something it may not.
                TypeName::Any => true,
                TypeName::Named(name) => name != contract,
            };
            // `op string(): string?` is refused too: the contract is a string,
            // and a `nil` is not one.
            if disagrees || declared.nullable {
                return Err(declaration(
                    format!(
                        "`op {}` answers with `{contract}`, but this declares `{}`",
                        op.name(),
                        declared.written()
                    ),
                    declared.span,
                )
                .with_help(format!(
                    "the language calls `op {}` itself and requires `{contract}` back — write \
                     `{contract}`, or write no return type at all",
                    op.name()
                )));
            }
        }

        let body = self.block()?;
        Ok(FnDecl {
            name,
            name_span,
            params,
            body,
            op,
            returns,
            visibility,
            doc,
        })
    }

    /// `private let balance = 0`, inside a class body.
    ///
    /// The binding forms mean here what they mean anywhere — `let` reassignable,
    /// `final` bound once, `const` frozen — so this reads the same keyword and
    /// differs only in where the result lands. An initializer is required; the
    /// blank form waits for the annotations that would give it a default.
    pub(super) fn field_decl(&mut self, modifiers: Modifiers) -> Result<FieldDecl> {
        let keyword = self.advance();
        let bind = match keyword.kind {
            TokenKind::Let => BindKind::Let,
            TokenKind::Final => BindKind::Final,
            _ => BindKind::Const,
        };
        let word = format!("`{}`", bind.word());

        let (name, name_span) = self.expect_ident(&format!("after {word}"))?;
        let ty = self.annotation()?;
        self.expect(TokenKind::Assign, &format!("in a {word} field"))?;
        let value = self.expression()?;
        self.end_of_statement()?;

        Ok(FieldDecl {
            name,
            name_span,
            bind,
            visibility: modifiers.visibility,
            ty,
            value,
            doc: modifiers.doc,
        })
    }

    /// Refuses a field a body has already declared.
    ///
    /// The same collision `refuse_duplicate` refuses among methods, and for the
    /// same reason — but kept apart from it because a field and a method do not
    /// share a table: `fn total` beside `let total` is two different things
    /// reached two different ways, and refusing the pair would be inventing a
    /// rule the evaluator does not have.
    pub(super) fn refuse_duplicate_field(
        declared: &[FieldDecl],
        field: &FieldDecl,
        whose: &str,
    ) -> Result<()> {
        if !declared.iter().any(|seen| seen.name == field.name) {
            return Ok(());
        }
        Err(declaration(
            format!("{whose} already declares a field `{}`", field.name),
            field.name_span,
        )
        .with_help(
            "the second would replace the first without a word — rename it, or delete the \
             one you meant to be rid of",
        ))
    }

    /// An annotation after a `:`, when one is written.
    ///
    /// The `:` is the marker, so this answers `None` without consuming anything
    /// when there is no annotation — which is what lets every declaration form
    /// call it unconditionally.
    pub(super) fn annotation(&mut self) -> Result<Option<TypeExpr>> {
        if !self.eat(&TokenKind::Colon) {
            return Ok(None);
        }
        self.type_expr().map(Some)
    }

    /// A type: `int`, `int?`, `list[string]`, `any`, `_`, `const dict[string, int]`.
    ///
    /// Recursive through the argument list, which is what makes
    /// `list[dict[string, int]]` fall out rather than be a case.
    pub(super) fn type_expr(&mut self) -> Result<TypeExpr> {
        let start = self.peek().span;
        // `const` first, because it qualifies whatever follows rather than being
        // part of the name — `const list[int]`, never `list[const int]`.
        let frozen = self.eat(&TokenKind::Const);

        let name = match self.peek().kind.clone() {
            TokenKind::Any => {
                self.advance();
                TypeName::Any
            }
            // `_` is an ordinary identifier everywhere else, and is the wildcard
            // only here. Recognising it in type position costs nothing a program
            // can observe: this grammar is new, so no `_` was ever written in it.
            TokenKind::Ident(word) if word == "_" => {
                self.advance();
                TypeName::Any
            }
            TokenKind::Ident(word) => {
                self.advance();
                TypeName::Named(word)
            }
            other => {
                return Err(syntax(
                    format!("expected a type, found {other}"),
                    self.peek().span,
                ));
            }
        };

        let mut args = Vec::new();
        if self.eat(&TokenKind::LBracket) {
            loop {
                args.push(self.type_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                // A trailing comma before the `]`, as the argument lists and
                // collection literals already allow.
                if self.check(&TokenKind::RBracket) {
                    break;
                }
            }
            self.expect(TokenKind::RBracket, "after the type arguments")?;
        }

        // `int??` is not a second kind of absent. It lexes as one `??`, so both
        // characters are in hand before a `?` is even eaten — and a `??` in type
        // position is otherwise the coalescing operator somewhere it cannot be.
        if self.check(&TokenKind::QuestionQuestion) {
            return Err(declaration(
                "a type is nullable or it is not — `??` says nothing `?` did not",
                start.to(self.peek().span),
            )
            .with_help("write one `?`"));
        }
        let nullable = self.eat(&TokenKind::Question);

        let end = self.tokens[self.pos.saturating_sub(1)].span;
        Ok(TypeExpr {
            name,
            args,
            nullable,
            frozen,
            span: start.to(end),
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
    pub(super) fn doc_of(token: &Token, what: &str) -> Result<Option<Doc>> {
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
    ) -> Result<()> {
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

    /// `alias ScoreTable = dict[string, int]`.
    ///
    /// Parsed like a binding and resolved like nothing: an alias declares a
    /// *name for a type*, so there is no value, no slot, and nothing for the
    /// evaluator to run. The resolver substitutes it and the statement is gone.
    pub(super) fn alias_stmt(&mut self) -> Result<Stmt> {
        let start = self.peek().span;
        let modifiers = self.modifiers("an alias")?;
        self.advance();
        let (name, name_span) = self.expect_ident("after `alias`")?;
        self.expect(TokenKind::Assign, "in an alias")?;
        let ty = self.type_expr()?;
        let span = start.to(ty.span);
        self.end_of_statement()?;

        Ok(Stmt {
            kind: StmtKind::Alias {
                name,
                name_span,
                ty,
                visibility: modifiers.visibility,
                doc: modifiers.doc,
            },
            span,
        })
    }

    /// `class Point { … }`, with an optional modifier in front of it.
    ///
    /// The span starts at the modifier when there is one, so a report about the
    /// declaration underlines the header the program wrote rather than the half
    pub(super) fn class_stmt(&mut self) -> Result<Stmt> {
        let start = self.peek().span;
        // The modifier when there is one, since that is the first token of the
        // header and so the one the documentation attached to. Visibility comes
        // before openness — `public final class` — because it is a word about
        // the name and the other is a word about the type.
        let header = self.peek().clone();
        let (visibility, _) = self.visibility_word();
        let doc = Self::doc_of(&header, "a class")?;
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
        let mut fields = Vec::new();
        while !self.check(&TokenKind::RBrace) && !self.at_end() {
            // The visibility word comes first and says nothing about which of the
            // two this is, so it is read before the branch and handed to whichever
            // side takes it. The doc block is read from the word, for the reason
            // the class header reads its own from the modifier.
            let member = self.peek().clone();
            let (visibility, vis_span) = self.visibility_word();
            match self.peek().kind {
                TokenKind::Fn | TokenKind::Op => {
                    let modifiers = Modifiers {
                        doc: Self::doc_of(&member, "a function")?,
                        visibility,
                        vis_span,
                    };
                    let decl = self.fn_decl(true, modifiers)?;
                    Self::refuse_duplicate(&methods, &decl, &name)?;
                    methods.push(std::rc::Rc::new(decl));
                }
                TokenKind::Let | TokenKind::Final | TokenKind::Const => {
                    let modifiers = Modifiers {
                        doc: Self::doc_of(&member, "a field")?,
                        visibility,
                        vis_span,
                    };
                    let field = self.field_decl(modifiers)?;
                    Self::refuse_duplicate_field(&fields, &field, &name)?;
                    fields.push(field);
                }
                _ => {
                    return Err(syntax(
                        format!("expected a field or a method, found {}", self.peek().kind),
                        self.peek().span,
                    ));
                }
            }
        }
        let end = self.expect(TokenKind::RBrace, "after the class body")?.span;

        Ok(Stmt {
            kind: StmtKind::Class {
                name,
                parent,
                parent_span,
                methods,
                fields,
                openness,
                visibility,
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
    pub(super) fn extend_stmt(&mut self) -> Result<Stmt> {
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
            let modifiers = self.modifiers("a function")?;
            let decl = self.fn_decl(true, modifiers)?;
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
    pub(super) fn import_stmt(&mut self) -> Result<Stmt> {
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
    pub(super) fn refuse_path(&mut self, module: &str) -> Result<()> {
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

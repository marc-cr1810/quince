//! The forms that declare a name: `fn`, `op`, `class`, `extend`, `import`.
//!
//! Where every modifier keyword the milestones add gets read — v0.7's
//! `public`/`private`/`protected` and its `: T` annotations, v0.8's `const`,
//! `override`, and `explicit`, v0.9's `[T]` parameter lists, v0.10's `enum`.


use crate::error::Result;
use crate::syntax::ast::{
    self, BindKind, Expr, ExprKind, FieldDecl, FnDecl, ImportName, ImportNames, Op, Openness, Param,
    Stmt, StmtKind, TypeExpr, TypeName, TypeParam, Var,
};
use crate::syntax::doc::Doc;
use crate::syntax::parser::{Modifiers, Parser, Site, declaration, is_member_modifier, syntax};
use crate::syntax::token::{Span, Token, TokenKind};

/// Whether the name is one of the language's own types.
///
/// `any` is in, though it is not a class: it is a type a program can write, and
/// a parameter shadowing it would be no less confusing for the difference.
fn names_a_builtin_type(name: &str) -> bool {
    name == "any"
        || crate::runtime::class::BUILTINS
            .iter()
            .any(|builtin| builtin.name() == name)
}

/// What the declaration starting at the current token turns out to be.
///
/// Read by the class body, by the `extend` body, and by statement dispatch,
/// which have the same problem for the same reason: `final` and `const` open a
/// binding *and* qualify a method, and `final` opens a class as well, so the
/// first word says nothing about which form this is.
///
/// [`Member::Neither`] rather than an `Option`, because "none of the three" is a
/// report each caller writes in its own words and not a case for them to invent
/// one for.
pub(super) enum Member {
    /// A binding: a field in a class body, a `let` at statement level.
    Field,
    Method,
    Class,
    Neither,
}

impl Parser {
    /// Reads ahead over the modifier words to see which kind of member follows.
    ///
    /// Consumes nothing. The ambiguity is entirely `final` and `const`, which
    /// are both binding keywords and both method modifiers, so the answer is
    /// whatever the first word that is *neither* a visibility word nor a
    /// modifier turns out to be. `let` settles it on its own — nothing but a
    /// field starts with it.
    pub(super) fn member_kind(&self) -> Member {
        let mut at = self.pos;
        // Whether a word that could have opened a field has gone past. If the
        // run ends at something that is not `fn` or `op`, that word was the
        // binding keyword after all.
        let mut binding = false;
        loop {
            let Some(token) = self.tokens.get(at) else {
                return Member::Neither;
            };
            match token.kind {
                TokenKind::Public | TokenKind::Private | TokenKind::Protected => at += 1,
                TokenKind::Const | TokenKind::Final => {
                    binding = true;
                    at += 1;
                }
                TokenKind::Let => return Member::Field,
                TokenKind::Fn | TokenKind::Op => return Member::Method,
                // `complete` and `sealed` introduce nothing else, so they need
                // no run of their own — reaching one means a class either way.
                TokenKind::Class | TokenKind::Complete | TokenKind::Sealed => {
                    return Member::Class;
                }
                _ if is_member_modifier(&token.kind) => at += 1,
                _ if binding => return Member::Field,
                _ => return Member::Neither,
            }
        }
    }

    pub(super) fn fn_stmt(&mut self) -> Result<Stmt> {
        let start = self.peek().span;
        let modifiers = self.declaration_modifiers("a function")?;
        let decl = self.fn_decl(Site::Free, modifiers)?;
        Ok(Stmt {
            span: start.to(decl.body.span),
            kind: StmtKind::Fn {
                decl: std::rc::Rc::new(decl),
                slot: None,
                // Set by the resolver, which is the only pass that sees the
                // whole scope and so the only one that can say whether another
                // declaration got here first.
                overload: false,
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
    pub(super) fn fn_decl(&mut self, site: Site, modifiers: Modifiers) -> Result<FnDecl> {
        let Modifiers {
            doc,
            visibility,
            vis_span,
            constant,
            overrides,
            guarded,
            explicit,
        } = modifiers;
        let method = site.takes_receiver();

        // `override` and `final` are claims about a superclass, so they can only
        // be written where there is one to make a claim about. Refused here
        // rather than at the resolver because the answer needs nothing but the
        // position — and because the resolver, seeing `override` on a plain
        // `fn`, could only report it as overriding nothing, which describes the
        // symptom rather than the mistake.
        if !site.inherits() {
            for (word, span) in [("override", overrides), ("final", guarded)] {
                let Some(span) = span else { continue };
                return Err(declaration(
                    format!("`{word}` means nothing on {}", site.what()),
                    span,
                )
                .with_help(match site {
                    Site::Extension =>
                        "an extension adds to a type and never replaces part of it, so nothing \
                         it declares can override or be overridden",
                    _ =>
                        "both words are about a superclass member — they belong on a method or \
                         an `op` declared in a class body",
                }));
            }
        }

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
                // Nobody omits the receiver, so there is nothing to default it
                // to — it is filled by the call itself.
                default: None,
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
            let default = match self.eat(&TokenKind::Assign) {
                true => Some(self.expression()?),
                false => None,
            };
            // A mandatory parameter after a defaulted one is refused: there is
            // no call that could reach it positionally, so it is not a parameter
            // with a surprising rule, it is a parameter with no way to be
            // filled. Caught here because the list is what the caret wants.
            if default.is_none()
                && let Some(earlier) = params.iter().rev().find(|param| param.default.is_some())
            {
                return Err(declaration(
                    format!("`{name}` has no default, but `{}` before it does", earlier.name),
                    span,
                )
                .with_help(
                    "every call that omitted the earlier one would have to omit this one too, \
                     and then there is nothing to give it — move it in front, or give it a \
                     default of its own",
                ));
            }
            params.push(Param {
                name,
                span,
                ty,
                bind,
                default,
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

        // `explicit` turns off the coercion a single-parameter constructor gives
        // its class, so it is meaningless on anything that is not one. Both
        // halves are checked here, where the op and the parameter list are in
        // hand and the report can say which of the two failed.
        if let Some(at) = explicit {
            let taken = params.len() - usize::from(method);
            let wrong = match op {
                Some(Op::Init) => (taken != 1).then(|| {
                    format!("`op init` takes {taken} parameters, and only a one-parameter constructor coerces")
                }),
                _ => Some(format!(
                    "`{}` is not a constructor",
                    match op {
                        Some(op) => format!("op {}", op.name()),
                        None => format!("fn {name}"),
                    }
                )),
            };
            if let Some(wrong) = wrong {
                return Err(declaration(format!("`explicit` means nothing here — {wrong}"), at)
                    .with_help(
                        "`explicit` refuses the implicit coercion of `let x: T = value`, and \
                         only a one-parameter `op init` offers one to refuse",
                    ));
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
            constant: constant.is_some(),
            overrides: overrides.is_some(),
            guarded: guarded.is_some(),
            explicit: explicit.is_some(),
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
        let (value, defaulted) = match self.eat(&TokenKind::Assign) {
            true => (self.expression()?, false),
            false => (Self::default_for(ty.as_ref(), name_span), true),
        };
        self.end_of_statement()?;

        Ok(FieldDecl {
            name,
            name_span,
            bind,
            visibility: modifiers.visibility,
            ty,
            value,
            defaulted,
            doc: modifiers.doc,
        })
    }

    /// What a declaration with no `= value` holds.
    ///
    /// `nil` where nothing was annotated, which is the dynamic binding v0.7
    /// already describes; otherwise the value the annotated type answers with.
    ///
    /// **A builtin answers with a literal**, not with a call: `int` is `0`,
    /// `string` is `""`, `list` is `[]`. v0.8 §3.4 already gave the two
    /// containers theirs this way, and the scalars join them — an `int` field a
    /// constructor is about to overwrite should not have to be written `= 0`,
    /// and a language that synthesizes `[]` and refuses `0` is drawing the line
    /// somewhere nobody can predict.
    ///
    /// v0.7 §3.3 argued the other way — "zero is a value somebody chose" — and
    /// what changed is generics: a field annotated `T` cannot be written with an
    /// initializer that suits every argument, so a type parameter with no
    /// default would make `class Pair[A, B]` unwritable. The line moved to where
    /// it can be defended: a *class* still says what it needs, because a class
    /// can, and `any` has no default because it names no representation.
    ///
    /// A class answers with a call to itself, built as an ordinary
    /// [`ExprKind::Call`] so the resolver gives its callee a slot and the
    /// evaluator constructs it through the path every other construction takes.
    /// The *base* name, so a user's `Stack[int]` reaches `Stack()` — what the
    /// arguments say is enforced afterwards, when the annotation is checked
    /// against the value, which is also what stamps the descriptor.
    ///
    /// A nullable annotation answers `nil` whatever it names. `int?` is a
    /// declaration that the absent case is real, and `0` is not the absent case.
    pub(super) fn default_for(ty: Option<&TypeExpr>, span: Span) -> Expr {
        let nil = Expr { kind: ExprKind::Nil, span };
        let Some(ty) = ty else { return nil };
        if ty.nullable {
            return nil;
        }
        let TypeName::Named(named) = &ty.name else {
            // `any` and `_`. Not a class, so there is nothing to call, and no
            // representation to have a zero of — the resolver refuses the
            // declaration before this can run.
            return nil;
        };
        let literal = match named.as_str() {
            "int" => Some(ExprKind::Int(0)),
            "float" => Some(ExprKind::Float(0.0)),
            "string" => Some(ExprKind::Str(String::new())),
            "bool" => Some(ExprKind::Bool(false)),
            "list" => Some(ExprKind::List(Vec::new())),
            "dict" => Some(ExprKind::Dict(Vec::new())),
            _ => None,
        };
        match literal {
            Some(kind) => Expr { kind, span },
            None => Expr {
                kind: ExprKind::Call {
                    callee: Box::new(Expr {
                        kind: ExprKind::Var(Var::new(named.clone())),
                        span,
                    }),
                    args: Vec::new(),
                },
                span,
            },
        }
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

    /// The `[T, U]` after a declaration's name, when one is written.
    ///
    /// Answers with an empty list without consuming anything when there is no
    /// bracket, so every declaration form can call it unconditionally — the
    /// same shape [`Self::annotation`] has, and for the same reason.
    ///
    /// `whose` names the declaration, because every refusal here is about a
    /// parameter list and the reader needs to know which one.
    pub(super) fn type_params(&mut self, whose: &str) -> Result<Vec<TypeParam>> {
        if !self.eat(&TokenKind::LBracket) {
            return Ok(Vec::new());
        }
        let mut params: Vec<TypeParam> = Vec::new();
        loop {
            // A parameter is a bare name and nothing else.
            let (name, span) = match self.peek().kind.clone() {
                TokenKind::Ident(word) => {
                    self.advance();
                    (word, self.tokens[self.pos - 1].span)
                }
                other => {
                    return Err(declaration(
                        format!("expected a type parameter, found {other}"),
                        self.peek().span,
                    )
                    .with_help(
                        "a parameter list declares names — `class Stack[T]` — and the types go \
                         in where the class is used",
                    ));
                }
            };
            // `class Stack[int]` lexes as a declaration of a parameter *named*
            // `int`, because a builtin type name is an ordinary identifier and
            // not a keyword. Almost always it is a use written where a
            // declaration goes — and where it is not, it makes `int` mean
            // something else for the length of the body, which no reader is
            // going to survive.
            if names_a_builtin_type(&name) {
                return Err(declaration(
                    format!("`{name}` is a type, so it cannot name a type parameter"),
                    span,
                )
                .with_help(format!(
                    "a parameter list declares names to stand for types — write \
                     `class {whose}[T]`, and `{name}` where `{whose}` is used"
                )));
            }
            // Two parameters of one name make every mention of it ambiguous,
            // and there is no reading of the second that is right.
            if let Some(earlier) = params.iter().find(|seen| seen.name == name) {
                return Err(declaration(
                    format!("`{whose}` already declares a type parameter `{name}`"),
                    span,
                )
                .with_label(earlier.span, "the first one")
                .with_help(format!(
                    "rename one of them — a mention of `{name}` cannot reach both"
                )));
            }
            params.push(TypeParam { name, span });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
            // A trailing comma, as every other bracketed list allows.
            if self.check(&TokenKind::RBracket) {
                break;
            }
        }
        self.expect(TokenKind::RBracket, "after the type parameters")?;
        Ok(params)
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
        let params = self.type_params(&name)?;

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
            // Which of the two this is cannot be read off the next word: `final`
            // and `const` introduce a field *and* qualify a method, so the
            // decision waits for the first token that is neither a visibility
            // word nor a modifier. The look ahead consumes nothing, because
            // each side reads its own modifiers and the two do not take the
            // same set.
            match self.member_kind() {
                Member::Method => {
                    let modifiers = self.declaration_modifiers("a function")?;
                    let decl = self.fn_decl(Site::Member, modifiers)?;
                    methods.push(std::rc::Rc::new(decl));
                }
                Member::Field => {
                    let modifiers = self.modifiers("a field")?;
                    let field = self.field_decl(modifiers)?;
                    Self::refuse_duplicate_field(&fields, &field, &name)?;
                    fields.push(field);
                }
                Member::Class | Member::Neither => {
                    return Err(syntax(
                        format!("expected a field or a method, found {}", self.peek().kind),
                        self.peek().span,
                    )
                    .with_help(
                        "a class body holds declarations and nothing else — `let` for a field, \
                         `fn` for a method, `op` for one the language calls",
                    ));
                }
            }
        }
        let end = self.expect(TokenKind::RBrace, "after the class body")?.span;

        Ok(Stmt {
            kind: StmtKind::Class {
                name,
                params,
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
            // The same reason `op` at the top level is caught in this file. An
            // extension declares no fields — there is no instance of its own to
            // put one on — so anything that is not a method is this report.
            if !matches!(self.member_kind(), Member::Method) {
                return Err(syntax(
                    format!("expected a method or op, found {}", self.peek().kind),
                    self.peek().span,
                )
                .with_help(
                    "an `extend` block adds methods to a type that already exists — it declares \
                     no fields, because it allocates nothing to hold one",
                ));
            }
            let modifiers = self.declaration_modifiers("a function")?;
            let decl = self.fn_decl(Site::Extension, modifiers)?;
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

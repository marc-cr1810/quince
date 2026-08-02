//! Type mistakes the pass can see without running the program.
//!
//! Advisory, and only that. §5 puts the enforcement of an annotation at run
//! time, and this does not move it: the language still refuses `let x: int =
//! "s"` when the binding executes, with the same message from the same
//! function. What this adds is the editor saying so first.
//!
//! Being approximate is what makes that acceptable here and unacceptable in the
//! resolver. A *refusal* that fires only where inference happened to succeed
//! would be a rule nobody could state; a *squiggle* that appears only where the
//! answer is certain is ordinary editor behaviour, and one that stayed silent
//! would simply be an editor doing less.
//!
//! So every rule below is one-sided. Nothing is reported unless the pass knows
//! both types and they definitely disagree — `Unknown` on either side reports
//! nothing, and so does anything this cannot decide.

use crate::error::{ErrorKind, QuinceError, Raised};
use crate::sema::infer::Types;
use crate::sema::types::{Type, stated};
use crate::syntax::ast::{Block, FieldDecl, Stmt, StmtKind, TypeExpr};

/// Every type mistake decidable from the source, in source order.
///
/// A `Vec` rather than a `Result` because an editor wants all of them: stopping
/// at the first is right for a compiler that cannot continue and wrong for a
/// document someone is still typing.
pub fn check(program: &[Stmt], types: &Types) -> Vec<Raised> {
    let mut found = Vec::new();
    stmts(program, types, &mut found);
    found.sort_by_key(|err| err.span.start);
    found
}

fn stmts(stmts: &[Stmt], types: &Types, found: &mut Vec<Raised>) {
    for stmt in stmts {
        one(stmt, types, found);
    }
}

fn one(stmt: &Stmt, types: &Types, found: &mut Vec<Raised>) {
    match &stmt.kind {
        StmtKind::Let {
            name, value, ty, ..
        } => {
            if let Some(ty) = ty {
                let held = types.of_expr(value.span.start);
                if let Some(err) = disagrees(ty, &held, &format!("`{name}`"), types, value.span) {
                    found.push(err);
                }
            }
        }
        StmtKind::Class { methods, fields, .. } => {
            for field in fields {
                check_field(field, types, found);
            }
            for decl in methods {
                stmts(&decl.body.stmts, types, found);
            }
        }
        StmtKind::Fn { decl, .. } => stmts(&decl.body.stmts, types, found),
        StmtKind::Extend { methods, .. } => {
            for decl in methods {
                stmts(&decl.body.stmts, types, found);
            }
        }
        StmtKind::If { then, otherwise, .. } => {
            block(then, types, found);
            if let Some(other) = otherwise {
                one(other, types, found);
            }
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => block(body, types, found),
        StmtKind::Try { body, handler, .. } => {
            block(body, types, found);
            block(handler, types, found);
        }
        StmtKind::Block(inner) => block(inner, types, found),
        StmtKind::Expr(_)
        | StmtKind::Return(_)
        | StmtKind::Throw(_)
        | StmtKind::Alias { .. }
        | StmtKind::Import { .. } => {}
    }
}

fn block(block: &Block, types: &Types, found: &mut Vec<Raised>) {
    stmts(&block.stmts, types, found);
}

fn check_field(field: &FieldDecl, types: &Types, found: &mut Vec<Raised>) {
    let Some(ty) = &field.ty else {
        return;
    };
    let held = types.of_expr(field.value.span.start);
    if let Some(err) = disagrees(ty, &held, &format!("`{}`", field.name), types, field.value.span) {
        found.push(err);
    }
}

/// The report for an initializer that cannot hold, or `None` for every case
/// this is not certain about.
///
/// The certainty is the whole design. Each `return None` below is a case where
/// the pass could be wrong, and being silent there is what keeps a squiggle
/// worth believing — an editor that cries wolf on a correct program is worse
/// than one that says nothing.
fn disagrees(
    ty: &TypeExpr,
    held: &Type,
    what: &str,
    types: &Types,
    span: crate::syntax::token::Span,
) -> Option<Raised> {
    let annotated = stated(ty);
    // `any` and `_` state the top type, which nothing disagrees with.
    let (Some(wanted), Some(actual)) = (annotated.class_name(), held.class_name()) else {
        return None;
    };
    // An annotation naming something that is not a type at all is a different
    // mistake with its own report, raised when the annotation is applied. Saying
    // "this is an int" about it would be answering a question nobody asked.
    if !is_a_type(types, wanted) {
        return None;
    }

    // `nil` is the one case where the *names* agreeing is not the question.
    if actual == "nil" {
        return match ty.admits_nil() {
            true => None,
            false => Some(refusal(
                format!("{what} is `{}`, which does not admit `nil`", ty.written()),
                format!("write `{}?` if it may be absent", ty.written()),
                span,
            )),
        };
    }
    if wanted == actual {
        // The names agree, so the arguments are the question. A literal's
        // elements are visible, so the pass infers them — `["a"]` is a
        // `list[string]` — and `let xs: list[int] = ["a"]` is a mistake anyone
        // reading the line can see. Only where *both* sides carry arguments,
        // since a literal that says nothing about its elements answers with the
        // bare `list` and there is nothing to compare.
        let (want, have) = (annotated.args(), held.args());
        if want.is_empty() || have.is_empty() || want.len() != have.len() {
            return None;
        }
        let disagreeing = want
            .iter()
            .zip(have)
            .find(|(want, have)| !fits(types, want, have));
        return disagreeing.map(|(want, have)| {
            refusal(
                format!(
                    "{what} is `{}`, but this is `{}`",
                    ty.written(),
                    held
                ),
                format!("`{have}` does not hold as `{want}`"),
                span,
            )
        });
    }
    // §4.1's widening: a float admits an int.
    if wanted == "float" && actual == "int" {
        return None;
    }
    // A subclass holds as its parent. The chain is the program's, so this is
    // the one rule that has to ask the pass rather than decide for itself.
    if descends_from(types, actual, wanted) {
        return None;
    }

    Some(refusal(
        format!("{what} is `{}`, but this is {}", ty.written(), an(actual)),
        match (wanted, actual) {
            ("int", "float") => "write `int(x)` to say which way it should round".to_string(),
            _ => format!("`{}` does not hold as `{}`", actual, ty.written()),
        },
        span,
    ))
}

/// Whether `class` is `ancestor` or descends from it, as the pass understands
/// the program's hierarchy.
///
/// A name the pass has never heard of is treated as agreeing, because answering
/// `false` would report a mistake about a class it simply was not told about —
/// one declared in an imported module, say. The run-time check is what actually
/// decides.
///
/// "Never heard of" is narrower than it sounds and getting it wrong made this
/// check report nothing at all: a builtin is not declared by the program and is
/// perfectly well known, so `string` has to count as known or every comparison
/// against one bails out.
fn descends_from(types: &Types, class: &str, ancestor: &str) -> bool {
    if !is_a_type(types, class) {
        return true;
    }
    let mut current = Some(class);
    let mut seen = 0;
    while let Some(name) = current {
        if name == ancestor {
            return true;
        }
        // The pass may be handed a cyclic hierarchy — `class A extends B` and
        // `class B extends A` is refused at run time, not here — so the walk is
        // bounded rather than trusting the chain to end.
        seen += 1;
        if seen > 64 {
            return true;
        }
        current = types.parent_of(name);
    }
    false
}

/// Whether a value of `have` may sit where `want` was asked for.
///
/// The §4.1 table again, one level down, and one-sided in the same way: an
/// answer it cannot be sure of is `true`. Written apart from [`disagrees`]
/// because that one produces a *report* about a named thing, and an argument
/// has no name to produce one about — it is a `list[…]`'s element, and what
/// the reader is shown is both whole types.
fn fits(types: &Types, want: &Type, have: &Type) -> bool {
    let (Some(wanted), Some(actual)) = (want.class_name(), have.class_name()) else {
        // `Unknown` on either side, which settles nothing.
        return true;
    };
    if actual == "nil" {
        return want.admits_nil();
    }
    if !is_a_type(types, wanted) {
        return true;
    }
    if wanted == actual {
        let (want, have) = (want.args(), have.args());
        return want.is_empty()
            || have.is_empty()
            || want.len() != have.len()
            || want.iter().zip(have).all(|(want, have)| fits(types, want, have));
    }
    // §4.1's widening, and a subclass holding as its parent.
    wanted == "float" && actual == "int" || descends_from(types, actual, wanted)
}

/// Whether the pass has heard of a type by this name.
///
/// A builtin, or a class the program declared. Narrower than it sounds, and
/// getting it wrong made this check report nothing at all: a builtin is not
/// *declared* by the program and is perfectly well known, so `string` has to
/// count or every comparison against one bails out.
fn is_a_type(types: &Types, name: &str) -> bool {
    types.declares_class(name)
        || crate::runtime::class::BUILTINS
            .iter()
            .any(|builtin| builtin.name() == name)
}

fn refusal(message: String, help: String, span: crate::syntax::token::Span) -> Raised {
    QuinceError::new(message, span)
        .with_kind(ErrorKind::Type)
        .with_help(help)
}

fn an(name: &str) -> String {
    match name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => format!("an {name}"),
        false => format!("a {name}"),
    }
}

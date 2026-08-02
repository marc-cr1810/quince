//! Substituting type aliases, before anything reads a type.
//!
//! An alias introduces no type. `alias ScoreTable = dict[string, int]` makes
//! `ScoreTable` and `dict[string, int]` one thing — `is` cannot tell them apart,
//! and a report prints whichever the program wrote. The way to make that true
//! rather than nearly true is to remove the alias entirely before any later pass
//! can form an opinion about the name, which is what this does.
//!
//! v0.9 gives an alias parameters (`alias Pair[T] = tuple[T, T]`), which is a
//! substitution with arguments rather than a different mechanism. It lands here.

use std::collections::HashMap;

use crate::error::{QuinceError, Raised, Result};
use crate::error::ErrorKind;
use crate::runtime::dict::KEY_TYPES;
use crate::syntax::ast::{Expr, ExprKind, Stmt, StmtKind, TypeExpr, TypeName};
use crate::syntax::token::Span;

/// Expands every alias in `program`, in place.
///
/// Two passes and not one. Aliases are collected first so that one may name
/// another declared below it, which is the same courtesy the resolver extends to
/// a function calling one further down the file — a rule that held for code and
/// not for types would be arbitrary.
pub(super) fn expand(program: &mut [Stmt]) -> Result<()> {
    let mut declared = HashMap::new();
    // In declaration order, kept beside the map. Which alias a cycle is
    // reported at has to be the same on every run, and a `HashMap`'s iteration
    // order is not — `alias A = B` / `alias B = A` blamed whichever the hash
    // happened to yield first.
    let mut order = Vec::new();
    collect(program, &mut declared, &mut order)?;

    // Each alias is expanded to a form containing no aliases *before* anything
    // is substituted with it, so a use site is rewritten once rather than
    // repeatedly until it settles. This is also where a cycle is found: the
    // resolution of `A` reaches `A`.
    let mut resolved: HashMap<String, TypeExpr> = HashMap::new();
    for name in &order {
        let expanded = resolve_one(name, &declared, &mut Vec::new())?;
        resolved.insert(name.clone(), expanded);
    }

    for stmt in program {
        substitute_stmt(stmt, &resolved)?;
    }
    Ok(())
}

/// Gathers every `alias` declaration, refusing a second one for a name.
fn collect(
    stmts: &mut [Stmt],
    into: &mut HashMap<String, TypeExpr>,
    order: &mut Vec<String>,
) -> Result<()> {
    for stmt in stmts {
        // Top level only. An alias inside a function would be a type visible for
        // the length of a block, and nothing else in the language scopes a
        // *type* — the parser allows it where a statement goes, and this is
        // where the decision to keep them file-wide is actually made.
        if let StmtKind::Alias {
            name,
            name_span,
            ty,
            ..
        } = &stmt.kind
        {
            if into.contains_key(name) {
                return Err(redeclared(name, *name_span));
            }
            into.insert(name.clone(), ty.clone());
            order.push(name.clone());
        }
    }
    Ok(())
}

/// One alias, expanded until it names no alias — or a cycle, refused.
fn resolve_one(
    name: &str,
    declared: &HashMap<String, TypeExpr>,
    path: &mut Vec<String>,
) -> Result<TypeExpr> {
    if path.iter().any(|seen| seen == name) {
        path.push(name.to_string());
        // Reported at the definition the cycle closes on, which is the one
        // somebody has to change.
        let span = declared.get(name).map_or(Span::new(0, 0), |ty| ty.span);
        return Err(cycles(path, span));
    }
    let Some(ty) = declared.get(name) else {
        // Not an alias, so nothing to expand. The name is checked for being a
        // type at all when the annotation is applied.
        unreachable!("only a declared alias is resolved");
    };
    path.push(name.to_string());
    let expanded = expand_type(ty, declared, path)?;
    path.pop();
    Ok(expanded)
}

/// Rewrites a type, replacing any alias it names with what that alias means.
fn expand_type(
    ty: &TypeExpr,
    declared: &HashMap<String, TypeExpr>,
    path: &mut Vec<String>,
) -> Result<TypeExpr> {
    let mut args = Vec::with_capacity(ty.args.len());
    for arg in &ty.args {
        args.push(expand_type(arg, declared, path)?);
    }

    let TypeName::Named(name) = &ty.name else {
        return Ok(TypeExpr { args, ..ty.clone() });
    };
    if !declared.contains_key(name.as_str()) {
        return Ok(TypeExpr { args, ..ty.clone() });
    }

    // The alias stands where the use site was, so it keeps the use site's span —
    // a report about `let x: ScoreTable = 1` should underline what was written,
    // not the declaration somewhere above. The two qualifiers combine rather
    // than being replaced: `const ScoreTable?` means what it says whatever the
    // alias was declared as.
    let target = resolve_one(name, declared, path)?;
    Ok(TypeExpr {
        name: target.name,
        args: target.args,
        nullable: ty.nullable || target.nullable,
        frozen: ty.frozen || target.frozen,
        span: ty.span,
    })
}

/// A declaration error about a type, worded where the type was written.
fn refused(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Declaration)
}

fn redeclared(name: &str, span: Span) -> Raised {
    QuinceError::new(format!("`{name}` is already an alias"), span)
        .with_kind(ErrorKind::Declaration)
        .with_help("the second would replace the first without a word")
}

fn cycles(path: &[String], span: Span) -> Raised {
    let start = path.first().cloned().unwrap_or_default();
    QuinceError::new(format!("`{start}` is defined in terms of itself"), span)
        .with_kind(ErrorKind::Declaration)
    .with_help(format!(
        "the definitions go {} — an alias abbreviates a type, so it cannot be one of the \
         things it abbreviates",
        path.join(" → ")
    ))
}

/// Refuses a container type whose arguments do not fit it.
///
/// Two rules: the arity — `list` takes one argument, `dict` one or two — and
/// §4.2's key constraint, which is [`KEY_TYPES`] written as a check.
///
/// Run here rather than in the parser, which is where it started and where it
/// was wrong: `dict[UserID, int]` is a good annotation whose key type cannot be
/// read until `UserID` is expanded. Anything deciding whether arguments fit has
/// to see the type after substitution.
///
/// Only the two containers the language has. A class the program declared takes
/// no arguments until v0.9 gives it some, and saying so here would be a rule
/// that has to be removed rather than extended, so an unknown head is left
/// alone: the name is checked when the annotation is applied.
pub(super) fn check_arguments(ty: &TypeExpr) -> Result<()> {
    let TypeName::Named(head) = &ty.name else {
        // `any` takes no arguments and the parser gives it none.
        return Ok(());
    };
    let (head, args, span) = (head.as_str(), ty.args.as_slice(), ty.span);
    let arity = match head {
        "list" => 1..=1,
        "dict" => 1..=2,
        // Not a container, so it takes no arguments — and a program that wrote
        // some has said something the language cannot read.
        _ if !args.is_empty() => {
            return Err(refused(
                format!("`{head}` takes no type arguments"),
                span,
            )
            .with_help("only `list` and `dict` are parameterised in this version"));
        }
        _ => return Ok(()),
    };

    if !args.is_empty() && !arity.contains(&args.len()) {
        let written = if *arity.start() == *arity.end() {
            format!("{} argument", arity.start())
        } else {
            format!("{} or {} arguments", arity.start(), arity.end())
        };
        let were = if args.len() == 1 { "was" } else { "were" };
        return Err(refused(
            format!("`{head}` takes {written}, but {} {were} written", args.len()),
            span,
        ));
    }

    // The key is the first argument, for both `dict[K, V]` and the `dict[K]`
    // shorthand. `any` is admitted: it says the keys are heterogeneous, which
    // every one of the hashable types already is against the others.
    if head == "dict"
        && let Some(key) = args.first()
        && let TypeName::Named(name) = &key.name
        && !KEY_TYPES.contains(&name.as_str())
    {
        return Err(refused(
            format!("`{name}` cannot be a dict key"),
            key.span,
        )
        .with_help(format!(
            "a dict is keyed by one of {} — a class is not a key, and one declaring `op eq` \
             gives up being one",
            KEY_TYPES.join(", ")
        )));
    }
    Ok(())
}


/// Walks a statement, rewriting every type it holds.
fn substitute_stmt(stmt: &mut Stmt, resolved: &HashMap<String, TypeExpr>) -> Result<()> {
    match &mut stmt.kind {
        StmtKind::Alias { ty, .. } => substitute(ty, resolved)?,
        StmtKind::Let { ty, value, .. } => {
            if let Some(ty) = ty {
                substitute(ty, resolved)?;
            }
            substitute_expr(value, resolved)?;
        }
        StmtKind::Fn { decl, .. } => substitute_fn(decl, resolved)?,
        StmtKind::Class { methods, fields, .. } => {
            for decl in methods {
                substitute_fn(decl, resolved)?;
            }
            for field in fields {
                if let Some(ty) = &mut field.ty {
                    substitute(ty, resolved)?;
                }
                substitute_expr(&mut field.value, resolved)?;
            }
        }
        StmtKind::Extend { methods, .. } => {
            for decl in methods {
                substitute_fn(decl, resolved)?;
            }
        }
        StmtKind::Expr(expr) | StmtKind::Throw(expr) => substitute_expr(expr, resolved)?,
        StmtKind::Return(value) => {
            if let Some(value) = value {
                substitute_expr(value, resolved)?;
            }
        }
        StmtKind::If { cond, then, otherwise } => {
            substitute_expr(cond, resolved)?;
            for stmt in &mut then.stmts {
                substitute_stmt(stmt, resolved)?;
            }
            if let Some(other) = otherwise {
                substitute_stmt(other, resolved)?;
            }
        }
        StmtKind::While { cond, body } => {
            substitute_expr(cond, resolved)?;
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, resolved)?;
            }
        }
        StmtKind::For { iter, body, .. } => {
            substitute_expr(iter, resolved)?;
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, resolved)?;
            }
        }
        StmtKind::Try { body, handler, .. } => {
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, resolved)?;
            }
            for stmt in &mut handler.stmts {
                substitute_stmt(stmt, resolved)?;
            }
        }
        StmtKind::Block(block) => {
            for stmt in &mut block.stmts {
                substitute_stmt(stmt, resolved)?;
            }
        }
        StmtKind::Import { .. } => {}
    }
    Ok(())
}

/// A function's parameters and return, which is where most annotations are.
///
/// The declaration is shared (`Rc`) so a closure can hold the body without
/// copying it, which means rewriting one needs the copy-on-write `Rc` gives —
/// and at this point nothing else holds it, so the copy never happens.
fn substitute_fn(
    decl: &mut std::rc::Rc<crate::syntax::ast::FnDecl>,
    resolved: &HashMap<String, TypeExpr>,
) -> Result<()> {
    let decl = std::rc::Rc::make_mut(decl);
    for param in &mut decl.params {
        if let Some(ty) = &mut param.ty {
            substitute(ty, resolved)?;
        }
    }
    if let Some(ty) = &mut decl.returns {
        substitute(ty, resolved)?;
    }
    for stmt in &mut decl.body.stmts {
        substitute_stmt(stmt, resolved)?;
    }
    Ok(())
}

/// `is` is the one expression holding a type.
fn substitute_expr(expr: &mut Expr, resolved: &HashMap<String, TypeExpr>) -> Result<()> {
    match &mut expr.kind {
        ExprKind::Is { value, ty } => {
            substitute(ty, resolved)?;
            substitute_expr(value, resolved)?;
        }
        ExprKind::Chain(inner) => substitute_expr(inner, resolved)?,
        ExprKind::Coalesce { lhs, rhs } => {
            substitute_expr(lhs, resolved)?;
            substitute_expr(rhs, resolved)?;
        }
        ExprKind::Unary { rhs, .. } => substitute_expr(rhs, resolved)?,
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => {
            substitute_expr(lhs, resolved)?;
            substitute_expr(rhs, resolved)?;
        }
        ExprKind::Call { callee, args } => {
            substitute_expr(callee, resolved)?;
            for arg in args {
                substitute_expr(arg, resolved)?;
            }
        }
        ExprKind::Index { target, index } => {
            substitute_expr(target, resolved)?;
            substitute_expr(index, resolved)?;
        }
        ExprKind::Slice { target, start, end } => {
            substitute_expr(target, resolved)?;
            for bound in [start, end].into_iter().flatten() {
                substitute_expr(bound, resolved)?;
            }
        }
        ExprKind::Field { target, .. } => substitute_expr(target, resolved)?,
        ExprKind::Assign { target, value } => {
            substitute_expr(target, resolved)?;
            substitute_expr(value, resolved)?;
        }
        ExprKind::List(items) => {
            for item in items {
                substitute_expr(item, resolved)?;
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                substitute_expr(key, resolved)?;
                substitute_expr(value, resolved)?;
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil
        | ExprKind::Var(_)
        | ExprKind::Super { .. } => {}
    }
    Ok(())
}

/// Replaces one type in place if it names an alias, then checks it.
///
/// The check is *here* and not in the parser, which is where it used to be and
/// where it was wrong: `dict[UserID, int]` is a perfectly good annotation whose
/// key type cannot be read until `UserID` has been expanded. Anything deciding
/// whether the arguments fit has to run after substitution, so it runs on the
/// way out of it.
fn substitute(ty: &mut TypeExpr, resolved: &HashMap<String, TypeExpr>) -> Result<()> {
    for arg in &mut ty.args {
        substitute(arg, resolved)?;
    }
    if let TypeName::Named(name) = &ty.name
        && let Some(target) = resolved.get(name.as_str())
    {
        *ty = TypeExpr {
            name: target.name.clone(),
            args: target.args.clone(),
            nullable: ty.nullable || target.nullable,
            frozen: ty.frozen || target.frozen,
            span: ty.span,
        };
    }
    check_arguments(ty)
}

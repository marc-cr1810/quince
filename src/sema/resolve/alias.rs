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
use crate::sema::resolve::Prior;
use crate::sema::types::{bound_help, satisfies, substituted};
use crate::syntax::ast::{
    Expr, ExprKind, ParamKind, Stmt, StmtKind, TypeExpr, TypeName, TypeParam, written_params,
};
use crate::syntax::token::Span;

/// Expands every alias in `program`, in place.
///
/// Two passes and not one. Aliases are collected first so that one may name
/// another declared below it, which is the same courtesy the resolver extends to
/// a function calling one further down the file — a rule that held for code and
/// not for types would be arbitrary.
pub(super) fn expand(program: &mut [Stmt], prior: &Prior) -> Result<()> {
    // The prior world first, so a class this entry redeclares takes its own
    // parameter list rather than the one it had a line ago.
    let mut classes = Classes::default();
    for (name, class) in &prior.classes {
        if !class.params.is_empty() {
            classes.params.insert(name.clone(), class.params.clone());
        }
        if let Some(parent) = &class.parent {
            classes.parents.insert(name.clone(), parent.clone());
        }
    }
    parameterised(program, &mut classes);
    // The aliases an earlier entry declared, which this one may name as freely
    // as it names a class an earlier entry declared. They arrive already
    // expanded — that is what they were rewritten to when *their* entry was
    // compiled — so a prior alias captures what its body meant at the time,
    // which is the same thing a prior class's methods do.
    let mut declared: HashMap<String, Alias> = prior.aliases.clone();
    // In declaration order, kept beside the map. Which alias a cycle is
    // reported at has to be the same on every run, and a `HashMap`'s iteration
    // order is not — `alias A = B` / `alias B = A` blamed whichever the hash
    // happened to yield first.
    //
    // This entry's names only, which is also what makes the duplicate refusal
    // in `collect` mean the right thing: two `alias A` in one file is a
    // mistake, and a prompt redeclaring one is the ordinary thing to do there.
    let mut order = Vec::new();
    collect(program, &mut declared, &mut order)?;

    // Each alias is expanded to a form containing no aliases *before* anything
    // is substituted with it, so a use site is rewritten once rather than
    // repeatedly until it settles. This is also where a cycle is found: the
    // resolution of `A` reaches `A`.
    let mut resolved: HashMap<String, Alias> = prior.aliases.clone();
    for name in &order {
        let expanded = resolve_one(name, &declared, &mut Vec::new())?;
        resolved.insert(name.clone(), expanded);
    }

    let ctx = Expansion {
        resolved: &resolved,
        classes: &classes,
    };
    for stmt in program {
        substitute_stmt(stmt, &ctx)?;
    }
    Ok(())
}

/// What this pass knows about the program's classes.
///
/// Two questions, and both are about a *type argument*: how many of them a class
/// takes, and what each has to satisfy. The hierarchy is here because §3.2's
/// bounds are ordinary matching and ordinary matching admits a subclass — so
/// `Box[Dog]` satisfies `T: Animal`, and answering that needs to know what
/// `Dog` extends.
#[derive(Default)]
struct Classes {
    /// The parameters each class declares, for the ones declaring any. An
    /// absent entry means "takes none", which is every class written before
    /// v0.9 and most written after.
    params: HashMap<String, Vec<TypeParam>>,
    /// What each class extends, for the ones extending anything.
    parents: HashMap<String, String>,
}

impl Classes {
    /// Whether `name`'s class descends from `ancestor`, by name.
    ///
    /// Bounded rather than tracked with a visited set: `class A extends B` and
    /// `class B extends A` is refused at run time, not here, so a cycle is a
    /// shape this has to survive rather than diagnose. The same trade
    /// [`Types::declares_method`](crate::sema::infer::Types::declares_method)
    /// makes.
    fn descends(&self, name: &str, ancestor: &str) -> bool {
        let mut current = name;
        for _ in 0..64 {
            if current == ancestor {
                return true;
            }
            match self.parents.get(current) {
                Some(up) => current = up.as_str(),
                None => return false,
            }
        }
        false
    }
}

/// Collects every class's parameters and parent.
///
/// From the whole tree rather than the top level, unlike the aliases above,
/// because a class may be declared inside a function and its parameter list is
/// no less real there. What that costs is that two classes of one name in two
/// scopes share an entry — a pre-existing limit of every by-name table in this
/// pass, and not one generics introduce.
fn parameterised(stmts: &[Stmt], into: &mut Classes) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Class {
                name,
                params,
                parent,
                ..
            } => {
                if !params.is_empty() {
                    into.params.insert(name.clone(), params.clone());
                }
                if let Some(parent) = parent {
                    into.parents.insert(name.clone(), parent.name.clone());
                }
            }
            StmtKind::Fn { decl, .. } => parameterised(&decl.body.stmts, into),
            StmtKind::Block(body) | StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                parameterised(&body.stmts, into)
            }
            StmtKind::If { then, otherwise, .. } => {
                parameterised(&then.stmts, into);
                if let Some(other) = otherwise {
                    parameterised(std::slice::from_ref(other), into);
                }
            }
            StmtKind::Try { body, handler, .. } => {
                parameterised(&body.stmts, into);
                parameterised(&handler.stmts, into);
            }
            _ => {}
        }
    }
}

/// One `alias` declaration: what it abbreviates, and the parameters it takes.
///
/// The parameters are what v0.9 §3.7 adds and are empty for every alias v0.7
/// could write. They stay beside the body all the way through, because the
/// arity of a use site is checked against them at both of the two moments an
/// alias is read — while another alias is being expanded, and at the use site
/// itself.
///
/// Public because a prompt carries them between entries: an alias binds no
/// value, so unlike every other declaration there is nothing in the heap for
/// [`Prior`] to read it back from, and the evaluator keeps the table instead.
/// See [`Interp::aliases`](crate::interp::Interp).
#[derive(Clone)]
pub struct Alias {
    pub params: Vec<TypeParam>,
    pub ty: TypeExpr,
}

/// What a substitution needs to know, threaded as one value.
///
/// Two tables that travel together and are read at the same moment: the second
/// is consulted by [`check_arguments`], which runs on the way out of every
/// substitution, so a signature carrying one and not the other would be a lie
/// about when the check happens.
struct Expansion<'a> {
    /// Each alias, expanded to a form naming no alias.
    resolved: &'a HashMap<String, Alias>,
    /// What the program's classes declare. See [`Classes`].
    classes: &'a Classes,
}

/// Gathers every `alias` declaration, refusing a second one for a name.
///
/// A second one *in this compilation*, which `order` is the record of — `into`
/// may already hold the same name from an earlier prompt entry, and redeclaring
/// there is the ordinary thing to do. The insert overwrites it, which is what
/// makes the later line win.
fn collect(
    stmts: &mut [Stmt],
    into: &mut HashMap<String, Alias>,
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
            params,
            ty,
            ..
        } = &stmt.kind
        {
            if order.iter().any(|seen| seen == name) {
                return Err(redeclared(name, *name_span));
            }
            into.insert(
                name.clone(),
                Alias {
                    params: params.clone(),
                    ty: ty.clone(),
                },
            );
            order.push(name.clone());
        }
    }
    Ok(())
}

/// One alias, expanded until it names no alias — or a cycle, refused.
fn resolve_one(
    name: &str,
    declared: &HashMap<String, Alias>,
    path: &mut Vec<String>,
) -> Result<Alias> {
    if path.iter().any(|seen| seen == name) {
        path.push(name.to_string());
        // Reported at the definition the cycle closes on, which is the one
        // somebody has to change.
        let span = declared
            .get(name)
            .map_or(Span::new(0, 0), |alias| alias.ty.span);
        return Err(cycles(path, span));
    }
    let Some(alias) = declared.get(name) else {
        // Not an alias, so nothing to expand. The name is checked for being a
        // type at all when the annotation is applied.
        unreachable!("only a declared alias is resolved");
    };
    path.push(name.to_string());
    // A parameter of *this* alias is not a name to be expanded — it stands for
    // whatever a use site writes, and is replaced by [`instantiated`] rather
    // than looked up. Nothing has to be done to leave it alone: no alias may be
    // named `T` and also be `T`, since a name declares one thing.
    let expanded = expand_type(&alias.ty, declared, path)?;
    path.pop();
    Ok(Alias {
        params: alias.params.clone(),
        ty: expanded,
    })
}

/// Rewrites a type, replacing any alias it names with what that alias means.
fn expand_type(
    ty: &TypeExpr,
    declared: &HashMap<String, Alias>,
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

    let target = resolve_one(name, declared, path)?;
    // The arguments handed over are the *expanded* ones — `Pair[Score]` with
    // `alias Score = int` binds `T` to `int` and not to a name about to
    // disappear. That is what the walk at the top of this function is for.
    instantiated(name, &target, &TypeExpr { args, ..ty.clone() })
}

/// An alias applied to the arguments a use site wrote.
///
/// The one place §3.7's "a resolution-time substitution introducing no type"
/// actually happens. Three things are combined and each comes from a different
/// side:
///
/// - **The body**, from the declaration, with each parameter replaced.
/// - **The span**, from the use, so a report about `let x: ScoreTable = 1`
///   underlines what was written and not the declaration somewhere above.
/// - **The two qualifiers**, combined rather than replaced: `const Pair[int]?`
///   means what it says whatever the alias was declared as.
///
/// An alias with no parameters takes this path too and comes out of it
/// unchanged, since [`substituted`] with no bindings is a clone — so the v0.7
/// form is not a second case here, it is this one with an empty list.
fn instantiated(name: &str, alias: &Alias, at: &TypeExpr) -> Result<TypeExpr> {
    if alias.params.len() != at.args.len() {
        return Err(wrong_arity(name, alias, at));
    }
    let bindings: Vec<(String, TypeExpr)> = alias
        .params
        .iter()
        .map(|param| param.name.clone())
        .zip(at.args.iter().cloned())
        .collect();
    let target = substituted(&alias.ty, &bindings);
    Ok(TypeExpr {
        name: target.name,
        applied: target.applied,
        args: target.args,
        nullable: at.nullable || target.nullable,
        frozen: at.frozen || target.frozen,
        span: at.span,
    })
}

/// A use of an alias that wrote the wrong number of arguments.
///
/// Deliberately *not* the rule [`check_arguments`] applies to a class, where
/// writing none at all is allowed and means the arguments are unconstrained.
/// A class has a body that a bare `T` can stand unsubstituted in; an alias has
/// only the substitution, and a `Pair` with nothing bound to its `T` would
/// expand to a `tuple[T, T]` naming something that does not exist. So every
/// parameter needs an argument, and there is nothing to infer one from.
fn wrong_arity(name: &str, alias: &Alias, at: &TypeExpr) -> Raised {
    let takes = match alias.params.len() {
        0 => "takes no type arguments".to_string(),
        count => format!(
            "takes {count} type {}",
            if count == 1 { "argument" } else { "arguments" }
        ),
    };
    let written = match at.args.len() {
        0 => "none were".to_string(),
        1 => "1 was".to_string(),
        count => format!("{count} were"),
    };
    let error = refused(format!("`{name}` {takes}, but {written} written"), at.span);
    match alias.params.is_empty() {
        true => error.with_help(format!(
            "`{name}` abbreviates `{}`, which is already complete",
            alias.ty.written()
        )),
        false => error.with_help(format!(
            "`{name}` declares `[{}]`, and an alias is a substitution — every parameter needs \
             an argument, because there is no body for an unwritten one to stand in",
            written_params(&alias.params)
        )),
    }
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

/// Holds each type argument to the bound its parameter declared — v0.9 §3.2.
///
/// Split out of [`check_arguments`] because a pack reaches it by a different
/// route: a list ending in one has no arity to check, so it returns straight
/// here rather than falling through the count comparison. The rules are the
/// same either way, which is the point of there being one function.
fn check_bounds(
    head: &str,
    params: &[TypeParam],
    args: &[TypeExpr],
    classes: &Classes,
) -> Result<()> {
    for (param, arg) in params.iter().zip(args) {
        // §3.3 — a const parameter wants a value and a type parameter wants
        // a type, and an annotation is the one place both are spelled the
        // same way. Which was meant is a fact about the declaration, so the
        // report quotes the declaration.
        let written = arg.written();
        match (&param.kind, &arg.name) {
            // A pack takes whatever is written in its position and every
            // position after it, so there is no one argument for it to
            // disagree with. The count is checked above.
            (ParamKind::Pack, _) => {}
            (ParamKind::Const { ty }, TypeName::Const(value)) => {
                let wanted = match &ty.name {
                    TypeName::Named(name) => name.as_str(),
                    _ => "",
                };
                if value.type_name() != wanted {
                    return Err(refused(
                        format!(
                            "`{head}`\u{2019}s `{}` is `{wanted}`, but `{written}` is `{}`",
                            param.name,
                            value.type_name()
                        ),
                        arg.span,
                    )
                    .with_label(param.span, "declared here"));
                }
            }
            (ParamKind::Const { .. }, _) => {
                return Err(refused(
                    format!(
                        "`{head}`\u{2019}s `{}` takes a value, and `{written}` is a type",
                        param.name
                    ),
                    arg.span,
                )
                .with_label(param.span, "declared here")
                .with_help(format!(
                    "the declaration writes `{}` — the argument in that position is a \
                     literal, or a name declared `const`",
                    param.written()
                )));
            }
            (ParamKind::Type { .. }, TypeName::Const(_)) => {
                return Err(refused(
                    format!(
                        "`{head}`\u{2019}s `{}` takes a type, and `{written}` is a value",
                        param.name
                    ),
                    arg.span,
                )
                .with_label(param.span, "declared here")
                .with_help(format!(
                    "write `const {}: …` in the declaration if it was meant to take one",
                    param.name
                )));
            }
            (ParamKind::Type { .. }, _) => {}
        }
        let Some(bound) = param.bound() else {
            continue;
        };
        if satisfies(bound, arg, &|name, ancestor| classes.descends(name, ancestor)) {
            continue;
        }
        return Err(refused(
            format!(
                "`{}` does not satisfy bound `{}`",
                arg.written(),
                bound.written()
            ),
            arg.span,
        )
        .with_label(param.span, "declared here")
        .with_help(bound_help(head, &param.name, bound)));
    }
    Ok(())
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
/// v0.7 admitted only the two containers the language has, and said so in the
/// help. v0.9 adds the classes the program declared, read off their parameter
/// lists by [`parameterised`] — which is why the arity here is looked up rather
/// than matched: a generic class is not a third case, it is the same case with
/// its count written somewhere else.
///
/// A head that is neither is still left alone when it takes no arguments. It may
/// not be a type at all, and *that* is checked where the annotation is applied.
fn check_arguments(ty: &TypeExpr, classes: &Classes) -> Result<()> {
    let TypeName::Named(head) = &ty.name else {
        // `any` takes no arguments and the parser gives it none.
        return Ok(());
    };
    let (head, args, span) = (head.as_str(), ty.args.as_slice(), ty.span);
    let arity = match head {
        "list" => 1..=1,
        "dict" => 1..=2,
        // The one head with no arity to check: `tuple[A, B]` and
        // `tuple[A, B, C]` are both good annotations, because a tuple's arity is
        // written rather than declared. §3.5, and it is why this is a `return`
        // rather than a wider range — there is no upper bound to state.
        "tuple" => return Ok(()),
        _ if classes.params.contains_key(head) => {
            let params = &classes.params[head];
            let count = params.len();
            // A pack ends the list and takes however many arguments are left,
            // so the count becomes a minimum with no upper bound to state —
            // the same shape `tuple` has, arrived at from a declaration rather
            // than from the language. §3.4.
            match params.last().is_some_and(TypeParam::is_pack) {
                true => {
                    if !args.is_empty() && args.len() < count - 1 {
                        return Err(refused(
                            format!(
                                "`{head}` takes at least {} type {}, but {} {} written",
                                count - 1,
                                if count == 2 { "argument" } else { "arguments" },
                                args.len(),
                                if args.len() == 1 { "was" } else { "were" }
                            ),
                            span,
                        )
                        .with_help(format!(
                            "`{head}` declares `[{}]`, and a pack takes whatever is left \
                             over — the parameters in front of it still each need one",
                            written_params(params)
                        )));
                    }
                    return check_bounds(head, params, args, classes);
                }
                false => count..=count,
            }
        }
        // Not a container and not a class that declared parameters, so it takes
        // no arguments — and a program that wrote some has said something the
        // language cannot read.
        _ if !args.is_empty() => {
            return Err(refused(format!("`{head}` takes no type arguments"), span).with_help(
                format!(
                    "`list` and `dict` are parameterised, and so is a class declaring a \
                     parameter list — write `class {head}[T]` if that is what `{head}` should be"
                ),
            ));
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
        )
        .with_help(match head {
            "list" => "a list has one element type — `list[int]`".to_string(),
            "dict" => "a dict takes a key type and optionally a value type — \
                       `dict[string, int]`, or `dict[string]` to leave the values \
                       unconstrained"
                .to_string(),
            // A class says its own arity, so the help quotes the declaration
            // back rather than describing it. Writing none at all is still
            // allowed and is not this report — an unwritten argument is
            // unconstrained, §3.1.
            _ => format!(
                "`{head}` declares {} type {}, and a use writes all of them or none",
                arity.start(),
                match arity.start() {
                    1 => "parameter",
                    _ => "parameters",
                }
            ),
        }));
    }

    // §3.2's bounds, checked where the argument is written — which is the whole
    // point of them being checked at resolution: the declaration is right and
    // is somewhere else, so the caret belongs on the word the program chose.
    let declared = classes.params.get(head).map_or(&[][..], Vec::as_slice);
    check_bounds(head, declared, args, classes)?;

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
fn substitute_stmt(stmt: &mut Stmt, ctx: &Expansion<'_>) -> Result<()> {
    match &mut stmt.kind {
        StmtKind::Alias { ty, .. } => substitute(ty, ctx)?,
        StmtKind::Destructure { value, .. } => substitute_expr(value, ctx)?,
        StmtKind::Let { ty, value, .. } => {
            if let Some(ty) = ty {
                substitute(ty, ctx)?;
            }
            substitute_expr(value, ctx)?;
        }
        StmtKind::Fn { decl, .. } => substitute_fn(decl, ctx)?,
        StmtKind::Class { methods, fields, .. } => {
            for decl in methods {
                substitute_fn(decl, ctx)?;
            }
            for field in fields {
                if let Some(ty) = &mut field.ty {
                    substitute(ty, ctx)?;
                }
                substitute_expr(&mut field.value, ctx)?;
            }
        }
        StmtKind::Extend {
            target,
            target_span,
            methods,
            constraint,
        } => {
            // An alias abbreviates a type and declares none, so there is
            // nothing under the name for an extension to add to. Caught here
            // because this is the only pass that knows the name is an alias:
            // by the evaluator it has been substituted out of every use, and
            // `extend` would report it as an undefined variable.
            if ctx.resolved.contains_key(target.name.as_str()) {
                return Err(refused(
                    format!("`{}` is an alias, so it cannot be extended", target.name),
                    *target_span,
                )
                .with_help(format!(
                    "`{}` abbreviates a type and introduces none — extend the type it \
                     abbreviates, and every use of the alias reaches the method",
                    target.name
                )));
            }
            // §3.6's "the resolver still refuses an `extend list[int]` block
            // whose target is not a real instantiation". Nothing extra is
            // needed to say it: `substitute` ends in [`check_arguments`], which
            // is the same arity, bound, and dict-key check every annotation in
            // the program goes through.
            if let Some(constraint) = constraint {
                substitute(constraint, ctx)?;
            }
            for decl in methods {
                substitute_fn(decl, ctx)?;
            }
        }
        StmtKind::Expr(expr) | StmtKind::Throw(expr) => substitute_expr(expr, ctx)?,
        StmtKind::Return(value) => {
            if let Some(value) = value {
                substitute_expr(value, ctx)?;
            }
        }
        StmtKind::If { cond, then, otherwise } => {
            substitute_expr(cond, ctx)?;
            for stmt in &mut then.stmts {
                substitute_stmt(stmt, ctx)?;
            }
            if let Some(other) = otherwise {
                substitute_stmt(other, ctx)?;
            }
        }
        StmtKind::While { cond, body } => {
            substitute_expr(cond, ctx)?;
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, ctx)?;
            }
        }
        StmtKind::For { iter, body, .. } => {
            substitute_expr(iter, ctx)?;
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, ctx)?;
            }
        }
        StmtKind::Try { body, handler, .. } => {
            for stmt in &mut body.stmts {
                substitute_stmt(stmt, ctx)?;
            }
            for stmt in &mut handler.stmts {
                substitute_stmt(stmt, ctx)?;
            }
        }
        StmtKind::Block(block) => {
            for stmt in &mut block.stmts {
                substitute_stmt(stmt, ctx)?;
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
    ctx: &Expansion<'_>,
) -> Result<()> {
    let decl = std::rc::Rc::make_mut(decl);
    for param in &mut decl.params {
        if let Some(ty) = &mut param.ty {
            substitute(ty, ctx)?;
        }
    }
    if let Some(ty) = &mut decl.returns {
        substitute(ty, ctx)?;
    }
    for stmt in &mut decl.body.stmts {
        substitute_stmt(stmt, ctx)?;
    }
    Ok(())
}

/// `is` is the one expression holding a type.
fn substitute_expr(expr: &mut Expr, ctx: &Expansion<'_>) -> Result<()> {
    match &mut expr.kind {
        ExprKind::Is { value, ty } => {
            substitute(ty, ctx)?;
            substitute_expr(value, ctx)?;
        }
        ExprKind::Chain(inner) => substitute_expr(inner, ctx)?,
        ExprKind::Coalesce { lhs, rhs } => {
            substitute_expr(lhs, ctx)?;
            substitute_expr(rhs, ctx)?;
        }
        ExprKind::Unary { rhs, .. } => substitute_expr(rhs, ctx)?,
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => {
            substitute_expr(lhs, ctx)?;
            substitute_expr(rhs, ctx)?;
        }
        ExprKind::Call { callee, args } => {
            substitute_expr(callee, ctx)?;
            for arg in args {
                substitute_expr(&mut arg.value, ctx)?;
            }
        }
        ExprKind::Index { target, index } => {
            substitute_expr(target, ctx)?;
            substitute_expr(index, ctx)?;
        }
        ExprKind::TypeArgs { target, args } => {
            substitute_expr(target, ctx)?;
            for arg in args {
                substitute_expr(arg, ctx)?;
            }
        }
        ExprKind::Slice { target, start, end } => {
            substitute_expr(target, ctx)?;
            for bound in [start, end].into_iter().flatten() {
                substitute_expr(bound, ctx)?;
            }
        }
        ExprKind::Field { target, .. } => substitute_expr(target, ctx)?,
        ExprKind::Assign { target, value }
        | ExprKind::AssignOp { target, value, .. }
        | ExprKind::AssignShort { target, value, .. } => {
            substitute_expr(target, ctx)?;
            substitute_expr(value, ctx)?;
        }
        ExprKind::List(items) | ExprKind::Tuple(items) => {
            for item in items {
                substitute_expr(item, ctx)?;
            }
        }
        ExprKind::Dict(pairs) => {
            for (key, value) in pairs {
                substitute_expr(key, ctx)?;
                substitute_expr(value, ctx)?;
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
fn substitute(ty: &mut TypeExpr, ctx: &Expansion<'_>) -> Result<()> {
    for arg in &mut ty.args {
        substitute(arg, ctx)?;
    }
    if let TypeName::Named(name) = &ty.name
        && let Some(target) = ctx.resolved.get(name.as_str())
    {
        // The same [`instantiated`] an alias naming another alias goes through,
        // so `Pair[float]` means one thing whether it is written in an
        // annotation or inside a second alias's body. Its arity refusal is the
        // one that fires here.
        *ty = instantiated(name, target, ty)?;
    }
    check_arguments(ty, ctx.classes)
}

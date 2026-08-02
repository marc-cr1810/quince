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
//!
//! # What the later milestones need from this
//!
//! [`against`] dispatches on the *shape* of a container annotation: `list[T]`
//! is one argument applied to every element, `dict[K, V]` is two applied
//! positionally, and `dict[K]` is the shorthand where the second is elided.
//! That is a per-type rule and not a general one, which is deliberate — the
//! three containers v0.7 has are the three shapes it knows.
//!
//! v0.9's `tuple[A, B]` is a fourth and fits the same frame: fixed arity,
//! positional, one argument per element by index. Its check is another arm.
//!
//! **A variadic pack is not.** `tuple[T...]` has no arity to zip against, so
//! the comparison stops being "pair the arguments up" and becomes "match the
//! declared shape against the actual ones" — one argument may stand for any
//! number of elements, and which ones it covers depends on what surrounds it.
//! That is a change to [`fits`], which currently refuses to answer when the
//! two argument lists differ in length. It answers `true` there, so a pack
//! arriving before the matcher does is silence rather than a wrong report.

use crate::error::{ErrorKind, QuinceError, Raised};
use crate::sema::infer::Types;
use crate::sema::types::{Type, stated};
use crate::runtime::value::Native;
use crate::sema::types::builtin_method;
use crate::syntax::ast::{
    BindKind, Block, Expr, ExprKind, FieldDecl, Param, Stmt, StmtKind, TypeExpr, TypeName,
};

/// Every type mistake decidable from the source, in source order.
///
/// A `Vec` rather than a `Result` because an editor wants all of them: stopping
/// at the first is right for a compiler that cannot continue and wrong for a
/// document someone is still typing.
pub fn check(program: &[Stmt], types: &Types) -> Vec<Raised> {
    let mut found = Vec::new();
    let mut bound = Bindings::default();
    // The file's own scope. Without it nothing is recorded at all, since a
    // declaration goes into the innermost scope and there was none.
    bound.push();
    stmts(program, types, &mut bound, &mut found);
    bound.pop();
    found.sort_by_key(|err| err.span.start);
    found
}

/// Which names are bound with which word, in scope.
///
/// A stack rather than a map, so a `let k` inside a block does not inherit the
/// `final k` outside it — shadowing has to work or this reports mistakes that
/// are not there, and one false squiggle costs more than ten missing ones.
///
/// The resolver already refuses a reassigned `final` *local*, so nothing here
/// is new for those. What it cannot see is a global: each REPL entry is its own
/// compilation, so `final k = 1` and `k = 2` typed a line apart are two
/// programs and the second has never heard of the first. Inside one file they
/// are one program, which is exactly the case an editor has in hand — and a
/// warning is the right shape for something true of a file and not of a
/// session.
#[derive(Default)]
struct Bindings {
    scopes: Vec<Vec<(String, BindKind, Option<TypeExpr>)>>,
}

impl Bindings {
    fn push(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, bind: BindKind, ty: Option<TypeExpr>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push((name.to_string(), bind, ty));
        }
    }

    /// The word `name` was bound with, innermost first.
    fn kind(&self, name: &str) -> Option<BindKind> {
        self.found(name).map(|(bind, _)| bind)
    }

    /// What the declaration annotated `name` as, if it annotated it.
    fn annotation(&self, name: &str) -> Option<TypeExpr> {
        self.found(name).and_then(|(_, ty)| ty)
    }

    fn found(&self, name: &str) -> Option<(BindKind, Option<TypeExpr>)> {
        self.scopes.iter().rev().find_map(|scope| {
            scope
                .iter()
                .rev()
                .find(|(bound, ..)| bound == name)
                .map(|(_, bind, ty)| (*bind, ty.clone()))
        })
    }
}

fn stmts(stmts: &[Stmt], types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    for stmt in stmts {
        one(stmt, types, bound, found);
    }
}

fn one(stmt: &Stmt, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    match &stmt.kind {
        StmtKind::Let {
            name, value, ty, bind, ..
        } => {
            if let Some(ty) = ty {
                against(ty, value, &format!("`{name}`"), types, found);
            }
            expression(value, types, bound, found);
            bound.declare(name, *bind, ty.clone());
        }
        StmtKind::Class { methods, fields, .. } => {
            for field in fields {
                check_field(field, types, found);
            }
            for decl in methods {
                body(decl, types, bound, found);
            }
        }
        StmtKind::Fn { decl, .. } => body(decl, types, bound, found),
        StmtKind::Extend { methods, .. } => {
            for decl in methods {
                body(decl, types, bound, found);
            }
        }
        StmtKind::If { cond, then, otherwise } => {
            expression(cond, types, bound, found);
            block(then, types, bound, found);
            if let Some(other) = otherwise {
                one(other, types, bound, found);
            }
        }
        StmtKind::While { cond, body: loop_body } => {
            expression(cond, types, bound, found);
            block(loop_body, types, bound, found);
        }
        StmtKind::For { iter, body: loop_body, .. } => {
            expression(iter, types, bound, found);
            block(loop_body, types, bound, found);
        }
        StmtKind::Try { body: attempted, handler, .. } => {
            block(attempted, types, bound, found);
            block(handler, types, bound, found);
        }
        StmtKind::Block(inner) => block(inner, types, bound, found),
        StmtKind::Expr(expr) | StmtKind::Throw(expr) => expression(expr, types, bound, found),
        StmtKind::Return(value) => {
            if let Some(value) = value {
                expression(value, types, bound, found);
            }
        }
        StmtKind::Alias { .. } | StmtKind::Import { .. } => {}
    }
}

fn block(block: &Block, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    bound.push();
    stmts(&block.stmts, types, bound, found);
    bound.pop();
}

/// A function body, with its parameters bound to the words they were declared
/// with — so `fn f(const n) { n = 2 }` is caught where `const n = 1; n = 2` is.
fn body(decl: &std::rc::Rc<crate::syntax::ast::FnDecl>, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    bound.push();
    for param in &decl.params {
        bound.declare(&param.name, param.bind, param.ty.clone());
    }
    stmts(&decl.body.stmts, types, bound, found);
    bound.pop();
}

fn check_field(field: &FieldDecl, types: &Types, found: &mut Vec<Raised>) {
    let Some(ty) = &field.ty else {
        return;
    };
    against(ty, &field.value, &format!("`{}`", field.name), types, found);
}

/// Walks an expression for the mistakes decidable inside one.
///
/// Only the containers, and only their mutations. A `list[int]` that the pass
/// can name has an element type, so `xs.push("a")` is as visible as
/// `let xs: list[int] = ["a"]` was — and it is the form people actually write,
/// since a list is usually built empty and filled.
///
/// What is *not* here is the rest of what a call could be checked for: an
/// argument against a parameter's annotation, a native's declared types. Those
/// need the pass to keep declarations it currently discards for a plain `fn`,
/// which is a change to what it records rather than to what this asks.
fn expression(expr: &Expr, types: &Types, bound: &Bindings, found: &mut Vec<Raised>) {
    for child in parts(expr) {
        expression(child, types, bound, found);
    }

    // Writing to a name bound once. The resolver refuses this for a local and
    // cannot see a global, so within one file this is what answers for one.
    if let ExprKind::Assign { target, value } = &expr.kind
        && let ExprKind::Var(var) = &target.kind
    {
        // Against the annotation the *declaration* carried, since that is what
        // constrains the name rather than the first value bound to it.
        if let Some(ty) = bound.annotation(&var.name) {
            against(&ty, value, &format!("`{}`", var.name), types, found);
        }
    }
    if let ExprKind::Assign { target, .. } = &expr.kind
        && let ExprKind::Var(var) = &target.kind
        && let Some(bind) = bound.kind(&var.name)
        && !bind.mutable()
    {
        found.push(refusal(
            format!("cannot reassign `{}`", var.name),
            format!(
                "it is bound with `{}`, which binds a name once — declare it with `let` to \
                 reassign it",
                bind.word()
            ),
            target.span,
        ));
    }

    // Reaching a member the declaration put out of reach. `may_offer` is the
    // same rule the completion list follows, so what the editor offers and what
    // it squiggles cannot disagree.
    if let ExprKind::Field { target, name, .. } = &expr.kind {
        visibility(target, name, expr.span, types, found);
    }

    let ExprKind::Call { callee, args } = &expr.kind else {
        // `xs[i] = v` is the other way into a typed container.
        if let ExprKind::Assign { target, value } = &expr.kind
            && let ExprKind::Index { target: collection, .. } = &target.kind
        {
            let held = receiver_type(collection, types);
            check_element(&held, "list", 0, value, "the item", types, found);
            check_element(&held, "dict", 1, value, "the value", types, found);
        }
        return;
    };
    // A call to a `fn` the program declared, against the parameters it named.
    if let ExprKind::Var(var) = &callee.kind {
        if let Some(decl) = types.function(&var.name) {
            arguments(&decl.params, args, types, found);
        } else if let Some(native) = types.native(&var.name) {
            native_arguments(native, args, types, found);
        }
    }

    let ExprKind::Field { target, name, .. } = &callee.kind else {
        return;
    };

    // A method call: the program's own, or the library's.
    let receiver = receiver_type(target, types);
    if let Some(class) = receiver.class_name() {
        if let Some(decl) = types.method_of(class, name) {
            // The receiver is `params[0]` and nobody writes it.
            arguments(decl.params.get(1..).unwrap_or(&[]), args, types, found);
        } else if let Some(native) = builtin_method(class, name) {
            native_arguments(native, args, types, found);
        }
    }

    // Mutating what `const` froze. `final` is the other axis and is untouched:
    // it binds the name once and leaves the object alone, so a `final` list
    // still grows.
    if MUTATORS.contains(&name.as_str())
        && let ExprKind::Var(var) = &target.kind
        && bound.kind(&var.name).is_some_and(|bind| bind.freezes())
    {
        found.push(refusal(
            format!("cannot modify `{}`", var.name),
            format!(
                "`{}` is `const`, which freezes the value deeply — bind it with `final` if only \
                 the name should be fixed",
                var.name
            ),
            expr.span,
        ));
    }

    // The mutating methods, by the argument each puts into the container.
    // One today; `insert` and the rest arrive with the collections v0.10 adds.
    let held = receiver_type(target, types);
    if let ("push", Some(item)) = (name.as_str(), args.first()) {
        check_element(&held, "list", 0, item, "the item", types, found);
    }
}

/// Refuses each argument the declaration has an annotation for.
fn arguments(params: &[Param], args: &[Expr], types: &Types, found: &mut Vec<Raised>) {
    // A mismatched count is an arity error, reported when the call runs. Saying
    // something about the arguments of a call that cannot happen would be two
    // complaints about one mistake.
    if params.len() != args.len() {
        return;
    }
    for (param, arg) in params.iter().zip(args) {
        if let Some(ty) = &param.ty {
            against(ty, arg, &format!("`{}`", param.name), types, found);
        }
    }
}

/// The same for a builtin, whose parameters name a *set* of types.
fn native_arguments(native: &Native, args: &[Expr], types: &Types, found: &mut Vec<Raised>) {
    if native.params.len() != args.len() {
        return;
    }
    for (param, arg) in native.params.iter().zip(args) {
        if param.accepts.is_empty() {
            continue;
        }
        let held = types.of_expr(arg.span.start);
        let Some(actual) = held.class_name() else {
            continue;
        };
        // The same one-sidedness as everywhere else: an int passed where a
        // float is taken is fine, and so is anything the pass cannot name.
        let admitted = param.accepts.iter().any(|builtin| {
            builtin.name() == actual || (builtin.name() == "float" && actual == "int")
        });
        if admitted {
            continue;
        }
        found.push(refusal(
            format!("`{}` is {}, but this is {}", param.name, param.written(), an(actual)),
            format!("`{}` takes {} there", native.name, param.written()),
            arg.span,
        ));
    }
}

/// Refuses a member reached from outside the visibility it was declared with.
fn visibility(
    target: &Expr,
    name: &str,
    span: crate::syntax::token::Span,
    types: &Types,
    found: &mut Vec<Raised>,
) {
    let Some(class) = receiver_type(target, types).class_name().map(str::to_string) else {
        return;
    };
    let Some((reach, owner)) = types.reach_of(&class, name) else {
        return;
    };
    if types.may_offer(reach, &class, types.class_at(span.start)) {
        return;
    }
    let word = reach.word().unwrap_or("private");
    found.push(refusal(
        format!("`{name}` is {word} to `{owner}`"),
        match reach.closes_subclass() {
            true => format!("only methods declared inside `{owner}` may reach it"),
            false => format!("only methods of `{owner}` and of the classes extending it may reach it"),
        },
        span,
    ));
}

/// What a receiver holds.
///
/// By name where the receiver is one, because [`Types::of_expr`] is keyed by
/// where an expression *starts* and a receiver shares its first byte with
/// everything wrapped around it — `xs`, `xs[0]`, and `xs.push` all begin at the
/// same offset, and the outer one is the one recorded. Asking for the binding
/// sidesteps the collision entirely.
fn receiver_type(expr: &Expr, types: &Types) -> Type {
    match &expr.kind {
        ExprKind::Var(var) => types.of_name(&var.name, expr.span.start),
        // A literal is its own answer, and has to be given directly for the
        // same reason a name does: `"a,b"` and `"a,b".split` begin at the same
        // byte, so the recorded type is the whole call's.
        ExprKind::Str(_) => Type::class("string"),
        ExprKind::Int(_) => Type::class("int"),
        ExprKind::Float(_) => Type::class("float"),
        ExprKind::Bool(_) => Type::class("bool"),
        ExprKind::List(_) => Type::class("list"),
        ExprKind::Dict(_) => Type::class("dict"),
        _ => types.of_expr(expr.span.start),
    }
}

/// The library methods that change what they are called on.
///
/// A list rather than a rule, because there is no rule: a native says what it
/// does in Rust, and nothing in its declaration marks it as mutating. Short
/// enough to keep by hand while that is true, and `every_mutator_is_a_method`
/// is what stops a name here being one nothing has.
const MUTATORS: &[&str] = &["push", "sort", "reverse", "remove"];

/// Refuses a value going into a container the pass has named the contents of.
fn check_element(
    held: &Type,
    container: &str,
    slot: usize,
    value: &Expr,
    what: &str,
    types: &Types,
    found: &mut Vec<Raised>,
) {
    if held.class_name() != Some(container) {
        return;
    }
    let Some(wanted) = held.args().get(slot) else {
        return;
    };
    let actual = types.of_expr(value.span.start);
    if fits(types, wanted, &actual) {
        return;
    }
    found.push(refusal(
        format!("{what} is `{wanted}`, but this is {}", an(&actual.to_string())),
        format!(
            "either give it {}, or declare the container to admit {}",
            an_type(wanted),
            an(&actual.to_string())
        ),
        value.span,
    ));
}

/// `a` or `an`, for a type quoted back.
fn an_type(ty: &Type) -> String {
    let written = ty.to_string();
    match written.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => format!("an `{written}`"),
        false => format!("a `{written}`"),
    }
}

/// One level of an expression's children.
fn parts(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Unary { rhs, .. } => vec![rhs],
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Logical { lhs, rhs, .. }
        | ExprKind::Coalesce { lhs, rhs } => vec![lhs, rhs],
        ExprKind::Call { callee, args } => {
            let mut parts = vec![callee.as_ref()];
            parts.extend(args);
            parts
        }
        ExprKind::Index { target, index } => vec![target, index],
        ExprKind::Field { target, .. } | ExprKind::Chain(target) => vec![target],
        ExprKind::Is { value, .. } => vec![value],
        ExprKind::Assign { target, value } => vec![target, value],
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Dict(pairs) => pairs.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::Slice { target, start, end } => {
            let mut parts = vec![target.as_ref()];
            parts.extend(start.as_deref());
            parts.extend(end.as_deref());
            parts
        }
        _ => Vec::new(),
    }
}

/// Checks an initializer against the annotation it is being bound to.
///
/// A *literal* is checked element by element rather than as a whole, and that
/// distinction is the whole point. Asking what `[1, "a"]`'s elements agree on
/// answers "nothing", which says only that the pass cannot name the element
/// type — while the question that matters is whether each element fits the
/// annotation, and `"a"` plainly does not. Joining first threw that away.
///
/// It is also what lets the report name the element the way the run-time check
/// does, since both are now looking at the same thing.
fn against(
    ty: &TypeExpr,
    value: &Expr,
    what: &str,
    types: &Types,
    found: &mut Vec<Raised>,
) {
    match (&value.kind, ty.args.as_slice()) {
        // `list[T]` over a list literal.
        (ExprKind::List(items), [element]) if names(ty, "list") => {
            for (index, item) in items.iter().enumerate() {
                against(element, item, &format!("item {index}"), types, found);
            }
            return;
        }
        // `dict[K, V]` over a dict literal. The `dict[K]` shorthand leaves
        // values unconstrained, so only the keys are asked about there.
        (ExprKind::Dict(pairs), [key] | [key, _]) if names(ty, "dict") => {
            let value_ty = ty.args.get(1);
            for (k, v) in pairs {
                against(key, k, "the key", types, found);
                if let Some(value_ty) = value_ty {
                    against(value_ty, v, "the value", types, found);
                }
            }
            return;
        }
        _ => {}
    }

    let held = types.of_expr(value.span.start);
    if let Some(err) = disagrees(ty, &held, what, types, value.span) {
        found.push(err);
    }
}

/// Whether an annotation names a particular type.
fn names(ty: &TypeExpr, name: &str) -> bool {
    matches!(&ty.name, TypeName::Named(written) if written == name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mutator_is_a_method_something_actually_has() {
        // The list is maintained by hand, because nothing in a native's
        // declaration says whether it mutates — so the one thing that can be
        // checked is that each name is a method at all. A typo here is a
        // `const` that quietly stops being enforced by the editor.
        let mut known: Vec<&str> = Vec::new();
        for builtin in crate::runtime::class::BUILTINS {
            known.extend(builtin.seed().methods.iter().map(|(name, _)| *name));
        }
        for mutator in MUTATORS {
            assert!(
                known.contains(mutator),
                "`{mutator}` is listed as mutating and is not a method of any builtin"
            );
        }
    }
}

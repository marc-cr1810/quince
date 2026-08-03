//! Mistakes the pass can see without running the program.
//!
//! §5 puts the enforcement of an annotation at run time and this does not move
//! it: the language still refuses `let x: int = "s"` when the binding executes,
//! with the same message from the same function. What this adds is the editor
//! saying so first.
//!
//! **What it reports are errors, not warnings.** Every rule below is one-sided
//! — nothing is reported unless the pass knows both types and they definitely
//! disagree, and `Unknown` on either side reports nothing. So a report here is
//! not a suspicion: the line will fail when it runs, with the same sentence.
//! Drawing it in the colour reserved for "this might be a problem" would
//! undersell it, and teach a reader to skim past the ones that are certain.
//!
//! What *is* approximate is the coverage, not the verdict — which cases this
//! can see, not whether a case it saw is real. That distinction is why the same
//! reasoning does not license a check in the resolver: a *refusal* firing only
//! where inference happened to succeed would be a rule nobody could state,
//! while an editor that marks what it is sure of and stays quiet about the rest
//! is just an editor doing what it can.
//!
//! The one thing this cannot know is whether the line runs at all. `let x: int
//! = "s"` inside a branch nothing reaches never fails — and is still wrong, in
//! the way every static type error in every language is wrong.
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
use crate::sema::types::{Type, builtin_ancestor, stated};
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

fn is_builtin_type(name: &str) -> bool {
    crate::runtime::class::BUILTINS
        .iter()
        .any(|b| b.name() == name)
}

fn is_unhashable_type(ty: &TypeExpr, types: &Types) -> bool {
    let TypeName::Named(name) = &ty.name else { return false; };
    match name.as_str() {
        "list" | "dict" => true,
        "int" | "float" | "string" | "bool" | "nil" | "any" => false,
        user_class => {
            if let Some(info) = types.classes.get(user_class) {
                !info.methods.contains_key("eq") && !info.methods.contains_key("hash")
            } else {
                !is_builtin_type(user_class)
            }
        }
    }
}

fn is_unhashable_expr(expr: &Expr, types: &Types) -> bool {
    match &expr.kind {
        ExprKind::List(_) | ExprKind::Dict(_) => true,
        _ => {
            let ty = types.of_expr(expr.span.start);
            if let Some(name) = ty.class_name() {
                matches!(name, "list" | "dict")
            } else {
                false
            }
        }
    }
}

fn root_var(expr: &Expr) -> Option<&crate::syntax::ast::Var> {
    match &expr.kind {
        ExprKind::Var(v) => Some(v),
        ExprKind::Index { target, .. } | ExprKind::Field { target, .. } => root_var(target),
        _ => None,
    }
}

fn has_return_value(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(Some(_)) => true,
        StmtKind::If { then, otherwise, .. } => {
            has_return_value(&then.stmts)
                || otherwise.as_ref().map_or(false, |o| match &o.kind {
                    StmtKind::Expr(_) => false,
                    _ => has_return_value(&[*(o.clone())]),
                })
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => has_return_value(&body.stmts),
        StmtKind::Block(b) => has_return_value(&b.stmts),
        StmtKind::Try { body, handler, .. } => {
            has_return_value(&body.stmts) || has_return_value(&handler.stmts)
        }
        _ => false,
    })
}

fn check_type_expr(ty: &TypeExpr, types: &Types, found: &mut Vec<Raised>) {
    for arg in &ty.args {
        check_type_expr(arg, types, found);
    }
    let TypeName::Named(name) = &ty.name else {
        return;
    };
    let is_builtin = is_builtin_type(name) || matches!(name.as_str(), "function" | "any" | "nil" | "_");
    if !is_builtin && !types.declares_class(name) {
        found.push(refusal(
            format!("unknown type `{name}`"),
            format!("no type or class by the name `{name}` exists in scope — check spelling or declare the class"),
            ty.span,
        ));
    }
    match name.as_str() {
        "int" | "float" | "string" | "bool" | "nil" | "any" | "function" => {
            if !ty.args.is_empty() {
                found.push(refusal(
                    format!("`{name}` takes no type arguments, got {}", ty.args.len()),
                    format!("`{name}` is not a generic type"),
                    ty.span,
                ));
            }
        }
        _ => {}
    }
}

fn one(stmt: &Stmt, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    match &stmt.kind {
        StmtKind::Let {
            name, value, ty, bind, ..
        } => {
            if let Some(ty) = ty {
                check_type_expr(ty, types, found);
                against(ty, value, &format!("`{name}`"), types, found);
            }
            expression(value, types, bound, found);
            bound.declare(name, *bind, ty.clone());
        }
        StmtKind::Class { name, parent, parent_span, fields, methods, .. } => {
            if let Some(parent) = parent {
                let pspan = parent_span.unwrap_or(stmt.span);
                if parent.name == *name {
                    found.push(refusal(
                        format!("class `{name}` cannot inherit from itself"),
                        "a class cannot extend itself".to_string(),
                        pspan,
                    ));
                } else if parent.name == "function" {
                    found.push(refusal(
                        "cannot inherit from builtin type `function`".to_string(),
                        "`function` is not a class and cannot be extended".to_string(),
                        pspan,
                    ));
                } else if let Some(info) = types.classes.get(&parent.name) {
                    if info.openness.closes_inheritance() {
                        found.push(refusal(
                            format!("cannot inherit from final class `{}`", parent.name),
                            format!("`{}` was declared as final or sealed and cannot be inherited from", parent.name),
                            pspan,
                        ));
                    }
                } else if bound.kind(&parent.name).is_some() {
                    found.push(refusal(
                        format!("cannot inherit from variable `{}`", parent.name),
                        "a superclass must be a class name".to_string(),
                        pspan,
                    ));
                }
            }
            for field in fields {
                check_field(field, types, found);
            }
            for decl in methods {
                body(decl, types, bound, found);
            }
        }
        StmtKind::Fn { decl, .. } => body(decl, types, bound, found),
        StmtKind::Import { module, names, .. } => {
            if let crate::syntax::ast::ImportNames::Names(names) = names {
                if let Some(std_mod) = crate::builtins::stdlib::module_named(module) {
                    for name in names {
                        if !std_mod.members.iter().any(|(m, _)| *m == name.name) {
                            found.push(refusal(
                                format!("module `{module}` has no member `{}`", name.name),
                                format!("module `{module}` exports: {}", std_mod.members.iter().map(|(m,_)| *m).collect::<Vec<_>>().join(", ")),
                                name.span,
                            ));
                        }
                    }
                }
            }
        }
        StmtKind::Extend { target, target_span, methods, .. } => {
            let is_known = is_builtin_type(&target.name) || types.declares_class(&target.name);
            if !is_known {
                found.push(refusal(
                    format!("`{}` is not a class or type", target.name),
                    "`extend` requires a class or builtin type name".to_string(),
                    *target_span,
                ));
            } else {
                if let Some(info) = types.classes.get(&target.name) {
                    if info.openness.closes_extension() {
                        found.push(refusal(
                            format!("cannot extend complete class `{}`", target.name),
                            format!("`{}` was declared as complete or sealed and cannot be extended", target.name),
                            *target_span,
                        ));
                    }
                }

                let builtin_opt = crate::runtime::class::BUILTINS.iter().find(|b| b.name() == target.name);

                for method in methods {
                    let is_shadowing_user = types.classes.get(&target.name)
                        .and_then(|info| info.methods.get(&method.name))
                        .is_some_and(|existing| existing.name_span != method.name_span);
                    let is_shadowing_builtin = builtin_ancestor(&types.classes, &target.name, &method.name).is_some()
                        || (is_builtin_type(&target.name) && builtin_method(&target.name, &method.name).is_some());

                    if let Some(op) = method.op {
                        let natively_supported = builtin_opt.is_some_and(|b| b.natively_supports_op(op));
                        if natively_supported || is_shadowing_user || is_shadowing_builtin {
                            found.push(refusal(
                                format!("`{}` natively supports `op {}` and cannot be overridden by an extension", target.name, op.name()),
                                "an extension may only add ops that the type does not already natively support".to_string(),
                                method.name_span,
                            ));
                        }
                    } else if is_shadowing_user || is_shadowing_builtin {
                        found.push(refusal(
                            format!("`{}` is already a method of `{}`", method.name, target.name),
                            "an extension adds methods to a type and cannot override existing ones".to_string(),
                            method.name_span,
                        ));
                    }
                }
            }
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
        StmtKind::Throw(expr) => {
            expression(expr, types, bound, found);
            let held = receiver_type(expr, types);
            if let Some(cname) = held.class_name() {
                if matches!(cname, "int" | "float" | "string" | "bool" | "nil" | "list" | "dict" | "function") {
                    found.push(refusal(
                        format!("cannot throw {}", an(cname)),
                        "only instances of `Error` or its subclasses may be thrown".to_string(),
                        expr.span,
                    ));
                } else if types.declares_class(cname) && !descends_from(types, cname, "Error") {
                    found.push(refusal(
                        format!("cannot throw instance of `{cname}`"),
                        format!("`{cname}` does not inherit from `Error`"),
                        expr.span,
                    ));
                }
            }
        }
        StmtKind::Expr(expr) => expression(expr, types, bound, found),
        StmtKind::Return(value) => {
            if let Some(value) = value {
                expression(value, types, bound, found);
            }
        }
        StmtKind::Alias { .. } => {}
    }
}

fn block(block: &Block, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    bound.push();
    stmts(&block.stmts, types, bound, found);
    bound.pop();
}

/// A function body, with its parameters bound to the words they were declared
/// with — so `fn f(const n) { n = 2 }` is caught where `const n = 1; n = 2` is.
fn check_op_returns(decl: &crate::syntax::ast::FnDecl, types: &Types, found: &mut Vec<Raised>) {
    let Some(op) = decl.op else { return };
    let (required_type, op_name) = match op {
        crate::syntax::ast::Op::Str => ("string", "string"),
        crate::syntax::ast::Op::Len => ("int", "len"),
        crate::syntax::ast::Op::Int => ("int", "int"),
        crate::syntax::ast::Op::Float => ("float", "float"),
        crate::syntax::ast::Op::Bool => ("bool", "bool"),
        crate::syntax::ast::Op::List => ("list", "list"),
        crate::syntax::ast::Op::Dict => ("dict", "dict"),
        crate::syntax::ast::Op::Iter => ("list", "iter"),
        crate::syntax::ast::Op::Eq => ("bool", "eq"),
        crate::syntax::ast::Op::Cmp => ("int", "cmp"),
        _ => return,
    };

    if let Some(ret) = &decl.returns {
        if let TypeName::Named(name) = &ret.name {
            if name != required_type {
                found.push(refusal(
                    format!("`op {op_name}` must return {}", an(required_type)),
                    format!("declared return type is `{name}`"),
                    ret.span,
                ));
            }
        }
    }

    check_op_body_returns(&decl.body.stmts, op_name, required_type, types, found);
}

fn check_op_body_returns(stmts: &[Stmt], op_name: &str, required_type: &str, types: &Types, found: &mut Vec<Raised>) {
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Return(Some(expr)) => {
                let held = receiver_type(expr, types);
                let actual_type = match &expr.kind {
                    ExprKind::Int(_) => Some("int"),
                    ExprKind::Float(_) => Some("float"),
                    ExprKind::Str(_) => Some("string"),
                    ExprKind::Bool(_) => Some("bool"),
                    ExprKind::List(_) => Some("list"),
                    ExprKind::Dict(_) => Some("dict"),
                    ExprKind::Nil => Some("nil"),
                    _ => held.class_name(),
                };
                if let Some(actual) = actual_type {
                    if actual != required_type && actual != "any" && actual != "unknown" {
                        found.push(refusal(
                            format!("`op {op_name}` must return {}", an(required_type)),
                            format!("`op {op_name}` returns {}, but {} is required", an(actual), an(required_type)),
                            expr.span,
                        ));
                    }
                }
            }
            StmtKind::If { then, otherwise, .. } => {
                check_op_body_returns(&then.stmts, op_name, required_type, types, found);
                if let Some(other) = otherwise {
                    check_op_body_returns(&[*(other.clone())], op_name, required_type, types, found);
                }
            }
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                check_op_body_returns(&body.stmts, op_name, required_type, types, found);
            }
            StmtKind::Block(b) => {
                check_op_body_returns(&b.stmts, op_name, required_type, types, found);
            }
            StmtKind::Try { body, handler, .. } => {
                check_op_body_returns(&body.stmts, op_name, required_type, types, found);
                check_op_body_returns(&handler.stmts, op_name, required_type, types, found);
            }
            _ => {}
        }
    }
}

fn binary_op_symbol(op: crate::syntax::ast::BinaryOp) -> &'static str {
    match op {
        crate::syntax::ast::BinaryOp::Add => "+",
        crate::syntax::ast::BinaryOp::Sub => "-",
        crate::syntax::ast::BinaryOp::Mul => "*",
        crate::syntax::ast::BinaryOp::Div => "/",
        crate::syntax::ast::BinaryOp::FloorDiv => "//",
        crate::syntax::ast::BinaryOp::Rem => "%",
        crate::syntax::ast::BinaryOp::Eq => "==",
        crate::syntax::ast::BinaryOp::Ne => "!=",
        crate::syntax::ast::BinaryOp::Lt => "<",
        crate::syntax::ast::BinaryOp::Le => "<=",
        crate::syntax::ast::BinaryOp::Gt => ">",
        crate::syntax::ast::BinaryOp::Ge => ">=",
        crate::syntax::ast::BinaryOp::In => "in",
        crate::syntax::ast::BinaryOp::BitAnd => "&",
        crate::syntax::ast::BinaryOp::BitOr => "|",
        crate::syntax::ast::BinaryOp::BitXor => "^",
        crate::syntax::ast::BinaryOp::Shl => "<<",
        crate::syntax::ast::BinaryOp::Shr => ">>",
    }
}

fn body(decl: &std::rc::Rc<crate::syntax::ast::FnDecl>, types: &Types, bound: &mut Bindings, found: &mut Vec<Raised>) {
    check_op_returns(decl, types, found);
    bound.push();
    for param in &decl.params {
        if let Some(ty) = &param.ty {
            check_type_expr(ty, types, found);
        }
        bound.declare(&param.name, param.bind, param.ty.clone());
    }
    if let Some(ty) = &decl.returns {
        check_type_expr(ty, types, found);
        if !ty.nullable && !has_return_value(&decl.body.stmts) {
            found.push(refusal(
                format!("function `{}` declares return type `{}` but might return without a value", decl.name, ty.written()),
                format!("return a value of type `{}` or declare the return type as `{}?`", ty.written(), ty.written()),
                decl.name_span,
            ));
        }
    }
    stmts(&decl.body.stmts, types, bound, found);
    bound.pop();
}

fn check_field(field: &FieldDecl, types: &Types, found: &mut Vec<Raised>) {
    let Some(ty) = &field.ty else {
        return;
    };
    check_type_expr(ty, types, found);
    against(ty, &field.value, &format!("`{}`", field.name), types, found);
}

fn expression(expr: &Expr, types: &Types, bound: &Bindings, found: &mut Vec<Raised>) {
    for child in parts(expr) {
        expression(child, types, bound, found);
    }

    if let ExprKind::Binary { op, lhs, rhs } = &expr.kind {
        if matches!(op, crate::syntax::ast::BinaryOp::Lt | crate::syntax::ast::BinaryOp::Le | crate::syntax::ast::BinaryOp::Gt | crate::syntax::ast::BinaryOp::Ge) {
            let lhs_type = receiver_type(lhs, types);
            let rhs_type = receiver_type(rhs, types);

            if let Some(cname) = lhs_type.class_name() {
                if types.declares_class(cname) {
                    let has_cmp = types.method_of(cname, "cmp").is_some();
                    let has_lt = types.method_of(cname, "lt").is_some();
                    let has_gt = types.method_of(cname, "gt").is_some();

                    if matches!(op, crate::syntax::ast::BinaryOp::Le | crate::syntax::ast::BinaryOp::Ge) && !has_cmp && (has_lt || has_gt) {
                        found.push(refusal(
                            format!("`{}` is not supported on `{cname}`", binary_op_symbol(*op)),
                            "`<=` and `>=` require `op cmp` because deriving them from `op lt` would assume the order is total".to_string(),
                            expr.span,
                        ));
                    }
                }
            }

            if let Some(lname) = lhs_type.class_name() {
                if is_builtin_type(lname) {
                    if let Some(rcname) = rhs_type.class_name() {
                        if types.declares_class(rcname) {
                            let has_cmp = types.method_of(rcname, "cmp").is_some();
                            let has_lt = types.method_of(rcname, "lt").is_some();
                            let has_gt = types.method_of(rcname, "gt").is_some();
                            if !has_cmp && (has_lt || has_gt) {
                                found.push(refusal(
                                    format!("`{}` is not supported between {} and `{rcname}`", binary_op_symbol(*op), an(lname)),
                                    "reflecting a comparison on the right operand requires `op cmp` — `op lt` cannot be read backwards".to_string(),
                                    expr.span,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    if let ExprKind::Binary { op, lhs, rhs } = &expr.kind {
        if matches!(op, crate::syntax::ast::BinaryOp::Div | crate::syntax::ast::BinaryOp::FloorDiv | crate::syntax::ast::BinaryOp::Rem) {
            let is_zero = match &rhs.kind {
                ExprKind::Int(0) => true,
                ExprKind::Float(f) if *f == 0.0 => true,
                _ => false,
            };
            if is_zero {
                found.push(refusal(
                    "division by zero".to_string(),
                    "cannot divide by zero".to_string(),
                    rhs.span,
                ));
            }
        }

        if matches!(op, crate::syntax::ast::BinaryOp::BitAnd | crate::syntax::ast::BinaryOp::BitOr | crate::syntax::ast::BinaryOp::BitXor | crate::syntax::ast::BinaryOp::Shl | crate::syntax::ast::BinaryOp::Shr) {
            let ltype = receiver_type(lhs, types);
            let rtype = receiver_type(rhs, types);
            let method_name = match op {
                crate::syntax::ast::BinaryOp::BitAnd => "bit_and",
                crate::syntax::ast::BinaryOp::BitOr => "bit_or",
                crate::syntax::ast::BinaryOp::BitXor => "bit_xor",
                crate::syntax::ast::BinaryOp::Shl => "shl",
                crate::syntax::ast::BinaryOp::Shr => "shr",
                _ => unreachable!(),
            };
            let l_has_op = ltype.class_name().is_some_and(|c| types.method_of(c, method_name).is_some());
            let r_has_op = rtype.class_name().is_some_and(|c| types.method_of(c, method_name).is_some());

            if !l_has_op && !r_has_op {
                if ltype.class_name() == Some("float") || rtype.class_name() == Some("float") {
                    found.push(refusal(
                        "bitwise operators are not supported on float".to_string(),
                        "bitwise operators require integer operands".to_string(),
                        expr.span,
                    ));
                }
            }
            if matches!(op, crate::syntax::ast::BinaryOp::Shl | crate::syntax::ast::BinaryOp::Shr) {
                if let ExprKind::Int(n) = &rhs.kind {
                    if *n < 0 || *n >= 64 {
                        found.push(refusal(
                            "shift count out of range (0..64)".to_string(),
                            "shift count must be between 0 and 63 inclusive".to_string(),
                            rhs.span,
                        ));
                    }
                }
            }
        }

        check_builtin_binary_op(*op, lhs, rhs, expr.span, types, found);

        if matches!(op, crate::syntax::ast::BinaryOp::Add | crate::syntax::ast::BinaryOp::Sub | crate::syntax::ast::BinaryOp::Mul | crate::syntax::ast::BinaryOp::Div | crate::syntax::ast::BinaryOp::FloorDiv | crate::syntax::ast::BinaryOp::Rem) {
            let ltype = receiver_type(lhs, types);
            let rtype = receiver_type(rhs, types);
            if let Some(lname) = ltype.class_name() {
                if is_builtin_type(lname) {
                    if let Some(rcname) = rtype.class_name() {
                        if types.declares_class(rcname) {
                            found.push(refusal(
                                format!("`{}` is not supported between {} and `{rcname}`", binary_op_symbol(*op), an(lname)),
                                "binary arithmetic asks the left operand only — a class on the right cannot answer".to_string(),
                                expr.span,
                            ));
                        }
                    }
                }
            }
        }
    }

    if let ExprKind::Unary { op: crate::syntax::ast::UnaryOp::BitNot, rhs } = &expr.kind {
        let rtype = receiver_type(rhs, types);
        if let Some(cname) = rtype.class_name() {
            if cname == "float" && types.method_of(cname, "bit_not").is_none() {
                found.push(refusal(
                    "bitwise operators are not supported on float".to_string(),
                    "bitwise NOT requires integer operand".to_string(),
                    expr.span,
                ));
            }
        }
    }

    if let ExprKind::Slice { target, .. } = &expr.kind {
        let held = receiver_type(target, types);
        if let Some(cname) = held.class_name() {
            if matches!(cname, "int" | "float" | "bool" | "nil") {
                found.push(refusal(
                    format!("cannot slice {}", an(cname)),
                    "slicing is only supported on sequences like string and list".to_string(),
                    expr.span,
                ));
            }
        }
    }

    if let ExprKind::Binary { op: crate::syntax::ast::BinaryOp::In, rhs, .. } = &expr.kind {
        let rhs_type = receiver_type(rhs, types);
        if let Some(name) = rhs_type.class_name() {
            if matches!(name, "int" | "float" | "bool" | "nil") {
                found.push(refusal(
                    format!("`in` is not supported on {}", an(name)),
                    "`in` operator requires a container (such as list, dict, string) or a type implementing `op contains`".to_string(),
                    expr.span,
                ));
            }
        }
    }

    if let ExprKind::Dict(pairs) = &expr.kind {
        for (k, _) in pairs {
            if is_unhashable_expr(k, types) {
                found.push(refusal(
                    "unhashable key in dict literal".to_string(),
                    "dict keys must be hashable immutable values (such as string, int, float, or bool)".to_string(),
                    k.span,
                ));
            }
        }
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

    if let ExprKind::Is { ty, .. } = &expr.kind {
        check_type_expr(ty, types, found);
    }

    if let ExprKind::Assign { target, .. } = &expr.kind
        && let ExprKind::Field { target: receiver, .. } = &target.kind
        && let ExprKind::Var(var) = &receiver.kind
        && bound.kind(&var.name).is_some_and(|bind| bind.freezes())
    {
        found.push(refusal(
            format!("cannot modify field of `{}`", var.name),
            format!(
                "`{}` is bound with `const`, which freezes the value deeply — declare it with `let` or `final` to allow modifying its fields",
                var.name
            ),
            target.span,
        ));
    }

    // Reaching a member the declaration put out of reach. `may_offer` is the
    // same rule the completion list follows, so what the editor offers and what
    // it squiggles cannot disagree.
    if let ExprKind::Field { target, name, .. } = &expr.kind {
        visibility(target, name, expr.span, types, found);
        let held = receiver_type(target, types);
        if let Some(cname) = held.class_name() {
            if is_builtin_type(cname) && builtin_method(cname, name).is_none() {
                found.push(refusal(
                    format!("`{cname}` has no method `{name}`"),
                    format!("type `{cname}` does not define method `{name}`"),
                    expr.span,
                ));
            }
        }
    }

    let ExprKind::Call { callee, args } = &expr.kind else {
        // `xs[i] = v` is the other way into a typed container.
        if let ExprKind::Assign { target, value } = &expr.kind
            && let ExprKind::Index { target: collection, index } = &target.kind
        {
            let held = receiver_type(collection, types);
            check_element(&held, "list", 0, value, "the item", types, found);
            check_element(&held, "dict", 0, index, "the key", types, found);
            check_element(&held, "dict", 1, value, "the value", types, found);
        }
        return;
    };
    // A call to a `fn` the program declared, against the parameters it named.
    if let ExprKind::Var(var) = &callee.kind {
        if var.name == "function" {
            found.push(refusal(
                "builtin type `function` cannot be instantiated".to_string(),
                "there is no value a function can be constructed from".to_string(),
                expr.span,
            ));
        } else if let Some(decl) = types.function(&var.name) {
            arguments(&decl.params, args, expr.span, types, found);
        } else if let Some(native) = types.native(&var.name) {
            native_arguments(native, args, expr.span, types, found);
        } else if let Some(cname) = receiver_type(callee, types).class_name() {
            if matches!(cname, "int" | "float" | "bool" | "nil") {
                found.push(refusal(
                    format!("{} is not callable", an(cname)),
                    "only functions, methods, and classes can be called".to_string(),
                    callee.span,
                ));
            }
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
            arguments(decl.params.get(1..).unwrap_or(&[]), args, expr.span, types, found);
        } else if let Some(native) = builtin_method(class, name) {
            native_arguments(native, args, expr.span, types, found);
        }
    }

    // Mutating what `const` froze. `final` is the other axis and is untouched:
    // it binds the name once and leaves the object alone, so a `final` list
    // still grows.
    if MUTATORS.contains(&name.as_str())
        && let Some(var) = root_var(target)
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
fn arguments(params: &[Param], args: &[Expr], span: crate::syntax::token::Span, types: &Types, found: &mut Vec<Raised>) {
    if params.len() != args.len() {
        found.push(refusal(
            format!("expected {} arguments, got {}", params.len(), args.len()),
            format!("function declared with {} parameters", params.len()),
            span,
        ));
        return;
    }
    for (param, arg) in params.iter().zip(args) {
        if let Some(ty) = &param.ty {
            against(ty, arg, &format!("`{}`", param.name), types, found);
        }
    }
}

/// The same for a builtin, whose parameters name a *set* of types.
fn native_arguments(native: &Native, args: &[Expr], span: crate::syntax::token::Span, types: &Types, found: &mut Vec<Raised>) {
    if native.params.len() != args.len() {
        found.push(refusal(
            format!("`{}` expected {} arguments, got {}", native.name, native.params.len(), args.len()),
            format!("`{}` takes {} arguments", native.name, native.params.len()),
            span,
        ));
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
        ExprKind::Call { callee, .. } => {
            if let ExprKind::Var(var) = &callee.kind {
                if types.declares_class(&var.name) || is_builtin_type(&var.name) {
                    return Type::class(&var.name);
                }
            }
            types.of_expr(expr.span.start)
        }
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

fn is_numeric_type(cname: &str) -> bool {
    matches!(cname, "int" | "float")
}

fn check_builtin_binary_op(
    op: crate::syntax::ast::BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    span: crate::syntax::token::Span,
    types: &Types,
    found: &mut Vec<Raised>,
) {
    use crate::syntax::ast::BinaryOp::*;
    let ltype = receiver_type(lhs, types);
    let rtype = receiver_type(rhs, types);

    let (Some(lname), Some(rname)) = (ltype.class_name(), rtype.class_name()) else { return };
    if !is_builtin_type(lname) || !is_builtin_type(rname) {
        return;
    }

    let op_method = match op {
        Add => "add",
        Sub => "sub",
        Mul => "mul",
        Div => "div",
        FloorDiv => "floor_div",
        Rem => "rem",
        Lt => "lt",
        Le => "le",
        Gt => "gt",
        Ge => "ge",
        _ => return,
    };

    if types.method_of(lname, op_method).is_some() || types.method_of(rname, op_method).is_some() {
        return;
    }

    let is_valid = match op {
        Add => (is_numeric_type(lname) && is_numeric_type(rname)) || (lname == "string" && rname == "string") || (lname == "list" && rname == "list"),
        Sub | Mul | Div | FloorDiv | Rem => is_numeric_type(lname) && is_numeric_type(rname),
        Lt | Le | Gt | Ge => (is_numeric_type(lname) && is_numeric_type(rname)) || (lname == "string" && rname == "string"),
        _ => true,
    };

    if !is_valid {
        found.push(refusal(
            format!("`{}` is not supported between {} and {}", binary_op_symbol(op), an(lname), an(rname)),
            format!("type `{lname}` and type `{rname}` cannot be combined with `{}`", binary_op_symbol(op)),
            span,
        ));
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

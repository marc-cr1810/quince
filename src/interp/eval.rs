//! Evaluating expressions, and reading and writing the names they mention.
//!
//! `eval` is the tree walk itself; `read` and `assign` are the two ends of a name.
//!
//! The run-time type checks v0.7 puts on an assignment go in `assign`, which is
//! already the one place every write to a binding, a field, and an element passes
//! through.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Result};
use crate::interp::call::Written;
use crate::interp::error::frozen;
use crate::interp::index::key_of;
use crate::interp::{Attr, Interp, resolved};
use crate::runtime::dict::{Dict, Key};
use crate::runtime::env::{self, AssignError};
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::{BoundMethod, Value};
use crate::syntax::ast::{
    BinaryOp, BindKind, Expr, ExprKind, LogicalOp, Op, ShortAssignOp, Slot, TypeExpr, Var,
};
use crate::syntax::token::Span;

/// The target and contents of a bracket, whichever of the two nodes wrote it.
///
/// `xs[i]` is an [`ExprKind::Index`] and `Pair[int, string]` is an
/// [`ExprKind::TypeArgs`], and in callee position the two have to be handled
/// together — a subscript and a type argument list are told apart by what the
/// target holds, not by how many commas are inside. This is the one place that
/// flattens the distinction the parser was forced to make.
fn brackets(expr: &Expr) -> Option<(&Expr, &[Expr])> {
    match &expr.kind {
        ExprKind::Index { target, index } => Some((target, std::slice::from_ref(index.as_ref()))),
        ExprKind::TypeArgs { target, args } => Some((target, args.as_slice())),
        _ => None,
    }
}

/// Which compound assignment `assign_op` is carrying out.
///
/// The two families reach it together because they agree about the part that is
/// hard — evaluating the pieces of the target exactly once — and disagree only
/// about what to do with the value found there.
#[derive(Clone, Copy)]
pub(super) enum Compound {
    /// `a += b` and its twelve siblings: always combine, always write.
    Op(BinaryOp),
    /// `a and= b`, `a or= b`, `a ??= b`: look first, and possibly do neither.
    Short(ShortAssignOp),
}

/// What a compound assignment decided to do with the value it read.
///
/// [`Produced::Keep`] is the short-circuit: the target answered for itself, so
/// the value travels back out as the expression's result without being written
/// anywhere. Not an `Option`, because the caller needs the value in both cases
/// and an `Option<Value>` would leave it holding the wrong one.
enum Produced {
    Write(Value),
    Keep(Value),
}

/// Where the two halves of `collection[key] = value` were written.
///
/// A subscripted write is the one form that can be refused for either of two
/// values — a `dict[string, int]` has an opinion about both — and "the key" and
/// "the value" are only worth saying if the caret agrees with which one.
#[derive(Clone, Copy)]
struct Subscript {
    key: Span,
    value: Span,
}

impl Interp {
    /// Evaluates several expressions in order, keeping the results already
    /// computed rooted while the remaining ones run.
    ///
    /// Every sub-expression after the first can reach a safe point — one
    /// argument that calls a function is enough — and until the caller stores
    /// them, the values computed so far exist only in this Rust frame. Without
    /// this, `[mk(), churn()]` collects `mk()`'s list and hands back a handle
    /// into a reused slot.
    ///
    /// The caller gets the values back unrooted, so it must consume them
    /// without reaching another safe point. Every caller allocates or binds
    /// immediately, which is what makes that safe.
    ///
    /// Only values that carry a handle are rooted. An `int` or a `bool` is
    /// inline and cannot be collected, and a string is reference counted
    /// outside the heap — so the common arithmetic path pays a discriminant
    /// check and nothing more.
    pub(super) fn eval_seq<'e>(
        &mut self,
        exprs: impl IntoIterator<Item = &'e Expr>,
        env: ObjId,
    ) -> Result<Vec<Value>> {
        let exprs = exprs.into_iter();
        let mark = self.temps.len();
        let mut values = Vec::with_capacity(exprs.size_hint().0);
        for expr in exprs {
            match self.eval(expr, env) {
                Ok(value) => {
                    if value.handle().is_some() {
                        self.temps.push(value.clone());
                    }
                    values.push(value);
                }
                Err(err) => {
                    self.temps.truncate(mark);
                    return Err(err);
                }
            }
        }
        self.temps.truncate(mark);
        Ok(values)
    }

    /// [`Interp::eval_seq`] for the two-operand case, which is every binary
    /// operator and every subscript.
    ///
    /// Spelled out rather than delegating, because the `Vec` that version
    /// returns would be a heap allocation on every single arithmetic operation.
    pub(super) fn eval_pair(
        &mut self,
        first: &Expr,
        second: &Expr,
        env: ObjId,
    ) -> Result<(Value, Value)> {
        let first = self.eval(first, env)?;

        // Only the second operand can reach a safe point, and only a handle can
        // be collected once it does.
        let mark = self.temps.len();
        if first.handle().is_some() {
            self.temps.push(first.clone());
        }
        let second = self.eval(second, env);
        self.temps.truncate(mark);

        Ok((first, second?))
    }

    /// Whether the rest of this postfix chain is to be skipped.
    ///
    /// True when a link before this one short-circuited, and true when this link
    /// is the `?.` that finds `nil` — in which case it also records that, so
    /// every node after it skips too. Reading and setting in one place is what
    /// keeps the two halves of the rule from drifting.
    pub(super) fn skips(&mut self, optional: bool, receiver: &Value) -> bool {
        if self.short_circuit {
            return true;
        }
        if optional && matches!(receiver, Value::Nil) {
            self.short_circuit = true;
            return true;
        }
        false
    }

    pub(super) fn eval(&mut self, expr: &Expr, env: ObjId) -> Result<Value> {
        match &expr.kind {
            // The end of a chain containing a `?.`. Whatever it produced is the
            // answer, and the flag stops here — an outer chain is a separate
            // expression and short-circuits on its own account.
            ExprKind::Chain(inner) => {
                let value = self.eval(inner, env);
                self.short_circuit = false;
                value
            }

            // The right side is evaluated only when the left is `nil`, which is
            // why this is not a `BinaryOp`: `d[k] ?? expensive()` must not run
            // `expensive()` when the key was there.
            ExprKind::Coalesce { lhs, rhs } => match self.eval(lhs, env)? {
                Value::Nil => self.eval(rhs, env),
                answered => Ok(answered),
            },

            ExprKind::Is { value, ty } => {
                let value = self.eval(value, env)?;
                Ok(Value::Bool(self.has_type(ty, &value)))
            }

            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(n) => Ok(Value::Float(*n)),
            ExprKind::Str(s) => Ok(Value::Str(Rc::from(s.as_str()))),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),

            ExprKind::Var(var) => self.read(var, env, expr.span),

            ExprKind::List(items) => {
                let values = self.eval_seq(items, env)?;
                Ok(Value::List(self.heap.alloc(Object::List(values))))
            }

            ExprKind::Dict(entries) => {
                let values = self.eval_seq(entries.iter().flat_map(|(k, v)| [k, v]), env)?;
                let mut dict = Dict::new();
                for pair in values.chunks_exact(2) {
                    // A repeated key overwrites, as in Python — the literal is
                    // just a run of insertions.
                    dict.insert(key_of(&self.heap, &pair[0], expr.span)?, pair[1].clone());
                }
                Ok(Value::Dict(self.heap.alloc(Object::Dict(dict))))
            }

            ExprKind::Unary { op, rhs } => {
                let value = self.eval(rhs, env)?;
                self.unary(*op, value, expr.span)
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let (l_val, r_val) = self.eval_pair(lhs, rhs, env)?;
                self.binary(*op, l_val, r_val, lhs.span, rhs.span, expr.span)
            }

            ExprKind::Logical { op, lhs, rhs } => {
                let lhs = self.eval(lhs, env)?;
                let truthy = self.is_truthy(&lhs)?;
                let short_circuits = match op {
                    LogicalOp::And => !truthy,
                    LogicalOp::Or => truthy,
                };
                // Returns the operand itself rather than a bool, so `a || b`
                // works as a default-value idiom.
                if short_circuits {
                    Ok(lhs)
                } else {
                    self.eval(rhs, env)
                }
            }

            ExprKind::Call { callee, args } => {
                // A method call is fused rather than evaluating the callee to a
                // bound method and then calling it. `xs.push(1)` is by far the
                // common form, and going through `ExprKind::Field` would
                // allocate an object per call only to drop it immediately.
                if matches!(&callee.kind, ExprKind::Field { .. }) {
                    return self.eval_method_call(callee, args, env, expr.span);
                }
                // `super.init(name)` is the overwhelmingly common form, and it
                // is fused for the same reason: no bound method to allocate.
                if let ExprKind::Super {
                    name,
                    parent,
                    receiver,
                } = &callee.kind
                {
                    let class = self.super_class(parent, env, callee.span)?;
                    let receiver = self.read(receiver, env, callee.span)?;

                    // `super.init` where the superclass is a builtin is the one
                    // `super` call that is not a method call. A builtin's `init`
                    // is a conversion — no receiver, and deliberately not among
                    // its methods — so the lookup below would not find it and
                    // inserting a receiver would be wrong if it did.
                    if name == Op::Init.name()
                        && let Some(builtin) = self.heap.class(class).builtin
                    {
                        let mark = self.temps.len();
                        self.temps.push(receiver.clone());
                        let values = self.eval_seq(args.iter().map(|arg| &arg.value), env);
                        self.temps.truncate(mark);
                        return self.super_init(class, builtin, &receiver, values?, expr.span);
                    }

                    let method = self.super_method(class, name, callee.span)?;
                    let mark = self.temps.len();
                    self.temps.push(receiver.clone());
                    self.temps.push(method.clone());
                    let values = self.eval_seq(args.iter().map(|arg| &arg.value), env);
                    self.temps.truncate(mark);
                    let values = values?;
                    let shape = self.shape_for_method(&method, args, &values, expr.span)?;
                    let named = method.callable_name(&self.heap).to_string();
                    let values = self.arranged(shape, args, values, &named, expr.span)?;
                    return self.call_method(receiver, method, values, expr.span);
                }

                // Brackets in callee position are two different calls sharing a
                // spelling: `Stack[int]()` builds a generic class, and
                // `handlers[i]()` calls what a list holds. Nothing in the
                // grammar separates them, so the bracketed target is evaluated
                // once — here, rather than by each branch — and what it turns
                // out to hold decides. v0.9 §3.1.
                if let Some((bracketed, inside)) = brackets(callee) {
                    let target = self.eval(bracketed, env)?;
                    if let Value::Class(id) = target {
                        return self.built_generic(id, inside, args, env, expr.span);
                    }
                    // Not a class, so the brackets were a subscript. Held across
                    // the index expressions for the reason the callee is held
                    // below.
                    let mark = self.temps.len();
                    if target.handle().is_some() {
                        self.temps.push(target.clone());
                    }
                    let inside = self.eval_seq(inside, env);
                    self.temps.truncate(mark);
                    let inside = inside?;
                    let [index] = inside.as_slice() else {
                        return Err(QuinceError::new(
                            format!(
                                "{} does not take type arguments",
                                target.type_name(&self.heap)
                            ),
                            callee.span,
                        )
                        .with_kind(ErrorKind::Type)
                        .with_help(
                            "a comma inside `[…]` supplies a generic class with its arguments, \
                             and only a class takes any — a subscript reads one element",
                        ));
                    };
                    let callee = self.index_get(&target, index, callee.span)?;
                    return self.call_evaluated(callee, args, env, expr.span);
                }

                let target = self.eval(callee, env)?;
                self.call_evaluated(target, args, env, expr.span)
            }

            ExprKind::Index { target, index } => {
                let (target, index) = self.eval_pair(target, index, env)?;
                self.index_get(&target, &index, expr.span)
            }

            // `Pair[int, string]` outside a call, which is a type written where
            // a value goes. Reached only from here: the call position is fused
            // above, where the brackets mean something.
            ExprKind::TypeArgs { target, .. } => {
                let target = self.eval(target, env)?;
                Err(self.not_a_value(&target, expr.span))
            }

            ExprKind::Slice { target, start, end } => {
                // Three sub-expressions, so neither `eval_pair` nor a hand-
                // rolled pair of marks fits. `eval_seq` already roots each
                // value against the ones evaluated after it, which is exactly
                // the guarantee needed here.
                let mut parts: Vec<&Expr> = Vec::with_capacity(3);
                parts.push(target);
                parts.extend(start.as_deref());
                parts.extend(end.as_deref());

                let mut values = self.eval_seq(parts, env)?.into_iter();
                let target = values.next().expect("the target is always evaluated");
                let start = start.as_ref().map(|_| values.next().expect("a bound"));
                let end = end.as_ref().map(|_| values.next().expect("a bound"));

                self.slice(&target, start.as_ref(), end.as_ref(), expr.span)
            }

            // Only reached when a method is not being called immediately — the
            // fused path above handles `x.m(…)`. Binding it makes a method an
            // ordinary value rather than syntax that works in one position.
            ExprKind::Field {
                target,
                name,
                optional,
            } => {
                let receiver = self.eval(target, env)?;
                // Either this link found `nil` behind a `?.`, or a link before
                // it did. Both answer `nil` and neither reaches the member.
                if self.skips(*optional, &receiver) {
                    return Ok(Value::Nil);
                }
                let name_span = Span::new(
                    (target.span.end as usize + 1).min(expr.span.end as usize),
                    expr.span.end as usize,
                );
                match self.attr(&receiver, name, target.span, name_span, expr.span)? {
                    Attr::Field(value) => Ok(value),
                    // Nothing between here and the allocation is a safe point,
                    // so the receiver needs no rooting on the way in; once it
                    // is inside, `trace` keeps it alive.
                    Attr::Method(method) => Ok(Value::BoundMethod(
                        self.heap
                            .alloc(Object::BoundMethod(BoundMethod { receiver, method })),
                    )),
                }
            }

            // Only reached when the method is not called immediately. Binding it
            // to `self` is the whole point of `super`: the parent's code runs,
            // but on this object.
            ExprKind::Super {
                name,
                parent,
                receiver,
            } => {
                let class = self.super_class(parent, env, expr.span)?;
                let receiver = self.read(receiver, env, expr.span)?;
                let method = self.super_method(class, name, expr.span)?;
                Ok(Value::BoundMethod(self.heap.alloc(Object::BoundMethod(
                    BoundMethod { receiver, method },
                ))))
            }

            ExprKind::Assign { target, value } => {
                let evaluated = self.eval(value, env)?;
                self.assign(target, evaluated, env, Written::rebound(value))
            }

            ExprKind::AssignOp { target, op, value } => {
                self.assign_op(target, Compound::Op(*op), value, env, expr.span)
            }

            ExprKind::AssignShort { target, op, value } => {
                self.assign_op(target, Compound::Short(*op), value, env, expr.span)
            }
        }
    }

    /// Reads a variable through the slot the resolver assigned it.
    pub(super) fn read(&mut self, var: &Var, env: ObjId, span: Span) -> Result<Value> {
        match resolved(&var.slot) {
            Slot::Local { hops, index } => {
                let scope = env::ancestor(&self.heap, env, hops);
                self.heap.env(scope).get(index).cloned().ok_or_else(|| {
                    // Declarations are hoisted to the top of their scope, so a
                    // slot can be reached before its `let` has run.
                    QuinceError::new(
                        format!("`{}` is used before it is declared", var.name),
                        span,
                    )
                    .with_kind(ErrorKind::Name)
        .with_help("a declaration takes effect from its own line, so move it above the use")
                })
            }
            Slot::Global => {
                // A const generic parameter is in scope in the body as a value,
                // read-only — v0.9 §3.3. It has no slot, because it is not a
                // binding anything declared: it is part of the receiver's
                // *type*, and which value it holds depends on which instance
                // the method was called on. So the resolver, seeing no
                // declaration, calls it a global, and this is where it stops
                // being one.
                //
                // Before the globals, because the class header is the nearer
                // declaration and the nearer declaration wins — the same rule a
                // parameter shadowing a global follows, reached by a different
                // road. The guard is what keeps that off the hot path: every
                // read of a global name would otherwise pay for a feature it is
                // not using, and `type_bindings` is empty in every frame of
                // every program that declares no generic class.
                if !self.type_bindings.is_empty()
                    && let Some(value) = self.const_binding(&var.name)
                {
                    return Ok(value);
                }
                self
                .heap
                .globals(env::module_of(&self.heap, env))
                .get(&var.name)
                .cloned()
                .ok_or_else(|| {
                    let mut err =
                        QuinceError::new(format!("undefined variable `{}`", var.name), span)
                            .with_help(
                                "nothing is bound to that name here — a name declared inside a \
                                 block is not visible outside it",
                            )
                            .with_kind(ErrorKind::Name);

                    let mut candidates: Vec<String> = self
                        .heap
                        .globals(env::module_of(&self.heap, env))
                        .iter()
                        .map(|(k, _)| k.to_string())
                        .collect();
                    for builtin in crate::runtime::class::BUILTINS {
                        candidates.push(builtin.name().to_string());
                    }
                    let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
                    if let Some(suggestion) = crate::error::did_you_mean(&var.name, refs) {
                        err = err.with_help(format!("did you mean `{suggestion}`?"));
                    }
                    err
                })
            }
        }
    }

    /// The value a name may be rebound to, given what it was declared to hold.
    ///
    /// An annotation constrains the *name*, not the one value bound to it first
    /// — so every write after the declaration is checked against the same
    /// annotation the declaration was, by the same function, and widens the same
    /// way. `let x: float = 0` followed by `x = 5` stores `5.0`, and a container
    /// rebound to a new one is rewalked and re-described.
    ///
    /// A name bound `final` or `const` refuses outright, whatever it was
    /// annotated as. Those two words are about the binding and the annotation is
    /// about the value, so they are asked in that order — a `final xs:
    /// list[int]` is a name that cannot be rebound *and* a list that must hold
    /// ints, and neither claim stands in for the other.
    fn rebound(
        &mut self,
        ty: Option<&TypeExpr>,
        bind: BindKind,
        value: Value,
        name: &str,
        span: Span,
        written: Written<'_>,
    ) -> Result<Value> {
        if !bind.mutable() {
            return Err(QuinceError::new(format!("cannot reassign `{name}`"), span)
                .with_kind(ErrorKind::Frozen)
        .with_help("it is bound with `final` or `const`, either of which binds a name once — declare it with `let` to reassign it")
                .with_help(match bind.freezes() {
                    true => format!("`{name}` is `const`, so the name is bound once and what it holds is frozen"),
                    false => format!("`{name}` is `final`, so the name is bound once"),
                }));
        }
        match ty {
            Some(ty) => self.coerced_from(ty, value, &format!("`{name}`"), span, written),
            None => Ok(value),
        }
    }

    /// Writes `value` through `target`, with `written` saying where the value
    /// came from so a refusal can underline it rather than the name it was
    /// being written to.
    ///
    /// The target's span is what a reader looks at to answer "which write", and
    /// the language marks it for every mistake that *is* about the target — a
    /// `final` name, a frozen object, an index past the end. A wrong *value* is
    /// not one of those: the message already names the boundary that refused
    /// it, so repeating the boundary under the caret leaves the actual culprit
    /// unmarked. `xs = ["hello"]` under a `list[int]` said "item 0 is `int`"
    /// with two dashes under `xs`.
    pub(super) fn assign(
        &mut self,
        target: &Expr,
        value: Value,
        env: ObjId,
        written: Written<'_>,
    ) -> Result<Value> {
        match &target.kind {
            ExprKind::Var(var) => self.assign_var(var, value, env, target.span, written),

            ExprKind::Index {
                target: collection,
                index: subscript,
            } => {
                // `value` was evaluated by the caller and is held across two
                // more evaluations, either of which can reach a safe point.
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let evaluated = self.eval_pair(collection, subscript, env);
                self.temps.truncate(mark);
                let (collection, index) = evaluated?;
                let at = Subscript {
                    key: subscript.span,
                    value: written.caret(target.span),
                };
                self.assign_index(collection, index, value, target.span, at)
            }

            ExprKind::Field {
                optional: _,
                target: object,
                name,
            } => {
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let object = self.eval(object, env);
                self.temps.truncate(mark);
                self.assign_field(object?, name, value, target.span)
            }

            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }

    /// `a op= b`, with the target's own sub-expressions evaluated once.
    ///
    /// That is the whole of what the form buys over writing `a = a op b` out —
    /// `d[f()] += 1` calls `f` a single time — so the shape here is: evaluate
    /// the pieces that say *where*, read through them, combine, and write back
    /// through the same pieces.
    ///
    /// The operator reached is the binary one, with no in-place slot beside it:
    /// a class defining `op add` gets `+=` for free, and one wanting in-place
    /// mutation writes a method and says so.
    ///
    /// `and=`, `or=`, and `??=` come through here too, as [`Compound::Short`].
    /// They share the whole of the "evaluate the pieces that say *where* exactly
    /// once" arrangement and differ only in what [`Self::combined`] decides,
    /// which is why they are a payload here rather than a second copy of it.
    pub(super) fn assign_op(
        &mut self,
        target: &Expr,
        op: Compound,
        rhs: &Expr,
        env: ObjId,
        span: Span,
    ) -> Result<Value> {
        match &target.kind {
            // A name needs nothing evaluated to reach it twice, so this is the
            // one case where "once" costs nothing to arrange.
            ExprKind::Var(var) => {
                let current = self.read(var, env, target.span)?;
                // The whole expression, because that is what made the value: a
                // `+=` that produces a float from an int and a float is refused
                // for what neither operand alone was.
                match self.combined(op, current, rhs, env, target.span, span)? {
                    Produced::Write(value) => {
                        self.assign_var(var, value, env, target.span, Written::produced(span))
                    }
                    Produced::Keep(value) => Ok(value),
                }
            }

            ExprKind::Index {
                target: collection,
                index: subscript,
            } => {
                let (collection, index) = self.eval_pair(collection, subscript, env)?;
                // Both are held across the read, the right side, and the
                // operator, any of which can run a program's code.
                let mark = self.temps.len();
                self.temps.extend(
                    [&collection, &index]
                        .into_iter()
                        .filter(|value| value.handle().is_some())
                        .cloned(),
                );
                let produced = self
                    .index_get(&collection, &index, span)
                    .and_then(|current| self.combined(op, current, rhs, env, target.span, span));
                self.temps.truncate(mark);
                match produced? {
                    Produced::Write(value) => {
                        let at = Subscript { key: subscript.span, value: span };
                        self.assign_index(collection, index, value, target.span, at)
                    }
                    Produced::Keep(value) => Ok(value),
                }
            }

            ExprKind::Field {
                target: object,
                name,
                ..
            } => {
                let receiver = self.eval(object, env)?;
                let mark = self.temps.len();
                if receiver.handle().is_some() {
                    self.temps.push(receiver.clone());
                }
                let produced = self
                    .attr(&receiver, name, object.span, target.span, span)
                    .map(|found| found.value().clone())
                    .and_then(|current| self.combined(op, current, rhs, env, target.span, span));
                self.temps.truncate(mark);
                match produced? {
                    Produced::Write(value) => {
                        self.assign_field(receiver, name, value, target.span)
                    }
                    Produced::Keep(value) => Ok(value),
                }
            }

            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }

    /// Evaluates the right side of a compound assignment and applies the
    /// operator, keeping the value already read rooted while it runs.
    ///
    /// For a [`Compound::Short`] the right side may not be evaluated at all —
    /// that is the point of the form — and then nothing is written either. The
    /// value already read is the answer, so it is handed back as
    /// [`Produced::Keep`] rather than being assigned over itself: `d[f()] ??= 0`
    /// on a key that is already set must not re-enter the dict, which for a
    /// `final` field would be the difference between working and being refused.
    fn combined(
        &mut self,
        op: Compound,
        current: Value,
        rhs: &Expr,
        env: ObjId,
        target_span: Span,
        span: Span,
    ) -> Result<Produced> {
        let op = match op {
            Compound::Op(op) => op,
            Compound::Short(op) => {
                let answered = match op {
                    ShortAssignOp::Coalesce => !matches!(current, Value::Nil),
                    ShortAssignOp::And => !self.is_truthy(&current)?,
                    ShortAssignOp::Or => self.is_truthy(&current)?,
                };
                if answered {
                    return Ok(Produced::Keep(current));
                }
                // No operator to apply: the right side *is* the new value,
                // exactly as it is for `a and b`, which answers with an operand
                // rather than with a bool.
                let mark = self.temps.len();
                if current.handle().is_some() {
                    self.temps.push(current);
                }
                let value = self.eval(rhs, env);
                self.temps.truncate(mark);
                return Ok(Produced::Write(value?));
            }
        };

        let mark = self.temps.len();
        if current.handle().is_some() {
            self.temps.push(current.clone());
        }
        let operand = self.eval(rhs, env);
        self.temps.truncate(mark);
        let value = self.binary(op, current, operand?, target_span, rhs.span, span)?;
        Ok(Produced::Write(value))
    }

    /// Writes through a name, checked against what the declaration said.
    fn assign_var(
        &mut self,
        var: &Var,
        value: Value,
        env: ObjId,
        span: Span,
        written: Written<'_>,
    ) -> Result<Value> {
        match resolved(&var.slot) {
            // The resolver rejected assignment to a `const` *local* declared
            // with one, but not to a `const` parameter — that word is read at
            // the call, not at the declaration it resolves. The annotation on
            // the slot is what answers here.
            Slot::Local { hops, index } => {
                let scope = env::ancestor(&self.heap, env, hops);
                let declared = self.heap.env(scope).ty(index);
                let bind = self.heap.env(scope).bind_kind(index);
                let value =
                    self.rebound(declared.as_deref(), bind, value, &var.name, span, written)?;
                self.heap.env_mut(scope).set(index, value.clone());
                Ok(value)
            }
            Slot::Global => {
                let name = &var.name;
                let module = env::module_of(&self.heap, env);
                // A global's `final`/`const` is refused by `assign` below, which
                // is where the flag lives — so only the annotation is asked here.
                let declared = self.heap.globals(module).ty(name);
                let value = match declared {
                    Some(ty) => {
                        self.coerced_from(&ty, value, &format!("`{name}`"), span, written)?
                    }
                    None => value,
                };
                match self.heap.globals_mut(module).assign(name, value.clone()) {
                    Ok(()) => {}
                    Err(AssignError::Undefined) => {
                        return Err(
                            QuinceError::new(format!("undefined variable `{name}`"), span)
                                .with_kind(ErrorKind::Name),
                        );
                    }
                    // `Frozen` rather than a kind of its own, because the class
                    // is named for the refusal and not for the mechanism: a
                    // `final` name and a `const` list are different things to
                    // pin, and both answer "that will not change" to whoever
                    // tried to change it.
                    //
                    // The local form of this is a `DeclarationError` from the
                    // resolver, which cannot see globals to give them the same
                    // treatment. Same sentence, two kinds — see Bindings.
                    Err(AssignError::Immutable) => {
                        return Err(
                            QuinceError::new(format!("cannot reassign `{name}`"), span)
                                .with_kind(ErrorKind::Frozen)
                                .with_help(
                                    "it is bound with `final` or `const`, either of which binds \
                                     a name once — declare it with `let` to reassign it",
                                ),
                        );
                    }
                }
                Ok(value)
            }
        }
    }

    /// Writes `collection[index] = value`, with both already evaluated.
    ///
    /// Taking them as values rather than as expressions is what lets `xs[i] +=
    /// 1` read and write through one evaluation of `xs` and one of `i`. `span`
    /// stays the target's — it answers "which write" for everything about the
    /// target — while `at` carries where the two *values* were written, so a
    /// container's annotation refusing one of them underlines the one it meant.
    fn assign_index(
        &mut self,
        collection: Value,
        index: Value,
        value: Value,
        span: Span,
        at: Subscript,
    ) -> Result<Value> {
        // The class first, and `const` before the class. A frozen object must
        // not get to *run* on a write that is already refused: an `op set` that
        // logged, counted or raised would have happened, and the assignment it
        // belonged to would still fail. Freezing is a promise about the object a
        // program holds, so it is checked on the instance rather than on the
        // payload underneath it.
        if let Some(method) = self.slot(&collection, Op::Set) {
            if collection.handle().is_some_and(|id| self.heap.is_frozen(id)) {
                return Err(frozen(&self.heap, &collection, span));
            }
            let method = self.op_for(
                Op::Set,
                method,
                &collection,
                &[index.clone(), value.clone()],
                (span, span, span),
            )?;
            // What the op answers is discarded: `x[i] = v` is worth `v`, the
            // same as every other assignment in the language.
            self.call_op(method, &collection, vec![index, value.clone()])?;
            return Ok(value);
        }

        // Written through to the payload for a class extending `dict` or
        // `list`, so `bag['a'] = 1` reaches the dict the object *is*. The
        // `const` check still names `collection`, since freezing applies to the
        // object a program holds.
        match collection.base(&self.heap).clone() {
            // Assigning to a missing key inserts it, where assigning past the
            // end of a list stays an error: a list's indices are positions, and
            // there is no meaningful gap to fill. The mutation happens inside
            // the `map` so that the borrow it needs has ended by the time the
            // error — which reads the heap to name the type — is built.
            Value::Dict(id) => {
                // Against the descriptor before the key is built, so a refused
                // pair leaves the dict as it was found.
                let checked_key = self.admitted(id, 0, index.clone(), "the key", at.key)?;
                let held = self.admitted(id, 1, value.clone(), "the value", at.value)?;
                let key = key_of(&self.heap, &checked_key, span)?;
                let written = self
                    .heap
                    .dict_mut(id)
                    .map(|entries| entries.insert(key, held.clone()));
                written.map_err(|_| frozen(&self.heap, &collection, span))?;
                Ok(held)
            }
            _ => {
                let (id, offset) = self.list_index(&collection, &index, span)?;
                let held = self.admitted(id, 0, value.clone(), "the item", at.value)?;
                let written = self
                    .heap
                    .list_mut(id)
                    .map(|items| items[offset] = held.clone());
                written.map_err(|_| frozen(&self.heap, &collection, span))?;
                Ok(held)
            }
        }
    }

    /// Writes `object.name = value`, with the receiver already evaluated.
    ///
    /// Assigning to a field creates it if it is not there, which is the only way
    /// an instance ever gets one — there is no declaration form that allocates,
    /// so `init` assigning to `self.x` is what defines `x`.
    fn assign_field(
        &mut self,
        object: Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<Value> {
        match object {
            Value::Instance(id) => {
                // The same reach a read is checked against. Writing is not a
                // weaker claim than reading: a `private` field the outside could
                // assign to would be private in name only.
                self.may_reach(self.heap.instance(id).class, name, span)?;
                let key = Key::Str(Rc::from(name));
                let written = self
                    .heap
                    .instance_mut(id)
                    .map(|instance| instance.fields.insert(key, value.clone()));
                written.map_err(|_| frozen(&self.heap, &Value::Instance(id), span))?;
                Ok(value)
            }
            // `Type` and not `Attr`: an int has no fields at all, so the
            // receiver is the mistake rather than the name reached for.
            other => Err(QuinceError::new(
                format!("cannot set a field on {}", other.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "only an instance of a class has fields — a builtin carries its value \
                 and nothing beside it",
            )),
        }
    }
}

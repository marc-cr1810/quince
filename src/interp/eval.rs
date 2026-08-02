//! Evaluating expressions, and reading and writing the names they mention.
//!
//! `eval` is the tree walk itself; `read` and `assign` are the two ends of a name.
//!
//! The run-time type checks v0.7 puts on an assignment go in `assign`, which is
//! already the one place every write to a binding, a field, and an element passes
//! through.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError};
use crate::interp::error::frozen;
use crate::interp::index::key_of;
use crate::interp::{Attr, Interp, resolved};
use crate::runtime::dict::{Dict, Key};
use crate::runtime::env::{self, AssignError};
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::{BoundMethod, Value};
use crate::syntax::ast::{Expr, ExprKind, LogicalOp, Op, Slot, Var};
use crate::syntax::token::Span;

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
    ) -> Result<Vec<Value>, QuinceError> {
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
    ) -> Result<(Value, Value), QuinceError> {
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

    pub(super) fn eval(&mut self, expr: &Expr, env: ObjId) -> Result<Value, QuinceError> {
        match &expr.kind {
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
                if let ExprKind::Field { target, name } = &callee.kind {
                    return self.eval_method_call(target, name, args, env, callee.span, expr.span);
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
                        let values = self.eval_seq(args, env);
                        self.temps.truncate(mark);
                        return self.super_init(class, builtin, &receiver, values?, expr.span);
                    }

                    let method = self.super_method(class, name, callee.span)?;
                    let mark = self.temps.len();
                    self.temps.push(receiver.clone());
                    self.temps.push(method.clone());
                    let values = self.eval_seq(args, env);
                    self.temps.truncate(mark);
                    return self.call_method(receiver, method, values?, expr.span);
                }

                let target = self.eval(callee, env)?;
                // The callee is held across every argument, any of which can
                // reach a safe point, and a closure built by an expression is
                // reachable from nowhere else. Kept out of `eval_seq` so the
                // argument vector stays exactly as long as the call needs.
                let mark = self.temps.len();
                if target.handle().is_some() {
                    self.temps.push(target.clone());
                }
                let values = self.eval_seq(args, env);
                self.temps.truncate(mark);
                self.call(target, values?, expr.span)
            }

            ExprKind::Index { target, index } => {
                let (target, index) = self.eval_pair(target, index, env)?;
                self.index_get(&target, &index, expr.span)
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
            ExprKind::Field { target, name } => {
                let receiver = self.eval(target, env)?;
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
                let value = self.eval(value, env)?;
                self.assign(target, value, env)
            }
        }
    }

    /// Reads a variable through the slot the resolver assigned it.
    pub(super) fn read(&mut self, var: &Var, env: ObjId, span: Span) -> Result<Value, QuinceError> {
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
                })
            }
            Slot::Global => self
                .heap
                .globals(env::module_of(&self.heap, env))
                .get(&var.name)
                .cloned()
                .ok_or_else(|| {
                    let mut err =
                        QuinceError::new(format!("undefined variable `{}`", var.name), span)
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
                }),
        }
    }

    pub(super) fn assign(&mut self, target: &Expr, value: Value, env: ObjId) -> Result<Value, QuinceError> {
        match &target.kind {
            ExprKind::Var(var) => {
                match resolved(&var.slot) {
                    // The resolver already rejected assignment to a `const`
                    // local, so reaching a slot means it is writable.
                    Slot::Local { hops, index } => {
                        let scope = env::ancestor(&self.heap, env, hops);
                        self.heap.env_mut(scope).set(index, value.clone());
                    }
                    Slot::Global => {
                        let name = &var.name;
                        let module = env::module_of(&self.heap, env);
                        match self.heap.globals_mut(module).assign(name, value.clone()) {
                            Ok(()) => {}
                            Err(AssignError::Undefined) => {
                                return Err(QuinceError::new(
                                    format!("undefined variable `{name}`"),
                                    target.span,
                                )
                                .with_kind(ErrorKind::Name));
                            }
                            // `Frozen` rather than a kind of its own, because the
                            // class is named for the refusal and not for the
                            // mechanism: a `final` name and a `const` list are
                            // different things to pin, and both answer "that will
                            // not change" to whoever tried to change it.
                            //
                            // The local form of this is a `DeclarationError` from
                            // the resolver, which cannot see globals to give them
                            // the same treatment. Same sentence, two kinds — see
                            // Bindings.
                            Err(AssignError::Immutable) => {
                                return Err(QuinceError::new(
                                    format!("cannot reassign `{name}`"),
                                    target.span,
                                )
                                .with_kind(ErrorKind::Frozen));
                            }
                        }
                    }
                }
                Ok(value)
            }

            ExprKind::Index {
                target: collection,
                index,
            } => {
                // `value` was evaluated by the caller and is held across two
                // more evaluations, either of which can reach a safe point.
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let evaluated = self.eval_pair(collection, index, env);
                self.temps.truncate(mark);
                let (collection, index) = evaluated?;

                // The class first, and `const` before the class. A frozen object
                // must not get to *run* on a write that is already refused: an
                // `op set` that logged, counted or raised would have happened,
                // and the assignment it belonged to would still fail. Freezing is
                // a promise about the object a program holds, so it is checked on
                // the instance rather than on the payload underneath it.
                if let Some(method) = self.slot(&collection, Op::Set) {
                    if collection
                        .handle()
                        .is_some_and(|id| self.heap.is_frozen(id))
                    {
                        return Err(frozen(&self.heap, &collection, target.span));
                    }
                    // What the op answers is discarded: `x[i] = v` is worth `v`,
                    // the same as every other assignment in the language.
                    self.call_op(method, &collection, vec![index, value.clone()])?;
                    return Ok(value);
                }

                // Written through to the payload for a class extending `dict` or
                // `list`, so `bag['a'] = 1` reaches the dict the object *is*. The
                // `const` check still names `collection`, since freezing applies to
                // the object a program holds.
                match collection.base(&self.heap).clone() {
                    // Assigning to a missing key inserts it, where assigning
                    // past the end of a list stays an error: a list's indices
                    // are positions, and there is no meaningful gap to fill.
                    // The mutation happens inside the `map` so that the borrow
                    // it needs has ended by the time the error — which reads
                    // the heap to name the type — is built.
                    Value::Dict(id) => {
                        let key = key_of(&self.heap, &index, target.span)?;
                        let written = self
                            .heap
                            .dict_mut(id)
                            .map(|entries| entries.insert(key, value.clone()));
                        written.map_err(|_| frozen(&self.heap, &collection, target.span))?;
                    }
                    _ => {
                        let (id, offset) = self.list_index(&collection, &index, target.span)?;
                        let written = self
                            .heap
                            .list_mut(id)
                            .map(|items| items[offset] = value.clone());
                        written.map_err(|_| frozen(&self.heap, &collection, target.span))?;
                    }
                }
                Ok(value)
            }

            // Assigning to a field creates it if it is not there, which is the
            // only way an instance ever gets one — there is no declaration
            // form, so `init` assigning to `self.x` is what defines `x`.
            ExprKind::Field {
                target: object,
                name,
            } => {
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let object = self.eval(object, env);
                self.temps.truncate(mark);

                match object? {
                    Value::Instance(id) => {
                        let key = Key::Str(Rc::from(name.as_str()));
                        let written = self
                            .heap
                            .instance_mut(id)
                            .map(|instance| instance.fields.insert(key, value.clone()));
                        written
                            .map_err(|_| frozen(&self.heap, &Value::Instance(id), target.span))?;
                        Ok(value)
                    }
                    // `Type` and not `Attr`: an int has no fields at all, so the
                    // receiver is the mistake rather than the name reached for.
                    other => Err(QuinceError::new(
                        format!("cannot set a field on {}", other.type_name(&self.heap)),
                        target.span,
                    )
                    .with_kind(ErrorKind::Type)),
                }
            }

            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }
}

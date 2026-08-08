//! Executing statements.
//!
//! The half of the evaluator that produces control flow rather than a value.
//! `exec` is the one safe point the collector may run at, so everything that
//! reaches a statement boundary passes through here.
//!
//! v0.10's `match` arms and `if let` are statement forms and land beside the `If`
//! arm; the lazy iteration protocol it introduces replaces the body of `iterate`.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Result};
use crate::interp::call::Written;
use crate::interp::error::an;
use crate::interp::{Flow, Interp, resolved};
use crate::sema::overload;
use crate::runtime::class::Class;
use crate::runtime::env::{self, Env};
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::{Function, Value};
use crate::syntax::ast::{BindKind, Block, Expr, ImportNames, Op, Slot, Stmt, StmtKind, TypeExpr};

impl Interp {
    pub(super) fn exec(&mut self, stmt: &Stmt, env: ObjId) -> Result<Flow> {
        // The one safe point in the evaluator. Every statement passes through
        // here, and nothing above it on the Rust stack holds an unrooted value.
        self.collect_if_needed();

        match &stmt.kind {
            StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(Flow::Normal)
            }

            StmtKind::Let {
                name,
                name_span: _,
                value,
                defaulted: _,
                bind,
                ty,
                visibility: _,
                doc: _,
                slot,
            } => {
                let initializer = value;
                let mut value = self.eval(initializer, env)?;
                // Against the annotation before the name is bound, so a refused
                // value is never observable under the name that refused it.
                // The initializer goes too, so a report about one element of a
                // literal can underline that element rather than the statement.
                if let Some(ty) = ty {
                    value = self.coerced_from(
                        ty,
                        value,
                        &format!("`{name}`"),
                        initializer.span,
                        Written::declared(initializer),
                    )?;
                }
                // Freezing before binding, so that a `const` can never be
                // observed thawed — not that anything runs in between, but the
                // order is the thing a reader checks first.
                if bind.freezes() {
                    self.heap.freeze(&value);
                }
                // The annotation goes with the name, not with this one value:
                // every later write is checked against the same one, by the
                // same function, and widens the same way.
                //
                // Only the annotation. A `final` or `const` *binding* is already
                // refused a rebinding without one — by the resolver for a local,
                // by the `mutable` flag for a global — so there is nothing here
                // for the unannotated case to add. A `const` parameter is the
                // one that needs standing in for, and does it at the call.
                self.bind_typed(
                    slot,
                    name,
                    value,
                    bind.mutable(),
                    ty.clone().map(Rc::new),
                    env,
                );
                Ok(Flow::Normal)
            }

            StmtKind::Fn {
                decl,
                slot,
                overload,
            } => {
                // Declared before the body ever runs, and closing over the scope
                // it is declared in, so the function can call itself.
                let func = Value::Function(self.heap.alloc(Object::Function(Function {
                    decl: Rc::clone(decl),
                    env,
                    owner: None,
                })));
                // The second and later declarations of a name join the set the
                // first one bound. Which those are was decided at resolution —
                // see `StmtKind::Fn::overload` — so a REPL entry redefining a
                // function replaces it, and two `fn f` in one file do not.
                let value = match overload {
                    true => match self.held(slot, &decl.name, env) {
                        Some(earlier) => self.also(&earlier, func),
                        None => func,
                    },
                    // Not an overload *in this compilation*. In a file that is
                    // the whole story — the resolver refuses a name declared
                    // twice in one scope — but the REPL compiles a line at a
                    // time, so two overloads typed on two lines each arrive as a
                    // first declaration. Folding them makes the rule the same
                    // either way: a signature already bound is replaced, and one
                    // that is not joins what is there.
                    false => match self.held(slot, &decl.name, env) {
                        Some(earlier @ (Value::Function(_) | Value::Overload(_))) => {
                            self.folded(func, earlier)
                        }
                        _ => func,
                    },
                };
                self.bind(slot, &decl.name, value, false, env);
                Ok(Flow::Normal)
            }

            StmtKind::Class {
                doc: _,
                name,
                params,
                parent,
                parent_span,
                methods,
                fields,
                openness,
                visibility,
                slot,
            } => {
                // Read before the class's own name is bound, so `class A extends
                // A` is an undefined variable rather than a cycle in the chain
                // that `Class::method` walks.
                let parent = match parent {
                    Some(parent) => {
                        // Every refusal below is about the parent, so it is
                        // reported at the word that names it: the statement's own
                        // span reaches to the closing brace, and a caret under
                        // twenty lines names nothing.
                        let at = parent_span.expect("a parent is parsed with its span");
                        match self.read(parent, env, at)? {
                            // A builtin with no `init` is the one parent that cannot
                            // work, and for a reason that needs no second list to
                            // record: extending a builtin means `super.init` writes
                            // the value the subclass *is*, and where there is no
                            // conversion there is nothing for it to call. That rules
                            // out `function` and `class`, exactly the two that refuse
                            // to be constructed on their own.
                            Value::Class(id)
                                if self.heap.class(id).builtin.is_some()
                                    && self.heap.class(id).slot(Op::Init).is_none() =>
                            {
                                let builtin = self.heap.class(id).name.clone();
                                return Err(QuinceError::new(
                                    format!("`{name}` cannot extend `{builtin}`"),
                                    at,
                                )
                                .with_kind(ErrorKind::Type)
                                .with_help(format!(
                                    "there is no value a {builtin} could be made from, so `super.init` would have nothing to call"
                                )));
                            }
                            // One of the two doors. The other is `extend`, refused
                            // in `may_extend` — and the modifier is quoted back
                            // rather than named, since `final` and `sealed` both
                            // reach here and the program wrote one of them.
                            Value::Class(id)
                                if self.heap.class(id).openness.closes_inheritance() =>
                            {
                                let closed = self.heap.class(id).name.clone();
                                let word = self.heap.class(id).openness.word().unwrap_or_default();
                                return Err(QuinceError::new(
                                    format!("`{name}` cannot extend `{closed}`"),
                                    at,
                                )
                                .with_kind(ErrorKind::Type)
                                .with_help(format!(
                                    "`{closed}` is declared `{word}`, so it has no subclasses — `{name}` can hold a `{closed}`, but it cannot be one"
                                )));
                            }
                            Value::Class(id) => Some(id),
                            other => {
                                return Err(QuinceError::new(
                                    format!(
                                        "a class can only extend a class, but `{}` is {}",
                                        parent.name,
                                        other.type_name(&self.heap)
                                    ),
                                    at,
                                )
                                .with_kind(ErrorKind::Type)
        .with_help("`extends` names a class, and the name here holds something else"));
                            }
                        }
                    }
                    None => None,
                };

                // A subclass wraps its methods in a scope holding the parent, so
                // `super` is an ordinary local wherever it appears — including
                // inside a closure nested in a method. The resolver pushed the
                // matching scope, so the hop counts already account for it.
                let enclosing = match parent {
                    Some(id) => {
                        let scope = self.heap.alloc(Object::Env(Env::new(Some(env), 1)));
                        self.heap.env_mut(scope).set(0, Value::Class(id));
                        scope
                    }
                    None => env,
                };

                // Methods close over that scope, exactly as a `fn` at the same
                // position would. Nothing here reaches a safe point, so the
                // functions are safe unrooted until the class owning them exists.
                let mut table: HashMap<String, Value> = HashMap::with_capacity(methods.len());
                let mut slots = Class::empty_slots();
                // The functions this body declared, kept apart from the table:
                // once the class exists each learns it is theirs, and a method
                // inherited into an overload set below must *not* — its reach is
                // the class that wrote it.
                let mut declared: Vec<ObjId> = Vec::with_capacity(methods.len());
                for decl in methods {
                    let func = Value::Function(self.heap.alloc(Object::Function(Function {
                        decl: Rc::clone(decl),
                        env: enclosing,
                        // Filled in below: the class does not exist yet, and a
                        // method's reach is its own class's.
                        owner: None,
                    })));
                    declared.extend(func.handle());
                    // Several declarations may share a name — v0.8 §3.5 — and
                    // the resolver has already refused any pair a call could not
                    // tell apart. What lands in the table is the set.
                    let joined = match table.get(&decl.name) {
                        Some(earlier) => self.also(earlier, func),
                        None => func,
                    };
                    // Every op lands in the table by name *and* in its slot. The
                    // name is what `super.init(msg)` reaches; the slot is what
                    // `Point(1, 2)` and `if p` reach, without hashing anything.
                    if let Some(op) = decl.op {
                        slots[op.index()] = Some(joined.clone());
                    }
                    table.insert(decl.name.clone(), joined);
                }

                // An `override` replaces the one signature it matches, not the
                // whole set: a subclass overriding `add(other: Vector)` leaves
                // `add(scalar: float)` inherited and reachable. Done here rather
                // than at the lookup, because `Class::method` answers with the
                // first table that holds the name and cannot see past it.
                //
                // `op init` is the exception, as it is for `override`: every
                // constructor in a hierarchy replaces its parent's outright —
                // that is what `super.init` is for — so merging them would make
                // `Deep(message)` reach `Error`'s constructor on a `Deep`.
                if let Some(id) = parent {
                    for (name, own) in table.iter_mut() {
                        if name == Op::Init.name() {
                            continue;
                        }
                        let Some(inherited) = self.heap.class(id).method(name, &self.heap) else {
                            continue;
                        };
                        *own = self.folded(own.clone(), inherited);
                    }
                }

                let mut class = Class {
                    name: name.clone(),
                    methods: table,
                    parent,
                    slots,
                    builtin: None,
                    openness: *openness,
                    fields: fields.iter().cloned().map(Rc::new).collect(),
                    // The scope the initializers run in, which is the one the
                    // methods closed over — a field's initializer sees what a
                    // method body would see, `super` included.
                    field_env: (!fields.is_empty()).then_some(enclosing),
                    visibility: *visibility,
                    params: params.iter().map(|param| param.name.clone()).collect(),
                };

                // Inherited rather than searched for: a class that declares no
                // `op init` of its own constructs with its parent's, which is
                // what `class TypeError extends Error {}` relies on — and the
                // same copy is what lets a subclass inherit `op string` or
                // `op eq` without restating it.
                if let Some(id) = parent {
                    let inherited = self.heap.class(id).slots.clone();
                    // The same per-signature merge the method table just took,
                    // for the ops: a subclass that overrides one `op add` keeps
                    // the other. A slot the subclass did not fill at all is the
                    // plain copy-down `inherit_slots` does.
                    for op in crate::syntax::ast::OPS {
                        if *op == Op::Init {
                            continue;
                        }
                        let (Some(own), Some(above)) =
                            (class.slots[op.index()].clone(), inherited[op.index()].clone())
                        else {
                            continue;
                        };
                        class.slots[op.index()] = Some(self.folded(own, above));
                    }
                    class.inherit_slots(&inherited);
                }

                let class = self.heap.alloc(Object::Class(class));

                // Now that the class exists, every method it declared learns
                // which class that was. Done after rather than during, because a
                // method's owner is the class object and the class object cannot
                // be allocated before the table it holds.
                for id in declared {
                    self.heap.function_mut(id).owner = Some(class);
                }

                self.bind(slot, name, Value::Class(class), false, env);
                Ok(Flow::Normal)
            }

            // An alias names a type and holds no value, so there is nothing here
            // to run. The resolver has already substituted every use of it.
            StmtKind::Alias { .. } => Ok(Flow::Normal),

            StmtKind::Extend {
                target,
                target_span,
                methods,
            } => {
                let value = self.read(target, env, *target_span)?;
                let Value::Class(id) = value else {
                    return Err(QuinceError::new(
                        format!(
                            "only a type can be extended, but `{}` is {}",
                            target.name,
                            an(value.type_name(&self.heap))
                        ),
                        *target_span,
                    )
                    .with_kind(ErrorKind::Type)
        .with_help("`extend` adds methods to a type, so the name after it has to hold one"));
                };

                // Every name checked before any is added, so a block whose third
                // method collides leaves the type exactly as it found it. A
                // half-applied extension would be the worst of both: a program
                // that reported an error and changed behaviour anyway.
                for decl in methods {
                    self.may_extend(id, decl, *target_span)?;
                }

                // Nothing here reaches a safe point, so the functions are safe
                // unrooted until the table holds them — and the table is a root,
                // which is what keeps them alive after that.
                for decl in methods {
                    let func = Value::Function(self.heap.alloc(Object::Function(Function {
                        decl: Rc::clone(decl),
                        env,
                        // Left `None`: an `extend` block is outside the class it
                        // adds to, so it reaches what any outsider reaches.
                        owner: None,
                    })));
                    // Overloads coexist across `extend` blocks as well as inside
                    // one, which is what §3.5 means by "extensions coexist
                    // across modules": what is already there under the name is
                    // joined rather than replaced, and `may_extend` above is what
                    // refused any pair a call could not tell apart.
                    let key = (id, decl.name.clone());
                    let joined = match self.extensions.get(&key) {
                        Some(earlier) => self.also(&earlier.clone(), func),
                        None => func,
                    };
                    self.extensions.insert(key, joined.clone());
                    if let Some(op) = decl.op {
                        self.heap.class_mut(id).slots[op.index()] = Some(joined);
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::Import {
                module,
                module_span,
                names,
            } => {
                let loaded = self.load_module(module, env, *module_span)?;
                let into = env::module_of(&self.heap, env);

                match names {
                    ImportNames::Module => {
                        self.heap
                            .globals_mut(into)
                            .declare(module, Value::Module(loaded), false);
                    }
                    ImportNames::Names(names) => {
                        // Every name read before any is bound, so a list whose
                        // third entry is not there leaves the scope exactly as it
                        // found it. The same rule `extend` follows, and for the
                        // same reason: a statement that reports an error and
                        // changes the program anyway is the worst of both.
                        let mut values = Vec::with_capacity(names.len());
                        for name in names {
                            let value = self
                                .heap
                                .globals(loaded)
                                .get(&name.name)
                                .cloned()
                                .ok_or_else(|| {
                                    self.not_in_module(module, &name.name, name.span, loaded)
                                })?;
                            self.may_import(loaded, &name.name, name.span)?;
                            values.push(value);
                        }
                        for (name, value) in names.iter().zip(values) {
                            self.heap
                                .globals_mut(into)
                                .declare(&name.name, value, false);
                        }
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = self.eval(cond, env)?;
                if self.is_truthy(&cond)? {
                    self.exec_block(then, env)
                } else if let Some(other) = otherwise {
                    self.exec(other, env)
                } else {
                    Ok(Flow::Normal)
                }
            }

            StmtKind::While { cond, body } => {
                loop {
                    let cond = self.eval(cond, env)?;
                    if !self.is_truthy(&cond)? {
                        break;
                    }
                    if let Flow::Return(value) = self.exec_block(body, env)? {
                        return Ok(Flow::Return(value));
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::For {
                iter, body, slot, ..
            } => self.exec_for(slot, iter, body, env),

            StmtKind::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval(expr, env)?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(value))
            }

            StmtKind::Try {
                body,
                handler,
                slot,
                ..
            } => self.exec_try(body, handler, slot, env),

            StmtKind::Throw(value) => {
                // Evaluating the operand can fail for reasons of its own, and that
                // error is the one to report — not whatever `throw` would have made
                // of a value it never got.
                let raised = self.eval(value, env)?;
                Err(self.throw(raised, stmt.span))
            }

            StmtKind::Block(block) => self.exec_block(block, env),
        }
    }

    /// Runs a `try` block, handing an error to its handler.
    ///
    /// `catch` needs no unwinding machinery of its own. Every site that pushes a
    /// scope, a temp, or a frame binds the result before it pops rather than
    /// propagating with `?`, so by the time a handler runs, `scopes`, `temps`, and
    /// `depth` are already back to their depth at the `try`. That discipline
    /// exists for the collector, and this feature is affordable because of it.
    ///
    /// What changes is not the correctness but the consequence of getting it
    /// wrong: an error used to be fatal, so a site that forgot to restore leaked
    /// roots into a process about to exit. A caught error resumes with those
    /// stacks still deep, which turns the same latent bug into unbounded growth —
    /// see `a_loop_that_catches_does_not_grow_the_heap`.
    pub(super) fn exec_try(
        &mut self,
        body: &Block,
        handler: &Block,
        slot: &Option<Slot>,
        env: ObjId,
    ) -> Result<Flow> {
        let err = match self.exec_block(body, env) {
            // A `return` inside a `try` travels as `Flow::Return`, a value in the
            // `Ok` channel, and errors travel in the `Err` channel. It passes
            // through untouched because it is not the kind of thing a handler can
            // see. `finally` is precisely the feature that would force those two
            // channels to meet, and there deliberately is none.
            Ok(flow) => return Ok(flow),
            Err(err) => err,
        };

        let index = match resolved(slot) {
            Slot::Local { index, .. } => index,
            Slot::Global => unreachable!("a catch binding is always a local"),
        };

        // A thrown payload crossed the unwind rooted by nothing, surviving only
        // because collection happens between statements and unwinding executes
        // none. Nothing between here and the `set` below reaches a safe point —
        // `alloc` does not collect — so it is still alive to be bound. A
        // `finally` would have run statements during that unwind, and this is the
        // line where that would have become a use-after-free.
        let caught = self.reify(&err);
        let scope = self
            .heap
            .alloc(Object::Env(Env::new(Some(env), handler.slot_count)));
        self.heap.env_mut(scope).set(index, caught);
        self.exec_scoped(&handler.stmts, scope)
    }

    pub(super) fn exec_for(
        &mut self,
        slot: &Option<Slot>,
        iter: &Expr,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow> {
        let iterable = self.eval(iter, env)?;

        // A class says what it iterates as by answering with a list — eager, and
        // the whole list at once, which is the same shape the loop already gets
        // from a list or a dict below. See Iteration in DESIGN.md for why there
        // is no lazier form to be had here yet.
        let items = if let Some(method) = self.slot(&iterable, Op::Iter) {
            let answer = self.call_op(method, &iterable, Vec::new())?;
            match answer.base(&self.heap) {
                Value::List(id) => {
                    let id = *id;
                    self.heap.list(id).clone()
                }
                got => return Err(self.op_returned(Op::Iter, &iterable, "a list", got)),
            }
        } else {
            // Otherwise a class extending `list` or `dict` iterates as one. A
            // string is not iterable to begin with — `chars` is how its
            // characters are reached — so neither is a class extending it, which
            // is the consistent answer rather than a gap.
            match iterable.base(&self.heap) {
                // Snapshotted, so mutating the collection inside the loop cannot
                // invalidate the iteration.
                Value::List(id) => self.heap.list(*id).clone(),
                // A dict iterates over its keys, as in Python. Its values are the
                // half you can already reach, through `d[k]`.
                Value::Dict(id) => self.heap.dict(*id).keys().collect(),
                _ => {
                    return Err(QuinceError::new(
                        format!("cannot iterate over {}", iterable.type_name(&self.heap)),
                        iter.span,
                    )
                    .with_kind(ErrorKind::Type)
                    .with_help("only lists and dicts can be iterated in a for loop"));
                }
            }
        };

        // The snapshot is the one place a Rust frame holds values across whole
        // statements. It has to be rooted: the loop body may drop the original
        // list, and then nothing on the heap refers to these items at all.
        let mark = self.temps.len();
        self.temps.extend(items.iter().cloned());
        let result = self.iterate(slot, items, body, env);
        self.temps.truncate(mark);
        result
    }

    pub(super) fn iterate(
        &mut self,
        slot: &Option<Slot>,
        items: Vec<Value>,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow> {
        let index = match resolved(slot) {
            Slot::Local { index, .. } => index,
            Slot::Global => unreachable!("a loop variable is always a local"),
        };
        for item in items {
            // A fresh scope per iteration, so a closure made inside the loop
            // captures that iteration's value rather than sharing one binding.
            let scope = self
                .heap
                .alloc(Object::Env(Env::new(Some(env), body.slot_count)));
            self.heap.env_mut(scope).set(index, item);
            if let Flow::Return(value) = self.exec_scoped(&body.stmts, scope)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Normal)
    }

    pub(super) fn exec_block(&mut self, block: &Block, env: ObjId) -> Result<Flow> {
        let scope = self
            .heap
            .alloc(Object::Env(Env::new(Some(env), block.slot_count)));
        self.exec_scoped(&block.stmts, scope)
    }

    /// The set `later` joins, given what the name already holds here.
    ///
    /// Both are declarations of this class's own, so neither replaces the other
    /// — the resolver refused any pair a call could not tell apart.
    fn also(&mut self, earlier: &Value, later: Value) -> Value {
        let mut candidates = match earlier {
            Value::Overload(id) => self.heap.overload(*id).to_vec(),
            other => vec![other.clone()],
        };
        candidates.push(later);
        Value::Overload(self.heap.alloc(Object::Overload(candidates)))
    }

    /// What a name reaches once `own` is laid over `existing`.
    ///
    /// Signature by signature: a declaration in `own` replaces the one in
    /// `existing` it matches and leaves the rest reachable. `own` unchanged is
    /// the answer whenever there is nothing left over, which is the
    /// overwhelmingly common case.
    ///
    /// Two callers, and the rule is the same for both: a subclass laying its
    /// methods over the ones it inherits, and a REPL line laying a declaration
    /// over the one an earlier line bound.
    ///
    /// A native is never merged into. A builtin's methods are a static table
    /// with no annotations to dispatch on, so a subclass declaring a method of
    /// that name replaces it outright — which is what `override` already said.
    fn folded(&mut self, own: Value, existing: Value) -> Value {
        let mine = match &own {
            Value::Overload(id) => self.heap.overload(*id).to_vec(),
            other => vec![other.clone()],
        };
        let above = match &existing {
            Value::Overload(id) => self.heap.overload(*id).to_vec(),
            other => vec![other.clone()],
        };
        let signatures: Vec<Rc<crate::syntax::ast::FnDecl>> = mine
            .iter()
            .filter_map(|value| self.declaration(value))
            .collect();
        let kept: Vec<Value> = above
            .into_iter()
            .filter(|value| match self.declaration(value) {
                Some(theirs) => !signatures
                    .iter()
                    .any(|ours| overload::clash(ours, &theirs).is_some()),
                None => false,
            })
            .collect();
        if kept.is_empty() {
            return own;
        }
        let candidates = mine.into_iter().chain(kept).collect();
        Value::Overload(self.heap.alloc(Object::Overload(candidates)))
    }

    /// The declaration behind a candidate, for anything comparing signatures.
    pub(super) fn declaration(&self, value: &Value) -> Option<Rc<crate::syntax::ast::FnDecl>> {
        match value {
            Value::Function(id) => Some(Rc::clone(&self.heap.function(*id).decl)),
            _ => None,
        }
    }

    /// What the slot the resolver picked already holds, if anything.
    fn held(&self, slot: &Option<Slot>, name: &str, env: ObjId) -> Option<Value> {
        match resolved(slot) {
            Slot::Local { index, .. } => self.heap.env(env).get(index).cloned(),
            Slot::Global => self
                .heap
                .globals(env::module_of(&self.heap, env))
                .get(name)
                .cloned(),
        }
    }

    /// Stores a freshly declared value in the slot the resolver picked for it.
    pub(super) fn bind(&mut self, slot: &Option<Slot>, name: &str, value: Value, mutable: bool, env: ObjId) {
        self.bind_typed(slot, name, value, mutable, None, env)
    }

    /// The same, recording what the declaration said the name holds.
    ///
    /// The annotation outlives the declaration because it is about the name and
    /// not about the first value bound to it: without this, `let x: int = 0`
    /// followed by `x = "s"` has nothing to check the second line against.
    pub(super) fn bind_typed(
        &mut self,
        slot: &Option<Slot>,
        name: &str,
        value: Value,
        mutable: bool,
        ty: Option<Rc<TypeExpr>>,
        env: ObjId,
    ) {
        match resolved(slot) {
            Slot::Local { index, .. } => {
                let bind = match mutable {
                    true => BindKind::Let,
                    false => BindKind::Final,
                };
                self.heap.env_mut(env).declare(index, value, ty, bind)
            }
            Slot::Global => {
                let module = env::module_of(&self.heap, env);
                self.heap
                    .globals_mut(module)
                    .declare_typed(name, value, mutable, ty)
            }
        }
    }

    /// Runs `stmts` in `scope`, keeping the scope rooted for as long as it is
    /// on the Rust stack.
    ///
    /// Every scope is created and entered here, which is what makes the root
    /// set complete — a scope allocated anywhere else would be collected out
    /// from under the frame using it.
    pub(super) fn exec_scoped(&mut self, stmts: &[Stmt], scope: ObjId) -> Result<Flow> {
        self.scopes.push(scope);
        let result = self.exec_stmts(stmts, scope);
        self.scopes.pop();
        result
    }

    pub(super) fn exec_stmts(&mut self, stmts: &[Stmt], env: ObjId) -> Result<Flow> {
        for stmt in stmts {
            if let Flow::Return(value) = self.exec(stmt, env)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Normal)
    }

}

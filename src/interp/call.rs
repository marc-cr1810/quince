//! Calling things, and constructing them.
//!
//! A function, a native, a bound method, a class: four callees and one entry
//! point. Arity checking and the receiver-prepending that makes a method call
//! work sit here too.
//!
//! v0.8's overload dispatch is a change to this file and to almost nothing else —
//! choosing among several declarations sharing a name happens where the argument
//! values are already in hand.


use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::interp::error::{an, check_arity, does_not_hold, frozen};
use crate::interp::{Attr, Flow, Interp, MAX_DEPTH, out_of_stack};
use crate::runtime::class::{BUILTINS as BUILTIN_TYPES, Builtin, Instance};
use crate::runtime::dict::{Dict, Key};
use crate::runtime::env::Env;
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::Value;
use crate::sema::types::holds;
use crate::syntax::ast::{Expr, Op, TypeExpr, TypeName};
use crate::syntax::token::Span;

impl Interp {
    pub(crate) fn call(&mut self, target: Value, args: Vec<Value>, span: Span) -> Result<Value> {
        match target {
            Value::Native(native) => {
                if let Some(arity) = native.arity {
                    check_arity(native.name, arity, args.len(), span)?;
                }
                // `args` lives in this frame and nothing roots it. That is safe
                // only while no builtin reaches a safe point; the first one to
                // call back into Quince has to root them here first.
                (native.func)(self, &args, span)
            }

            Value::Function(id) => {
                let func = self.heap.function(id).clone();
                check_arity(&func.decl.name, func.decl.params.len(), args.len(), span)?;

                if self.depth >= MAX_DEPTH {
                    return Err(QuinceError::new(
                        format!("recursion limit of {MAX_DEPTH} calls exceeded"),
                        span,
                    )
                    .with_kind(ErrorKind::Recursion));
                }

                // The same refusal for the same reason, arrived at differently:
                // `MAX_DEPTH` counts calls, and this measures what a call was
                // standing in for. Both are here because each catches what the
                // other cannot — a count is the same everywhere and so is worth
                // being the documented limit, and a measurement is the only thing
                // that sees an expensive shape run out early.
                if out_of_stack() {
                    return Err(QuinceError::new("recursion is too deep to continue", span)
                        .with_kind(ErrorKind::Recursion)
                        .with_help(
                            "this nests further than the interpreter can follow — the recursion \
                             needs a base case that stops sooner",
                        ));
                }

                // Parameters are the body scope's first slots, in order, so
                // binding them needs no names at all.
                let scope = self.heap.alloc(Object::Env(Env::new(
                    Some(func.env),
                    func.decl.body.slot_count,
                )));
                for (index, mut arg) in args.into_iter().enumerate() {
                    // Against the parameter's annotation, at the boundary the
                    // value actually crosses. The span is the call's, because
                    // that is where the wrong value was written — the
                    // declaration is right and is somewhere else.
                    if let Some(ty) = func.decl.params.get(index).and_then(|p| p.ty.clone()) {
                        let named = format!("`{}`", func.decl.params[index].name);
                        arg = self.coerced(&ty, arg, &named, span)?;
                    }
                    self.heap.env_mut(scope).set(index as u16, arg);
                }

                self.depth += 1;
                self.reaching.push(func.owner);
                let result = self.exec_scoped(&func.decl.body.stmts, scope);
                self.reaching.pop();
                self.depth -= 1;

                // The one place a call into another module's code comes back
                // out, and so the one place that can say whose text the spans in
                // an error belong to. A function carries the scope it was
                // defined in, so this is right however far the value travelled
                // before it was called.
                let result = result.map_err(|err| match self.module_source(func.env) {
                    Some(source) => err.in_module(source),
                    None => err,
                });

                let mut produced = match result? {
                    Flow::Return(value) => value,
                    Flow::Normal => Value::Nil,
                };
                // A declared return is checked on the way out, which catches the
                // implicit `nil` a function that falls off its end produces —
                // the case an annotation most often exists to rule out.
                if let Some(ty) = func.decl.returns.clone() {
                    let named = format!("`{}`\u{2019}s return", func.decl.name);
                    produced = self.coerced(&ty, produced, &named, span)?;
                }
                Ok(produced)
            }

            Value::BoundMethod(id) => {
                let bound = self.heap.bound_method(id).clone();
                self.call_method(bound.receiver, bound.method, args, span)
            }

            // Calling a class builds an instance and hands it to `init`, which
            // is why `init` returns nothing useful: the object already exists
            // by the time it runs.
            Value::Class(id) => {
                // Except for a builtin, where there is no object to build. An
                // int has nowhere to keep a field, so its `init` returns the
                // value instead of filling one in, and the call yields that. The
                // difference is only in what construction *produces* — the class
                // is a class like any other, and its `init` is found the same way.
                if let Some(builtin) = self.heap.class(id).builtin {
                    return self.construct_builtin(id, builtin, args, span);
                }

                let instance_id = self.heap.alloc(Object::Instance(Instance {
                    class: id,
                    fields: Dict::new(),
                    // Filled below, either by an `op init` calling `super.init` or
                    // by the conversion this class inherited. Not here, because
                    // only the constructor knows what value to convert.
                    payload: None,
                }));
                let instance = Value::Instance(instance_id);

                // Declared fields, before `op init` — so an `init` assigning one
                // overwrites a value that is already there, which is what makes
                // `let balance = 0` followed by `self.balance = opening` read the
                // way it looks. Rooted through `temps`, because an initializer is
                // an arbitrary expression and may reach a safe point — the same
                // reason the `op init` case below pushes the instance.
                let mark = self.temps.len();
                self.temps.push(instance.clone());
                let initialized = self.init_fields(id, instance_id, span);
                self.temps.truncate(mark);
                initialized?;

                // The `op init` the class resolved when it was built, not a
                // lookup of the name `init` — a method merely *called* `init` is
                // an ordinary method, and construction must not reach it.
                match self.heap.class(id).slot(Op::Init).cloned() {
                    // A native here is a conversion, inherited by a class that
                    // declares no `op init` of its own: nothing was written to run,
                    // so construction does what an `op init` forwarding to
                    // `super.init` would have done. `Username("marc")` needs no
                    // constructor to be a string, and writing the one that only
                    // forwards is boilerplate the language can supply.
                    Some(init @ Value::Native(_)) => {
                        let builtin = self
                            .builtin_base(id)
                            .expect("only a builtin's seed puts a native in `init`");
                        self.set_payload(instance_id, init, builtin, args, span)
                            .map_err(|err| {
                                // The arity comes from the conversion, so it says
                                // `string` where the call site said `Username`.
                                // True, and worth saying rather than rewriting:
                                // this class is built from what its base is.
                                let name = self.heap.class(id).name.clone();
                                err.with_help(format!(
                                    "`{name}` extends `{}`, so it is built from whatever a {} is",
                                    builtin.name(),
                                    builtin.name()
                                ))
                            })?;
                    }
                    // `init` runs Quince code, so it reaches safe points, and
                    // the instance needs no root here: it sits in slot 0 of the
                    // constructor's scope, which `exec_scoped` roots for the
                    // whole body.
                    //
                    // That holds only because `self` cannot be reassigned — the
                    // resolver refuses it, see `Param::receiver`. This used to
                    // push the instance onto `temps` for exactly that reason,
                    // and the root came out when the language rule went in.
                    Some(init) => {
                        self.call_method(instance.clone(), init, args, span)?;
                    }
                    // No constructor, so the only correct call passes nothing.
                    None => {
                        let name = self.heap.class(id).name.clone();
                        // Marking is what makes a constructor a constructor, so
                        // a plain `fn init` is the likeliest reason to be here
                        // with arguments in hand. Saying so turns the one mistake
                        // the marking rule allows into an instruction.
                        let unmarked = self
                            .heap
                            .class(id)
                            .method(Op::Init.name(), &self.heap)
                            .is_some();
                        check_arity(&name, 0, args.len(), span).map_err(|err| {
                            if unmarked {
                                err.with_help(format!(
                                    "`{name}` has a method `init`, but only `op init` runs when a class is constructed"
                                ))
                            } else {
                                err
                            }
                        })?;
                    }
                }
                Ok(instance)
            }

            other => Err(QuinceError::new(
                format!("{} is not callable", other.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("only functions and classes can be called")),
        }
    }

    /// Calling a builtin type: `int("42")`, `list()`.
    ///
    /// No instance is allocated. A builtin's `init` is a conversion, so it takes
    /// the call's arguments and nothing else, and the value it returns is the
    /// result of the call. That is why this reaches `call` rather than
    /// `call_method` — there is no receiver to insert.
    pub(super) fn construct_builtin(
        &mut self,
        id: ObjId,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // One argument is a conversion of that argument, and its class is what
        // gets to say what it converts to. Asked here rather than in each of the
        // six natives, because it is one question — and asked before them, so an
        // `op int` beats the payload underneath it, which is the whole point of
        // declaring one.
        //
        // `string` and `bool` would have reached their op anyway, through
        // `display` and `is_truthy`. This just gets there first, with the same
        // answer.
        if let [arg] = args.as_slice()
            && let Some(op) = builtin.conversion()
            && let Some(method) = self.slot(arg, op)
        {
            let arg = arg.clone();
            let produced = self.call_op(method, &arg, Vec::new())?;
            // An `op int` answering with a string would make `int(x)` return
            // something that is not an int, which nothing downstream is written
            // to survive.
            if produced.base(&self.heap).class(&self.heap) != self.heap.builtin_class(builtin) {
                let expected = an(builtin.name());
                return Err(self.op_returned(op, &arg, &expected, &produced));
            }
            return Ok(produced);
        }

        let Some(init) = self.heap.class(id).slot(Op::Init).cloned() else {
            // Only `function` reaches this: `nil` and `class` are keywords, so
            // neither is bound as a global and nothing can name them to call.
            return Err(
                QuinceError::new(format!("cannot make {}", an(builtin.name())), span)
                    .with_kind(ErrorKind::Type)
                    .with_help(format!(
                        "there is no value a {} could be made from — `fn` is how one is written",
                        builtin.name()
                    )),
            );
        };
        self.call(init, args, span)
    }

    /// `super.init(…)` where the superclass is a builtin: the conversion runs,
    /// and what it produces becomes the receiver's payload.
    ///
    /// The conversion is the same one `string(x)` reaches, so the arities and the
    /// errors are the ones already written — `super.init("abc")` on an `int`
    /// ancestor raises the same `ValueError` as `int("abc")` does, at the
    /// `super.init` rather than at the constructor call.
    pub(super) fn super_init(
        &mut self,
        class: ObjId,
        builtin: Builtin,
        receiver: &Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        let Value::Instance(id) = receiver else {
            unreachable!("`super` binds the enclosing method's receiver, always an instance");
        };
        let init = self
            .heap
            .class(class)
            .slot(Op::Init)
            .cloned()
            .expect("a builtin reached as a superclass is one that converts");
        self.set_payload(*id, init, builtin, args, span)
    }

    /// Runs a conversion and keeps what it produced as `id`'s payload.
    ///
    /// The value as the annotation `ty` says it should be, or a refusal.
    ///
    /// Three things in the order §3.3 and §4.1 need them: the check, so a
    /// refusal reports the value the program wrote; the widening, because
    /// `let x: float = 0` has to *store* a float or `type(x)` would disagree
    /// with the annotation next to it; and the freeze, at the boundary the value
    /// crosses.
    ///
    /// Hands the value back rather than checking in place, because the widening
    /// makes this a conversion and not only a test — and a caller that bound the
    /// original would have an `int` under an annotation reading `float`.
    pub(super) fn coerced(
        &mut self,
        ty: &TypeExpr,
        value: Value,
        what: &str,
        span: Span,
    ) -> Result<Value> {
        // A name that is not a type at all is a different mistake from a value
        // that does not hold, and reporting it as the second blames the value
        // for the annotation being wrong. Checked here rather than at
        // resolution because that is where the answer is: a class is an
        // ordinary binding, and which ones exist is not known until they run.
        if let TypeName::Named(name) = &ty.name
            && !self.names_a_type(name)
        {
            return Err(self.no_such_type(name, ty.span));
        }
        if !holds(ty, &value, &self.heap) {
            // A container that failed only on its contents gets a report about
            // the element rather than about itself — "this is a list" when a
            // list was asked for says nothing. Rewalked rather than threaded out
            // of `holds`, because this runs once, on the way to an error.
            if let Some(precise) = self.offending_element(ty, &value, span) {
                return Err(precise);
            }
            return Err(does_not_hold(&self.heap, ty, &value, what, span));
        }
        // Elements widen too, or `let xs: list[float] = [1, 2]` would hold ints
        // under an annotation reading `float` — the same contradiction §4.1
        // rules out for a plain binding, one level down.
        let value = self.widen_elements(ty, value, span)?;
        // The one widening §4.1 admits, made real. Narrowing is not symmetric
        // and is not here: `let n: int = 3.7` would have to choose a rounding,
        // and `int(x)` is how a program says which.
        let value = match (&ty.name, &value) {
            (TypeName::Named(name), Value::Int(n)) if name == "float" => Value::Float(*n as f64),
            _ => value,
        };
        // The container remembers what it was checked as, so a later `push` or
        // index-set can be refused without re-walking it. §3.9's reified header.
        if !ty.args.is_empty()
            && let Some(id) = value.base(&self.heap).handle()
        {
            self.heap.describe(id, Rc::new(ty.clone()));
        }
        // `const T` freezes deeply, exactly as a `const` binding does — the same
        // word at a place a binding cannot reach.
        if ty.frozen {
            self.heap.freeze(&value);
        }
        Ok(value)
    }

    /// The report for the element that made a container fail, if one did.
    ///
    /// `None` when the container itself was the mistake — a dict where a list
    /// was asked for — which is the case the caller already words well.
    fn offending_element(&mut self, ty: &TypeExpr, value: &Value, span: Span) -> Option<Raised> {
        let name = match &ty.name {
            TypeName::Named(name) => name.as_str(),
            TypeName::Any => return None,
        };
        match (name, value.base(&self.heap).clone()) {
            ("list", Value::List(id)) => {
                let arg = ty.args.first()?;
                let items = self.heap.list(id).clone();
                let (index, item) = items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| !holds(arg, item, &self.heap))?;
                Some(does_not_hold(
                    &self.heap,
                    arg,
                    item,
                    &format!("item {index}"),
                    span,
                ))
            }
            ("dict", Value::Dict(id)) => {
                let dict = self.heap.dict(id).clone();
                if let Some(arg) = ty.args.first()
                    && let Some(key) = dict.keys().find(|key| !holds(arg, key, &self.heap))
                {
                    return Some(does_not_hold(&self.heap, arg, &key, "the key", span));
                }
                let arg = ty.args.get(1)?;
                let held = dict.values().find(|held| !holds(arg, held, &self.heap))?;
                Some(does_not_hold(&self.heap, arg, held, "the value", span))
            }
            _ => None,
        }
    }

    /// Rewrites a container's elements to the types its annotation states.
    ///
    /// Only the §4.1 widening has anything to do — every other element already
    /// holds by the time this runs. In place, because the value the program
    /// holds is the one that has to change: a copy would leave the original
    /// reachable through every other name for it.
    fn widen_elements(&mut self, ty: &TypeExpr, value: Value, span: Span) -> Result<Value> {
        let name = match &ty.name {
            TypeName::Named(name) => name.as_str(),
            TypeName::Any => return Ok(value),
        };
        match (name, value.base(&self.heap).clone()) {
            ("list", Value::List(id)) => {
                let Some(arg) = ty.args.first().cloned() else {
                    return Ok(value);
                };
                let items = self.heap.list(id).clone();
                let mut widened = Vec::with_capacity(items.len());
                for item in items {
                    widened.push(self.coerced(&arg, item, "the item", span)?);
                }
                if let Ok(held) = self.heap.list_mut(id) {
                    *held = widened;
                }
            }
            ("dict", Value::Dict(id)) => {
                let Some(arg) = ty.args.get(1).cloned() else {
                    return Ok(value);
                };
                let dict = self.heap.dict(id).clone();
                let mut widened = Vec::new();
                for (key, held) in dict.iter() {
                    widened.push((key.clone(), self.coerced(&arg, held.clone(), "the value", span)?));
                }
                if let Ok(held) = self.heap.dict_mut(id) {
                    for (key, value) in widened {
                        held.insert(key, value);
                    }
                }
            }
            _ => {}
        }
        Ok(value)
    }

    /// Refuses a value about to be put into a described container.
    ///
    /// The other half of the reified header: [`Self::coerced`] checks a
    /// container's contents once when it crosses an annotated boundary, and this
    /// is what keeps them true afterwards. `slot` picks which argument applies —
    /// a list's element, a dict's key, or a dict's value.
    ///
    /// Answers the value back, because the same §4.1 widening applies: pushing
    /// an int onto a `list[float]` stores the float.
    pub(crate) fn admitted(
        &mut self,
        container: ObjId,
        slot: usize,
        value: Value,
        what: &str,
        span: Span,
    ) -> Result<Value> {
        let Some(descriptor) = self.heap.descriptor(container) else {
            return Ok(value);
        };
        let Some(ty) = descriptor.args.get(slot) else {
            return Ok(value);
        };
        if !holds(ty, &value, &self.heap) {
            return Err(does_not_hold(&self.heap, ty, &value, what, span));
        }
        Ok(match (&ty.name, &value) {
            (TypeName::Named(name), Value::Int(n)) if name == "float" => Value::Float(*n as f64),
            _ => value,
        })
    }

    /// Whether `name` names a class that exists.
    ///
    /// The builtins, and whatever the program bound at the top level. A class
    /// declared inside a function and used as an annotation in the same scope
    /// is not found — which is a gap, and the honest one: this has no scope in
    /// hand, and the alternative is threading one through every check for a
    /// shape nothing in the corpus writes.
    fn names_a_type(&self, name: &str) -> bool {
        if BUILTIN_TYPES.iter().any(|builtin| builtin.name() == name) {
            return true;
        }
        matches!(
            self.heap.globals(self.globals).get(name),
            Some(Value::Class(_))
        )
    }

    /// Reports an annotation naming something that is not a type.
    fn no_such_type(&self, name: &str, span: Span) -> crate::error::Raised {
        let mut known: Vec<&str> = BUILTIN_TYPES.iter().map(|builtin| builtin.name()).collect();
        let declared: Vec<String> = self
            .heap
            .globals(self.globals)
            .iter()
            .filter(|(_, value)| matches!(value, Value::Class(_)))
            .map(|(key, _)| key.to_string())
            .collect();
        known.extend(declared.iter().map(String::as_str));

        let err = QuinceError::new(format!("there is no type called `{name}`"), span)
            .with_kind(ErrorKind::Name);
        match crate::error::did_you_mean(name, known) {
            Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
            None => err,
        }
    }

    /// Runs the initializer of every field the class and its ancestors declared.
    ///
    /// Ancestors first, so a subclass redeclaring a name writes last and wins —
    /// the same order [`Class::field`] reads them back in, which is what keeps
    /// the value an instance holds and the declaration a report names in step.
    ///
    /// The chain is collected before anything runs, because an initializer is
    /// arbitrary Quince code: it may allocate, collect, or construct another
    /// instance of the same class, and none of that may happen while a borrow of
    /// the class is being held.
    fn init_fields(&mut self, class: ObjId, instance: ObjId, span: Span) -> Result<()> {
        let mut chain = Vec::new();
        let mut current = Some(class);
        while let Some(id) = current {
            chain.push(id);
            current = self.heap.class(id).parent;
        }

        for id in chain.into_iter().rev() {
            let declared = self.heap.class(id).fields.clone();
            let Some(env) = self.heap.class(id).field_env else {
                continue;
            };
            for field in declared {
                let mut value = self.eval(&field.value, env)?;
                if let Some(ty) = field.ty.clone() {
                    let named = format!("`{}`", field.name);
                    value = self.coerced(&ty, value, &named, span)?;
                }
                if field.bind.freezes() {
                    self.heap.freeze(&value);
                }
                let key = Key::Str(Rc::from(field.name.as_str()));
                let written = self
                    .heap
                    .instance_mut(instance)
                    .map(|held| held.fields.insert(key, value));
                written.map_err(|_| frozen(&self.heap, &Value::Instance(instance), span))?;
            }
        }
        Ok(())
    }

    /// Both ways a payload comes to exist end here: an explicit `super.init(…)`,
    /// and the implicit construction a class declaring no `op init` gets. They are
    /// the same operation on purpose — an implicit `op init` is not a second rule,
    /// only the observation that a class inheriting a conversion as its
    /// constructor should run it as one.
    pub(super) fn set_payload(
        &mut self,
        id: ObjId,
        init: Value,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // Checked before the conversion runs, so a second write is refused on the
        // strength of the first rather than after quietly replacing it. The
        // resolver requires that *a* `super.init` is written but cannot say how
        // many run — a call in each arm of an `if` is one call — so this is where
        // "once" is actually enforced.
        if self.heap.instance(id).payload.is_some() {
            let name = self.heap.class(self.heap.instance(id).class).name.clone();
            return Err(QuinceError::new(
                format!("`super.init` was already called for this {name}"),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "{} is given its {} once, and this would replace it",
                an(&name),
                builtin.name()
            )));
        }

        // A conversion is a native and natives reach no safe point, so the
        // instance behind `id` cannot move or be collected across this call.
        let value = self.call(init, args, span)?;
        match self.heap.instance_mut(id) {
            Ok(instance) => {
                instance.payload = Some(value);
                Ok(Value::Nil)
            }
            // Freezing happens to a value a constructor already returned, so
            // reaching this means `init` was called a second time by hand, on an
            // object that never got a payload the first time.
            Err(_) => Err(frozen(&self.heap, &Value::Instance(id), span)),
        }
    }

    /// The builtin a class descends from, if it descends from one at all.
    ///
    /// Walks the same chain `Class::method` walks, and terminates for the same
    /// reason: a parent is read before its subclass's name is bound, so the chain
    /// cannot contain a cycle.
    pub(super) fn builtin_base(&self, class: ObjId) -> Option<Builtin> {
        let mut class = self.heap.class(class);
        loop {
            if let Some(builtin) = class.builtin {
                return Some(builtin);
            }
            class = self.heap.class(class.parent?);
        }
    }

    /// Evaluates `receiver.name(args)`.
    ///
    /// Kept out of `eval` to stop the arm's locals widening a frame that
    /// recurses once per node in the tree. Measured against this program's
    /// recursion limit it made no difference — debug frames are dominated by
    /// things other than one arm — but it keeps `eval` readable.
    pub(super) fn eval_method_call(
        &mut self,
        target: &Expr,
        name: &str,
        args: &[Expr],
        env: ObjId,
        callee_span: Span,
        span: Span,
    ) -> Result<Value> {
        let receiver = self.eval(target, env)?;
        let name_span = Span::new(
            (target.span.end as usize + 1).min(callee_span.end as usize),
            callee_span.end as usize,
        );
        let attr = self.attr(&receiver, name, target.span, name_span, callee_span)?;

        // The receiver is held across every argument, any of which can reach a
        // safe point — the same hazard as an ordinary callee, and the same fix.
        // The attribute needs it too: a field holding a function is reachable
        // only through that field, which an argument is free to overwrite.
        let mark = self.temps.len();
        for value in [&receiver, attr.value()] {
            if value.handle().is_some() {
                self.temps.push(value.clone());
            }
        }
        let values = self.eval_seq(args, env);
        self.temps.truncate(mark);
        let values = values?;

        match attr {
            Attr::Method(method) => self.call_method(receiver, method, values, span),
            // A field that happens to hold a function; it never took a receiver.
            Attr::Field(value) => self.call(value, values, span),
        }
    }

    /// Calls `method` with `receiver` in front of `args`.
    ///
    /// The receiver is passed as argument zero for both kinds of method, which
    /// is what lets one `Native` serve as a free function and a method at once,
    /// and what lets a user method be an ordinary function whose first
    /// parameter the parser wrote.
    pub(super) fn call_method(
        &mut self,
        receiver: Value,
        method: Value,
        mut args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // Arity is reported against what the call site can actually write. The
        // receiver is one of the declared arguments but has no syntax as one,
        // so quoting the declared count would ask for an argument the user has
        // no way to supply. Every method has a receiver, making the subtraction
        // safe.
        let declared = match &method {
            Value::Native(native) => native.arity,
            Value::Function(id) => Some(self.heap.function(*id).decl.params.len()),
            // `attr` only ever produces the two above.
            other => panic!("expected a method, found {other:?}"),
        };
        if let Some(arity) = declared {
            check_arity(
                method.callable_name(&self.heap),
                arity.saturating_sub(1),
                args.len(),
                span,
            )?;
        }

        // A native was written against a `Value` and matches on its variant, so
        // an instance of a class extending a builtin has to arrive as the value
        // it *is* rather than as the object holding it — `e.upper()` reaches
        // `string`'s `upper`, which knows nothing about classes. A method written
        // in Quince gets the instance, because `self.domain` is the whole reason
        // it was written. Those two cases are exactly `Native` and `Function`:
        // every user method is a function, and a native only ever comes from a
        // builtin's seed.
        //
        // The one substitution in the interpreter, and the reason it is only one:
        // every path that gives a method a receiver comes through here.
        let receiver = match (&method, &receiver) {
            (Value::Native(_), Value::Instance(id)) => match &self.heap.instance(*id).payload {
                Some(payload) => payload.clone(),
                None => return Err(self.no_payload(*id, span)),
            },
            _ => receiver,
        };

        args.insert(0, receiver);
        self.call(method, args, span)
    }
}

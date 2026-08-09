//! The walk itself: every statement and expression, in scope order.
//!
//! Separate from the scope machinery next door because it is where the rules
//! live rather than where the bookkeeping does — and because it is what the next
//! three milestones add to. v0.7's visibility checks, v0.8's `const fn` purity
//! analysis and `override` rules, and v0.10's exhaustiveness check are each an
//! arm or two here, over a scope stack that does not change.

use std::collections::{HashMap, HashSet};

use crate::error::Result;
use crate::syntax::ast::{
    self, Block, Expr, ExprKind, FieldDecl, FnDecl, Op, Slot, Stmt, StmtKind, TypeExpr, TypeName,
};
use crate::syntax::token::Span;
use crate::sema::resolve::{
    Constness, Inherited, Resolver, Scope, builtin_named, declaration, refuse_clashes,
};

impl Resolver {

    // -- statements --------------------------------------------------------

    pub(super) fn stmts(&mut self, stmts: &mut [Stmt]) -> Result<()> {
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    pub(super) fn stmt(&mut self, stmt: &mut Stmt) -> Result<()> {
        let span = stmt.span;
        match &mut stmt.kind {
            StmtKind::Expr(expr) => self.expr(expr),

            StmtKind::Let {
                name,
                value,
                slot,
                ty,
                defaulted,
                ..
            } => {
                if *defaulted {
                    self.answers_for_itself(ty.as_ref(), &format!("`{name}`"), span)?;
                }
                self.expr(value)?;
                *slot = Some(self.slot_of(name));
                Ok(())
            }

            StmtKind::Destructure {
                names,
                rest,
                value,
                ..
            } => {
                self.expr(value)?;
                for bound in names.iter_mut().chain(rest.as_mut()) {
                    bound.slot = Some(self.slot_of(&bound.name));
                }
                Ok(())
            }

            // Nothing to resolve — an import names no expression and reserves no
            // slot. What is left is the one rule the grammar cannot state: a
            // module is loaded once, into the scope of the file that asked for
            // it, so an import inside a function or a loop would be a load whose
            // effect depends on whether the code ran. Refused here, where the
            // scope stack knows the answer.
            // An alias declares a name for a type and binds no value, so there
            // is no slot and nothing to resolve. `alias::expand` has already
            // removed every use of it by the time this runs.
            StmtKind::Alias { .. } => Ok(()),

            StmtKind::Import { module, .. } => {
                if !self.scopes.is_empty() {
                    return Err(declaration(
                        format!("`{module}` can only be imported at the top level"),
                        span,
                    )
                    .with_help("move this import to the top of the file"));
                }
                Ok(())
            }

            StmtKind::Fn { decl, slot, .. } => {
                *slot = Some(self.slot_of(&decl.name));
                // Unique until the evaluator starts cloning it into closures,
                // which cannot happen before the program runs.
                let decl =
                    std::rc::Rc::get_mut(decl).expect("the parser hands out unshared declarations");
                // A `fn` declared inside an `op init` is not itself construction:
                // it can be returned, stored, and called long after the object is
                // built, so `super.init` in its body is refused like anywhere else.
                let outer = std::mem::replace(&mut self.in_init, false);
                let result = self.function(decl);
                self.in_init = outer;
                result
            }

            StmtKind::Extend {
                target,
                methods,
                target_span: _,
                // Nothing to resolve and nothing to bring into scope. §3.6's
                // constraint names an *instantiation* — `list[int]`, not
                // `list[T]` — so it introduces no parameter a body could
                // mention, and `alias::expand` has already held it to the arity
                // and bounds every other annotation is held to.
                constraint: _,
            } => {
                // Read like any other name, which is what lets `extend int` and
                // `extend Money` take one path: a builtin type is a global holding
                // a class, and a program's class is a binding holding one.
                target.slot = Some(self.slot_of(&target.name));

                // No scope holding `super`, unlike a class with a parent. An
                // extension cannot shadow — that is the first refusal — so a
                // method it adds is never an override, and there is nothing above
                // it for `super` to mean. A `super` written here is the ordinary
                // "used outside a class" error.
                //
                // `None` for the base, because the `super.init` obligation belongs
                // to a class that descends from a builtin. An extension declares no
                // type and constructs nothing, and cannot hold an `op init` to
                // check in the first place.
                self.methods(methods, &target.name, None, span)
            }

            StmtKind::Class {
                name,
                params,
                parent,
                methods,
                fields,
                slot,
                ..
            } => {
                // The parent is read in the enclosing scope, before the class's
                // own name is bound, which is what makes `class A extends A` an
                // undefined-variable error instead of a cycle.
                if let Some(parent) = parent {
                    parent.slot = Some(self.slot_of(&parent.name));
                }
                *slot = Some(self.slot_of(name));

                // Before the bodies, so that a class whose header is wrong is
                // reported on the header. Nothing in here reads a scope — it is
                // a question about two declarations — but it wants the parent's
                // name, which the arm below moves out of reach.
                self.overriding(name, parent.as_ref().map(|p| p.name.as_str()), methods)?;

                // The parameters are in scope for the fields and the methods and
                // for nothing else — a class header is where `T` starts meaning
                // something. Restored rather than cleared, so a class declared
                // inside a method of another leaves the outer one's in place.
                let enclosing_params = std::mem::replace(
                    &mut self.type_params,
                    params.iter().map(|param| param.name.clone()).collect(),
                );

                // Methods of a subclass are resolved inside a scope holding
                // `super`, so a reference to it is an ordinary local lookup at
                // whatever depth it appears — including from a closure nested
                // in a method. The evaluator builds the matching scope.
                let Some(parent) = parent else {
                    let result = self
                        .field_values(fields)
                        .and_then(|()| self.methods(methods, name, None, span));
                    self.type_params = enclosing_params;
                    return result;
                };
                // Declaring no `op init` is fine and needs no check: the class
                // inherits its base's conversion and construction runs it, so
                // `class Username extends string {}` builds a string. Declaring one
                // is what takes construction over, and `methods` is where that
                // obligation is checked.
                let base = self.builtin_base(&parent.name);

                self.scopes.push(Scope::default());
                let result = self
                    .declare(ast::SUPER, false, span)
                    .and_then(|()| self.field_values(fields))
                    .and_then(|()| self.methods(methods, name, base, span));
                self.scopes.pop();
                self.type_params = enclosing_params;
                result
            }

            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                self.expr(cond)?;
                self.block(then)?;
                match otherwise {
                    Some(other) => self.stmt(other),
                    None => Ok(()),
                }
            }

            StmtKind::While { cond, body } => {
                self.expr(cond)?;
                self.block(body)
            }

            StmtKind::For {
                var,
                iter,
                body,
                slot,
            } => {
                self.expr(iter)?;
                // The loop variable belongs to the body's scope, because a fresh
                // one is built per iteration. It takes slot 0 there.
                body.slot_count = self.scoped(&mut body.stmts, &[(var.clone(), true, span)])?;
                *slot = Some(Slot::Local { hops: 0, index: 0 });
                Ok(())
            }

            StmtKind::Return(value) => match value {
                Some(expr) => self.expr(expr),
                None => Ok(()),
            },

            StmtKind::Try {
                body,
                binding,
                handler,
                slot,
            } => {
                // Two scopes, not one. A `let` inside the try block may not have
                // run when the error fired, so a handler that could see its slots
                // could read one that was never written — which is the "used
                // before it is declared" case this pass already reports, and
                // separate scopes mean it cannot arise here at all.
                self.block(body)?;
                // The binding belongs to the handler's scope and takes slot 0
                // there, the same treatment `for x in xs` gets.
                handler.slot_count =
                    self.scoped(&mut handler.stmts, &[(binding.clone(), true, span)])?;
                *slot = Some(Slot::Local { hops: 0, index: 0 });
                Ok(())
            }

            StmtKind::Throw(value) => self.expr(value),

            StmtKind::Block(block) => self.block(block),
        }
    }

    pub(super) fn block(&mut self, block: &mut Block) -> Result<()> {
        block.slot_count = self.scoped(&mut block.stmts, &[])?;
        Ok(())
    }

    /// Resolves a class's methods, holding `base` — the builtin the class
    /// descends from, if any — for the length of the one check that needs it.
    pub(super) fn methods(
        &mut self,
        methods: &mut [std::rc::Rc<FnDecl>],
        class: &str,
        base: Option<&'static str>,
        span: Span,
    ) -> Result<()> {
        // Held for the length of the bodies, because `self.m(…)` is answered by
        // the class's own table and the walk that reaches it is several layers
        // down. Restored rather than cleared, so a class declared inside a
        // method of another leaves the outer one in place.
        // §3.5's rules are about the *set* of declarations sharing a name, so
        // they are asked before any body is walked — and here rather than at the
        // parser because aliases have been expanded by now, which is what makes
        // `dict[string, int]` and a `ScoreTable` the duplicate they are.
        let mut sharing: HashMap<&str, Vec<&std::rc::Rc<FnDecl>>> = HashMap::new();
        for decl in methods.iter() {
            sharing.entry(decl.name.as_str()).or_default().push(decl);
        }
        let mut names: Vec<&&str> = sharing.keys().collect();
        names.sort_unstable();
        for name in names {
            refuse_clashes(&sharing[*name], name, &format!("`{class}`"))?;
        }
        drop(sharing);

        let enclosing = self.in_class.replace(class.to_string());
        let result = self.method_bodies(methods, class, base, span);
        self.in_class = enclosing;
        result
    }

    fn method_bodies(
        &mut self,
        methods: &mut [std::rc::Rc<FnDecl>],
        class: &str,
        base: Option<&'static str>,
        span: Span,
    ) -> Result<()> {
        for decl in methods {
            let decl =
                std::rc::Rc::get_mut(decl).expect("the parser hands out unshared declarations");
            let is_init = decl.op == Some(Op::Init);

            let outer = std::mem::replace(&mut self.in_init, is_init);
            self.super_inits = 0;
            let result = self.function(decl);
            let calls = self.super_inits;
            self.in_init = outer;
            result?;

            // A class descending from a builtin *is* a value of that builtin, and
            // `super.init` is the only thing that gives it one. An `op init`
            // without it builds an object that looks finished and fails at the
            // first method call, so the class is refused rather than stored.
            //
            // Only when the class declares its own `op init`: one that inherits
            // its parent's inherits a call that is already there.
            if let Some(base) = base
                && is_init
                && calls == 0
            {
                return Err(declaration(
                    format!("`{class}`'s `op init` never calls `super.init`"),
                    span,
                )
                .with_help(format!(
                    "`{class}` extends `{base}`, so `super.init` is what gives it its {base}"
                )));
            }
        }
        Ok(())
    }

    /// Refuses a declaration with no initializer whose type has no default.
    ///
    /// `what` names the declaration, so a field and a binding read alike. An
    /// unannotated one is never refused: `let x` is the dynamic binding v0.7
    /// describes and holds `nil`, and this rule is about annotations.
    /// Whether a `Ts...` appears anywhere in an annotation, at any depth.
    ///
    /// One question with two askers: whether a default can be decided here, and
    /// whether an argument count can be. Both are "no" for the same reason —
    /// the arity is not written down — so the predicate is one function.
    fn mentions_a_pack(ty: &TypeExpr) -> bool {
        matches!(ty.name, TypeName::Pack(_)) || ty.args.iter().any(Self::mentions_a_pack)
    }

    fn answers_for_itself(&self, ty: Option<&TypeExpr>, what: &str, span: Span) -> Result<()> {
        let Some(ty) = ty else {
            return Ok(());
        };
        let written = ty.written();
        let named = match &ty.name {
            // A type parameter is the one annotation this cannot decide. `T`
            // has a default exactly when whatever it was bound to has one, and
            // nothing here knows what that is — so the question is deferred to
            // construction, where the binding is. §3.6 settles the same
            // question the same way: one mistake reporting from two places at
            // two times is worse than reporting late.
            //
            // This is also what the *synthesised* initializer would otherwise
            // hit. A field with no `= …` becomes a call to its annotated type,
            // and `T()` names no class — so the evaluator builds the default
            // from the *substituted* type instead. See `Interp::init_fields`.
            TypeName::Named(name) if self.type_params.iter().any(|param| param == name) => {
                return Ok(());
            }
            // A pack, deferred for the same reason and one step further: how
            // many elements `tuple[Ts...]` has is not written anywhere this
            // pass can read, so neither is whether there is a value to
            // synthesize. `Interp::default_of` decides it where the arity is
            // known. §3.4, and it is why §3.5's flat refusal of a defaulted
            // `tuple` does not reach the field in `class CustomTuple[Ts...]`.
            _ if Self::mentions_a_pack(ty) => return Ok(()),
            TypeName::Named(name) if self.default_constructible(name) => return Ok(()),
            TypeName::Named(name) => name.clone(),
            // Not a class, so there is nothing to call. `any?` would admit
            // `nil`, but a default that depended on the `?` would make the
            // annotation decide whether the declaration is legal, and §3.6
            // settles the same question the other way for a parameter.
            //
            // A const argument is here only if someone annotated a binding with
            // a bare value — `let x: 16` — which is not a type and has no
            // default for the same reason.
            TypeName::Any | TypeName::Const(_) | TypeName::Pack(_) => written.clone(),
        };
        Err(declaration(
            format!("`{named}` has no default constructor, so {what} needs an initializer"),
            span,
        )
        .with_help(match self.constructors.get(&named) {
            Some(_) => format!(
                "`{named}` declares an `op init` that takes arguments, which is that class \
                 saying it needs them — write `= {named}(…)`"
            ),
            None => format!(
                "there is no honest default for `{written}`, so write `= …` with the value you \
                 meant — a class with no `op init`, and `list` and `dict`, are what answer for \
                 themselves"
            ),
        }))
    }

    /// Resolves the expressions a class's field declarations initialize with.
    ///
    /// In the scope the evaluator runs them in, which is the one the methods
    /// close over — `Class::field_env`. Nothing declares a name here: a field is
    /// not a slot in that scope, it is an entry on each instance.
    fn field_values(&mut self, fields: &mut [FieldDecl]) -> Result<()> {
        for field in fields {
            if field.defaulted {
                self.answers_for_itself(
                    field.ty.as_ref(),
                    &format!("`{}`", field.name),
                    field.name_span,
                )?;
            }
            self.expr(&mut field.value)?;
        }
        Ok(())
    }

    /// Holds a class's methods to what `override` and `final` claim about them.
    ///
    /// Three refusals, and the second is the one that makes the first worth
    /// having: a keyword required where it is true but writable where it is not
    /// is documentation nobody can trust, and a misspelled method name is
    /// exactly the mistake the pair catches.
    ///
    /// `op init` is exempt. Every constructor in a hierarchy replaces its
    /// parent's — that is what `super.init` is for — so requiring the word there
    /// would mean writing it on every subclass in the language and would say
    /// nothing when it was.
    fn overriding(
        &self,
        class: &str,
        parent: Option<&str>,
        methods: &[std::rc::Rc<FnDecl>],
    ) -> Result<()> {
        for decl in methods {
            if decl.op == Some(Op::Init) {
                continue;
            }
            // How the member reads in a report — `op add` or `fn total`, which
            // is what the program wrote and what it has to go and change.
            let named = match decl.op {
                Some(op) => format!("op {}", op.name()),
                None => format!("fn {}", decl.name),
            };
            let found = match parent {
                Some(parent) => self.inherited(Some(class), parent, &decl.name),
                None => Inherited::Absent,
            };
            match found {
                Inherited::Found { owner, member } => {
                    if member.guarded {
                        return Err(declaration(
                            format!("cannot override `{named}`, which is final in `{owner}`"),
                            decl.name_span,
                        )
                        .with_help(format!(
                            "`{owner}` declares it `final`, which is that class saying its \
                             implementation is the one — `{class}` can add a method under \
                             another name, but it cannot replace this one"
                        )));
                    }
                    if !decl.overrides {
                        return Err(declaration(
                            format!("`{named}` replaces `{}`'s and does not say so", owner),
                            decl.name_span,
                        )
                        .with_help(format!(
                            "write `override {named}` — replacing a superclass member silently \
                             is how a rename in `{owner}` stops being an error and starts being \
                             two methods that no longer meet"
                        )));
                    }
                }
                // A stray `override` is refused only where the chain is known
                // all the way up. See [`Inherited`]: a parent this pass cannot
                // see is a check that has to decline to answer.
                Inherited::Absent if decl.overrides => {
                    return Err(declaration(
                        format!("`{named}` overrides nothing"),
                        decl.name_span,
                    )
                    .with_help(match parent {
                        Some(parent) => format!(
                            "neither `{parent}` nor anything it extends declares `{}` — check \
                             the spelling, or delete the `override`",
                            decl.name
                        ),
                        None => format!(
                            "`{class}` extends nothing, so there is no member for this to \
                             replace — delete the `override`, or give the class a superclass"
                        ),
                    }));
                }
                Inherited::Absent | Inherited::Unknown => {}
            }
        }
        Ok(())
    }

    /// The builtin `parent` descends from, by name.
    ///
    /// Wrong in one direction only, and deliberately: `final S = string` followed
    /// by `class X extends S` looks like it descends from nothing, because nothing
    /// has been evaluated and `S` is not a class name. So the check above can miss
    /// a class that owes a `super.init` — never accuse one that does not — and the
    /// evaluator carries the guard for what gets through.
    pub(super) fn builtin_base(&self, parent: &str) -> Option<&'static str> {
        let mut seen = HashSet::new();
        let mut name = parent;
        loop {
            if let Some(builtin) = builtin_named(name) {
                return Some(builtin);
            }
            // `class A extends B` with `class B extends A` is a cycle in what was
            // written. The evaluator refuses it — a parent is read before the
            // subclass's name is bound — but this walk runs first and has to
            // terminate on its own.
            if !seen.insert(name) {
                return None;
            }
            name = self.parents.get(name)?;
        }
    }

    /// Refuses a mutation inside a `const fn` or `const op`.
    ///
    /// What `const` restricts is **state**, not effects: `print` is allowed,
    /// because a rule that made debugging one impossible is a rule people route
    /// around, and `throw` and an early `return` are control flow rather than
    /// mutation. What is left is the three shapes a body has for changing
    /// something a caller can see — a field, an element, and a name bound
    /// outside — and each is refused here.
    ///
    /// Deliberately blunt about the first two. `let d = {}` followed by
    /// `d["a"] = 1` mutates a dict this call made and nobody else can reach, and
    /// it is refused anyway: telling the two apart is an escape analysis, and
    /// this pass numbers slots. The restrictive answer is the one that can be
    /// relaxed later — an error can become legal, a semantics cannot.
    fn refuse_mutation(&self, target: &Expr) -> Result<()> {
        let Some(constness) = &self.constness else {
            return Ok(());
        };
        let named = &constness.named;
        let (what, help) = match &target.kind {
            ExprKind::Field { .. } => (
                "assign to a field",
                "a `const` method answers a question about the receiver — one that changes it \
                 is an ordinary `fn`",
            ),
            ExprKind::Index { .. } => (
                "assign through an index",
                "this is refused even for a container the call made itself: telling those apart \
                 is an analysis this pass does not do, and the strict answer is the one that can \
                 be relaxed later",
            ),
            ExprKind::Var(var) => {
                // A name the function bound is its own, and reassigning it is
                // not mutation anybody outside can see. Anything shallower is,
                // and a global is the shallowest of all.
                let inside = match self.find(&var.name) {
                    Some((Slot::Local { hops, .. }, _)) => {
                        self.scopes.len() - 1 - hops as usize >= constness.base
                    }
                    _ => false,
                };
                if inside {
                    return Ok(());
                }
                (
                    "reassign a name bound outside it",
                    "the binding belongs to whoever called this, so writing to it is exactly \
                     the state change `const` promises not to make",
                )
            }
            _ => return Ok(()),
        };
        Err(declaration(format!("`{named}` may not {what}"), target.span).with_help(help))
    }

    /// Refuses `self.m(…)` where `m` is not itself `const`.
    ///
    /// The rule that makes the first one hold: a `const fn` that could call a
    /// mutating method on its own receiver would have promised nothing. Only
    /// `self`, because that is the receiver `const` is a promise about — a
    /// method called on an argument is caught by the argument being frozen, if
    /// it was declared `const`, and is otherwise the caller's own business.
    ///
    /// A name no class in the chain declares is left alone. It is a field
    /// holding a function, or a method an `extend` block added, and neither is
    /// something this pass can read a `const` off.
    fn refuse_impure_call(&self, callee: &Expr) -> Result<()> {
        let Some(constness) = &self.constness else {
            return Ok(());
        };
        let ExprKind::Field { target, name, .. } = &callee.kind else {
            return Ok(());
        };
        if !matches!(&target.kind, ExprKind::Var(var) if var.name == ast::SELF) {
            return Ok(());
        }
        let Some(class) = &self.in_class else {
            return Ok(());
        };
        let Inherited::Found { owner, member } = self.inherited(None, class, name) else {
            return Ok(());
        };
        if member.constant {
            return Ok(());
        }
        Err(declaration(
            format!(
                "`{}` may not call `{name}`, which is not `const`",
                constness.named
            ),
            callee.span,
        )
        .with_help(format!(
            "declare `{owner}`'s `{name}` as `const` too, or make this one an ordinary `fn` — \
             a `const` method that could reach a mutating one would be promising nothing"
        )))
    }

    pub(super) fn function(&mut self, decl: &mut FnDecl) -> Result<()> {
        // Parameters occupy the body scope's first slots, in order, which is
        // what lets a call bind them by index without consulting their names.
        //
        // A parameter is rebindable — except the receiver, which nobody wrote
        // and which the method does not own. That immutability is also what
        // keeps slot 0 pointing at the instance for the whole of `init`.
        let params: Vec<_> = decl
            .params
            .iter()
            .map(|param| (param.name.clone(), !param.receiver, param.span))
            .collect();

        // A default is evaluated in the callee's *declaration* scope, so it is
        // resolved here — before the body scope is pushed — and the hop counts
        // it gets are the ones `Function::env` will hand it at the call. A
        // default naming another parameter therefore does not reach it, which
        // is the same answer §3.6 gives: the scope is the declaration's, not
        // the call's.
        for param in &mut decl.params {
            if let Some(default) = &mut param.default {
                self.expr(default)?;
            }
        }

        // A `fn` nested inside a `const fn` stays inside it — the opposite of
        // what `in_init` does, and for the opposite reason. A nested `fn` is not
        // *construction*, because it can run long after the object is built; but
        // it closes over the receiver and the enclosing locals, so letting it
        // mutate them would be the whole promise escaping through a closure.
        let outer = self.constness.clone();
        if decl.constant {
            self.constness = Some(Constness {
                base: self.scopes.len(),
                named: match decl.op {
                    Some(op) => format!("const op {}", op.name()),
                    None => format!("const fn {}", decl.name),
                },
            });
        }
        let result = self.scoped(&mut decl.body.stmts, &params);
        self.constness = outer;
        decl.body.slot_count = result?;
        Ok(())
    }

    // -- expressions -------------------------------------------------------

    pub(super) fn expr(&mut self, expr: &mut Expr) -> Result<()> {
        match &mut expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Bool(_)
            | ExprKind::Nil => Ok(()),

            // A wrapper around a chain and a question about a type: both hold
            // one expression and neither binds a name, so resolving is resolving
            // what is inside them.
            ExprKind::Chain(inner) => self.expr(inner),
            ExprKind::Is { value, .. } => self.expr(value),
            ExprKind::Coalesce { lhs, rhs } => {
                self.expr(lhs)?;
                self.expr(rhs)
            }

            ExprKind::Var(var) => {
                // `self` is a parameter of the enclosing method, so failing to
                // find it locally means there is no enclosing method. Left to
                // run time it would fall through to a global lookup and report
                // `undefined variable`, which describes the symptom rather than
                // the mistake.
                if var.name == ast::SELF && self.find(ast::SELF).is_none() {
                    return Err(declaration(
                        "`self` is only valid inside a method",
                        expr.span,
                    )
                    .with_help(
                        "a method is a `fn` or an `op` declared in a class body — a plain `fn`, \
                         even one nested inside a method, has no receiver to name",
                    ));
                }
                var.slot = Some(self.slot_of(&var.name));
                Ok(())
            }

            ExprKind::Dict(entries) => {
                for (key, value) in entries {
                    self.expr(key)?;
                    self.expr(value)?;
                }
                Ok(())
            }

            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for item in items {
                    self.expr(item)?;
                }
                Ok(())
            }

            ExprKind::Unary { rhs, .. } => self.expr(rhs),

            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => {
                self.expr(lhs)?;
                self.expr(rhs)
            }

            ExprKind::Call { callee, args } => {
                // Counted here rather than in the `Super` arm, which cannot see
                // whether it is being called. A bare `super.init` is a reference
                // to a constructor that may never run, so it satisfies nothing.
                if let ExprKind::Super { name, .. } = &callee.kind
                    && name == Op::Init.name()
                {
                    self.super_inits += 1;
                }
                self.refuse_impure_call(callee)?;
                self.expr(callee)?;
                for arg in args {
                    self.expr(&mut arg.value)?;
                }
                Ok(())
            }

            ExprKind::Index { target, index } => {
                self.expr(target)?;
                self.expr(index)
            }

            // Every argument is a name that has to resolve to a class, so they
            // resolve as the ordinary expressions they are. Which class each
            // names — and whether the target takes arguments at all — is a run
            // time question, for the reason `extend` puts the same one there.
            ExprKind::TypeArgs { target, args } => {
                self.expr(target)?;
                for arg in args {
                    self.expr(arg)?;
                }
                Ok(())
            }

            ExprKind::Slice { target, start, end } => {
                self.expr(target)?;
                for bound in [start, end].into_iter().flatten() {
                    self.expr(bound)?;
                }
                Ok(())
            }

            ExprKind::Field { target, .. } => self.expr(target),

            ExprKind::Super {
                name,
                parent,
                receiver,
            } => {
                // `super` lives one scope out from the method body, so failing
                // to find it means there is no enclosing subclass method. The
                // two halves fail differently and are worth separating: a class
                // with no parent has no `super` at all, while a plain function
                // has neither.
                if self.find(ast::SUPER).is_none() {
                    return Err(declaration(
                        "`super` is only valid inside a method of a class that extends another",
                        expr.span,
                    )
                    .with_help(
                        "`super` names the parent class, so there has to be one — write \
                         `class C extends Parent` to give it one",
                    ));
                }
                // `super.init` is construction, and `op init` is the method that
                // *is* construction — for a class extending a builtin it is what
                // gives the object its value, and for any other it re-runs a
                // constructor on an object that already finished. Confining it is
                // also what makes the count above mean what the check reads it as.
                if name == Op::Init.name() && !self.in_init {
                    return Err(declaration(
                        "`super.init` is only valid inside `op init`",
                        expr.span,
                    )
                    .with_help("construction happens once, in the method that constructs"));
                }
                parent.slot = Some(self.slot_of(ast::SUPER));
                receiver.slot = Some(self.slot_of(ast::SELF));
                Ok(())
            }

            // The two write forms, checked alike: `n += 1` is refused wherever
            // `n = n + 1` is, which is what §3.7's "the usual `final` and
            // `const` rules apply unchanged" means when written down.
            ExprKind::Assign { target, value }
            | ExprKind::AssignOp { target, value, .. }
            | ExprKind::AssignShort { target, value, .. } => {
                self.expr(value)?;
                // Before the `final`/`const` binding check below, because the
                // two overlap on a reassignment inside a `const fn` and this is
                // the more specific answer: the name is refused for being
                // outside the function, not for how it was declared.
                self.refuse_mutation(target)?;
                // A `final` or `const` local is known to be immutable here, so
                // the error arrives before the program runs. Globals keep their
                // run-time check, since the binding may not exist yet.
                //
                // The message says "reassign" rather than naming the keyword:
                // only the *name* is being refused here, and `const` refuses
                // rather more than that. Saying so would blur the line the two
                // forms exist to draw.
                if let ExprKind::Var(var) = &target.kind
                    && let Some((_, false)) = self.find(&var.name)
                {
                    // `self` is immutable by the same mechanism, but for a
                    // different reason, and saying "reassign" would teach that
                    // it is a binding someone chose. It is the receiver.
                    let (message, help) = match var.name == ast::SELF {
                        true => (
                            "`self` is the receiver, not a variable to assign to".to_string(),
                            "assign to one of its fields instead — `self.name = value`"
                                .to_string(),
                        ),
                        false => (
                            format!("cannot reassign `{}`", var.name),
                            format!(
                                "it is bound with `final` or `const`, either of which binds a \
                                 name once — declare `{}` with `let` to reassign it",
                                var.name
                            ),
                        ),
                    };
                    return Err(declaration(message, target.span).with_help(help));
                }
                self.expr(target)
            }
        }
    }
}

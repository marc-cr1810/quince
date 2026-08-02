//! The walk itself: every statement and expression, in scope order.
//!
//! Separate from the scope machinery next door because it is where the rules
//! live rather than where the bookkeeping does — and because it is what the next
//! three milestones add to. v0.7's visibility checks, v0.8's `const fn` purity
//! analysis and `override` rules, and v0.10's exhaustiveness check are each an
//! arm or two here, over a scope stack that does not change.

use std::collections::HashSet;

use crate::error::Result;
use crate::syntax::ast::{self, Block, Expr, ExprKind, FnDecl, Op, Slot, Stmt, StmtKind};
use crate::syntax::token::Span;
use crate::sema::resolve::{Resolver, Scope, builtin_named, declaration};

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
                name, value, slot, ..
            } => {
                self.expr(value)?;
                *slot = Some(self.slot_of(name));
                Ok(())
            }

            // Nothing to resolve — an import names no expression and reserves no
            // slot. What is left is the one rule the grammar cannot state: a
            // module is loaded once, into the scope of the file that asked for
            // it, so an import inside a function or a loop would be a load whose
            // effect depends on whether the code ran. Refused here, where the
            // scope stack knows the answer.
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

            StmtKind::Fn { decl, slot } => {
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
                parent,
                methods,
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

                // Methods of a subclass are resolved inside a scope holding
                // `super`, so a reference to it is an ordinary local lookup at
                // whatever depth it appears — including from a closure nested
                // in a method. The evaluator builds the matching scope.
                let Some(parent) = parent else {
                    return self.methods(methods, name, None, span);
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
                    .and_then(|()| self.methods(methods, name, base, span));
                self.scopes.pop();
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
        decl.body.slot_count = self.scoped(&mut decl.body.stmts, &params)?;
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

            ExprKind::List(items) => {
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
                self.expr(callee)?;
                for arg in args {
                    self.expr(arg)?;
                }
                Ok(())
            }

            ExprKind::Index { target, index } => {
                self.expr(target)?;
                self.expr(index)
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

            ExprKind::Assign { target, value } => {
                self.expr(value)?;
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
                    let message = match var.name == ast::SELF {
                        true => "`self` is the receiver, not a variable to assign to".to_string(),
                        false => format!("cannot reassign `{}`", var.name),
                    };
                    return Err(declaration(message, target.span));
                }
                self.expr(target)
            }
        }
    }
}

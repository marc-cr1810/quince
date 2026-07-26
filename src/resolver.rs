//! Static resolution of variable references.
//!
//! Runs between parsing and evaluation, rewriting every name into either a
//! `(hops, index)` pair addressing a slot in an enclosing scope, or `Global`.
//! Two things fall out of that:
//!
//! - Reading a local stops being a hash of a `String` against a chain of maps
//!   and becomes an index into a `Vec`.
//! - Mistakes that used to surface only when a line happened to run — assigning
//!   to a `const`, declaring the same name twice — are reported before the
//!   program starts.
//!
//! Globals stay dynamic. The REPL defines them a line at a time, and a program
//! is allowed to call a function declared further down the file, so neither can
//! be pinned to a slot.

use std::collections::{HashMap, HashSet};

use crate::ast::{self, Block, Expr, ExprKind, FnDecl, Op, Slot, Stmt, StmtKind};
use crate::class::BUILTINS;
use crate::error::QuinceError;
use crate::token::Span;

/// Resolves a whole program in place.
pub fn resolve(program: &mut [Stmt]) -> Result<(), QuinceError> {
    let mut resolver = Resolver::default();
    // The top level has no scope, so `scoped` never runs over it — which is why
    // nothing used to register the classes declared there or look at the names
    // its bindings take. Slots are unaffected: `declare_slot` returns early
    // without a scope, because a global is bound by name at run time.
    resolver.declare_all(program, &[])?;
    resolver.stmts(program)
}

/// The builtin type called `name`, if there is one.
///
/// Read off the same list the globals are bound from, so a builtin type added
/// later is reserved without this file being touched. Hands back the static name
/// rather than the one passed in, so a caller can keep it past the borrow it
/// searched with.
fn builtin_named(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .map(|builtin| builtin.name())
        .find(|builtin| *builtin == name)
}

/// Whether `name` is one of the types the language defines.
fn is_builtin_type(name: &str) -> bool {
    builtin_named(name).is_some()
}

#[derive(Debug)]
struct Local {
    index: u16,
    mutable: bool,
}

#[derive(Default, Debug)]
struct Scope {
    names: HashMap<String, Local>,
    count: u16,
}

/// The lexical scope stack. Empty means "at the top level", where every name is
/// a global.
#[derive(Default)]
struct Resolver {
    scopes: Vec<Scope>,
    /// Names that belong to a type, and so are not available to anything else.
    ///
    /// Flat rather than per-scope, and never popped: a type name means the type
    /// everywhere, which is what lets `int(5)` and a future `extend int` be read
    /// without asking what is in scope at that point. Over-broad for a class
    /// declared inside a function — its name is reserved for the rest of the
    /// program — and deliberately so, on the same reasoning as `declare`: an
    /// error can be relaxed later, a semantics cannot.
    types: HashSet<String>,
    /// What each class extends, by name. Flat and never popped, for the reason
    /// [`Resolver::types`] is.
    ///
    /// Enough to answer the one question this file asks of a hierarchy: whether a
    /// class descends from a builtin, and so whether its `op init` owes a
    /// `super.init`. Names rather than classes, because nothing has been
    /// evaluated yet.
    parents: HashMap<String, String>,
    /// Whether the body being resolved is an `op init`. Cleared for a `fn` nested
    /// inside one, which could run at any time and so is not construction.
    in_init: bool,
    /// How many `super.init(…)` calls the current `op init` body contains.
    ///
    /// A count of what is *written*, not of what runs — a call in each arm of an
    /// `if` is two. That is why the only thing decided from it here is whether
    /// there are none; the evaluator is what enforces that exactly one happens.
    super_inits: usize,
}

impl Resolver {
    // -- scopes ------------------------------------------------------------

    /// Resolves `stmts` as a new scope, returning the number of slots it needs.
    ///
    /// Declarations are collected before anything is resolved, so a function
    /// can refer to a sibling declared after it — which is what makes mutually
    /// recursive nested functions work, and matches how the evaluator behaved
    /// when scopes were name-keyed maps.
    fn scoped(
        &mut self,
        stmts: &mut [Stmt],
        predeclare: &[(String, bool, Span)],
    ) -> Result<u16, QuinceError> {
        self.scopes.push(Scope::default());
        let result = self
            .declare_all(stmts, predeclare)
            .and_then(|()| self.stmts(stmts));
        let scope = self.scopes.pop().expect("a scope was pushed above");
        result.map(|()| scope.count)
    }

    fn declare_all(
        &mut self,
        stmts: &mut [Stmt],
        predeclare: &[(String, bool, Span)],
    ) -> Result<(), QuinceError> {
        // Classes first, so that a `let` stealing a type's name is refused
        // whichever order the two were written in. Without this pass the check
        // below would only catch the half of the mistake that happens to come
        // second in the file.
        for stmt in stmts.iter() {
            if let StmtKind::Class { name, parent, .. } = &stmt.kind {
                self.declare_type(name, stmt.span)?;
                // Recorded in the same pass, so a class can extend one declared
                // further down the file — which the evaluator refuses, but as an
                // undefined variable rather than as a chain this failed to see.
                if let Some(parent) = parent {
                    self.parents.insert(name.clone(), parent.name.clone());
                }
            }
        }

        for (name, mutable, span) in predeclare {
            self.declare(name, *mutable, *span)?;
        }
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Let { name, bind, .. } => {
                    self.declare(name, bind.mutable(), stmt.span)?
                }
                StmtKind::Fn { decl, .. } => self.declare(&decl.name, false, stmt.span)?,
                StmtKind::Class { name, .. } => self.declare_slot(name, false, stmt.span)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Records that `name` belongs to a type, refusing the builtin type names.
    ///
    /// `class int { … }` is the mistake this catches. Shadowing a builtin type is
    /// refused for the same reason shadowing it with a `let` is: every mention of
    /// `int` after that line would mean something the language did not choose.
    fn declare_type(&mut self, name: &str, span: Span) -> Result<(), QuinceError> {
        if is_builtin_type(name) {
            return Err(QuinceError::new(
                format!("`{name}` is a type built into the language"),
                span,
            )
            .with_help("pick another name for this class"));
        }
        self.types.insert(name.to_string());
        Ok(())
    }

    /// Reserves a slot for a binding, refusing one that would take a type's name.
    ///
    /// The check is here rather than in the evaluator because it is decidable
    /// without running anything, and because at the top level there is no slot to
    /// collide with — a global is bound by name, so `let string = "x"` used to
    /// replace the type silently and every later mention of `string` meant a
    /// string. That is the whole reason this exists.
    fn declare(&mut self, name: &str, mutable: bool, span: Span) -> Result<(), QuinceError> {
        if is_builtin_type(name) || self.types.contains(name) {
            let what = match is_builtin_type(name) {
                true => "a type built into the language",
                false => "a class in this program",
            };
            return Err(
                QuinceError::new(format!("`{name}` is the name of {what}"), span).with_help(
                    format!(
                        "a type's name cannot also be a variable — rename this one, not `{name}`"
                    ),
                ),
            );
        }
        self.declare_slot(name, mutable, span)
    }

    /// Reserves a slot in the innermost scope, with no opinion about the name.
    ///
    /// Redeclaring a name in the same scope is an error rather than silent
    /// shadowing: with slots the two would be separate storage, so a closure
    /// made between them would quietly keep the older one. Shadowing across
    /// nested scopes is untouched. This is the restrictive choice on purpose —
    /// an error can be relaxed later, a semantics cannot.
    ///
    /// Called directly only for a `class`, whose name *is* the type and so
    /// cannot be refused for being one.
    fn declare_slot(&mut self, name: &str, mutable: bool, span: Span) -> Result<(), QuinceError> {
        let Some(scope) = self.scopes.last_mut() else {
            return Ok(()); // top level: a global, bound by name at run time
        };
        if scope.names.contains_key(name) {
            return Err(QuinceError::new(
                format!("`{name}` is already declared in this scope"),
                span,
            ));
        }
        let index = scope.count;
        scope.count = index.checked_add(1).ok_or_else(|| {
            QuinceError::new("a scope may not declare more than 65535 names", span)
        })?;
        scope
            .names
            .insert(name.to_string(), Local { index, mutable });
        Ok(())
    }

    /// Walks outwards counting scopes, so the hop count matches the runtime
    /// parent chain.
    fn find(&self, name: &str) -> Option<(Slot, bool)> {
        for (hops, scope) in self.scopes.iter().rev().enumerate() {
            if let Some(local) = scope.names.get(name) {
                let slot = Slot::Local {
                    hops: hops as u16,
                    index: local.index,
                };
                return Some((slot, local.mutable));
            }
        }
        None
    }

    fn slot_of(&self, name: &str) -> Slot {
        self.find(name).map_or(Slot::Global, |(slot, _)| slot)
    }

    // -- statements --------------------------------------------------------

    fn stmts(&mut self, stmts: &mut [Stmt]) -> Result<(), QuinceError> {
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &mut Stmt) -> Result<(), QuinceError> {
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

            StmtKind::Class {
                name,
                parent,
                methods,
                slot,
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

    fn block(&mut self, block: &mut Block) -> Result<(), QuinceError> {
        block.slot_count = self.scoped(&mut block.stmts, &[])?;
        Ok(())
    }

    /// Resolves a class's methods, holding `base` — the builtin the class
    /// descends from, if any — for the length of the one check that needs it.
    fn methods(
        &mut self,
        methods: &mut [std::rc::Rc<FnDecl>],
        class: &str,
        base: Option<&'static str>,
        span: Span,
    ) -> Result<(), QuinceError> {
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
                return Err(QuinceError::new(
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
    fn builtin_base(&self, parent: &str) -> Option<&'static str> {
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

    fn function(&mut self, decl: &mut FnDecl) -> Result<(), QuinceError> {
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

    fn expr(&mut self, expr: &mut Expr) -> Result<(), QuinceError> {
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
                    return Err(QuinceError::new(
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
                    return Err(QuinceError::new(
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
                    return Err(QuinceError::new(
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
                    return Err(QuinceError::new(message, target.span));
                }
                self.expr(target)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::StmtKind;

    fn resolved(src: &str) -> Vec<Stmt> {
        let tokens = crate::lexer::Lexer::new(src)
            .tokenize()
            .expect("should lex");
        let mut program = crate::parser::Parser::new(tokens)
            .parse()
            .expect("should parse");
        resolve(&mut program).unwrap_or_else(|e| panic!("should resolve `{src}`: {}", e.message));
        program
    }

    fn resolve_err(src: &str) -> String {
        let tokens = crate::lexer::Lexer::new(src)
            .tokenize()
            .expect("should lex");
        let mut program = crate::parser::Parser::new(tokens)
            .parse()
            .expect("should parse");
        resolve(&mut program)
            .expect_err("should fail to resolve")
            .message
    }

    #[test]
    fn a_type_name_is_not_available_to_a_binding() {
        // Every declaration form, because `declare` is the one choke point they
        // all pass through and this is what says so.
        for src in [
            "let string = 1",
            "final string = 1",
            "const string = 1",
            "fn string() {\n}",
            "fn f(string) {\n}",
            "for string in [1] {\n}",
            "fn f() {\n let string = 1\n}",
        ] {
            assert_eq!(
                resolve_err(src),
                "`string` is the name of a type built into the language",
                "`{src}` should be refused"
            );
        }
    }

    #[test]
    fn a_class_name_is_not_available_either_whichever_order_they_come_in() {
        // The pre-pass exists for the first of these: without it only the
        // mistake written second in the file would be caught.
        for src in [
            "let Point = 1\nclass Point {\n}",
            "class Point {\n}\nlet Point = 1",
            "class Point {\n}\nfn f() {\n let Point = 1\n}",
        ] {
            assert_eq!(
                resolve_err(src),
                "`Point` is the name of a class in this program",
                "`{src}` should be refused"
            );
        }
    }

    #[test]
    fn a_class_may_not_be_named_after_a_builtin_type() {
        assert_eq!(
            resolve_err("class int {\n}"),
            "`int` is a type built into the language"
        );
    }

    #[test]
    fn a_type_name_still_resolves_to_a_global_everywhere() {
        // The point of reserving the name: `int` inside a function is the same
        // `int` as outside, with nothing in between able to have taken it.
        let program = resolved("fn f() {\n return int\n}");
        let StmtKind::Fn { decl, .. } = &program[0].kind else {
            panic!("expected a function");
        };
        let StmtKind::Return(Some(expr)) = &decl.body.stmts[0].kind else {
            panic!("expected a return with a value");
        };
        let ExprKind::Var(var) = &expr.kind else {
            panic!("expected a variable");
        };
        assert_eq!(var.slot, Some(Slot::Global));
    }

    /// The slot a variable reference was given.
    fn var(expr: &Expr) -> Slot {
        let ExprKind::Var(var) = &expr.kind else {
            panic!("expected a variable, found {:?}", expr.kind);
        };
        var.slot.expect("the resolver should have filled this in")
    }

    fn body(stmt: &Stmt) -> &Block {
        let StmtKind::Fn { decl, .. } = &stmt.kind else {
            panic!("expected a function declaration");
        };
        &decl.body
    }

    #[test]
    fn top_level_names_stay_global() {
        let program = resolved("let x = 1\nprint(x)");
        let StmtKind::Let { slot, .. } = &program[0].kind else {
            panic!("expected a let");
        };
        assert_eq!(*slot, Some(Slot::Global));

        let StmtKind::Expr(call) = &program[1].kind else {
            panic!("expected a call");
        };
        let ExprKind::Call { callee, args } = &call.kind else {
            panic!("expected a call");
        };
        assert_eq!(var(callee), Slot::Global, "`print` is a builtin global");
        assert_eq!(var(&args[0]), Slot::Global);
    }

    #[test]
    fn parameters_take_the_first_slots_in_order() {
        let program = resolved("fn add(a, b) { return a + b }");
        let body = body(&program[0]);
        assert_eq!(body.slot_count, 2);

        let StmtKind::Return(Some(expr)) = &body.stmts[0].kind else {
            panic!("expected a return");
        };
        let ExprKind::Binary { lhs, rhs, .. } = &expr.kind else {
            panic!("expected an addition");
        };
        assert_eq!(var(lhs), Slot::Local { hops: 0, index: 0 });
        assert_eq!(var(rhs), Slot::Local { hops: 0, index: 1 });
    }

    #[test]
    fn locals_are_numbered_after_the_parameters() {
        let program = resolved("fn f(a) { let b = 1\nreturn b }");
        let body = body(&program[0]);
        assert_eq!(body.slot_count, 2);

        let StmtKind::Return(Some(expr)) = &body.stmts[1].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 0, index: 1 });
    }

    #[test]
    fn a_capture_counts_hops_out_to_the_enclosing_scope() {
        let program = resolved("fn outer() {\n let n = 0\n fn inner() { return n }\n}");
        let outer = body(&program[0]);
        // `n` and `inner` both occupy slots in outer's scope.
        assert_eq!(outer.slot_count, 2);

        let StmtKind::Return(Some(expr)) = &body(&outer.stmts[1]).stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 1, index: 0 });
    }

    #[test]
    fn a_block_is_its_own_scope() {
        let program = resolved("fn f() {\n let a = 1\n { let b = 2\n return b }\n}");
        let outer = body(&program[0]);
        assert_eq!(outer.slot_count, 1, "the inner block has its own slots");

        let StmtKind::Block(inner) = &outer.stmts[1].kind else {
            panic!("expected a block");
        };
        assert_eq!(inner.slot_count, 1);
        let StmtKind::Return(Some(expr)) = &inner.stmts[1].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 0, index: 0 });
    }

    #[test]
    fn the_loop_variable_is_the_first_slot_of_the_body() {
        let program = resolved("fn f() { for item in [1] { return item } }");
        let StmtKind::For { body, slot, .. } = &body(&program[0]).stmts[0].kind else {
            panic!("expected a for loop");
        };
        assert_eq!(*slot, Some(Slot::Local { hops: 0, index: 0 }));
        assert_eq!(body.slot_count, 1);

        let StmtKind::Return(Some(expr)) = &body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 0, index: 0 });
    }

    #[test]
    fn declarations_are_visible_to_functions_declared_above_them() {
        // Mutual recursion between nested functions only works if declarations
        // are collected before anything is resolved.
        let program = resolved("fn outer() {\n fn a() { return b }\n fn b() { return 1 }\n}");
        let StmtKind::Return(Some(expr)) = &body(&body(&program[0]).stmts[0]).stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 1, index: 1 });
    }

    #[test]
    fn self_is_the_first_slot_of_a_method() {
        // It is a parameter, so it takes slot 0 and the written parameters
        // follow. Nothing in the evaluator has to know it was a keyword.
        let program = resolved("class C {\n fn m(a) { return self }\n}");
        let StmtKind::Class { methods, .. } = &program[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(methods[0].body.slot_count, 2, "`self` and `a`");

        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 0, index: 0 });
    }

    #[test]
    fn a_closure_inside_a_method_captures_self_by_hops() {
        // The reason `self` is a parameter rather than something injected at
        // call time: the existing scope chain carries it inwards for free.
        let program = resolved("class C {\n fn m() {\n fn inner() { return self }\n}\n}");
        let StmtKind::Class { methods, .. } = &program[0].kind else {
            panic!("expected a class");
        };
        let StmtKind::Fn { decl, .. } = &methods[0].body.stmts[0].kind else {
            panic!("expected a nested function");
        };
        let StmtKind::Return(Some(expr)) = &decl.body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 1, index: 0 });
    }

    #[test]
    fn self_outside_a_method_is_caught_before_running() {
        // Left to the evaluator this would fall through to a global lookup and
        // report `undefined variable`, naming the symptom rather than the
        // mistake.
        assert_eq!(
            resolve_err("fn f() { return self }"),
            "`self` is only valid inside a method"
        );
        assert_eq!(
            resolve_err("print(self)"),
            "`self` is only valid inside a method"
        );
    }

    #[test]
    fn self_cannot_be_reassigned() {
        // The receiver is not a binding the method owns. This is also what lets
        // the constructor drop its `temps` root: slot 0 is guaranteed to still
        // name the instance when `init` returns.
        for body in [
            "self = nil",
            "self = 1",
            "self = self",
            // Reached through the scope chain, which costs nothing extra:
            // `find` reports the mutability of whatever scope it lands in.
            "fn inner() { self = nil }",
        ] {
            assert_eq!(
                resolve_err(&format!("class C {{ fn m() {{ {body} }} }}")),
                "`self` is the receiver, not a variable to assign to",
                "for `{body}`"
            );
        }
    }

    #[test]
    fn a_method_may_still_reach_through_self() {
        // Only the name is pinned. The instance stays as mutable as any other.
        resolved("class C { fn m() { self.x = 1\n return self } }");
    }

    #[test]
    fn super_sits_one_scope_outside_a_method_body() {
        // The scope wrapped around the methods holds exactly `super`, so from a
        // method body it is one hop out at slot 0. The evaluator builds a scope
        // of that shape, and the two have to agree.
        let program = resolved("class A {}\nclass B extends A {\n fn m() { return super.m }\n}");
        let StmtKind::Class { methods, .. } = &program[1].kind else {
            panic!("expected a class");
        };
        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        let ExprKind::Super {
            parent, receiver, ..
        } = &expr.kind
        else {
            panic!("expected a super lookup");
        };
        assert_eq!(parent.slot, Some(Slot::Local { hops: 1, index: 0 }));
        assert_eq!(
            receiver.slot,
            Some(Slot::Local { hops: 0, index: 0 }),
            "`self` is still the method's own first parameter"
        );
    }

    #[test]
    fn a_class_without_a_parent_adds_no_scope() {
        // The extra scope exists only for `super`, so a plain class must not
        // pay a hop for it — otherwise every capture out of a method body would
        // be counted wrong.
        let program = resolved("fn f() {\n let n = 1\n class C {\n fn m() { return n }\n}\n}");
        let StmtKind::Class { methods, .. } = &body(&program[0]).stmts[1].kind else {
            panic!("expected a class");
        };
        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(var(expr), Slot::Local { hops: 1, index: 0 });
    }

    #[test]
    fn a_subclass_adds_exactly_one_hop() {
        let program = resolved(
            "fn f() {\n let n = 1\n class A {}\n class C extends A {\n fn m() { return n }\n}\n}",
        );
        let StmtKind::Class { methods, .. } = &body(&program[0]).stmts[2].kind else {
            panic!("expected a class");
        };
        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(
            var(expr),
            Slot::Local { hops: 2, index: 0 },
            "through `super`"
        );
    }

    #[test]
    fn super_outside_a_subclass_method_is_caught_before_running() {
        assert_eq!(
            resolve_err("class C {\n fn m() { return super.m() }\n}"),
            "`super` is only valid inside a method of a class that extends another"
        );
        assert_eq!(
            resolve_err("fn f() { return super.m() }"),
            "`super` is only valid inside a method of a class that extends another"
        );
    }

    #[test]
    fn a_class_binds_its_own_name() {
        let program = resolved("fn f() {\n class C {}\n return C\n}");
        let StmtKind::Class { slot, .. } = &body(&program[0]).stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(*slot, Some(Slot::Local { hops: 0, index: 0 }));
    }

    #[test]
    fn redeclaring_in_the_same_scope_is_an_error() {
        assert_eq!(
            resolve_err("fn f() { let x = 1\n let x = 2 }"),
            "`x` is already declared in this scope"
        );
    }

    #[test]
    fn shadowing_a_name_from_an_outer_scope_is_allowed() {
        let program = resolved("fn f() {\n let x = 1\n { let x = 2\n return x }\n}");
        let StmtKind::Block(inner) = &body(&program[0]).stmts[1].kind else {
            panic!("expected a block");
        };
        let StmtKind::Return(Some(expr)) = &inner.stmts[1].kind else {
            panic!("expected a return");
        };
        assert_eq!(
            var(expr),
            Slot::Local { hops: 0, index: 0 },
            "the inner `x`"
        );
    }

    #[test]
    fn reassigning_a_bound_local_is_caught_before_running() {
        // The assignment is unreachable, which is the point: a run-time check
        // would never have seen it.
        assert_eq!(
            resolve_err("fn f() { final k = 1\n if false { k = 2 } }"),
            "cannot reassign `k`"
        );
        assert_eq!(
            resolve_err("fn f() { const k = 1\n if false { k = 2 } }"),
            "cannot reassign `k`",
            "`const` binds the name too"
        );
    }

    #[test]
    fn a_bound_global_is_left_to_the_evaluator() {
        // Globals may not exist yet when the resolver runs, so their mutability
        // is not knowable here.
        resolved("final k = 1\nk = 2");
    }

    #[test]
    fn extending_a_builtin_requires_super_init() {
        assert_eq!(
            resolve_err("class Bad extends string {\n op init(s) { self.raw = s }\n}"),
            "`Bad`'s `op init` never calls `super.init`"
        );
        // Through a chain, which is what `parents` exists for: the builtin is two
        // links up and neither link mentions it.
        assert_eq!(
            resolve_err(
                "class Email extends string {\n op init(s) { super.init(s) }\n}\n\
                 class Work extends Email {\n op init(s) { self.raw = s }\n}"
            ),
            "`Work`'s `op init` never calls `super.init`"
        );
        // Declaring no `op init` inherits one that already calls it.
        resolved(
            "class Email extends string {\n op init(s) { super.init(s) }\n}\n\
             class Work extends Email {}",
        );
        // A class descending from no builtin owes nothing.
        resolved("class Animal {\n op init(n) { self.n = n }\n}");
    }

    #[test]
    fn declaring_no_op_init_is_what_asks_for_the_implicit_one() {
        // Nothing owed: the class inherits its base's conversion and construction
        // runs it. Writing `op init(s) { super.init(s) }` would say no more.
        resolved("class Username extends string {}");
        resolved("class Stack extends list {}");
        // Through a chain of classes that each declare nothing.
        resolved("class Chars extends string {}\nclass Word extends Chars {}");
        // An ancestor's `op init` is inherited whole, and was checked where it was
        // written.
        resolved(
            "class Email extends string {\n op init(s) { super.init(s) }\n}\n\
             class Work extends Email {}\nclass Home extends Work {}",
        );
        // A class descending from no builtin needs no constructor at all.
        resolved("class Marker {}\nclass Sub extends Marker {}");
    }

    #[test]
    fn super_init_is_confined_to_op_init() {
        assert_eq!(
            resolve_err(
                "class Email extends string {\n op init(s) { super.init(s) }\n\
                 fn reset(s) { super.init(s) }\n}"
            ),
            "`super.init` is only valid inside `op init`"
        );
        // The rule is about `super.init`, not about extending a builtin — calling
        // a parent's constructor on an object that already finished is the same
        // mistake whatever the parent is.
        assert_eq!(
            resolve_err(
                "class Animal {\n op init(n) { self.n = n }\n}\n\
                 class Dog extends Animal {\n fn reset(n) { super.init(n) }\n}"
            ),
            "`super.init` is only valid inside `op init`"
        );
        // A nested `fn` can outlive construction, so it is not inside it.
        assert_eq!(
            resolve_err(
                "class Email extends string {\n\
                 op init(s) { super.init(s)\n fn later() { super.init(s) }\n }\n}"
            ),
            "`super.init` is only valid inside `op init`"
        );
        // Every other `super` name is untouched.
        resolved(
            "class Animal {\n fn speak() { return 1 }\n}\n\
             class Dog extends Animal {\n fn speak() { return super.speak() }\n}",
        );
    }

    #[test]
    fn a_written_super_init_is_counted_but_not_a_held_one() {
        // One call in each arm is one call as far as this check goes: it asks
        // whether construction is written at all, and the evaluator is what
        // refuses two from running.
        resolved(
            "class Cond extends int {\n\
             op init(x) { if x < 0 { super.init(0) } else { super.init(x) } }\n}",
        );
        // A reference is not a call, so holding one satisfies nothing — which is
        // the whole reason the count lives in the `Call` arm.
        assert_eq!(
            resolve_err(
                "class Held extends string {\n op init(s) { final f = super.init\n f(s) }\n}"
            ),
            "`Held`'s `op init` never calls `super.init`"
        );
    }

    #[test]
    fn a_cycle_in_what_was_written_does_not_hang_the_walk() {
        // Refused by the evaluator, which reads a parent before binding the
        // subclass's name — but this walk runs first and has to terminate.
        resolved("class A extends B {}\nclass B extends A {}");
    }
}

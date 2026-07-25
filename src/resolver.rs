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

use std::collections::HashMap;

use crate::ast::{self, Block, Expr, ExprKind, FnDecl, Slot, Stmt, StmtKind};
use crate::error::QuinceError;
use crate::token::Span;

/// Resolves a whole program in place.
pub fn resolve(program: &mut [Stmt]) -> Result<(), QuinceError> {
    Resolver::default().stmts(program)
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
        for (name, mutable, span) in predeclare {
            self.declare(name, *mutable, *span)?;
        }
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Let { name, mutable, .. } => self.declare(name, *mutable, stmt.span)?,
                StmtKind::Fn { decl, .. } => self.declare(&decl.name, false, stmt.span)?,
                StmtKind::Class { name, .. } => self.declare(name, false, stmt.span)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Reserves a slot in the innermost scope.
    ///
    /// Redeclaring a name in the same scope is an error rather than silent
    /// shadowing: with slots the two would be separate storage, so a closure
    /// made between them would quietly keep the older one. Shadowing across
    /// nested scopes is untouched. This is the restrictive choice on purpose —
    /// an error can be relaxed later, a semantics cannot.
    fn declare(&mut self, name: &str, mutable: bool, span: Span) -> Result<(), QuinceError> {
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
                self.function(decl)
            }

            StmtKind::Class {
                name,
                methods,
                slot,
            } => {
                *slot = Some(self.slot_of(name));
                for decl in methods {
                    let decl = std::rc::Rc::get_mut(decl)
                        .expect("the parser hands out unshared declarations");
                    self.function(decl)?;
                }
                Ok(())
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

            StmtKind::Block(block) => self.block(block),
        }
    }

    fn block(&mut self, block: &mut Block) -> Result<(), QuinceError> {
        block.slot_count = self.scoped(&mut block.stmts, &[])?;
        Ok(())
    }

    fn function(&mut self, decl: &mut FnDecl) -> Result<(), QuinceError> {
        // Parameters occupy the body scope's first slots, in order, which is
        // what lets a call bind them by index without consulting their names.
        let params: Vec<_> = decl
            .params
            .iter()
            .map(|param| (param.name.clone(), true, param.span))
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

            ExprKind::Assign { target, value } => {
                self.expr(value)?;
                // A `const` local is known to be immutable here, so the error
                // arrives before the program runs. Globals keep their run-time
                // check, since the binding may not exist yet.
                if let ExprKind::Var(var) = &target.kind
                    && let Some((_, false)) = self.find(&var.name)
                {
                    return Err(QuinceError::new(
                        format!("cannot assign to constant `{}`", var.name),
                        target.span,
                    ));
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
    fn assigning_to_a_constant_local_is_caught_before_running() {
        // The assignment is unreachable, which is the point: a run-time check
        // would never have seen it.
        assert_eq!(
            resolve_err("fn f() { const k = 1\n if false { k = 2 } }"),
            "cannot assign to constant `k`"
        );
    }

    #[test]
    fn a_constant_global_is_left_to_the_evaluator() {
        // Globals may not exist yet when the resolver runs, so their mutability
        // is not knowable here.
        resolved("const k = 1\nk = 2");
    }
}

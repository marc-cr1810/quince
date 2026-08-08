use super::*;

use crate::syntax::ast::{Block, Expr, ExprKind, StmtKind};

fn resolved(src: &str) -> Vec<Stmt> {
    let tokens = crate::syntax::lexer::Lexer::new(src)
        .tokenize()
        .expect("should lex");
    let mut program = crate::syntax::parser::Parser::new(tokens)
        .parse()
        .expect("should parse");
    resolve(&mut program).unwrap_or_else(|e| panic!("should resolve `{src}`: {}", e.message));
    program
}

fn resolve_err(src: &str) -> String {
    let tokens = crate::syntax::lexer::Lexer::new(src)
        .tokenize()
        .expect("should lex");
    let mut program = crate::syntax::parser::Parser::new(tokens)
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
    assert_eq!(var(&args[0].value), Slot::Global);
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
         class Dog extends Animal {\n override fn speak() { return super.speak() }\n}",
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

#[test]
fn replacing_a_superclass_member_has_to_say_so() {
    // Both halves, because either alone is worthless: a word that is required
    // where it is true and writable where it is not is documentation nobody can
    // trust, and a word that is optional catches no rename at all.
    assert_eq!(
        resolve_err("class A {\n fn m() { return 1 }\n}\nclass B extends A {\n fn m() { return 2 }\n}"),
        "`fn m` replaces `A`'s and does not say so"
    );
    assert_eq!(
        resolve_err("class A {\n fn m() { return 1 }\n}\nclass B extends A {\n override fn n() { return 2 }\n}"),
        "`fn n` overrides nothing"
    );
    resolved("class A {\n fn m() { return 1 }\n}\nclass B extends A {\n override fn m() { return 2 }\n}");
    // And one that adds rather than replaces needs no word.
    resolved("class A {\n fn m() { return 1 }\n}\nclass B extends A {\n fn n() { return 2 }\n}");
}

#[test]
fn override_reaches_through_the_whole_chain() {
    // Not just the immediate parent: what a grandparent declared is what a
    // grandchild replaces, and the walk is what makes the two agree.
    resolved(
        "class A {\n fn m() { return 1 }\n}\nclass B extends A {}\n\
         class C extends B {\n override fn m() { return 2 }\n}",
    );
    assert_eq!(
        resolve_err(
            "class A {\n fn m() { return 1 }\n}\nclass B extends A {}\n\
             class C extends B {\n fn m() { return 2 }\n}"
        ),
        "`fn m` replaces `A`'s and does not say so"
    );
}

#[test]
fn a_builtin_ancestors_methods_count_as_declared() {
    // A seed table is a `static`, so this is answerable at resolution — and it
    // has to be, or `class Email extends string` would be the one hierarchy in
    // the language where a member could be replaced silently.
    assert_eq!(
        resolve_err("class Email extends string {\n fn upper() { return \"E\" }\n}"),
        "`fn upper` replaces `string`'s and does not say so"
    );
    resolved("class Email extends string {\n override fn upper() { return \"E\" }\n}");
    // A builtin descends from nothing, so a name it does not seed is absent
    // rather than unknown, and a stray `override` there is still refused.
    assert_eq!(
        resolve_err("class Email extends string {\n override fn shout() { return \"E\" }\n}"),
        "`fn shout` overrides nothing"
    );
}

#[test]
fn a_final_member_cannot_be_replaced() {
    assert_eq!(
        resolve_err(
            "class A {\n final fn m() { return 1 }\n}\nclass B extends A {\n override fn m() { return 2 }\n}"
        ),
        "cannot override `fn m`, which is final in `A`"
    );
    // Reported as the guard rather than as a missing keyword, because adding
    // `override` is exactly the advice that would not work.
    assert_eq!(
        resolve_err(
            "class A {\n final fn m() { return 1 }\n}\nclass B extends A {\n fn m() { return 2 }\n}"
        ),
        "cannot override `fn m`, which is final in `A`"
    );
}

#[test]
fn a_constructor_is_exempt() {
    // Every constructor in a hierarchy replaces its parent's — that is what
    // `super.init` is for — so requiring the word would mean writing it on
    // every subclass in the language and saying nothing when it was written.
    resolved(
        "class A {\n op init(n) { self.n = n }\n}\n\
         class B extends A {\n op init() { super.init(1) }\n}",
    );
}

#[test]
fn an_unseen_superclass_is_not_accused() {
    // `extends` names a binding and nothing has been evaluated, so a parent
    // this pass cannot find is a check that declines to answer. Wrong in one
    // direction only: it may miss an `override` that should have been written,
    // and must never refuse one that was.
    resolved("from shapes import Shape\nclass Circle extends Shape {\n override fn area() { return 1 }\n}");
}

#[test]
fn a_const_method_may_not_change_state() {
    // The three shapes a body has for changing something a caller can see.
    assert_eq!(
        resolve_err("class C {\n op init() { self.n = 0 }\n const fn m() { self.n = 1 }\n}"),
        "`const fn m` may not assign to a field"
    );
    assert_eq!(
        resolve_err("class C {\n op init() { self.xs = [1] }\n const fn m() { self.xs[0] = 2 }\n}"),
        "`const fn m` may not assign through an index"
    );
    assert_eq!(
        resolve_err("let n = 0\nconst fn m() { n = 1 }"),
        "`const fn m` may not reassign a name bound outside it"
    );
    // An `op` is held to the same rule and reads back as one.
    assert_eq!(
        resolve_err("class C {\n op init() { self.n = 0 }\n const op string() { self.n = 1\n return \"\" }\n}"),
        "`const op string` may not assign to a field"
    );
}

#[test]
fn a_const_method_owns_what_it_bound_itself() {
    // Locals, parameters, and a loop variable are all bindings the call made,
    // so writing to one changes nothing anybody outside can see. Without the
    // depth comparison every `const fn` with a mutable local would be refused.
    resolved("const fn m(n: int): int {\n let total = 0\n total = total + n\n n = n + 1\n return total + n\n}");
    resolved("const fn m(xs: list): int {\n let total = 0\n for x in xs { total = total + 1 }\n return total\n}");
    // And a nested block's binding is still the function's own.
    resolved("const fn m(): int {\n let a = 1\n { let b = 2\n b = 3\n a = b }\n return a\n}");
}

#[test]
fn purity_reaches_into_a_nested_function() {
    // The opposite of what `in_init` does, and for the opposite reason: a
    // nested `fn` closes over the receiver and the enclosing locals, so letting
    // it mutate them would be the whole promise escaping through a closure.
    assert_eq!(
        resolve_err(
            "class C {\n op init() { self.n = 0 }\n\
             const fn m() {\n fn inner() { self.n = 1 }\n return inner }\n}"
        ),
        "`const fn m` may not assign to a field"
    );
}

#[test]
fn a_const_method_may_only_call_const_methods() {
    assert_eq!(
        resolve_err(
            "class C {\n op init() { self.n = 0 }\n fn bump() { self.n = 1 }\n\
             const fn m() { self.bump() }\n}"
        ),
        "`const fn m` may not call `bump`, which is not `const`"
    );
    resolved(
        "class C {\n op init() { self.n = 0 }\n const fn peek(): int { return self.n }\n\
         const fn m(): int { return self.peek() }\n}",
    );
    // Inherited, because a subclass's `const` method reaches its parent's
    // methods through the same chain a call does.
    assert_eq!(
        resolve_err(
            "class A {\n fn bump() { }\n}\nclass B extends A {\n const fn m() { self.bump() }\n}"
        ),
        "`const fn m` may not call `bump`, which is not `const`"
    );
    // The rule runs one way only: an ordinary `fn` may call anything.
    resolved("class C {\n const fn peek(): int { return 1 }\n fn m(): int { return self.peek() }\n}");
}

#[test]
fn purity_restricts_state_and_not_effects() {
    // `print` alters no heap memory, and a rule that made debugging a `const
    // fn` impossible is a rule people route around. `throw` and an early
    // `return` are control flow rather than mutation.
    resolved("const fn m(n: int): int {\n print(n)\n if n < 0 { throw Error(\"no\") }\n return n\n}");
}

#[test]
fn a_binding_with_no_initializer_takes_its_types_default() {
    // The three shapes that can answer: a class with no constructor, one whose
    // constructor takes nothing, and the two builtin containers.
    resolved("class Marker {}\nlet m: Marker");
    resolved("class C {\n op init() { self.n = 1 }\n}\nlet c: C");
    resolved("let xs: list[int]\nlet d: dict[string, int]");
    // And the ones that cannot.
    assert_eq!(
        resolve_err("let n: int"),
        "`int` has no default constructor, so `n` needs an initializer"
    );
    assert_eq!(
        resolve_err("class C {\n op init(n) { self.n = n }\n}\nlet c: C"),
        "`C` has no default constructor, so `c` needs an initializer"
    );
    // A subclass inherits the answer, whichever way it goes.
    resolved("class A {\n op init() {}\n}\nclass B extends A {}\nlet b: B");
    assert_eq!(
        resolve_err("class A {\n op init(n) { self.n = n }\n}\nclass B extends A {}\nlet b: B"),
        "`B` has no default constructor, so `b` needs an initializer"
    );
}

#[test]
fn no_annotation_and_no_initializer_is_still_nil() {
    // This rule is about annotations. `let x` is the dynamic binding v0.7
    // describes and is untouched.
    resolved("let x\nprint(x)");
    resolved("class C {\n let data\n}");
}

#[test]
fn a_fields_initializer_is_resolved_in_the_scope_it_runs_in() {
    // `Class::field_env` is the scope the methods closed over, so a name a
    // field's initializer reads has to be given a slot against that same scope.
    // Nothing walked these before v0.8, and reaching one panicked in `read`.
    let program = resolved("fn f() {\n let base = 1\n class C {\n let n = base\n }\n return C()\n}");
    let StmtKind::Class { fields, .. } = &body(&program[0]).stmts[1].kind else {
        panic!("expected a class");
    };
    assert_eq!(
        var(&fields[0].value),
        Slot::Local { hops: 0, index: 0 },
        "`base` is a local of the function the class was declared in"
    );
    // A subclass's fields run in the scope holding `super`, one deeper — the
    // same scope its methods close over, which is what makes the hop counts and
    // `Class::field_env` agree.
    let program = resolved(
        "fn f() {\n let base = 1\n class A {}\n class B extends A {\n let n = base\n }\n return B()\n}",
    );
    let StmtKind::Class { fields, .. } = &body(&program[0]).stmts[2].kind else {
        panic!("expected a class");
    };
    assert_eq!(var(&fields[0].value), Slot::Local { hops: 1, index: 0 });
}

#[test]
fn two_declarations_sharing_a_name_need_distinct_types() {
    // In a scope, in a class, and in an `extend` block — the three places §3.5
    // names, all reaching one check.
    assert_eq!(
        resolve_err("fn f(a: int) {}\nfn f(b: int) {}"),
        "this scope already declares `f` with these parameter types"
    );
    assert_eq!(
        resolve_err("class C {\n fn m(a: int) {}\n fn m(b: int) {}\n}"),
        "`C` already declares `m` with these parameter types"
    );
    assert_eq!(
        resolve_err("extend int {\n fn m(a: int) {}\n fn m(b: int) {}\n}"),
        "`int` already declares `m` with these parameter types"
    );
    resolved("fn f(a: int) {}\nfn f(a: string) {}");
    resolved("class C {\n fn m(a: int) {}\n fn m(a: string) {}\n}");
}

#[test]
fn an_ambiguous_pair_is_refused_where_it_is_declared() {
    assert_eq!(
        resolve_err("fn f(a: float) {}\nfn f(a: int?) {}"),
        "this scope declares a `f` this one cannot be told apart from"
    );
}

#[test]
fn the_first_declaration_of_a_name_binds_it_and_the_rest_join() {
    // One slot for the name, however many declarations it has — which is what
    // makes them one name holding several declarations rather than several
    // names quietly shadowing each other.
    let program = resolved("fn outer() {\n fn f(a: int) {}\n fn f(a: string) {}\n let n = 1\n}");
    let body = body(&program[0]);
    assert_eq!(body.slot_count, 2, "`f` and `n`");
    let flags: Vec<bool> = body
        .stmts
        .iter()
        .filter_map(|stmt| match &stmt.kind {
            StmtKind::Fn { overload, .. } => Some(*overload),
            _ => None,
        })
        .collect();
    assert_eq!(flags, vec![false, true]);
}

#[test]
fn a_name_declared_once_is_not_an_overload() {
    // The flag is what tells the evaluator to *join* rather than replace, and a
    // REPL entry redefining a function must replace. Its scope declares the
    // name once, so the flag is off.
    let program = resolved("fn f(a: int) {}");
    let StmtKind::Fn { overload, .. } = &program[0].kind else {
        panic!("expected a function");
    };
    assert!(!overload);
}

#[test]
fn an_alias_is_expanded_before_signatures_are_compared() {
    // Why the check is here and not at the parser: `ScoreTable` and what it
    // abbreviates are one type, and two declarations spelling it differently
    // are one signature.
    assert_eq!(
        resolve_err(
            "alias ScoreTable = dict[string, int]\n\
             fn f(a: ScoreTable) {}\nfn f(a: dict[string, int]) {}"
        ),
        "this scope already declares `f` with these parameter types"
    );
}

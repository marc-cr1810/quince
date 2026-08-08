use super::*;
use crate::syntax::ast::{
    BindKind, Expr, ExprKind, FnDecl, Op, Openness, UnaryOp, Visibility,
};
use crate::syntax::lexer::Lexer;

    fn parse(src: &str) -> Result<Vec<Stmt>> {
        let tokens = Lexer::new(src).tokenize().expect("should lex");
        Parser::new(tokens).parse()
    }

    fn parse_ok(src: &str) -> Vec<Stmt> {
        parse(src).unwrap_or_else(|e| panic!("should parse `{src}`: {}", e.message))
    }

    fn parse_err(src: &str) -> Raised {
        parse(src).expect_err("should fail to parse")
    }

    /// Renders an expression as an s-expression, so precedence and associativity
    /// are visible in the assertion itself.
    fn sexpr(expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Int(n) => n.to_string(),
            ExprKind::Float(n) => n.to_string(),
            ExprKind::Str(s) => format!("{s:?}"),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Nil => "nil".into(),
            ExprKind::Var(var) => var.name.clone(),
            ExprKind::List(items) => format!("[{}]", joined(items)),
            ExprKind::Dict(entries) => {
                let pairs: Vec<_> = entries
                    .iter()
                    .map(|(key, value)| format!("{}: {}", sexpr(key), sexpr(value)))
                    .collect();
                format!("{{{}}}", pairs.join(" "))
            }
            ExprKind::Unary { op, rhs } => {
                let op = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not",
                    UnaryOp::BitNot => "~",
                };
                format!("({op} {})", sexpr(rhs))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                format!("({} {} {})", op.symbol(), sexpr(lhs), sexpr(rhs))
            }
            ExprKind::Logical { op, lhs, rhs } => {
                format!("({} {} {})", op.word(), sexpr(lhs), sexpr(rhs))
            }
            ExprKind::Call { callee, args } => {
                let written: Vec<String> = args
                    .iter()
                    .map(|arg| match &arg.name {
                        Some((name, _)) => format!("{name}: {}", sexpr(&arg.value)),
                        None => sexpr(&arg.value),
                    })
                    .collect();
                format!("(call {} {})", sexpr(callee), written.join(" "))
            }
            ExprKind::Index { target, index } => {
                format!("(index {} {})", sexpr(target), sexpr(index))
            }
            ExprKind::TypeArgs { target, args } => {
                format!("(type-args {} {})", sexpr(target), joined(args))
            }
            ExprKind::Slice { target, start, end } => {
                let bound = |b: &Option<Box<Expr>>| b.as_deref().map_or(String::new(), sexpr);
                format!("(slice {} {} {})", sexpr(target), bound(start), bound(end))
            }
            ExprKind::Field {
                target,
                name,
                optional,
            } => {
                let dot = if *optional { "?." } else { "." };
                format!("({dot} {} {name})", sexpr(target))
            }
            ExprKind::Chain(inner) => format!("(chain {})", sexpr(inner)),
            ExprKind::Coalesce { lhs, rhs } => format!("(?? {} {})", sexpr(lhs), sexpr(rhs)),
            ExprKind::Is { value, ty } => format!("(is {} {})", sexpr(value), ty.written()),
            ExprKind::Assign { target, value } => {
                format!("(= {} {})", sexpr(target), sexpr(value))
            }
            ExprKind::AssignOp { target, op, value } => {
                format!("({}= {} {})", op.symbol(), sexpr(target), sexpr(value))
            }
            ExprKind::AssignShort { target, op, value } => {
                format!("({}= {} {})", op.symbol(), sexpr(target), sexpr(value))
            }
            ExprKind::Super { name, .. } => format!("(super {name})"),
        }
    }

    fn joined(exprs: &[Expr]) -> String {
        exprs.iter().map(sexpr).collect::<Vec<_>>().join(" ")
    }

    /// Parses a single expression statement and renders it.
    fn expr_of(src: &str) -> String {
        let stmts = parse_ok(src);
        assert_eq!(stmts.len(), 1, "expected one statement from `{src}`");
        match &stmts[0].kind {
            StmtKind::Expr(expr) => sexpr(expr),
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    #[test]
    fn precedence_follows_arithmetic() {
        assert_eq!(expr_of("1 + 2 * 3"), "(+ 1 (* 2 3))");
        assert_eq!(expr_of("1 * 2 + 3"), "(+ (* 1 2) 3)");
        assert_eq!(expr_of("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    }

    #[test]
    fn arithmetic_is_left_associative() {
        assert_eq!(expr_of("1 - 2 - 3"), "(- (- 1 2) 3)");
        assert_eq!(expr_of("8 / 4 / 2"), "(/ (/ 8 4) 2)");
    }

    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        assert_eq!(expr_of("a + 1 < b * 2"), "(< (+ a 1) (* b 2))");
    }

    #[test]
    fn logical_operators_bind_loosest() {
        assert_eq!(expr_of("a or b and c"), "(or a (and b c))");
        assert_eq!(expr_of("a == 1 and b != 2"), "(and (== a 1) (!= b 2))");
    }

    #[test]
    fn unary_binds_tighter_than_arithmetic() {
        assert_eq!(expr_of("-a * b"), "(* (- a) b)");
        assert_eq!(expr_of("not a and b"), "(and (not a) b)");
        // The space is load-bearing: `--a` munches as one `--` token and is the
        // decrement statement, so a double negation has to be written apart.
        assert_eq!(expr_of("- -a"), "(- (- a))");
    }

    #[test]
    fn assignment_is_right_associative() {
        assert_eq!(expr_of("a = b = c"), "(= a (= b c))");
        assert_eq!(expr_of("a = 1 + 2"), "(= a (+ 1 2))");
    }

    #[test]
    fn assignment_targets_are_restricted() {
        let err = parse_err("1 = 2");
        assert!(err.message.contains("cannot assign"), "{}", err.message);
        // Index and field targets stay legal.
        assert_eq!(expr_of("a[0] = 1"), "(= (index a 0) 1)");
        assert_eq!(expr_of("a.b = 1"), "(= (. a b) 1)");
    }

    #[test]
    fn postfix_operators_chain() {
        assert_eq!(expr_of("f(1)(2)"), "(call (call f 1) 2)");
        assert_eq!(expr_of("a.b.c"), "(. (. a b) c)");
        assert_eq!(expr_of("a[0][1]"), "(index (index a 0) 1)");
        assert_eq!(expr_of("a.b(1)[2]"), "(index (call (. a b) 1) 2)");
    }

    #[test]
    fn a_colon_in_a_subscript_makes_it_a_slice() {
        // Which form it is turns on the `:`, and either bound may be missing,
        // so all four shapes have to be distinguished from a plain index.
        assert_eq!(expr_of("a[1:2]"), "(slice a 1 2)");
        assert_eq!(expr_of("a[1:]"), "(slice a 1 )");
        assert_eq!(expr_of("a[:2]"), "(slice a  2)");
        assert_eq!(expr_of("a[:]"), "(slice a  )");
        assert_eq!(expr_of("a[1]"), "(index a 1)");
    }

    #[test]
    fn slice_bounds_are_full_expressions() {
        // The bounds parse with `expression`, so a `:` cannot be mistaken for
        // the start of one and arithmetic in a bound needs no parentheses.
        assert_eq!(
            expr_of("a[i + 1:len(a) - 1]"),
            "(slice a (+ i 1) (- (call len a) 1))"
        );
        assert_eq!(expr_of("a[:2][0]"), "(index (slice a  2) 0)");
    }

    #[test]
    fn calls_take_arguments_with_optional_trailing_comma() {
        assert_eq!(expr_of("f()"), "(call f )");
        assert_eq!(expr_of("f(1, 2)"), "(call f 1 2)");
        assert_eq!(expr_of("f(1, 2,)"), "(call f 1 2)");
    }

    #[test]
    fn lists_parse_with_optional_trailing_comma() {
        assert_eq!(expr_of("[]"), "[]");
        assert_eq!(expr_of("[1, 2, 3]"), "[1 2 3]");
        assert_eq!(expr_of("[1, 2,]"), "[1 2]");
        assert_eq!(expr_of("[[1], 2]"), "[[1] 2]");
    }

    #[test]
    fn dicts_parse_with_optional_trailing_comma() {
        assert_eq!(expr_of("({})"), "{}");
        assert_eq!(expr_of(r#"({"a": 1, "b": 2})"#), r#"{"a": 1 "b": 2}"#);
        assert_eq!(expr_of(r#"({"a": 1,})"#), r#"{"a": 1}"#);
        assert_eq!(expr_of("(({1 + 1: [2]}))"), "{(+ 1 1): [2]}");
    }

    #[test]
    fn a_brace_in_condition_position_still_opens_a_block() {
        // The ambiguity Rust has with struct literals does not arise here: a
        // dict literal is not a postfix form, so once a condition has parsed,
        // `{` can only be the block.
        let stmts = parse_ok("if a { }");
        let StmtKind::If { cond, then, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert_eq!(sexpr(cond), "a");
        assert!(then.stmts.is_empty());

        // And a dict literal *inside* the condition is still reachable.
        let stmts = parse_ok(r#"if a == {"k": 1} { }"#);
        let StmtKind::If { cond, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert_eq!(sexpr(cond), r#"(== a {"k": 1})"#);
    }

    #[test]
    fn a_statement_beginning_with_a_brace_is_a_block_not_a_dict() {
        assert!(matches!(parse_ok("{ }")[0].kind, StmtKind::Block(_)));
        let err = parse_err(r#"{ "a": 1 }"#);
        assert!(err.message.contains("needs parentheses"), "{}", err.message);
    }

    #[test]
    fn in_parses_as_a_comparison_level_operator() {
        assert_eq!(expr_of("a in b"), "(in a b)");
        assert_eq!(expr_of("a + 1 in b"), "(in (+ a 1) b)");
        assert_eq!(expr_of("a in b and c"), "(and (in a b) c)");
        // The loop form takes `in` before any expression is parsed, so the two
        // uses cannot collide.
        let stmts = parse_ok("for k in d { }");
        let StmtKind::For { var, iter, .. } = &stmts[0].kind else {
            panic!("expected a for loop");
        };
        assert_eq!((var.as_str(), sexpr(iter)), ("k", "d".to_string()));
    }

    #[test]
    fn a_call_on_the_next_line_is_a_separate_statement() {
        // Without this rule `let a = b` followed by `(c)` would silently become
        // a call to `b`.
        let stmts = parse_ok("let a = b\n(c)");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0].kind, StmtKind::Let { .. }));
        assert_eq!(expr_of("(c)"), "c");
    }

    #[test]
    fn method_chains_may_break_across_lines() {
        assert_eq!(
            expr_of("a\n  .b()\n  .c()"),
            "(call (. (call (. a b) ) c) )"
        );
    }

    #[test]
    fn statements_may_be_separated_by_newline_or_semicolon() {
        assert_eq!(parse_ok("let a = 1\nlet b = 2").len(), 2);
        assert_eq!(parse_ok("let a = 1; let b = 2").len(), 2);
        assert_eq!(parse_ok("let a = 1;").len(), 1);
    }

    #[test]
    fn run_on_statements_are_rejected() {
        let err = parse_err("let a = 1 let b = 2");
        assert!(
            err.message.contains("expected a newline"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_three_binding_keywords_share_a_node() {
        let stmts = parse_ok("let a = 1\nfinal b = 2\nconst c = 3");
        let kinds: Vec<_> = stmts
            .iter()
            .map(|stmt| match &stmt.kind {
                StmtKind::Let { bind, .. } => *bind,
                other => panic!("unexpected statement: {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [BindKind::Let, BindKind::Final, BindKind::Const],
            "the keyword should be the only difference"
        );
    }

    #[test]
    fn if_else_if_chains_nest() {
        let stmts = parse_ok("if a { } else if b { } else { }");
        let StmtKind::If { otherwise, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        let inner = otherwise.as_ref().expect("expected an else branch");
        let StmtKind::If { otherwise, .. } = &inner.kind else {
            panic!("expected `else if` to nest an if");
        };
        assert!(matches!(
            otherwise.as_ref().map(|s| &s.kind),
            Some(StmtKind::Block(_))
        ));
    }

    #[test]
    fn else_may_sit_on_its_own_line() {
        let stmts = parse_ok("if a {\n}\nelse {\n}");
        let StmtKind::If { otherwise, .. } = &stmts[0].kind else {
            panic!("expected an if");
        };
        assert!(otherwise.is_some());
    }

    #[test]
    fn loops_parse() {
        assert!(matches!(
            parse_ok("while a < 10 { a = a + 1 }")[0].kind,
            StmtKind::While { .. }
        ));
        let stmts = parse_ok("for item in [1, 2] { print(item) }");
        let StmtKind::For { var, .. } = &stmts[0].kind else {
            panic!("expected a for loop");
        };
        assert_eq!(var, "item");
    }

    #[test]
    fn return_may_omit_its_value() {
        let stmts = parse_ok("fn f() {\n  return\n}");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        assert!(matches!(decl.body.stmts[0].kind, StmtKind::Return(None)));
    }

    #[test]
    fn return_takes_a_value_on_the_same_line() {
        let stmts = parse_ok("fn f() { return 1 + 2 }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        let StmtKind::Return(Some(value)) = &decl.body.stmts[0].kind else {
            panic!("expected a returned value");
        };
        assert_eq!(sexpr(value), "(+ 1 2)");
    }

    #[test]
    fn functions_declare_parameters() {
        let stmts = parse_ok("fn add(a, b,) { return a + b }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a fn");
        };
        assert_eq!(decl.name, "add");
        let names: Vec<_> = decl.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    /// The methods of the one class in `src`.
    fn methods_of(src: &str) -> Vec<std::rc::Rc<FnDecl>> {
        let stmts = parse_ok(src);
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        methods.clone()
    }

    #[test]
    fn op_marks_a_method_the_language_calls() {
        let methods = methods_of("class C { op init(x) { self.x = x } }");
        assert_eq!(methods[0].name, "init");
        assert_eq!(methods[0].op, Some(Op::Init));
    }

    #[test]
    fn fn_leaves_a_method_ordinary_even_when_named_after_an_op() {
        // The whole point of marking: the name alone decides nothing, so this is
        // a method called as `c.init()` and nothing more.
        let methods = methods_of("class C { fn init(x) { self.x = x } }");
        assert_eq!(methods[0].name, "init");
        assert_eq!(methods[0].op, None);
    }

    #[test]
    fn a_misspelled_op_is_rejected_where_it_is_written() {
        let err = parse_err("class C { op innit(x) { self.x = x } }");
        assert!(
            err.message.contains("`innit` is not an operation"),
            "{}",
            err.message
        );
        // The list is the suggestion, so it has to be there.
        let help = err.help.expect("should say what op can define");
        assert!(help.contains("init"), "{help}");
        // Pointing at the name, not at the `op`.
        assert_eq!(&"class C { op innit(x) { self.x = x } }"[13..18], "innit");
        assert_eq!(err.span.start, 13);
    }

    #[test]
    fn an_op_declaring_the_wrong_number_of_parameters_is_rejected() {
        let src = "class C { op bool(a, b) { return true } }";
        let err = parse_err(src);
        assert!(
            err.message.contains("`op bool` takes 0 parameters, but 2"),
            "{}",
            err.message
        );
        // Pointing at the parameter list, which is the part to change.
        assert_eq!(&src[17..23], "(a, b)");
        assert_eq!(err.span.start, 17);
        assert_eq!(err.span.end, 23);

        // The count excludes `self`, so the one-parameter ops want exactly one
        // besides it — and the message says "parameter", not "parameters".
        let err = parse_err("class C { op add() { return 1 } }");
        assert!(
            err.message.contains("`op add` takes 1 parameter, but 0"),
            "{}",
            err.message
        );
    }

    /// `init` is the exception, and the only one.
    ///
    /// A constructor's parameters belong to the class. Checking them here would
    /// mean deciding how many arguments `Point(1, 2)` may pass, which is not the
    /// parser's to decide.
    #[test]
    fn an_op_init_may_declare_any_parameters() {
        for src in [
            "class C { op init() { } }",
            "class C { op init(a) { } }",
            "class C { op init(a, b, c) { } }",
        ] {
            let methods = methods_of(src);
            assert_eq!(methods[0].op, Some(Op::Init), "{src}");
        }
    }

    /// Every op is declarable at the arity the table gives it.
    ///
    /// Deliberately tautological about the *number* — it reads `arity()` to build
    /// the source, so it cannot tell a wrong number from a right one. What it
    /// catches is an op the check refuses at its own arity, and `self` being
    /// miscounted. `arity_is_what_the_language_passes` pins the numbers, and
    /// nothing can confirm them for real until each op is wired.
    #[test]
    fn every_op_can_be_declared_at_its_own_arity() {
        for op in crate::syntax::ast::OPS {
            let Some(arity) = op.arity() else { continue };
            let params: Vec<String> = (0..arity).map(|i| format!("p{i}")).collect();
            let src = format!(
                "class C {{ op {}({}) {{ }} }}",
                op.name(),
                params.join(", ")
            );
            let methods = methods_of(&src);
            assert_eq!(methods[0].op, Some(*op), "{src}");
            // `self` plus what the language passes.
            assert_eq!(methods[0].params.len(), arity + 1, "{src}");
        }
    }

    #[test]
    fn op_outside_a_class_is_rejected() {
        let err = parse_err("op init(x) { }");
        assert!(
            err.message.contains("only valid inside a class body"),
            "{}",
            err.message
        );
        assert!(err.help.is_some(), "should point at `fn`");
    }

    #[test]
    fn missing_brace_reports_where_it_was_expected() {
        let err = parse_err("if a  print(1) }");
        assert!(err.message.contains("expected `{`"), "{}", err.message);
    }

    #[test]
    fn spans_cover_the_whole_expression() {
        let src = "let x = 1 + 2 * 3";
        let stmts = parse_ok(src);
        let StmtKind::Let { value, .. } = &stmts[0].kind else {
            panic!("expected a let");
        };
        assert_eq!(
            &src[value.span.start as usize..value.span.end as usize],
            "1 + 2 * 3"
        );
        assert_eq!(
            &src[stmts[0].span.start as usize..stmts[0].span.end as usize],
            src
        );
    }

    #[test]
    fn parenthesised_spans_include_the_parens() {
        let src = "(1 + 2)";
        let stmts = parse_ok(src);
        assert_eq!(
            &src[stmts[0].span.start as usize..stmts[0].span.end as usize],
            src
        );
    }

    #[test]
    fn a_method_gets_self_as_its_first_parameter() {
        // The whole of what makes the receiver implicit: everything downstream
        // sees an ordinary parameter list.
        let stmts = parse_ok("class C {\n fn m(a, b) { return a }\n}");
        let StmtKind::Class { name, methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(name, "C");
        assert_eq!(methods.len(), 1);

        let params: Vec<_> = methods[0].params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["self", "a", "b"]);
    }

    #[test]
    fn a_plain_function_gets_no_self() {
        let stmts = parse_ok("fn f(a) { return a }");
        let StmtKind::Fn { decl, .. } = &stmts[0].kind else {
            panic!("expected a function");
        };
        let params: Vec<_> = decl.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(params, ["a"]);
    }

    #[test]
    fn self_parses_as_an_ordinary_variable() {
        let stmts = parse_ok("class C {\n fn m() { return self.x }\n}");
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        let StmtKind::Return(Some(expr)) = &methods[0].body.stmts[0].kind else {
            panic!("expected a return");
        };
        assert_eq!(sexpr(expr), "(. self x)");
    }

    #[test]
    fn a_superclass_is_a_name_like_any_other() {
        let stmts = parse_ok("class Dog extends Animal {}");
        let StmtKind::Class { name, parent, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(name, "Dog");
        assert_eq!(parent.as_ref().map(|p| p.name.as_str()), Some("Animal"));
    }

    #[test]
    fn super_must_be_followed_by_a_name() {
        // `super` alone has no useful value — the parent class is better named
        // directly — so requiring the `.name` puts the error on the `super`.
        assert_eq!(
            parse_err("class B extends A {\n fn m() { return super }\n}").message,
            "expected `.` after `super`, found `}`"
        );
    }

    #[test]
    fn every_modifier_marks_a_class_and_none_forbids_a_parent() {
        // A modifier says what may attach to the class from below and beside it,
        // never what it may descend from — so all three allow `extends`.
        for (src, expected) in [
            ("class Dog extends Animal {}", Openness::Open),
            ("final class Dog extends Animal {}", Openness::Final),
            ("complete class Dog extends Animal {}", Openness::Complete),
            ("sealed class Dog extends Animal {}", Openness::Sealed),
        ] {
            let stmts = parse_ok(src);
            let StmtKind::Class {
                name,
                parent,
                openness,
                ..
            } = &stmts[0].kind
            else {
                panic!("expected a class from `{src}`");
            };
            assert_eq!(name, "Dog");
            assert_eq!(*openness, expected, "`{src}`");
            assert_eq!(parent.as_ref().map(|p| p.name.as_str()), Some("Animal"));
        }
    }

    #[test]
    fn final_still_introduces_a_binding() {
        // `final` is the one modifier that is also a binding form, so it is the
        // one the parser has to look past `class` to tell apart. The lookahead is
        // a single token, and every other `final` goes where it always did.
        let stmts = parse_ok("final x = 1");
        let StmtKind::Let { name, bind, .. } = &stmts[0].kind else {
            panic!("expected a binding");
        };
        assert_eq!(name, "x");
        assert_eq!(*bind, BindKind::Final);

        // And a `final` in front of anything else is still a binding missing its
        // name, rather than a modifier the parser invented a meaning for.
        assert_eq!(
            parse_err("final extend int {}").message,
            "expected a name after `final`, found `extend`"
        );
    }

    #[test]
    fn a_modifier_has_nothing_to_say_without_a_class() {
        // `complete` and `sealed` introduce nothing else, so they need no
        // lookahead — and the error lands on what is missing rather than on a
        // binding the program never wrote.
        assert_eq!(
            parse_err("sealed x = 1").message,
            "expected `class` after `sealed`, found `x`"
        );
        assert_eq!(
            parse_err("complete fn f() {}").message,
            "expected `class` after `complete`, found `fn`"
        );
    }

    #[test]
    fn an_empty_class_is_allowed() {
        let stmts = parse_ok("class C {}");
        let StmtKind::Class { methods, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert!(methods.is_empty());
    }

    #[test]
    fn a_class_body_holds_fields_and_methods_and_nothing_else() {
        // A `let` is a field as of v0.7. Anything that is neither is still
        // refused, because it would otherwise parse as a statement and then
        // silently do nothing — there is nowhere in a class body for it to go.
        assert_eq!(
            parse_err("class C {\n print(1)\n}").message,
            "expected a field or a method, found print"
        );
    }

    #[test]
    fn a_class_body_declares_fields_with_a_visibility() {
        let stmts = parse_ok(
            "class C {\n let a = 1\n private final b = 2\n protected const c = 3\n fn m() {}\n}",
        );
        let StmtKind::Class {
            methods, fields, ..
        } = &stmts[0].kind
        else {
            panic!("expected a class");
        };
        assert_eq!(methods.len(), 1);
        let seen: Vec<_> = fields
            .iter()
            .map(|field| (field.name.as_str(), field.bind, field.visibility))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("a", BindKind::Let, Visibility::Public),
                ("b", BindKind::Final, Visibility::Private),
                ("c", BindKind::Const, Visibility::Protected),
            ]
        );
    }

    #[test]
    fn an_op_may_not_be_hidden_from_the_language_that_calls_it() {
        // `public op` says nothing new and is allowed; the two that restrict
        // would make a method `print` is entitled to call and forbidden from
        // calling.
        parse_ok("class C {\n public op string() { return \"c\" }\n}");
        for word in ["private", "protected"] {
            let src = format!("class C {{\n {word} op string() {{ return \"c\" }}\n}}");
            assert_eq!(
                parse_err(&src).message,
                format!("an `op` may not be {word}")
            );
        }
    }

    #[test]
    fn a_visibility_word_belongs_to_a_top_level_declaration() {
        // Inside a function there is no importing module to hide a name from, so
        // the word would do nothing — and a modifier that does nothing is worse
        // than one that is refused.
        assert_eq!(
            parse_err("fn f() {\n private let x = 1\n}").message,
            "`private` means nothing here"
        );
        // At the top level all three declaration forms take one.
        parse_ok("public let a = 1\nprivate fn f() {}\npublic class C {}\nprivate final class D {}");
    }

    #[test]
    fn a_visibility_word_needs_something_to_modify() {
        assert_eq!(
            parse_err("public 1 + 1").message,
            "expected a declaration after `public`, found 1"
        );
    }

    #[test]
    fn parses_the_hello_example() {
        let src = include_str!("../../../examples/hello.qn");
        let stmts = parse_ok(src);
        assert_eq!(stmts.len(), 3, "fn, let, if");
        assert!(matches!(stmts[0].kind, StmtKind::Fn { .. }));
        assert!(matches!(stmts[1].kind, StmtKind::Let { .. }));
        assert!(matches!(stmts[2].kind, StmtKind::If { .. }));
    }

    #[test]
    fn exponentiation_associates_to_the_right() {
        // The one binary operator in the language that does not associate left,
        // and it differs because left association would make it useless for
        // what it is for.
        assert_eq!(expr_of("2 ** 3 ** 2"), "(** 2 (** 3 2))");
        assert_eq!(expr_of("2 * 3 ** 2"), "(* 2 (** 3 2))");
        assert_eq!(expr_of("2 ** 3 * 2"), "(* (** 2 3) 2)");
    }

    #[test]
    fn exponentiation_binds_tighter_than_unary_minus() {
        // `-2 ** 2` is `-(2 ** 2)`, following Python and ordinary mathematical
        // notation. The other reading is written with parentheses.
        assert_eq!(expr_of("-2 ** 2"), "(- (** 2 2))");
        assert_eq!(expr_of("(-2) ** 2"), "(** (- 2) 2)");
        // The exponent is a full unary expression, so a negative one needs none.
        assert_eq!(expr_of("2 ** -1"), "(** 2 (- 1))");
    }

    #[test]
    fn a_compound_assignment_keeps_its_target_whole() {
        // One node rather than a rewrite into `a = a op b`, because the rule is
        // that the target is evaluated *once* — a tree mentioning it twice
        // could not say that.
        assert_eq!(expr_of("n += 1"), "(+= n 1)");
        assert_eq!(expr_of("d[k] //= 2"), "(//= (index d k) 2)");
        assert_eq!(expr_of("obj.total **= 2"), "(**= (. obj total) 2)");
        // The right side is a whole expression, as it is for `=`.
        assert_eq!(expr_of("n *= 1 + 2"), "(*= n (+ 1 2))");
    }

    #[test]
    fn a_compound_assignment_needs_somewhere_to_write() {
        let err = parse_err("1 += 2");
        assert!(
            err.message.contains("cannot assign"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_short_circuiting_assignments_are_their_own_node() {
        // Not a fourteenth `AssignOp`: `a ??= b` may leave `b` unevaluated and
        // may write nothing at all, which is not what `a = a op b` describes.
        assert_eq!(expr_of("n ??= 0"), "(??= n 0)");
        assert_eq!(expr_of("flag and= ready"), "(and= flag ready)");
        assert_eq!(expr_of("flag or= ready"), "(or= flag ready)");
        // The target may be an index or a field, as for every other assignment.
        assert_eq!(expr_of("d[k] ??= 0"), "(??= (index d k) 0)");
        assert_eq!(expr_of("obj.total ??= 0"), "(??= (. obj total) 0)");
        // And the right side is a whole expression.
        assert_eq!(expr_of("n ??= 1 + 2"), "(??= n (+ 1 2))");
    }

    #[test]
    fn the_negated_operators_are_the_plain_ones_negated() {
        // `not in` and `is not` build a `Not` over the node that already exists
        // rather than a second node meaning the opposite, so every pass that
        // understands `in` and `is` needs no change to understand these.
        assert_eq!(expr_of("k not in d"), "(not (in k d))");
        assert_eq!(expr_of("v is not string"), "(not (is v string))");
        // They bind where their positive forms bind.
        assert_eq!(expr_of("a + 1 not in b"), "(not (in (+ a 1) b))");
        assert_eq!(expr_of("k not in d and x"), "(and (not (in k d)) x)");
        // A `not` that is not followed by `in` is still the prefix operator —
        // and reaches the same tree, because `not` binds looser than a
        // comparison. The two ways of writing it agree, which is the point.
        assert_eq!(expr_of("not a in b"), "(not (in a b))");
        assert_eq!(expr_of("not v is string"), "(not (is v string))");
    }

    #[test]
    fn not_binds_looser_than_a_comparison_and_tighter_than_and() {
        // The one unary operator that is not at `UNARY_BP`. `not a == b` asks
        // whether `a` and `b` differ, which is what it reads as — where `!` sat
        // it would have compared the negation of `a` against `b`.
        assert_eq!(expr_of("not a == b"), "(not (== a b))");
        assert_eq!(expr_of("not a and b"), "(and (not a) b)");
        assert_eq!(expr_of("not a or not b"), "(or (not a) (not b))");
        assert_eq!(expr_of("not a + b"), "(not (+ a b))");
        // `-` and `~` are symbols and stay where they were.
        assert_eq!(expr_of("-a == b"), "(== (- a) b)");
        assert_eq!(expr_of("~a & b"), "(& (~ a) b)");
    }

    #[test]
    fn the_increments_are_statements_and_desugar_to_a_compound_assignment() {
        // Both spellings, both meaning `n += 1`. They differ in C by what they
        // evaluate to, and neither evaluates to anything here — so there is
        // nothing left for the distinction to be about.
        assert_eq!(expr_of("n++"), "(+= n 1)");
        assert_eq!(expr_of("++n"), "(+= n 1)");
        assert_eq!(expr_of("n--"), "(-= n 1)");
        assert_eq!(expr_of("--n"), "(-= n 1)");
        // Which also means the target is evaluated once for free, since that is
        // the rule the compound assignment already carries.
        assert_eq!(expr_of("d[k]++"), "(+= (index d k) 1)");
        assert_eq!(expr_of("obj.total--"), "(-= (. obj total) 1)");
    }

    #[test]
    fn an_increment_is_refused_inside_an_expression() {
        // The whole reason they are statements: `x = i++` has no answer that is
        // not a puzzle, so it is refused rather than given one.
        for src in ["let x = i++", "f(i++)", "if i++ > 3 { }", "print(++i)"] {
            let err = parse_err(src);
            assert!(
                err.message.contains("is a statement on its own"),
                "`{src}` should be refused as an expression, got: {}",
                err.message
            );
        }
    }

    #[test]
    fn an_increment_on_the_next_line_belongs_to_that_line() {
        // The rule a `(` on a fresh line already follows: without it, `let a = b`
        // followed by `++c` would silently count `b` up.
        let stmts = parse_ok("let a = b\n++c");
        assert_eq!(stmts.len(), 2);
        assert!(matches!(stmts[0].kind, StmtKind::Let { .. }));
    }

    #[test]
    fn a_class_header_takes_a_parameter_list() {
        let stmts = parse_ok("class Stack[T] { }");
        let StmtKind::Class { params, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].name, "T");

        let stmts = parse_ok("public final class Pair[A, B,] { }");
        let StmtKind::Class { params, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        // The trailing comma every other bracketed list allows.
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["A", "B"]);

        // And a class without one is unchanged.
        let stmts = parse_ok("class Point { }");
        let StmtKind::Class { params, .. } = &stmts[0].kind else {
            panic!("expected a class");
        };
        assert!(params.is_empty());
    }

    #[test]
    fn a_parameter_list_declares_names_rather_than_writing_types() {
        // The likeliest mistake is writing a *use* where a declaration goes.
        // It parses — a builtin type name is an ordinary identifier — so it has
        // to be refused rather than failing to lex.
        let err = parse_err("class Stack[int] { }");
        assert!(
            err.message.contains("`int` is a type"),
            "{}",
            err.message
        );

        // And something that cannot be a name at all is the other report.
        let err = parse_err("class Stack[3] { }");
        assert!(
            err.message.contains("expected a type parameter"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_comma_inside_brackets_supplies_type_arguments() {
        // One argument cannot be told from a subscript and stays an `Index` —
        // the target decides, at run time. Two can only be one thing.
        assert_eq!(expr_of("Pair[int, string]"), "(type-args Pair int string)");
        assert_eq!(expr_of("Stack[int]"), "(index Stack int)");
        assert_eq!(expr_of("xs[i]"), "(index xs i)");
    }

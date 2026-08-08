use super::*;

use quince::syntax::token::{KEYWORDS, TokenKind};
use rustyline::completion::Completer as _;
use rustyline::hint::Hinter as _;

use crate::repl::helper::is_input_incomplete;
use crate::repl::highlight::highlight_token;

/// A REPL that has already run `code`, and the snapshot it would offer from.
fn after(code: &str) -> Snapshot {
    let mut interp = Interp::new();
    let program = quince::compile(code).expect("the test program compiles");
    interp.run_repl(&program).expect("it runs");
    Snapshot::of(&interp)
}

/// What a dot after `before` would offer, by name.
fn offered(snapshot: &Snapshot, before: &str) -> Vec<String> {
    let mut names: Vec<String> = snapshot
        .members_after(before)
        .into_iter()
        .map(|symbol| symbol.name)
        .collect();
    names.sort();
    names
}

/// What a REPL that has run `code` would offer for `line`.
fn completed(code: &str, line: &str) -> Vec<String> {
    let mut interp = Interp::new();
    let program = quince::compile(code).expect("the test program compiles");
    interp.run_repl(&program).expect("it runs");
    let helper = QuinceHelper {
        use_color: false,
        snapshot: Arc::new(Mutex::new(Snapshot::of(&interp))),
    };
    let history = rustyline::history::MemHistory::new();
    let context = rustyline::Context::new(&history);
    let (_, matches) = helper
        .complete(line, line.len(), &context)
        .expect("completion works");
    matches.into_iter().map(|pair| pair.replacement).collect()
}

#[test]
fn an_import_line_offers_modules_then_their_members() {
    // Two positions wanting two lists. `from math import ` used to offer
    // `math, io, time, random` — the four things that cannot go there.
    let modules = completed("let unused = 1", "import ");
    assert!(modules.contains(&"math".to_string()), "{modules:?}");
    assert!(!modules.contains(&"floor".to_string()), "{modules:?}");

    let members = completed("let unused = 1", "from math import ");
    assert!(members.contains(&"floor".to_string()), "{members:?}");
    assert!(members.contains(&"pi".to_string()), "{members:?}");
    assert!(!members.contains(&"math".to_string()), "{members:?}");
    // `io`'s members belong to `io`.
    assert!(!members.contains(&"read".to_string()), "{members:?}");

    // And every name after the first, so a list keeps completing.
    let more = completed("let unused = 1", "from math import abs, ");
    assert!(more.contains(&"floor".to_string()), "{more:?}");

    // Still naming the module, so still modules.
    let naming = completed("let unused = 1", "from ma");
    assert!(naming.contains(&"math".to_string()), "{naming:?}");
}

#[test]
fn an_imported_name_keeps_what_the_module_knew_about_it() {
    let mut interp = Interp::new();
    let program = quince::compile("from math import floor, pi").expect("it compiles");
    interp.run_repl(&program).expect("it runs");
    let snapshot = Snapshot::of(&interp);

    let floor = snapshot
        .globals
        .iter()
        .find(|symbol| symbol.name == "floor")
        .expect("`floor` is bound");
    // The short spelling is not the worse one: `floor` carries the
    // parameters and documentation `math.floor` carries.
    assert_eq!(floor.signature(), "fn floor(n): int");
    assert!(floor.doc.is_some(), "an imported native keeps its documentation");

    let pi = snapshot
        .globals
        .iter()
        .find(|symbol| symbol.name == "pi")
        .expect("`pi` is bound");
    assert_eq!(pi.signature(), "pi: float");
}

#[test]
fn a_from_import_does_not_bind_the_module() {
    // `from math import floor` binds `floor` and nothing else, so `math.`
    // reaches nothing — which is what the language does, and the completion
    // list says so rather than papering over it.
    let names = completed("from math import floor", "math.");
    assert!(names.is_empty(), "{names:?}");

    let bound = completed("import io", "io.");
    assert!(bound.contains(&"read".to_string()), "{bound:?}");
}

#[test]
fn completion_offers_the_methods_that_exist_and_no_others() {
    // The list this replaced was written from memory: it offered `pop`,
    // `insert`, `clear`, `slice`, `contains`, and Rust's `to_uppercase`,
    // none of which are Quince methods, and omitted `chars`, `upper`, and
    // `lower`, which are. It now comes off the live class objects, so a
    // method the language does not have cannot be offered at all.
    let names = offered(&after("let s = \"a\""), "s");

    for real in ["chars", "upper", "lower", "join", "split"] {
        assert!(names.contains(&real.to_string()), "{real} should be offered");
    }
    for fake in ["pop", "insert", "clear", "to_uppercase", "len", "push"] {
        assert!(!names.contains(&fake.to_string()), "{fake} is not a string method");
    }
}

#[test]
fn completion_answers_from_the_value_that_is_actually_bound() {
    // The REPL's whole advantage over the editor: `words` is a list because
    // the value under that name is one, which is not a guess and cannot be
    // wrong.
    let snapshot = after("let words = \"a,b\".split(\",\")");
    let names = offered(&snapshot, "words");
    assert!(names.contains(&"push".to_string()), "{names:?}");
    assert!(!names.contains(&"lower".to_string()), "{names:?}");
}

#[test]
fn completion_follows_a_chain_through_what_a_method_returns() {
    let snapshot = after("let words = [\"b\", \"a\"]");
    let names = offered(&snapshot, "words.sort()");
    assert!(names.contains(&"map".to_string()), "{names:?}");
}

#[test]
fn completion_types_a_literal_without_anything_being_bound() {
    let snapshot = after("let unused = 1");
    assert!(offered(&snapshot, "\"abc\"").contains(&"upper".to_string()));
    assert!(offered(&snapshot, "[1, 2]").contains(&"push".to_string()));
}

#[test]
fn completion_offers_an_extensions_methods() {
    // `extend` puts its methods beside a class rather than in it, so they
    // were callable and never offered — the old map read `Class::methods`
    // and stopped there.
    let snapshot = after("extend list {\n  fn second() { return self[1] }\n}\nlet xs = [1, 2]");
    let names = offered(&snapshot, "xs");
    assert!(names.contains(&"second".to_string()), "{names:?}");
    assert!(names.contains(&"push".to_string()), "{names:?}");
}

#[test]
fn completion_offers_nothing_for_a_receiver_it_cannot_identify() {
    // This used to answer with every method of every type — forty names of
    // which two applied, which is a list nobody can read.
    let snapshot = after("let unused = 1");
    assert!(offered(&snapshot, "mystery").is_empty());
}

#[test]
fn all_keywords_are_highlighted() {
    for kw in KEYWORDS {
        if let Some(kind) = TokenKind::keyword(kw) {
            let styled = highlight_token(kind.clone(), kw, true, None, None);

            if kw == &"self" || kw == &"super" {
                assert!(styled.contains("\x1b[1;36m"), "{kw} should be bold cyan");
            } else if kw == &"true" || kw == &"false" {
                assert!(styled.contains("\x1b[33m"), "{kw} should be yellow");
            } else if kw == &"nil" {
                assert!(styled.contains("\x1b[2m"), "{kw} should be dim");
            } else {
                assert!(styled.contains("\x1b[1;35m"), "{kw} should be bold magenta");
            }
        }
    }
}

#[test]
fn context_aware_syntax_highlighting_differentiates_identifiers() {
    let fn_decl = highlight_token(
        TokenKind::Ident("calculate".to_string()),
        "calculate",
        true,
        Some(&TokenKind::Fn),
        None,
    );
    assert!(
        fn_decl.contains("\x1b[1;36m"),
        "fn name should be bold cyan"
    );

    let class_decl = highlight_token(
        TokenKind::Ident("Point".to_string()),
        "Point",
        true,
        Some(&TokenKind::Class),
        None,
    );
    assert!(
        class_decl.contains("\x1b[1;33m"),
        "class name should be bold yellow"
    );

    let call = highlight_token(
        TokenKind::Ident("foo".to_string()),
        "foo",
        true,
        None,
        Some(&TokenKind::LParen),
    );
    assert!(
        call.contains("\x1b[1;34m"),
        "function call should be bold blue"
    );

    let builtin = highlight_token(
        TokenKind::Ident("print".to_string()),
        "print",
        true,
        None,
        None,
    );
    assert!(
        builtin.contains("\x1b[1;36m"),
        "builtin function should be bold cyan"
    );
}

#[test]
fn validator_detects_incomplete_expressions() {
    assert!(is_input_incomplete("1 +"));
    assert!(is_input_incomplete("fn foo() {"));
    assert!(is_input_incomplete("print([1, 2,"));
    assert!(is_input_incomplete("\"unterminated string"));
    assert!(!is_input_incomplete("1 + 2"));
    assert!(!is_input_incomplete("let x = 10"));
}

#[test]
fn completion_offers_a_modules_members() {
    let snapshot = after("import math");
    let names = offered(&snapshot, "math");
    assert!(names.contains(&"floor".to_string()), "{names:?}");
    assert!(names.contains(&"pi".to_string()), "{names:?}");
    // And only that module's. `read` belongs to `io`.
    assert!(!names.contains(&"read".to_string()), "{names:?}");
}

#[test]
fn a_class_object_offers_methods_and_no_fields() {
    // `Dog.bark` reaches the method — `print(Dog.bark)` writes `<fn bark>`
    // — so a dot on the class lists them. `Dog.breed` reaches nothing: a
    // field exists because an instance assigned it.
    let snapshot = after(
        "class Dog {\n  op init() { self.breed = \"collie\" }\n  fn bark() { return \"woof\" }\n}\nlet d = Dog()",
    );
    let on_class = offered(&snapshot, "Dog");
    assert!(on_class.contains(&"bark".to_string()), "{on_class:?}");
    assert!(!on_class.contains(&"breed".to_string()), "{on_class:?}");

    let on_instance = offered(&snapshot, "d");
    assert!(on_instance.contains(&"bark".to_string()), "{on_instance:?}");
    assert!(on_instance.contains(&"breed".to_string()), "{on_instance:?}");
}

#[test]
fn context_aware_method_completion_filters_by_type() {
    let snapshot = after("let s = \"a\"\nlet xs = [1]\nlet d = {\"k\": 1}");
    let strings = offered(&snapshot, "s");
    assert!(strings.contains(&"upper".to_string()));
    assert!(!strings.contains(&"push".to_string()));
    assert!(!strings.contains(&"keys".to_string()));

    let lists = offered(&snapshot, "xs");
    assert!(lists.contains(&"push".to_string()));
    assert!(!lists.contains(&"upper".to_string()));

    let dicts = offered(&snapshot, "d");
    assert!(dicts.contains(&"keys".to_string()));
    assert!(!dicts.contains(&"push".to_string()));
}

#[test]
fn subclass_methods_and_variables_are_completed_and_hinted() {
    let mut interp = Interp::new();
    let code = r#"
        class Animal {
            fn speak() { return "..." }
        }
        class Dog extends Animal {
            op init() {
                self.breed = "collie"
            }
            fn bark() { return "woof" }
        }
        let d = Dog()
    "#;
    let program = quince::compile(code).expect("the test program compiles");
    interp.run_repl(&program).expect("it runs");

    let helper = QuinceHelper {
        use_color: false,
        snapshot: Arc::new(Mutex::new(Snapshot::of(&interp))),
    };
    let history = rustyline::history::MemHistory::new();
    let context = rustyline::Context::new(&history);

    // An instance offers its own methods, its parent's, and the fields the
    // `init` that ran actually assigned.
    let (start, matches) = helper.complete("d.", 2, &context).expect("completion works");
    assert_eq!(start, 2);
    let offered: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
    for expected in ["bark", "speak", "breed"] {
        assert!(offered.contains(&expected.to_string()), "{offered:?}");
    }

    assert_eq!(helper.hint("d.b", 3, &context), Some("ark".to_string()));
    assert_eq!(helper.hint("d.s", 3, &context), Some("peak".to_string()));
    assert_eq!(helper.hint("d.br", 4, &context), Some("eed".to_string()));

    // And a class object answers about the instances it makes.
    let (_, matches) = helper.complete("Dog.", 4, &context).expect("completion works");
    let offered: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
    assert!(offered.contains(&"bark".to_string()), "{offered:?}");
    assert!(offered.contains(&"speak".to_string()), "{offered:?}");
}

#[test]
fn repl_binds_last_value_to_underscore() {
    let mut interp = Interp::new();
    let program = quince::compile("10 + 20").unwrap();
    if let Ok(Some(val)) = interp.run_repl(&program) {
        interp.set_global("_", val);
    }
    let check_pgm = quince::compile("_ * 2").unwrap();
    let res = interp.run_repl(&check_pgm).unwrap();
    assert_eq!(res, Some(Value::Int(60)));
}

#[test]
fn a_repl_entry_may_redefine_what_an_earlier_one_declared() {
    // Declaring a name twice in one program is refused. This is why that
    // refusal is per-compile and not per-process: at a prompt, writing the
    // function again *is* how you fix it, and a REPL that made you restart
    // over a typo would be the worse tool.
    let mut interp = Interp::new();
    for source in ["fn a() { return 1 }", "fn a() { return 2 }"] {
        let program = quince::compile(source).expect("each entry compiles on its own");
        interp.run_repl(&program).expect("and runs");
    }
    let call = quince::compile("a()").unwrap();
    assert_eq!(interp.run_repl(&call).unwrap(), Some(Value::Int(2)));
}

/// Feeds each line to one interpreter as its own entry, answering with the
/// value of the last.
///
/// Through `compile_within`, which is what the REPL loop itself uses: an entry
/// is resolved against what the session already has bound, so the declaration
/// rules see the lines before it. Reaching for `compile` here would test a
/// pipeline the REPL does not have.
fn entries(lines: &[&str]) -> Option<quince::runtime::value::Value> {
    let mut interp = Interp::new();
    let mut last = None;
    for line in lines {
        let program = quince::compile_within(line, &interp.declarations())
            .unwrap_or_else(|err| panic!("`{line}` should compile: {}", err.message));
        last = interp.run_repl(&program).expect("it runs");
    }
    last
}

/// The message the first line to be refused produced.
fn refused(lines: &[&str]) -> String {
    let mut interp = Interp::new();
    for line in lines {
        match quince::compile_within(line, &interp.declarations()) {
            Ok(program) => {
                interp.run_repl(&program).expect("it runs");
            }
            Err(err) => return err.message,
        }
    }
    panic!("no line was refused");
}

#[test]
fn overloads_typed_on_separate_lines_join_and_a_repeat_replaces() {
    // A REPL line is its own compilation, so two overloads typed on two lines
    // each arrive as a *first* declaration. What makes them one name holding two
    // declarations is that the entry is resolved against what is already bound —
    // the same parameter types replace, different ones join.
    use quince::runtime::value::Value;
    let declared = ["fn f(a: int): int { return 1 }", "fn f(s: string): int { return 2 }"];
    assert_eq!(entries(&[declared[0], declared[1], "f(1)"]), Some(Value::Int(1)));
    assert_eq!(entries(&[declared[0], declared[1], "f(\"x\")"]), Some(Value::Int(2)));

    let again = "fn f(a: int): int { return 99 }";
    assert_eq!(
        entries(&[declared[0], declared[1], again, "f(1)"]),
        Some(Value::Int(99)),
        "the same signature is replaced"
    );
    assert_eq!(
        entries(&[declared[0], declared[1], again, "f(\"x\")"]),
        Some(Value::Int(2)),
        "the other one is untouched"
    );
}

#[test]
fn an_entry_may_not_add_an_overload_nothing_could_choose_between() {
    // The half a redefinition is not. Retyping a declaration to change it means
    // writing the same parameter types; writing *different* ones that some call
    // would reach equally well is adding an ambiguity, and is refused here for
    // the reason it is refused in a file.
    assert_eq!(
        refused(&["fn f(a: float): int { return 1 }", "fn f(a: int?): int { return 2 }"]),
        "an earlier entry declares a `f` this one cannot be told apart from"
    );
}

#[test]
fn the_declaration_rules_see_the_lines_before_them() {
    // Every rule that reads a hierarchy used to decline to answer in the REPL,
    // because each entry was resolved against an empty world. A class declared
    // on one line is a class the next line can see.
    assert_eq!(
        refused(&[
            "class Animal { fn speak(): string { return \"...\" } }",
            "class Dog extends Animal { fn speak(): string { return \"woof\" } }",
        ]),
        "`fn speak` replaces `Animal`'s and does not say so"
    );
    assert_eq!(
        refused(&[
            "class A { final fn m(): int { return 1 } }",
            "class B extends A { override fn m(): int { return 2 } }",
        ]),
        "cannot override `fn m`, which is final in `A`"
    );
    assert_eq!(
        refused(&[
            "class Money { op init(cents: int) { self.cents = cents } }",
            "let m: Money",
        ]),
        "`Money` has no default constructor, so `m` needs an initializer"
    );
    // And a class's own methods are told apart from the ones it merged in from a
    // superclass, so a grandchild is reported against whichever class wrote the
    // member rather than against whichever one it happened to reach.
    assert_eq!(
        refused(&[
            "class A { fn m(): int { return 1 } }",
            "class B extends A {}",
            "class C extends B { fn m(): int { return 3 } }",
        ]),
        "`fn m` replaces `A`'s and does not say so"
    );
}

#[test]
fn redefining_is_still_what_a_repl_is_for() {
    // The property every one of the rules above has to leave alone. A REPL entry
    // redeclaring a name is the ordinary thing to do, and `globals` — the set
    // that refuses a name declared twice — is deliberately not seeded.
    use quince::runtime::value::Value;
    assert_eq!(entries(&["let x = 1", "let x = 2", "x"]), Some(Value::Int(2)));
    assert_eq!(
        entries(&[
            "class P { fn m(): int { return 1 } }",
            "class P { fn m(): int { return 7 } }",
            "P().m()",
        ]),
        Some(Value::Int(7))
    );
}

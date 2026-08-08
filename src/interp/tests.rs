use super::*;

use crate::builtins::convert::checked_trunc;
use crate::runtime::class::Builtin;
use crate::runtime::dict::Key;
use crate::runtime::heap::Object;
use crate::runtime::value::Value;
use crate::syntax::ast::Op;
use crate::syntax::token::Span;

fn global(interp: &Interp, name: &str) -> Option<Value> {
    interp.heap.globals(interp.globals).get(name).cloned()
}

fn run(source: &str) -> Interp {
    let program = crate::compile(source).expect("the test program should parse");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    interp.run(&program).expect("the test program should run");
    interp
}

/// The list `map` is building is rooted while the callback runs.
///
/// `map` is the first native to call Quince code, which makes it the first
/// to cross a safe point with something of its own on the Rust stack. The
/// receiver, the function, the element in flight, and the list being filled
/// are all on `temps` for that reason — and the list is the one nothing else
/// can reach, since it is not bound to a name until `map` returns.
///
/// The churn is *inside* the callback, and that detail is the test. A first
/// version churned before the map and passed with the rooting deleted: the
/// collections it counted had all happened already, and by the time the map
/// ran the threshold had been raised past what eight small allocations could
/// reach. Allocating during each call is what puts a real collection between
/// two pushes into the list being built.
///
/// Checked by deleting the `temps.push` of `out` in `walk_list`, which makes
/// this panic at `handle points at a collected object`.
#[test]
fn the_list_being_mapped_into_survives_collection() {
    let interp = run("fn churn(k) {\n\
         \x20   let scratch = []\n\
         \x20   let i = 0\n\
         \x20   while i < k {\n\
         \x20       scratch.push([i])\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         }\n\
         fn wrap(x) {\n\
         \x20   churn(400)\n\
         \x20   let boxed = [x, x]\n\
         \x20   return boxed\n\
         }\n\
         let source = [1, 2, 3, 4, 5, 6, 7, 8]\n\
         let mapped = source.map(wrap)\n\
         let n = len(mapped)\n\
         let third = mapped[2]");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        matches!(global(&interp, "n"), Some(Value::Int(8))),
        "the mapped list did not survive: got {:?}",
        global(&interp, "n")
    );
    // Reaching into an element proves the results survived too, not just the
    // list holding them.
    let Some(Value::List(id)) = global(&interp, "third") else {
        panic!("the third element should be a list");
    };
    assert_eq!(interp.heap.list(id), &[Value::Int(3), Value::Int(3)]);
}

/// A constructor's arguments are rooted while the class's field initializers run.
///
/// A field initializer is arbitrary Quince code — `let left: Node = Leaf()` is
/// a call — so it reaches a safe point, and it runs *between* the arguments
/// being evaluated and `op init` binding them into its scope. In that window
/// the arguments live only in a Rust `Vec`, which the collector cannot see.
///
/// The shape below is the smallest one that has all three parts: `Pair` has a
/// field initializer that calls something, `wrap` hands it a freshly allocated
/// argument that no name holds, and `churn` inside the initializer forces a
/// real collection while that argument is in flight.
///
/// Checked by deleting the `temps.extend(args…)` in `Interp::call`'s instance
/// branch, which makes this panic at `handle points at a collected object`
/// inside the parameter type check — the first thing to look at the argument
/// after the collection swept it.
#[test]
fn a_constructors_arguments_survive_its_field_initializers() {
    let interp = run("fn churn(k) {\n\
         \x20   let scratch = []\n\
         \x20   let i = 0\n\
         \x20   while i < k {\n\
         \x20       scratch.push([i])\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         }\n\
         class Leaf {\n\
         \x20   op init() { churn(400) }\n\
         }\n\
         class Pair {\n\
         \x20   let filler: Leaf = Leaf()\n\
         \x20   let items: list\n\
         \x20   op init(items: list) { self.items = items }\n\
         }\n\
         fn wrap(a, b) {\n\
         \x20   return Pair([a, b])\n\
         }\n\
         let made = wrap(1, 2)\n\
         let held = made.items\n\
         let n = len(held)");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        matches!(global(&interp, "n"), Some(Value::Int(2))),
        "the constructor argument did not survive: got {:?}",
        global(&interp, "n")
    );
    let Some(Value::List(id)) = global(&interp, "held") else {
        panic!("the argument should still be a list");
    };
    assert_eq!(interp.heap.list(id), &[Value::Int(1), Value::Int(2)]);
}

/// `io`'s file half, which the corpus cannot hold.
///
/// A case writes nothing: `tests/cases` is checked in, and a suite that
/// leaves files behind — or that races another case over the same name —
/// stops being something anyone trusts. Here a temp directory named for the
/// test is cheap, and the round trip is what actually needs proving.
#[test]
fn io_reads_back_what_it_wrote() {
    let dir = std::env::temp_dir().join("quince-io-roundtrip");
    std::fs::create_dir_all(&dir).expect("a temp directory should be creatable");
    let path = dir.join("notes.txt");
    let _ = std::fs::remove_file(&path);
    // Escaped, because a Windows path is full of backslashes and the lexer
    // reads `\n` in a string literal as a newline wherever it finds one.
    let quoted = path.display().to_string().replace('\\', "\\\\");

    let interp = run(&format!(
        "import io\n\
         let path = \"{quoted}\"\n\
         let missing = io.exists(path)\n\
         io.write(path, \"alpha\\nbeta\\n\")\n\
         let there = io.exists(path)\n\
         let whole = io.read(path)\n\
         io.append(path, \"gamma\\n\")\n\
         let lines = io.lines(path)\n\
         let count = len(lines)\n\
         let last = lines[2]"
    ));

    assert_eq!(global(&interp, "missing"), Some(Value::Bool(false)));
    assert_eq!(global(&interp, "there"), Some(Value::Bool(true)));
    assert_eq!(global(&interp, "whole"), Some(Value::from("alpha\nbeta\n")));
    // Three lines, not four: a trailing newline ends the last one rather
    // than starting an empty one.
    assert_eq!(global(&interp, "count"), Some(Value::Int(3)));
    assert_eq!(global(&interp, "last"), Some(Value::from("gamma")));

    std::fs::remove_file(&path).expect("the test's own file should be removable");
}

#[test]
fn reading_a_file_that_is_not_there_is_catchable() {
    // The point of `ErrorKind::Io` having a class: a missing file is an
    // ordinary thing to happen to a running program, so a program can decide
    // what to do about it rather than being ended by it.
    let missing = std::env::temp_dir().join("quince-does-not-exist-9f3c.txt");
    let quoted = missing.display().to_string().replace('\\', "\\\\");
    let interp = run(&format!(
        "import io\n\
         let caught = nil\n\
         try {{\n\
         \x20   io.read(\"{quoted}\")\n\
         }} catch e {{\n\
         \x20   caught = type(e)\n\
         }}"
    ));
    assert_eq!(global(&interp, "caught"), Some(Value::from("IoError")));
}

/// Builds a list on the heap and hands back both halves, since a test that
/// makes a cycle needs the handle as well as the value.
fn list(interp: &mut Interp, items: Vec<Value>) -> (ObjId, Value) {
    let id = interp.heap.alloc(Object::List(items));
    (id, Value::List(id))
}

fn push(interp: &mut Interp, id: ObjId, value: Value) {
    interp
        .heap
        .list_mut(id)
        .expect("never frozen here")
        .push(value);
}

#[test]
fn numbers_compare_across_int_and_float() {
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    assert!(interp.equals(&Value::Int(1), &Value::Float(1.0)).unwrap());
    assert!(interp.equals(&Value::Float(1.0), &Value::Int(1)).unwrap());
    assert!(!interp.equals(&Value::Int(1), &Value::Float(1.5)).unwrap());
}

#[test]
fn unrelated_types_are_never_equal() {
    // Strong typing: no coercion sneaks in through `==`.
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    assert!(!interp.equals(&Value::Int(1), &Value::from("1")).unwrap());
    assert!(!interp.equals(&Value::Int(1), &Value::Bool(true)).unwrap());
    assert!(!interp.equals(&Value::Nil, &Value::Bool(false)).unwrap());
}

#[test]
fn lists_compare_structurally() {
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let (_, a) = list(&mut interp, vec![Value::Int(1), Value::from("x")]);
    let (_, b) = list(&mut interp, vec![Value::Int(1), Value::from("x")]);
    let (_, c) = list(&mut interp, vec![Value::Int(2)]);
    assert!(interp.equals(&a, &b).unwrap());
    assert!(!interp.equals(&a, &c).unwrap());
}

/// Guards the self-referential case from running forever — for the one shape
/// it can. Two distinct cycles still overflow the stack; see `equals`.
#[test]
fn identical_handles_short_circuit_comparison() {
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let (id, value) = list(&mut interp, vec![]);
    push(&mut interp, id, value.clone());
    assert!(interp.equals(&value, &value).unwrap());
}

#[test]
fn a_type_name_is_a_global_unless_the_lexer_claimed_it() {
    // The exception set is derived from `TokenKind::keyword`, so it can grow
    // without anyone touching this file — a type named after a future keyword
    // would silently stop being bound. Pinned here so that becomes a failure,
    // and stated as two lists so the reason stays legible.
    let interp = Interp::with_output(Box::new(Vec::new()));

    for builtin in BUILTIN_TYPES {
        let name = builtin.name();
        let bound = global(&interp, name);
        match TokenKind::keyword(name) {
            Some(_) => assert!(
                bound.is_none(),
                "`{name}` is a keyword, so no global could ever be read under it"
            ),
            None => assert_eq!(
                bound,
                Some(Value::Class(interp.heap.builtin_class(*builtin))),
                "`{name}` should be bound to its own class"
            ),
        }
    }

    // The two that are keywords today. Written out so that one of them
    // ceasing to be a keyword is a decision rather than a diff.
    assert!(global(&interp, "nil").is_none());
    assert!(global(&interp, "class").is_none());
}

/// Which builtins can be extended, decided by the one thing that decides it:
/// whether there is a conversion for `super.init` to call. Enumerated rather
/// than spot-checked, so a builtin added later cannot land on either side of
/// this line by accident.
#[test]
fn a_builtin_can_be_extended_exactly_when_it_converts() {
    for builtin in BUILTIN_TYPES {
        // `nil` and `class` cannot be written after `extends` at all — one is
        // a keyword, the other is not bound as a global — so the two that are
        // reachable here are the constructible ones and `function`.
        if matches!(builtin, Builtin::Nil | Builtin::Class) {
            continue;
        }
        let source = format!(
            "class Sub extends {} {{\n op init(x) {{ super.init(x) }}\n}}",
            builtin.name()
        );
        let program = crate::compile(&source).expect("should parse");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        let result = interp.run(&program);

        match builtin.seed().init {
            Some(_) => assert!(
                result.is_ok(),
                "`{}` converts, so it can be extended: {result:?}",
                builtin.name()
            ),
            None => assert_eq!(
                result.expect_err("no conversion, so no subclass").message,
                format!("`Sub` cannot extend `{}`", builtin.name())
            ),
        }
    }
}

/// Which class a global names, for reaching into its slots.
fn class_of(interp: &Interp, name: &str) -> ObjId {
    match global(interp, name) {
        Some(Value::Class(id)) => id,
        other => panic!("`{name}` should be a class, found {other:?}"),
    }
}

/// Declaring an `op` fills its slot as well as the method table.
///
/// Nothing reads a slot yet, so this is the only way to see it — and it is
/// worth seeing on its own, because the alternative failure is silent: an op
/// that lands in `methods` and nowhere else simply never runs.
#[test]
fn declaring_an_op_fills_its_slot() {
    let interp = run("class Money {\n\
                      op init(c) { self.c = c }\n\
                      op string() { return \"$\" }\n\
                      fn plain() { return 1 }\n\
                      }\n");
    let class = interp.heap.class(class_of(&interp, "Money"));

    for op in [Op::Init, Op::Str] {
        assert!(
            matches!(class.slot(op), Some(Value::Function(_))),
            "`op {}` was declared but its slot is empty",
            op.name()
        );
    }
    // A slot is filled by `op`, never inferred from a name — the whole point
    // of the keyword. `plain` is in the table and in no slot at all.
    assert!(class.methods.contains_key("plain"));
    for op in crate::syntax::ast::OPS {
        if !matches!(op, Op::Init | Op::Str) {
            assert!(
                class.slot(*op).is_none(),
                "`{}` was never declared but has a slot",
                op.name()
            );
        }
    }
}

/// Every slot inherits, not just `init`.
///
/// `init` copying down is what `class TypeError extends Error {}` already
/// relied on; this pins that the same loop carries the rest, and that a
/// subclass redeclaring one keeps its own.
#[test]
fn a_subclass_inherits_the_slots_it_does_not_declare() {
    let interp = run("class Base {\n\
                      op init() { }\n\
                      op string() { return \"base\" }\n\
                      op bool() { return false }\n\
                      }\n\
                      class Child extends Base {\n\
                      override op string() { return \"child\" }\n\
                      }\n");
    let base = interp.heap.class(class_of(&interp, "Base")).clone();
    let child = interp.heap.class(class_of(&interp, "Child"));

    assert_eq!(
        child.slot(Op::Bool),
        base.slot(Op::Bool),
        "`op bool` should have been copied down"
    );
    assert_eq!(
        child.slot(Op::Init),
        base.slot(Op::Init),
        "`op init` should have been copied down"
    );
    assert!(
        child.slot(Op::Str) != base.slot(Op::Str),
        "`Child`'s own `op string` should have won"
    );
}

/// The payload is unobservable from Quince until the operators land, so the
/// value `super.init` stored is checked here instead — and it has to be the
/// converted value, not the argument.
#[test]
fn super_init_stores_what_the_conversion_produced() {
    let interp = run("class Count extends int {\n\
                      op init(x) { super.init(x) }\n\
                      }\n\
                      final n = Count(\"42\")\n");

    let Some(Value::Instance(id)) = global(&interp, "n") else {
        panic!("`n` should be an instance");
    };
    assert_eq!(interp.heap.instance(id).payload, Some(Value::Int(42)));
}

/// An implicit `op init` is the inherited conversion run as one, so the payload
/// it stores has to be the converted value rather than the argument — the same
/// assertion as for an explicit `super.init`, reached without writing one.
#[test]
fn declaring_no_op_init_still_converts() {
    let interp = run("class Count extends int {}\nfinal n = Count(\"42\")\n");

    let Some(Value::Instance(id)) = global(&interp, "n") else {
        panic!("`n` should be an instance");
    };
    assert_eq!(interp.heap.instance(id).payload, Some(Value::Int(42)));
}

/// Equality and hashing are one decision, and this is the half a corpus case
/// cannot state: two keys that compare equal must reach the same bucket, so the
/// dict has to end up with one entry rather than two that happen to print alike.
#[test]
fn a_payload_hashes_as_the_value_it_equals() {
    let interp = run("class Username extends string {}\n\
                      final d = {}\n\
                      d[Username(\"marc\")] = 1\n\
                      d[\"marc\"] = 2\n");

    let Some(Value::Dict(id)) = global(&interp, "d") else {
        panic!("`d` should be a dict");
    };
    let dict = interp.heap.dict(id);
    assert_eq!(dict.len(), 1, "an equal key must not make a second entry");
    assert_eq!(
        dict.get(&Key::Str(Rc::from("marc"))),
        Some(&Value::Int(2)),
        "the second write should have replaced the first"
    );
}

/// `op eq` is what costs a class its use as a dict key, and this is the half a
/// corpus case cannot state: that the very same class *without* the op is a
/// perfectly good key. One `.err` file shows the refusal; only a pair shows
/// that the op is the cause rather than the shape.
#[test]
fn declaring_op_eq_is_what_costs_the_dict_key() {
    let interp = run("class Plain extends string {}\n\
                      final d = {}\n\
                      d[Plain(\"marc\")] = 1\n");
    let Some(Value::Dict(id)) = global(&interp, "d") else {
        panic!("`d` should be a dict");
    };
    assert_eq!(interp.heap.dict(id).len(), 1, "the base is a fine key");

    let program = crate::compile(
        "class Decides extends string {\n\
         op eq(other) { return true }\n\
         }\n\
         final d = {}\n\
         d[Decides(\"marc\")] = 1\n",
    )
    .expect("the test program should parse");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp
        .run(&program)
        .expect_err("a class that decides `==` cannot be a key");
    assert!(
        err.message.contains("cannot be a dict key"),
        "expected the key refusal, got: {}",
        err.message
    );
}

/// A subclass gets its payload from the ancestor that has one, however far up
/// the chain that is, because `super`'s receiver is always the original `self`.
#[test]
fn a_payload_is_written_through_an_inherited_init() {
    let interp = run("class Email extends string {\n\
                      op init(s) { super.init(s) }\n\
                      }\n\
                      class Work extends Email {}\n\
                      final e = Work(\"a@b.com\")\n");

    let Some(Value::Instance(id)) = global(&interp, "e") else {
        panic!("`e` should be an instance");
    };
    assert_eq!(
        interp.heap.instance(id).payload,
        Some(Value::from("a@b.com"))
    );
    // The subclass, not the class whose `init` ran.
    assert_eq!(
        interp.heap.class(interp.heap.instance(id).class).name,
        "Work"
    );
}

#[test]
fn truncation_rejects_what_no_int_can_hold() {
    let span = Span::new(0, 1);

    assert_eq!(checked_trunc(3.7, span), Ok(3));
    assert_eq!(checked_trunc(-3.7, span), Ok(-3));
    assert_eq!(checked_trunc(-0.5, span), Ok(0));

    // `as` would answer these with a saturated bound, silently. The boundary
    // is worth pinning in both directions because it is not symmetric, and
    // the asymmetry is easy to get wrong: this test caught a `>` that should
    // have been `>=` and was quietly saturating 2^63 to `i64::MAX`.
    assert_eq!(
        checked_trunc(i64::MAX as f64, span).unwrap_err().kind,
        ErrorKind::Overflow,
        "i64::MAX as f64 rounds up to 2^63, which is out of range"
    );
    assert_eq!(
        checked_trunc(9223372036854774784.0, span),
        Ok(9223372036854774784),
        "the largest float below 2^63 is in range and must convert"
    );
    assert_eq!(
        checked_trunc(i64::MIN as f64, span),
        Ok(i64::MIN),
        "-2^63 is exact as an f64, so the low bound converts"
    );
    assert_eq!(
        checked_trunc(f64::INFINITY, span).unwrap_err().kind,
        ErrorKind::Overflow
    );
    // A NaN is not out of range, it is not a number at all — which is a
    // different mistake, and gets a different kind.
    assert_eq!(
        checked_trunc(f64::NAN, span).unwrap_err().kind,
        ErrorKind::Value
    );
}

#[test]
fn a_conversion_separates_the_wrong_type_from_the_wrong_value() {
    // The whole reason `ErrorKind::Value` exists. Both of these are `int`
    // refusing an argument, but one is fixed at the call and the other is
    // fixed wherever the string came from.
    let cases = [
        ("int([1])", ErrorKind::Type),
        ("int(nil)", ErrorKind::Type),
        ("int(\"abc\")", ErrorKind::Value),
        ("float(\"abc\")", ErrorKind::Value),
    ];

    for (source, expected) in cases {
        let program = crate::compile(source).expect("should parse");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        let err = interp.run(&program).expect_err("should be refused");
        assert_eq!(err.kind, expected, "{source}");
    }
}

#[test]
fn a_conversion_is_reached_through_the_class_a_name_is_bound_to() {
    // Not a special form in `eval`: `int` is an ordinary global holding an
    // ordinary class, so it converts just as well through another name.
    let interp = run("final make = int\nfinal n = make(\"42\")");
    assert_eq!(global(&interp, "n"), Some(Value::Int(42)));
}

#[test]
fn a_loop_does_not_grow_the_heap_without_bound() {
    // Two allocations an iteration — the scope and the list — so without a
    // collector this settles at several thousand live objects.
    let interp = run("let i = 0\nwhile i < 2000 {\n let x = [1, 2, 3]\n i = i + 1\n}");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        interp.heap.live() < 600,
        "heap grew to {} objects",
        interp.heap.live()
    );
}

#[test]
fn a_loop_that_catches_does_not_grow_the_heap() {
    // `catch` does not create the hazard here so much as stop hiding it.
    // Every site that pushes a scope, a temp, or a frame restores it before
    // propagating, but while an error was fatal a site that forgot would leak
    // roots into a process about to exit, where nothing could observe it. A
    // caught error resumes with those stacks still deep, so the same latent
    // bug becomes unbounded growth — which is what this measures.
    let interp = run("let i = 0\n\
         while i < 2000 {\n\
         \x20 try {\n\
         \x20  throw Error(\"x\")\n\
         \x20 } catch e {\n\
         \x20  i = i + 1\n\
         \x20 }\n\
         }");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        interp.heap.live() < 600,
        "heap grew to {} objects",
        interp.heap.live()
    );
    // All three stacks back to their depth at the `try`.
    assert!(
        interp.scopes.is_empty(),
        "{} scopes left behind",
        interp.scopes.len()
    );
    assert!(
        interp.temps.is_empty(),
        "{} temps left behind",
        interp.temps.len()
    );
    assert_eq!(interp.depth, 0, "depth left at {}", interp.depth);
}

#[test]
fn a_thrown_payload_survives_the_unwind() {
    // The instance travels inside `QuinceError` through frames that root
    // nothing: no scope and no `temps` entry refers to it for the whole
    // unwind. It survives only because collection happens between statements
    // and unwinding executes none.
    //
    // Churning first puts the heap over its collection threshold, so a safe
    // point crossed on the way out would actually free the payload rather
    // than merely being allowed to. Reading `e.n` afterwards is what would
    // fail. This is the invariant a `finally` would have broken, by running
    // statements during the unwind — see DESIGN.md.
    let interp = run("class Deep extends Error {\n\
         \x20   op init(message, n) {\n\
         \x20       super.init(message)\n\
         \x20       self.n = n\n\
         \x20   }\n\
         }\n\
         fn churn(k) {\n\
         \x20   let scratch = []\n\
         \x20   let i = 0\n\
         \x20   while i < k {\n\
         \x20       scratch.push([i])\n\
         \x20       i = i + 1\n\
         \x20   }\n\
         \x20   return len(scratch)\n\
         }\n\
         fn go(d) {\n\
         \x20   if d <= 0 {\n\
         \x20       throw Deep(\"bottom\", 42)\n\
         \x20   }\n\
         \x20   return go(d - 1)\n\
         }\n\
         churn(3000)\n\
         let got = 0\n\
         try {\n\
         \x20   go(50)\n\
         } catch e {\n\
         \x20   got = e.n\n\
         }");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        matches!(global(&interp, "got"), Some(Value::Int(42))),
        "the payload did not survive: got {:?}",
        global(&interp, "got")
    );
}

#[test]
fn an_extension_survives_collection() {
    // The one root the interpreter holds that no walk of the heap could
    // reach: an extension's function is deliberately *not* in the class's
    // method table, so `int` does not keep it alive and nothing else refers
    // to it. Deleting the line that roots `extensions` makes this panic at
    // `handle points at a collected object` rather than merely fail.
    let interp = run("extend int { fn double() { return self * 2 } }\n\
         let i = 0\n\
         while i < 2000 {\n let junk = [0]\n i = i + 1\n }\n\
         let n = 7.double()");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert!(
        matches!(global(&interp, "n"), Some(Value::Int(14))),
        "the extension did not survive: got {:?}",
        global(&interp, "n")
    );
}

#[test]
fn a_declared_method_beats_an_extension_on_an_ancestor() {
    // The half of the lookup order a corpus case cannot show on its own: both
    // walks cover the whole chain, and the *methods* walk finishes first, so
    // a subclass's own method wins over an extension added to its parent —
    // not merely over one added to itself.
    let interp = run("class Animal { fn speak() { return \"...\" } }\n\
         class Dog extends Animal { fn name() { return \"dog\" } }\n\
         extend Animal { fn name() { return \"animal\" } }\n\
         let through_dog = Dog().name()\n\
         let through_animal = Animal().name()");

    let Some(Value::Str(dog)) = global(&interp, "through_dog") else {
        panic!("`Dog().name()` should have answered");
    };
    assert_eq!(&*dog, "dog", "the declared method has to win");

    let Some(Value::Str(animal)) = global(&interp, "through_animal") else {
        panic!("`Animal().name()` should have answered");
    };
    assert_eq!(&*animal, "animal", "and the extension still answers for it");
}

#[test]
fn a_modifier_closes_one_class_and_not_its_ancestors() {
    // Openness belongs to the declaration that said the word, and no walk of
    // the chain goes looking for it. `Dog` closing itself leaves `Animal` open
    // to both routes — which is what the two statements after `Dog` running at
    // all prove — and `Cat` is open because it said nothing.
    let interp = run("class Animal { fn speak() { return \"...\" } }\n\
         sealed class Dog extends Animal {}\n\
         class Cat extends Animal {}\n\
         extend Animal { fn legs() { return 4 } }");

    let openness = |name: &str| match global(&interp, name) {
        Some(Value::Class(id)) => interp.heap.class(id).openness,
        other => panic!("`{name}` should be a class, got {other:?}"),
    };
    assert!(openness("Dog").closes_inheritance());
    assert!(openness("Dog").closes_extension());
    for open in ["Animal", "Cat"] {
        assert!(
            !openness(open).closes_inheritance() && !openness(open).closes_extension(),
            "`{open}` should have stayed open"
        );
    }
}

#[test]
fn each_modifier_closes_only_its_own_door() {
    // The table in `Openness`, measured on real class objects rather than on
    // the enum: `final` leaves `extend` alone, and `complete` leaves the
    // hierarchy alone. Both halves are invisible to a corpus case, which can
    // only ever show the refusals.
    let interp = run("final class F {}\n\
         complete class C {}\n\
         extend F { fn tag() { return 1 } }\n\
         class Sub extends C {}");

    let openness = |name: &str| match global(&interp, name) {
        Some(Value::Class(id)) => interp.heap.class(id).openness,
        other => panic!("`{name}` should be a class, got {other:?}"),
    };
    assert!(openness("F").closes_inheritance());
    assert!(!openness("F").closes_extension(), "`final` is not `sealed`");
    assert!(openness("C").closes_extension());
    assert!(
        !openness("C").closes_inheritance(),
        "`complete` is not `sealed`"
    );
}

#[test]
fn a_captured_scope_survives_collection() {
    // The closure is reachable only through `f`, and its captured scope only
    // through the closure. Tracing has to follow both links.
    let interp = run("fn make() {\n\
         let n = [1, 2, 3]\n\
         fn get() { return n }\n\
         return get\n\
         }\n\
         let f = make()\n\
         let i = 0\n\
         while i < 2000 {\n let junk = [0]\n i = i + 1\n }\n\
         let survived = f()");

    assert!(interp.heap.collections > 0, "the collector never ran");
    let Some(Value::List(id)) = global(&interp, "survived") else {
        panic!("the closure did not return its captured list");
    };
    assert_eq!(interp.heap.list(id).len(), 3);
}

#[test]
fn the_iteration_snapshot_survives_the_list_it_came_from() {
    // The first iteration overwrites every element, so the lists the *later*
    // iterations still have to visit are reachable only from the snapshot
    // held in `exec_for`'s Rust frame.
    let interp = run("let items = [[1], [2], [3]]\n\
         let total = 0\n\
         for pair in items {\n\
         items[0] = 0\n\
         items[1] = 0\n\
         items[2] = 0\n\
         let i = 0\n\
         while i < 400 {\n let junk = [0]\n i = i + 1\n }\n\
         total = total + len(pair)\n\
         }");

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "total"), Some(Value::Int(3)));
}

/// Churns enough objects to force several collections, then returns `value`.
fn churn(value: &str) -> String {
    format!(
        "fn churn() {{\n\
         let i = 0\n\
         while i < 3000 {{ let junk = [0]; i = i + 1 }}\n\
         return {value}\n\
         }}\n"
    )
}

#[test]
fn a_list_element_survives_evaluating_a_later_one() {
    // `mk()`'s list lives only in `eval_seq`'s Rust-local `Vec` while
    // `churn()` runs. Unrooted, its slot was reused by a scope and `len`
    // panicked with "expected a list, found Env".
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         {}\
         let pair = [mk(), churn()]\n\
         let kept = len(pair[0])",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

#[test]
fn an_operand_survives_evaluating_the_other() {
    // Structural equality reads both lists out of the heap, so a collected
    // left operand is a panic rather than a wrong answer.
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         {}\
         let same = mk() == churn()",
        churn("[1, 2, 3]")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "same"), Some(Value::Bool(true)));
}

#[test]
fn the_left_operand_survives_evaluating_the_right() {
    // The path `+` on lists takes, and the reason the rooting had to land
    // before concatenation did.
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         {}\
         let kept = len(mk() + churn())",
        churn("[4]")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(4)));
}

#[test]
fn a_slice_target_survives_evaluating_its_bounds() {
    // The list being sliced sits in a Rust frame while the bounds run, and
    // a bound is an arbitrary expression that can reach a safe point. Pins
    // that `Slice` goes through `eval_seq` rather than hand-rolling it.
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3, 4] }}\n\
         {}\
         let kept = len(mk()[1:churn()])",
        churn("3")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(2)));
}

#[test]
fn a_bound_method_keeps_its_receiver_alive() {
    // The list is reachable from nowhere but the bound method: no variable
    // names it, and it is not inside any other object. If `trace` skipped
    // the receiver, `push` would later write through a handle whose slot
    // had been reused.
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         let m = mk().push\n\
         {}\
         let junk = churn()\n\
         m(4)",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");

    let Some(Value::BoundMethod(id)) = global(&interp, "m") else {
        panic!("`m` should be a bound method");
    };
    let Value::List(list) = interp.heap.bound_method(id).receiver else {
        panic!("the receiver should be a list");
    };
    assert_eq!(
        interp.heap.list(list).len(),
        4,
        "the push should have landed"
    );
}

#[test]
fn an_instance_survives_its_own_constructor() {
    // Slot 0 is the root, and it stays pointing at the instance because
    // `self` cannot be reassigned. This passed with a `temps` root too, and
    // still passes now that the root is gone — what makes it hold is the
    // resolver rule, which `self_cannot_be_reassigned` pins down.
    let interp = run(&format!(
        "{}\
         class C {{\n\
         op init(n) {{\n\
         self.n = n\n\
         let junk = churn()\n\
         }}\n\
         }}\n\
         let c = C(7)\n\
         let kept = c.n",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
}

#[test]
fn a_class_survives_the_instance_that_is_all_that_names_it() {
    // Nothing refers to the class but the instance's `class` handle: the
    // name it was declared under went out of scope with `mk`. Reaching it
    // is what `type` and every later method lookup depend on.
    let interp = run(&format!(
        "fn mk() {{\n\
         class Hidden {{ fn who() {{ return 42 }} }}\n\
         return Hidden()\n\
         }}\n\
         let obj = mk()\n\
         {}\
         let junk = churn()\n\
         let kept = obj.who()\n\
         let name = type(obj)",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(42)));
    assert_eq!(global(&interp, "name"), Some(Value::from("Hidden")));
}

#[test]
fn a_parent_class_survives_the_subclass_that_names_it() {
    // `Base` goes out of scope with `build`, leaving the subclass's `parent`
    // handle as the only thing that reaches it. Method lookup walks that
    // chain, so losing it turns an inherited call into a panic.
    let interp = run(&format!(
        "fn build() {{\n\
         class Base {{ fn greet() {{ return 42 }} }}\n\
         class Sub extends Base {{}}\n\
         return Sub()\n\
         }}\n\
         let obj = build()\n\
         {}\
         let junk = churn()\n\
         let kept = obj.greet()",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(42)));
}

#[test]
fn the_scope_holding_super_survives_with_the_methods_that_close_over_it() {
    // A subclass's methods close over a scope whose only slot is the parent
    // class. Nothing names that scope, so it is reachable only as a method's
    // captured environment — and `super.speak()` reads straight out of it.
    //
    // No new root was needed for it, which is the point: making `super` a
    // captured local rather than a field on the class means the collector
    // work was already done. This is the test that would notice if that
    // stopped being true — deleting `Function`'s env tracing fails it.
    let interp = run(&format!(
        "fn build() {{\n\
         class Base {{ fn speak() {{ return 1 }} }}\n\
         class Sub extends Base {{ override fn speak() {{ return super.speak() + 1 }} }}\n\
         return Sub()\n\
         }}\n\
         let obj = build()\n\
         {}\
         let junk = churn()\n\
         let kept = obj.speak()",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(2)));
}

#[test]
fn a_field_survives_collection_with_its_instance() {
    // The list is reachable only through the field, so this is the instance
    // half of what `Dict::trace` already does for a dict.
    let interp = run(&format!(
        "class Box {{ op init() {{ self.items = [1, 2, 3] }} }}\n\
         let b = Box()\n\
         {}\
         let junk = churn()\n\
         let kept = len(b.items)",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

/// The payload half of the same guarantee, which needs a payload that is
/// actually a handle: a string is an `Rc` and a collection cannot touch it, so
/// only a list or dict ancestor puts anything at risk. The payload is not a
/// field, so `Dict::trace` does not cover it and `Instance::trace` must.
#[test]
fn a_payload_survives_collection_with_its_instance() {
    let interp = run(&format!(
        "class Bag extends dict {{ op init(d) {{ super.init(d) }} }}\n\
         let b = Bag({{\"a\": 1, \"b\": 2}})\n\
         {}\
         let junk = churn()\n\
         let kept = b.keys()",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    let Some(Value::List(id)) = global(&interp, "kept") else {
        panic!("`keys` returns a list");
    };
    assert_eq!(interp.heap.list(id).len(), 2);
}

#[test]
fn a_method_held_in_a_field_survives_evaluating_the_arguments() {
    // A field holding a function is reachable only through that field, and
    // an argument is free to overwrite it. A *method* is safe without this
    // — the rooted receiver reaches its class, and the class its methods —
    // so the hazard belongs to fields alone. The closure has to be a local
    // one: a top-level `fn` is a global, and globals are always rooted.
    let interp = run(&format!(
        "class Holder {{}}\n\
         fn build() {{\n\
         fn seven(n) {{ return 7 }}\n\
         let h = Holder()\n\
         h.f = seven\n\
         return h\n\
         }}\n\
         {}\
         fn clear(h) {{\n\
         h.f = nil\n\
         return churn()\n\
         }}\n\
         let h = build()\n\
         let kept = h.f(clear(h))",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
}

#[test]
fn a_receiver_survives_evaluating_the_arguments() {
    // The receiver exists only in `eval_method_call`'s Rust frame while the
    // argument runs, and evaluating an argument reaches a safe point. A
    // dict receiver rather than a list because `remove` returns something
    // drawn from it, so a collected receiver is a wrong answer and not only
    // a panic.
    let interp = run(&format!(
        "fn mk() {{ return {{\"k\": 7}} }}\n\
         {}\
         let got = mk().remove(churn())",
        churn("\"k\"")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "got"), Some(Value::Int(7)));
}

#[test]
fn an_argument_survives_evaluating_a_later_argument() {
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         fn first(a, b) {{ return a }}\n\
         {}\
         let kept = len(first(mk(), churn()))",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

#[test]
fn the_callee_survives_evaluating_the_arguments() {
    // A closure built by an expression is reachable from nowhere but the
    // Rust frame until the call actually begins.
    let interp = run(&format!(
        "fn make() {{ fn id(x) {{ return x }} return id }}\n\
         {}\
         let kept = make()(churn())",
        churn("7")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
}

#[test]
fn a_dict_entry_survives_evaluating_a_later_one() {
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         {}\
         let d = {{\"a\": mk(), \"b\": churn()}}\n\
         let kept = len(d[\"a\"])",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

#[test]
fn an_assigned_value_survives_evaluating_its_target() {
    // `xs[churn()] = mk()` evaluates the value first, then the target.
    let interp = run(&format!(
        "fn mk() {{ return [1, 2, 3] }}\n\
         {}\
         let xs = [0, 0]\n\
         xs[churn()] = mk()\n\
         let kept = len(xs[1])",
        churn("1")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

#[test]
fn a_dict_survives_collection_with_its_contents() {
    let interp = run(&format!(
        "let d = {{\"kept\": [1, 2, 3]}}\n\
         {}\
         let ignored = churn()\n\
         let kept = len(d[\"kept\"])",
        churn("0")
    ));

    assert!(interp.heap.collections > 0, "the collector never ran");
    assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
}

#[test]
fn an_unreachable_recursive_function_is_collected() {
    // A function whose scope holds the function: the cycle that rules out
    // reference counting. Redefining `f` should still reclaim the old one.
    let interp = run("let i = 0\nwhile i < 2000 {\n fn f() { return f }\n i = i + 1\n}");

    assert!(
        interp.heap.live() < 600,
        "heap grew to {} objects",
        interp.heap.live()
    );
}

#[test]
fn a_default_is_a_fresh_value_at_every_call() {
    // The one place Python's answer is refused outright. A default evaluated
    // once at the declaration shares one list between every call that omits it,
    // which is the single most reported footgun in that language — so this is
    // the property, not an implementation detail.
    let interp = run(
        "fn collect(item, into: list = []): list {\n\
         into.push(item)\n\
         return into\n\
         }\n\
         let a = collect(1)\n\
         let b = collect(2)\n",
    );
    for (name, expected) in [("a", 1), ("b", 2)] {
        let Some(Value::List(id)) = global(&interp, name) else {
            panic!("`{name}` should hold a list");
        };
        assert_eq!(interp.heap.list(id), &[Value::Int(expected)]);
    }
}

#[test]
fn a_default_is_evaluated_in_the_declaration_scope() {
    // Not the caller's. A default is part of the declaration and reads the
    // names the declaration could see, which is what makes it readable at all —
    // whichever scope a call happens to be in.
    let interp = run(
        "fn build() {\n\
         let hidden = 9\n\
         fn f(n, from = hidden) { return n + from }\n\
         return f\n\
         }\n\
         let f = build()\n\
         let hidden = 100\n\
         let answer = f(1)\n",
    );
    assert_eq!(global(&interp, "answer"), Some(Value::Int(10)));
}

#[test]
fn a_named_argument_reaches_the_parameter_it_names() {
    let interp = run(
        "fn f(a: int, b: int = 2, c: int = 3): int { return a * 100 + b * 10 + c }\n\
         let all = f(1, 2, 3)\n\
         let middle = f(1, c: 9)\n\
         let shuffled = f(c: 9, b: 8, a: 7)\n",
    );
    assert_eq!(global(&interp, "all"), Some(Value::Int(123)));
    assert_eq!(global(&interp, "middle"), Some(Value::Int(129)));
    assert_eq!(global(&interp, "shuffled"), Some(Value::Int(789)));
}

#[test]
fn a_compound_assignment_evaluates_its_target_once() {
    // The whole reason `d[f()] += 1` is a language form rather than something
    // a program writes out. Deleting the single-evaluation shape in `assign_op`
    // makes `calls` two.
    let interp = run(
        "let calls = 0\n\
         fn key(): string {\n\
         calls += 1\n\
         return \"k\"\n\
         }\n\
         let counts = {\"k\": 10}\n\
         counts[key()] += 1\n",
    );
    assert_eq!(global(&interp, "calls"), Some(Value::Int(1)));
    let Some(Value::Dict(id)) = global(&interp, "counts") else {
        panic!("`counts` should hold a dict");
    };
    assert_eq!(
        interp.heap.dict(id).get(&Key::Str("k".into())),
        Some(&Value::Int(11))
    );
}

#[test]
fn dispatch_prefers_an_exact_type_to_a_widened_one() {
    // v0.7 §4.1's widening, read as a *preference* rather than as a yes/no: an
    // `int` holds as a `float`, and it holds as an `int` better.
    let interp = run(
        "fn width(n: int): string { return \"int\" }\n\
         fn width(n: float): string { return \"float\" }\n\
         let a = width(1)\n\
         let b = width(1.0)\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("int")));
    assert_eq!(global(&interp, "b"), Some(Value::from("float")));
}

#[test]
fn dispatch_prefers_an_exact_container_to_a_widened_one() {
    // The same preference one argument deeper, and it is what makes `any`
    // widening survivable at a call. Once `list[any]` admits a `list[int]`, the
    // class name alone cannot tell the two declarations apart — both would score
    // the same, both would match, and a program declaring both could call
    // neither.
    let interp = run(
        "fn pick(xs: list[int]): string { return \"ints\" }\n\
         fn pick(xs: list[any]): string { return \"anything\" }\n\
         let ints: list[int] = [1, 2]\n\
         let words: list[string] = [\"a\"]\n\
         let a = pick(ints)\n\
         let b = pick(words)\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("ints")));
    assert_eq!(global(&interp, "b"), Some(Value::from("anything")));

    // A bare `list` is a widening too — its argument is elided, which is to say
    // `any?` — so an exact one still wins.
    let interp = run(
        "fn pick(xs: list[int]): string { return \"ints\" }\n\
         fn pick(xs: list): string { return \"some list\" }\n\
         let ints: list[int] = [1, 2]\n\
         let a = pick(ints)\n\
         let b = pick([\"a\"])\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("ints")));
    assert_eq!(global(&interp, "b"), Some(Value::from("some list")));
}

#[test]
fn an_unannotated_overload_is_tried_last() {
    let interp = run(
        "fn take(n: int): string { return \"int\" }\n\
         fn take(anything): string { return \"other\" }\n\
         let a = take(1)\n\
         let b = take(\"s\")\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("int")));
    assert_eq!(global(&interp, "b"), Some(Value::from("other")));
}

#[test]
fn an_override_replaces_one_signature_and_not_the_set() {
    // §3.5's last rule. The subclass's table entry has to be merged with the
    // parent's, because `Class::method` answers with the first table holding
    // the name and cannot see past it.
    let interp = run(
        "class Base {\n\
         fn hello(n: int): string { return \"base int\" }\n\
         fn hello(s: string): string { return \"base string\" }\n\
         }\n\
         class Kid extends Base {\n\
         override fn hello(n: int): string { return \"kid int\" }\n\
         }\n\
         let a = Kid().hello(1)\n\
         let b = Kid().hello(\"s\")\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("kid int")));
    assert_eq!(global(&interp, "b"), Some(Value::from("base string")));
}

#[test]
fn a_constructor_is_replaced_rather_than_overloaded_across_a_subclass() {
    // The exception the merge above has to make. Every constructor in a
    // hierarchy replaces its parent's — that is what `super.init` is for — so
    // folding them into one set would make `Kid(1)` reach `Base`'s constructor
    // on a `Kid`.
    let interp = run(
        "class Base {\n\
         op init(n: int) { self.n = n }\n\
         }\n\
         class Kid extends Base {\n\
         op init(n: int, m: int) { super.init(n)\n self.m = m }\n\
         }\n\
         let built = Kid(1, 2)\n",
    );
    assert!(global(&interp, "built").is_some());
    let program = crate::compile("class Base {\n op init(n: int) { self.n = n }\n}\nclass Kid extends Base {\n op init(n: int, m: int) { super.init(n)\n self.m = m }\n}\nKid(1)\n")
        .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("`Kid` needs both arguments");
    assert!(err.message.contains("takes 2 arguments"), "{}", err.message);
}

#[test]
fn an_overloaded_name_is_still_one_function_value() {
    // Nothing a program can do tells a set apart from a function: it has the
    // same type, it is truthy, and it is called the same way.
    let interp = run(
        "fn f(n: int): int { return 1 }\n\
         fn f(s: string): int { return 2 }\n\
         let held = f\n\
         let kind = type(f)\n\
         let answer = held(\"s\")\n",
    );
    assert_eq!(global(&interp, "kind"), Some(Value::from("function")));
    assert_eq!(global(&interp, "answer"), Some(Value::Int(2)));
}

#[test]
fn a_class_may_declare_several_constructors() {
    // Told apart the same way any other overloaded name is, and reachable
    // through every path that builds one: a call, a keyword call, default
    // construction, and the implicit coercion of §3.3.
    let interp = run(
        "class Money {\n\
         op init() { self.cents = 0 }\n\
         op init(cents: int) { self.cents = cents }\n\
         op init(text: string) { self.cents = int(text) }\n\
         op init(whole: int, part: int) { self.cents = whole * 100 + part }\n\
         }\n\
         let a = Money().cents\n\
         let b = Money(5).cents\n\
         let c = Money(\"700\").cents\n\
         let d = Money(part: 5, whole: 2).cents\n\
         let e: Money\n\
         let f = e.cents\n\
         let coerced: Money = 12\n\
         let g = coerced.cents\n",
    );
    for (name, expected) in [("a", 0), ("b", 5), ("c", 700), ("d", 205), ("f", 0), ("g", 12)] {
        assert_eq!(
            global(&interp, name),
            Some(Value::Int(expected)),
            "`{name}` is wrong"
        );
    }
}

#[test]
fn coercion_picks_the_constructor_the_value_fits() {
    // The offer §3.3 describes is made by whichever single-parameter
    // constructor takes this value. A class that also declares others still
    // makes it — the payload is what decides, not how many there are.
    let interp = run(
        "class Money {\n\
         op init(cents: int) { self.cents = cents }\n\
         op init(text: string) { self.cents = int(text) }\n\
         }\n\
         let a: Money = 5\n\
         let b: Money = \"700\"\n\
         let x = a.cents\n\
         let y = b.cents\n",
    );
    assert_eq!(global(&interp, "x"), Some(Value::Int(5)));
    assert_eq!(global(&interp, "y"), Some(Value::Int(700)));

    // And a value no constructor takes is still reported against the
    // annotation, rather than blamed on the constructor.
    let program = crate::compile(
        "class Money {\n op init(cents: int) { self.cents = cents }\n}\nlet a: Money = 1.5\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("a float is not a `Money`");
    assert!(err.message.contains("`a` is `Money`"), "{}", err.message);
}

#[test]
fn an_operator_reports_against_the_expression_that_wrote_it() {
    // A class declaring `op mul` has said what `*` means to it, so an operand it
    // does not take is a refusal. The report is the point: an ordinary call
    // reports a wrong argument against the parameter that refused it, which
    // here would underline a parameter nobody wrote, in a declaration somewhere
    // else entirely.
    let program = crate::compile(
        "extend list {\n\
         op mul(factor: int): list { return self }\n\
         }\n\
         let doubled = [1, 2] * 2.4\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("a float is not an int");
    // The sentence every binary type error uses, so a reader cannot tell from
    // the shape of the report whether the class declared the slot and refused
    // the operand or never declared it at all. What the class *does* take is
    // the help line, which is the one thing the ordinary report cannot say.
    assert_eq!(err.message, "cannot multiply list and float");
    assert_eq!(err.labels.len(), 3, "both operands and the operator: {:?}", err.labels);
    assert!(
        err.help.iter().any(|line| line.contains("`op mul` for: (int)")),
        "the report says what the class does declare: {:?}",
        err.help
    );

    // The same sentence whether the name has one declaration or several, which
    // is what makes it a rule rather than a special case.
    let program = crate::compile(
        "extend list {\n\
         op mul(factor: int): list { return self }\n\
         op mul(factor: string): list { return self }\n\
         }\n\
         let doubled = [1, 2] * 2.4\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("a float is neither");
    assert_eq!(err.message, "cannot multiply list and float");
    assert!(
        err.help.iter().any(|line| line.contains("(int), (string)")),
        "both are listed: {:?}",
        err.help
    );

    // And a class that declares nothing at all reaches the same sentence
    // through the path that was always there.
    let program = crate::compile("let n = [1, 2] * 2.4\n").expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("a list does not multiply");
    assert_eq!(err.message, "cannot multiply list and float");
}

#[test]
fn every_operator_that_takes_an_operand_reports_the_same_way() {
    // The three that are not binary operators. `x[i]` and `needle in x` have no
    // pair of operand spans to label, so they keep a report of their own — but
    // it is one report and not three, and it says the same two things: which
    // operator refused, and what it does take.
    for (source, expected) in [
        (
            "class G {\n op get(i: int) { return i }\n}\nlet x = G()[\"a\"]\n",
            "`op get` on a G does not take (string)",
        ),
        (
            "class G {\n op init() { self.n = 0 }\n op set(i: int, v: int) { self.n = v }\n}\n\
             let g = G()\ng[\"a\"] = 1\n",
            "`op set` on a G does not take (string, int)",
        ),
        (
            "class G {\n op contains(needle: int): bool { return true }\n}\n\
             let found = \"a\" in G()\n",
            "`op contains` on a G does not take (string)",
        ),
    ] {
        let program = crate::compile(source).expect("the program parses");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        let err = interp.run(&program).expect_err("the operand does not fit");
        assert_eq!(err.message, expected);
    }
}

#[test]
fn a_binary_operator_reads_the_same_whether_the_class_declared_it() {
    // The property the report is for. Two programs, one where `list` declares
    // `op mul` and refuses the operand and one where it declares nothing, and a
    // reader cannot tell them apart from the sentence or the shape.
    let refused = |source: &str| {
        let program = crate::compile(source).expect("the program parses");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        interp.run(&program).expect_err("the operands do not fit")
    };
    let declared = refused(
        "extend list {\n op mul(factor: int): list { return self }\n}\nlet n = [1] * 2.4\n",
    );
    let bare = refused("let n = [1] * 2.4\n");
    assert_eq!(declared.message, bare.message);
    assert_eq!(declared.labels.len(), bare.labels.len());
    // The one difference, and it is an addition rather than a change.
    assert_ne!(declared.help, bare.help);
}

#[test]
fn a_refused_value_is_underlined_and_not_the_name_it_was_written_to() {
    // The report used to mark the *target* of a write, so `xs = ["hello"]`
    // under a `list[int]` said "item 0 is `int`" with the caret two characters
    // wide under `xs`. The sentence already names the boundary that refused the
    // value; the caret's job is the other half of the question, which is which
    // value.
    fn refused(source: &str) -> (String, &str) {
        let program = crate::compile(source).expect("the program parses");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        let err = interp.run(&program).expect_err("the value does not hold");
        (err.message.clone(), &source[err.span.start as usize..err.span.end as usize])
    }

    for (source, message, underlined) in [
        // The reported case: a write to a name declared with an annotation,
        // where the element is what is wrong and the element is what is marked.
        (
            "let xs: list[int] = []\nxs = [\"hello\", \"world\"]\n",
            "item 0 is `int`, but this is a string",
            "\"hello\"",
        ),
        // A local, which travels a different path to the same check.
        (
            "fn probe() {\n let xs: list[int] = []\n xs = [1, \"two\"]\n}\nprobe()\n",
            "item 1 is `int`, but this is a string",
            "\"two\"",
        ),
        // Nothing to do with containers: the value is still the subject.
        (
            "let n: int = 1\nn = \"s\"\n",
            "`n` is `int`, but this is a string",
            "\"s\"",
        ),
        // A subscripted write has two values it can be refused for, and the
        // caret has to agree with which one the message named.
        (
            "let d: dict[string, int] = {}\nd[\"k\"] = \"v\"\n",
            "the value is `int`, but this is a string",
            "\"v\"",
        ),
        (
            "let d: dict[string, int] = {}\nd[1] = 2\n",
            "the key is `string`, but this is an int",
            "1",
        ),
        (
            "let xs: list[int] = [0]\nxs[0] = \"a\"\n",
            "the item is `int`, but this is a string",
            "\"a\"",
        ),
        // `a += b` produces a value that is neither operand, so the whole
        // expression is what made it and the whole expression is marked.
        ("let n: int = 1\nn += 1.5\n", "`n` is `int`, but this is a float", "n += 1.5"),
    ] {
        assert_eq!(refused(source), (message.to_string(), underlined), "for {source:?}");
    }
}

#[test]
fn a_nested_annotation_is_refused_at_the_depth_it_disagrees() {
    // "item 1 is `list[int]`, but this is a list" is true and useless. The
    // wording and the caret both come from the innermost element that actually
    // disagrees — which is what `sema::check` reports for the same program.
    let source = "let xs: list[list[int]] = []\nxs = [[1], [\"a\"]]\n";
    let program = crate::compile(source).expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("the nested element does not hold");
    assert_eq!(err.message, "item 0 is `int`, but this is a string");
    assert_eq!(&source[err.span.start as usize..err.span.end as usize], "\"a\"");
}

#[test]
fn a_declaration_still_points_at_the_annotation_that_refused_the_value() {
    // The second label is only meaningful while the annotation's span and the
    // value's index the same text. A declaration holds both, so it keeps it —
    // and a write to a name declared elsewhere, which is every REPL entry after
    // the first, must not draw one.
    let refused = |source: &str| {
        let program = crate::compile(source).expect("the program parses");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        interp.run(&program).expect_err("the value does not hold")
    };
    let declaration = refused("let xs: list[int] = [\"a\"]\n");
    assert_eq!(declaration.labels.len(), 2, "{:?}", declaration.labels);

    let write = refused("let xs: list[int] = []\nxs = [\"a\"]\n");
    assert_eq!(write.message, declaration.message);
    assert!(write.labels.is_empty(), "{:?}", write.labels);
}

#[test]
fn a_described_container_is_what_its_annotation_said() {
    // `holds` used to walk the elements and nothing else, so an empty
    // `list[int]` was equally a `list[string]` — the annotation next to it said
    // otherwise and was not consulted. The header is the answer now.
    let interp = run(
        "fn ints(xs: list[int]): string { return \"ints\" }\n\
         let empty: list[int] = []\n\
         let a = ints(empty)\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("ints")));

    for (source, expected) in [
        (
            "fn strings(xs: list[string]): int { return 0 }\n\
             let empty: list[int] = []\nlet n = strings(empty)\n",
            "`xs` is `list[string]`, but this is `list[int]`",
        ),
        // Nullability is part of what it holds: a `nil` written through the
        // second is a `nil` read out of the first.
        (
            "fn strict(xs: list[int]): int { return 0 }\n\
             let loose: list[int?] = []\nlet n = strict(loose)\n",
            "`xs` is `list[int]`, but this is `list[int?]`",
        ),
        (
            "fn flags(d: dict[string, bool]): int { return 0 }\n\
             let scores: dict[string, int] = {}\nlet n = flags(scores)\n",
            "`d` is `dict[string, bool]`, but this is `dict[string, int]`",
        ),
    ] {
        let program = crate::compile(source).expect("the program parses");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        let err = interp.run(&program).expect_err("the header says otherwise");
        assert_eq!(err.message, expected);
    }
}

#[test]
fn the_top_type_still_takes_any_container() {
    // The one thing that widens, and it is safe because a *write* is checked
    // against the header rather than against the annotation it arrived through
    // — `xs.push("s")` inside `fn f(xs: list[any])` is refused on the strength
    // of the `list[int]` the caller passed. Without this, "a list of anything"
    // would have no spelling but the bare `list`.
    let interp = run(
        "fn anything(xs: list[any]): int { return len(xs) }\n\
         fn bare(xs: list): int { return len(xs) }\n\
         let ints: list[int] = [1, 2]\n\
         let a = anything(ints)\n\
         let b = bare(ints)\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::Int(2)));
    assert_eq!(global(&interp, "b"), Some(Value::Int(2)));

    let program = crate::compile(
        "fn grow(xs: list[any]) { xs.push(\"s\") }\n\
         let ints: list[int] = [1, 2]\ngrow(ints)\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("the header guards the write");
    assert!(err.message.contains("the item is `int`"), "{}", err.message);
}

#[test]
fn is_reads_an_elided_argument_as_the_any_it_stands_for() {
    // §3.10 says an argument nobody wrote is `any?`, and v0.9 §3.1 says the same
    // of an unbounded `[T]`. That was being decided at seven sites, each
    // slightly differently, and the two that mattered most disagreed: `is`
    // compared arguments by identity where an annotation compared them by
    // admission. One rule now, shared by both.
    let prelude = "let ints: list[int] = [1, 2]\n\
                   let maybe: list[int?] = [1, nil]\n\
                   let table: dict[string, int] = {\"a\": 1}\n\
                   let keyed: dict[string] = {\"a\": 1}\n";
    for (test, expected) in [
        // The property that was broken: `list` and `list[any?]` are one type
        // written two ways, so they cannot answer differently.
        ("ints is list", true),
        ("ints is list[any?]", true),
        ("maybe is list", true),
        ("maybe is list[any?]", true),
        // `any` is the top type and widens — but does not admit `nil`, so it
        // does not admit a container that may hold one.
        ("ints is list[any]", true),
        ("maybe is list[any]", false),
        // Nothing else widens. §4.1's invariance is intact.
        ("ints is list[int]", true),
        ("ints is list[int?]", false),
        ("ints is list[string]", false),
        // `dict[K]` is `dict[K, any?]`, and that decides both directions: a
        // `dict[string, int]` is one, and one is not a `dict[string, int]`.
        ("table is dict", true),
        ("table is dict[string]", true),
        ("table is dict[string, any]", true),
        ("keyed is dict[string]", true),
        ("keyed is dict[string, int]", false),
        ("keyed is dict[string, any]", false),
        // A container nothing described is every argument elided.
        ("[1, 2] is list", true),
        ("[1, 2] is list[any?]", true),
        ("[1, 2] is list[int]", false),
        // The one rule `is` keeps that an annotation does not.
        ("1 is float", false),
    ] {
        let interp = run(&format!("{prelude}let answer = {test}\n"));
        assert_eq!(
            global(&interp, "answer"),
            Some(Value::Bool(expected)),
            "for `{test}`"
        );
    }
}

#[test]
fn an_unstamped_container_is_whatever_an_annotation_decides_it_is() {
    // The one place `is` and an annotation part company, and they are answering
    // different questions rather than disagreeing. `is` reports what a value
    // already is, so an undescribed `[1, 2]` is a `list[any?]` — it has no
    // element type because nothing has given it one. A `let` is the thing that
    // gives it one, and is free to give it a narrower one than `any?`.
    let interp = run(
        "let raw = [1, 2]\n\
         let before = raw is list[int]\n\
         let ints: list[int] = raw\n\
         let after = raw is list[int]\n",
    );
    assert_eq!(global(&interp, "before"), Some(Value::Bool(false)));
    assert_eq!(global(&interp, "after"), Some(Value::Bool(true)));
}

#[test]
fn a_shorthand_header_is_a_claim_that_the_rest_is_unconstrained() {
    // `dict[K]` says "I only care about the keys" — and §3.10 spells out what
    // that means, which is that it is shorthand for `dict[K, _?]`. So it *is* a
    // claim about the values: the claim that they are anything at all. A
    // `dict[string]` is therefore not a `dict[string, int]`, for the same reason
    // a `list[any?]` is not a `list[int]`.
    //
    // This used to be accepted, on the reasoning that the shorthand said nothing
    // about values and so the values had to be walked. That was a hole and not a
    // convenience: `admitted` finds no argument at the value slot of a
    // `dict[string]` header and waves every write through, so the program below
    // used to finish with `{"a": 1, "x": "not an int"}` under an annotation
    // reading `dict[string, int]`.
    let program = crate::compile(
        "fn takes(d: dict[string, int]) { d[\"x\"] = \"not an int\" }\n\
         let loose: dict[string] = {\"a\": 1}\ntakes(loose)\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp
        .run(&program)
        .expect_err("the shorthand is not a `dict[string, int]`");
    assert_eq!(
        err.message,
        "`d` is `dict[string, int]`, but this is `dict[string]`"
    );

    // The other direction still holds, and by the same rule: a `dict[string,
    // int]` is a `dict[string]`, because `int` is one of the things the elided
    // `any?` admits.
    let interp = run(
        "fn keyed(d: dict[string]): int { return len(d) }\n\
         let scores: dict[string, int] = {\"a\": 1}\n\
         let n = keyed(scores)\n",
    );
    assert_eq!(global(&interp, "n"), Some(Value::Int(1)));
}

#[test]
fn two_declarations_that_only_an_empty_container_confuses() {
    // The pair is legal, because a container that says what it holds tells them
    // apart. What is refused is the *call* that does not say — which is a
    // property of the argument and so cannot be decided where they are declared.
    let interp = run(
        "fn total(xs: list[int]): string { return \"ints\" }\n\
         fn total(xs: list[string]): string { return \"strings\" }\n\
         let a = total([1, 2])\n\
         let b = total([\"x\"])\n\
         let described: list[string] = []\n\
         let c = total(described)\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::from("ints")));
    assert_eq!(global(&interp, "b"), Some(Value::from("strings")));
    assert_eq!(global(&interp, "c"), Some(Value::from("strings")));

    let program = crate::compile(
        "fn total(xs: list[int]): string { return \"ints\" }\n\
         fn total(xs: list[string]): string { return \"strings\" }\n\
         let a = total([])\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("nothing says which");
    assert_eq!(err.message, "more than one `total` takes (list)");
}

#[test]
fn a_type_parameter_is_checked_against_what_the_instance_was_built_with() {
    // The whole of v0.9 §3.1 in one assertion: `T` is not a new kind of type,
    // it is a type not yet written down, and the boundary a value crosses is
    // the ordinary one once it has been.
    let interp = run(
        "class Box[T] {\n\
         \x20   public let value: T? = nil\n\
         \x20   public fn set(v: T) { self.value = v }\n\
         }\n\
         let b = Box[int]()\n\
         b.set(7)\n\
         let held = b.value\n",
    );
    assert_eq!(global(&interp, "held"), Some(Value::Int(7)));

    let program = crate::compile(
        "class Box[T] {\n\
         \x20   public let value: T? = nil\n\
         \x20   public fn set(v: T) { self.value = v }\n\
         }\n\
         let b = Box[int]()\n\
         b.set(\"no\")\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("`T` is `int` on this one");
    // Reported as the type it stands for, not as `T`. A message naming the
    // parameter would be true and useless — the reader has to know what to pass.
    assert_eq!(err.message, "`v` is `int`, but this is a string");
}

#[test]
fn each_instance_carries_its_own_arguments() {
    // Two instances of one class, and the class object is the same object for
    // both — the arguments are on the *instance*, which is what lets `extend`
    // stay keyed by class handle. See `interp::generic`.
    let interp = run(
        "class Box[T] {\n\
         \x20   public let value: T? = nil\n\
         \x20   public fn set(v: T) { self.value = v }\n\
         }\n\
         let ints = Box[int]()\n\
         let words = Box[string]()\n\
         ints.set(1)\n\
         words.set(\"a\")\n\
         let a = ints is Box[int]\n\
         let b = ints is Box[string]\n\
         let c = words is Box[string]\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::Bool(true)));
    assert_eq!(global(&interp, "b"), Some(Value::Bool(false)));
    assert_eq!(global(&interp, "c"), Some(Value::Bool(true)));
}

#[test]
fn an_unsupplied_parameter_is_unconstrained_rather_than_absent() {
    // §3.1's defaulting. A bare `Box()` binds `T` to the top type, which is
    // gradual typing behaving as it does everywhere else — not an error, and
    // not `nil`.
    let interp = run(
        "class Box[T] {\n\
         \x20   public let value: T? = nil\n\
         \x20   public fn set(v: T) { self.value = v }\n\
         }\n\
         let loose = Box()\n\
         loose.set(1)\n\
         loose.set(\"a\")\n\
         let held = loose.value\n",
    );
    assert_eq!(global(&interp, "held"), Some(Value::from("a")));
}

#[test]
fn a_parameter_reaches_inside_a_container_annotation() {
    // `list[T]` on a `Stack[int]` is a `list[int]`, and the list is stamped
    // with that when the field crosses its annotation — so a later write is
    // refused by the header rather than by anything generics added.
    let program = crate::compile(
        "class Stack[T] {\n\
         \x20   private let items: list[T] = []\n\
         \x20   public fn sneak() { self.items.push(\"s\") }\n\
         }\n\
         let s = Stack[int]()\n\
         s.sneak()\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("the field holds ints");
    assert!(err.message.contains("`int`"), "{}", err.message);
}

#[test]
fn a_nested_construction_does_not_inherit_the_outer_header() {
    // `pending` is taken and not read: the `Box` built as an argument must not
    // be stamped with the `Stack`'s arguments, which is the bug a field rather
    // than a parameter invites.
    let interp = run(
        "class Box[T] { public op init() {} }\n\
         class Holder[T] {\n\
         \x20   public let held: any? = nil\n\
         \x20   public op init(held: any) { self.held = held }\n\
         }\n\
         let h = Holder[int](Box[string]())\n\
         let inner = h.held\n\
         let a = inner is Box[string]\n\
         let b = inner is Box[int]\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::Bool(true)));
    assert_eq!(global(&interp, "b"), Some(Value::Bool(false)));
}

#[test]
fn brackets_before_a_call_are_a_subscript_when_the_target_is_not_a_class() {
    // `Stack[int]()` and `handlers[i]()` are the same three tokens either side
    // of a bracket, and only the target says which. Nothing about generics may
    // cost the second form.
    let interp = run(
        "fn one(): int { return 1 }\n\
         let fns = [one]\n\
         let n = fns[0]()\n",
    );
    assert_eq!(global(&interp, "n"), Some(Value::Int(1)));
}

#[test]
fn an_annotation_binds_the_parameters_of_the_construction_beside_it() {
    // §3.1's inference. It has to reach the construction *before* the fields
    // run, because a field annotated `list[T]` is stamped as it crosses — an
    // annotation applied afterwards would arrive to find the list already
    // described as holding anything.
    let program = crate::compile(
        "class Stack[T] {\n\
         \x20   private let items: list[T] = []\n\
         \x20   public fn push(item: T) { self.items.push(item) }\n\
         }\n\
         let s: Stack[int] = Stack()\n\
         s.push(\"no\")\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("`T` came from the annotation");
    assert_eq!(err.message, "`item` is `int`, but this is a string");

    // And the header is what `is` reads back, so the inference is not a
    // check-time fiction.
    let interp = run(
        "class Stack[T] { public op init() {} }\n\
         let s: Stack[int] = Stack()\n\
         let a = s is Stack[int]\n\
         let b = s is Stack[string]\n",
    );
    assert_eq!(global(&interp, "a"), Some(Value::Bool(true)));
    assert_eq!(global(&interp, "b"), Some(Value::Bool(false)));
}

#[test]
fn a_written_argument_list_wins_over_the_annotation() {
    // A disagreement is something to report, not a gap to fill.
    let program = crate::compile(
        "class Stack[T] { public op init() {} }\n\
         let s: Stack[int] = Stack[string]()\n",
    )
    .expect("the program parses");
    let mut interp = Interp::with_output(Box::new(Vec::new()));
    let err = interp.run(&program).expect_err("the two disagree");
    assert_eq!(err.message, "`s` is `Stack[int]`, but this is `Stack[string]`");
}

#[test]
fn inference_does_not_reach_through_an_unrelated_call() {
    // The rule is syntactic and narrow on purpose: an annotation silently
    // reconfiguring a construction several calls down is not inference. Only a
    // bare construction of the very class named takes the lent header.
    let interp = run(
        "class Stack[T] { public op init() {} }\n\
         fn wrap(s): any { return s }\n\
         let loose: any = wrap(Stack())\n\
         let a = loose is Stack[int]\n\
         let b = loose is Stack[string]\n",
    );
    // Nothing described it, so it reads as every argument elided — a
    // `Stack[any?]`, the top type of its family, which is neither of these two
    // under the invariance v0.7 §4.1 settles. Unconstrained is a *state*, not a
    // wildcard: `is` answers about what a value is known to hold, and this one
    // is known to hold anything.
    assert_eq!(global(&interp, "a"), Some(Value::Bool(false)));
    assert_eq!(global(&interp, "b"), Some(Value::Bool(false)));

    // Which is what an annotation buys wherever one is written. Note the second
    // of these: the header comes from *crossing the return annotation*, which is
    // v0.7 §3.9's rule and not this milestone's — a value describes itself at
    // every annotated boundary it passes, and a generic instance is no
    // different from the list that rule was written for.
    for source in [
        "class Stack[T] { public op init() {} }\n\
         let told: Stack[int] = Stack()\n",
        "class Stack[T] { public op init() {} }\n\
         fn make(): Stack[int] { return Stack() }\n\
         let told = make()\n",
    ] {
        let interp = run(&format!(
            "{source}let a = told is Stack[int]\nlet b = told is Stack[string]\n"
        ));
        assert_eq!(global(&interp, "a"), Some(Value::Bool(true)), "{source}");
        assert_eq!(global(&interp, "b"), Some(Value::Bool(false)), "{source}");
    }
}

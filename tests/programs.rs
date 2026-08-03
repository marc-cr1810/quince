//! Runs every program in `tests/cases`, comparing against a companion file.
//!
//! A `.out` file holds the expected stdout; a `.err` file holds the expected
//! error message. Adding a case is dropping in two files — no Rust changes.
//!
//! A case is one `.qn` file, or a directory holding a `main.qn` and the modules
//! it imports. The companion files are named for the case and sit beside it
//! either way, so a case growing a second file changes nothing about how it is
//! checked. A directory case runs its `main.qn`, and its report names whichever
//! file actually raised — which is usually one of the imported ones.
//!
//! A `.in` file is the fourth companion and the only one that is an *input*: it
//! is what the case reads from standard input, and absent means empty. Optional
//! like `.report` and for a related reason — a case that never reads should not
//! have to say so, and one that does would otherwise be untestable, since
//! `io.line` at a terminal is not something a suite can arrange.
//!
//! A `.report` file holds the whole rendered diagnostic — the header, the
//! caret, every label, the help line. It is optional and the other two are not,
//! because the three assert different contracts: `.out` is what the program
//! printed, `.err` is the message a `catch` would see, and `.report` is what
//! someone reading the terminal gets. Only the last one has an opinion about
//! *where* the caret lands, which is why it exists at all — before it, a
//! milestone named "span-accurate diagnostics" had no test that any span was
//! accurate.
//!
//! Opt a case in by creating an empty `.report` beside it and running the suite
//! with `QUINCE_BLESS=1`, which fills in every `.report` that exists and does not
//! match. Blessing never creates one, so a case gains a rendered assertion only
//! because someone asked for it, and a report that changes under a refactor has
//! to be looked at before it is accepted.

use std::cell::RefCell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use quince::error::Result;
use quince::interp::Interp;

/// A writer the test can read back afterwards.
#[derive(Clone, Default)]
struct Captured(Rc<RefCell<Vec<u8>>>);

impl Captured {
    fn contents(&self) -> String {
        String::from_utf8(self.0.borrow().clone()).expect("output should be utf-8")
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs a program, returning its output or the error it failed with.
///
/// The whole error rather than its message, because `.report` needs the spans
/// and labels that `message` throws away.
fn run(source: &str, path: &Path, input: String) -> Result<String> {
    let program = quince::compile(source)?;
    let captured = Captured::default();
    let mut interp = Interp::with_io(
        Box::new(captured.clone()),
        Box::new(std::io::Cursor::new(input.into_bytes())),
    );
    // What the case's imports resolve against. A single-file case never uses it;
    // a directory case is entirely about it.
    interp.set_path(path.to_path_buf());
    match interp.run(&program) {
        Ok(()) => Ok(captured.contents()),
        Err(err) => Err(err),
    }
}

/// Whether `QUINCE_BLESS` asked for expected files to be rewritten.
fn blessing() -> bool {
    std::env::var_os("QUINCE_BLESS").is_some_and(|value| value != "0")
}

/// Compares one expectation, blessing it instead if that was asked for.
///
/// Trailing whitespace is trimmed from both ends of the comparison and nothing
/// else is touched: a report's internal padding is part of what it renders, and
/// a harness that normalised it could not tell a misaligned caret from a correct
/// one. What the trim buys is that a missing final newline in a companion file
/// is not a failure.
fn compare(
    failures: &mut Vec<String>,
    path: &Path,
    what: &str,
    expected: &str,
    actual: &str,
) {
    if expected.trim_end() == actual.trim_end() {
        return;
    }
    let name = path.file_stem().unwrap().to_string_lossy();
    if blessing() {
        std::fs::write(path, format!("{}\n", actual.trim_end()))
            .expect("a blessed file should be writable");
        return;
    }
    failures.push(format!(
        "{name}: {what} did not match\n  expected: {:?}\n  actual:   {:?}",
        expected.trim_end(),
        actual.trim_end()
    ));
}

/// Runs the corpus the way the binary does.
///
/// Through `with_stack` rather than a size of its own, so the test exercises
/// the configuration real programs get. `err_recursion.qn` recurses until the
/// interpreter's limit stops it, which is only a clean error if the stack is
/// large enough to reach that limit — so this is the case that would catch
/// `STACK_SIZE` and `MAX_DEPTH` drifting apart.
#[test]
fn cases_produce_their_expected_output() {
    quince::interp::with_stack(check_cases);
}

fn check_cases() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut checked = 0;
    let mut failures = Vec::new();

    // A case is either a `.qn` file or a directory holding a `main.qn` and the
    // modules it imports. The companions sit beside the one or beside the other,
    // named for the case either way, so nothing about `.out`/`.err`/`.report`
    // changes when a case grows a second file.
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/cases should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "qn") || path.join("main.qn").is_file()
        })
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        // A directory case runs its `main.qn`, and its reports name whichever
        // file raised — which for these cases is usually not that one.
        let (entry, reported_as) = match path.is_dir() {
            true => (path.join("main.qn"), "main.qn".to_string()),
            false => (path.clone(), format!("{name}.qn")),
        };
        let source = std::fs::read_to_string(&entry).expect("case should be readable");
        // Absent means empty, which is what a case that never reads should see.
        let input = std::fs::read_to_string(path.with_extension("in")).unwrap_or_default();
        let result = run(&source, &entry, input);

        let out_path = path.with_extension("out");
        let err_path = path.with_extension("err");
        let report_path = path.with_extension("report");
        let expected_out = std::fs::read_to_string(&out_path).ok();
        let expected_err = std::fs::read_to_string(&err_path).ok();
        let expected_report = std::fs::read_to_string(&report_path).ok();

        if expected_out.is_none() && expected_err.is_none() && expected_report.is_none() {
            failures.push(format!("{name}: has no .out, .err, or .report file"));
            continue;
        }

        match result {
            Ok(actual) => {
                checked += 1;
                match expected_out {
                    Some(expected) => {
                        compare(&mut failures, &out_path, "output", &expected, &actual)
                    }
                    // A `.err` or `.report` and no `.out` says the case is meant
                    // to fail, and it did not.
                    None => failures.push(format!(
                        "{name}: expected an error, succeeded with: {actual:?}"
                    )),
                }
            }
            Err(err) => {
                checked += 1;
                if expected_out.is_some() {
                    failures.push(format!("{name}: expected output, failed: {}", err.message));
                    continue;
                }
                if let Some(expected) = expected_err {
                    compare(&mut failures, &err_path, "error", &expected, &err.message);
                }
                if let Some(expected) = expected_report {
                    // The case's own file name rather than its path, so a report
                    // does not bake in where the repository happens to live. An
                    // error raised inside an imported module carries that
                    // module's text and is drawn against it — the same choice
                    // `main.rs` makes, and the reason a directory case can pin a
                    // caret in a file it did not start in.
                    let rendered = match &err.origin {
                        Some(origin) => err.report(&origin.text, &origin.path),
                        None => err.report(&source, &reported_as),
                    };
                    compare(&mut failures, &report_path, "report", &expected, &rendered);
                }
            }
        }
    }

    assert!(checked > 0, "no cases found in {}", dir.display());
    assert!(
        failures.is_empty(),
        "{} of {checked} cases failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// The recursion limit has to fire *before* the native stack runs out, and it
/// has to do that on whatever stack the host gave the caller.
///
/// Run from a thread far too small to hold 250 interpreter frames, so the test
/// fails — by crashing the process, loudly — if `with_stack` ever stops
/// supplying a stack of its own. Without it this program aborts on a 256 KiB
/// main stack rather than reporting anything.
#[test]
fn the_recursion_limit_fires_on_a_stack_too_small_to_reach_it() {
    let message = std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            quince::interp::with_stack(|| {
                let program = quince::compile("fn deep() { return deep() }\ndeep()")
                    .expect("the program should compile");
                let mut interp = Interp::with_output(Box::new(Vec::new()));
                interp
                    .run(&program)
                    .expect_err("recursing forever should fail")
                    .message
            })
        })
        .expect("should be able to spawn a small thread")
        .join()
        .expect("the run should not panic");

    assert!(
        message.contains("recursion limit"),
        "expected the limit to stop it, got: {message}"
    );
}

/// An expensive recursion shape is refused, by whichever guard sees it first.
///
/// `MAX_DEPTH` was calibrated against the cheapest recursion there is, a call in
/// a `return`. A call inside a printed value carries the renderer on the stack as
/// well, and this used to abort the process: 250 of those did not fit the stack
/// that 250 of the cheap kind fit comfortably, and a counter cannot see the
/// difference. That is what `out_of_stack` was added for.
///
/// **Which guard trips here is not fixed, and the test no longer asserts one.**
/// Boxing `QuinceError` took ~128 bytes out of every `Result` in the descent, and
/// that was enough for 250 expensive frames to fit under the reserve after all —
/// so the counter now answers first for this shape, where the measurement used to.
/// Both refusals are correct and neither is the point. The point is that the
/// process survives, and asserting the mechanism made an unrelated frame-size win
/// look like a failure.
///
/// The measured guard is still live and still the only thing that catches a shape
/// too expensive to reach the count — verified by raising `MAX_DEPTH` out of the
/// way, at which point this program trips `out_of_stack` exactly as it used to.
/// If both guards regress, this test does not fail politely: it takes the test
/// binary down with a stack overflow, which is the loud end of the trade and the
/// reason to keep it here rather than in a unit test.
#[test]
fn a_class_that_prints_itself_is_refused_rather_than_crashing() {
    let message = quince::interp::with_stack(|| {
        let program = quince::compile(
            "class Loud {\n\
             op init() { }\n\
             op string() { return \"I am \" + string(self) }\n\
             }\n\
             print(Loud())\n",
        )
        .expect("the program should compile");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        interp
            .run(&program)
            .expect_err("printing itself forever should fail")
            .message
    });

    assert!(
        message.contains("too deep") || message.contains("recursion limit"),
        "expected one of the recursion guards to stop it, got: {message}"
    );
}

/// The corpus runs the interpreter in-process, which once let the binary ship
/// without the resolver while every other test passed. This covers the wiring.
#[test]
fn the_binary_runs_a_program_end_to_end() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_quince"))
        .args(["run", "examples/hello.qn"])
        .output()
        .expect("the binary should run");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello, world\n");
}

/// What the pass claims about a program is what the program does.
///
/// The corpus is the only place these two can be put beside each other. A unit
/// test asserts that the pass says `int`; this asserts that the value which
/// ends up under that name *is* an int, over a hundred programs nobody wrote
/// with a type checker in mind. A pass that is allowed to answer "unknown" has
/// exactly one way to be wrong — answering something else — and this is what
/// looks for it.
///
/// Only globals, and only cases that ran to the end: a program that raised
/// stopped with its names in whatever state it stopped in, and a local is gone
/// by the time there is anything to read it from.
#[test]
fn what_the_pass_claims_is_what_the_programs_produce() {
    quince::interp::with_stack(check_inference);
}

fn check_inference() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut checked = 0;
    let mut claims = 0;
    let mut failures = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/cases should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "qn") || path.join("main.qn").is_file()
        })
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let entry = match path.is_dir() {
            true => path.join("main.qn"),
            false => path.clone(),
        };
        let source = std::fs::read_to_string(&entry).expect("case should be readable");
        // A case that does not compile is one of the error cases, and has no
        // program to run or to infer over.
        let Ok(program) = quince::compile(&source) else {
            continue;
        };
        let types = quince::sema::infer::infer(&program);

        let input = std::fs::read_to_string(path.with_extension("in")).unwrap_or_default();
        let mut interp = Interp::with_io(
            Box::new(Captured::default()),
            Box::new(std::io::Cursor::new(input.into_bytes())),
        );
        interp.set_path(entry);
        if interp.run(&program).is_err() {
            continue;
        }
        checked += 1;

        for (global, value) in interp.get_globals() {
            // Asked from the end of the file, which is where an editor asks
            // about a top-level name and where the program has just stopped.
            let claimed = types.of_name(&global, source.len() as u32);
            let Some(named) = claimed.class_name() else {
                continue;
            };
            claims += 1;
            let actual = value.type_name(&interp.heap);
            // A nullable claim is satisfied by `nil` as well as by its class:
            // `string?` says the name holds one or the other, so finding either
            // is the claim coming true rather than failing.
            if actual == "nil" && claimed.admits_nil() {
                continue;
            }
            // A claim of `Base` is satisfied by a `Derived`, because §4.1 says
            // a subclass holds as its parent — so an annotated binding is
            // inferred as what it was *annotated* while holding something more
            // specific, and both are true at once.
            let mut current = Some(value.class(&interp.heap));
            let mut descends = false;
            while let Some(id) = current {
                if interp.heap.class(id).name == named {
                    descends = true;
                    break;
                }
                current = interp.heap.class(id).parent;
            }
            if !descends {
                failures.push(format!(
                    "{name}: `{global}` was inferred as `{claimed}` and is a `{actual}`"
                ));
            }
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert!(checked > 40, "only {checked} cases ran");
    assert!(claims > 150, "only {claims} names were claimed, so this proves little");
}

/// Every native that declares what it returns, called, with the answer checked.
///
/// `Native::returns` is read by the inference pass and believed, so a wrong
/// entry is not a missing feature — it is the editor confidently naming the
/// wrong type, which is worse than the `Unknown` it replaced. Nothing in the
/// type system can check the field, because a native's body is Rust and its
/// return is a `Value` built at run time. So it is checked the only way it can
/// be: by calling the thing and asking what came back.
///
/// The table below is the one hand-written part, and the completeness assertion
/// at the end is what keeps it honest — a native that declares a return and has
/// no call here fails the test rather than going unchecked.
#[test]
fn every_declared_return_is_what_the_native_actually_returns() {
    quince::interp::with_stack(check_returns);
}

fn check_returns() {
    // A scratch file for `io`, whose members are the only ones that need the
    // world to exist before they can be called.
    let scratch = std::env::temp_dir().join("quince_returns_check.txt");
    let path = scratch.to_string_lossy().replace('\\', "\\\\");
    let _ = std::fs::write(&scratch, "one\ntwo\n");

    // `label` names where the native lives, so that two natives called `int`
    // are told apart. `expr` is a call that reaches it.
    let calls: &[(&str, String)] = &[
        ("math.floor", "math.floor(2.5)".into()),
        ("math.ceil", "math.ceil(2.5)".into()),
        ("math.round", "math.round(2.5)".into()),
        ("math.sqrt", "math.sqrt(4)".into()),
        ("math.pow", "math.pow(2, 3)".into()),
        ("io.read", format!("io.read(\"{path}\")")),
        ("io.write", format!("io.write(\"{path}\", \"one\\ntwo\\n\")")),
        ("io.append", format!("io.append(\"{path}\", \"\")")),
        ("io.exists", format!("io.exists(\"{path}\")")),
        ("io.lines", format!("io.lines(\"{path}\")")),
        ("time.now", "time.now()".into()),
        ("time.sleep", "time.sleep(0)".into()),
        ("random.seed", "random.seed(1)".into()),
        ("random.int", "random.int(1, 2)".into()),
        ("random.float", "random.float()".into()),
        ("global.print", "print()".into()),
        ("global.len", "len(\"ab\")".into()),
        ("global.type", "type(1)".into()),
        ("list.reverse", "[1, 2].reverse()".into()),
        ("list.find", "[1, 2].find(2)".into()),
        ("list.map", "[1, 2].map(twice)".into()),
        ("list.filter", "[1, 2].filter(big)".into()),
        ("list.sort", "[2, 1].sort()".into()),
        ("list.push", "[1].push(2)".into()),
        ("dict.keys", "{\"a\": 1}.keys()".into()),
        ("dict.values", "{\"a\": 1}.values()".into()),
        ("string.repeat", "\"ab\".repeat(2)".into()),
        ("string.upper", "\"ab\".upper()".into()),
        ("string.lower", "\"AB\".lower()".into()),
        ("string.trim", "\" a \".trim()".into()),
        ("string.starts_with", "\"ab\".starts_with(\"a\")".into()),
        ("string.ends_with", "\"ab\".ends_with(\"b\")".into()),
        ("string.replace", "\"ab\".replace(\"a\", \"c\")".into()),
        ("string.split", "\"a,b\".split(\",\")".into()),
        ("string.chars", "\"ab\".chars()".into()),
        ("string.join", "\",\".join([\"a\", \"b\"])".into()),
        ("new.int", "int(\"4\")".into()),
        ("new.float", "float(1)".into()),
        ("new.string", "string(1)".into()),
        ("new.bool", "bool(1)".into()),
        ("new.list", "list()".into()),
        ("new.dict", "dict()".into()),
    ];

    let mut source = String::from(
        "import math\nimport io\nimport time\nimport random\n\
         fn twice(n) { return n * 2 }\nfn big(n) { return n > 1 }\n",
    );
    for (label, expr) in calls {
        source.push_str(&format!("print(\"{label}\" + \" \" + type({expr}))\n"));
    }

    let captured = Captured::default();
    let mut interp = Interp::with_output(Box::new(captured.clone()));
    let program = quince::compile(&source).expect("the generated program should compile");
    interp
        .run(&program)
        .unwrap_or_else(|err| panic!("the generated program should run: {}", err.message));
    let _ = std::fs::remove_file(&scratch);

    let mut failures = Vec::new();
    let mut checked = 0;
    for line in captured.contents().lines() {
        // `print()` writes a blank line of its own before its label arrives, so
        // anything that is not a pair is not an answer.
        let Some((label, actual)) = line.split_once(' ') else {
            continue;
        };
        let native = native_named(label)
            .unwrap_or_else(|| panic!("`{label}` does not name a native"));
        checked += 1;
        match native.returns {
            Some(declared) if declared.name() != actual => failures.push(format!(
                "`{label}` declares it returns `{}` and returned a `{actual}`",
                declared.name()
            )),
            _ => {}
        }
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert_eq!(checked, calls.len(), "not every call reported");

    // And that the table above covers everything that makes a claim. A native
    // declaring a return with no call here would otherwise be believed by the
    // editor and checked by nothing.
    let labelled: Vec<&str> = calls.iter().map(|(label, _)| *label).collect();
    for (label, native) in every_native() {
        if native.returns.is_some() && !labelled.contains(&label.as_str()) {
            failures.push(format!("`{label}` declares a return and is never called here"));
        }
        assert!(!native.doc.is_empty(), "`{label}` has no documentation");
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Every native a program can reach, with the label this test names it by.
fn every_native() -> Vec<(String, &'static quince::runtime::value::Native)> {
    let mut found = Vec::new();
    for module in quince::builtins::stdlib::MODULES {
        for (name, member) in module.members {
            if let quince::builtins::stdlib::Member::Fn(native) = member {
                found.push((format!("{}.{name}", module.name), *native));
            }
        }
    }
    for builtin in quince::runtime::class::BUILTINS {
        let seed = builtin.seed();
        for (name, native) in seed.methods {
            found.push((format!("{}.{name}", seed.name), *native));
        }
        if let Some(init) = seed.init {
            found.push((format!("new.{}", seed.name), init));
        }
    }
    for native in quince::builtins::BUILTINS {
        found.push((format!("global.{}", native.name), *native));
    }
    found
}

fn native_named(label: &str) -> Option<&'static quince::runtime::value::Native> {
    every_native()
        .into_iter()
        .find_map(|(name, native)| (name == label).then_some(native))
}

/// Every native's parameter names, counted against the arity it declares.
///
/// Two numbers stating the same fact, so they can drift — and what a drift
/// costs is the editor labelling the wrong argument while someone types, which
/// looks exactly like knowledge. The counts differ by where the native lives,
/// and that difference is real rather than an inconsistency to be smoothed
/// over: a method seeded onto a type takes its receiver as `args[0]`, and a
/// module's member takes no receiver at all.
#[test]
fn every_native_names_the_parameters_it_takes() {
    let mut failures = Vec::new();
    for (label, native) in every_native() {
        // A variadic native has no count to check against. `print` takes any
        // number and `list` takes none or one, and `arity: None` is how both
        // say so.
        let Some(arity) = native.arity else {
            continue;
        };
        // A type's method is called on a receiver that `arity` counts and the
        // caller does not write; everything else is written in full.
        let receiver = usize::from(matches!(
            label.split('.').next(),
            Some("string" | "list" | "dict")
        ));
        let expected = arity - receiver;
        if native.params.len() != expected {
            failures.push(format!(
                "`{label}` declares arity {arity} and names {} parameter(s), expected {expected}",
                native.params.len()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A declared parameter type has to admit everything the implementation does.
///
/// The risk this milestone's native tables introduce: a declaration narrower
/// than the body is a working program refused, and it fails at the call rather
/// than anywhere near the table that got it wrong. `math` is the family where
/// the union matters — every one of these takes an int or a float — and it is
/// also the only family safe to call from a test, since the rest touch the
/// filesystem, the clock, or the RNG.
///
/// The others are covered by the corpus rather than from here, for that reason.
#[test]
fn a_declared_parameter_admits_what_the_builtin_actually_takes() {
    let numeric = [
        "floor(2.5)", "floor(2)", "ceil(2.5)", "ceil(2)", "round(2.5)", "round(2)",
        "abs(-2.5)", "abs(-2)", "sqrt(4.0)", "sqrt(4)", "pow(2, 3)", "pow(2.0, 3.0)",
        "min(1, 2.0)", "max(1.0, 2)",
    ];
    let mut failures = Vec::new();
    for call in numeric {
        let src = format!("from math import floor, ceil, round, abs, sqrt, pow, min, max\n{call}\n");
        let program = match quince::compile(&src) {
            Ok(program) => program,
            Err(err) => {
                failures.push(format!("{call}: did not compile: {}", err.message));
                continue;
            }
        };
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        if let Err(err) = interp.run(&program) {
            failures.push(format!("{call}: {}", err.message));
        }
    }
    assert!(
        failures.is_empty(),
        "a declaration refuses what its body accepts:\n{}",
        failures.join("\n")
    );
}

/// Every parameter that names types names ones that exist, and reads back
/// sensibly when a report quotes it.
#[test]
fn a_declared_parameter_reads_back_as_a_type() {
    let known: Vec<&str> = quince::runtime::class::BUILTINS
        .iter()
        .map(|builtin| builtin.name())
        .collect();
    for (label, native) in every_native() {
        for param in native.params {
            for accepted in param.accepts {
                assert!(
                    known.contains(&accepted.name()),
                    "`{label}` accepts `{}`, which is not a builtin type",
                    accepted.name()
                );
            }
            // An empty set is "anything" and says so; a non-empty one quotes
            // the types it names.
            let written = param.written();
            match param.accepts.is_empty() {
                true => assert_eq!(written, "any", "`{label}`'s `{}`", param.name),
                false => assert!(
                    written.contains('`'),
                    "`{label}`'s `{}` reads as {written}",
                    param.name
                ),
            }
        }
    }
}

/// Every error the corpus provokes explains what to do about it.
///
/// A message says what is wrong and a `help:` line says what to write instead,
/// and the second is the one a reader is actually looking for. This holds the
/// line at the level the corpus reaches, which is every error the language
/// raises that anybody thought worth a case.
///
/// The exceptions are listed rather than inferred, because "this one needs no
/// help" is a judgement and judgements should be written down where the next
/// person can disagree with one.
#[test]
fn every_error_says_what_to_do_about_it() {
    // Errors whose message is already the whole answer, or whose text is not
    // the language's to explain.
    const NO_HELP_NEEDED: &[&str] = &[
        // The message is a `throw`'s own, written by the program. The language
        // has no idea what would fix it.
        "err_throw_uncaught",
        // These already name the fix inside the message itself, and a `help:`
        // line would be the same sentence twice.
        "err_dict_statement",
        "err_split_empty",
        "err_dict_unhashable",
        // A grammar expectation. "expected `catch` after the `try` block" *is*
        // the instruction — the parser knows exactly which token is missing and
        // says so, and there is nothing a second line could add that the first
        // has not.
        "err_try_without_catch",
    ];

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut failures = Vec::new();
    let mut checked = 0;

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/cases should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "err"))
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if NO_HELP_NEEDED.contains(&name.as_str()) {
            continue;
        }
        let base = path.with_extension("");
        // A directory case runs its `main.qn`; a single-file case is itself.
        let entry = match base.join("main.qn").is_file() {
            true => base.join("main.qn"),
            false => base.with_extension("qn"),
        };
        let Ok(text) = std::fs::read_to_string(&entry) else {
            continue;
        };
        let input = std::fs::read_to_string(base.with_extension("in")).unwrap_or_default();
        let Err(err) = run(&text, &entry, input) else {
            continue;
        };
        checked += 1;
        if err.help.is_none() {
            failures.push(format!("{name}: {}", err.message));
        }
    }

    assert!(checked > 40, "only {checked} errors were reached");
    assert!(
        failures.is_empty(),
        "{} error(s) say what is wrong and not what to do:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Initializing a new project directory creates main.qn and .gitignore.
#[test]
fn initialising_a_new_project_creates_main_and_gitignore() {
    let temp = std::env::temp_dir().join(format!("quince_init_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_quince"))
        .args(["init", temp.to_str().unwrap()])
        .output()
        .expect("the binary should run");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let main_file = temp.join("main.qn");
    assert!(main_file.exists(), "main.qn should be created");
    let content = std::fs::read_to_string(&main_file).expect("main.qn should be readable");
    assert!(content.contains("fn main()"), "main.qn should contain starter function");

    let gitignore = temp.join(".gitignore");
    assert!(gitignore.exists(), ".gitignore should be created");

    let _ = std::fs::remove_dir_all(&temp);
}

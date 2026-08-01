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

use quince::error::QuinceError;
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
fn run(source: &str, path: &Path) -> Result<String, QuinceError> {
    let program = quince::compile(source)?;
    let captured = Captured::default();
    let mut interp = Interp::with_output(Box::new(captured.clone()));
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
        let result = run(&source, &entry);

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

/// A count of calls is not what the limit is really about.
///
/// `MAX_DEPTH` was calibrated against the cheapest recursion there is, a call in
/// a `return`. A call inside a printed value carries the renderer on the stack as
/// well, and 250 of those do not fit the stack that 250 of the cheap kind fit
/// comfortably — the counter cannot see the difference, and this used to abort
/// the process. If the measured guard regresses, this test does not fail politely
/// either: it takes the test binary down with a stack overflow, which is the loud
/// end of the trade and the reason to keep it here rather than in a unit test.
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
        message.contains("too deep"),
        "expected the stack guard to stop it, got: {message}"
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

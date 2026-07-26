//! Runs every program in `tests/cases`, comparing against a companion file.
//!
//! A `.out` file holds the expected stdout; a `.err` file holds the expected
//! error message. Adding a case is dropping in two files — no Rust changes.

use std::cell::RefCell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

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

/// Runs a program, returning its output or the message it failed with.
fn run(source: &str) -> Result<String, String> {
    let program = quince::compile(source).map_err(|err| err.message)?;
    let captured = Captured::default();
    let mut interp = Interp::with_output(Box::new(captured.clone()));
    match interp.run(&program) {
        Ok(()) => Ok(captured.contents()),
        Err(err) => Err(err.message),
    }
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

    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/cases should exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "qn"))
        .collect();
    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let source = std::fs::read_to_string(&path).expect("case should be readable");
        let result = run(&source);

        let expected_out = std::fs::read_to_string(path.with_extension("out")).ok();
        let expected_err = std::fs::read_to_string(path.with_extension("err")).ok();

        match (expected_out, expected_err, result) {
            (Some(expected), _, Ok(actual)) => {
                checked += 1;
                if expected.trim_end() != actual.trim_end() {
                    failures.push(format!(
                        "{name}: output did not match\n  expected: {:?}\n  actual:   {:?}",
                        expected.trim_end(),
                        actual.trim_end()
                    ));
                }
            }
            (_, Some(expected), Err(actual)) => {
                checked += 1;
                if expected.trim_end() != actual.trim_end() {
                    failures.push(format!(
                        "{name}: error did not match\n  expected: {:?}\n  actual:   {:?}",
                        expected.trim_end(),
                        actual.trim_end()
                    ));
                }
            }
            (Some(_), _, Err(err)) => {
                failures.push(format!("{name}: expected output, failed: {err}"))
            }
            (_, Some(_), Ok(out)) => failures.push(format!(
                "{name}: expected an error, succeeded with: {out:?}"
            )),
            (None, None, _) => failures.push(format!("{name}: has neither a .out nor a .err file")),
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

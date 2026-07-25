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

/// Runs the corpus on a thread whose stack size this test chooses.
///
/// `err_recursion.qn` deliberately recurses until the interpreter's own limit
/// stops it, and how much native stack those 250 frames cost moves with the
/// build profile and with edits to `eval` that have nothing to do with
/// recursion. Relying on whatever the test harness happens to default to means
/// an unrelated change turns a clean error message into a SIGSEGV, which is
/// what it did. 8 MiB is what the main thread of a real `quince run` gets.
///
/// This makes the *test* honest about what it needs. It does not give the
/// interpreter that guarantee in production — see `MAX_DEPTH` in `interp.rs`.
#[test]
fn cases_produce_their_expected_output() {
    let checker = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(check_cases)
        .expect("should be able to spawn a thread");

    // Propagates the original panic rather than a wrapper, so a failing case
    // still reports which case and why.
    if let Err(payload) = checker.join() {
        std::panic::resume_unwind(payload);
    }
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

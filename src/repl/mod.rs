//! The interactive prompt.
//!
//! Five files: this one is the loop and the meta-command list, and each of the
//! others is one thing the prompt does. [`snapshot`] is what the REPL knows —
//! taken from live values rather than from inference, which is the one advantage
//! it has over the editor — and [`helper`], [`highlight`], and [`meta`] are
//! completion, colouring, and the `:` commands.
//!
//! The two surfaces answer the same questions from different sides, and both go
//! through [`crate::cursor`] before they answer anything.

pub mod helper;
pub mod highlight;
pub mod meta;
pub mod snapshot;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use anyhow::Result;
use rustyline::highlight::Highlighter;

use quince::color::Style;
use quince::interp::Interp;
use quince::interp::show::Ask;
use quince::runtime::value::Value;
use crate::repl::helper::QuinceHelper;
use crate::repl::highlight::count_open_braces;
use crate::repl::meta::{handle_meta_command, MetaAction};
use crate::repl::snapshot::Snapshot;

const META_COMMANDS: &[&str] = &[
    ":help", ":vars", ":type", ":ast", ":tokens", ":clear", ":load", ":time", ":exit", ":quit",
];

/// Runs the interactive REPL with live syntax highlighting and line history.
pub fn run_repl(use_color_stdout: bool, use_color_stderr: bool) -> Result<()> {
    let pkg_name = Style::BOLD_CYAN.paint("quince", use_color_stdout);
    let version = Style::YELLOW.paint(env!("CARGO_PKG_VERSION"), use_color_stdout);
    let hint = Style::DIM.paint("ctrl-d or :exit to exit, :help for commands", use_color_stdout);
    println!("{pkg_name} {version} — {hint}");

    let config = rustyline::Config::builder().auto_add_history(true).build();
    let mut rl = rustyline::Editor::with_config(config)?;
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));

    rl.set_helper(Some(QuinceHelper {
        use_color: use_color_stdout,
        snapshot: Arc::clone(&snapshot),
    }));

    let mut interp = Interp::new();
    let mut buffer = String::new();

    loop {
        // What the interpreter knows, re-read after every entry. One call,
        // because the answer is in the objects rather than in a copy of them.
        if let Ok(mut held) = snapshot.lock() {
            *held = Snapshot::of(&interp);
        }

        let open_braces = count_open_braces(&buffer);
        let mut line = match if buffer.is_empty() {
            rl.readline(">>> ")
        } else {
            let initial = "    ".repeat(open_braces);
            rl.readline_with_initial("... ", (&initial, ""))
        } {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                buffer.clear();
                println!("^C");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("{}", Style::DIM.paint("Goodbye!", use_color_stdout));
                break;
            }
            Err(err) => {
                return Err(err.into());
            }
        };

        // Auto-dedent if the line starts with a closing brace '}' and has 4+ leading spaces
        if line.trim_start().starts_with('}') && line.starts_with("    ") {
            line = line[4..].to_string();
            if use_color_stdout {
                let prompt_dot = Style::BOLD_YELLOW.paint("...", true);
                let highlighted = match rl.helper() {
                    Some(h) => h.highlight(&line, line.len()),
                    None => Cow::Borrowed(&line[..]),
                };
                println!("\x1B[1A\x1B[2K{prompt_dot} {highlighted}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            } else {
                println!("\x1B[1A\x1B[2K... {line}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        }

        // Handle REPL Meta-Commands & exit commands
        let trimmed_line = line.trim();
        if buffer.is_empty() {
            match handle_meta_command(
                trimmed_line,
                &mut interp,
                use_color_stdout,
                use_color_stderr,
            )? {
                MetaAction::Handled => continue,
                MetaAction::Exit => {
                    println!("{}", Style::DIM.paint("Goodbye!", use_color_stdout));
                    break;
                }
                MetaAction::NotMeta => {}
            }
        }

        buffer.push_str(&line);
        buffer.push('\n');

        // Resolved against what the session already has bound, so an entry is
        // checked the way a line of a file would be: a class declared earlier is
        // a class this line can see, and a declaration that cannot be told apart
        // from one already there is refused rather than quietly replacing it.
        let program = match quince::compile_within(&buffer, &interp.declarations()) {
            Ok(program) => program,
            Err(err) if err.span.start as usize >= buffer.trim_end().len() => continue,
            Err(err) => {
                eprintln!("{}", err.report_styled(&buffer, "<repl>", use_color_stderr));
                buffer.clear();
                continue;
            }
        };

        let source = std::mem::take(&mut buffer);

        match interp.run_repl(&program) {
            Ok(Some(Value::Nil)) | Ok(None) => {}
            // Printing the echo is itself a call into the program once a class
            // can define `op string`, so it can fail — and a failure there is a
            // Quince error to report, not a reason to leave the REPL. `_` is
            // bound either way: the expression evaluated, and only printing it
            // did not.
            Ok(Some(value)) => {
                let printed = interp.display_pretty(&value, use_color_stdout, Ask::Class);
                interp.set_global("_", value);
                match printed {
                    Ok(text) => println!("{text}"),
                    Err(err) => {
                        eprintln!("{}", err.report_styled(&source, "<repl>", use_color_stderr))
                    }
                }
            }
            Err(err) => eprintln!("{}", err.report_styled(&source, "<repl>", use_color_stderr)),
        }
    }

    Ok(())
}

mod cli;

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use clap::Parser as _;

use quince::error::QuinceError;
use quince::interp::Interp;
use quince::lexer::Lexer;
use quince::value::Value;

use crate::cli::{Cli, Command, Dump};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Run { file, dump } => {
            let source = std::fs::read_to_string(&file)
                .with_context(|| format!("could not read {}", file.display()))?;
            run(&source, &file.display().to_string(), dump);
            Ok(())
        }
        Command::Repl => repl(),
    }
}

fn run(source: &str, path: &str, dump: Option<Dump>) {
    let tokens = report(Lexer::new(source).tokenize(), source, path);

    if dump == Some(Dump::Tokens) {
        for token in &tokens {
            println!(
                "{:>4}..{:<4} {:?}",
                token.span.start, token.span.end, token.kind
            );
        }
        return;
    }

    // Dumped after resolution, so the slot each name was assigned is visible.
    let program = report(quince::compile_tokens(tokens), source, path);

    if dump == Some(Dump::Ast) {
        for stmt in &program {
            println!("{stmt:#?}");
        }
        return;
    }

    report(Interp::new().run(&program), source, path);
}

/// A line-at-a-time REPL.
///
/// An error whose span sits at the very end of the input means the parser ran
/// out of tokens mid-construct, so the entry is treated as unfinished and more
/// input is read instead of reporting it.
fn repl() -> Result<()> {
    println!("quince {} — ctrl-d to exit", env!("CARGO_PKG_VERSION"));
    let mut interp = Interp::new();
    let stdin = std::io::stdin();
    let mut buffer = String::new();

    loop {
        print!("{} ", if buffer.is_empty() { ">" } else { "." });
        std::io::stdout().flush()?;

        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            println!();
            return Ok(());
        }
        buffer.push_str(&line);

        let program = match quince::compile(&buffer) {
            Ok(program) => program,
            Err(err) if err.span.start as usize >= buffer.trim_end().len() => continue,
            Err(err) => {
                eprintln!("{}", err.report(&buffer, "<repl>"));
                buffer.clear();
                continue;
            }
        };
        // Held until the entry has run, so a runtime error still has a source to
        // point at.
        let source = std::mem::take(&mut buffer);

        match interp.run_repl(&program) {
            // `nil` is what every statement-like call evaluates to, so echoing it
            // would put a `nil` under every `print`.
            Ok(Some(Value::Nil)) | Ok(None) => {}
            Ok(Some(value)) => println!("{}", value.display(&interp.heap)),
            Err(err) => eprintln!("{}", err.report(&source, "<repl>")),
        }
    }
}

/// Renders a compile or runtime error against its source and exits.
///
/// These diagnostics already carry a location and caret, so they bypass anyhow
/// rather than picking up a second "Error:" prefix.
fn report<T>(result: Result<T, QuinceError>, source: &str, path: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{}", err.report(source, path));
            std::process::exit(1);
        }
    }
}

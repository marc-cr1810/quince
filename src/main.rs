mod cli;

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use clap::Parser as _;

use quince::color::Style;
use quince::error::QuinceError;
use quince::interp::Interp;
use quince::lexer::Lexer;
use quince::value::Value;

use crate::cli::{Cli, Command, Dump};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let use_color_stdout = cli.color.use_color_stdout();
    let use_color_stderr = cli.color.use_color_stderr();

    match cli.command {
        // Both paths run on a thread Quince sizes itself rather than on
        // whatever stack the platform handed `main`. See `STACK_SIZE`.
        Command::Run { file, dump } => {
            let source = std::fs::read_to_string(&file)
                .with_context(|| format!("could not read {}", file.display()))?;
            let path = file.display().to_string();
            quince::interp::with_stack(|| run(&source, &path, dump, use_color_stdout, use_color_stderr));
            Ok(())
        }
        Command::Repl => {
            quince::interp::with_stack(|| repl(use_color_stdout, use_color_stderr))
        }
    }
}

fn run(
    source: &str,
    path: &str,
    dump: Option<Dump>,
    use_color_stdout: bool,
    use_color_stderr: bool,
) {
    let tokens = report(
        Lexer::new(source).tokenize(),
        source,
        path,
        use_color_stderr,
    );

    if dump == Some(Dump::Tokens) {
        for token in &tokens {
            let span_str = Style::DIM.paint(
                format!("{:>4}..{:<4}", token.span.start, token.span.end),
                use_color_stdout,
            );
            let kind_str = Style::BOLD_CYAN.paint(
                format!("{:?}", token.kind),
                use_color_stdout,
            );
            println!("{span_str} {kind_str}");
        }
        return;
    }

    // Dumped after resolution, so the slot each name was assigned is visible.
    let program = report(
        quince::compile_tokens(tokens),
        source,
        path,
        use_color_stderr,
    );

    if dump == Some(Dump::Ast) {
        for stmt in &program {
            let ast_str = Style::CYAN.paint(format!("{stmt:#?}"), use_color_stdout);
            println!("{ast_str}");
        }
        return;
    }

    report(
        Interp::new().run(&program),
        source,
        path,
        use_color_stderr,
    );
}

/// A line-at-a-time REPL.
///
/// An error whose span sits at the very end of the input means the parser ran
/// out of tokens mid-construct, so the entry is treated as unfinished and more
/// input is read instead of reporting it.
fn repl(use_color_stdout: bool, use_color_stderr: bool) -> Result<()> {
    let pkg_name = Style::BOLD_CYAN.paint("quince", use_color_stdout);
    let version = Style::YELLOW.paint(env!("CARGO_PKG_VERSION"), use_color_stdout);
    let hint = Style::DIM.paint("ctrl-d to exit", use_color_stdout);
    println!("{pkg_name} {version} — {hint}");

    let mut interp = Interp::new();
    let stdin = std::io::stdin();
    let mut buffer = String::new();

    loop {
        let prompt_char = if buffer.is_empty() { ">" } else { "." };
        let prompt = if buffer.is_empty() {
            Style::BOLD_GREEN.paint(prompt_char, use_color_stdout)
        } else {
            Style::BOLD_YELLOW.paint(prompt_char, use_color_stdout)
        };

        print!("{prompt} ");
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
                eprintln!("{}", err.report_styled(&buffer, "<repl>", use_color_stderr));
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
            Ok(Some(value)) => println!("{}", value.display_styled(&interp.heap, use_color_stdout)),
            Err(err) => eprintln!("{}", err.report_styled(&source, "<repl>", use_color_stderr)),
        }
    }
}

/// Renders a compile or runtime error against its source and exits.
///
/// These diagnostics already carry a location and caret, so they bypass anyhow
/// rather than picking up a second "Error:" prefix.
fn report<T>(
    result: Result<T, QuinceError>,
    source: &str,
    path: &str,
    color: bool,
) -> T {
    match result {
        Ok(value) => value,
        Err(err) => {
            eprintln!("{}", err.report_styled(source, path, color));
            std::process::exit(1);
        }
    }
}

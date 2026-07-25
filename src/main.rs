mod cli;
mod repl;

use anyhow::{Context, Result};
use clap::Parser as _;

use quince::color::Style;
use quince::error::QuinceError;
use quince::interp::Interp;
use quince::lexer::Lexer;

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
            quince::interp::with_stack(|| repl::run_repl(use_color_stdout, use_color_stderr))
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

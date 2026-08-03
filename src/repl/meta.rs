//! The `:` commands — everything the prompt does that is not Quince.

use anyhow::Result;

use std::time::Instant;

use quince::color::Style;
use quince::interp::Interp;
use quince::interp::show::Ask;
use quince::syntax::lexer::Lexer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetaAction {
    Handled,
    NotMeta,
    Exit,
}

pub(crate) fn handle_meta_command(
    input: &str,
    interp: &mut Interp,
    use_color_stdout: bool,
    use_color_stderr: bool,
) -> Result<MetaAction> {
    let mut parts = input.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
        ":exit" | ":quit" => Ok(MetaAction::Exit),
        ":help" => {
            println!(
                "{}",
                Style::BOLD_CYAN.paint("Quince REPL Meta-Commands:", use_color_stdout)
            );
            println!(
                "  {}   Display this help message",
                Style::YELLOW.paint(":help", use_color_stdout)
            );
            println!(
                "  {}   List all declared global variables",
                Style::YELLOW.paint(":vars", use_color_stdout)
            );
            println!(
                "  {}   Show the runtime type of an expression",
                Style::YELLOW.paint(":type <expr>", use_color_stdout)
            );
            println!(
                "  {}    Dump the compiled AST of an expression",
                Style::YELLOW.paint(":ast <expr>", use_color_stdout)
            );
            println!(
                "  {} Dump tokens for an expression",
                Style::YELLOW.paint(":tokens <expr>", use_color_stdout)
            );
            println!(
                "  {}   Load and run a Quince script file",
                Style::YELLOW.paint(":load <file>", use_color_stdout)
            );
            println!(
                "  {}   Time the execution of an expression",
                Style::YELLOW.paint(":time <expr>", use_color_stdout)
            );
            println!(
                "  {}  Clear screen and reset REPL environment",
                Style::YELLOW.paint(":clear", use_color_stdout)
            );
            println!(
                "  {}   Exit the REPL",
                Style::YELLOW.paint(":exit", use_color_stdout)
            );
            println!(
                "  {}   Exit the REPL",
                Style::YELLOW.paint(":quit", use_color_stdout)
            );
            Ok(MetaAction::Handled)
        }
        ":vars" => {
            let globals = interp.get_globals();
            if globals.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("No global variables defined.", use_color_stdout)
                );
            } else {
                for (name, val) in globals {
                    let name_str = Style::BOLD.paint(&name, use_color_stdout);
                    // Structural on purpose, and the one place that is right: this
                    // lists the environment rather than echoing a result, and it
                    // is what you would reach for to debug a class whose
                    // `op string` is what went wrong. Running it here would mean a
                    // broken op could break the tool for finding it — the same
                    // trade error messages make.
                    //
                    // Which is a promise this line cannot keep on its own yet:
                    // the renderer gains the argument that says "do not ask" in
                    // the step that gives it something to ask, and this is one of
                    // the two callers that has to pass it.
                    let val_str = match interp.display_pretty(&val, use_color_stdout, Ask::Nothing) {
                        Ok(text) => text,
                        Err(err) => {
                            eprintln!("{}", err.report_styled("", "<repl>", use_color_stderr));
                            continue;
                        }
                    };
                    let type_str = Style::DIM.paint(
                        format!("({})", val.type_name(&interp.heap)),
                        use_color_stdout,
                    );
                    println!("{name_str} = {val_str} {type_str}");
                }
            }
            Ok(MetaAction::Handled)
        }
        ":type" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :type <expression>", use_color_stdout)
                );
                return Ok(MetaAction::Handled);
            }
            match quince::compile(arg) {
                Ok(program) => match interp.run_repl(&program) {
                    Ok(Some(val)) => {
                        println!(
                            "{}",
                            Style::CYAN.paint(val.type_name(&interp.heap), use_color_stdout)
                        );
                    }
                    Ok(None) => println!("{}", Style::DIM.paint("nil", use_color_stdout)),
                    Err(err) => eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr)),
                },
                Err(err) => eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr)),
            }
            Ok(MetaAction::Handled)
        }
        ":ast" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :ast <expression>", use_color_stdout)
                );
                return Ok(MetaAction::Handled);
            }
            match quince::compile(arg) {
                Ok(program) => {
                    for stmt in &program {
                        println!(
                            "{}",
                            Style::CYAN.paint(format!("{stmt:#?}"), use_color_stdout)
                        );
                    }
                }
                Err(err) => eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr)),
            }
            Ok(MetaAction::Handled)
        }
        ":tokens" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :tokens <expression>", use_color_stdout)
                );
                return Ok(MetaAction::Handled);
            }
            match Lexer::new(arg).tokenize() {
                Ok(tokens) => {
                    for token in &tokens {
                        let span_str = Style::DIM.paint(
                            format!("{:>4}..{:<4}", token.span.start, token.span.end),
                            use_color_stdout,
                        );
                        let kind_str =
                            Style::BOLD_CYAN.paint(format!("{:?}", token.kind), use_color_stdout);
                        println!("{span_str} {kind_str}");
                    }
                }
                Err(err) => eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr)),
            }
            Ok(MetaAction::Handled)
        }
        ":load" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :load <filename.q>", use_color_stdout)
                );
                return Ok(MetaAction::Handled);
            }
            let source = match std::fs::read_to_string(arg) {
                Ok(src) => src,
                Err(err) => {
                    eprintln!(
                        "{}",
                        Style::RED.paint(format!("could not read {arg}: {err}"), use_color_stderr)
                    );
                    return Ok(MetaAction::Handled);
                }
            };
            let program = match quince::compile(&source) {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("{}", err.report_styled(&source, arg, use_color_stderr));
                    return Ok(MetaAction::Handled);
                }
            };
            match interp.run_repl(&program) {
                Ok(_) => println!(
                    "{}",
                    Style::GREEN.paint(format!("Loaded {arg}"), use_color_stdout)
                ),
                Err(err) => eprintln!("{}", err.report_styled(&source, arg, use_color_stderr)),
            }
            Ok(MetaAction::Handled)
        }
        ":time" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :time <expression>", use_color_stdout)
                );
                return Ok(MetaAction::Handled);
            }
            let start = Instant::now();
            let program = match quince::compile(arg) {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr));
                    return Ok(MetaAction::Handled);
                }
            };
            match interp.run_repl(&program) {
                Ok(Some(val)) => {
                    let elapsed = start.elapsed();
                    let val_str = match interp.display_pretty(&val, use_color_stdout, Ask::Class) {
                        Ok(text) => text,
                        Err(err) => {
                            eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr));
                            return Ok(MetaAction::Handled);
                        }
                    };
                    let time_str = Style::DIM
                        .paint(format!("(evaluated in {:.2?})", elapsed), use_color_stdout);
                    println!("{val_str} {time_str}");
                }
                Ok(None) => {
                    let elapsed = start.elapsed();
                    let time_str = Style::DIM
                        .paint(format!("(evaluated in {:.2?})", elapsed), use_color_stdout);
                    println!("{time_str}");
                }
                Err(err) => eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr)),
            }
            Ok(MetaAction::Handled)
        }
        ":clear" => {
            print!("\x1B[2J\x1B[1;1H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            *interp = Interp::new();
            Ok(MetaAction::Handled)
        }
        "exit" | "quit" | "exit()" | "quit()" => Ok(MetaAction::Exit),
        _ if input.starts_with(':') => {
            println!(
                "{}",
                Style::RED.paint(
                    format!("Unknown command `{input}`. Type :help for commands."),
                    use_color_stdout
                )
            );
            Ok(MetaAction::Handled)
        }
        _ => Ok(MetaAction::NotMeta),
    }
}

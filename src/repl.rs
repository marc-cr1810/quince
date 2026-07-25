use anyhow::Result;
use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use quince::class;
use quince::color::Style;
use quince::interp::Interp;
use quince::lexer::Lexer;
use quince::token::TokenKind;
use quince::value::Value;

const KEYWORDS: &[&str] = &[
    "fn", "op", "class", "extends", "self", "super", "let", "final", "const", "if", "else",
    "while", "for", "in", "return", "try", "catch", "throw", "true", "false", "nil",
];

const META_COMMANDS: &[&str] = &[
    ":help", ":vars", ":type", ":ast", ":tokens", ":clear", ":load", ":time",
];

/// Every method name any builtin type has, read off the type tables rather than
/// restated, so completion cannot offer a method the language does not have.
fn method_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = class::BUILTINS
        .iter()
        .flat_map(|builtin| builtin.methods.iter().map(|(name, _)| *name))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[derive(Clone)]
pub struct QuinceHelper {
    pub use_color: bool,
    pub globals: Arc<Mutex<Vec<String>>>,
}

impl Helper for QuinceHelper {}

impl Completer for QuinceHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (start, word) = extract_word(line, pos);
        let mut matches = Vec::new();

        if word.starts_with(':') {
            for cmd in META_COMMANDS {
                if cmd.starts_with(word) {
                    matches.push(Pair {
                        display: cmd.to_string(),
                        replacement: cmd.to_string(),
                    });
                }
            }
            return Ok((start, matches));
        }

        if start > 0 && line.as_bytes().get(start - 1) == Some(&b'.') {
            for method in method_names() {
                if method.starts_with(word) {
                    matches.push(Pair {
                        display: method.to_string(),
                        replacement: method.to_string(),
                    });
                }
            }
            return Ok((start, matches));
        }

        let mut string_candidates = Vec::new();
        if let Ok(globals) = self.globals.lock() {
            string_candidates = globals.clone();
        }

        for cand in KEYWORDS {
            if cand.starts_with(word) && *cand != word {
                matches.push(Pair {
                    display: cand.to_string(),
                    replacement: cand.to_string(),
                });
            }
        }

        for cand in &string_candidates {
            if cand.starts_with(word) && cand != word {
                matches.push(Pair {
                    display: cand.clone(),
                    replacement: cand.clone(),
                });
            }
        }

        matches.sort_by(|a, b| a.display.cmp(&b.display));
        matches.dedup_by(|a, b| a.display == b.display);

        Ok((start, matches))
    }
}

fn extract_word(line: &str, pos: usize) -> (usize, &str) {
    let line_up_to_pos = &line[..pos.min(line.len())];
    let start = line_up_to_pos
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .map(|i| i + 1)
        .unwrap_or(0);
    (start, &line_up_to_pos[start..])
}

impl Hinter for QuinceHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<Self::Hint> {
        if pos < line.len() || line.trim().is_empty() {
            return None;
        }

        let (start, word) = extract_word(line, pos);
        if word.is_empty() {
            return None;
        }

        let mut candidates = Vec::new();
        if word.starts_with(':') {
            candidates.extend(META_COMMANDS.iter().copied());
        } else if start > 0 && line.as_bytes().get(start - 1) == Some(&b'.') {
            candidates.extend(method_names());
        } else {
            candidates.extend(KEYWORDS.iter().copied());
            if let Ok(globals) = self.globals.lock() {
                for g in globals.iter() {
                    if g.starts_with(word) && g != word {
                        return Some(g[word.len()..].to_string());
                    }
                }
            }
        }

        for cand in candidates {
            if cand.starts_with(word) && cand != word {
                return Some(cand[word.len()..].to_string());
            }
        }

        None
    }
}

impl Validator for QuinceHelper {}

impl Highlighter for QuinceHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        if !self.use_color {
            return Cow::Borrowed(line);
        }

        let matched_brackets = find_matching_brackets(line, pos);
        let mut output = String::with_capacity(line.len() + 64);
        let mut last_end = 0;

        let tokens = match Lexer::new(line).tokenize() {
            Ok(tokens) => tokens,
            Err(err) => {
                let err_start = err.span.start as usize;
                let valid_prefix = &line[..err_start.min(line.len())];
                match Lexer::new(valid_prefix).tokenize() {
                    Ok(tokens) => tokens,
                    Err(_) => return Cow::Borrowed(line),
                }
            }
        };

        for token in tokens {
            if token.kind == TokenKind::Eof {
                continue;
            }

            let start = token.span.start as usize;
            let end = token.span.end as usize;

            if start > line.len() || end > line.len() || start < last_end {
                continue;
            }

            if start > last_end {
                push_trivia_with_brackets(
                    &mut output,
                    &line[last_end..start],
                    last_end,
                    &matched_brackets,
                    self.use_color,
                );
            }

            let text = &line[start..end];
            let styled = if text.len() == 1 && matched_brackets.contains(&start) {
                Style::BOLD_YELLOW.paint(text, true)
            } else {
                highlight_token(token.kind, text, self.use_color)
            };
            output.push_str(&styled);
            last_end = end;
        }

        if last_end < line.len() {
            push_trivia_with_brackets(
                &mut output,
                &line[last_end..],
                last_end,
                &matched_brackets,
                self.use_color,
            );
        }

        Cow::Owned(output)
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        if self.use_color {
            Cow::Owned(format!("\x1b[2;90m{hint}\x1b[0m"))
        } else {
            Cow::Borrowed(hint)
        }
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if !self.use_color {
            return Cow::Borrowed(prompt);
        }

        if prompt.starts_with('>') {
            Cow::Owned(format!("{} ", Style::BOLD_GREEN.paint(">", true)))
        } else if prompt.starts_with('.') {
            let rest = &prompt[1..];
            Cow::Owned(format!("{}{rest}", Style::BOLD_YELLOW.paint(".", true)))
        } else {
            Cow::Borrowed(prompt)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _forced: bool) -> bool {
        true
    }
}

fn find_matching_brackets(line: &str, pos: usize) -> Vec<usize> {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return Vec::new();
    }

    let check_positions = [pos, pos.saturating_sub(1)];
    for &check_pos in &check_positions {
        if check_pos >= bytes.len() {
            continue;
        }
        let target = bytes[check_pos];
        let (open, close, forward) = match target {
            b'(' => (b'(', b')', true),
            b'[' => (b'[', b']', true),
            b'{' => (b'{', b'}', true),
            b')' => (b'(', b')', false),
            b']' => (b'[', b']', false),
            b'}' => (b'{', b'}', false),
            _ => continue,
        };

        let mut depth = 0;
        if forward {
            for i in check_pos..bytes.len() {
                if bytes[i] == open {
                    depth += 1;
                } else if bytes[i] == close {
                    depth -= 1;
                    if depth == 0 {
                        return vec![check_pos, i];
                    }
                }
            }
        } else {
            for i in (0..=check_pos).rev() {
                if bytes[i] == close {
                    depth += 1;
                } else if bytes[i] == open {
                    depth -= 1;
                    if depth == 0 {
                        return vec![check_pos, i];
                    }
                }
            }
        }
    }
    Vec::new()
}

fn push_trivia_with_brackets(
    output: &mut String,
    trivia: &str,
    base_idx: usize,
    matched_brackets: &[usize],
    use_color: bool,
) {
    let mut current = 0;
    while let Some(hash_pos) = trivia[current..].find('#') {
        let hash_idx = current + hash_pos;
        push_plain_or_bracket(
            output,
            &trivia[current..hash_idx],
            base_idx + current,
            matched_brackets,
            use_color,
        );
        let end_idx = trivia[hash_idx..]
            .find('\n')
            .map(|i| hash_idx + i)
            .unwrap_or(trivia.len());
        let comment = &trivia[hash_idx..end_idx];
        output.push_str(&Style::DIM.paint(comment, use_color));
        current = end_idx;
    }
    if current < trivia.len() {
        push_plain_or_bracket(
            output,
            &trivia[current..],
            base_idx + current,
            matched_brackets,
            use_color,
        );
    }
}

fn push_plain_or_bracket(
    output: &mut String,
    text: &str,
    base_idx: usize,
    matched_brackets: &[usize],
    use_color: bool,
) {
    for (i, ch) in text.char_indices() {
        let abs_idx = base_idx + i;
        if use_color && matched_brackets.contains(&abs_idx) {
            output.push_str(&Style::BOLD_YELLOW.paint(ch, true));
        } else {
            output.push(ch);
        }
    }
}

fn highlight_token(kind: TokenKind, text: &str, use_color: bool) -> String {
    match kind {
        TokenKind::Fn
        | TokenKind::Op
        | TokenKind::Class
        | TokenKind::Extends
        | TokenKind::Let
        | TokenKind::Final
        | TokenKind::Const
        | TokenKind::If
        | TokenKind::Else
        | TokenKind::While
        | TokenKind::For
        | TokenKind::In
        | TokenKind::Return => Style::BOLD_MAGENTA.paint(text, use_color),

        TokenKind::SelfKw | TokenKind::Super => Style::BOLD_CYAN.paint(text, use_color),

        TokenKind::True | TokenKind::False => Style::YELLOW.paint(text, use_color),
        TokenKind::Nil => Style::DIM.paint(text, use_color),

        TokenKind::Int(_) | TokenKind::Float(_) => Style::CYAN.paint(text, use_color),

        TokenKind::Str(_) => Style::GREEN.paint(text, use_color),

        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::SlashSlash
        | TokenKind::Percent
        | TokenKind::Assign
        | TokenKind::Eq
        | TokenKind::Ne
        | TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Gt
        | TokenKind::Ge
        | TokenKind::Not
        | TokenKind::AndAnd
        | TokenKind::OrOr => Style::DIM.paint(text, use_color),

        _ => text.to_string(),
    }
}

fn count_open_braces(text: &str) -> usize {
    let mut depth: isize = 0;
    for ch in text.chars() {
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
        }
    }
    depth.max(0) as usize
}

/// Runs the interactive REPL with live syntax highlighting and line history.
pub fn run_repl(use_color_stdout: bool, use_color_stderr: bool) -> Result<()> {
    let pkg_name = Style::BOLD_CYAN.paint("quince", use_color_stdout);
    let version = Style::YELLOW.paint(env!("CARGO_PKG_VERSION"), use_color_stdout);
    let hint = Style::DIM.paint("ctrl-d to exit, :help for commands", use_color_stdout);
    println!("{pkg_name} {version} — {hint}");

    let config = rustyline::Config::builder().auto_add_history(true).build();
    let mut rl = rustyline::Editor::with_config(config)?;
    let globals_store = Arc::new(Mutex::new(Vec::new()));

    rl.set_helper(Some(QuinceHelper {
        use_color: use_color_stdout,
        globals: Arc::clone(&globals_store),
    }));

    let mut interp = Interp::new();
    let mut buffer = String::new();

    loop {
        // Sync global variables for tab autocompletion
        if let Ok(mut store) = globals_store.lock() {
            *store = interp.get_globals().into_iter().map(|(k, _)| k).collect();
        }

        let open_braces = count_open_braces(&buffer);
        let mut line = match if buffer.is_empty() {
            rl.readline("> ")
        } else {
            let initial = "    ".repeat(open_braces);
            rl.readline_with_initial(". ", (&initial, ""))
        } {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                buffer.clear();
                println!("^C");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
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
                let prompt_dot = Style::BOLD_YELLOW.paint(".", true);
                let highlighted = match rl.helper() {
                    Some(h) => h.highlight(&line, line.len()),
                    None => Cow::Borrowed(&line[..]),
                };
                print!("\x1B[1A\x1B[2K{prompt_dot} {highlighted}\n");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            } else {
                print!("\x1B[1A\x1B[2K. {line}\n");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        }

        // Handle REPL Meta-Commands
        let trimmed_line = line.trim();
        if buffer.is_empty() && trimmed_line.starts_with(':') {
            if handle_meta_command(
                trimmed_line,
                &mut interp,
                use_color_stdout,
                use_color_stderr,
            )? {
                continue;
            }
        }

        buffer.push_str(&line);
        buffer.push('\n');

        let program = match quince::compile(&buffer) {
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
            Ok(Some(value)) => println!("{}", value.display_pretty(&interp.heap, use_color_stdout)),
            Err(err) => eprintln!("{}", err.report_styled(&source, "<repl>", use_color_stderr)),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_offers_the_methods_that_exist_and_no_others() {
        // The list this replaced was written from memory: it offered `pop`,
        // `insert`, `clear`, `slice`, `contains`, and Rust's `to_uppercase`,
        // none of which are Quince methods, and omitted `chars`, `upper`, and
        // `lower`, which are. Deriving it makes that unrepresentable; this
        // fails if anyone hand-writes the list again.
        let names = method_names();

        for real in ["chars", "upper", "lower", "push", "keys", "remove", "join"] {
            assert!(names.contains(&real), "{real} should be offered");
        }
        for fake in [
            "pop",
            "insert",
            "clear",
            "slice",
            "contains",
            "to_uppercase",
            "len",
        ] {
            assert!(!names.contains(&fake), "{fake} is not a method");
        }
    }
}

fn handle_meta_command(
    input: &str,
    interp: &mut Interp,
    use_color_stdout: bool,
    use_color_stderr: bool,
) -> Result<bool> {
    let mut parts = input.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();

    match cmd {
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
            Ok(true)
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
                    let val_str = val.display_pretty(&interp.heap, use_color_stdout);
                    let type_str = Style::DIM.paint(
                        format!("({})", val.type_name(&interp.heap)),
                        use_color_stdout,
                    );
                    println!("{name_str} = {val_str} {type_str}");
                }
            }
            Ok(true)
        }
        ":type" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :type <expression>", use_color_stdout)
                );
                return Ok(true);
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
            Ok(true)
        }
        ":ast" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :ast <expression>", use_color_stdout)
                );
                return Ok(true);
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
            Ok(true)
        }
        ":tokens" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :tokens <expression>", use_color_stdout)
                );
                return Ok(true);
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
            Ok(true)
        }
        ":load" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :load <filename.q>", use_color_stdout)
                );
                return Ok(true);
            }
            let source = match std::fs::read_to_string(arg) {
                Ok(src) => src,
                Err(err) => {
                    eprintln!(
                        "{}",
                        Style::RED.paint(format!("could not read {arg}: {err}"), use_color_stderr)
                    );
                    return Ok(true);
                }
            };
            let program = match quince::compile(&source) {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("{}", err.report_styled(&source, arg, use_color_stderr));
                    return Ok(true);
                }
            };
            match interp.run_repl(&program) {
                Ok(_) => println!(
                    "{}",
                    Style::GREEN.paint(format!("Loaded {arg}"), use_color_stdout)
                ),
                Err(err) => eprintln!("{}", err.report_styled(&source, arg, use_color_stderr)),
            }
            Ok(true)
        }
        ":time" => {
            if arg.is_empty() {
                println!(
                    "{}",
                    Style::DIM.paint("Usage: :time <expression>", use_color_stdout)
                );
                return Ok(true);
            }
            let start = Instant::now();
            let program = match quince::compile(arg) {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr));
                    return Ok(true);
                }
            };
            match interp.run_repl(&program) {
                Ok(Some(val)) => {
                    let elapsed = start.elapsed();
                    let val_str = val.display_pretty(&interp.heap, use_color_stdout);
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
            Ok(true)
        }
        ":clear" => {
            print!("\x1B[2J\x1B[1;1H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            *interp = Interp::new();
            println!(
                "{}",
                Style::DIM.paint("REPL state cleared.", use_color_stdout)
            );
            Ok(true)
        }
        _ if input.starts_with(':') => {
            println!(
                "{}",
                Style::RED.paint(
                    format!("Unknown command `{input}`. Type :help for commands."),
                    use_color_stdout
                )
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

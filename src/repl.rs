use std::borrow::Cow;
use anyhow::Result;
use rustyline::completion::Completer;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::Helper;

use quince::color::Style;
use quince::interp::Interp;
use quince::lexer::Lexer;
use quince::token::TokenKind;
use quince::value::Value;

pub struct QuinceHelper {
    pub use_color: bool,
}

impl Helper for QuinceHelper {}

impl Completer for QuinceHelper {
    type Candidate = String;
}

impl Hinter for QuinceHelper {
    type Hint = String;
}

impl Validator for QuinceHelper {}

impl Highlighter for QuinceHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        if !self.use_color {
            return Cow::Borrowed(line);
        }

        let mut output = String::with_capacity(line.len() + 32);
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
                push_trivia(&mut output, &line[last_end..start], self.use_color);
            }

            let text = &line[start..end];
            let styled = highlight_token(token.kind, text, self.use_color);
            output.push_str(&styled);
            last_end = end;
        }

        if last_end < line.len() {
            push_trivia(&mut output, &line[last_end..], self.use_color);
        }

        Cow::Owned(output)
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
            Cow::Owned(format!("{} ", Style::BOLD_YELLOW.paint(".", true)))
        } else {
            Cow::Borrowed(prompt)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _forced: bool) -> bool {
        true
    }
}

fn push_trivia(output: &mut String, trivia: &str, use_color: bool) {
    let mut current = 0;
    while let Some(hash_pos) = trivia[current..].find('#') {
        let hash_idx = current + hash_pos;
        output.push_str(&trivia[current..hash_idx]);
        let end_idx = trivia[hash_idx..]
            .find('\n')
            .map(|i| hash_idx + i)
            .unwrap_or(trivia.len());
        let comment = &trivia[hash_idx..end_idx];
        output.push_str(&Style::DIM.paint(comment, use_color));
        current = end_idx;
    }
    if current < trivia.len() {
        output.push_str(&trivia[current..]);
    }
}

fn highlight_token(kind: TokenKind, text: &str, use_color: bool) -> String {
    match kind {
        TokenKind::Fn
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

/// Runs the interactive REPL with live syntax highlighting and line history.
pub fn run_repl(use_color_stdout: bool, use_color_stderr: bool) -> Result<()> {
    let pkg_name = Style::BOLD_CYAN.paint("quince", use_color_stdout);
    let version = Style::YELLOW.paint(env!("CARGO_PKG_VERSION"), use_color_stdout);
    let hint = Style::DIM.paint("ctrl-d to exit", use_color_stdout);
    println!("{pkg_name} {version} — {hint}");

    let config = rustyline::Config::builder()
        .auto_add_history(true)
        .build();
    let mut rl = rustyline::Editor::with_config(config)?;
    rl.set_helper(Some(QuinceHelper {
        use_color: use_color_stdout,
    }));

    let mut interp = Interp::new();
    let mut buffer = String::new();

    loop {
        let prompt = if buffer.is_empty() { "> " } else { ". " };

        let line = match rl.readline(prompt) {
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
            Ok(Some(value)) => println!("{}", value.display_styled(&interp.heap, use_color_stdout)),
            Err(err) => eprintln!("{}", err.report_styled(&source, "<repl>", use_color_stderr)),
        }
    }

    Ok(())
}

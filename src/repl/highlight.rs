//! Colouring the line as it is typed, and matching its brackets.


use std::borrow::Cow;
use rustyline::highlight::Highlighter;

use quince::color::Style;
use quince::syntax::lexer::Lexer;
use quince::syntax::token::TokenKind;
use crate::repl::helper::QuinceHelper;

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

        for (i, token) in tokens.iter().enumerate() {
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
            let prev_kind = if i > 0 {
                Some(&tokens[i - 1].kind)
            } else {
                None
            };
            let next_kind = if i + 1 < tokens.len() {
                Some(&tokens[i + 1].kind)
            } else {
                None
            };

            let styled = if text.len() == 1 && matched_brackets.contains(&start) {
                Style::BOLD_YELLOW.paint(text, true)
            } else {
                highlight_token(
                    token.kind.clone(),
                    text,
                    self.use_color,
                    prev_kind,
                    next_kind,
                )
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

        if let Some(rest) = prompt.strip_prefix(">>>") {
            Cow::Owned(format!("{}{rest}", Style::BOLD_GREEN.paint(">>>", true)))
        } else if let Some(rest) = prompt.strip_prefix("...") {
            Cow::Owned(format!("{}{rest}", Style::BOLD_YELLOW.paint("...", true)))
        } else if prompt.starts_with('>') {
            Cow::Owned(format!("{} ", Style::BOLD_GREEN.paint(">", true)))
        } else if let Some(rest) = prompt.strip_prefix('.') {
            Cow::Owned(format!("{}{rest}", Style::BOLD_YELLOW.paint(".", true)))
        } else {
            Cow::Borrowed(prompt)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _forced: rustyline::highlight::CmdKind) -> bool {
        true
    }
}

pub(crate) fn find_matching_brackets(line: &str, pos: usize) -> Vec<usize> {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return Vec::new();
    }

    let check_positions = [pos, pos.saturating_sub(1)];
    for &check_pos in &check_positions {
        if check_pos >= bytes.len() {
            continue;
        }

        let ch = bytes[check_pos] as char;
        let (matching_ch, search_forward) = match ch {
            '(' => (')', true),
            ')' => ('(', false),
            '[' => (']', true),
            ']' => ('[', false),
            '{' => ('}', true),
            '}' => ('{', false),
            _ => continue,
        };

        if search_forward {
            let mut depth = 0;
            for (i, &byte) in bytes.iter().enumerate().skip(check_pos) {
                let c = byte as char;
                if c == ch {
                    depth += 1;
                } else if c == matching_ch {
                    depth -= 1;
                    if depth == 0 {
                        return vec![check_pos, i];
                    }
                }
            }
        } else {
            let mut depth = 0;
            for i in (0..=check_pos).rev() {
                let c = bytes[i] as char;
                if c == ch {
                    depth += 1;
                } else if c == matching_ch {
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

pub(crate) fn push_trivia_with_brackets(
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

pub(crate) fn push_plain_or_bracket(
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

pub(crate) fn highlight_token(
    kind: TokenKind,
    text: &str,
    use_color: bool,
    prev_kind: Option<&TokenKind>,
    next_kind: Option<&TokenKind>,
) -> String {
    match kind {
        TokenKind::SelfKw | TokenKind::Super => Style::BOLD_CYAN.paint(text, use_color),

        TokenKind::True | TokenKind::False => Style::YELLOW.paint(text, use_color),
        TokenKind::Nil => Style::DIM.paint(text, use_color),

        TokenKind::Ident(_) => {
            if matches!(prev_kind, Some(TokenKind::Fn)) {
                Style::BOLD_CYAN.paint(text, use_color)
            } else if matches!(prev_kind, Some(TokenKind::Class)) {
                Style::BOLD_YELLOW.paint(text, use_color)
            } else if matches!(next_kind, Some(TokenKind::LParen)) {
                Style::BOLD_BLUE.paint(text, use_color)
            } else if [
                "print", "type", "string", "list", "dict", "int", "float", "bool", "len",
            ]
            .contains(&text)
            {
                Style::BOLD_CYAN.paint(text, use_color)
            } else {
                text.to_string()
            }
        }

        _ if TokenKind::keyword(text).is_some() => Style::BOLD_MAGENTA.paint(text, use_color),

        TokenKind::Int(_) | TokenKind::Float(_) => Style::CYAN.paint(text, use_color),

        TokenKind::Str(_) => Style::GREEN.paint(text, use_color),

        TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::SlashSlash
        | TokenKind::Percent
        | TokenKind::StarStar
        | TokenKind::Assign
        | TokenKind::AssignOp(_)
        // `and=` and `or=` are spelled with letters and so are caught by the
        // keyword arm above before ever reaching here. Listed anyway, because
        // which arm claims a token is not a thing to leave to arm order.
        | TokenKind::AssignShort(_)
        | TokenKind::PlusPlus
        | TokenKind::MinusMinus
        | TokenKind::Eq
        | TokenKind::Ne
        | TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Gt
        | TokenKind::Ge => Style::DIM.paint(text, use_color),

        _ => text.to_string(),
    }
}

pub(crate) fn count_open_braces(text: &str) -> usize {
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

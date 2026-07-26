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
use quince::token::{KEYWORDS, TokenKind};
use quince::value::Value;

const META_COMMANDS: &[&str] = &[
    ":help", ":vars", ":type", ":ast", ":tokens", ":clear", ":load", ":time",
];

/// Every method name any builtin type has, read off the type tables rather than
/// restated, so completion cannot offer a method the language does not have.
fn method_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = class::BUILTINS
        .iter()
        .flat_map(|builtin| builtin.seed().methods.iter().map(|(name, _)| *name))
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

use std::collections::HashMap;

fn method_names_for_type(
    type_name: &str,
    custom_methods: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    if let Some(methods) = custom_methods.get(type_name) {
        let mut names = methods.clone();
        names.sort();
        names.dedup();
        return names;
    }

    for builtin in class::BUILTINS {
        if builtin.name() == type_name {
            let mut names: Vec<String> = builtin
                .seed()
                .methods
                .iter()
                .map(|(name, _)| name.to_string())
                .collect();
            names.sort();
            names.dedup();
            return names;
        }
    }

    let mut names: Vec<String> = method_names().into_iter().map(String::from).collect();
    for methods in custom_methods.values() {
        names.extend(methods.clone());
    }
    names.sort();
    names.dedup();
    names
}

fn extract_var_before_dot(line: &str, dot_pos: usize) -> Option<&str> {
    if dot_pos == 0 {
        return None;
    }
    let prefix = &line[..dot_pos];
    let start = prefix
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(|i| i + 1)
        .unwrap_or(0);
    let var_name = prefix[start..].trim();
    if var_name.is_empty() {
        None
    } else {
        Some(var_name)
    }
}

fn get_dot_candidates(
    line: &str,
    dot_pos: usize,
    globals: &[(String, String)],
    custom_map: &HashMap<String, Vec<String>>,
    var_fields_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut set = Vec::new();
    if let Some(var_name) = extract_var_before_dot(line, dot_pos) {
        if let Some((_, type_name)) = globals.iter().find(|(k, _)| k == var_name) {
            set.extend(method_names_for_type(type_name, custom_map));
        } else if let Some(methods) = custom_map.get(var_name) {
            set.extend(methods.clone());
        } else {
            set.extend(method_names().into_iter().map(String::from));
            for methods in custom_map.values() {
                set.extend(methods.clone());
            }
        }

        if let Some(fields) = var_fields_map.get(var_name) {
            set.extend(fields.clone());
        } else {
            for fields in var_fields_map.values() {
                set.extend(fields.clone());
            }
        }
    } else {
        set.extend(method_names().into_iter().map(String::from));
        for methods in custom_map.values() {
            set.extend(methods.clone());
        }
    }
    set.sort();
    set.dedup();
    set
}

#[derive(Clone)]
pub struct QuinceHelper {
    pub use_color: bool,
    pub globals: Arc<Mutex<Vec<(String, String)>>>,
    pub custom_methods: Arc<Mutex<HashMap<String, Vec<String>>>>,
    pub var_fields: Arc<Mutex<HashMap<String, Vec<String>>>>,
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
            let custom_map = self
                .custom_methods
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default();
            let var_fields_map = self
                .var_fields
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default();
            let globals = self
                .globals
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            let methods =
                get_dot_candidates(line, start - 1, &globals, &custom_map, &var_fields_map);

            for method in methods {
                if method.starts_with(word) {
                    matches.push(Pair {
                        display: method.clone(),
                        replacement: method,
                    });
                }
            }
            return Ok((start, matches));
        }

        let mut string_candidates = Vec::new();
        if let Ok(globals) = self.globals.lock() {
            string_candidates = globals.iter().map(|(k, _)| k.clone()).collect();
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
            candidates.extend(META_COMMANDS.iter().copied().map(String::from));
        } else if start > 0 && line.as_bytes().get(start - 1) == Some(&b'.') {
            let custom_map = self
                .custom_methods
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default();
            let var_fields_map = self
                .var_fields
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default();
            let globals = self
                .globals
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            let methods =
                get_dot_candidates(line, start - 1, &globals, &custom_map, &var_fields_map);
            candidates.extend(methods);
        } else {
            candidates.extend(KEYWORDS.iter().copied().map(String::from));
            if let Ok(globals) = self.globals.lock() {
                for (g, _) in globals.iter() {
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

#[cfg(test)]
pub fn is_input_incomplete(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return false;
    }

    let tokens = match Lexer::new(input).tokenize() {
        Ok(tokens) => tokens,
        Err(_) => {
            return true;
        }
    };

    let mut parens = 0i32;
    let mut brackets = 0i32;
    let mut braces = 0i32;
    let mut last_kind = None;

    for token in &tokens {
        match token.kind {
            TokenKind::LParen => parens += 1,
            TokenKind::RParen => parens = (parens - 1).max(0),
            TokenKind::LBracket => brackets += 1,
            TokenKind::RBracket => brackets = (brackets - 1).max(0),
            TokenKind::LBrace => braces += 1,
            TokenKind::RBrace => braces = (braces - 1).max(0),
            TokenKind::Eof => {}
            ref kind => last_kind = Some(kind.clone()),
        }
    }

    if parens > 0 || brackets > 0 || braces > 0 {
        return true;
    }

    if let Some(kind) = last_kind {
        matches!(
            kind,
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
                | TokenKind::AndAnd
                | TokenKind::OrOr
                | TokenKind::Comma
                | TokenKind::Dot
        )
    } else {
        false
    }
}

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

        if prompt.starts_with(">>>") {
            let rest = &prompt[3..];
            Cow::Owned(format!("{}{rest}", Style::BOLD_GREEN.paint(">>>", true)))
        } else if prompt.starts_with("...") {
            let rest = &prompt[3..];
            Cow::Owned(format!("{}{rest}", Style::BOLD_YELLOW.paint("...", true)))
        } else if prompt.starts_with('>') {
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
            for i in check_pos..bytes.len() {
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

fn highlight_token(
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
    let globals_store: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let custom_methods_store: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let var_fields_store: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    rl.set_helper(Some(QuinceHelper {
        use_color: use_color_stdout,
        globals: Arc::clone(&globals_store),
        custom_methods: Arc::clone(&custom_methods_store),
        var_fields: Arc::clone(&var_fields_store),
    }));

    let mut interp = Interp::new();
    let mut buffer = String::new();

    loop {
        // Sync global variables, custom class methods, and instance fields for tab autocompletion
        if let Ok(mut store) = globals_store.lock() {
            let mut vars = Vec::new();
            let mut custom_map = HashMap::new();
            let mut fields_map: HashMap<String, Vec<String>> = HashMap::new();

            for (k, v) in interp.get_globals() {
                match &v {
                    Value::Class(id) => {
                        let class_obj = interp.heap.class(*id);
                        vars.push((k.clone(), class_obj.name.clone()));
                        let mut methods = Vec::new();
                        let mut current_id = Some(*id);
                        while let Some(cls_id) = current_id {
                            let cls = interp.heap.class(cls_id);
                            for m in cls.methods.keys() {
                                methods.push(m.clone());
                            }
                            current_id = cls.parent;
                        }
                        methods.sort();
                        methods.dedup();
                        custom_map.insert(class_obj.name.clone(), methods);
                    }
                    Value::Instance(id) => {
                        let type_name = v.type_name(&interp.heap).to_string();
                        vars.push((k.clone(), type_name.clone()));
                        let inst = interp.heap.instance(*id);
                        let fields: Vec<String> = inst
                            .fields
                            .iter()
                            .filter_map(|(key, _)| match key.to_value() {
                                Value::Str(s) => Some(s.to_string()),
                                _ => None,
                            })
                            .collect();
                        fields_map.insert(k.clone(), fields);

                        if !custom_map.contains_key(&type_name) {
                            let mut methods = Vec::new();
                            let mut current_id = Some(inst.class);
                            while let Some(cls_id) = current_id {
                                let cls = interp.heap.class(cls_id);
                                for m in cls.methods.keys() {
                                    methods.push(m.clone());
                                }
                                current_id = cls.parent;
                            }
                            methods.sort();
                            methods.dedup();
                            custom_map.insert(type_name, methods);
                        }
                    }
                    Value::Dict(id) => {
                        let type_name = v.type_name(&interp.heap).to_string();
                        vars.push((k.clone(), type_name));
                        let dict = interp.heap.dict(*id);
                        let fields: Vec<String> = dict
                            .iter()
                            .filter_map(|(key, _)| match key.to_value() {
                                Value::Str(s) => Some(s.to_string()),
                                _ => None,
                            })
                            .collect();
                        fields_map.insert(k.clone(), fields);
                    }
                    _ => {
                        let type_name = v.type_name(&interp.heap).to_string();
                        vars.push((k.clone(), type_name));
                    }
                }
            }
            *store = vars;
            if let Ok(mut methods_store) = custom_methods_store.lock() {
                *methods_store = custom_map;
            }
            if let Ok(mut fields_store) = var_fields_store.lock() {
                *fields_store = fields_map;
            }
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
                print!("\x1B[1A\x1B[2K{prompt_dot} {highlighted}\n");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            } else {
                print!("\x1B[1A\x1B[2K... {line}\n");
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
            Ok(Some(value)) => {
                println!("{}", value.display_pretty(&interp.heap, use_color_stdout));
                interp.set_global("_", value);
            }
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

    #[test]
    fn all_keywords_are_highlighted() {
        for kw in KEYWORDS {
            if let Some(kind) = TokenKind::keyword(kw) {
                let styled = highlight_token(kind.clone(), kw, true, None, None);

                if kw == &"self" || kw == &"super" {
                    assert!(styled.contains("\x1b[1;36m"), "{kw} should be bold cyan");
                } else if kw == &"true" || kw == &"false" {
                    assert!(styled.contains("\x1b[33m"), "{kw} should be yellow");
                } else if kw == &"nil" {
                    assert!(styled.contains("\x1b[2m"), "{kw} should be dim");
                } else {
                    assert!(styled.contains("\x1b[1;35m"), "{kw} should be bold magenta");
                }
            }
        }
    }

    #[test]
    fn context_aware_syntax_highlighting_differentiates_identifiers() {
        let fn_decl = highlight_token(
            TokenKind::Ident("calculate".to_string()),
            "calculate",
            true,
            Some(&TokenKind::Fn),
            None,
        );
        assert!(
            fn_decl.contains("\x1b[1;36m"),
            "fn name should be bold cyan"
        );

        let class_decl = highlight_token(
            TokenKind::Ident("Point".to_string()),
            "Point",
            true,
            Some(&TokenKind::Class),
            None,
        );
        assert!(
            class_decl.contains("\x1b[1;33m"),
            "class name should be bold yellow"
        );

        let call = highlight_token(
            TokenKind::Ident("foo".to_string()),
            "foo",
            true,
            None,
            Some(&TokenKind::LParen),
        );
        assert!(
            call.contains("\x1b[1;34m"),
            "function call should be bold blue"
        );

        let builtin = highlight_token(
            TokenKind::Ident("print".to_string()),
            "print",
            true,
            None,
            None,
        );
        assert!(
            builtin.contains("\x1b[1;36m"),
            "builtin function should be bold cyan"
        );
    }

    #[test]
    fn validator_detects_incomplete_expressions() {
        assert!(is_input_incomplete("1 +"));
        assert!(is_input_incomplete("fn foo() {"));
        assert!(is_input_incomplete("print([1, 2,"));
        assert!(is_input_incomplete("\"unterminated string"));
        assert!(!is_input_incomplete("1 + 2"));
        assert!(!is_input_incomplete("let x = 10"));
    }

    #[test]
    fn context_aware_method_completion_filters_by_type() {
        let custom_map = HashMap::from([(
            "Point".to_string(),
            vec!["distance".to_string(), "move".to_string()],
        )]);

        let string_methods = method_names_for_type("string", &HashMap::new());
        assert!(string_methods.contains(&"upper".to_string()));
        assert!(string_methods.contains(&"lower".to_string()));
        assert!(!string_methods.contains(&"push".to_string()));
        assert!(!string_methods.contains(&"keys".to_string()));

        let list_methods = method_names_for_type("list", &HashMap::new());
        assert!(list_methods.contains(&"push".to_string()));
        assert!(!list_methods.contains(&"upper".to_string()));

        let point_methods = method_names_for_type("Point", &custom_map);
        assert_eq!(
            point_methods,
            vec!["distance".to_string(), "move".to_string()]
        );
    }

    #[test]
    fn subclass_methods_and_variables_are_completed_and_hinted() {
        let mut interp = Interp::new();
        let code = r#"
            class Animal {
                fn speak() { return "..." }
            }
            class Dog extends Animal {
                op init() {
                    self.breed = "collie"
                }
                fn bark() { return "woof" }
            }
            let d = Dog()
        "#;
        let program = quince::compile(code).unwrap();
        interp.run_repl(&program).unwrap();

        let globals_store = Arc::new(Mutex::new(Vec::new()));
        let custom_methods_store = Arc::new(Mutex::new(HashMap::new()));
        let var_fields_store = Arc::new(Mutex::new(HashMap::new()));

        let mut vars = Vec::new();
        let mut custom_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut fields_map: HashMap<String, Vec<String>> = HashMap::new();

        for (k, v) in interp.get_globals() {
            match &v {
                Value::Class(id) => {
                    let class_obj = interp.heap.class(*id);
                    vars.push((k.clone(), class_obj.name.clone()));
                    let mut methods = Vec::new();
                    let mut current_id = Some(*id);
                    while let Some(cls_id) = current_id {
                        let cls = interp.heap.class(cls_id);
                        for m in cls.methods.keys() {
                            methods.push(m.clone());
                        }
                        current_id = cls.parent;
                    }
                    methods.sort();
                    methods.dedup();
                    custom_map.insert(class_obj.name.clone(), methods);
                }
                Value::Instance(id) => {
                    let type_name = v.type_name(&interp.heap).to_string();
                    vars.push((k.clone(), type_name.clone()));
                    let inst = interp.heap.instance(*id);
                    let fields: Vec<String> = inst
                        .fields
                        .iter()
                        .filter_map(|(key, _)| match key.to_value() {
                            Value::Str(s) => Some(s.to_string()),
                            _ => None,
                        })
                        .collect();
                    fields_map.insert(k.clone(), fields);

                    if !custom_map.contains_key(&type_name) {
                        let mut methods = Vec::new();
                        let mut current_id = Some(inst.class);
                        while let Some(cls_id) = current_id {
                            let cls = interp.heap.class(cls_id);
                            for m in cls.methods.keys() {
                                methods.push(m.clone());
                            }
                            current_id = cls.parent;
                        }
                        methods.sort();
                        methods.dedup();
                        custom_map.insert(type_name, methods);
                    }
                }
                _ => {
                    let type_name = v.type_name(&interp.heap).to_string();
                    vars.push((k.clone(), type_name));
                }
            }
        }
        *globals_store.lock().unwrap() = vars;
        *custom_methods_store.lock().unwrap() = custom_map;
        *var_fields_store.lock().unwrap() = fields_map;

        let helper = QuinceHelper {
            use_color: false,
            globals: globals_store,
            custom_methods: custom_methods_store,
            var_fields: var_fields_store,
        };

        let history = rustyline::history::MemHistory::new();
        let dummy_ctx = rustyline::Context::new(&history);

        // Test completion on instance variable `d.`
        let (start, matches) = helper.complete("d.", 2, &dummy_ctx).unwrap();
        assert_eq!(start, 2);
        let match_displays: Vec<String> = matches.into_iter().map(|p| p.display).collect();
        assert!(match_displays.contains(&"bark".to_string()), "should offer subclass method bark");
        assert!(match_displays.contains(&"speak".to_string()), "should offer superclass method speak");
        assert!(match_displays.contains(&"breed".to_string()), "should offer instance variable breed");

        // Test hinter on `d.b`
        let hint_b = helper.hint("d.b", 3, &dummy_ctx);
        assert_eq!(hint_b, Some("ark".to_string()));

        // Test hinter on `d.s`
        let hint_s = helper.hint("d.s", 3, &dummy_ctx);
        assert_eq!(hint_s, Some("peak".to_string()));

        // Test hinter on `d.br`
        let hint_br = helper.hint("d.br", 4, &dummy_ctx);
        assert_eq!(hint_br, Some("eed".to_string()));

        // Test completion directly on Class object `Dog.`
        let (_, dog_matches) = helper.complete("Dog.", 4, &dummy_ctx).unwrap();
        let dog_displays: Vec<String> = dog_matches.into_iter().map(|p| p.display).collect();
        assert!(dog_displays.contains(&"bark".to_string()));
        assert!(dog_displays.contains(&"speak".to_string()));
    }

    #[test]
    fn repl_binds_last_value_to_underscore() {
        let mut interp = Interp::new();
        let program = quince::compile("10 + 20").unwrap();
        if let Ok(Some(val)) = interp.run_repl(&program) {
            interp.set_global("_", val);
        }
        let check_pgm = quince::compile("_ * 2").unwrap();
        let res = interp.run_repl(&check_pgm).unwrap();
        assert_eq!(res, Some(Value::Int(60)));
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

use anyhow::Result;
use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::time::Instant;

use crate::cursor;
use quince::class;
use quince::color::Style;
use quince::infer::{Kind, Symbol, Type};
use quince::interp::Interp;
use quince::show::Ask;
use quince::lexer::Lexer;
use quince::token::{KEYWORDS, TokenKind};
use quince::value::Value;

const META_COMMANDS: &[&str] = &[
    ":help", ":vars", ":type", ":ast", ":tokens", ":clear", ":load", ":time",
];

/// What the interpreter knows right now, as symbols.
///
/// Rebuilt after every entry, from the live objects. That is the REPL's whole
/// advantage over the editor and it was being thrown away: a bound name has a
/// value, and a value has a class, so there is nothing here to infer. What the
/// receiver is, is a fact.
///
/// This replaces three hand-maintained maps — globals as `(String, String)`,
/// methods as `HashMap<String, Vec<String>>`, fields as another — which between
/// them could not say what a member returned, missed every `extend`ed method,
/// and fell back to offering every method of every type when the receiver was
/// not a plain global.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Every global, with the class of the value actually bound to it.
    globals: Vec<Symbol>,
    /// What a dot reaches on a value of each class.
    members: HashMap<String, Vec<Symbol>>,
}

impl Snapshot {
    /// Reads the interpreter's globals and the classes they reach.
    fn of(interp: &Interp) -> Snapshot {
        let mut snapshot = Snapshot::default();
        for (name, value) in interp.get_globals() {
            let class = value.type_name(&interp.heap).to_string();
            let mut symbol = Symbol::new(&name, kind_of(&value), Type::class(&class));
            // Calling a class makes one of it, which is what `Point(` needs to
            // know to offer the parameters of `Point`'s `init`.
            match &value {
                // Calling a class makes one of it, which is what `Point(`
                // needs in order to offer `Point`'s `init` parameters.
                Value::Class(id) => {
                    symbol.returns = Type::class(interp.heap.class(*id).name.clone())
                }
                // A module is not a class, so it is keyed apart from one — a
                // program may perfectly well declare `class math`.
                Value::Module(id) => {
                    symbol.ty =
                        Type::class(format!("module {}", interp.heap.globals(*id).name().unwrap_or_default()))
                }
                _ => {}
            }
            snapshot.globals.push(symbol);
            snapshot.learn(interp, &value);
        }
        // The builtin types, so `"abc".` and `[1, 2].` are answerable before a
        // program has bound anything of that class.
        for builtin in class::BUILTINS {
            let id = interp.heap.builtin_class(*builtin);
            snapshot.learn_class(interp, builtin.name(), id);
        }
        snapshot
    }

    /// Records what a dot reaches on `value`, and on the class it names.
    fn learn(&mut self, interp: &Interp, value: &Value) {
        let class = value.type_name(&interp.heap).to_string();
        match value {
            // A class object: its instances are what anyone asks about.
            Value::Class(id) => {
                let named = interp.heap.class(*id).name.clone();
                self.learn_class(interp, &named, *id);
            }
            // A module's names come out of the scope object it is, which is the
            // same object `import` produced — there is no second list.
            Value::Module(id) => {
                let named = format!("module {}", interp.heap.globals(*id).name().unwrap_or_default());
                if !self.members.contains_key(&named) {
                    let members = interp
                        .heap
                        .globals(*id)
                        .iter()
                        .map(|(member, held)| match held {
                            Value::Native(native) => {
                                let mut symbol =
                                    quince::infer::symbol_of_native(native, Kind::Function);
                                symbol.name = member.to_string();
                                symbol
                            }
                            _ => Symbol::new(
                                member,
                                Kind::Variable,
                                Type::class(held.type_name(&interp.heap)),
                            ),
                        })
                        .collect();
                    self.members.insert(named, members);
                }
            }
            Value::Instance(id) => {
                let instance = interp.heap.instance(*id);
                self.learn_class(interp, &class, instance.class);
                // Fields exist because something assigned them, so they are read
                // off the instance rather than guessed from the class body.
                let fields: Vec<Symbol> = instance
                    .fields
                    .iter()
                    .filter_map(|(key, held)| match key.to_value() {
                        Value::Str(name) => Some(Symbol::new(
                            name.to_string(),
                            Kind::Field,
                            Type::class(held.type_name(&interp.heap)),
                        )),
                        _ => None,
                    })
                    .collect();
                let known = self.members.entry(class).or_default();
                for field in fields {
                    if !known.iter().any(|seen| seen.name == field.name) {
                        known.push(field);
                    }
                }
            }
            _ => {
                let id = value.class(&interp.heap);
                self.learn_class(interp, &class, id);
            }
        }
    }

    /// Records the methods callable on a value of the class `id`.
    ///
    /// Through `Interp::methods_of`, which makes the same two walks dispatch
    /// makes — so an `extend` block's methods are offered, which they never
    /// were before.
    fn learn_class(&mut self, interp: &Interp, name: &str, id: quince::heap::ObjId) {
        if self.members.contains_key(name) {
            return;
        }
        let members = interp
            .methods_of(id)
            .into_iter()
            .map(|(method, value)| match &value {
                Value::Native(native) => {
                    let mut symbol = quince::infer::symbol_of_native(native, Kind::Method);
                    symbol.name = method;
                    symbol
                }
                Value::Function(handle) => {
                    let decl = &interp.heap.function(*handle).decl;
                    let mut symbol = Symbol::new(&method, Kind::Method, Type::class("function"));
                    symbol.doc = decl.doc.clone();
                    symbol.params = decl
                        .params
                        .iter()
                        .filter(|param| !param.receiver)
                        .map(|param| param.name.clone())
                        .collect();
                    symbol
                }
                _ => Symbol::new(&method, Kind::Method, Type::class("function")),
            })
            .collect();
        self.members.insert(name.to_string(), members);
    }

    /// What the text before a dot evaluates to.
    ///
    /// A dotted path resolved a segment at a time against what is bound, and
    /// failing that a literal read by the lexer. The same two questions the
    /// editor asks, answered from values instead of from a tree.
    fn type_of(&self, before: &str) -> Type {
        let Some(path) = cursor::path_ending_at(before) else {
            return cursor::trailing_literal_type(before);
        };
        let mut segments = path.split('.');
        let Some(first) = segments.next() else {
            return Type::Unknown;
        };
        let call = first.strip_suffix("()");
        let mut ty = match self
            .globals
            .iter()
            .find(|symbol| symbol.name == call.unwrap_or(first))
        {
            // Calling a name makes what it returns; naming it holds it. A class
            // named and not called is the exception: `Dog.bark` reaches the
            // method, so a dot on the class object finds what its instances
            // have — which the language allows and this has to follow.
            Some(symbol) if call.is_some() || symbol.kind == Kind::Class => {
                symbol.returns.clone()
            }
            Some(symbol) => symbol.ty.clone(),
            None => return cursor::trailing_literal_type(before),
        };
        for segment in segments {
            let Some(class) = ty.class_name() else {
                return Type::Unknown;
            };
            let call = segment.strip_suffix("()");
            let name = call.unwrap_or(segment);
            let found = self
                .members
                .get(class)
                .and_then(|members| members.iter().find(|symbol| symbol.name == name));
            ty = match (found, call) {
                (Some(symbol), Some(_)) => symbol.returns.clone(),
                (Some(symbol), None) => symbol.ty.clone(),
                (None, _) => return Type::Unknown,
            };
        }
        ty
    }

    /// Everything a dot after `before` reaches.
    ///
    /// A class object gets methods and no fields. `Dog.bark` finds the method
    /// and `Dog.breed` finds nothing — a field exists because an instance
    /// assigned it, and the class never did.
    fn members_after(&self, before: &str) -> Vec<Symbol> {
        let on_class_object = cursor::path_ending_at(before)
            .filter(|path| !path.contains('.') && !path.ends_with("()"))
            .and_then(|name| self.globals.iter().find(|symbol| symbol.name == name))
            .is_some_and(|symbol| symbol.kind == Kind::Class);

        match self.type_of(before).class_name() {
            Some(class) => self
                .members
                .get(class)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|symbol| !(on_class_object && symbol.kind == Kind::Field))
                .collect(),
            None => Vec::new(),
        }
    }
}

/// What a value is, for a completion list that has to draw it.
fn kind_of(value: &Value) -> Kind {
    match value {
        Value::Class(_) => Kind::Class,
        Value::Function(_) | Value::Native(_) | Value::BoundMethod(_) => Kind::Function,
        Value::Module(_) => Kind::Module,
        _ => Kind::Variable,
    }
}

/// The text before the dot the cursor sits after, if it does.
fn before_dot(line: &str, start: usize) -> Option<&str> {
    if start == 0 || line.as_bytes().get(start - 1) != Some(&b'.') {
        return None;
    }
    Some(line[..start - 1].trim_end())
}

#[derive(Clone)]
pub struct QuinceHelper {
    pub use_color: bool,
    /// What the interpreter knew after the last entry.
    pub snapshot: Arc<Mutex<Snapshot>>,
}

impl QuinceHelper {
    /// Everything a dot after `before` reaches, from the last snapshot.
    fn members(&self, before: &str) -> Vec<Symbol> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.members_after(before))
            .unwrap_or_default()
    }

    /// Every name bound so far.
    fn in_scope(&self) -> Vec<Symbol> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.globals.clone())
            .unwrap_or_default()
    }
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

        // After a dot, what the receiver *is* decides the list, and the
        // receiver is a value the interpreter is holding. Where it cannot be
        // identified nothing is offered — this used to answer with every method
        // of every type, which is a list of forty names of which two apply.
        if let Some(before) = before_dot(line, start) {
            for member in self.members(before) {
                if member.name.starts_with(word) {
                    matches.push(Pair {
                        display: member.signature(),
                        replacement: member.name,
                    });
                }
            }
            matches.sort_by(|a, b| a.replacement.cmp(&b.replacement));
            return Ok((start, matches));
        }

        let string_candidates: Vec<String> = self.in_scope().into_iter().map(|s| s.name).collect();

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
        } else if let Some(before) = before_dot(line, start) {
            candidates.extend(self.members(before).into_iter().map(|member| member.name));
        } else {
            candidates.extend(KEYWORDS.iter().copied().map(String::from));
            for symbol in self.in_scope() {
                if symbol.name.starts_with(word) && symbol.name != word {
                    return Some(symbol.name[word.len()..].to_string());
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

        // Handle REPL Meta-Commands
        let trimmed_line = line.trim();
        if buffer.is_empty() && trimmed_line.starts_with(':')
            && handle_meta_command(
                trimmed_line,
                &mut interp,
                use_color_stdout,
                use_color_stderr,
            )?
        {
            continue;
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
                    // Printed the way the prompt would print it, `op string` and
                    // all: this echoes a result, so showing it differently from
                    // the same expression typed bare would be a difference with
                    // no reason behind it. Measured before rendering, so what the
                    // timing reports is the expression and not its printing.
                    let val_str = match interp.display_pretty(&val, use_color_stdout, Ask::Class) {
                        Ok(text) => text,
                        Err(err) => {
                            eprintln!("{}", err.report_styled(arg, "<repl>", use_color_stderr));
                            return Ok(true);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A REPL that has already run `code`, and the snapshot it would offer from.
    fn after(code: &str) -> Snapshot {
        let mut interp = Interp::new();
        let program = quince::compile(code).expect("the test program compiles");
        interp.run_repl(&program).expect("it runs");
        Snapshot::of(&interp)
    }

    /// What a dot after `before` would offer, by name.
    fn offered(snapshot: &Snapshot, before: &str) -> Vec<String> {
        let mut names: Vec<String> = snapshot
            .members_after(before)
            .into_iter()
            .map(|symbol| symbol.name)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn completion_offers_the_methods_that_exist_and_no_others() {
        // The list this replaced was written from memory: it offered `pop`,
        // `insert`, `clear`, `slice`, `contains`, and Rust's `to_uppercase`,
        // none of which are Quince methods, and omitted `chars`, `upper`, and
        // `lower`, which are. It now comes off the live class objects, so a
        // method the language does not have cannot be offered at all.
        let names = offered(&after("let s = \"a\""), "s");

        for real in ["chars", "upper", "lower", "join", "split"] {
            assert!(names.contains(&real.to_string()), "{real} should be offered");
        }
        for fake in ["pop", "insert", "clear", "to_uppercase", "len", "push"] {
            assert!(!names.contains(&fake.to_string()), "{fake} is not a string method");
        }
    }

    #[test]
    fn completion_answers_from_the_value_that_is_actually_bound() {
        // The REPL's whole advantage over the editor: `words` is a list because
        // the value under that name is one, which is not a guess and cannot be
        // wrong.
        let snapshot = after("let words = \"a,b\".split(\",\")");
        let names = offered(&snapshot, "words");
        assert!(names.contains(&"push".to_string()), "{names:?}");
        assert!(!names.contains(&"lower".to_string()), "{names:?}");
    }

    #[test]
    fn completion_follows_a_chain_through_what_a_method_returns() {
        let snapshot = after("let words = [\"b\", \"a\"]");
        let names = offered(&snapshot, "words.sort()");
        assert!(names.contains(&"map".to_string()), "{names:?}");
    }

    #[test]
    fn completion_types_a_literal_without_anything_being_bound() {
        let snapshot = after("let unused = 1");
        assert!(offered(&snapshot, "\"abc\"").contains(&"upper".to_string()));
        assert!(offered(&snapshot, "[1, 2]").contains(&"push".to_string()));
    }

    #[test]
    fn completion_offers_an_extensions_methods() {
        // `extend` puts its methods beside a class rather than in it, so they
        // were callable and never offered — the old map read `Class::methods`
        // and stopped there.
        let snapshot = after("extend list {\n  fn second() { return self[1] }\n}\nlet xs = [1, 2]");
        let names = offered(&snapshot, "xs");
        assert!(names.contains(&"second".to_string()), "{names:?}");
        assert!(names.contains(&"push".to_string()), "{names:?}");
    }

    #[test]
    fn completion_offers_nothing_for_a_receiver_it_cannot_identify() {
        // This used to answer with every method of every type — forty names of
        // which two applied, which is a list nobody can read.
        let snapshot = after("let unused = 1");
        assert!(offered(&snapshot, "mystery").is_empty());
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
    fn completion_offers_a_modules_members() {
        let snapshot = after("import math");
        let names = offered(&snapshot, "math");
        assert!(names.contains(&"floor".to_string()), "{names:?}");
        assert!(names.contains(&"pi".to_string()), "{names:?}");
        // And only that module's. `read` belongs to `io`.
        assert!(!names.contains(&"read".to_string()), "{names:?}");
    }

    #[test]
    fn a_class_object_offers_methods_and_no_fields() {
        // `Dog.bark` reaches the method — `print(Dog.bark)` writes `<fn bark>`
        // — so a dot on the class lists them. `Dog.breed` reaches nothing: a
        // field exists because an instance assigned it.
        let snapshot = after(
            "class Dog {\n  op init() { self.breed = \"collie\" }\n  fn bark() { return \"woof\" }\n}\nlet d = Dog()",
        );
        let on_class = offered(&snapshot, "Dog");
        assert!(on_class.contains(&"bark".to_string()), "{on_class:?}");
        assert!(!on_class.contains(&"breed".to_string()), "{on_class:?}");

        let on_instance = offered(&snapshot, "d");
        assert!(on_instance.contains(&"bark".to_string()), "{on_instance:?}");
        assert!(on_instance.contains(&"breed".to_string()), "{on_instance:?}");
    }

    #[test]
    fn context_aware_method_completion_filters_by_type() {
        let snapshot = after("let s = \"a\"\nlet xs = [1]\nlet d = {\"k\": 1}");
        let strings = offered(&snapshot, "s");
        assert!(strings.contains(&"upper".to_string()));
        assert!(!strings.contains(&"push".to_string()));
        assert!(!strings.contains(&"keys".to_string()));

        let lists = offered(&snapshot, "xs");
        assert!(lists.contains(&"push".to_string()));
        assert!(!lists.contains(&"upper".to_string()));

        let dicts = offered(&snapshot, "d");
        assert!(dicts.contains(&"keys".to_string()));
        assert!(!dicts.contains(&"push".to_string()));
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
        let program = quince::compile(code).expect("the test program compiles");
        interp.run_repl(&program).expect("it runs");

        let helper = QuinceHelper {
            use_color: false,
            snapshot: Arc::new(Mutex::new(Snapshot::of(&interp))),
        };
        let history = rustyline::history::MemHistory::new();
        let context = rustyline::Context::new(&history);

        // An instance offers its own methods, its parent's, and the fields the
        // `init` that ran actually assigned.
        let (start, matches) = helper.complete("d.", 2, &context).expect("completion works");
        assert_eq!(start, 2);
        let offered: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
        for expected in ["bark", "speak", "breed"] {
            assert!(offered.contains(&expected.to_string()), "{offered:?}");
        }

        assert_eq!(helper.hint("d.b", 3, &context), Some("ark".to_string()));
        assert_eq!(helper.hint("d.s", 3, &context), Some("peak".to_string()));
        assert_eq!(helper.hint("d.br", 4, &context), Some("eed".to_string()));

        // And a class object answers about the instances it makes.
        let (_, matches) = helper.complete("Dog.", 4, &context).expect("completion works");
        let offered: Vec<String> = matches.into_iter().map(|pair| pair.replacement).collect();
        assert!(offered.contains(&"bark".to_string()), "{offered:?}");
        assert!(offered.contains(&"speak".to_string()), "{offered:?}");
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

    #[test]
    fn a_repl_entry_may_redefine_what_an_earlier_one_declared() {
        // Declaring a name twice in one program is refused. This is why that
        // refusal is per-compile and not per-process: at a prompt, writing the
        // function again *is* how you fix it, and a REPL that made you restart
        // over a typo would be the worse tool.
        let mut interp = Interp::new();
        for source in ["fn a() { return 1 }", "fn a() { return 2 }"] {
            let program = quince::compile(source).expect("each entry compiles on its own");
            interp.run_repl(&program).expect("and runs");
        }
        let call = quince::compile("a()").unwrap();
        assert_eq!(interp.run_repl(&call).unwrap(), Some(Value::Int(2)));
    }
}

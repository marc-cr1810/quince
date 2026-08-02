//! Completion, hinting, and deciding whether a line is finished.
//!
//! The `rustyline` traits, in one place. Everything they need to *decide* comes
//! from [`super::snapshot`]; this file is the wiring.


use std::sync::{Arc, Mutex};
use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use crate::repl::snapshot::{Snapshot, before_dot, import_candidates};

use quince::sema::symbols::{Kind, Symbol};
use quince::syntax::token::KEYWORDS;
#[cfg(test)]
use quince::syntax::token::TokenKind;
use crate::cursor;
use crate::repl::META_COMMANDS;

#[derive(Clone)]
pub struct QuinceHelper {
    pub use_color: bool,
    /// What the interpreter knew after the last entry.
    pub snapshot: Arc<Mutex<Snapshot>>,
}

impl QuinceHelper {
    /// Everything a dot after `before` reaches, from the last snapshot.
    pub(crate) fn members(&self, before: &str) -> Vec<Symbol> {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.members_after(before))
            .unwrap_or_default()
    }

    /// Every type that can be named in an annotation.
    ///
    /// The builtins and whatever classes the session has declared — read off
    /// the live globals rather than inferred, which is the REPL's advantage:
    /// a name either holds a class right now or it does not.
    pub(crate) fn type_names(&self) -> Vec<String> {
        let mut names: Vec<String> = quince::runtime::class::BUILTINS
            .iter()
            .map(|builtin| builtin.name().to_string())
            .collect();
        names.push("any".to_string());
        names.extend(
            self.in_scope()
                .into_iter()
                .filter(|symbol| symbol.kind == Kind::Class)
                .map(|symbol| symbol.name),
        );
        names.sort();
        names.dedup();
        names
    }

    /// Every name bound so far.
    pub(crate) fn in_scope(&self) -> Vec<Symbol> {
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

        // An `import` line wants modules or a module's members, and which one
        // depends on how far along it is.
        if let Some(site) = cursor::import_site(&line[..start.min(line.len())]) {
            for candidate in import_candidates(&site) {
                if candidate.starts_with(word) {
                    matches.push(Pair {
                        display: candidate.clone(),
                        replacement: candidate,
                    });
                }
            }
            return Ok((start, matches));
        }

        // In type position the only things that can follow are types, so the
        // names in scope are exactly the wrong list. The same rule the editor
        // uses, from the same function — a REPL that completed differently from
        // the editor would be two languages.
        if cursor::in_type_position(line, start.min(line.len())) {
            for candidate in self.type_names() {
                if candidate.starts_with(word) {
                    matches.push(Pair {
                        display: candidate.clone(),
                        replacement: candidate,
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

pub(crate) fn extract_word(line: &str, pos: usize) -> (usize, &str) {
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
        } else if let Some(site) = cursor::import_site(&line[..start.min(line.len())]) {
            candidates.extend(import_candidates(&site));
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
use quince::syntax::lexer::Lexer;

#[cfg(test)]
pub(crate) fn is_input_incomplete(input: &str) -> bool {
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

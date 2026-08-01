//! Reading the text around a cursor.
//!
//! Shared by the two surfaces the language is used through. Both have to answer
//! the same question before they can answer any other — *what did the person
//! just write a dot after* — and both used to answer it for themselves, with
//! the results that produced: the editor decided a receiver's class by whether
//! its name started with a capital letter, and the REPL, failing to recognise
//! one at all, offered every method of every type.
//!
//! Nothing here decides what anything *means*. It reads structure — where a
//! name begins, whether parentheses balance, which token the text ends on — and
//! hands the answer to the inference pass or to the live interpreter, whichever
//! the caller has. That line is the point: this is the last place either
//! surface touches raw text, and it is deliberately incapable of guessing.

use quince::infer::Type;
use quince::lexer::Lexer;
use quince::token::TokenKind;

/// The dotted path `text` ends with, with every argument list normalised to `()`.
///
/// `o.inner`, `math`, `b.twin()` — read backwards, one segment at a time. The
/// arguments are dropped because they change nothing about what a call
/// produces, and the parentheses are kept because they change everything:
/// `Point` is a class and `Point()` is a `Point`.
///
/// `None` for anything that does not end at a name — `"abc"`, `xs[0]`. This is
/// not a parser, and an approximation here is the guess the whole tranche was
/// written to remove.
pub fn path_ending_at(text: &str) -> Option<String> {
    let before: Vec<char> = text.trim_end().chars().collect();
    let is_name_char = |c: char| c == '_' || c.is_alphanumeric();
    let mut segments: Vec<String> = Vec::new();
    let mut cursor = before.len();

    loop {
        // An argument list, skipped over as a whole so that a call on something
        // nested — `f(g(x))` — still finds the name in front of it.
        let call = cursor > 0 && before[cursor - 1] == ')';
        if call {
            let mut depth = 0;
            loop {
                if cursor == 0 {
                    return None;
                }
                cursor -= 1;
                match before[cursor] {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }

        let end = cursor;
        while cursor > 0 && is_name_char(before[cursor - 1]) {
            cursor -= 1;
        }
        if cursor == end {
            return None;
        }
        let mut segment: String = before[cursor..end].iter().collect();
        if call {
            segment.push_str("()");
        }
        segments.push(segment);

        if cursor > 0 && before[cursor - 1] == '.' {
            cursor -= 1;
        } else {
            break;
        }
    }

    segments.reverse();
    Some(segments.join("."))
}

/// The type of a literal written immediately before a dot.
///
/// `"abc".`, `[1, 2].`, `5.`. Decided by lexing the text and looking at the
/// tokens, not by checking whether it ends in a quote — the old heuristic did
/// the latter and so read `xs[0]` as a list, because both end in a bracket.
///
/// A closing bracket is a collection literal only when what precedes its
/// opening one cannot be indexed. `[1, 2]` is a list; `xs[0]` is an item out of
/// one, and the token in front of the `[` is what tells them apart.
pub fn trailing_literal_type(text: &str) -> Type {
    let Ok(tokens) = Lexer::new(text).tokenize() else {
        return Type::Unknown;
    };
    let kinds: Vec<&TokenKind> = tokens
        .iter()
        .map(|token| &token.kind)
        .filter(|kind| **kind != TokenKind::Eof)
        .collect();
    let Some(last) = kinds.last() else {
        return Type::Unknown;
    };

    let closing = match last {
        TokenKind::Str(_) => return Type::class("string"),
        TokenKind::Int(_) => return Type::class("int"),
        TokenKind::Float(_) => return Type::class("float"),
        TokenKind::True | TokenKind::False => return Type::class("bool"),
        TokenKind::Nil => return Type::class("nil"),
        TokenKind::RBracket => (TokenKind::LBracket, TokenKind::RBracket, "list"),
        TokenKind::RBrace => (TokenKind::LBrace, TokenKind::RBrace, "dict"),
        _ => return Type::Unknown,
    };

    let (open, close, class) = closing;
    let mut depth = 0;
    for index in (0..kinds.len()).rev() {
        if *kinds[index] == close {
            depth += 1;
        } else if *kinds[index] == open {
            depth -= 1;
            if depth == 0 {
                // Indexing, if a value sits in front of the bracket.
                let indexable = matches!(
                    index.checked_sub(1).map(|before| kinds[before]),
                    Some(TokenKind::Ident(_))
                        | Some(TokenKind::RParen)
                        | Some(TokenKind::RBracket)
                        | Some(TokenKind::Str(_))
                );
                return if indexable {
                    Type::Unknown
                } else {
                    Type::class(class)
                };
            }
        }
    }
    Type::Unknown
}

/// What may follow the cursor on an `import` line.
pub enum ImportSite {
    /// A module name — after `import`, and after `from`.
    Module,
    /// A name the named module declares, after `from … import`.
    Member(String),
}

/// Where on an `import` line the cursor is, if it is on one.
///
/// The two positions want different lists and used to get the same one: `from
/// math import ` offered `math, io, time, random`, which are the four things
/// that cannot go there. The comment above it even said the members were what
/// a module's completion listed anyway, which was not true and had never been.
///
/// `text` is the line up to the cursor. Whether it ends in whitespace is what
/// separates a word being typed from one about to be — `from ma` is still
/// naming a module and `from math ` is waiting for the word `import`.
pub fn import_site(text: &str) -> Option<ImportSite> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let editing = !text.ends_with(char::is_whitespace);

    match words.as_slice() {
        // `import ` and `import ma`.
        ["import"] if !editing => Some(ImportSite::Module),
        ["import", _] if editing => Some(ImportSite::Module),
        // `from ` and `from ma`.
        ["from"] if !editing => Some(ImportSite::Module),
        ["from", _] if editing => Some(ImportSite::Module),
        // `from math import `, and every name after it. Not while `import`
        // itself is still being typed, which is what the length check is for.
        ["from", module, "import", ..] if !(words.len() == 3 && editing) => {
            Some(ImportSite::Member((*module).to_string()))
        }
        _ => None,
    }
}

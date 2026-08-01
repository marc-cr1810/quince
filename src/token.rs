use std::fmt;

/// A byte range into the source that produced a token or AST node.
///
/// Line and column are derived from this on demand rather than stored, so the
/// offsets stay the single source of truth.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span {
            start: start as u32,
            end: end as u32,
        }
    }

    /// The span covering both `self` and `other`, for parent AST nodes.
    pub fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Int(i64),
    Float(f64),
    Str(String),
    Ident(String),

    Fn,
    /// Introduces a method the language calls on the program's behalf, rather
    /// than one the program calls by name. See [`crate::ast::Op`].
    Op,
    Class,
    Extends,
    /// Adds methods to a type that already exists. Deliberately not [`Self::Extends`]:
    /// that word already means inheritance in a class header, and a block which
    /// declares no new type would be a pun on it.
    Extend,
    /// The three class modifiers, one per state a declaration can be in. See
    /// [`crate::ast::Openness`] for which door each one closes — [`Self::Final`]
    /// is the fourth, and is shared with the binding forms.
    Complete,
    Sealed,
    /// Keywords rather than identifiers so that using one where it has no
    /// meaning is caught by the resolver, with a message that says why.
    SelfKw,
    Super,
    Let,
    Final,
    Const,
    /// `import` is reserved; the `from` that may precede it is not. See
    /// [`KEYWORDS`].
    Import,
    If,
    Else,
    While,
    For,
    In,
    Return,
    Try,
    Catch,
    Throw,
    True,
    False,
    Nil,

    Plus,
    Minus,
    Star,
    Slash,
    SlashSlash,
    Percent,

    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,

    Not,
    AndAnd,
    OrOr,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Colon,
    Semi,

    Eof,
}

/// Every reserved word, for the completer and the highlighter to read.
///
/// The one to keep in step with [`TokenKind::keyword`] below: a word mapped
/// there and missing here is a keyword the completer never offers and the
/// highlighter's own test never checks, which is what `extend` was until the
/// class modifiers arrived and the omission was noticed beside them.
///
/// `from` is deliberately absent. `from math import floor` needs the word and
/// reserving it would cost `op init(from, to)`, which is how anyone writes a
/// range and which the corpus already had. It is recognised by the parser at the
/// one position where it can mean anything — the start of a statement, with an
/// `import` two tokens later — and is an ordinary identifier everywhere else.
pub const KEYWORDS: &[&str] = &[
    "fn", "op", "class", "extends", "extend", "self", "super", "let", "final", "complete",
    "sealed", "const", "import", "if", "else", "while", "for", "in", "return", "try", "catch",
    "throw", "true", "false", "nil",
];

impl TokenKind {
    /// Maps an identifier to its keyword, if it is one.
    pub fn keyword(word: &str) -> Option<TokenKind> {
        let kind = match word {
            "fn" => TokenKind::Fn,
            "op" => TokenKind::Op,
            "class" => TokenKind::Class,
            "extends" => TokenKind::Extends,
            "extend" => TokenKind::Extend,
            "self" => TokenKind::SelfKw,
            "super" => TokenKind::Super,
            "let" => TokenKind::Let,
            "final" => TokenKind::Final,
            "complete" => TokenKind::Complete,
            "sealed" => TokenKind::Sealed,
            "const" => TokenKind::Const,
            "import" => TokenKind::Import,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "throw" => TokenKind::Throw,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            _ => return None,
        };
        Some(kind)
    }

    /// What the keyword means, for an editor asked to explain one.
    ///
    /// Here rather than in `lsp.rs`, which is where it used to live as ten
    /// entries covering a quarter of the list. A keyword is the language's, so
    /// its description is too — and the exhaustive match means a word cannot be
    /// reserved without being explained, which the ten-entry table had no way
    /// to notice it was failing to do. `every_keyword_explains_itself` walks
    /// [`KEYWORDS`] to hold the two together.
    ///
    /// `None` for everything that is not a keyword: an operator explains itself
    /// and a literal is its own answer.
    pub fn doc(&self) -> Option<&'static str> {
        let text = match self {
            TokenKind::Fn => "Declares a function. It captures the scope it was written in, so a function returned from another keeps what that one bound.",
            TokenKind::Op => "Declares a method the language calls on the program's behalf — `op add` answers `+`, `op string` answers `print`. Being one is stated rather than inferred from the name, so a misspelling is an error instead of a method nothing calls.",
            TokenKind::Class => "Declares a type. Its methods take the receiver as `self`, and it may say what it is open to with `final`, `complete`, or `sealed`.",
            TokenKind::Extends => "Names the class this one descends from. The parent is an ordinary value looked up in the enclosing scope, not a static label.",
            TokenKind::Extend => "Adds methods to a type that already exists, including a builtin. It may not add an `op`: an extension grows a type's vocabulary, it does not change how the language dispatches on it.",
            TokenKind::SelfKw => "The receiver, inside a method. Bound as an ordinary first parameter the parser inserts, so a closure nested in a method captures it like any other name.",
            TokenKind::Super => "The parent class, inside a method of a class that extends one. Looks a method up starting at the parent while binding it to the current receiver.",
            TokenKind::Let => "Binds a name that may be reassigned.",
            TokenKind::Final => "Binds a name once. The object it names is untouched, so a `final` list still grows. Before `class`, it means no other class may extend this one.",
            TokenKind::Complete => "Before `class`: the method table is finished, so no `extend` block may add to it. Subclasses are still welcome, since a subclass adds nothing to the class it descends from.",
            TokenKind::Sealed => "Before `class`: `final` and `complete` at once — neither a subclass nor an `extend` block.",
            TokenKind::Const => "Binds a name once and freezes the value, deeply, and through every other name that already reaches it.",
            TokenKind::Import => "Loads a module — one the language ships, or a file beside this one. `import math` binds the module; `from math import floor` binds the names.",
            TokenKind::If => "Runs a block when a condition is truthy. A class decides its own truthiness with `op bool`.",
            TokenKind::Else => "The block to run when the `if` before it did not.",
            TokenKind::While => "Repeats a block for as long as a condition is truthy.",
            TokenKind::For => "Walks a string, a list, a dict's keys, or a class that declares `op iter`. The loop variable is fresh each time round.",
            TokenKind::In => "Between a `for` loop's variable and what it walks; on its own, whether a collection holds a value.",
            TokenKind::Return => "Hands a value back from a function. Written with nothing after it, it hands back `nil`.",
            TokenKind::Try => "Runs a block, handing control to its `catch` if anything in it raises.",
            TokenKind::Catch => "Binds the raised error and handles it. The `try` block's own bindings are not visible here, because a `let` inside it may never have run.",
            TokenKind::Throw => "Raises an error, which must be an instance of `Error`. Checked at the `throw` so the report names the mistake rather than surfacing later.",
            TokenKind::True => "The true boolean.",
            TokenKind::False => "The false boolean.",
            TokenKind::Nil => "The absence of a value.",

            TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::Ident(_)
            | TokenKind::Plus
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
            | TokenKind::OrOr
            | TokenKind::LParen
            | TokenKind::RParen
            | TokenKind::LBrace
            | TokenKind::RBrace
            | TokenKind::LBracket
            | TokenKind::RBracket
            | TokenKind::Comma
            | TokenKind::Dot
            | TokenKind::Colon
            | TokenKind::Semi
            | TokenKind::Eof => return None,
        };
        Some(text)
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Int(n) => write!(f, "{n}"),
            TokenKind::Float(n) => write!(f, "{n}"),
            TokenKind::Str(s) => write!(f, "\"{s}\""),
            TokenKind::Ident(name) => write!(f, "{name}"),

            TokenKind::Fn => write!(f, "fn"),
            TokenKind::Op => write!(f, "op"),
            TokenKind::Class => write!(f, "class"),
            TokenKind::Extends => write!(f, "extends"),
            TokenKind::Extend => write!(f, "extend"),
            TokenKind::SelfKw => write!(f, "self"),
            TokenKind::Super => write!(f, "super"),
            TokenKind::Let => write!(f, "let"),
            TokenKind::Final => write!(f, "final"),
            TokenKind::Complete => write!(f, "complete"),
            TokenKind::Sealed => write!(f, "sealed"),
            TokenKind::Const => write!(f, "const"),
            TokenKind::Import => write!(f, "import"),
            TokenKind::If => write!(f, "if"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::While => write!(f, "while"),
            TokenKind::For => write!(f, "for"),
            TokenKind::In => write!(f, "in"),
            TokenKind::Return => write!(f, "return"),
            TokenKind::Try => write!(f, "try"),
            TokenKind::Catch => write!(f, "catch"),
            TokenKind::Throw => write!(f, "throw"),
            TokenKind::True => write!(f, "true"),
            TokenKind::False => write!(f, "false"),
            TokenKind::Nil => write!(f, "nil"),

            TokenKind::Plus => write!(f, "+"),
            TokenKind::Minus => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::SlashSlash => write!(f, "//"),
            TokenKind::Percent => write!(f, "%"),

            TokenKind::Assign => write!(f, "="),
            TokenKind::Eq => write!(f, "=="),
            TokenKind::Ne => write!(f, "!="),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Le => write!(f, "<="),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::Ge => write!(f, ">="),

            TokenKind::Not => write!(f, "!"),
            TokenKind::AndAnd => write!(f, "&&"),
            TokenKind::OrOr => write!(f, "||"),

            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::LBrace => write!(f, "{{"),
            TokenKind::RBrace => write!(f, "}}"),
            TokenKind::LBracket => write!(f, "["),
            TokenKind::RBracket => write!(f, "]"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Dot => write!(f, "."),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::Semi => write!(f, ";"),

            TokenKind::Eof => write!(f, "end of input"),
        }
    }
}

/// The `##` lines written immediately above a token.
///
/// One entry per line, holding the text after the `##` and the span of the
/// whole line — so a report about one tag underlines that tag rather than the
/// paragraph above it.
///
/// Raw here on purpose. The lexer's job is to notice that these lines are
/// documentation rather than commentary; deciding what they *say* is
/// [`crate::doc`]'s, and it needs the declaration in hand to do it.
#[derive(Clone, Debug, PartialEq)]
pub struct DocBlock {
    pub lines: Vec<(String, Span)>,
}

impl DocBlock {
    /// Where the block begins, for a report that has nothing more specific to
    /// point at.
    pub fn span(&self) -> Span {
        match (self.lines.first(), self.lines.last()) {
            (Some((_, first)), Some((_, last))) => first.to(*last),
            _ => Span::new(0, 0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Whether a line break separates this token from the previous one.
    ///
    /// Statements are newline-terminated, so the parser needs to see line
    /// structure. Recording it per token keeps newlines out of the stream
    /// itself, which would otherwise have to be skipped at every match site.
    pub newline_before: bool,
    /// The documentation written above this token, if any.
    ///
    /// Carried on the token for exactly the reason `newline_before` is. A
    /// `Doc` token in the stream would have to be skipped at every match site
    /// in the parser — a hundred places that have no interest in it, any one of
    /// which could forget. Recording it here means the four declaration sites
    /// that *do* care ask for it and nothing else changes.
    pub doc: Option<DocBlock>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VS Code grammar, read at compile time so a renamed or deleted file
    /// fails the build rather than the assertion.
    const GRAMMAR: &str = include_str!("../editors/vscode/syntaxes/quince.tmLanguage.json");

    /// Every whole word the grammar highlights.
    ///
    /// Each keyword rule is an alternation of literal words — `\b(fn|op|class)\b`
    /// — so the words are whatever sits between `\b(` and the `)` that closes it.
    /// Alternatives that are not plain words belong to the character-class rules
    /// and are dropped.
    fn highlighted_words(grammar: &str) -> Vec<String> {
        let json: serde_json::Value =
            serde_json::from_str(grammar).expect("the grammar is JSON");
        let mut patterns = Vec::new();
        collect_matches(&json, &mut patterns);

        let mut words = Vec::new();
        for pattern in patterns {
            let mut rest = pattern.as_str();
            while let Some(open) = rest.find("\\b(") {
                let inner = &rest[open + 3..];
                let Some(close) = inner.find(')') else { break };
                words.extend(
                    inner[..close]
                        .split('|')
                        .filter(|word| {
                            !word.is_empty() && word.chars().all(|c| c.is_ascii_alphabetic())
                        })
                        .map(String::from),
                );
                rest = &inner[close..];
            }
        }
        words
    }

    fn collect_matches(node: &serde_json::Value, into: &mut Vec<String>) {
        match node {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if key == "match"
                        && let Some(pattern) = value.as_str()
                    {
                        into.push(pattern.to_string());
                    }
                    collect_matches(value, into);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_matches(item, into);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_keyword_explains_itself() {
        // The guard the ten-entry table in `lsp.rs` could not have: it covered
        // a quarter of the list and had no way to say so. Reserving a word now
        // means explaining it, and the exhaustive match in `doc` means the
        // compiler asks first.
        for keyword in KEYWORDS {
            let kind = TokenKind::keyword(keyword)
                .unwrap_or_else(|| panic!("`{keyword}` is listed and does not lex"));
            let doc = kind
                .doc()
                .unwrap_or_else(|| panic!("`{keyword}` is reserved and undocumented"));
            // Non-empty and nothing more. `true` is answered by "The true
            // boolean." and a rule about length would only be a worse guess
            // than the writer's about how much a word needs saying.
            assert!(!doc.is_empty(), "`{keyword}` is documented with nothing");
        }
    }

    #[test]
    fn only_a_keyword_explains_itself() {
        // The other direction, for the same reason the grammar is checked both
        // ways: a word that stops being reserved and keeps its explanation goes
        // on reading as part of the language.
        for kind in [
            TokenKind::Plus,
            TokenKind::LParen,
            TokenKind::Eof,
            TokenKind::Int(1),
            TokenKind::Ident("total".into()),
        ] {
            assert_eq!(kind.doc(), None, "{kind:?} is not a keyword");
        }
    }

    /// The one consumer of [`KEYWORDS`] that cannot read it.
    ///
    /// VS Code parses `quince.tmLanguage.json` without ever running our code, so
    /// the words in it are a copy, and the copy had drifted by three — `extend`,
    /// `complete`, and `sealed` were reserved by the lexer and plain identifiers
    /// to the highlighter. Generating the file would be the obvious fix and is
    /// the wrong one: the grammar sorts keywords into four rules by category, and
    /// only a person can say which category a new word belongs to. So the copy
    /// stays a copy and this says when it is wrong.
    #[test]
    fn the_editor_grammar_spells_every_keyword() {
        let highlighted = highlighted_words(GRAMMAR);
        for keyword in KEYWORDS {
            assert!(
                highlighted.iter().any(|word| word == keyword),
                "`{keyword}` is reserved but the VS Code grammar does not highlight it — \
                 add it to editors/vscode/syntaxes/quince.tmLanguage.json"
            );
        }
    }

    /// And nothing the grammar highlights has stopped being a keyword, which is
    /// the same drift in the other direction: a word removed from the language
    /// keeps its colour and reads as reserved when it is not.
    #[test]
    fn the_editor_grammar_highlights_nothing_else() {
        for word in highlighted_words(GRAMMAR) {
            assert!(
                KEYWORDS.contains(&word.as_str()),
                "the VS Code grammar highlights `{word}`, which is not a reserved word"
            );
        }
    }
}

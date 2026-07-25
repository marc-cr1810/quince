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
    Class,
    /// A keyword rather than an identifier so that using it outside a method is
    /// caught by the resolver, with a message that says why.
    SelfKw,
    Let,
    Const,
    If,
    Else,
    While,
    For,
    In,
    Return,
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

impl TokenKind {
    /// Maps an identifier to its keyword, if it is one.
    pub fn keyword(word: &str) -> Option<TokenKind> {
        let kind = match word {
            "fn" => TokenKind::Fn,
            "class" => TokenKind::Class,
            "self" => TokenKind::SelfKw,
            "let" => TokenKind::Let,
            "const" => TokenKind::Const,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            _ => return None,
        };
        Some(kind)
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
            TokenKind::Class => write!(f, "class"),
            TokenKind::SelfKw => write!(f, "self"),
            TokenKind::Let => write!(f, "let"),
            TokenKind::Const => write!(f, "const"),
            TokenKind::If => write!(f, "if"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::While => write!(f, "while"),
            TokenKind::For => write!(f, "for"),
            TokenKind::In => write!(f, "in"),
            TokenKind::Return => write!(f, "return"),
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
}

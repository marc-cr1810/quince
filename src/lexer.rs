use crate::error::QuinceError;
use crate::token::{Span, Token, TokenKind};

pub struct Lexer<'a> {
    src: &'a str,
    /// Byte offset of the next character to read.
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer { src, pos: 0 }
    }

    /// Consumes the whole input, producing tokens terminated by `Eof`.
    pub fn tokenize(mut self) -> Result<Vec<Token>, QuinceError> {
        let mut tokens = Vec::new();

        loop {
            let newline_before = self.skip_trivia();
            let start = self.pos;

            let Some(c) = self.advance() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(start, start),
                    newline_before,
                });
                return Ok(tokens);
            };

            let kind = match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                ',' => TokenKind::Comma,
                '.' => TokenKind::Dot,
                ':' => TokenKind::Colon,
                ';' => TokenKind::Semi,

                '+' => TokenKind::Plus,
                '-' => TokenKind::Minus,
                '*' => TokenKind::Star,
                '/' if self.eat('/') => TokenKind::SlashSlash,
                '/' => TokenKind::Slash,
                '%' => TokenKind::Percent,

                '=' if self.eat('=') => TokenKind::Eq,
                '=' => TokenKind::Assign,
                '!' if self.eat('=') => TokenKind::Ne,
                '!' => TokenKind::Not,
                '<' if self.eat('=') => TokenKind::Le,
                '<' => TokenKind::Lt,
                '>' if self.eat('=') => TokenKind::Ge,
                '>' => TokenKind::Gt,

                '&' if self.eat('&') => TokenKind::AndAnd,
                '|' if self.eat('|') => TokenKind::OrOr,
                '&' | '|' => {
                    return Err(QuinceError::new(
                        format!("unexpected '{c}' (did you mean '{c}{c}'?)"),
                        Span::new(start, self.pos),
                    ));
                }

                '"' => self.string(start)?,
                c if c.is_ascii_digit() => self.number(start)?,
                c if is_ident_start(c) => self.ident(start),

                _ => {
                    return Err(QuinceError::new(
                        format!("unexpected character '{c}'"),
                        Span::new(start, self.pos),
                    ));
                }
            };

            tokens.push(Token {
                kind,
                span: Span::new(start, self.pos),
                newline_before,
            });
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    /// The character after `peek`, needed to tell `1.5` from `1.max`.
    fn peek_next(&self) -> Option<char> {
        let mut chars = self.src[self.pos..].chars();
        chars.next()?;
        chars.next()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// Consumes `expected` if it is next, reporting whether it did.
    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += expected.len_utf8();
            true
        } else {
            false
        }
    }

    /// Skips whitespace and comments, reporting whether a line break was crossed.
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    newline |= c == '\n';
                    self.advance();
                }
                // `#` rather than `//`, which is floor division. This also makes a
                // `#!` shebang line a comment for free.
                Some('#') => {
                    // The terminating newline is left for the next iteration so it
                    // still counts as a line break.
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
                _ => return newline,
            }
        }
    }

    fn ident(&mut self, start: usize) -> TokenKind {
        while self.peek().is_some_and(is_ident_continue) {
            self.advance();
        }
        let word = &self.src[start..self.pos];
        TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()))
    }

    fn number(&mut self, start: usize) -> Result<TokenKind, QuinceError> {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.advance();
        }

        // Only a digit after the dot makes this a float; `1.max()` stays an int
        // followed by a field access.
        let is_float =
            self.peek() == Some('.') && self.peek_next().is_some_and(|c| c.is_ascii_digit());
        if is_float {
            self.advance();
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }

        let text = &self.src[start..self.pos];
        let span = Span::new(start, self.pos);

        if is_float {
            text.parse::<f64>()
                .map(TokenKind::Float)
                .map_err(|_| QuinceError::new(format!("invalid number '{text}'"), span))
        } else {
            text.parse::<i64>()
                .map(TokenKind::Int)
                .map_err(|_| QuinceError::new(format!("integer '{text}' is out of range"), span))
        }
    }

    fn string(&mut self, start: usize) -> Result<TokenKind, QuinceError> {
        let mut value = String::new();

        loop {
            let Some(c) = self.advance() else {
                return Err(QuinceError::new(
                    "unterminated string literal",
                    Span::new(start, self.pos),
                ));
            };

            match c {
                '"' => return Ok(TokenKind::Str(value)),
                '\\' => {
                    let escape_start = self.pos - 1;
                    let Some(esc) = self.advance() else {
                        return Err(QuinceError::new(
                            "unterminated string literal",
                            Span::new(start, self.pos),
                        ));
                    };
                    value.push(match esc {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        '\\' => '\\',
                        '"' => '"',
                        _ => {
                            return Err(QuinceError::new(
                                format!("unknown escape '\\{esc}'"),
                                Span::new(escape_start, self.pos),
                            ));
                        }
                    });
                }
                c => value.push(c),
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenizes to kinds only, dropping the trailing `Eof`.
    fn kinds(src: &str) -> Vec<TokenKind> {
        let mut tokens = Lexer::new(src).tokenize().expect("should lex");
        assert_eq!(tokens.pop().map(|t| t.kind), Some(TokenKind::Eof));
        tokens.into_iter().map(|t| t.kind).collect()
    }

    fn error(src: &str) -> QuinceError {
        Lexer::new(src).tokenize().expect_err("should fail to lex")
    }

    #[test]
    fn empty_input_is_just_eof() {
        let tokens = Lexer::new("").tokenize().unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].kind, TokenKind::Eof);
    }

    #[test]
    fn keywords_are_distinct_from_idents() {
        assert_eq!(kinds("let"), vec![TokenKind::Let]);
        assert_eq!(kinds("letter"), vec![TokenKind::Ident("letter".into())]);
        assert_eq!(kinds("_x1"), vec![TokenKind::Ident("_x1".into())]);
    }

    #[test]
    fn two_char_operators_win_over_one() {
        assert_eq!(
            kinds("== = != ! <= < >= > && ||"),
            vec![
                TokenKind::Eq,
                TokenKind::Assign,
                TokenKind::Ne,
                TokenKind::Not,
                TokenKind::Le,
                TokenKind::Lt,
                TokenKind::Ge,
                TokenKind::Gt,
                TokenKind::AndAnd,
                TokenKind::OrOr,
            ]
        );
    }

    #[test]
    fn integers_and_floats() {
        assert_eq!(kinds("42"), vec![TokenKind::Int(42)]);
        assert_eq!(kinds("2.5"), vec![TokenKind::Float(2.5)]);
    }

    #[test]
    fn trailing_dot_is_not_part_of_the_number() {
        // Keeps `1.max()` available as a method call rather than a bad float.
        assert_eq!(
            kinds("1.max"),
            vec![
                TokenKind::Int(1),
                TokenKind::Dot,
                TokenKind::Ident("max".into())
            ]
        );
    }

    #[test]
    fn integer_overflow_is_an_error() {
        let err = error("99999999999999999999");
        assert!(err.message.contains("out of range"), "{}", err.message);
    }

    #[test]
    fn strings_handle_escapes() {
        assert_eq!(kinds(r#""hi""#), vec![TokenKind::Str("hi".into())]);
        assert_eq!(kinds(r#""a\nb""#), vec![TokenKind::Str("a\nb".into())]);
        assert_eq!(kinds(r#""q\"q""#), vec![TokenKind::Str("q\"q".into())]);
        assert_eq!(
            kinds(r#""back\\slash""#),
            vec![TokenKind::Str("back\\slash".into())]
        );
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let err = error(r#""oops"#);
        assert!(err.message.contains("unterminated"), "{}", err.message);
    }

    #[test]
    fn unknown_escape_is_an_error() {
        let err = error(r#""a\qb""#);
        assert!(err.message.contains("unknown escape"), "{}", err.message);
    }

    #[test]
    fn comments_and_whitespace_are_skipped() {
        assert_eq!(
            kinds("let # trailing comment\n x"),
            vec![TokenKind::Let, TokenKind::Ident("x".into())]
        );
        assert_eq!(kinds("# only a comment"), vec![]);
        assert_eq!(kinds("#!/usr/bin/env quince\nlet"), vec![TokenKind::Let]);
    }

    #[test]
    fn one_slash_divides_and_two_floor_divide() {
        assert_eq!(
            kinds("a / b // c"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Slash,
                TokenKind::Ident("b".into()),
                TokenKind::SlashSlash,
                TokenKind::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn single_ampersand_suggests_the_pair() {
        let err = error("a & b");
        assert!(err.message.contains("&&"), "{}", err.message);
    }

    #[test]
    fn newlines_are_recorded_on_the_following_token() {
        let tokens = Lexer::new("a b\nc").tokenize().unwrap();
        let flags: Vec<_> = tokens.iter().map(|t| t.newline_before).collect();
        // a, b, c, Eof — only `c` follows a line break.
        assert_eq!(flags, vec![false, false, true, false]);
    }

    #[test]
    fn a_comment_does_not_swallow_its_newline() {
        let tokens = Lexer::new("a # note\nb").tokenize().unwrap();
        assert!(
            tokens[1].newline_before,
            "`b` should still start a new line"
        );
    }

    #[test]
    fn spans_cover_exactly_the_token() {
        let tokens = Lexer::new("let x = 42").tokenize().unwrap();
        let spans: Vec<_> = tokens.iter().map(|t| (t.span.start, t.span.end)).collect();
        assert_eq!(spans, vec![(0, 3), (4, 5), (6, 7), (8, 10), (10, 10)]);
    }

    #[test]
    fn spans_survive_multibyte_characters() {
        // `é` is two bytes, so a naive char-count would misplace everything after it.
        let src = r#"let é = "π""#;
        let tokens = Lexer::new(src).tokenize().unwrap();
        let last = &tokens[tokens.len() - 2];
        assert_eq!(last.kind, TokenKind::Str("π".into()));
        assert_eq!(
            &src[last.span.start as usize..last.span.end as usize],
            r#""π""#
        );
    }

    #[test]
    fn lexes_the_hello_example() {
        let src = "\
fn greet(name) {
    return \"hello, \" + name
}

let x = 42

if x > 10 {
    print(greet(\"world\"))
}
";
        let tokens = Lexer::new(src).tokenize().expect("example should lex");
        assert_eq!(tokens.first().map(|t| &t.kind), Some(&TokenKind::Fn));
        assert_eq!(tokens.last().map(|t| &t.kind), Some(&TokenKind::Eof));
    }
}

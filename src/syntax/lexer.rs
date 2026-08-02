use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::syntax::token::{DocBlock, Span, Token, TokenKind};

/// An error for text that does not tokenise.
///
/// Every error the lexer raises is one of these — there is no other way for
/// this stage to fail — so the kind is applied in one place rather than at
/// seven call sites, and a new one cannot be added unclassified.
fn syntax(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Syntax)
}

pub struct Lexer<'a> {
    src: &'a str,
    /// Byte offset of the next character to read.
    pos: usize,
    /// `##` lines seen since the last token, waiting for the token they
    /// document.
    pending_doc: Vec<(String, Span)>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src,
            pos: 0,
            pending_doc: Vec::new(),
        }
    }

    /// Hands over the documentation gathered for the token about to be pushed.
    fn take_doc(&mut self) -> Option<DocBlock> {
        if self.pending_doc.is_empty() {
            return None;
        }
        Some(DocBlock {
            lines: std::mem::take(&mut self.pending_doc),
        })
    }

    /// Whether everything between the last line break and `at` is whitespace.
    ///
    /// What separates `## docs` from `let x = 1  ## a note`. Only a `##` that
    /// starts its line is documentation, because documentation is written
    /// *above* the thing it describes — a trailing one would attach to whatever
    /// came next, which is a line the writer was not looking at.
    fn at_line_start(&self, at: usize) -> bool {
        let start = self.src[..at].rfind('\n').map_or(0, |index| index + 1);
        self.src[start..at].chars().all(|c| c.is_whitespace())
    }

    /// Consumes the whole input, producing tokens terminated by `Eof`.
    pub fn tokenize(mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();

        loop {
            let newline_before = self.skip_trivia();
            let start = self.pos;

            let Some(c) = self.advance() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(start, start),
                    newline_before,
                    doc: self.take_doc(),
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
                // Maximal munch, and the order matters: `int??` has to lex as a
                // type and a `??` rather than as two nullability marks, so the
                // parser can refuse it as one mistake with both characters in
                // hand.
                '?' if self.eat('?') => TokenKind::QuestionQuestion,
                '?' if self.eat('.') => TokenKind::QuestionDot,
                '?' => TokenKind::Question,
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
                // Before the bare `<`, so `a << b` is a shift and not two
                // comparisons — maximal munch, as everywhere else here.
                '<' if self.eat('<') => TokenKind::Shl,
                '<' => TokenKind::Lt,
                '>' if self.eat('=') => TokenKind::Ge,
                '>' if self.eat('>') => TokenKind::Shr,
                '>' => TokenKind::Gt,

                '&' if self.eat('&') => TokenKind::AndAnd,
                '|' if self.eat('|') => TokenKind::OrOr,
                // Single-character forms are the bitwise operators as of v0.7.
                // They used to be refused with "did you mean `&&`?", which was
                // right while they meant nothing and is wrong now.
                '&' => TokenKind::Amp,
                '|' => TokenKind::Pipe,
                '^' => TokenKind::Caret,
                '~' => TokenKind::Tilde,
                '"' | '\'' => self.string(start, c)?,
                c if c.is_ascii_digit() => self.number(start)?,
                c if is_ident_start(c) => self.ident(start),

                _ => {
                    return Err(syntax(
                        format!("unexpected character '{c}'"),
                        Span::new(start, self.pos),
                    ));
                }
            };

            tokens.push(Token {
                kind,
                span: Span::new(start, self.pos),
                newline_before,
                doc: self.take_doc(),
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
                    let start = self.pos;
                    // The terminating newline is left for the next iteration so it
                    // still counts as a line break.
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                    // `##` at the start of a line is documentation and is kept;
                    // everything else is commentary and is thrown away exactly
                    // as it always was. A shebang is `#!` and so is unaffected,
                    // and neither is `### ---` — that is a `##` whose text
                    // happens to begin with a `#`, which is a banner and reads
                    // as one.
                    let line = &self.src[start..self.pos];
                    if let Some(text) = line.strip_prefix("##")
                        && self.at_line_start(start)
                    {
                        self.pending_doc
                            .push((text.trim().to_string(), Span::new(start, self.pos)));
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

    fn number(&mut self, start: usize) -> Result<TokenKind> {
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
                .map_err(|_| syntax(format!("invalid number '{text}'"), span))
        } else {
            text.parse::<i64>()
                .map(TokenKind::Int)
                .map_err(|_| syntax(format!("integer '{text}' is out of range"), span))
        }
    }

    /// Scans a string literal, ending at whichever quote opened it.
    ///
    /// The two styles differ in nothing but their delimiter — there is one
    /// string type and no character type, so `'a'` and `"a"` produce the same
    /// token. Only the terminator varies, which is why it is a parameter rather
    /// than two near-copies of this loop.
    fn string(&mut self, start: usize, quote: char) -> Result<TokenKind> {
        let mut value = String::new();

        loop {
            let Some(c) = self.advance() else {
                return Err(syntax(
                    "unterminated string literal",
                    Span::new(start, self.pos),
                ));
            };

            match c {
                c if c == quote => return Ok(TokenKind::Str(value)),
                '\\' => {
                    let escape_start = self.pos - 1;
                    let Some(esc) = self.advance() else {
                        return Err(syntax(
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
                        // Both quotes escape in both styles. An escape that is
                        // an error in one style and not the other is a rule
                        // nobody would remember, and `"it\'s"` is what someone
                        // moving a literal between styles writes.
                        '"' => '"',
                        '\'' => '\'',
                        _ => {
                            return Err(syntax(
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

    fn error(src: &str) -> Raised {
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
    fn either_quote_delimits_the_same_string() {
        // Not two string types: the token carries no trace of which quote was
        // used, so nothing downstream can tell them apart.
        assert_eq!(kinds("'hi'"), kinds(r#""hi""#));
        assert_eq!(kinds("'a\\nb'"), kinds(r#""a\nb""#));
    }

    #[test]
    fn a_quote_is_ordinary_inside_the_other_style() {
        assert_eq!(kinds(r#""it's""#), vec![TokenKind::Str("it's".into())]);
        assert_eq!(
            kinds(r#"'say "hi"'"#),
            vec![TokenKind::Str(r#"say "hi""#.into())]
        );
    }

    #[test]
    fn both_quotes_escape_in_both_styles() {
        assert_eq!(kinds(r#"'it\'s'"#), vec![TokenKind::Str("it's".into())]);
        assert_eq!(kinds(r#""it\'s""#), vec![TokenKind::Str("it's".into())]);
        assert_eq!(kinds(r#"'q\"q'"#), vec![TokenKind::Str("q\"q".into())]);
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let err = error(r#""oops"#);
        assert!(err.message.contains("unterminated"), "{}", err.message);
        // The other style reports the same way rather than running to the end of
        // the file looking for a double quote.
        let err = error("'oops");
        assert!(err.message.contains("unterminated"), "{}", err.message);
    }

    #[test]
    fn one_style_does_not_terminate_the_other() {
        let err = error("'oops\"");
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

    /// The documentation gathered onto the first token that is not `Eof`.
    fn docs(src: &str) -> Vec<String> {
        Lexer::new(src)
            .tokenize()
            .expect("the source lexes")
            .iter()
            .find(|token| token.kind != TokenKind::Eof)
            .and_then(|token| token.doc.clone())
            .map(|block| block.lines.into_iter().map(|(text, _)| text).collect())
            .unwrap_or_default()
    }

    #[test]
    fn two_hashes_at_the_start_of_a_line_are_kept() {
        assert_eq!(docs("## the docs\nlet x = 1"), vec!["the docs".to_string()]);
        assert_eq!(
            docs("## first\n##\n## third\nlet x = 1"),
            vec!["first".to_string(), String::new(), "third".to_string()]
        );
        // Indented, because a method's documentation sits inside a class body.
        assert_eq!(docs("class C {\n    ## the docs\n    fn m() {}\n}"), Vec::<String>::new());
        assert_eq!(
            docs("    ## the docs\n    fn m() {}"),
            vec!["the docs".to_string()]
        );
    }

    #[test]
    fn one_hash_is_still_thrown_away() {
        // The whole point of choosing `##`: a comment stays a comment, and
        // nothing anybody has already written becomes documentation.
        assert_eq!(docs("# just a note\nlet x = 1"), Vec::<String>::new());
        assert_eq!(docs("#!/usr/bin/env quince\nlet x = 1"), Vec::<String>::new());
    }

    #[test]
    fn a_trailing_double_hash_documents_nothing() {
        // It would otherwise attach to whatever came next, which is a line the
        // writer was not looking at when they typed it.
        assert_eq!(docs("let x = 1  ## a note\nlet y = 2"), Vec::<String>::new());
    }

    #[test]
    fn documentation_attaches_to_the_token_that_follows_it() {
        let tokens = Lexer::new("## the docs\nlet x = 1\nlet y = 2")
            .tokenize()
            .expect("the source lexes");
        assert!(tokens[0].doc.is_some(), "the first `let` is documented");
        assert!(
            tokens.iter().skip(1).all(|token| token.doc.is_none()),
            "nothing else is"
        );
    }

    #[test]
    fn a_doc_line_carries_the_span_of_the_line_it_was_written_on() {
        // What lets a report about one tag underline that tag. Checked against
        // the source text rather than against numbers, so it cannot pass while
        // pointing somewhere plausible and wrong.
        let src = "## first\n## @param x second\nfn f(x) {}";
        let tokens = Lexer::new(src).tokenize().expect("the source lexes");
        let block = tokens[0].doc.clone().expect("the `fn` is documented");
        for (text, span) in block.lines {
            let written = &src[span.start as usize..span.end as usize];
            assert_eq!(written.trim_start_matches('#').trim(), text);
        }
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
    fn a_single_ampersand_is_the_bitwise_operator() {
        // It used to be refused with "did you mean `&&`?", which was the right
        // answer while it meant nothing. v0.7 gives it a meaning, so the
        // suggestion would now be wrong — and the pair still wins on munch.
        assert_eq!(
            kinds("a & b && c"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Amp,
                TokenKind::Ident("b".into()),
                TokenKind::AndAnd,
                TokenKind::Ident("c".into()),
            ]
        );
        assert_eq!(
            kinds("a | b || c"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Pipe,
                TokenKind::Ident("b".into()),
                TokenKind::OrOr,
                TokenKind::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn a_shift_wins_over_two_comparisons() {
        // Maximal munch, and the case that needs saying: `<` and `<=` were
        // already both there, so `<<` had to be checked before the bare one.
        assert_eq!(
            kinds("a << b >> c <= d"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Shl,
                TokenKind::Ident("b".into()),
                TokenKind::Shr,
                TokenKind::Ident("c".into()),
                TokenKind::Le,
                TokenKind::Ident("d".into()),
            ]
        );
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

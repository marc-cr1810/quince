//! What a `##` block says.
//!
//! The lexer decides which lines are documentation; this decides what they
//! mean. A doc block is a summary and then a run of tags — `@param`, `@return`,
//! `@throws` — and the tags are the whole reason this is a parser rather than a
//! string on the side.
//!
//! **A tag names something the declaration has, and is checked against it.** A
//! `@param radius` on a function whose parameter is `r` is refused, with the
//! caret under the tag. That is the same rule `op lenght` is refused by, for
//! the same reason: a name written twice in two places is a name that will
//! drift, and the only defence is for the second copy to be checked against the
//! first. Documentation that has quietly stopped describing its function is
//! worse than none, because it is read and believed.
//!
//! The tag set is closed. [`Tag::from_name`] is the only way in, so `@parm` is
//! an error naming the three that exist rather than a line silently swallowed
//! into the prose above it.

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::syntax::ast::Param;
use crate::syntax::token::{DocBlock, Span};

/// A tag a doc block may carry.
///
/// A closed set, listed in [`TAGS`], for the reason [`crate::syntax::ast::Op`] is: the
/// alternative is a typo that reads as prose and is never noticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    /// `@param name text` — one of the declaration's parameters.
    Param,
    /// `@return text` — what calling it produces.
    Return,
    /// `@throws Class text` — an error it may raise.
    Throws,
}

/// Every tag, for validating one and for listing them in the error when a
/// written one does not exist.
pub static TAGS: &[Tag] = &[Tag::Param, Tag::Return, Tag::Throws];

impl Tag {
    /// The word written after the `@`.
    ///
    /// An exhaustive match, so a new tag cannot be added without being given
    /// one — and it has to be listed in [`TAGS`] to be reachable, which
    /// `every_listed_tag_round_trips_through_its_name` holds up.
    pub fn name(self) -> &'static str {
        match self {
            Tag::Param => "param",
            Tag::Return => "return",
            Tag::Throws => "throws",
        }
    }

    /// Whether the tag is followed by a name before its text.
    ///
    /// `@param x the offset` names a parameter and `@throws ValueError when …`
    /// names a class; `@return the distance` names nothing, because there is
    /// only ever one thing to return.
    pub fn takes_a_name(self) -> bool {
        match self {
            Tag::Param | Tag::Throws => true,
            Tag::Return => false,
        }
    }

    /// Whether the tag only makes sense on something with a parameter list.
    ///
    /// A `let` binds a value; it has no parameters, returns nothing, and raises
    /// nothing. Writing `@return` above one is a mistake with no reading, so it
    /// is refused rather than rendered.
    pub fn needs_a_signature(self) -> bool {
        match self {
            Tag::Param | Tag::Return | Tag::Throws => true,
        }
    }

    pub fn from_name(name: &str) -> Option<Tag> {
        TAGS.iter().copied().find(|tag| tag.name() == name)
    }
}

/// A `@return`, which names nothing because there is only one return.
///
/// Its own type rather than a bare `String` so that it carries a span. A report
/// about `@return` on a binding has to underline the `@return`, not the summary
/// three lines above it — the same rule every other diagnostic in the language
/// follows.
#[derive(Clone, Debug, PartialEq)]
pub struct Returns {
    pub text: String,
    pub span: Span,
}

/// One `@param` or `@throws`, with what it names.
#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub name: String,
    pub text: String,
    /// The line the tag was written on, so a report about it underlines that
    /// line and not the whole block.
    pub span: Span,
}

/// A parsed doc block.
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    /// Everything before the first tag, as written.
    pub summary: String,
    pub params: Vec<Named>,
    pub returns: Option<Returns>,
    pub throws: Vec<Named>,
    /// Where the block was written.
    pub span: Span,
}

/// An error for documentation that does not describe what it sits above.
///
/// The same kind the resolver raises, and for the same reason: by the time this
/// runs the text is well-formed and what is left to get wrong is names.
fn doc_error(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Declaration)
}

impl Doc {
    /// Reads a block into its summary and tags.
    ///
    /// A line that does not begin with `@` continues whatever came before it,
    /// which is what lets a tag's text run to a second line without repeating
    /// the tag.
    pub fn parse(block: &DocBlock) -> Result<Doc> {
        let mut doc = Doc {
            summary: String::new(),
            params: Vec::new(),
            returns: None,
            throws: Vec::new(),
            span: block.span(),
        };
        // Where a continuation line goes: the summary until a tag opens, and
        // that tag's text afterwards.
        let mut open: Option<(Tag, usize)> = None;
        let mut summary: Vec<&str> = Vec::new();

        for (line, span) in &block.lines {
            let Some(rest) = line.strip_prefix('@') else {
                match open {
                    None => summary.push(line),
                    Some((Tag::Param, index)) => continue_text(&mut doc.params[index].text, line),
                    Some((Tag::Throws, index)) => continue_text(&mut doc.throws[index].text, line),
                    Some((Tag::Return, _)) => continue_text(
                        &mut doc.returns.as_mut().expect("opened by @return").text,
                        line,
                    ),
                }
                continue;
            };

            let mut words = rest.split_whitespace();
            let word = words.next().unwrap_or_default();
            let Some(tag) = Tag::from_name(word) else {
                let known: Vec<String> =
                    TAGS.iter().map(|tag| format!("`@{}`", tag.name())).collect();
                return Err(doc_error(
                    format!("`@{word}` is not a documentation tag"),
                    *span,
                )
                .with_help(format!("the tags are {}", known.join(", "))));
            };

            let rest = rest[word.len()..].trim();
            if tag.takes_a_name() {
                let mut parts = rest.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or_default();
                if name.is_empty() {
                    return Err(doc_error(
                        format!("`@{}` needs a name after it", tag.name()),
                        *span,
                    ));
                }
                let text = parts.next().unwrap_or_default().trim().to_string();
                let named = Named {
                    name: name.to_string(),
                    text,
                    span: *span,
                };
                match tag {
                    Tag::Param => {
                        if let Some(previous) =
                            doc.params.iter().find(|param| param.name == named.name)
                        {
                            return Err(doc_error(
                                format!("`{}` is documented twice", named.name),
                                named.span,
                            )
                            .with_help(format!(
                                "it was already described on line {}",
                                previous.span.start
                            )));
                        }
                        doc.params.push(named);
                        open = Some((Tag::Param, doc.params.len() - 1));
                    }
                    Tag::Throws => {
                        doc.throws.push(named);
                        open = Some((Tag::Throws, doc.throws.len() - 1));
                    }
                    Tag::Return => unreachable!("`@return` takes no name"),
                }
            } else {
                if doc.returns.is_some() {
                    return Err(doc_error(
                        "`@return` is written twice, and there is one return".to_string(),
                        *span,
                    ));
                }
                doc.returns = Some(Returns {
                    text: rest.to_string(),
                    span: *span,
                });
                open = Some((Tag::Return, 0));
            }
        }

        // Trailing blank lines are separators someone typed for the shape of
        // the block, not part of what it says.
        while summary.last().is_some_and(|line| line.is_empty()) {
            summary.pop();
        }
        doc.summary = summary.join("\n");
        Ok(doc)
    }

    /// Reads documentation that came from somewhere other than a `##` block.
    ///
    /// A native's doc is a Rust string literal, so it has no source to point
    /// at — every span is empty and a report drawn against one would underline
    /// the first byte of the program. That is why a malformed native doc is
    /// caught by a test rather than surfaced as a diagnostic. What this buys is
    /// one renderer: `print` and a function someone wrote are drawn by the same
    /// code, from the same shape.
    pub fn parse_text(text: &str) -> Result<Doc> {
        let block = DocBlock {
            lines: text
                .lines()
                .map(|line| (line.trim().to_string(), Span::new(0, 0)))
                .collect(),
        };
        Doc::parse(&block)
    }

    /// Checks the block against the parameters of what it documents.
    ///
    /// The point of the whole design. A `@param` naming something the
    /// declaration does not have is refused, because it is documentation that
    /// has stopped describing its function and there is no reading under which
    /// it is right.
    ///
    /// The other direction is left alone: a parameter with no `@param` is a
    /// parameter nobody has got round to, which is ordinary and is not a
    /// mistake. Refusing it would mean a half-documented function cannot be
    /// written at all, and what people do about a rule like that is delete the
    /// documentation.
    pub fn check(&self, params: &[Param]) -> Result<()> {
        for documented in &self.params {
            if !params
                .iter()
                .any(|param| !param.receiver && param.name == documented.name)
            {
                let mut error = doc_error(
                    format!("`{}` is not a parameter", documented.name),
                    documented.span,
                );
                let names: Vec<&str> = params
                    .iter()
                    .filter(|param| !param.receiver)
                    .map(|param| param.name.as_str())
                    .collect();
                error = match names.is_empty() {
                    true => error.with_help("it takes none".to_string()),
                    false => error.with_help(format!("it takes {}", names.join(", "))),
                };
                return Err(error);
            }
        }
        Ok(())
    }

    /// Refuses the tags that need a signature, for a declaration that has none.
    ///
    /// A `let` and a `class` get a summary and nothing else.
    pub fn check_has_no_signature(&self, what: &str) -> Result<()> {
        let offender = self
            .params
            .first()
            .map(|named| (Tag::Param, named.span))
            .or_else(|| self.throws.first().map(|named| (Tag::Throws, named.span)))
            .or_else(|| self.returns.as_ref().map(|it| (Tag::Return, it.span)));
        match offender {
            Some((tag, span)) if tag.needs_a_signature() => Err(doc_error(
                format!("`@{}` does not describe {what}", tag.name()),
                span,
            )
            .with_help(format!(
                "`@{}` describes part of a signature, and {what} has none — a summary is all \
                 that applies here",
                tag.name()
            ))),
            _ => Ok(()),
        }
    }

    /// Whether the block says anything at all.
    pub fn is_empty(&self) -> bool {
        self.summary.is_empty()
            && self.params.is_empty()
            && self.returns.is_none()
            && self.throws.is_empty()
    }
}

/// Adds a continuation line to a tag's text.
fn continue_text(text: &mut String, line: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lexer::Lexer;
    use crate::syntax::token::TokenKind;

    /// The doc block written above the first real token of `src`.
    fn block(src: &str) -> DocBlock {
        let tokens = Lexer::new(src).tokenize().expect("the source lexes");
        tokens
            .iter()
            .find(|token| token.kind != TokenKind::Eof)
            .and_then(|token| token.doc.clone())
            .unwrap_or_else(|| panic!("no documentation was gathered from {src:?}"))
    }

    fn parse(src: &str) -> Doc {
        Doc::parse(&block(src)).expect("the block parses")
    }

    fn parse_err(src: &str) -> String {
        let block = block(src);
        let err = Doc::parse(&block).expect_err("the block should be refused");
        match err.help {
            Some(help) => format!("{}: {help}", err.message),
            None => err.message,
        }
    }

    #[test]
    fn a_block_with_no_tags_is_all_summary() {
        let doc = parse("## The distance.\n## Never negative.\nfn f() {}\n");
        assert_eq!(doc.summary, "The distance.\nNever negative.");
        assert!(doc.params.is_empty());
        assert_eq!(doc.returns, None);
    }

    #[test]
    fn a_tag_ends_the_summary_and_starts_its_own_text() {
        let doc = parse(
            "## The distance from the origin.\n##\n## @param x the horizontal offset\n## @param y the vertical offset\n## @return the distance\nfn f(x, y) {}\n",
        );
        assert_eq!(doc.summary, "The distance from the origin.");
        assert_eq!(doc.params.len(), 2);
        assert_eq!(doc.params[0].name, "x");
        assert_eq!(doc.params[0].text, "the horizontal offset");
        assert_eq!(doc.params[1].name, "y");
        assert_eq!(doc.returns.map(|it| it.text).as_deref(), Some("the distance"));
    }

    #[test]
    fn a_plain_line_after_a_tag_continues_it() {
        let doc = parse(
            "## Sums it.\n## @param xs the numbers,\n## which may be empty\n## @return the total\nfn f(xs) {}\n",
        );
        assert_eq!(doc.summary, "Sums it.");
        assert_eq!(doc.params[0].text, "the numbers,\nwhich may be empty");
    }

    #[test]
    fn a_throws_names_the_class_it_raises() {
        let doc = parse("## Reads it.\n## @throws IoError when the file is missing\nfn f(p) {}\n");
        assert_eq!(doc.throws.len(), 1);
        assert_eq!(doc.throws[0].name, "IoError");
        assert_eq!(doc.throws[0].text, "when the file is missing");
    }

    #[test]
    fn a_tag_that_does_not_exist_is_refused() {
        // Rather than swallowed into the prose, which is what a doc format
        // without a closed tag set does with a typo.
        let message = parse_err("## Sums it.\n## @parm xs the numbers\nfn f(xs) {}\n");
        assert!(message.contains("`@parm` is not a documentation tag"), "{message}");
        assert!(message.contains("`@param`"), "{message}");
    }

    #[test]
    fn a_tag_that_needs_a_name_and_has_none_is_refused() {
        let message = parse_err("## Sums it.\n## @param\nfn f(xs) {}\n");
        assert!(message.contains("`@param` needs a name"), "{message}");
    }

    #[test]
    fn documenting_one_parameter_twice_is_refused() {
        let message =
            parse_err("## Sums it.\n## @param xs the numbers\n## @param xs again\nfn f(xs) {}\n");
        assert!(message.contains("`xs` is documented twice"), "{message}");
    }

    #[test]
    fn two_returns_are_refused() {
        let message = parse_err("## Sums it.\n## @return the total\n## @return or not\nfn f() {}\n");
        assert!(message.contains("`@return` is written twice"), "{message}");
    }

    #[test]
    fn a_param_the_declaration_does_not_have_is_refused() {
        // The rule the format exists for. A doc naming `radius` above a
        // function taking `r` has drifted, and there is no reading of it that
        // is right.
        let doc = parse("## The area.\n## @param radius how far out\nfn f(r) {}\n");
        let params = vec![Param {
            name: "r".to_string(),
            span: Span::new(0, 0),
            ty: None,
            bind: crate::syntax::ast::BindKind::Let,
            receiver: false,
        }];
        let err = doc.check(&params).expect_err("a drifted name is refused");
        assert!(err.message.contains("`radius` is not a parameter"), "{}", err.message);
        assert_eq!(err.help.as_deref(), Some("it takes r"));
    }

    #[test]
    fn a_parameter_nobody_documented_is_allowed() {
        // The other direction, deliberately. A rule against it would mean a
        // half-documented function cannot be written, and what people do about
        // a rule like that is delete the documentation.
        let doc = parse("## The area.\n## @param w the width\nfn f(w, h) {}\n");
        let params = ["w", "h"]
            .into_iter()
            .map(|name| Param {
                name: name.to_string(),
                span: Span::new(0, 0),
                ty: None,
                bind: crate::syntax::ast::BindKind::Let,
                receiver: false,
            })
            .collect::<Vec<_>>();
        assert!(doc.check(&params).is_ok());
    }

    #[test]
    fn the_receiver_is_not_a_parameter_anyone_documents() {
        // `self` is inserted by the parser, not written by the person, so it is
        // not theirs to describe — and naming it should read as the mistake it
        // is rather than quietly working.
        let doc = parse("## The area.\n## @param self the receiver\nfn f(w) {}\n");
        let params = vec![
            Param {
                name: "self".to_string(),
                span: Span::new(0, 0),
                ty: None,
                bind: crate::syntax::ast::BindKind::Let,
                receiver: true,
            },
            Param {
                name: "w".to_string(),
                span: Span::new(0, 0),
                ty: None,
                bind: crate::syntax::ast::BindKind::Let,
                receiver: false,
            },
        ];
        let err = doc.check(&params).expect_err("`self` is not documented");
        assert!(err.message.contains("`self` is not a parameter"), "{}", err.message);
    }

    #[test]
    fn a_signature_tag_on_something_with_no_signature_is_refused() {
        let doc = parse("## A cache.\n## @return nothing\nlet cache = 1\n");
        let err = doc
            .check_has_no_signature("a binding")
            .expect_err("a binding does not return");
        assert!(err.message.contains("`@return` does not describe a binding"), "{}", err.message);
    }

    /// The grammar that cannot read [`TAGS`].
    ///
    /// The same copy the keywords are under, guarded the same way and for the
    /// reason that one exists: VS Code parses the file without running our
    /// code, so a tag added here and not there is a tag the editor renders as
    /// prose while the parser accepts it.
    #[test]
    fn the_editor_grammar_spells_every_documentation_tag() {
        const GRAMMAR: &str =
            include_str!("../../editors/vscode/syntaxes/quince.tmLanguage.json");
        for tag in TAGS {
            let written = format!("{}|", tag.name());
            let last = format!("{})", tag.name());
            assert!(
                GRAMMAR.contains(&written) || GRAMMAR.contains(&last),
                "`@{}` is a documentation tag and the VS Code grammar does not \
                 highlight it — add it to the `comments` rule in \
                 editors/vscode/syntaxes/quince.tmLanguage.json",
                tag.name()
            );
        }
    }

    #[test]
    fn every_listed_tag_round_trips_through_its_name() {
        for tag in TAGS {
            assert_eq!(Tag::from_name(tag.name()), Some(*tag), "{}", tag.name());
        }
        assert_eq!(Tag::from_name("parm"), None);
        assert_eq!(Tag::from_name(""), None);
    }
}

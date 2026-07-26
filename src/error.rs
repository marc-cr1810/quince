use std::fmt;

use crate::color::Style;
use crate::heap::ObjId;
use crate::token::Span;

/// What sort of error this is, independent of how it is worded.
///
/// Two jobs. It picks the class a `catch` reifies the error into, and it is what
/// a typed `catch e: TypeError` will eventually filter on. Message strings are
/// for humans and programs should never match on them, so this is the half that
/// has to stay stable — retrofitting it after programs are written against
/// message text is not a thing that can be done quietly.
///
/// Every variant names a class bound as a global at startup, and every one of
/// those extends `Error`, so `catch e` binds the same shape whatever went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Unclassified, and the default. There are around forty raise sites and
    /// most of them predate this enum; leaving `new` to fill this in is what
    /// kept adding a kind from touching all of them.
    Runtime,
    Type,
    Name,
    /// A field or method that the receiver does not have. Separate from
    /// [`ErrorKind::Type`] because the receiver's type is usually right and the
    /// name usually is not, which is a different mistake to go looking for.
    Attr,
    /// The right type carrying a value it cannot represent — `int("abc")`, where
    /// a string is exactly what `int` accepts and that particular string is not
    /// a number. Separate from [`ErrorKind::Type`] because the call is well
    /// formed and only the data is wrong, so the fix is upstream in the data
    /// rather than at the call.
    Value,
    Index,
    Key,
    Frozen,
    Recursion,
    ZeroDivision,
    Overflow,
    /// Raised by `throw`. The class comes from the instance in
    /// [`QuinceError::payload`], so this variant names none of its own.
    Thrown,
}

impl ErrorKind {
    /// The class an error of this kind reifies into.
    ///
    /// Kept beside the variants rather than in the interpreter so that adding a
    /// kind and forgetting to bind its class is a compile error in one file
    /// instead of a missing global discovered by a `catch`.
    pub fn class_name(&self) -> &'static str {
        match self {
            // `Thrown` never reaches here — the payload carries its own class —
            // but the base is the honest answer rather than a panic.
            ErrorKind::Runtime | ErrorKind::Thrown => "Error",
            ErrorKind::Type => "TypeError",
            ErrorKind::Name => "NameError",
            ErrorKind::Attr => "AttributeError",
            ErrorKind::Value => "ValueError",
            ErrorKind::Index => "IndexError",
            ErrorKind::Key => "KeyError",
            ErrorKind::Frozen => "FrozenError",
            ErrorKind::Recursion => "RecursionError",
            ErrorKind::ZeroDivision => "ZeroDivisionError",
            ErrorKind::Overflow => "OverflowError",
        }
    }
}

/// Every kind's class, for the startup that has to bind them.
///
/// Maintained by hand for the same reason `class::BUILTINS` is: the mapping runs
/// from kind to name, and Rust cannot be asked to walk it backwards. A stale
/// entry here is a `catch` that cannot find its class, which is why
/// `every_listed_kind_names_a_distinct_class` exists.
pub static ERROR_KINDS: &[ErrorKind] = &[
    ErrorKind::Runtime,
    ErrorKind::Type,
    ErrorKind::Name,
    ErrorKind::Attr,
    ErrorKind::Value,
    ErrorKind::Index,
    ErrorKind::Key,
    ErrorKind::Frozen,
    ErrorKind::Recursion,
    ErrorKind::ZeroDivision,
    ErrorKind::Overflow,
];

/// An error carrying the source range that caused it.
///
/// Rendering is deferred to `report`, so the error itself stays cheap and does
/// not need to borrow the source. It stays a Rust struct for the whole unwind
/// and becomes a Quince value only at a `catch` — half the raise sites hold
/// `&Heap` rather than `&mut Heap`, and an uncaught error is about to be printed
/// and discarded, so allocating at raise time would be work done for the case
/// that throws it away.
#[derive(Clone, Debug, PartialEq)]
pub struct QuinceError {
    pub message: String,
    pub span: Span,
    pub kind: ErrorKind,
    /// The instance a `throw` raised, handed to the handler unchanged.
    ///
    /// An [`ObjId`] rather than a `Value` because `throw` accepts only an
    /// instance of `Error`, so the narrower type is the accurate one — and it is
    /// what lets this struct stay `PartialEq`, which `Value` is not.
    pub payload: Option<ObjId>,
    /// What an uncaught error reports itself as, when that is not derivable from
    /// [`QuinceError::kind`].
    ///
    /// Only a `throw` sets this, and only because it has to: a thrown
    /// `ParseError` should report as `ParseError` rather than as the base
    /// `Error`, and the name lives on the instance's class, which [`report`] has
    /// no heap to look up. So it is captured at the raise, where the heap is in
    /// hand. Every other error derives its label from its kind and leaves this
    /// `None`, allocating nothing.
    ///
    /// [`report`]: QuinceError::report
    pub label: Option<String>,
    /// What to do about it, rendered as a `= help:` line under the caret.
    ///
    /// Separate from [`message`] because the two answer different questions —
    /// the message says what is wrong, the help says what to write instead — and
    /// because only the message is what a `catch` sees. A handler inspecting
    /// `err.message` should not have to skip past advice aimed at whoever is
    /// reading the terminal.
    ///
    /// [`message`]: QuinceError::message
    pub help: Option<String>,
}

impl QuinceError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        QuinceError {
            message: message.into(),
            span,
            kind: ErrorKind::Runtime,
            payload: None,
            label: None,
            help: None,
        }
    }

    /// Classifies an error, as a builder so a raise site adds one line.
    pub fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Attaches advice, as a builder for the same reason as [`with_kind`].
    ///
    /// [`with_kind`]: QuinceError::with_kind
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// The error a `throw` raises, carrying the instance that was thrown.
    ///
    /// `class` is the thrown instance's own class, which is what an uncaught
    /// throw reports as — a subclass says its own name, not `Error`.
    pub fn thrown(
        payload: ObjId,
        class: impl Into<String>,
        message: impl Into<String>,
        span: Span,
    ) -> Self {
        QuinceError {
            message: message.into(),
            span,
            kind: ErrorKind::Thrown,
            payload: Some(payload),
            label: Some(class.into()),
            help: None,
        }
    }

    /// What this error reports itself as, if anything.
    ///
    /// `None` for an unclassified error, so `error: …` reads exactly as it did
    /// before kinds existed rather than gaining a bracket that says nothing. That
    /// also means the bracket appearing is a signal the kind is known, which is a
    /// standing nudge to classify the raise sites that still are not.
    fn label(&self) -> Option<&str> {
        let name = match &self.label {
            Some(label) => label.as_str(),
            // A `throw` always carries its label above, so reaching the `Thrown`
            // arm would mean one was built without a class.
            None => match self.kind {
                ErrorKind::Runtime | ErrorKind::Thrown => return None,
                kind => kind.class_name(),
            },
        };
        // `error[Error]` says nothing that `error` did not, so the base class
        // reports bare — whether it got there by being unclassified or by a
        // literal `throw Error("…")`.
        (name != ErrorKind::Runtime.class_name()).then_some(name)
    }

    /// Renders the error against the source it came from, with a caret pointing
    /// at the offending range.
    pub fn report(&self, source: &str, path: &str) -> String {
        self.report_styled(source, path, false)
    }

    /// Renders the error against the source with optional ANSI colors.
    pub fn report_styled(&self, source: &str, path: &str, color: bool) -> String {
        let (line, col) = line_col(source, self.span.start as usize);
        let text = source.lines().nth(line - 1).unwrap_or("");

        // The span can run past the end of its line (an unterminated string, for
        // one), so clamp the underline to what is actually on this line.
        let width = (self.span.end - self.span.start).max(1) as usize;
        let width = width
            .min(text.chars().count().saturating_sub(col - 1))
            .max(1);

        let gutter = line.to_string().len();

        // `error[TypeError]` rather than `TypeError: …`, because this report
        // already borrows rustc's shape — the `-->`, the gutter, the caret — and
        // rustc puts its code in exactly that bracket.
        let err_label = match self.label() {
            Some(name) => Style::BOLD_RED.paint(format!("error[{name}]"), color),
            None => Style::BOLD_RED.paint("error", color),
        };
        let msg = Style::BOLD.paint(&self.message, color);
        let arrow = Style::BOLD_CYAN.paint("-->", color);
        let bar = Style::BOLD_CYAN.paint("|", color);
        let caret = Style::BOLD_RED.paint("^".repeat(width), color);

        let mut out = format!(
            "{err_label}: {msg}\n\
             {blank:>gutter$}{arrow} {path}:{line}:{col}\n\
             {blank:>gutter$} {bar}\n\
             {line} {bar} {text}\n\
             {blank:>gutter$} {bar} {pad}{caret}",
            blank = "",
            pad = " ".repeat(col - 1),
        );

        // Aligned under the gutter rather than the caret, as rustc does: the
        // advice is about the whole error, not about the column.
        if let Some(help) = &self.help {
            let eq = Style::BOLD_CYAN.paint("=", color);
            let tag = Style::BOLD.paint("help:", color);
            out.push_str(&format!("\n{blank:>gutter$} {eq} {tag} {help}", blank = ""));
        }

        out
    }
}

impl fmt::Display for QuinceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for QuinceError {}

/// Resolves a byte offset to a 1-based line and column.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let line = source[..offset].matches('\n').count() + 1;
    let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = source[line_start..offset].chars().count() + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_starts_at_one() {
        assert_eq!(line_col("abc", 0), (1, 1));
    }

    #[test]
    fn line_col_tracks_newlines() {
        let src = "abc\ndef\nghi";
        assert_eq!(line_col(src, 4), (2, 1));
        assert_eq!(line_col(src, 6), (2, 3));
        assert_eq!(line_col(src, 8), (3, 1));
    }

    #[test]
    fn line_col_counts_chars_not_bytes() {
        let src = "let é = 1";
        // `é` is two bytes, so the `=` sits at byte 7 but is the 7th character;
        // counting bytes would report column 8.
        assert_eq!(line_col(src, 7), (1, 7));
    }

    #[test]
    fn every_listed_kind_names_a_distinct_class() {
        // What actually stops a new variant going unbound is that `class_name`
        // is an exhaustive match, so it cannot be added without being named.
        // This checks the other half: that the names are usable as globals, one
        // class each, and that `Thrown` stays out — it borrows its class from the
        // instance that was thrown.
        let mut names = Vec::new();
        for kind in ERROR_KINDS {
            assert_ne!(*kind, ErrorKind::Thrown, "`Thrown` names no class");
            names.push(kind.class_name());
        }
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "two kinds share a class: {names:?}"
        );
    }

    #[test]
    fn a_new_error_is_unclassified_and_carries_no_payload() {
        let err = QuinceError::new("boom", Span::new(0, 1));
        assert_eq!(err.kind, ErrorKind::Runtime);
        assert_eq!(err.payload, None);
    }

    #[test]
    fn a_classified_error_reports_its_kind() {
        let src = "let x = xs[9]";
        let err = QuinceError::new("index 9 is out of range", Span::new(8, 13))
            .with_kind(ErrorKind::Index);
        let out = err.report(src, "test.qn");
        assert!(out.starts_with("error[IndexError]: "), "{out}");
    }

    #[test]
    fn an_unclassified_error_reports_bare() {
        // Unchanged from before kinds existed, which is the point: a bracket
        // appears only when it carries something.
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        assert!(
            err.report(src, "test.qn").starts_with("error: "),
            "{}",
            err.report(src, "test.qn")
        );
    }

    #[test]
    fn a_thrown_error_reports_the_class_that_was_thrown() {
        use crate::heap::{Heap, Object};

        // A real handle rather than a fabricated one: `ObjId`'s field is private
        // so that the heap stays the only source of them.
        let mut heap = Heap::new();
        let payload = heap.alloc(Object::List(vec![]));

        let src = "throw ParseError(\"bad\", 1)";
        let err = QuinceError::thrown(payload, "ParseError", "bad", Span::new(0, 26));
        assert!(
            err.report(src, "test.qn")
                .starts_with("error[ParseError]: "),
            "{}",
            err.report(src, "test.qn")
        );

        // The base class adds nothing over the message, so it stays bare.
        let bare = QuinceError::thrown(payload, "Error", "bad", Span::new(0, 26));
        assert!(
            bare.report(src, "test.qn").starts_with("error: "),
            "{}",
            bare.report(src, "test.qn")
        );
    }

    #[test]
    fn help_renders_under_the_caret_and_stays_out_of_the_message() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9))
            .with_help("remove it, or quote it as a string");
        let out = err.report(src, "test.qn");
        assert!(
            out.ends_with("= help: remove it, or quote it as a string"),
            "{out}"
        );
        // The line the caret is on has to survive the help being appended.
        assert!(out.contains("^\n"), "{out}");
        // What a `catch` sees is unchanged: advice is for the terminal.
        assert_eq!(err.message, "unexpected character '@'");
    }

    #[test]
    fn a_report_without_help_is_byte_identical() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        assert!(!err.report(src, "test.qn").contains("help"), "no help line");
    }

    #[test]
    fn report_points_at_the_span() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        let out = err.report(src, "test.qn");
        assert!(out.contains("error: unexpected character '@'"), "{out}");
        assert!(out.contains("--> test.qn:1:9"), "{out}");
        assert!(out.ends_with("^"), "{out}");
    }

    #[test]
    fn report_styled_with_color() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        let out = err.report_styled(src, "test.qn", true);
        assert!(
            out.contains("\x1b[1;31merror\x1b[0m: \x1b[1munexpected character '@'\x1b[0m"),
            "{out}"
        );
        assert!(out.contains("\x1b[1;36m-->\x1b[0m test.qn:1:9"), "{out}");
    }
}

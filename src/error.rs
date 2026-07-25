use std::fmt;

use crate::color::Style;
use crate::token::Span;

/// An error carrying the source range that caused it.
///
/// Rendering is deferred to `report`, so the error itself stays cheap and does
/// not need to borrow the source.
#[derive(Clone, Debug, PartialEq)]
pub struct QuinceError {
    pub message: String,
    pub span: Span,
}

impl QuinceError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        QuinceError {
            message: message.into(),
            span,
        }
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

        let err_label = Style::BOLD_RED.paint("error", color);
        let msg = Style::BOLD.paint(&self.message, color);
        let arrow = Style::BOLD_CYAN.paint("-->", color);
        let bar = Style::BOLD_CYAN.paint("|", color);
        let caret = Style::BOLD_RED.paint("^".repeat(width), color);

        format!(
            "{err_label}: {msg}\n\
             {blank:>gutter$}{arrow} {path}:{line}:{col}\n\
             {blank:>gutter$} {bar}\n\
             {line} {bar} {text}\n\
             {blank:>gutter$} {bar} {pad}{caret}",
            blank = "",
            pad = " ".repeat(col - 1),
        )
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
        assert!(out.contains("\x1b[1;31merror\x1b[0m: \x1b[1munexpected character '@'\x1b[0m"), "{out}");
        assert!(out.contains("\x1b[1;36m-->\x1b[0m test.qn:1:9"), "{out}");
    }
}

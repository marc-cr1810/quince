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

#[derive(Clone, Debug, PartialEq)]
pub struct LabeledSpan {
    pub span: Span,
    pub label: Option<String>,
    pub style: Option<Style>,
}

impl LabeledSpan {
    pub fn new(span: Span, label: impl Into<String>) -> Self {
        LabeledSpan {
            span,
            label: Some(label.into()),
            style: None,
        }
    }

    pub fn unlabelled(span: Span) -> Self {
        LabeledSpan {
            span,
            label: None,
            style: None,
        }
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.style = Some(style);
        self
    }
}

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
    /// Labeled sub-spans for rich multi-token diagnostic annotations.
    pub labels: Vec<LabeledSpan>,
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
            labels: Vec::new(),
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

    /// Attaches a sub-labeled span for diagnostic tree annotations.
    pub fn with_label(mut self, span: Span, label: impl Into<String>) -> Self {
        self.labels.push(LabeledSpan::new(span, label));
        self
    }

    /// Attaches multiple sub-labeled spans.
    pub fn with_labels(mut self, labels: impl IntoIterator<Item = LabeledSpan>) -> Self {
        self.labels.extend(labels);
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
            labels: Vec::new(),
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
        let gutter = line.to_string().len();

        // 1. Error header & title
        let err_tag = match self.label() {
            Some(name) => format!("quince::{name}"),
            None => "quince::Error".to_string(),
        };

        let header_str = Style::BOLD_RED.paint(format!("Error: {err_tag}"), color);
        let cross_mark = Style::BOLD_RED.paint("×", color);
        let msg_str = Style::BOLD.paint(&self.message, color);

        let mut out = String::new();
        out.push_str(&format!("{header_str}\n\n"));
        out.push_str(&format!(" {cross_mark} {msg_str}\n"));

        const FRAME_STYLE: Style = Style::BOLD_BLUE;

        // 2. Location frame line:  ╭─[path:line:col]
        let top_corner = FRAME_STYLE.paint("╭─[", color);
        let right_bracket = FRAME_STYLE.paint("]", color);
        let loc_str = format!("{path}:{line}:{col}");
        out.push_str(&format!(" {blank:>gutter$}{top_corner}{loc_str}{right_bracket}\n", blank = ""));

        // 3. Source line: 1 │ text
        let pipe = FRAME_STYLE.paint("│", color);
        let line_num_str = Style::DIM.paint(format!("{line:>gutter$}"), color);
        out.push_str(&format!("{line_num_str} {pipe} {text}\n"));

        // 4. Multi-span or Single-span annotation tree
        let spans_to_render = if !self.labels.is_empty() {
            self.labels.clone()
        } else {
            vec![LabeledSpan::new(self.span, self.message.clone())]
        };

        let line_start_offset = source
            .lines()
            .take(line - 1)
            .map(|l| l.len() + 1)
            .sum::<usize>();

        const PALETTE: &[Style] = &[
            Style::BOLD_CYAN,
            Style::BOLD_YELLOW,
            Style::BOLD_MAGENTA,
            Style::BOLD_GREEN,
            Style::BOLD_RED,
        ];

        struct EvaluatedSpan {
            start_col: usize,
            end_col: usize,
            target_col: usize,
            label: Option<String>,
            style: Style,
        }

        let mut eval_spans = Vec::new();
        for (idx, ls) in spans_to_render.iter().enumerate() {
            let mut start = ls.span.start as usize;
            let mut end = ls.span.end as usize;

            if start < source.len() && end <= source.len() && start < end {
                let slice = &source[start..end];
                let trimmed = slice.trim();
                if !trimmed.is_empty() {
                    let leading = slice.len() - slice.trim_start().len();
                    let trailing = slice.len() - slice.trim_end().len();
                    start += leading;
                    end = end.saturating_sub(trailing).max(start + 1);
                }
            }

            let span_start_col = if start >= line_start_offset {
                source[line_start_offset..start.min(source.len())].chars().count() + 1
            } else {
                1
            };

            let span_end_col = if end >= line_start_offset {
                source[line_start_offset..end.min(source.len())].chars().count() + 1
            } else {
                span_start_col + 1
            };

            let width = (span_end_col.saturating_sub(span_start_col)).max(1);
            let target_col = span_start_col + (width - 1) / 2;
            let style = ls.style.unwrap_or_else(|| PALETTE[idx % PALETTE.len()]);

            eval_spans.push(EvaluatedSpan {
                start_col: span_start_col,
                end_col: span_start_col + width,
                target_col,
                label: ls.label.clone(),
                style,
            });
        }

        eval_spans.sort_by_key(|s| s.start_col);

        let dot = FRAME_STYLE.paint("·", color);

        // Render Underline Row
        let max_col = eval_spans
            .iter()
            .map(|s| s.end_col)
            .max()
            .unwrap_or(col + 1)
            .max(text.chars().count() + 1);

        let mut underline_chars = vec![' '; max_col + 1];

        for span in &eval_spans {
            let pointer_char = '┬';
            let bar_char = '─';

            for c in span.start_col..span.end_col {
                if c <= max_col {
                    if c == span.target_col {
                        underline_chars[c] = pointer_char;
                    } else if underline_chars[c] == ' ' {
                        underline_chars[c] = bar_char;
                    }
                }
            }
        }

        let mut underline_line = format!(" {blank:>gutter$}{dot} ", blank = "");
        for c in 1..=max_col {
            let ch = underline_chars[c];
            if ch != ' ' {
                let style = eval_spans
                    .iter()
                    .find(|s| c >= s.start_col && c < s.end_col)
                    .map(|s| s.style)
                    .unwrap_or(Style::BOLD_RED);
                underline_line.push_str(&style.paint(ch, color));
            } else {
                underline_line.push(' ');
            }
        }
        out.push_str(underline_line.trim_end());
        out.push('\n');

        // Render Branch Tree Rows
        let labeled_indices: Vec<usize> = eval_spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.label.is_some())
            .map(|(i, _)| i)
            .rev()
            .collect();

        for &active_idx in &labeled_indices {
            let target_col = eval_spans[active_idx].target_col;
            let target_style = eval_spans[active_idx].style;
            let label_text = eval_spans[active_idx].label.as_ref().unwrap();

            let mut row = format!(" {blank:>gutter$}{dot} ", blank = "");
            let mut current_col = 1;

            while current_col < target_col {
                let passing = eval_spans
                    .iter()
                    .find(|s| s.label.is_some() && s.target_col == current_col && s.target_col < target_col);

                if let Some(passing_span) = passing {
                    row.push_str(&passing_span.style.paint("│", color));
                } else {
                    row.push(' ');
                }
                current_col += 1;
            }

            let branch = target_style.paint(format!("╰── {label_text}"), color);
            row.push_str(&branch);
            out.push_str(&row);
            out.push('\n');
        }

        // 5. Closing Bottom Frame Line: ╰────
        let bottom_corner = FRAME_STYLE.paint("╰────", color);
        out.push_str(&format!(" {blank:>gutter$}{bottom_corner}\n", blank = ""));

        // 6. Help line
        if let Some(help) = &self.help {
            let tag = FRAME_STYLE.paint("help:", color);
            out.push_str(&format!(" {blank:>gutter$}{tag} {help}\n", blank = ""));
        }

        out.trim_end().to_string()
    }
}

impl fmt::Display for QuinceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Computes Levenshtein distance between two strings for fuzzy matching suggestions.
pub fn lev_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b_chars.len() + 1]; a_chars.len() + 1];

    for i in 0..=a_chars.len() {
        d[i][0] = i;
    }
    for j in 0..=b_chars.len() {
        d[0][j] = j;
    }

    for i in 1..=a_chars.len() {
        for j in 1..=b_chars.len() {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
        }
    }
    d[a_chars.len()][b_chars.len()]
}

/// Finds the closest matching candidate for `name` if one exists within a small edit distance.
pub fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best_match = None;
    let mut best_dist = usize::MAX;

    for candidate in candidates {
        let dist = lev_distance(name, candidate);
        let max_dist = if name.len() <= 4 { 1 } else { 2 };
        if dist <= max_dist && dist < best_dist {
            best_dist = dist;
            best_match = Some(candidate);
        }
    }
    best_match
}

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
        assert!(out.contains("Error: quince::IndexError"), "{out}");
        assert!(out.contains("× index 9 is out of range"), "{out}");
    }

    #[test]
    fn an_unclassified_error_reports_bare() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        let out = err.report(src, "test.qn");
        assert!(out.contains("Error: quince::Error"), "{out}");
        assert!(out.contains("× unexpected character '@'"), "{out}");
    }

    #[test]
    fn a_thrown_error_reports_the_class_that_was_thrown() {
        use crate::heap::{Heap, Object};

        let mut heap = Heap::new();
        let payload = heap.alloc(Object::List(vec![]));

        let src = "throw ParseError(\"bad\", 1)";
        let err = QuinceError::thrown(payload, "ParseError", "bad", Span::new(0, 26));
        let out = err.report(src, "test.qn");
        assert!(out.contains("Error: quince::ParseError"), "{out}");
        assert!(out.contains("× bad"), "{out}");

        let bare = QuinceError::thrown(payload, "Error", "bad", Span::new(0, 26));
        let bare_out = bare.report(src, "test.qn");
        assert!(bare_out.contains("Error: quince::Error"), "{bare_out}");
    }

    #[test]
    fn help_renders_under_the_caret_and_stays_out_of_the_message() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9))
            .with_help("remove it, or quote it as a string");
        let out = err.report(src, "test.qn");
        assert!(
            out.contains("help: remove it, or quote it as a string"),
            "{out}"
        );
        assert!(out.contains("┬"), "{out}");
        assert_eq!(err.message, "unexpected character '@'");
    }

    #[test]
    fn a_report_without_help_is_byte_identical() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        assert!(!err.report(src, "test.qn").contains("help:"), "no help line");
    }

    #[test]
    fn report_points_at_the_span() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        let out = err.report(src, "test.qn");
        assert!(out.contains("Error: quince::Error"), "{out}");
        assert!(out.contains("╭─[test.qn:1:9]"), "{out}");
        assert!(out.contains("┬"), "{out}");
        assert!(out.contains("╰── unexpected character '@'"), "{out}");
    }

    #[test]
    fn report_styled_with_color() {
        let src = "let x = @";
        let err = QuinceError::new("unexpected character '@'", Span::new(8, 9));
        let out = err.report_styled(src, "test.qn", true);
        assert!(out.contains("\x1b[1;31mError: quince::Error\x1b[0m"), "{out}");
        assert!(out.contains("\x1b[1;34m╭─[\x1b[0mtest.qn:1:9\x1b[1;34m]\x1b[0m"), "{out}");
    }

    #[test]
    fn multi_span_diagnostic_tree_rendering() {
        let src = "10 / \"bob\"";
        let err = QuinceError::new("Types mismatched for operation", Span::new(0, 10))
            .with_kind(ErrorKind::Type)
            .with_label(Span::new(0, 2), "int")
            .with_label(Span::new(3, 4), "doesn't support these values")
            .with_label(Span::new(5, 10), "string")
            .with_help("Change int or string to be the right types and try again");

        let out = err.report(src, "entry #20");
        assert!(out.contains("Error: quince::TypeError"), "{out}");
        assert!(out.contains("× Types mismatched for operation"), "{out}");
        assert!(out.contains("╭─[entry #20:1:1]"), "{out}");
        assert!(out.contains("1 │ 10 / \"bob\""), "{out}");
        assert!(out.contains("╰── string"), "{out}");
        assert!(out.contains("╰── doesn't support these values"), "{out}");
        assert!(out.contains("╰── int"), "{out}");
        assert!(out.contains("help: Change int or string to be the right types and try again"), "{out}");
    }

    #[test]
    fn fuzzy_did_you_mean_suggestions() {
        let candidates = vec!["name", "age", "items", "count"];
        assert_eq!(did_you_mean("namme", candidates.clone()), Some("name"));
        assert_eq!(did_you_mean("cont", candidates.clone()), Some("count"));
        assert_eq!(did_you_mean("completely_unrelated", candidates), None);
    }
}

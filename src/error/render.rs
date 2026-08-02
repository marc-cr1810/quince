//! Rendering an error against the source it came from.
//!
//! Separate from the error's own definition because the two are needed in
//! different places: every raise site builds a [`QuinceError`], and only the
//! two entry points and the language server ever draw one.

use std::collections::BTreeMap;
use std::fmt;

use crate::color::Style;
use crate::error::kind::ErrorKind;
use crate::error::{LabeledSpan, QuinceError};

impl QuinceError {
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
                kind => kind.code(),
            },
        };
        // `error[Error]` says nothing that `error` did not, so the base class
        // reports bare — whether it got there by being unclassified or by a
        // literal `throw Error("…")`.
        (name != ErrorKind::Runtime.code()).then_some(name)
    }

    /// Renders the error against the source it came from, with a caret pointing
    /// at the offending range.
    pub fn report(&self, source: &str, path: &str) -> String {
        self.report_styled(source, path, false)
    }

    /// Renders the error against the source with optional ANSI colors.
    ///
    /// Labels are grouped by the line they fall on and each line is shown with
    /// its own underline, because a span's column means nothing without the line
    /// it was measured against. Rendering one line and drawing every label
    /// against it — which is what this did before the sweep — puts a label for
    /// line 3 at column 17 of line 2, where it underlines whitespace past the end
    /// of the text. `a_label_on_a_later_line_is_shown_with_that_line` is the case
    /// that pins it.
    pub fn report_styled(&self, source: &str, path: &str, color: bool) -> String {
        let (line, col) = line_col(source, self.span.start as usize);

        // Sized for the highest line number any block will show, so the frame
        // stays square when a label sits ten lines below the caret.
        let gutter = self
            .labels
            .iter()
            .map(|ls| line_col(source, ls.span.start as usize).0)
            .chain(std::iter::once(line))
            .max()
            .unwrap_or(line)
            .to_string()
            .len();

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

        // 3 and 4. One block per source line that carries a label.
        // A diagnostic that supplies no labels gets a bare caret, not a copy of
        // its own message. The message is already the line above; repeating it
        // word for word under the caret is the one thing every report did that
        // no report should — it fills the space where a label would go, so the
        // reader learns to skip it, and then a real label goes unread too. A
        // label earns its place by saying something the message did not: what
        // this span *is*, and how it differs from the other one.
        let spans_to_render = if !self.labels.is_empty() {
            self.labels.clone()
        } else {
            vec![LabeledSpan::unlabelled(self.span)]
        };

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

        // Where each line begins, so a byte offset can be turned into a column
        // on whichever line it actually falls. Built once: the old code computed
        // one line's offset and measured every span against it, which is the bug
        // this pass is here to fix.
        let mut line_starts = vec![0usize];
        for (offset, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }

        // Each label, paired with the line it belongs to. The palette index
        // follows the order the labels were declared in rather than the order
        // they are drawn, so a span keeps its colour wherever it lands.
        let mut by_line: BTreeMap<usize, Vec<EvaluatedSpan>> = BTreeMap::new();
        for (idx, ls) in spans_to_render.iter().enumerate() {
            let mut start = (ls.span.start as usize).min(source.len());
            let mut end = (ls.span.end as usize).min(source.len());

            if start < end {
                let slice = &source[start..end];
                if !slice.trim().is_empty() {
                    let leading = slice.len() - slice.trim_start().len();
                    let trailing = slice.len() - slice.trim_end().len();
                    start += leading;
                    end = end.saturating_sub(trailing).max(start + 1);
                }
            }

            let (span_line, _) = line_col(source, start);
            let line_start = line_starts[span_line - 1];
            let line_end = line_start + source.lines().nth(span_line - 1).unwrap_or("").len();
            // A span that runs past its first line is drawn to the end of that
            // line. Carrying it onto the next would need a continuation mark the
            // frame has no room for, and the first line is where it starts.
            let end = end.clamp(start, line_end.max(start));

            let span_start_col = source[line_start..start].chars().count() + 1;
            let span_end_col = source[line_start..end].chars().count() + 1;

            let width = (span_end_col.saturating_sub(span_start_col)).max(1);
            let target_col = span_start_col + (width - 1) / 2;
            let style = ls.style.unwrap_or_else(|| PALETTE[idx % PALETTE.len()]);

            by_line.entry(span_line).or_default().push(EvaluatedSpan {
                start_col: span_start_col,
                end_col: span_start_col + width,
                target_col,
                label: ls.label.clone(),
                style,
            });
        }

        let dot = FRAME_STYLE.paint("·", color);
        let pipe = FRAME_STYLE.paint("│", color);

        for (block, (line_no, mut eval_spans)) in by_line.into_iter().enumerate() {
            eval_spans.sort_by_key(|s| s.start_col);
            let text = source.lines().nth(line_no - 1).unwrap_or("");

            // A gap between blocks is marked rather than closed over, so two
            // labels three lines apart do not read as if they were adjacent.
            if block > 0 {
                let ellipsis = FRAME_STYLE.paint("⋮", color);
                out.push_str(&format!(" {blank:>gutter$}{ellipsis}\n", blank = ""));
            }

            let line_num_str = Style::DIM.paint(format!("{line_no:>gutter$}"), color);
            out.push_str(&format!("{line_num_str} {pipe} {text}\n"));

            // Render Underline Row
            let max_col = eval_spans
                .iter()
                .map(|s| s.end_col)
                .max()
                .unwrap_or(col + 1)
                .max(text.chars().count() + 1);

            let mut underline_chars = vec![' '; max_col + 1];

            for span in &eval_spans {
                // `┬` is where a label's branch attaches, so a span with no label
                // draws a plain underline. Otherwise the connector hangs off the
                // bottom of the report pointing at nothing.
                let pointer_char = if span.label.is_some() { '┬' } else { '─' };
                let bar_char = '─';

                // `take` bounds the span above and the vector's own length bounds
                // it at `max_col`, which is what the range loop needed a guard for.
                for (c, slot) in underline_chars
                    .iter_mut()
                    .enumerate()
                    .take(span.end_col)
                    .skip(span.start_col)
                {
                    if c == span.target_col {
                        *slot = pointer_char;
                    } else if *slot == ' ' {
                        *slot = bar_char;
                    }
                }
            }

            let mut underline_line = format!(" {blank:>gutter$}{dot} ", blank = "");
            // Column 0 is not a column: the vector is `max_col + 1` long so that a
            // column number indexes it directly, which is what `skip(1)` steps over.
            for (c, &ch) in underline_chars.iter().enumerate().skip(1) {
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
                    let passing = eval_spans.iter().find(|s| {
                        s.label.is_some()
                            && s.target_col == current_col
                            && s.target_col < target_col
                    });

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
    use crate::syntax::token::Span;

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
    /// Both compile-time kinds report a word even though neither names a class.
    #[test]
    fn a_compile_time_error_reports_a_kind_it_cannot_be_caught_as() {
        let src = "let x = ";
        let err = QuinceError::new("expected an expression, found `end of input`", Span::new(8, 8))
            .with_kind(ErrorKind::Syntax);
        assert!(
            err.report(src, "test.qn").contains("SyntaxError"),
            "a syntax error names itself in the report"
        );
        assert_eq!(
            err.kind.class_name(),
            None,
            "and still names no class to catch"
        );
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
        use crate::runtime::heap::{Heap, Object};

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
        assert!(out.contains('─'), "the caret row is drawn\n{out}");
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

        // The caret sits under the `@` itself. Counted in characters and not in
        // bytes: the gutters differ — `│` is three bytes and `·` is two — so byte
        // offsets disagree by one on rows that line up perfectly on screen.
        let lines: Vec<&str> = out.lines().collect();
        let at = lines.iter().position(|l| l.contains("let x = @")).unwrap();
        let column_of = |row: &str, mark: char| row.chars().position(|c| c == mark);
        assert_eq!(
            column_of(lines[at + 1], '─'),
            column_of(lines[at], '@'),
            "the caret should be under the `@`\n{out}"
        );

        // The message is the line above and is not repeated as a label.
        assert!(!out.contains("╰── "), "no branch without a label\n{out}");
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

    /// A label measured against the wrong line points at nothing.
    ///
    /// Before the sweep this rendered one line and drew every label against it,
    /// so the `string` label — 17 characters into a 12-character line — underlined
    /// empty space past the end of the text and the report claimed a caret it did
    /// not have. Anything that puts an operand on its own line hit this: a
    /// wrapped condition, a multi-line call, a list built over four lines.
    #[test]
    fn a_label_on_a_later_line_is_shown_with_that_line() {
        let src = "let y = xs +\n  \"tail\"\n";
        let err = QuinceError::new("cannot add list and string", Span::new(8, 21))
            .with_kind(ErrorKind::Type)
            .with_label(Span::new(8, 10), "list")
            .with_label(Span::new(15, 21), "string");

        let out = err.report(src, "test.qn");
        let lines: Vec<&str> = out.lines().collect();

        let first = lines
            .iter()
            .position(|l| l.contains("1 │ let y = xs +"))
            .unwrap_or_else(|| panic!("the caret's own line is shown\n{out}"));
        let second = lines
            .iter()
            .position(|l| l.contains("2 │   \"tail\""))
            .unwrap_or_else(|| panic!("the second label's line is shown too\n{out}"));
        assert!(first < second, "lines come in source order\n{out}");

        // Each label sits under the line it was measured against, which is what
        // the single-line renderer could not do.
        let list_at = lines.iter().position(|l| l.contains("╰── list")).unwrap();
        let string_at = lines.iter().position(|l| l.contains("╰── string")).unwrap();
        assert!(list_at > first && list_at < second, "`list` is under line 1\n{out}");
        assert!(string_at > second, "`string` is under line 2\n{out}");

        // No underline may run past the text it is drawn under, which is the
        // symptom the old renderer showed.
        for pair in lines.windows(2) {
            let (text, under) = (pair[0], pair[1]);
            let Some((_, body)) = text.split_once('│') else {
                continue;
            };
            let Some((_, marks)) = under.split_once('·') else {
                continue;
            };
            assert!(
                marks.trim_end().chars().count() <= body.chars().count(),
                "an underline runs past its line:\n{text}\n{under}"
            );
        }
    }

    /// The message is not repeated under its own caret.
    #[test]
    fn an_unlabelled_span_underlines_without_a_branch() {
        let src = "print(f(1))";
        let err = QuinceError::new("`f` takes 2 arguments, but 1 were given", Span::new(6, 10))
            .with_kind(ErrorKind::Type);

        let out = err.report(src, "test.qn");
        assert_eq!(
            out.matches("`f` takes 2 arguments").count(),
            1,
            "the message appears once, not again as a label\n{out}"
        );
        // `╰── ` with its space, because the frame's own bottom corner is `╰────`.
        assert!(!out.contains("╰── "), "no branch without a label\n{out}");
        assert!(!out.contains('┬'), "no connector without a branch\n{out}");
    }

}

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

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
    /// A module that could not be loaded: one that is not there, one that cannot
    /// be read, one that imports itself.
    ///
    /// One kind for all three because they are one thing to whoever hit it — the
    /// import did not work — and because the fix is in the same place every
    /// time. Not [`ErrorKind::Name`], which was the first answer and the wrong
    /// one: a cycle reported as a name error says nothing true, and the three
    /// belong together more than any of them belongs with an undefined variable.
    Import,
    /// The filesystem refused: a file that is not there, a directory that cannot
    /// be written to.
    ///
    /// The one kind whose cause is outside the program, and so the one whose
    /// occurrence can differ between two runs of identical source. That is why
    /// it is separate from [`ErrorKind::Import`], which it once stood in for: an
    /// import failing is a fact about the program, and `io.read` failing is a
    /// fact about the machine.
    Io,
    /// Raised by `throw`. The class comes from the instance in
    /// [`QuinceError::payload`], so this variant names none of its own.
    Thrown,
    /// Text that does not parse — an unterminated string, a missing operand, a
    /// `{` where a name belongs.
    ///
    /// Raised inside `compile`, so see [`ErrorKind::class_name`] for why it
    /// names no class.
    Syntax,
    /// Text that parses and still is not a program: a name declared twice,
    /// `self` outside a method, an `op` where no `op` may go, a parameter count
    /// that does not match the operation.
    ///
    /// Separate from [`ErrorKind::Syntax`] because the grammar is satisfied and
    /// the rule broken is about names and where they may appear — the same
    /// reason [`ErrorKind::Attr`] is separate from [`ErrorKind::Type`]. Telling
    /// someone their syntax is wrong when it parsed sends them looking in the
    /// wrong place.
    Declaration,
}

impl ErrorKind {
    /// The class an error of this kind reifies into, or `None` if it can never
    /// be caught.
    ///
    /// Kept beside the variants rather than in the interpreter so that adding a
    /// kind and forgetting to bind its class is a compile error in one file
    /// instead of a missing global discovered by a `catch`.
    ///
    /// The `None` arm is the whole compile-time story. `compile` runs to
    /// completion before `Interp::run` is called, so a `catch` cannot be
    /// executing when a syntax or declaration error is raised — there is no
    /// frame to unwind to. Binding classes for them anyway would let someone
    /// write `catch e: SyntaxError` and get a clause the language has no way to
    /// say can never fire. The day `import` compiles a module part-way through a
    /// run, these gain a class and this arm shrinks; until then the invariant is
    /// "every *catchable* kind names a class", and this is where it is enforced.
    pub fn class_name(&self) -> Option<&'static str> {
        match self {
            ErrorKind::Syntax | ErrorKind::Declaration => None,
            // `Thrown` never reaches here — the payload carries its own class —
            // but the base is the honest answer rather than a panic.
            ErrorKind::Runtime | ErrorKind::Thrown => Some("Error"),
            ErrorKind::Type => Some("TypeError"),
            ErrorKind::Name => Some("NameError"),
            ErrorKind::Attr => Some("AttributeError"),
            ErrorKind::Value => Some("ValueError"),
            ErrorKind::Index => Some("IndexError"),
            ErrorKind::Key => Some("KeyError"),
            ErrorKind::Frozen => Some("FrozenError"),
            ErrorKind::Recursion => Some("RecursionError"),
            ErrorKind::ZeroDivision => Some("ZeroDivisionError"),
            ErrorKind::Overflow => Some("OverflowError"),
            ErrorKind::Import => Some("ImportError"),
            ErrorKind::Io => Some("IoError"),
        }
    }

    /// The word this kind reports itself as, inside `error[…]`.
    ///
    /// A different question from [`ErrorKind::class_name`], and the split is
    /// what lets the two compile-time kinds report a name without pretending to
    /// be catchable. Every other kind defers, so a class name is written once
    /// and the two answers cannot drift.
    pub fn code(&self) -> &'static str {
        match self {
            ErrorKind::Syntax => "SyntaxError",
            ErrorKind::Declaration => "DeclarationError",
            // The `None` arms are the two above, both matched already.
            kind => kind.class_name().unwrap_or("Error"),
        }
    }
}

/// Every kind's class, for the startup that has to bind them.
///
/// Maintained by hand for the same reason `class::BUILTINS` is: the mapping runs
/// from kind to name, and Rust cannot be asked to walk it backwards. A stale
/// entry here is a `catch` that cannot find its class, which is why
/// `every_listed_kind_names_a_distinct_class` exists.
///
/// The compile-time kinds are absent because they have no class to bind. That
/// they are exactly the kinds absent — rather than some of them being an
/// oversight — is what `only_the_uncatchable_kinds_are_unlisted` checks.
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
    ErrorKind::Import,
    ErrorKind::Io,
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
    /// The module this error's spans are measured against, when that is not the
    /// file the program was started from.
    ///
    /// A span is an offset into one text, and once a program is more than one
    /// file there is more than one text it could be an offset into. Without this
    /// an error raised inside an imported module would be drawn against the
    /// importer's source: the right numbers, the wrong file, a caret under
    /// whatever happened to be at that offset. That is precisely the defect the
    /// v0.5 diagnostics sweep existed to remove, so imports had to carry the
    /// answer with them rather than reintroduce it.
    ///
    /// `None` means the starting module, which the caller of [`report`] already
    /// holds the text of. Only the imported ones need saying.
    ///
    /// [`report`]: QuinceError::report
    pub origin: Option<Rc<ModuleSource>>,
}

/// The text of one module, and what to call it in a report.
///
/// Shared rather than copied per error: one module is imported once and may
/// raise many times, and an `Rc` here is what keeps a report from being a reason
/// to hold every source file twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSource {
    pub path: String,
    pub text: Rc<str>,
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
            origin: None,
        }
    }

    /// Records which module's text this error's spans belong to.
    ///
    /// Set on the way *out* — when an error escapes the execution of an imported
    /// module, or a call to a function that module declared — rather than at the
    /// raise. A raise site knows what went wrong and has no idea what file it is
    /// in; the frame it unwinds through knows exactly. The first one to set it
    /// wins, and that is the innermost, so an error crossing three modules is
    /// reported against the one that actually raised it.
    pub fn in_module(mut self, source: Rc<ModuleSource>) -> Self {
        if self.origin.is_none() {
            self.origin = Some(source);
        }
        self
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
            origin: None,
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

/// Computes Levenshtein distance between two strings for fuzzy matching suggestions.
pub fn lev_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b_chars.len() + 1]; a_chars.len() + 1];

    // Each dimension is one longer than its string, so `enumerate` walks exactly
    // the range the edit distance is defined over.
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
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
            names.push(
                kind.class_name()
                    .unwrap_or_else(|| panic!("{kind:?} is listed but names no class")),
            );
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

    /// The other direction of the same invariant.
    ///
    /// `every_listed_kind_names_a_distinct_class` checks that nothing listed is
    /// uncatchable. This checks that nothing unlisted is catchable, which is the
    /// half that would otherwise go wrong quietly: forgetting to add a new kind
    /// to `ERROR_KINDS` costs a `catch` that cannot find its class, and the
    /// symptom is a panic in `error_class` on the day someone first triggers it.
    #[test]
    fn only_the_uncatchable_kinds_are_unlisted() {
        for kind in [ErrorKind::Syntax, ErrorKind::Declaration] {
            assert_eq!(kind.class_name(), None, "{kind:?} should name no class");
            assert!(
                !ERROR_KINDS.contains(&kind),
                "{kind:?} names no class and must not be listed for binding"
            );
        }
        // Every kind reachable from a report. Written out rather than derived,
        // because the point is to fail when a variant is added and its
        // catchability never answered for.
        let all = [
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
            ErrorKind::Thrown,
            ErrorKind::Syntax,
            ErrorKind::Declaration,
        ];
        for kind in all {
            // `Thrown` is the one kind that names a class and is still not bound
            // from the list: it borrows the thrown instance's own.
            if kind == ErrorKind::Thrown {
                continue;
            }
            assert_eq!(
                kind.class_name().is_some(),
                ERROR_KINDS.contains(&kind),
                "{kind:?} disagrees about whether it can be caught"
            );
        }
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

    #[test]
    fn fuzzy_did_you_mean_suggestions() {
        let candidates = vec!["name", "age", "items", "count"];
        assert_eq!(did_you_mean("namme", candidates.clone()), Some("name"));
        assert_eq!(did_you_mean("cont", candidates.clone()), Some("count"));
        assert_eq!(did_you_mean("completely_unrelated", candidates), None);
    }
}

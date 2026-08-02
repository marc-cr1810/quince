//! The error every stage of the pipeline raises.
//!
//! One type for all of them, carrying the span that caused it and the kind that
//! says what sort of mistake it was. The kind lives in [`kind`], the rendering in
//! `render`, and the name-suggestion machinery every "no such thing" message
//! reaches for in [`suggest`] — this file is the error itself and the builders
//! that shape one.

pub mod kind;
mod render;
pub mod suggest;

pub use kind::{ERROR_KINDS, ErrorKind};
pub use suggest::{did_you_mean, lev_distance};

use std::rc::Rc;

use crate::color::Style;
use crate::runtime::heap::ObjId;
use crate::syntax::token::Span;

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

/// An error on its way up, boxed.
///
/// [`QuinceError`] is ~128 bytes and every stage of the pipeline returns one in
/// a `Result`, which means the evaluator's hot path moved the whole struct on
/// paths that overwhelmingly succeed. Boxing puts a pointer there instead, and
/// pays the allocation only when something actually goes wrong — which is the
/// same trade the struct's own doc comment makes about deferring rendering.
///
/// The builders take `Box<Self>`, so a raise site writes what it always wrote:
/// `QuinceError::new(…).with_kind(…)` builds a `Raised` directly and no call
/// site says `Box` out loud.
pub type Raised = Box<QuinceError>;

/// What every fallible stage of the pipeline answers with.
pub type Result<T> = std::result::Result<T, Raised>;

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
    pub fn new(message: impl Into<String>, span: Span) -> Raised {
        Box::new(QuinceError {
            message: message.into(),
            span,
            kind: ErrorKind::Runtime,
            payload: None,
            label: None,
            help: None,
            labels: Vec::new(),
            origin: None,
        })
    }

    /// Records which module's text this error's spans belong to.
    ///
    /// Set on the way *out* — when an error escapes the execution of an imported
    /// module, or a call to a function that module declared — rather than at the
    /// raise. A raise site knows what went wrong and has no idea what file it is
    /// in; the frame it unwinds through knows exactly. The first one to set it
    /// wins, and that is the innermost, so an error crossing three modules is
    /// reported against the one that actually raised it.
    pub fn in_module(mut self: Raised, source: Rc<ModuleSource>) -> Raised {
        if self.origin.is_none() {
            self.origin = Some(source);
        }
        self
    }

    /// Classifies an error, as a builder so a raise site adds one line.
    pub fn with_kind(mut self: Raised, kind: ErrorKind) -> Raised {
        self.kind = kind;
        self
    }

    /// Attaches advice, as a builder for the same reason as [`with_kind`].
    ///
    /// [`with_kind`]: QuinceError::with_kind
    pub fn with_help(mut self: Raised, help: impl Into<String>) -> Raised {
        self.help = Some(help.into());
        self
    }

    /// Attaches a sub-labeled span for diagnostic tree annotations.
    pub fn with_label(mut self: Raised, span: Span, label: impl Into<String>) -> Raised {
        self.labels.push(LabeledSpan::new(span, label));
        self
    }

    /// Attaches multiple sub-labeled spans.
    pub fn with_labels(mut self: Raised, labels: impl IntoIterator<Item = LabeledSpan>) -> Raised {
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
    ) -> Raised {
        Box::new(QuinceError {
            message: message.into(),
            span,
            kind: ErrorKind::Thrown,
            payload: Some(payload),
            label: Some(class.into()),
            help: None,
            labels: Vec::new(),
            origin: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_error_is_unclassified_and_carries_no_payload() {
        let err = QuinceError::new("boom", Span::new(0, 1));
        assert_eq!(err.kind, ErrorKind::Runtime);
        assert_eq!(err.payload, None);
    }

    /// The reason [`Raised`] exists, stated as a number.
    ///
    /// A `Result` is as large as its larger arm, and this one is returned from
    /// every stage of the pipeline on paths that overwhelmingly succeed — so the
    /// error arm sets what the success path pays. Unboxed, [`QuinceError`] was
    /// over 128 bytes and every one of those `Result`s carried it.
    ///
    /// Asserted rather than described because the cost is invisible at the call
    /// sites that pay it: nothing in `eval` mentions the error's size, and a
    /// field added to [`QuinceError`] in good faith would put it back without
    /// anything complaining. This is what complains.
    #[test]
    fn the_error_travels_as_a_pointer_rather_than_a_struct() {
        assert_eq!(size_of::<Raised>(), size_of::<*const ()>());
        assert!(
            size_of::<Result<()>>() <= 16,
            "the pipeline's Result grew to {} bytes — has the error stopped being boxed?",
            size_of::<Result<()>>()
        );
    }
}

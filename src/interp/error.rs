//! Raising, catching, and describing a failure.
//!
//! `throw` turning a Quince instance into a Rust error, `catch` turning one back
//! into an instance, and the shared constructors for the mistakes the evaluator
//! reports on its own behalf.

use std::rc::Rc;

use crate::error::{ErrorKind, LabeledSpan, QuinceError, Raised, Result};
use crate::interp::{Interp, KIND, MESSAGE};
use crate::runtime::class::Instance;
use crate::runtime::dict::{Dict, Key};
use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{BinaryOp, TypeExpr};
use crate::syntax::token::Span;

/// Refuses a value that does not hold as the annotation it was checked against.
///
/// One constructor for all four boundaries — a binding, an argument, a return,
/// and a field — because they are one mistake and should read as one. `what`
/// names the boundary, so the sentence says where rather than making the caret
/// carry it alone.
pub(crate) fn does_not_hold(
    heap: &Heap,
    ty: &TypeExpr,
    value: &Value,
    what: &str,
    span: Span,
) -> Raised {
    let (message, help) = crate::sema::types::refusal(ty, value, heap, what);
    let mut err = QuinceError::new(message, span).with_kind(ErrorKind::Type);
    // The annotation, when it is somewhere the caret is not. Two marks on one
    // line — what turned up, and what was required — save the reader looking
    // for the declaration that made this a mistake.
    //
    // The offending value keeps a bare underline rather than a label of its
    // own. Supplying any label at all replaces the plain caret a report would
    // otherwise draw, so the value has to be listed explicitly to stay marked —
    // and it is listed unlabelled, because the message one line up already says
    // what it is and every other diagnostic in the language marks its subject
    // exactly this way.
    if ty.span != span && ty.span.end > ty.span.start {
        err = err.with_labels([
            LabeledSpan::unlabelled(span),
            LabeledSpan::new(ty.span, format!("declared `{}` here", ty.written())),
        ]);
    }
    match help {
        Some(help) => err.with_help(help),
        None => err,
    }
}

impl Interp {
    /// Raises `value`, which has to be an instance of `Error`.
    ///
    /// Checked here rather than at the `catch` so the error names the mistake.
    /// Allowing anything to be thrown would mean a handler binding a bare `int`,
    /// and the failure would surface as a missing field on it — a complaint about
    /// `int` methods, several lines from the `throw` that caused it, with the
    /// thrown value nowhere in the message.
    ///
    /// It also keeps a promise worth having: everything a handler binds has a
    /// `message` and a `kind`, because everything it binds extends `Error`.
    /// Returns the error to raise rather than raising it, because the caller is
    /// the one that owes an `Err` either way — evaluating the operand can fail on
    /// its own, and those two failures must not be confused for each other.
    pub(super) fn throw(&mut self, raised: Value, span: Span) -> Raised {
        let Value::Instance(id) = raised else {
            return QuinceError::new(
                format!(
                    "`throw` needs an instance of `Error`, but was given {}",
                    raised.type_name(&self.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "everything a `catch` binds has a `message` and a `kind`, because everything it \
                 binds extends `Error` — wrap the value in one",
            );
        };

        let class = self.heap.instance(id).class;
        if !self.descends_from_error(class) {
            return QuinceError::new(
                format!(
                    "`throw` needs an instance of `Error`, but `{}` does not extend it",
                    self.heap.class(class).name
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "write `extends Error` on it, so a handler binding it finds the `message` and \
                 `kind` every caught value has",
            );
        }

        // Both read for the report an uncaught throw prints. The class is what it
        // reports *as*, so a `ParseError` says its own name rather than `Error`.
        //
        // A subclass that overrides `init` without calling `super.init` has no
        // `message`, so the class name stands in — a worse message, but never a
        // second error raised while reporting the first one.
        let name = self.heap.class(class).name.clone();
        let message = match self
            .heap
            .instance(id)
            .fields
            .get(&Key::Str(Rc::from(MESSAGE)))
        {
            Some(Value::Str(message)) => message.to_string(),
            Some(other) => other.display_base(&self.heap),
            None => name.clone(),
        };
        QuinceError::thrown(id, name, message, span)
    }

    /// Turns an error into the value a handler binds.
    ///
    /// The one place an error becomes a Quince object, and the only place that
    /// has the `&mut Heap` to build one — which is the whole reason raising stays
    /// cheap. Half the raise sites hold `&Heap`, and an uncaught error is about
    /// to be printed and discarded, so an error that nobody catches allocates
    /// nothing at all.
    ///
    /// Allocates but reaches no safe point, so the instance is safe unrooted
    /// until the caller stores it.
    pub(super) fn reify(&mut self, err: &QuinceError) -> Value {
        // A `throw` already built its instance, and the handler binds that same
        // object unchanged and unwrapped — which is what makes a user's own
        // fields survive the round trip.
        if let Some(id) = err.payload {
            return Value::Instance(id);
        }

        let class = self.error_class(err.kind);
        let mut fields = Dict::new();
        fields.insert(
            Key::Str(Rc::from(MESSAGE)),
            Value::from(err.message.as_str()),
        );
        // Set directly rather than by calling `init`, because this is the runtime
        // building the object rather than a program asking for one. The values
        // match what `Error.init` would have produced.
        // `error_class` above has already refused any kind that names no class,
        // so reaching here means there is one.
        let kind_name = err.kind.class_name().expect("a reified kind names a class");
        fields.insert(Key::Str(Rc::from(KIND)), Value::from(kind_name));
        Value::Instance(self.heap.alloc(Object::Instance(Instance {
            class,
            fields,
            // `Error` extends nothing, so nothing in the chain a user's own error
            // class sits on has a payload to fill.
            payload: None,
        })))
    }

    /// Whether `class` is `Error` or descends from it.
    ///
    /// Walks the same parent chain `Class::method` does, and terminates for
    /// the same reason: a parent is evaluated before the class naming it is
    /// bound, so the chain cannot contain a cycle.
    pub(super) fn descends_from_error(&self, class: ObjId) -> bool {
        let base = self.error_class(ErrorKind::Runtime);
        let mut at = Some(class);
        while let Some(id) = at {
            if id == base {
                return true;
            }
            at = self.heap.class(id).parent;
        }
        false
    }
}

/// The error for a mutation the heap refused.
///
/// It names `const` rather than saying only "frozen", because freezing has
/// exactly one cause in the language and the reader's next question is always
/// what did this. The value it names may be several steps from the `const` that
/// froze it — that is what "deeply" means.
pub(crate) fn frozen(heap: &Heap, value: &Value, span: Span) -> Raised {
    let what = value.type_name(heap);
    QuinceError::new(format!("cannot modify `const` {what}"), span)
        .with_kind(ErrorKind::Frozen)
        .with_help(
            "`const` freezes a value deeply, and through every other name that already \
             reaches it — bind it with `final` to fix the name and leave the value writable",
        )
}

pub(crate) fn type_error(
    heap: &Heap,
    op: BinaryOp,
    lhs: &Value,
    rhs: &Value,
    lhs_span: Span,
    rhs_span: Span,
    expr_span: Span,
) -> Raised {
    use BinaryOp::*;
    let verb = match op {
        Add => "add",
        Sub => "subtract",
        Mul => "multiply",
        Div | FloorDiv => "divide",
        Rem => "take the remainder of",
        BitAnd | BitOr | BitXor => "combine the bits of",
        Shl | Shr => "shift",
        Lt | Le | Gt | Ge => "compare",
        Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
    };

    let op_span = if lhs_span.end <= rhs_span.start {
        Span::new(lhs_span.end as usize, rhs_span.start as usize)
    } else {
        expr_span
    };

    QuinceError::new(
        format!(
            "cannot {verb} {} and {}",
            lhs.type_name(heap),
            rhs.type_name(heap)
        ),
        expr_span,
    )
    .with_kind(ErrorKind::Type)
    .with_label(lhs_span, lhs.type_name(heap))
    .with_label(op_span, "doesn't support these values")
    .with_label(rhs_span, rhs.type_name(heap))
    .with_help(format!(
        "Change {} or {} to be compatible types and try again",
        lhs.type_name(heap),
        rhs.type_name(heap)
    ))
}

pub(crate) fn check_arity(name: &str, expected: usize, found: usize, span: Span) -> Result<()> {
    if expected == found {
        return Ok(());
    }
    let plural = if expected == 1 { "" } else { "s" };
    let help = if found > expected {
        let diff = found - expected;
        format!(
            "remove {diff} argument{} to match `{name}`'s signature",
            if diff == 1 { "" } else { "s" }
        )
    } else {
        let diff = expected - found;
        format!(
            "add {diff} argument{} to match `{name}`'s signature",
            if diff == 1 { "" } else { "s" }
        )
    };
    Err(QuinceError::new(
        format!("`{name}` takes {expected} argument{plural}, but {found} were given"),
        span,
    )
    .with_kind(ErrorKind::Type)
    .with_help(help))
}

// -- wording ---------------------------------------------------------------

/// A type name as it reads in a message, so one reads as English rather than as
/// a template.
///
/// `nil` takes no article: it is the one type name that is also the name of its
/// only value, so "from nil" names the value and "from a nil" names nothing.
pub(crate) fn an(noun: &str) -> String {
    if noun == crate::runtime::class::Builtin::Nil.name() {
        return noun.to_string();
    }
    let article = match noun.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => "an",
        false => "a",
    };
    format!("{article} {noun}")
}

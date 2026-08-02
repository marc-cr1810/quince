//! `string`'s methods.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError};
use crate::runtime::class::Builtin;
use crate::runtime::heap::{Heap, Object};
use crate::runtime::value::{Native, Value};
use crate::syntax::token::Span;

/// The receiver of a string method.
///
/// Dispatch guarantees the type: the method was reached through
/// `class::STR`'s table, which nothing but a string can name.
pub(super) fn text(args: &[Value]) -> &Rc<str> {
    match &args[0] {
        Value::Str(text) => text,
        other => unreachable!("a string method received {other:?}"),
    }
}

/// A string argument, or an error naming what arrived instead.
pub(super) fn text_arg(
    heap: &Heap,
    args: &[Value],
    at: usize,
    name: &str,
    span: Span,
) -> Result<Rc<str>, QuinceError> {
    match &args[at] {
        Value::Str(text) => Ok(Rc::clone(text)),
        other => Err(QuinceError::new(
            format!(
                "`{name}` needs a string, but was given {}",
                other.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    }
}

/// `"ab".repeat(3)` — the string joined to itself that many times.
///
/// Refuses a negative count rather than answering with an empty string, on the
/// same reasoning as `sqrt(-1)`: a program asking for `-2` copies has a bug
/// upstream, and an empty string would carry it further before anything noticed.
/// Zero copies is a real answer and is allowed.
pub static REPEAT: Native = Native {
    name: "repeat",
    arity: Some(2),
    params: &["times"],
    returns: Some(Builtin::Str),
    doc: "The string written `n` times, end to end.",
    func: |interp, args, span| {
        let Value::Int(count) = args[1].base(&interp.heap) else {
            return Err(QuinceError::new(
                format!(
                    "`repeat` needs an int, but was given {}",
                    args[1].type_name(&interp.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type));
        };
        let count = *count;
        if count < 0 {
            return Err(
                QuinceError::new(format!("cannot repeat a string {count} times"), span)
                    .with_kind(ErrorKind::Value)
                    .with_help("a count of copies is not negative"),
            );
        }
        Ok(Value::Str(Rc::from(
            text(args).repeat(count as usize).as_str(),
        )))
    },
};

pub static UPPER: Native = Native {
    name: "upper",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::Str),
    doc: "The string with every character in upper case.",
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).to_uppercase()))),
};

pub static LOWER: Native = Native {
    name: "lower",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::Str),
    doc: "The string with every character in lower case.",
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).to_lowercase()))),
};

pub static TRIM: Native = Native {
    name: "trim",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::Str),
    doc: "The string without leading or trailing whitespace.",
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).trim()))),
};

pub static STARTS_WITH: Native = Native {
    name: "starts_with",
    arity: Some(2),
    params: &["prefix"],
    returns: Some(Builtin::Bool),
    doc: "Whether the string begins with `prefix`.",
    func: |interp, args, span| {
        let prefix = text_arg(&interp.heap, args, 1, "starts_with", span)?;
        Ok(Value::Bool(text(args).starts_with(prefix.as_ref())))
    },
};

pub static ENDS_WITH: Native = Native {
    name: "ends_with",
    arity: Some(2),
    params: &["suffix"],
    returns: Some(Builtin::Bool),
    doc: "Whether the string ends with `suffix`.",
    func: |interp, args, span| {
        let suffix = text_arg(&interp.heap, args, 1, "ends_with", span)?;
        Ok(Value::Bool(text(args).ends_with(suffix.as_ref())))
    },
};

pub static REPLACE: Native = Native {
    name: "replace",
    arity: Some(3),
    params: &["from", "to"],
    returns: Some(Builtin::Str),
    doc: "The string with every `from` replaced by `to`.",
    func: |interp, args, span| {
        let from = text_arg(&interp.heap, args, 1, "replace", span)?;
        if from.is_empty() {
            // `Value` and not `Type`: a string is exactly what `replace` accepts,
            // and that particular string is the mistake.
            return Err(QuinceError::new(
                "`replace` needs something to look for, but was given \"\"".to_string(),
                span,
            )
            .with_kind(ErrorKind::Value));
        }
        let to = text_arg(&interp.heap, args, 2, "replace", span)?;
        Ok(Value::Str(Rc::from(
            text(args).replace(from.as_ref(), to.as_ref()),
        )))
    },
};

/// Splitting on `""` is an error rather than yielding the characters, because
/// the two are different requests and `chars` already answers the second one.
pub static SPLIT: Native = Native {
    name: "split",
    arity: Some(2),
    params: &["separator"],
    returns: Some(Builtin::List),
    doc: "The string cut at every `separator`, as a list of strings. The separator is not kept, and it may not be empty — use `chars`.",
    func: |interp, args, span| {
        let sep = text_arg(&interp.heap, args, 1, "split", span)?;
        if sep.is_empty() {
            return Err(QuinceError::new(
                "`split` needs a separator, but was given \"\" — use `chars` instead".to_string(),
                span,
            )
            .with_kind(ErrorKind::Value));
        }
        let parts: Vec<Value> = text(args)
            .split(sep.as_ref())
            .map(|part| Value::Str(Rc::from(part)))
            .collect();
        Ok(Value::List(interp.heap.alloc(Object::List(parts))))
    },
};

pub static CHARS: Native = Native {
    name: "chars",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::List),
    doc: "The string's characters, each as a string of its own.",
    func: |interp, args, _span| {
        let chars: Vec<Value> = text(args)
            .chars()
            .map(|c| Value::Str(Rc::from(c.to_string())))
            .collect();
        Ok(Value::List(interp.heap.alloc(Object::List(chars))))
    },
};

/// The separator is the receiver, as in Python: `", ".join(xs)`. Reads badly
/// once, but it is the string that decides how the pieces are put together.
pub static JOIN: Native = Native {
    name: "join",
    arity: Some(2),
    params: &["items"],
    returns: Some(Builtin::Str),
    doc: "The list's items rendered and joined with the string between them.",
    func: |interp, args, span| {
        let Value::List(id) = &args[1] else {
            return Err(QuinceError::new(
                format!(
                    "`join` needs a list, but was given {}",
                    args[1].type_name(&interp.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type));
        };

        let mut parts = Vec::with_capacity(interp.heap.list(*id).len());
        for item in interp.heap.list(*id) {
            match item {
                Value::Str(part) => parts.push(Rc::clone(part)),
                other => {
                    return Err(QuinceError::new(
                        format!(
                            "`join` needs a list of strings, but found {}",
                            other.type_name(&interp.heap)
                        ),
                        span,
                    )
                    .with_kind(ErrorKind::Type));
                }
            }
        }
        Ok(Value::Str(Rc::from(parts.join(text(args)))))
    },
};

//! Building a builtin from a value: `int("42")`, `list(x)`, `string(x)`.
//!
//! A builtin type's `op init`, reached by calling the type. These take no receiver,
//! unlike every other native — there is nothing to receive, because the value they
//! return *is* what the construction produced. See `Class::init` and
//! `Interp::construct_builtin`.
//!
//! Two error kinds, and the split is deliberate. `ErrorKind::Type` is for an
//! argument the conversion never accepts, where the fix is at the call: `int([1])`.
//! `ErrorKind::Value` is for one it does accept carrying data it cannot use, where
//! the call is right and the data is not: `int("abc")`. Naming that difference is
//! the whole reason `Value` was added as a kind.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::interp::error::an;
use crate::interp::show::Ask;
use crate::runtime::class::Builtin;
use crate::runtime::dict::Dict;
use crate::runtime::heap::{Heap, Object};
use crate::runtime::value::{Native, Value};
use crate::syntax::token::Span;

/// The argument named as a type, for a conversion that cannot accept it at all.
pub(super) fn not_convertible(heap: &Heap, to: &str, value: &Value, span: Span) -> Raised {
    QuinceError::new(
        format!("cannot make {} from {}", an(to), an(value.type_name(heap))),
        span,
    )
    .with_kind(ErrorKind::Type)
}


/// Rejects a float that no integer can represent, which `as` would otherwise
/// answer with a saturated bound — silently, and almost never usefully.
pub(crate) fn checked_trunc(f: f64, span: Span) -> Result<i64> {
    if f.is_nan() {
        return Err(
            QuinceError::new("cannot make an int from NaN", span).with_kind(ErrorKind::Value)
        );
    }
    // Compared before truncating: truncation of a value this large is a no-op,
    // so doing it first would not bring anything into range.
    //
    // The two comparisons are deliberately not symmetric. `i64::MIN` is -2^63,
    // which an `f64` holds exactly, so a float equal to it converts. `i64::MAX`
    // is 2^63-1, which an `f64` cannot hold — `i64::MAX as f64` rounds *up* to
    // 2^63 — so a float that equal is already out of range, and `>` rather than
    // `>=` here would let it through to saturate silently.
    if f < i64::MIN as f64 || f >= i64::MAX as f64 {
        return Err(
            QuinceError::new(format!("{f} is too large to be an int"), span)
                .with_kind(ErrorKind::Overflow),
        );
    }
    Ok(f.trunc() as i64)
}

/// Named for the type rather than for `init`, because this native's name is what
/// an arity error quotes and `int(1, 2)` should be told about `int`.
pub static INT_INIT: Native = Native {
    name: "int",
    arity: Some(1),
    params: &["value"],
    returns: Some(Builtin::Int),
    doc: "An int made from `value`. A float truncates toward zero, a string is parsed, and a bool is `1` or `0`.",
    // Dispatching on the base, so a class extending `int` converts as the int it
    // is, while the message still names the class the line was written with.
    func: |interp, args, span| match &args[0].base(&interp.heap).clone() {
        Value::Int(n) => Ok(Value::Int(*n)),
        // Toward zero, unlike `//`, which floors. `int` follows the same rule as
        // Python and Rust's `as`; `//` deliberately does not, so that `-7 // 2`
        // stays the mathematical quotient.
        Value::Float(f) => Ok(Value::Int(checked_trunc(*f, span)?)),
        // Surrounding whitespace is dropped, since a number read from a file or
        // a prompt arrives with it and stripping is what the caller would do.
        Value::Str(text) => text.trim().parse::<i64>().map(Value::Int).map_err(|_| {
            QuinceError::new(
                format!("cannot make an int from {}", args[0].repr_base(&interp.heap)),
                span,
            )
            .with_kind(ErrorKind::Value)
        }),
        // `1` and `0`, as every language with this conversion has it. This is not
        // a crack in the rule that a bool is not a number: `1 + true` stays an
        // error, because nobody asked for a conversion there.
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        other => Err(not_convertible(&interp.heap, "int", other, span)),
    },
};

pub static FLOAT_INIT: Native = Native {
    name: "float",
    arity: Some(1),
    params: &["value"],
    returns: Some(Builtin::Float),
    doc: "A float made from `value`.",
    func: |interp, args, span| match &args[0].base(&interp.heap).clone() {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(text) => text.trim().parse::<f64>().map(Value::Float).map_err(|_| {
            QuinceError::new(
                format!("cannot make a float from {}", args[0].repr_base(&interp.heap)),
                span,
            )
            .with_kind(ErrorKind::Value)
        }),
        Value::Bool(b) => Ok(Value::Float(*b as i64 as f64)),
        other => Err(not_convertible(&interp.heap, "float", other, span)),
    },
};

/// Total, and the same text `print` writes — there is no value without a
/// rendering, so this cannot fail and needs no error kind at all.
pub static STR_INIT: Native = Native {
    name: "string",
    arity: Some(1),
    params: &["value"],
    returns: Some(Builtin::Str),
    doc: "The value rendered as a string, the same way `print` writes it. A class may answer for itself with `op string`.",
    func: |interp, args, _span| {
        let text = interp.display(&args[0], Ask::Class)?;
        Ok(Value::Str(Rc::from(text)))
    },
};

/// Exactly the test `if` applies, exposed as a value. Also total.
pub static BOOL_INIT: Native = Native {
    name: "bool",
    arity: Some(1),
    params: &["value"],
    returns: Some(Builtin::Bool),
    doc: "Whether `value` is truthy. A class may answer for itself with `op bool`.",
    func: |interp, args, _span| Ok(Value::Bool(interp.is_truthy(&args[0])?)),
};

/// `list()` is empty and `list(xs)` copies. Nothing else converts: `list("ab")`
/// could mean characters, and `list({"a": 1})` could mean keys, entries, or
/// values. Both are refused rather than guessed at, with the method that means
/// the likely thing named in the help.
pub static LIST_INIT: Native = Native {
    name: "list",
    arity: None,
    params: &["items"],
    returns: Some(Builtin::List),
    doc: "A new list, empty or holding what `value` iterates to.",
    func: |interp, args, span| match args {
        [] => Ok(Value::List(interp.heap.alloc(Object::List(Vec::new())))),
        [only] => match only.base(&interp.heap).clone() {
            // Shallow, as in Python: the new list holds the same elements rather
            // than copies of them, so a nested list stays shared.
            Value::List(id) => {
                let items = interp.heap.list(id).clone();
                Ok(Value::List(interp.heap.alloc(Object::List(items))))
            }
            Value::Str(_) => Err(not_convertible(&interp.heap, "list", only, span)
                .with_help("`chars` splits a string into its characters")),
            Value::Dict(_) => Err(not_convertible(&interp.heap, "list", only, span)
                .with_help("`keys` or `values` picks which half of a dict to take")),
            _ => Err(not_convertible(&interp.heap, "list", only, span)),
        },
        _ => Err(too_many("list", args.len(), span)),
    },
};

pub static DICT_INIT: Native = Native {
    name: "dict",
    arity: None,
    params: &["entries"],
    returns: Some(Builtin::Dict),
    doc: "A new dict, empty or built from `value`.",
    func: |interp, args, span| match args {
        [] => Ok(Value::Dict(interp.heap.alloc(Object::Dict(Dict::new())))),
        [only] => match only.base(&interp.heap).clone() {
            Value::Dict(id) => {
                let entries = interp.heap.dict(id).clone();
                Ok(Value::Dict(interp.heap.alloc(Object::Dict(entries))))
            }
            _ => Err(not_convertible(&interp.heap, "dict", only, span)),
        },
        _ => Err(too_many("dict", args.len(), span)),
    },
};

/// `check_arity` states one exact count, which these two conversions do not
/// have — they take nothing or one thing.
pub(super) fn too_many(name: &str, found: usize, span: Span) -> Raised {
    QuinceError::new(
        format!("`{name}` takes 0 or 1 arguments, but {found} were given"),
        span,
    )
    .with_kind(ErrorKind::Type)
}

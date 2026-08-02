//! `math` — the numeric functions, and the two that pick between values.

use crate::error::{ErrorKind, QuinceError, Result};
use crate::runtime::class::Builtin;
use crate::runtime::heap::Heap;
use crate::runtime::value::{Native, Value};
use crate::syntax::token::Span;

use super::{Member, Module};


pub static MATH: Module = Module {
    name: "math",
    members: &[
        ("pi", Member::Const(|| Value::Float(std::f64::consts::PI))),
        ("e", Member::Const(|| Value::Float(std::f64::consts::E))),
        ("floor", Member::Fn(&FLOOR)),
        ("ceil", Member::Fn(&CEIL)),
        ("round", Member::Fn(&ROUND)),
        ("abs", Member::Fn(&ABS)),
        ("sqrt", Member::Fn(&SQRT)),
        ("pow", Member::Fn(&POW)),
        ("min", Member::Fn(&MIN)),
        ("max", Member::Fn(&MAX)),
    ],
};

/// The number `name` was given, as an `f64`.
///
/// Ints are accepted everywhere a float is, which is the same latitude `+` gives
/// them. The payload is unwrapped first, so a `class Celsius extends float` can
/// be handed to `math.floor` and get an answer about the float it is.
pub(super) fn number(name: &str, args: &[Value], heap: &Heap, span: Span) -> Result<f64> {
    match args[0].base(heap) {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(n) => Ok(*n),
        other => Err(QuinceError::new(
            format!(
                "`{name}` needs a number, but was given {}",
                other.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    }
}

/// Rounding answers with an int, because that is what it is for: `math.floor(x)`
/// is written to get something to index or count with, and a `float` that
/// happens to have no fractional part would have to be converted at every call
/// site. `abs`, by contrast, gives back the kind it was given — the magnitude of
/// a float is a float.
static FLOOR: Native = Native {
    name: "floor",
    arity: Some(1),
    params: &["n"],
    returns: Some(Builtin::Int),
    doc: "The largest integer that is not greater than `n`.",
    func: |interp, args, span| {
        Ok(Value::Int(
            number("floor", args, &interp.heap, span)?.floor() as i64
        ))
    },
};

static CEIL: Native = Native {
    name: "ceil",
    arity: Some(1),
    params: &["n"],
    returns: Some(Builtin::Int),
    doc: "The smallest integer that is not less than `n`.",
    func: |interp, args, span| {
        Ok(Value::Int(
            number("ceil", args, &interp.heap, span)?.ceil() as i64
        ))
    },
};

static ROUND: Native = Native {
    name: "round",
    arity: Some(1),
    params: &["n"],
    returns: Some(Builtin::Int),
    doc: "`n` rounded to the nearest integer, halves away from zero.",
    func: |interp, args, span| {
        Ok(Value::Int(
            number("round", args, &interp.heap, span)?.round() as i64
        ))
    },
};

static ABS: Native = Native {
    name: "abs",
    arity: Some(1),
    params: &["n"],
    returns: None,
    doc: "The magnitude of `n`, keeping the type it was given: an int stays an int.",
    func: |interp, args, span| match args[0].base(&interp.heap) {
        Value::Int(n) => Ok(Value::Int(n.abs())),
        _ => Ok(Value::Float(number("abs", args, &interp.heap, span)?.abs())),
    },
};

static SQRT: Native = Native {
    name: "sqrt",
    arity: Some(1),
    params: &["n"],
    returns: Some(Builtin::Float),
    doc: "The square root of `n`. Refused for a negative `n` rather than answered with a NaN.",
    func: |interp, args, span| {
        let n = number("sqrt", args, &interp.heap, span)?;
        // Refused rather than answered with a NaN. There is no complex number to
        // hand back and no NaN literal to compare one against, so a NaN here
        // would travel until it reached something that printed it — which is the
        // failure at a distance `try`/`catch` exists to replace.
        if n < 0.0 {
            // Named through the base renderer rather than as the `f64` it was
            // widened to, so a message about `sqrt(-1)` says `-1` and one about
            // `sqrt(-1.0)` says `-1.0`. What the caller wrote is what they will
            // go looking for.
            return Err(QuinceError::new(
                format!(
                    "`sqrt` is not defined for {}",
                    args[0].repr_base(&interp.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Value)
            .with_help("take the square root of a number that is not negative"));
        }
        Ok(Value::Float(n.sqrt()))
    },
};

static POW: Native = Native {
    name: "pow",
    arity: Some(2),
    params: &["base", "exponent"],
    returns: Some(Builtin::Float),
    doc: "`base` raised to `exponent`, always as a float.",
    func: |interp, args, span| {
        let base = number("pow", args, &interp.heap, span)?;
        let exponent = number("pow", &args[1..], &interp.heap, span)?;
        Ok(Value::Float(base.powf(exponent)))
    },
};

/// `min` and `max` compare two numbers and nothing else — not a list, and not
/// two values of a class that defines `op cmp`. Widening them is a decision
/// about which of the two spellings is the real one, and it can be made when
/// there is a program that wants it.
static MIN: Native = Native {
    name: "min",
    arity: Some(2),
    params: &["a", "b"],
    returns: None,
    doc: "The smaller of two numbers, keeping the type of whichever won.",
    func: |interp, args, span| pick(args, &interp.heap, span, "min"),
};

static MAX: Native = Native {
    name: "max",
    arity: Some(2),
    params: &["a", "b"],
    returns: None,
    doc: "The larger of two numbers, keeping the type of whichever won.",
    func: |interp, args, span| pick(args, &interp.heap, span, "max"),
};

/// Keeps the int-ness of whichever argument wins, so `math.min(1, 2)` is an int
/// and not `1.0`.
fn pick(args: &[Value], heap: &Heap, span: Span, name: &str) -> Result<Value> {
    let left = number(name, args, heap, span)?;
    let right = number(name, &args[1..], heap, span)?;
    let take_left = if name == "min" {
        left <= right
    } else {
        left >= right
    };
    Ok(match take_left {
        true => args[0].base(heap).clone(),
        false => args[1].base(heap).clone(),
    })
}

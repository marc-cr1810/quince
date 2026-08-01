//! The modules a program can import without a file to import them from.
//!
//! A stdlib module is an ordinary [`Globals`] holding ordinary [`Native`]s — the
//! same object an imported file produces, built from a table instead of by
//! running statements. Nothing downstream can tell the two apart, which is the
//! property worth keeping: `math.floor` and `util.helper` are one lookup, and
//! `from math import floor` and `from util import helper` are one code path.
//!
//! **A member takes no receiver.** A module hands its names back unbound — see
//! the module arm of `Interp::attr` — so `math.floor(2.5)` calls `FLOOR` with
//! one argument and `arity` is `Some(1)`. This is the opposite of the natives in
//! `interp.rs` seeded onto a type, where `upper` is `Some(1)` because `args[0]`
//! is the string it upper-cases. The difference is real and it is the reason
//! these live in their own file rather than beside those.
//!
//! Nothing here is bound until it is imported. That is the whole reason `import`
//! went in before the library did: a name in the global scope is a name a
//! program can never use again, and `floor`, `ceil`, `round`, `abs`, `sqrt`,
//! `pow`, `min` and `max` would have been eight of them for one domain.

use crate::env::Globals;
use crate::error::{ErrorKind, QuinceError};
use crate::heap::{Heap, ObjId, Object};
use crate::token::Span;
use crate::value::{Native, Value};

/// One module the language ships.
pub struct Module {
    pub name: &'static str,
    /// What the module declares. Every entry is immutable — a program may not
    /// reassign `math.pi`, for the same reason it may not reassign `print`.
    pub members: &'static [(&'static str, Member)],
}

/// A name a stdlib module declares.
pub enum Member {
    Fn(&'static Native),
    /// A constant, built each time a module is, because a `Value` cannot be a
    /// `const` when it may hold a handle. None do yet.
    Const(fn() -> Value),
}

/// Every module `import` can find without looking at the filesystem.
///
/// Also the list of names a file may not take: `import math` must not change
/// meaning because someone dropped a `math.qn` beside their program, so this
/// wins and the collision is reported. Small and fixed is what makes that a
/// reasonable rule rather than a trap.
pub static MODULES: &[&Module] = &[&MATH];

/// The stdlib module called `name`, if there is one.
pub fn module_named(name: &str) -> Option<&'static Module> {
    MODULES.iter().copied().find(|module| module.name == name)
}

/// Builds `module`'s scope in the heap.
///
/// Called once per module per interpreter; the caller caches the result, which
/// is what makes `import math` in two files the same object both times.
pub fn build(module: &Module, heap: &mut Heap) -> ObjId {
    let mut globals = Globals::module(module.name, None);
    for (name, member) in module.members {
        let value = match member {
            Member::Fn(native) => Value::Native(native),
            Member::Const(build) => build(),
        };
        globals.declare(*name, value, false);
    }
    heap.alloc(Object::Globals(globals))
}

// -- math --------------------------------------------------------------------

static MATH: Module = Module {
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
fn number(name: &str, args: &[Value], heap: &crate::heap::Heap, span: Span) -> Result<f64, QuinceError> {
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
    func: |interp, args, span| {
        Ok(Value::Int(
            number("floor", args, &interp.heap, span)?.floor() as i64
        ))
    },
};

static CEIL: Native = Native {
    name: "ceil",
    arity: Some(1),
    func: |interp, args, span| {
        Ok(Value::Int(
            number("ceil", args, &interp.heap, span)?.ceil() as i64
        ))
    },
};

static ROUND: Native = Native {
    name: "round",
    arity: Some(1),
    func: |interp, args, span| {
        Ok(Value::Int(
            number("round", args, &interp.heap, span)?.round() as i64
        ))
    },
};

static ABS: Native = Native {
    name: "abs",
    arity: Some(1),
    func: |interp, args, span| match args[0].base(&interp.heap) {
        Value::Int(n) => Ok(Value::Int(n.abs())),
        _ => Ok(Value::Float(number("abs", args, &interp.heap, span)?.abs())),
    },
};

static SQRT: Native = Native {
    name: "sqrt",
    arity: Some(1),
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
    func: |interp, args, span| pick(args, &interp.heap, span, "min"),
};

static MAX: Native = Native {
    name: "max",
    arity: Some(2),
    func: |interp, args, span| pick(args, &interp.heap, span, "max"),
};

/// Keeps the int-ness of whichever argument wins, so `math.min(1, 2)` is an int
/// and not `1.0`.
fn pick(args: &[Value], heap: &Heap, span: Span, name: &str) -> Result<Value, QuinceError> {
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

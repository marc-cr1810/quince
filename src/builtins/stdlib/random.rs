//! `random` — a seeded generator, and the three ways to draw from it.

use crate::error::{ErrorKind, QuinceError, Result};
use crate::runtime::class::Builtin;
use crate::runtime::heap::Heap;
use crate::runtime::value::{Arg, Native, Value};
use crate::syntax::token::Span;

use super::math::number;
use super::{Member, Module};


/// What the generator starts at when nothing has said otherwise.
///
/// A fixed number, so a program that draws without seeding draws the same
/// sequence every run. That is the unusual half of the decision and the
/// deliberate one: it makes a bug involving random numbers reproducible, and it
/// lets the corpus assert exact values rather than ranges — which is the
/// difference between testing `random` and testing that it returns a number at
/// all.
///
/// A program that wants unpredictability asks for it — `random.seed(time.now())`
/// — which is also the one place these two modules meet.
pub const DEFAULT_SEED: u64 = 0x2545F4914F6CDD1D;

pub static RANDOM: Module = Module {
    name: "random",
    members: &[
        ("seed", Member::Fn(&SEED)),
        ("int", Member::Fn(&RAND_INT)),
        ("float", Member::Fn(&RAND_FLOAT)),
        ("choice", Member::Fn(&CHOICE)),
    ],
};

/// xorshift64*, which is four lines and good enough to pick a test fixture or
/// shuffle a list.
///
/// Written out rather than pulled in: the dependency list has been kept short on
/// purpose, and this is not cryptography. Anything needing a generator nobody
/// can predict needs one this file should not pretend to provide.
pub fn next_u64(state: &mut u64) -> u64 {
    // Zero is the one state xorshift cannot leave, so it is never entered.
    if *state == 0 {
        *state = DEFAULT_SEED;
    }
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545F4914F6CDD1D)
}

static SEED: Native = Native {
    name: "seed",
    arity: Some(1),
    params: &[Arg::of("n", &[Builtin::Int])],
    returns: Some(Builtin::Nil),
    doc: "Sets the generator's starting point, so a run can be repeated exactly.",
    func: |interp, args, span| {
        let n = number("seed", args, &interp.heap, span)?;
        interp.set_seed(n as i64 as u64);
        Ok(Value::Nil)
    },
};

/// Inclusive at both ends, which is what someone rolling a die means by
/// `random.int(1, 6)`.
static RAND_INT: Native = Native {
    name: "int",
    arity: Some(2),
    params: &[Arg::of("low", &[Builtin::Int]), Arg::of("high", &[Builtin::Int])],
    returns: Some(Builtin::Int),
    doc: "A random integer between `low` and `high`, including both ends.",
    func: |interp, args, span| {
        let low = integer("int", args, &interp.heap, span)?;
        let high = integer("int", &args[1..], &interp.heap, span)?;
        if low > high {
            return Err(QuinceError::new(
                format!("`int` needs a low bound at or below the high one, but {low} > {high}"),
                span,
            )
            .with_kind(ErrorKind::Value));
        }
        // Through `u64` and back, so the full width is reachable even when the
        // bounds sit at opposite ends of what an int can hold.
        let width = (high.wrapping_sub(low) as u64).wrapping_add(1);
        let draw = match width {
            0 => interp.next_random(),
            width => interp.next_random() % width,
        };
        Ok(Value::Int(low.wrapping_add(draw as i64)))
    },
};

/// In `[0, 1)`, the interval every other language's `random()` uses.
static RAND_FLOAT: Native = Native {
    name: "float",
    arity: Some(0),
    params: &[],
    returns: Some(Builtin::Float),
    doc: "A random float in `[0, 1)`.",
    func: |interp, _args, _span| {
        // The top 53 bits, which is exactly the mantissa a float has. Taking the
        // low ones instead is the classic way to end up with a generator whose
        // last bit is worse than its first.
        let bits = interp.next_random() >> 11;
        Ok(Value::Float(bits as f64 / (1u64 << 53) as f64))
    },
};

static CHOICE: Native = Native {
    name: "choice",
    arity: Some(1),
    params: &[Arg::of("items", &[Builtin::List])],
    returns: None,
    doc: "One item picked from `items`, so its type is whatever the list holds.",
    func: |interp, args, span| {
        let Value::List(id) = args[0].base(&interp.heap) else {
            return Err(QuinceError::new(
                format!(
                    "`choice` needs a list, but was given {}",
                    args[0].type_name(&interp.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type));
        };
        let id = *id;
        let len = interp.heap.list(id).len();
        if len == 0 {
            return Err(
                QuinceError::new("`choice` cannot choose from an empty list", span)
                    .with_kind(ErrorKind::Value),
            );
        }
        let index = (interp.next_random() % len as u64) as usize;
        Ok(interp.heap.list(id)[index].clone())
    },
};

/// The int `name` was given, refusing a float: `random.int(1.5, 3)` is a
/// question with no answer rather than one worth guessing at.
fn integer(name: &str, args: &[Value], heap: &Heap, span: Span) -> Result<i64> {
    match args[0].base(heap) {
        Value::Int(n) => Ok(*n),
        other => Err(QuinceError::new(
            format!(
                "`{name}` needs an int, but was given {}",
                other.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    }
}

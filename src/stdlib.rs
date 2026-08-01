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

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

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
pub static MODULES: &[&Module] = &[&MATH, &IO, &TIME, &RANDOM];

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

// -- io ----------------------------------------------------------------------

/// Paths are relative to the working directory, not to the file doing the
/// reading.
///
/// Deliberately the opposite of `import`, which resolves beside the importer.
/// The two answer different questions: an import names part of the program,
/// which travels with it, while a path names data the program was pointed at,
/// which belongs to whoever ran it. `quince run reports/build.qn` reading
/// `input.csv` should mean the `input.csv` of whoever typed that.
static IO: Module = Module {
    name: "io",
    members: &[
        ("read", Member::Fn(&READ)),
        ("write", Member::Fn(&WRITE)),
        ("append", Member::Fn(&APPEND)),
        ("exists", Member::Fn(&EXISTS)),
        ("lines", Member::Fn(&LINES)),
        ("line", Member::Fn(&LINE)),
    ],
};

/// The string `name` was given, refusing anything else.
fn text<'a>(name: &str, args: &'a [Value], heap: &'a Heap, span: Span) -> Result<&'a str, QuinceError> {
    match args[0].base(heap) {
        Value::Str(s) => Ok(s),
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

/// Every filesystem refusal, carrying the path and what the OS said.
///
/// Never a panic: a file that is not there is an ordinary thing to happen to a
/// running program, and is catchable like any other error.
fn io_error(what: &str, path: &str, err: std::io::Error, span: Span) -> QuinceError {
    QuinceError::new(format!("could not {what} `{path}`: {err}"), span).with_kind(ErrorKind::Io)
}

static READ: Native = Native {
    name: "read",
    arity: Some(1),
    func: |interp, args, span| {
        let path = text("read", args, &interp.heap, span)?.to_string();
        match std::fs::read_to_string(&path) {
            Ok(contents) => Ok(Value::Str(Rc::from(contents.as_str()))),
            Err(err) => Err(io_error("read", &path, err, span)),
        }
    },
};

static WRITE: Native = Native {
    name: "write",
    arity: Some(2),
    func: |interp, args, span| {
        let path = text("write", args, &interp.heap, span)?.to_string();
        let contents = text("write", &args[1..], &interp.heap, span)?.to_string();
        match std::fs::write(&path, contents) {
            Ok(()) => Ok(Value::Nil),
            Err(err) => Err(io_error("write", &path, err, span)),
        }
    },
};

static APPEND: Native = Native {
    name: "append",
    arity: Some(2),
    func: |interp, args, span| {
        use std::io::Write as _;
        let path = text("append", args, &interp.heap, span)?.to_string();
        let contents = text("append", &args[1..], &interp.heap, span)?.to_string();
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path);
        match opened.and_then(|mut file| file.write_all(contents.as_bytes())) {
            Ok(()) => Ok(Value::Nil),
            Err(err) => Err(io_error("append to", &path, err, span)),
        }
    },
};

/// The one member that answers rather than raising, because "is it there" is the
/// question asked *instead* of handling the error.
static EXISTS: Native = Native {
    name: "exists",
    arity: Some(1),
    func: |interp, args, span| {
        let path = text("exists", args, &interp.heap, span)?;
        Ok(Value::Bool(std::path::Path::new(path).exists()))
    },
};

static LINES: Native = Native {
    name: "lines",
    arity: Some(1),
    func: |interp, args, span| {
        let path = text("lines", args, &interp.heap, span)?.to_string();
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => return Err(io_error("read", &path, err, span)),
        };
        // A trailing newline ends the last line rather than starting an empty
        // one, which is what everything that counts lines agrees it means.
        let items: Vec<Value> = contents
            .strip_suffix('\n')
            .unwrap_or(&contents)
            .split('\n')
            .map(|line| Value::Str(Rc::from(line.strip_suffix('\r').unwrap_or(line))))
            .collect();
        Ok(Value::List(interp.heap.alloc(Object::List(items))))
    },
};

/// One line from standard input, or `nil` at end of input.
///
/// `nil` rather than an empty string, because a blank line *is* an empty string
/// and a program reading until input runs out has to tell the two apart. The
/// newline is not included, for the same reason `split` does not keep its
/// separator.
static LINE: Native = Native {
    name: "line",
    arity: Some(0),
    func: |interp, _args, span| {
        let mut line = String::new();
        match interp.read_line(&mut line) {
            Ok(0) => Ok(Value::Nil),
            Ok(_) => {
                let line = line.strip_suffix('\n').unwrap_or(&line);
                Ok(Value::Str(Rc::from(line.strip_suffix('\r').unwrap_or(line))))
            }
            Err(err) => Err(io_error("read", "standard input", err, span)),
        }
    },
};

// -- time --------------------------------------------------------------------

static TIME: Module = Module {
    name: "time",
    members: &[("now", Member::Fn(&NOW)), ("sleep", Member::Fn(&SLEEP))],
};

/// Seconds since the Unix epoch, as a float.
///
/// One clock and not two. A monotonic one is the correct thing to measure
/// elapsed time with, but the language has no way to say "this float may not be
/// compared to that one" — so shipping both would ship two floats that look
/// alike, must not be mixed, and carry nothing saying which is which. It waits
/// for a type that can hold the distinction.
static NOW: Native = Native {
    name: "now",
    arity: Some(0),
    func: |_interp, _args, span| {
        let since = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
            QuinceError::new("the system clock is set before the epoch", span)
                .with_kind(ErrorKind::Value)
        })?;
        Ok(Value::Float(since.as_secs_f64()))
    },
};

static SLEEP: Native = Native {
    name: "sleep",
    arity: Some(1),
    func: |interp, args, span| {
        let seconds = number("sleep", args, &interp.heap, span)?;
        if seconds < 0.0 {
            return Err(
                QuinceError::new(format!("cannot sleep for {seconds} seconds"), span)
                    .with_kind(ErrorKind::Value)
                    .with_help("a duration is not negative"),
            );
        }
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
        Ok(Value::Nil)
    },
};

// -- random ------------------------------------------------------------------

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

static RANDOM: Module = Module {
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
fn integer(name: &str, args: &[Value], heap: &Heap, span: Span) -> Result<i64, QuinceError> {
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

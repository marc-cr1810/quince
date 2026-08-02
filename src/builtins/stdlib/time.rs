//! `time` — the clock, and sleeping.

use crate::error::{ErrorKind, QuinceError};
use crate::runtime::class::Builtin;
use crate::runtime::value::{Native, Value};

use super::math::number;
use super::{Member, Module};

use std::time::{SystemTime, UNIX_EPOCH};


pub static TIME: Module = Module {
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
    params: &[],
    returns: Some(Builtin::Float),
    doc: "Seconds since the Unix epoch, as a float.",
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
    params: &["seconds"],
    returns: Some(Builtin::Nil),
    doc: "Pauses for `seconds`.",
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

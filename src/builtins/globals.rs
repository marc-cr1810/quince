//! The three functions every program starts with.
//!
//! `print`, `len`, and `type` are globals rather than methods because each either
//! applies to every type or to none of them. Anything that acts on one particular
//! type is a method on that type instead, seeded from [`super::types`].

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError};
use crate::interp::show::Ask;
use crate::runtime::class::Builtin;
use crate::runtime::value::{Arg, Native, Value};
use crate::syntax::ast::Op;

/// The globals every program starts with.
///
/// Anything that acts on one particular type is a method instead, reached
/// through that type's table in [`super::types`]. What remains here either applies to
/// every type (`len`, `type`) or to none of them (`print`).
pub static BUILTINS: &[&Native] = &[&PRINT, &LEN, &TYPE];

static PRINT: Native = Native {
    name: "print",
    arity: None,
    params: &[Arg::any("values")],
    returns: Some(Builtin::Nil),
    doc: "Writes its arguments to standard output, separated by spaces, and ends the line.",
    func: |interp, args, _span| {
        // Every argument is rendered before anything is written, so a failure
        // part-way through prints nothing rather than half a line. Printing is
        // where a program's `op string` runs, and it can raise.
        let mut parts = Vec::with_capacity(args.len());
        for value in args {
            parts.push(interp.display(value, Ask::Class)?);
        }
        writeln!(interp.out, "{}", parts.join(" ")).expect("failed to write output");
        Ok(Value::Nil)
    },
};

static LEN: Native = Native {
    name: "len",
    arity: Some(1),
    params: &[Arg::any("value")],
    returns: Some(Builtin::Int),
    doc: "How many characters are in a string, items in a list, or entries in a dict. A class may answer for itself with `op len`.",
    // Not a method, so it does not come through `call_method`'s substitution and
    // has to unwrap for itself. The error names the class rather than its base:
    // `len` failing on a `Box` should say `Box`.
    func: |interp, args, span| {
        // The class first, so a class extending `list` that counts something
        // other than its items is asked before the list it carries is measured.
        if let Some(method) = interp.slot(&args[0], Op::Len) {
            let answer = interp.call_op(method, &args[0], Vec::new())?;
            return match answer.base(&interp.heap) {
                // Read as it is given. A negative one is not refused: nothing
                // indexes with this, so an odd length is the class's own answer
                // to its own question, the way any int is a fine `op cmp`.
                Value::Int(n) => Ok(Value::Int(*n)),
                got => Err(interp.op_returned(Op::Len, &args[0], "an int", got)),
            };
        }
        match args[0].base(&interp.heap) {
            Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
            Value::List(id) => Ok(Value::Int(interp.heap.list(*id).len() as i64)),
            Value::Dict(id) => Ok(Value::Int(interp.heap.dict(*id).len() as i64)),
            _ => Err(QuinceError::new(
                format!(
                    "`len` does not apply to {}",
                    args[0].type_name(&interp.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type)),
        }
    },
};

static TYPE: Native = Native {
    name: "type",
    arity: Some(1),
    params: &[Arg::any("value")],
    returns: Some(Builtin::Str),
    doc: "The name of the value's type, as a string.",
    func: |interp, args, _span| Ok(Value::Str(Rc::from(args[0].type_name(&interp.heap)))),
};

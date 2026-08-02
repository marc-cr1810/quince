//! `io` — reading and writing files, and reading a line of the program's input.

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::runtime::class::Builtin;
use crate::runtime::heap::{Heap, Object};
use crate::runtime::value::{Native, Value};
use crate::syntax::token::Span;

use super::{Member, Module};

use std::rc::Rc;



/// Paths are relative to the working directory, not to the file doing the
/// reading.
///
/// Deliberately the opposite of `import`, which resolves beside the importer.
/// The two answer different questions: an import names part of the program,
/// which travels with it, while a path names data the program was pointed at,
/// which belongs to whoever ran it. `quince run reports/build.qn` reading
/// `input.csv` should mean the `input.csv` of whoever typed that.
pub static IO: Module = Module {
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
fn text<'a>(name: &str, args: &'a [Value], heap: &'a Heap, span: Span) -> Result<&'a str> {
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
fn io_error(what: &str, path: &str, err: std::io::Error, span: Span) -> Raised {
    QuinceError::new(format!("could not {what} `{path}`: {err}"), span).with_kind(ErrorKind::Io)
}

static READ: Native = Native {
    name: "read",
    arity: Some(1),
    params: &["path"],
    returns: Some(Builtin::Str),
    doc: "The whole contents of the file at `path`, as one string.",
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
    params: &["path", "contents"],
    returns: Some(Builtin::Nil),
    doc: "Writes `contents` to `path`, replacing what was there.",
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
    params: &["path", "contents"],
    returns: Some(Builtin::Nil),
    doc: "Adds `contents` to the end of `path`, creating the file if it is not there.",
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
    params: &["path"],
    returns: Some(Builtin::Bool),
    doc: "Whether there is anything at `path`. The one member that answers rather than raising.",
    func: |interp, args, span| {
        let path = text("exists", args, &interp.heap, span)?;
        Ok(Value::Bool(std::path::Path::new(path).exists()))
    },
};

static LINES: Native = Native {
    name: "lines",
    arity: Some(1),
    params: &["path"],
    returns: Some(Builtin::List),
    doc: "The lines of the file at `path`, without their line endings.",
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
    params: &[],
    returns: None,
    doc: "One line from standard input, or `nil` once input has run out. A blank line is an empty string, which is why the end is `nil` and not one.",
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

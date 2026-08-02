//! Subscripting and slicing.
//!
//! `x[i]`, `x[i] = v`, and `x[a:b]`, plus the two rules they share: what a
//! negative index means and how bounds are clamped.
//!
//! This is where v0.7's nullable dict indexing — `d[key]` answering `V?` instead
//! of raising — lands, and where v0.10 replaces the `Slice` node with a `range`
//! handed to `op get`.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Result};
use crate::interp::Interp;
use crate::runtime::dict::{Key, NotAKey};
use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::Op;
use crate::syntax::token::Span;

impl Interp {
    /// Reads `target[index]`, dispatching on what is being subscripted.
    ///
    /// A class extending a builtin is subscripted as one, and yields the base
    /// type: `Username("marc")[0]` is the string `"m"`.
    pub(super) fn index_get(
        &mut self,
        target: &Value,
        index: &Value,
        span: Span,
    ) -> Result<Value> {
        // The class first, so a class extending `dict` whose `x[k]` answers with
        // a default is asked before the dict it carries raises a key error.
        // Whatever the op returns is the answer — a subscript has no type it has
        // to be, any more than `+` does.
        if let Some(method) = self.slot(target, Op::Get) {
            return self.call_op(method, target, vec![index.clone()]);
        }

        match target.base(&self.heap) {
            // A missing key answers `nil`, not an error — v0.7 §3.10, and the
            // one thing in the milestone that breaks a running program.
            //
            // The trade: a language with `T?` in it should have one story about
            // absence rather than two, and `??` makes the total form ergonomic
            // for the first time. What it gives up is `d[key]` alone
            // distinguishing "missing" from "present, holding `nil`" — which is
            // not lost, only spelled in two expressions: `key in d` tests the key
            // set directly, whatever is stored under it.
            Value::Dict(id) => {
                let key = key_of(&self.heap, index, span)?;
                Ok(self.heap.dict(*id).get(&key).cloned().unwrap_or(Value::Nil))
            }
            // Indexed by character, not by byte, because `len` already counts
            // characters — a subscript that disagreed with the length would be
            // indefensible. The cost is a walk, since the storage is UTF-8.
            Value::Str(text) => {
                let chars: Vec<char> = text.chars().collect();
                let offset = resolve_index(&self.heap, index, chars.len(), "string", span)?;
                Ok(Value::Str(Rc::from(chars[offset].to_string())))
            }

            _ => {
                let (id, offset) = self.list_index(target, index, span)?;
                Ok(self.heap.list(id)[offset].clone())
            }
        }
    }

    /// `target[start:end]`, on a string or a list.
    pub(super) fn slice(
        &mut self,
        target: &Value,
        start: Option<&Value>,
        end: Option<&Value>,
        span: Span,
    ) -> Result<Value> {
        // A class that declares `op get` is not sliced around it. The op answers
        // one index, because there is no value in the language meaning "1 to 3"
        // for it to be handed — so slicing an instance would reach past the op to
        // whatever it is carrying, which is the one thing declaring the op said
        // not to do.
        if self.slot(target, Op::Get).is_some() {
            return Err(QuinceError::new(
                format!(
                    "{} declares `op get`, which answers one index at a time",
                    target.type_name(&self.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "read the elements one at a time — or slice `list(x)`, if the slice you want is \
                 of what the object holds rather than of what `op get` answers",
            ));
        }

        // Cloned rather than matched in place: the list arm allocates, so the
        // immutable borrow `base` takes of the heap has to be over by then.
        match target.base(&self.heap).clone() {
            Value::Str(text) => {
                let chars: Vec<char> = text.chars().collect();
                let (from, to) = slice_bounds(&self.heap, start, end, chars.len(), span)?;
                Ok(Value::Str(Rc::from(
                    chars[from..to].iter().collect::<String>(),
                )))
            }
            Value::List(id) => {
                let (from, to) =
                    slice_bounds(&self.heap, start, end, self.heap.list(id).len(), span)?;
                let items = self.heap.list(id)[from..to].to_vec();
                Ok(Value::List(self.heap.alloc(Object::List(items))))
            }
            _ => Err(QuinceError::new(
                format!("cannot slice {}", target.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("only lists and strings support slicing")),
        }
    }

    /// Resolves a list subscript, accepting Python-style negative indices.
    pub(super) fn list_index(
        &self,
        target: &Value,
        index: &Value,
        span: Span,
    ) -> Result<(ObjId, usize)> {
        let Value::List(id) = target.base(&self.heap) else {
            return Err(QuinceError::new(
                format!("cannot index {}", target.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("only lists, dicts, and strings support indexing"));
        };
        let offset = resolve_index(&self.heap, index, self.heap.list(*id).len(), "list", span)?;
        Ok((*id, offset))
    }
}

/// Converts a value to a dict key, explaining why if it cannot be one.
///
/// The payload unwrap belongs here rather than in `Key::from_value`, which has no
/// heap to reach a payload through — and this is the only caller that is not a
/// test. It is also not a separate decision from `equals`: if `Username("marc")`
/// equals `"marc"` then the two must hash alike, or a dict holds two equal keys in
/// different buckets. So a subclass is hashable exactly when its base is, and a
/// dict cannot tell the two apart — `keys()` hands back the base type.
pub(crate) fn key_of(heap: &Heap, value: &Value, span: Span) -> Result<Key> {
    // Asked before the unwrap, and it has to be: a subclass of `string` that
    // decides `==` for itself is no longer equal to the string it carries, so
    // filing it under that string would put it where `==` would never look.
    //
    // Python does exactly this, and not by convention — defining `__eq__` sets
    // `__hash__` to `None`, and the class stops being usable as a key.
    if heap.class(value.class(heap)).slot(Op::Eq).is_some() {
        return Err(QuinceError::new(
            format!(
                "a {} cannot be a dict key, because its `op eq` decides what equals it",
                value.type_name(heap)
            ),
            span,
        )
        // All three refusals here are `Type` and not `Key`: `KeyError` is a key
        // the dict does not hold, and these are values that may not be keys at
        // all. Python draws the same line, raising `TypeError: unhashable type`.
        .with_kind(ErrorKind::Type)
        .with_help(
            "a dict finds a key by its contents alone and cannot run `op eq`, so two keys \
             the class calls equal would be filed apart",
        ));
    }
    Key::from_value(value.base(heap)).map_err(|reason| {
        let message = match reason {
            NotAKey::Unhashable => format!(
                "a {} cannot be a dict key, because it is compared by identity",
                value.type_name(heap)
            ),
            NotAKey::Nan => "NaN cannot be a dict key, because it is not equal to itself".into(),
        };
        QuinceError::new(message, span).with_kind(ErrorKind::Type)
    })
}

/// Resolves a subscript against a length, accepting Python-style negatives.
///
/// Shared by lists and strings so the two cannot drift apart on what `-1`
/// means or on how an out-of-range index reads.
pub(crate) fn resolve_index(
    heap: &Heap,
    index: &Value,
    len: usize,
    what: &str,
    span: Span,
) -> Result<usize> {
    let Value::Int(raw) = index else {
        return Err(QuinceError::new(
            format!(
                "{what} index must be an int, found {}",
                index.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type));
    };

    let offset = if *raw < 0 { *raw + len as i64 } else { *raw };
    if offset < 0 || offset >= len as i64 {
        let mut err = QuinceError::new(
            format!("index {raw} is out of range for a {what} of length {len}"),
            span,
        )
        .with_kind(ErrorKind::Index);

        if len == 0 {
            err = err.with_help(format!("the {what} is empty"));
        } else {
            err = err.with_help(format!("valid range is 0..{}", len - 1));
        }

        return Err(err);
    }
    Ok(offset as usize)
}

/// Resolves slice bounds, which are **clamped rather than checked**.
///
/// `xs[:100]` asks for at most a hundred, not for a hundred that must exist, so
/// clamping is what makes "take the first n" writable without a length test
/// first. A subscript keeps erroring, because a single out-of-range index can
/// only be a mistake. An inverted range yields nothing rather than erroring,
/// for the same reason.
pub(crate) fn slice_bounds(
    heap: &Heap,
    start: Option<&Value>,
    end: Option<&Value>,
    len: usize,
    span: Span,
) -> Result<(usize, usize)> {
    let len = len as i64;
    let resolve = |bound: Option<&Value>, default: i64| -> Result<i64> {
        let Some(bound) = bound else {
            return Ok(default);
        };
        let Value::Int(raw) = bound else {
            return Err(QuinceError::new(
                format!("slice bounds must be ints, found {}", bound.type_name(heap)),
                span,
            )
            .with_kind(ErrorKind::Type));
        };
        Ok(if *raw < 0 { *raw + len } else { *raw })
    };

    let from = resolve(start, 0)?.clamp(0, len);
    let to = resolve(end, len)?.clamp(0, len);
    Ok((from as usize, to.max(from) as usize))
}

//! `dict`'s methods.


use crate::error::{ErrorKind, QuinceError};
use crate::interp::error::frozen;
use crate::interp::index::key_of;
use crate::runtime::class::Builtin;
use crate::runtime::heap::Object;
use crate::runtime::value::{Native, Value};

pub static KEYS: Native = Native {
    name: "keys",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::List),
    doc: "The dict's keys, in insertion order.",
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let keys: Vec<_> = interp.heap.dict(*id).keys().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(keys))))
        }
        other => Err(QuinceError::new(
            format!(
                "`keys` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

pub static VALUES: Native = Native {
    name: "values",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::List),
    doc: "The dict's values, in insertion order.",
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let values: Vec<_> = interp.heap.dict(*id).values().cloned().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(values))))
        }
        other => Err(QuinceError::new(
            format!(
                "`values` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

/// Removing a key that is not there is an error, for the same reason reading one
/// is: silently doing nothing hides the typo that caused it.
/// `d.get(k, fallback)` — the value at `k`, or `fallback` if it is not there.
///
/// The reason to want it is that `d[k]` raises, which is right for a key a
/// program believes is present and wrong for one it is asking about. Both are
/// real questions and they need different spellings.
///
/// The fallback is required rather than defaulting to `nil`, because a dict may
/// hold `nil` — and a one-argument `get` would answer the same thing for "not
/// there" and "there, and nil", which is exactly the distinction someone reaches
/// for `get` to make.
pub static GET: Native = Native {
    name: "get",
    arity: Some(3),
    params: &["key", "default"],
    returns: None,
    doc: "The value stored under `key`, or `default` if there is none — so its type is whatever the dict holds.",
    func: |interp, args, span| match args[0].base(&interp.heap) {
        Value::Dict(id) => {
            let key = key_of(&interp.heap, &args[1], span)?;
            Ok(interp
                .heap
                .dict(*id)
                .get(&key)
                .cloned()
                .unwrap_or_else(|| args[2].clone()))
        }
        other => Err(QuinceError::new(
            format!(
                "`get` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

pub static REMOVE: Native = Native {
    name: "remove",
    arity: Some(2),
    params: &["key"],
    returns: None,
    doc: "Takes `key` out of the dict and answers with what it held. Raises if the key is not there.",
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let key = key_of(&interp.heap, &args[1], span)?;
            let removed = interp
                .heap
                .dict_mut(*id)
                .map(|entries| entries.remove(&key));
            removed
                .map_err(|_| frozen(&interp.heap, &args[0], span))?
                .ok_or_else(|| {
                    QuinceError::new(
                        format!("key {} is not in the dict", args[1].repr_base(&interp.heap)),
                        span,
                    )
                    .with_kind(ErrorKind::Key)
                })
        }
        other => Err(QuinceError::new(
            format!(
                "`remove` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};


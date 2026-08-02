//! `list`'s methods.


use crate::error::{ErrorKind, QuinceError, Result};
use crate::interp::Interp;
use crate::interp::error::frozen;
use crate::runtime::class::Builtin;
use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::{Arg, Native, Value};
use crate::syntax::ast::BinaryOp;
use crate::syntax::token::Span;

/// The in-place counterpart to `+`, which builds a new list.
///
/// `args[0]` is the receiver, so a method's declared arity is one more than the
/// number of arguments written at the call site.
/// `xs.reverse()` — a new list, back to front.
pub static REVERSE: Native = Native {
    name: "reverse",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::List),
    doc: "A new list with the items in the opposite order. The receiver is left alone.",
    func: |interp, args, span| {
        let source = receiver_list("reverse", args, &interp.heap, span)?;
        let mut items = interp.heap.list(source).to_vec();
        items.reverse();
        Ok(Value::List(interp.heap.alloc(Object::List(items))))
    },
};

/// `xs.find(v)` — the index of the first element equal to `v`, or `-1`.
///
/// `-1` rather than `nil`, so the answer is always an int and a caller can
/// compare it without first asking which kind it got. It is also what `in`
/// already implies: `x in xs` answers the question `find` is being asked for
/// when the index does not matter.
pub static FIND: Native = Native {
    name: "find",
    arity: Some(2),
    params: &[Arg::any("item")],
    returns: Some(Builtin::Int),
    doc: "Where `item` first appears in the list, or `-1` if it is not there.",
    func: |interp, args, span| {
        let source = receiver_list("find", args, &interp.heap, span)?;
        let mut index = 0;
        loop {
            let Some(item) = interp.heap.list(source).get(index).cloned() else {
                return Ok(Value::Int(-1));
            };
            // Equality can run an `op eq`, so the length is re-read every time
            // round rather than captured — the same reason `walk_list` does.
            if interp.equals(&item, &args[1])? {
                return Ok(Value::Int(index as i64));
            }
            index += 1;
        }
    },
};

/// `xs.sum()` — the elements added together, left to right.
///
/// Through the same `+` an expression uses, so a list of strings concatenates
/// and a class defining `op add` sums.
///
/// It starts at the *first element*, not at zero, and that is the whole reason
/// the second sentence above is true. Starting at zero would mean every sum
/// began `0 + x`, so a list of strings would fail and so would a list of any
/// class that defines `op add` — which is to say `sum` would work for numbers
/// and lie about the rest. An empty list is the one case with nothing to start
/// from, and answers `0`: the identity for the only type that can be summed
/// without knowing what is in the list.
pub static SUM: Native = Native {
    name: "sum",
    arity: Some(1),
    params: &[],
    returns: None,
    doc: "The items added together with `+`, left to right, so its type is whatever adding them gives. An empty list sums to `0`.",
    func: |interp, args, span| {
        let source = receiver_list("sum", args, &interp.heap, span)?;
        let mark = interp.temps.len();
        interp.temps.push(args[0].clone());

        let Some(mut total) = interp.heap.list(source).first().cloned() else {
            interp.temps.truncate(mark);
            return Ok(Value::Int(0));
        };
        let mut index = 1;
        let result = loop {
            let Some(item) = interp.heap.list(source).get(index).cloned() else {
                break Ok(total);
            };
            // The running total is a value held across a call that can allocate,
            // so it is rooted like any other. The previous one is dropped only
            // once its replacement exists.
            interp.temps.push(total.clone());
            let sum = interp.binary(BinaryOp::Add, total, item, span, span, span);
            interp.temps.pop();
            match sum {
                Ok(value) => total = value,
                Err(err) => break Err(err),
            }
            index += 1;
        };

        interp.temps.truncate(mark);
        result
    },
};

/// The list `name` was called on, or a type error naming it.
pub(super) fn receiver_list(name: &str, args: &[Value], heap: &Heap, span: Span) -> Result<ObjId> {
    match args[0].base(heap) {
        Value::List(id) => Ok(*id),
        other => Err(QuinceError::new(
            format!(
                "`{name}` needs a list, but was given {}",
                other.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    }
}

/// Walks a list, calling `f` on each element, with everything rooted.
///
/// **This is the first thing in the tree to call Quince code from inside a
/// native**, and the rule it has to keep is the one written at the `Native` arm
/// of `call`: `args` lives in a Rust frame and nothing roots it, which was safe
/// only for as long as no builtin reached a safe point. Calling a function
/// reaches one on every statement of its body.
///
/// So three things go on `temps` before anything runs: the receiver, the
/// function, and the list being built. The results are rooted by being *in* that
/// list rather than by being pushed separately, and each element is rooted while
/// it is in flight. Everything is dropped at the mark on the way out, including
/// on the error path — `truncate` runs before the `?`, which is the discipline
/// every other frame here already follows.
///
/// The length is re-read each time round rather than captured. A callback that
/// shortens the list it was handed would otherwise index past the end of it.
pub(super) fn walk_list(
    interp: &mut Interp,
    name: &str,
    args: &[Value],
    span: Span,
    mut f: impl FnMut(&mut Interp, Value, ObjId) -> Result<()>,
) -> Result<Value> {
    let source = receiver_list(name, args, &interp.heap, span)?;
    let mark = interp.temps.len();
    interp.temps.push(args[0].clone());
    interp.temps.push(args[1].clone());
    let out = interp.heap.alloc(Object::List(Vec::new()));
    interp.temps.push(Value::List(out));

    let mut index = 0;
    let result = loop {
        if index >= interp.heap.list(source).len() {
            break Ok(());
        }
        let item = interp.heap.list(source)[index].clone();
        interp.temps.push(item.clone());
        let step = f(interp, item, out);
        interp.temps.pop();
        if let Err(err) = step {
            break Err(err);
        }
        index += 1;
    };

    interp.temps.truncate(mark);
    result?;
    Ok(Value::List(out))
}

/// `xs.map(f)` — a new list of `f` applied to each element.
pub static MAP: Native = Native {
    name: "map",
    arity: Some(2),
    params: &[Arg::any("f")],
    returns: Some(Builtin::List),
    doc: "A new list holding `f` applied to each item.",
    func: |interp, args, span| {
        walk_list(interp, "map", args, span, |interp, item, out| {
            let mapped = interp.call(args[1].clone(), vec![item], span)?;
            interp
                .heap
                .list_mut(out)
                .map(|items| items.push(mapped))
                .expect("a list this call allocated cannot be frozen");
            Ok(())
        })
    },
};

/// `xs.filter(f)` — the elements `f` answered truthily for.
///
/// The predicate's answer goes through `is_truthy`, so a class deciding its own
/// truthiness decides this too, exactly as it does in an `if`.
pub static FILTER: Native = Native {
    name: "filter",
    arity: Some(2),
    params: &[Arg::any("f")],
    returns: Some(Builtin::List),
    doc: "A new list holding the items `f` answered truthily for.",
    func: |interp, args, span| {
        walk_list(interp, "filter", args, span, |interp, item, out| {
            let verdict = interp.call(args[1].clone(), vec![item.clone()], span)?;
            if interp.is_truthy(&verdict)? {
                interp
                    .heap
                    .list_mut(out)
                    .map(|items| items.push(item))
                    .expect("a list this call allocated cannot be frozen");
            }
            Ok(())
        })
    },
};

/// `xs.sort()` — a new list, in the order `<` puts the elements in.
///
/// A new list rather than a rearrangement of this one, so it is usable on a
/// `const` list and so `sort` cannot surprise a second name for the same list.
///
/// A merge sort, because comparing can run Quince code — a class's `op lt` — and
/// so can fail, which `sort_by` has no way to report. It is also stable, which a
/// comparison-defining class has every right to expect.
///
/// Rooting is simpler here than in `walk_list`: every element goes onto `temps`
/// once at the start, so the Rust `Vec`s the merge passes around hold clones of
/// values that are already reachable.
pub static SORT: Native = Native {
    name: "sort",
    arity: Some(1),
    params: &[],
    returns: Some(Builtin::List),
    doc: "A new list with the items in ascending order. Stable, and it asks `op lt` or `op cmp` where a class defines one.",
    func: |interp, args, span| {
        let source = receiver_list("sort", args, &interp.heap, span)?;
        let mark = interp.temps.len();
        interp.temps.push(args[0].clone());
        let items: Vec<Value> = interp.heap.list(source).to_vec();
        interp.temps.extend(items.iter().cloned());

        let sorted = merge_sort(interp, items, span);
        interp.temps.truncate(mark);
        let sorted = sorted?;
        Ok(Value::List(interp.heap.alloc(Object::List(sorted))))
    },
};

pub(super) fn merge_sort(
    interp: &mut Interp,
    items: Vec<Value>,
    span: Span,
) -> Result<Vec<Value>> {
    if items.len() <= 1 {
        return Ok(items);
    }
    let mut right = items;
    let left = right.split_off(right.len() / 2);
    // `split_off` hands back the tail, so the names are the wrong way round —
    // swapped here rather than at every use below.
    let (left, right) = (right, left);
    let left = merge_sort(interp, left, span)?;
    let right = merge_sort(interp, right, span)?;

    let mut merged = Vec::with_capacity(left.len() + right.len());
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        // `right < left` rather than `left <= right`, because `<=` is `op cmp`'s
        // alone and a class may define `op lt` without it. Taking the left one
        // unless the right is strictly smaller is what keeps this stable.
        let takes_right = interp.less_than(&right[j], &left[i], span)?;
        match takes_right {
            true => {
                merged.push(right[j].clone());
                j += 1;
            }
            false => {
                merged.push(left[i].clone());
                i += 1;
            }
        }
    }
    merged.extend_from_slice(&left[i..]);
    merged.extend_from_slice(&right[j..]);
    Ok(merged)
}

pub static PUSH: Native = Native {
    name: "push",
    arity: Some(2),
    params: &[Arg::any("item")],
    returns: Some(Builtin::Nil),
    doc: "Adds `item` to the end of the list.",
    func: |interp, args, span| match &args[0] {
        Value::List(id) => {
            // Against what the list was declared to hold, if it was declared to
            // hold anything. §3.9's descriptor is what makes this a lookup
            // rather than a walk.
            let item = interp.admitted(*id, 0, args[1].clone(), "the item", span)?;
            let pushed = interp.heap.list_mut(*id).map(|items| items.push(item));
            pushed.map_err(|_| frozen(&interp.heap, &args[0], span))?;
            Ok(Value::Nil)
        }
        other => Err(QuinceError::new(
            format!(
                "`push` needs a list, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

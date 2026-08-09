//! What the language asks a value.
//!
//! Two kinds of question, together because a class answers both the same way: the
//! operators a program writes — `+`, `<`, `in`, unary `-` — and the ones it does
//! not, whether a value is true and whether two are equal.
//!
//! Every one of them consults the class's `Op` slot first and falls back to what
//! the builtin means by it. v0.7's bitwise slots, v0.8's overloaded operators, and
//! v0.10's `op range` and `op next` are all rows in that same lookup.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::interp::Interp;
use crate::interp::error::{op_mismatch, type_error};
use crate::interp::index::key_of;
use crate::runtime::dict::Key;
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{BinaryOp, Op, Reflect, UnaryOp};
use crate::syntax::token::Span;

impl Interp {
    /// Whether `lhs < rhs`, asking the class if it has an opinion.
    ///
    /// For `sort`, which needs an ordering it can call rather than an expression
    /// to evaluate. The same path `<` takes, so a class defining `op lt` or
    /// `op cmp` sorts by what it said.
    pub(crate) fn less_than(&mut self, lhs: &Value, rhs: &Value, span: Span) -> Result<bool> {
        let answer = self.binary(BinaryOp::Lt, lhs.clone(), rhs.clone(), span, span, span)?;
        match answer.base(&self.heap) {
            Value::Bool(b) => Ok(*b),
            // `<` on a pair it does not apply to has already raised by here, so
            // this is a class whose `op lt` answered with something else — which
            // `binary` refuses too. Kept as a refusal rather than an unwrap
            // because the two paths could drift.
            got => Err(QuinceError::new(
                format!(
                    "sorting compared two values and got {} rather than a bool",
                    got.type_name(&self.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }

    /// The method a value's class supplies for `op`, declared or inherited.
    ///
    /// One array read after finding the class, which is the whole point of the
    /// shape: `if x` on a class that defines nothing costs a bounds check, not a
    /// hashed name.
    pub(crate) fn slot(&self, value: &Value, op: Op) -> Option<Value> {
        let class = value.class(&self.heap);
        self.heap.class(class).slot(op).cloned()
    }

    /// Calls what a slot holds, with the receiver and arguments rooted.
    ///
    /// No span comes in, because a slot call cannot be wrong at the point of the
    /// call: the parser checked the parameter count against [`Op::arity`], and
    /// only an `op` ever fills a slot, so there is no arity to report and nothing
    /// uncallable. The span it does need is for the recursion limit, and the op's
    /// own body is where that should point — an `op string` that prints itself is
    /// a mistake in the op, not at the `print` that innocently asked.
    ///
    /// Rooting, because this is a safe point: the receiver and every argument sit
    /// in Rust locals belonging to callers that were built to hold neither across
    /// a call. Restored before returning either way, which is the discipline
    /// `exec_try` depends on.
    pub(crate) fn call_op(
        &mut self,
        method: Value,
        receiver: &Value,
        args: Vec<Value>,
    ) -> Result<Value> {
        let span = match &method {
            Value::Function(id) => self.heap.function(*id).decl.body.span,
            // Which candidate runs is decided by `call_method` below, from the
            // argument the operator is passing. The span is only for the
            // recursion limit, and the first declaration is as good a place to
            // point at as any — they share a name and a class.
            Value::Overload(id) => match self.heap.overload(*id).first() {
                Some(Value::Function(first)) => self.heap.function(*first).decl.body.span,
                other => unreachable!("an overload set holds functions, found {other:?}"),
            },
            // `Class::builtin` fills exactly one slot from a seed table, so a
            // native in a slot is an `init` and nothing else; and `init` is not
            // among the ops that come through here — construction has its own
            // path. Both halves are in this crate, and `a_native_fills_no_slot_
            // but_init` holds the first.
            //
            // A placeholder span used to stand here. It was never rendered,
            // which is exactly the problem: an unreachable branch that answers
            // with byte zero is indistinguishable from a reachable one that is
            // wrong, and the sweep cannot tell them apart from the outside.
            other => unreachable!("an op slot holds a function, found {other:?}"),
        };

        let mark = self.temps.len();
        self.temps.push(receiver.clone());
        self.temps.extend(args.iter().cloned());
        let result = self.call_method(receiver.clone(), method, args, span);
        self.temps.truncate(mark);
        result
    }

    /// The candidate an operator's slot supplies for these operands.
    ///
    /// The gate between "this class answers for the operator" and "this class
    /// answers for the operator *here*". A slot that exists but takes other
    /// types is not a fallback to the builtin behaviour — declaring `op mul` is
    /// a class saying what `*` means to it — so this refuses rather than
    /// falling through, and refuses at the expression rather than inside the
    /// declaration.
    pub(super) fn op_for(
        &self,
        op: Op,
        method: Value,
        receiver: &Value,
        args: &[Value],
        spans: (Span, Span, Span),
    ) -> Result<Value> {
        match self.fitting(&method, args, true) {
            Some(chosen) => Ok(chosen),
            None => Err(op_mismatch(
                &self.heap,
                op,
                receiver,
                args,
                &self.signatures(&method, true),
                spans,
            )),
        }
    }

    /// The same for a binary operator.
    ///
    /// Split from [`Interp::op_for`] for the report rather than for the rule:
    /// `a * b` has two operands with spans of their own, so it gets exactly the
    /// diagnostic every other binary type error gets — the same sentence, a
    /// label on each side, and the operator itself marked in between. A reader
    /// should not be able to tell from the shape of the report whether the class
    /// declared the slot and refused the operand or never declared it at all;
    /// what the class *does* take goes in the help line, which is the one thing
    /// the ordinary report has nothing to say about.
    pub(super) fn binary_op_for(
        &self,
        op: BinaryOp,
        // Exactly what `binary_slot` produced: which slot answered, what it
        // holds, whose class it belongs to, and the operand being passed to it —
        // which is not always `rhs`, since a reflected `op cmp` arrives with the
        // two swapped. They travel together because a caller that could pass one
        // without the others would be passing a mismatched set.
        found: (Op, Value, &Value, &Value),
        operands: (&Value, &Value),
        spans: (Span, Span, Span),
    ) -> Result<Value> {
        let (slot, method, receiver, other) = found;
        let (lhs, rhs) = operands;
        if let Some(chosen) = self.fitting(&method, std::slice::from_ref(other), true) {
            return Ok(chosen);
        }
        let (lhs_span, rhs_span, expr_span) = spans;
        Err(
            type_error(&self.heap, op, lhs, rhs, lhs_span, rhs_span, expr_span).with_help(
                format!(
                    "`{}` declares `op {}` for: {} — convert the operand, or declare one for \
                     these types beside the ones that are there",
                    receiver.type_name(&self.heap),
                    slot.name(),
                    self.signatures(&method, true).join(", ")
                ),
            ),
        )
    }

    /// The error for an `op` that answered with the wrong kind of value.
    ///
    /// Reported against the op's body rather than the expression that triggered
    /// it, for the same reason the recursion limit is: the line that wrote `if x`
    /// is not the line that is wrong.
    pub(crate) fn op_returned(
        &self,
        op: Op,
        receiver: &Value,
        expected: &str,
        got: &Value,
    ) -> Raised {
        // Reached only after that op answered, so the slot is filled and holds a
        // function — see `call_op` for why a native cannot be here.
        let span = match self.slot(receiver, op) {
            Some(Value::Function(id)) => self.heap.function(id).decl.body.span,
            Some(Value::Overload(id)) => match self.heap.overload(id).first() {
                Some(Value::Function(first)) => self.heap.function(*first).decl.body.span,
                other => unreachable!("an overload set holds functions, found {other:?}"),
            },
            other => unreachable!("`op {}` answered from {other:?}", op.name()),
        };
        QuinceError::new(
            format!(
                "`op {}` must answer with {expected}, but {}'s returned {}",
                op.name(),
                receiver.type_name(&self.heap),
                got.type_name(&self.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)
        .with_help(format!(
            "the language calls `op {}` itself and reads {expected} back, so a class chooses \
             what it answers, not what it answers *with*",
            op.name()
        ))
    }

    /// Whether a value counts as true, which a class may answer with `op bool`.
    pub fn is_truthy(&mut self, value: &Value) -> Result<bool> {
        let Some(method) = self.slot(value, Op::Bool) else {
            return Ok(value.is_truthy_base(&self.heap));
        };
        let answer = self.call_op(method, value, Vec::new())?;
        // Nothing is coerced. `op bool` deciding `if x` by returning a list would
        // mean the emptiness of that list quietly decided the branch, one
        // indirection away from anything the reader can see.
        match answer.base(&self.heap) {
            Value::Bool(b) => Ok(*b),
            other => Err(self.op_returned(Op::Bool, value, "a bool", other)),
        }
    }

    /// Which operand's class answers for `op`, with the arguments already in the
    /// order [`Interp::call_op`] wants them.
    ///
    /// The left operand is asked first, always. The right is asked only if
    /// [`Op::reflect`] permits it, and the `bool` says whether that is what
    /// happened — which the caller has to know for an op whose answer means the
    /// opposite when the operands arrive swapped.
    pub(super) fn binary_slot(
        &mut self,
        op: Op,
        lhs: &Value,
        rhs: &Value,
    ) -> Option<(Value, Value, Value, bool)> {
        if let Some(method) = self.slot(lhs, op) {
            return Some((method, lhs.clone(), rhs.clone(), false));
        }
        if op.reflect() != Reflect::Never
            && let Some(method) = self.slot(rhs, op)
        {
            return Some((method, rhs.clone(), lhs.clone(), true));
        }
        None
    }

    /// Whether two values are equal, which a class may answer with `op eq`.
    ///
    /// Numbers compare across `int` and `float`, since they are one numeric
    /// tower, but no other pair of types is ever equal — `1 == "1"` is `false`
    /// rather than a coercion.
    ///
    /// A container holding itself is only survivable here when both sides are the
    /// same handle. Two *distinct* cycles recurse without bound and take the
    /// process down with a native stack overflow — a crash that predates the
    /// slots, shared with the renderer, and tracked as its own piece of work
    /// rather than patched in the middle of this one. The fix is Python's: record
    /// the pair being compared and answer `true` on reaching it again.
    pub fn equals(&mut self, lhs: &Value, rhs: &Value) -> Result<bool> {
        // Asked before the payload is unwrapped, so a subclass of `string` that
        // declares `op eq` beats the string it carries. Either side may answer,
        // because `==` cannot depend on which order it was written in.
        if let Some((method, receiver, other, _)) = self.binary_slot(Op::Eq, lhs, rhs) {
            let answer = self.call_op(method, &receiver, vec![other])?;
            return match answer.base(&self.heap) {
                Value::Bool(b) => Ok(*b),
                got => Err(self.op_returned(Op::Eq, &receiver, "a bool", got)),
            };
        }

        // Cloned rather than borrowed: comparing what a container holds can run a
        // program's `op eq`, and a borrow of the heap cannot be held across that.
        let (a, b) = (lhs.base(&self.heap).clone(), rhs.base(&self.heap).clone());
        let equal = match (&a, &b) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => *a as f64 == *b,
            (Value::Str(a), Value::Str(b)) => a == b,
            // One handle is equal to itself without a walk, which is the only
            // self-reference this can answer. See the note above.
            (Value::List(a), Value::List(b)) if a == b => true,
            (Value::List(a), Value::List(b)) => return self.lists_equal(*a, *b),
            // Elementwise, exactly as a list is — and never equal to a list,
            // because the arity that makes a tuple a tuple would then be a
            // property two equal values disagreed about. §3.5.
            (Value::Tuple(a), Value::Tuple(b)) if a == b => true,
            (Value::Tuple(a), Value::Tuple(b)) => return self.tuples_equal(*a, *b),
            // Order is not part of a dict's identity, only its contents:
            // `{"a": 1, "b": 2}` equals `{"b": 2, "a": 1}`.
            (Value::Dict(a), Value::Dict(b)) if a == b => true,
            (Value::Dict(a), Value::Dict(b)) => return self.dicts_equal(*a, *b),
            // Functions compare by identity; there is no useful structural test.
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Native(a), Value::Native(b)) => std::ptr::eq(*a, *b),
            // Two bindings of the same method to the same object are equal even
            // though `x.push` allocates a fresh one each time — otherwise
            // `x.push == x.push` would be false, which nothing could justify.
            // The receiver compares by identity rather than structurally, so
            // `[].push` and another empty list's `push` stay distinct.
            (Value::BoundMethod(a), Value::BoundMethod(b)) => {
                if a == b {
                    true
                } else {
                    let (a, b) = (self.heap.bound_method(*a), self.heap.bound_method(*b));
                    a.method == b.method && a.receiver == b.receiver
                }
            }
            // Classes, and instances carrying no payload, compare by identity. Two
            // separately built `Point(1, 1)`s are different objects, and saying
            // otherwise would require deciding that fields are all that a class is
            // — which is false the moment one of them is mutable.
            //
            // One extending a builtin was unwrapped above, so `Username("marc")`
            // equals `"marc"`, and by transitivity a `Slug("marc")` too. Its extra
            // fields are invisible to `==`, which is the price of being a string
            // rather than a wrapper around one — and the same decision as hashing,
            // since two equal values must land in the same bucket.
            (Value::Class(a), Value::Class(b)) => a == b,
            (Value::Instance(a), Value::Instance(b)) => a == b,
            // By identity, like a class, and the cache in `load_module` is what
            // makes that useful rather than pedantic: a module is built once, so
            // two imports of `math` are the same object and compare equal. If it
            // were built per import, this would answer `false` for two things a
            // programmer has every reason to call the same.
            (Value::Module(a), Value::Module(b)) => a == b,
            _ => false,
        };
        Ok(equal)
    }

    /// Item by item, by index rather than over an iterator: each comparison can
    /// run a program's `op eq`, which is free to mutate either list. `get` is
    /// what makes a list that shrank mid-comparison end the comparison rather
    /// than panic — and having shrunk, it is no longer the same length.
    pub(super) fn lists_equal(&mut self, a: ObjId, b: ObjId) -> Result<bool> {
        if self.heap.list(a).len() != self.heap.list(b).len() {
            return Ok(false);
        }
        let mut i = 0;
        loop {
            let (Some(x), Some(y)) = (
                self.heap.list(a).get(i).cloned(),
                self.heap.list(b).get(i).cloned(),
            ) else {
                return Ok(self.heap.list(a).len() == self.heap.list(b).len());
            };
            if !self.equals(&x, &y)? {
                return Ok(false);
            }
            i += 1;
        }
    }

    /// Elementwise, and by index for the reason [`Interp::lists_equal`] is:
    /// comparing an element can run a program's `op eq`, which cannot hold a
    /// borrow of the heap across it.
    ///
    /// Arity is checked once at the top and not again at the bottom, which is
    /// the one way this differs from the list version — a tuple cannot shrink
    /// under a comparison, because nothing in the language can write to one.
    pub(super) fn tuples_equal(&mut self, a: ObjId, b: ObjId) -> Result<bool> {
        if self.heap.tuple(a).len() != self.heap.tuple(b).len() {
            return Ok(false);
        }
        for i in 0..self.heap.tuple(a).len() {
            let (x, y) = (self.heap.tuple(a)[i].clone(), self.heap.tuple(b)[i].clone());
            if !self.equals(&x, &y)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// By key, which cannot ask a class anything — a [`Key`] holds no handle, so
    /// two keys are equal exactly when `key_of` maps them to the same one. Only
    /// the values are compared through `equals`.
    pub(super) fn dicts_equal(&mut self, a: ObjId, b: ObjId) -> Result<bool> {
        if self.heap.dict(a).len() != self.heap.dict(b).len() {
            return Ok(false);
        }
        // Collected up front for the same reason the list walk uses `get`: a
        // comparison can run Quince code, and iterating either dict across that
        // is not allowed. A key that has gone missing since is not equal.
        let keys: Vec<Key> = self
            .heap
            .dict(a)
            .iter()
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let (Some(x), Some(y)) = (
                self.heap.dict(a).get(&key).cloned(),
                self.heap.dict(b).get(&key).cloned(),
            ) else {
                return Ok(false);
            };
            if !self.equals(&x, &y)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    // -- operators ---------------------------------------------------------

    pub(super) fn unary(&mut self, op: UnaryOp, value: Value, span: Span) -> Result<Value> {
        // `not` asks only for truthiness, which unwraps a payload for itself.
        if let UnaryOp::Not = op {
            let truthy = self.is_truthy(&value)?;
            return Ok(Value::Bool(!truthy));
        }
        // The class first, so one that says what the operator means to it is
        // asked before the number it might be carrying. Whatever it answers is
        // the answer: unlike `op bool`, there is no type `-x` or `~x` has to be.
        let slot = match op {
            UnaryOp::Neg => Op::Neg,
            UnaryOp::BitNot => Op::BitNot,
            UnaryOp::Not => unreachable!("truthiness is answered above"),
        };
        if let Some(method) = self.slot(&value, slot) {
            return self.call_op(method, &value, Vec::new());
        }

        // `~` acts on the bits, which only an int has.
        if let UnaryOp::BitNot = op {
            return match value.base(&self.heap) {
                Value::Int(n) => Ok(Value::Int(!n)),
                other => Err(QuinceError::new(
                    format!("`~` does not apply to {}", other.type_name(&self.heap)),
                    span,
                )
                .with_kind(ErrorKind::Type)
                .with_help("only an int has bits — convert with `int(x)` first")),
            };
        }

        // Otherwise negation acts on the number, so a class extending `int` is
        // unwrapped to it and `-Count(5)` is `-5` rather than a `Count`. The error
        // still names the class, because that is the value the line was written
        // about.
        match value.base(&self.heap) {
            Value::Int(n) => n.checked_neg().map(Value::Int).ok_or_else(|| {
                QuinceError::new("integer overflow", span)
        .with_kind(ErrorKind::Overflow)
        .with_help("an int holds 64 bits — convert with `float(x)` for a wider range, at the cost of exactness")
            }),
            Value::Float(n) => Ok(Value::Float(-n)),
            _ => Err(QuinceError::new(
                format!("cannot negate {}", value.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }

    pub(crate) fn binary(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        lhs_span: Span,
        rhs_span: Span,
        expr_span: Span,
    ) -> Result<Value> {
        use BinaryOp::*;

        // Equality is defined for every pair of types, which is why there is no
        // type error to raise here — unlike `-`, no pair of values is refused.
        // The `?` is for the class that answers for itself: an `op eq` is
        // ordinary Quince code and can raise like any other.
        match op {
            Eq => return Ok(Value::Bool(self.equals(&lhs, &rhs)?)),
            Ne => return Ok(Value::Bool(!self.equals(&lhs, &rhs)?)),
            In => return self.contains(&rhs, &lhs, expr_span),
            _ => {}
        }

        // The ordering operators, asked before the operands are unwrapped for the
        // same reason: a class extending `int` that says how it orders has to beat
        // the int it carries.
        // The three spans travel together because the diagnostics that need the
        // operands need the whole expression too, and threading them one at a
        // time through two layers was three parameters either way.
        let spans = (lhs_span, rhs_span, expr_span);
        if let Some(answer) = self.compare(op, &lhs, &rhs, spans)? {
            return Ok(answer);
        }

        // And the arithmetic, which has to be asked before the `+` on strings and
        // lists below: a class extending `list` that says what `+` means to it is
        // saying it instead of concatenation, not as well as.
        if let Some(answer) = self.arith(op, &lhs, &rhs, spans)? {
            return Ok(answer);
        }

        // Dispatch on what the operands *are*, so a class extending a builtin is
        // operated on as one — and the result is the base type, not the subclass.
        let (a, b) = (lhs.base(&self.heap).clone(), rhs.base(&self.heap).clone());

        // `+` is the one operator shared between numbers and the collections.
        if let (Add, Value::Str(a), Value::Str(b)) = (op, &a, &b) {
            return Ok(Value::Str(Rc::from(format!("{a}{b}"))));
        }

        // Concatenation builds a new list rather than extending the left one.
        if let (Add, Value::List(a), Value::List(b)) = (op, &a, &b) {
            let mut items = self.heap.list(*a).clone();
            items.extend_from_slice(self.heap.list(*b));
            return Ok(Value::List(self.heap.alloc(Object::List(items))));
        }

        if let (Value::Str(x), Value::Str(y)) = (&a, &b) {
            return match op {
                Lt => Ok(Value::Bool(x < y)),
                Le => Ok(Value::Bool(x <= y)),
                Gt => Ok(Value::Bool(x > y)),
                Ge => Ok(Value::Bool(x >= y)),
                _ => Err(type_error(&self.heap, op, &lhs, &rhs, lhs_span, rhs_span, expr_span)),
            };
        }

        match (&a, &b) {
            // Both ints: stay an int, and refuse to wrap on overflow.
            (Value::Int(x), Value::Int(y)) => int_op(op, *x, *y, expr_span),

            // Any float involved promotes the whole operation.
            (Value::Float(_), Value::Int(_))
            | (Value::Int(_), Value::Float(_))
            | (Value::Float(_), Value::Float(_)) => float_op(op, as_float(&a), as_float(&b), expr_span),

            _ => Err(type_error(&self.heap, op, &lhs, &rhs, lhs_span, rhs_span, expr_span)),
        }
    }

    /// `<`, `<=`, `>` and `>=` when a class answers for one of the operands.
    ///
    /// `Ok(None)` means neither class does, and what the operands *are* decides —
    /// which is every comparison in a program that declares no ops at all.
    ///
    /// The operator's own op is asked first, so `op lt` beats `op cmp` for `<`.
    /// Only two of the four have one: `<=` and `>=` are `op cmp`'s alone. That is
    /// what makes declaring `op lt` give you `<` and nothing else, and it is not
    /// an omission — deriving `a <= b` from `not (a > b)` would assume the order
    /// is total, and `op cmp` exists precisely so a class can decline to be. C++
    /// draws the line in the same place: `operator<` alone leaves `a <= b` a
    /// compile error.
    pub(super) fn compare(
        &mut self,
        op: BinaryOp,
        lhs: &Value,
        rhs: &Value,
        spans: (Span, Span, Span),
    ) -> Result<Option<Value>> {
        let span = spans.2;
        let specific = match op {
            BinaryOp::Lt => Some(Op::Lt),
            BinaryOp::Gt => Some(Op::Gt),
            BinaryOp::Le | BinaryOp::Ge => None,
            // Not an ordering operator, so there is nothing here to ask about.
            _ => return Ok(None),
        };

        if let Some(specific) = specific
            && let Some((method, receiver, other, _)) = self.binary_slot(specific, lhs, rhs)
        {
            let method = self.binary_op_for(op, (specific, method, &receiver, &other), (lhs, rhs), spans)?;
            let answer = self.call_op(method, &receiver, vec![other])?;
            return match answer.base(&self.heap) {
                Value::Bool(b) => Ok(Some(Value::Bool(*b))),
                got => Err(self.op_returned(specific, &receiver, "a bool", got)),
            };
        }

        let Some((method, receiver, other, reflected)) = self.binary_slot(Op::Cmp, lhs, rhs) else {
            self.partly_ordered(op, lhs, rhs, span)?;
            // `5 < Money(3)` where `Money` declares `op lt`: the same mistake as
            // `2 - Money(3)`, and it gets the same explanation. An `op cmp` on the
            // right *would* have answered, which is why this is only reached once
            // that has been asked for and found missing.
            if let Some(specific) = specific {
                self.only_asks_the_left(specific, lhs, rhs, spans)?;
            }
            return Ok(None);
        };
        let method = self.binary_op_for(op, (Op::Cmp, method, &receiver, &other), (lhs, rhs), spans)?;
        let answer = self.call_op(method, &receiver, vec![other])?;
        let ordering = match answer.base(&self.heap) {
            // Any int, read for its sign: `-1`, `0` and `1` are the convention,
            // and `self.cents - other.cents` is what people actually write.
            Value::Int(n) => *n,
            got => return Err(self.op_returned(Op::Cmp, &receiver, "an int", got)),
        };
        // Reflected means the class was handed the operands the other way round,
        // so its answer is about the reverse question and the sign is backwards.
        // Saturating, because negating `i64::MIN` is not an int — and a program
        // is free to return one.
        let ordering = if reflected {
            ordering.saturating_neg()
        } else {
            ordering
        };
        let answer = match op {
            BinaryOp::Lt => ordering < 0,
            BinaryOp::Le => ordering <= 0,
            BinaryOp::Gt => ordering > 0,
            BinaryOp::Ge => ordering >= 0,
            _ => unreachable!("every other operator returned above"),
        };
        Ok(Some(Value::Bool(answer)))
    }

    /// Explains `a <= b` on a class that declared `op lt` or `op gt` and no
    /// `op cmp`.
    ///
    /// The plain "cannot compare" this would otherwise fall through to is true
    /// and useless: the class plainly does order itself for `<`, so the reader's
    /// conclusion is that their op was ignored. This is the one place the rule
    /// needs stating, so it gets stated here rather than in the documentation
    /// nobody is reading at the moment it happens.
    pub(super) fn partly_ordered(
        &mut self,
        op: BinaryOp,
        lhs: &Value,
        rhs: &Value,
        span: Span,
    ) -> Result<()> {
        let symbol = match op {
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            // `<` and `>` reaching here means no class answered for them either,
            // which is an ordinary type error and not this.
            _ => return Ok(()),
        };
        for side in [lhs, rhs] {
            for declared in [Op::Lt, Op::Gt] {
                if self.slot(side, declared).is_some() {
                    return Err(QuinceError::new(
                        format!(
                            "`{symbol}` needs `op cmp`, which {} does not declare",
                            side.type_name(&self.heap)
                        ),
                        span,
                    )
                    .with_kind(ErrorKind::Type)
                    .with_help(format!(
                        "`op {}` answers `{}` alone. `op cmp` answers all four \
                         comparisons at once, returning a negative int, zero, or a \
                         positive one",
                        declared.name(),
                        if declared == Op::Lt { "<" } else { ">" }
                    )));
                }
            }
        }
        Ok(())
    }

    /// `+`, `-`, `*`, `/`, `//` and `%` when the left operand's class answers.
    ///
    /// The left operand's, and only the left's. `2 - Money(3)` reaching `Money`'s
    /// `sub` would compute `3 - 2` and be wrong by a sign with nothing to catch
    /// it, so [`Op::reflect`] says `Never` for all seven and `binary_slot` never
    /// asks the right. Writing `2 - Money(3)` is a type error, which is the same
    /// answer C++ gives a class with a member `operator-` and no free function.
    ///
    /// Whatever the op returns is the value of the expression — no type is
    /// required, and none could be. A class extending `list` whose `+` appends
    /// returns another of itself, which is the entire point of declaring it.
    pub(super) fn arith(
        &mut self,
        op: BinaryOp,
        lhs: &Value,
        rhs: &Value,
        spans: (Span, Span, Span),
    ) -> Result<Option<Value>> {
        let slot = match op {
            BinaryOp::Add => Op::Add,
            BinaryOp::Sub => Op::Sub,
            BinaryOp::Mul => Op::Mul,
            BinaryOp::Div => Op::Div,
            BinaryOp::FloorDiv => Op::FloorDiv,
            BinaryOp::Rem => Op::Rem,
            BinaryOp::BitAnd => Op::BitAnd,
            BinaryOp::BitOr => Op::BitOr,
            BinaryOp::BitXor => Op::BitXor,
            BinaryOp::Shl => Op::BitShl,
            BinaryOp::Shr => Op::BitShr,
            BinaryOp::Pow => Op::Pow,
            // The comparisons and `in`, which are answered above.
            _ => return Ok(None),
        };
        let Some((method, receiver, other, _)) = self.binary_slot(slot, lhs, rhs) else {
            return self.only_asks_the_left(slot, lhs, rhs, spans).map(|()| None);
        };
        let chosen = self.binary_op_for(op, (slot, method, &receiver, &other), (lhs, rhs), spans)?;
        self.call_op(chosen, &receiver, vec![other]).map(Some)
    }

    /// Explains `2 - Money(3)`, where the class on the *right* is the one that
    /// declared the op.
    ///
    /// [`Reflect`] says why the right is not asked, and this is the moment a
    /// program finds out. Falling through to "cannot subtract int and Money" with
    /// "change the types" would be advice to go and rewrite a class that is
    /// already correct — the same failure the `<=` diagnostic exists to avoid.
    pub(super) fn only_asks_the_left(
        &mut self,
        slot: Op,
        lhs: &Value,
        rhs: &Value,
        spans: (Span, Span, Span),
    ) -> Result<()> {
        // Only the right, since the left having it is how we would not be here.
        if self.slot(rhs, slot).is_none() {
            return Ok(());
        }
        let (lhs_span, rhs_span, span) = spans;
        Err(QuinceError::new(
            format!(
                "`op {}` is {}'s, and the value on the left is the one asked",
                slot.name(),
                rhs.type_name(&self.heap),
            ),
            span,
        )
        .with_kind(ErrorKind::Type)
        // The whole diagnostic is about which side is which, so the two sides
        // are what the labels name. Saying it in the message and then drawing one
        // caret across both operands leaves the reader to work out which end the
        // sentence is talking about.
        .with_label(
            lhs_span,
            format!("{}, and this is the side asked", lhs.type_name(&self.heap)),
        )
        .with_label(
            rhs_span,
            format!("`op {}` is here", slot.name()),
        )
        .with_help(format!(
            "reaching {} from the right would hand it the two values the other way round, so it \
             is not asked — convert the {}, or swap the operands if that is what you meant",
            rhs.type_name(&self.heap),
            lhs.type_name(&self.heap),
        )))
    }

    /// `needle in haystack`.
    ///
    /// An unhashable needle is an error rather than a plain `false`, for the
    /// same reason `d[[]]` is: a value that could never have been inserted is a
    /// mistake in the program, and answering `false` would hide it.
    pub(super) fn contains(
        &mut self,
        haystack: &Value,
        needle: &Value,
        span: Span,
    ) -> Result<Value> {
        // The haystack's class first. `op contains` is the other half of `in`:
        // [`Op::Eq`] decides what a *list* holds by comparing items, and this
        // decides for a class that does its own looking — a range that answers
        // from two numbers rather than by storing what is between them.
        if let Some(method) = self.slot(haystack, Op::Contains) {
            let method = self.op_for(
                Op::Contains,
                method,
                haystack,
                std::slice::from_ref(needle),
                (span, span, span),
            )?;
            let answer = self.call_op(method, haystack, vec![needle.clone()])?;
            return match answer.base(&self.heap) {
                Value::Bool(b) => Ok(Value::Bool(*b)),
                got => Err(self.op_returned(Op::Contains, haystack, "a bool", got)),
            };
        }

        // Both sides unwrap: a subclass of `list` can be searched, and a subclass
        // of `string` can be the part searched for. `equals` and `key_of` unwrap the
        // needle for themselves, so only the string arm needs it named.
        //
        // Cloned rather than borrowed, because comparing an item is a call into
        // the interpreter and cannot hold a borrow of the heap across it.
        let base = haystack.base(&self.heap).clone();
        let found = match &base {
            Value::Dict(id) => self
                .heap
                .dict(*id)
                .contains(&key_of(&self.heap, needle, span)?),
            // By index rather than over an iterator, for the same reason: each
            // comparison can run a program's `op eq`, which is free to mutate the
            // very list being searched. `get` is what makes a list that shrank
            // mid-search end the search rather than panic.
            Value::List(id) => {
                let id = *id;
                let mut found = false;
                let mut i = 0;
                while let Some(item) = self.heap.list(id).get(i).cloned() {
                    if self.equals(&item, needle)? {
                        found = true;
                        break;
                    }
                    i += 1;
                }
                found
            }
            // As for a list, and by index for the same reason — `op eq` can
            // run anything, though it cannot shorten the tuple it is searching.
            Value::Tuple(id) => {
                let id = *id;
                let mut found = false;
                let mut i = 0;
                while let Some(item) = self.heap.tuple(id).get(i).cloned() {
                    if self.equals(&item, needle)? {
                        found = true;
                        break;
                    }
                    i += 1;
                }
                found
            }
            Value::Str(text) => match needle.base(&self.heap) {
                Value::Str(part) => text.contains(part.as_ref()),
                _ => {
                    return Err(QuinceError::new(
                        format!(
                            "cannot look for {} in a string",
                            needle.type_name(&self.heap)
                        ),
                        span,
                    )
                    .with_kind(ErrorKind::Type));
                }
            },
            _ => {
                return Err(QuinceError::new(
                    format!("cannot use `in` on {}", haystack.type_name(&self.heap)),
                    span,
                )
                .with_kind(ErrorKind::Type)
        .with_help("`in` searches a string, a list, or a dict's keys — a class answers for itself with `op contains`"));
            }
        };
        Ok(Value::Bool(found))
    }
}

pub(crate) fn as_float(value: &Value) -> f64 {
    match value {
        Value::Int(n) => *n as f64,
        Value::Float(n) => *n,
        _ => unreachable!("as_float is only reached for numbers"),
    }
}

/// Rounds toward negative infinity, which Rust's `/` does not — it truncates
/// toward zero, so `-7 / 2` is `-3` there but `-4` here.
///
/// `div_euclid` is not the same thing: it keeps the remainder non-negative, so
/// it disagrees whenever the divisor is negative.
pub(crate) fn floor_div(a: i64, b: i64) -> Option<i64> {
    let quotient = a.checked_div(b)?;
    if a % b != 0 && (a < 0) != (b < 0) {
        quotient.checked_sub(1)
    } else {
        Some(quotient)
    }
}

/// Integer arithmetic. Reports overflow rather than wrapping.
pub(crate) fn int_op(op: BinaryOp, a: i64, b: i64, span: Span) -> Result<Value> {
    use BinaryOp::*;
    let overflow = || QuinceError::new("integer overflow", span)
        .with_kind(ErrorKind::Overflow)
        .with_help("an int holds 64 bits — convert with `float(x)` for a wider range, at the cost of exactness");

    let value =
        match op {
            BitAnd => Value::Int(a & b),
            BitOr => Value::Int(a | b),
            BitXor => Value::Int(a ^ b),
            // A shift by a negative or oversized count has no answer worth
            // guessing at, so it is refused rather than wrapped to something
            // that looks deliberate. `checked_sh*` is what draws the line.
            Shl => Value::Int(
                u32::try_from(b)
                    .ok()
                    .and_then(|by| a.checked_shl(by))
                    .ok_or_else(|| shift_count(b, span))?,
            ),
            Shr => Value::Int(
                u32::try_from(b)
                    .ok()
                    .and_then(|by| a.checked_shr(by))
                    .ok_or_else(|| shift_count(b, span))?,
            ),
            Add => Value::Int(a.checked_add(b).ok_or_else(overflow)?),
            Sub => Value::Int(a.checked_sub(b).ok_or_else(overflow)?),
            Mul => Value::Int(a.checked_mul(b).ok_or_else(overflow)?),
            // An `int ** negative-int` answers a float, because the integer
            // result does not exist — the same rule `/` already follows and not
            // a new one. Overflow the other way stays checked.
            Pow if b < 0 => Value::Float((a as f64).powf(b as f64)),
            Pow => Value::Int(
                u32::try_from(b)
                    .ok()
                    .and_then(|by| a.checked_pow(by))
                    .ok_or_else(overflow)?,
            ),
            // True division always leaves the integers behind, so `1 / 2` is `0.5`
            // rather than `0`. `//` is there when an int is wanted.
            Div => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision)
                        .with_help(
                            "guard the divisor, or use `??` on a lookup that may be absent",
                        ));
                }
                Value::Float(a as f64 / b as f64)
            }
            FloorDiv => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision)
                        .with_help(
                            "guard the divisor, or use `??` on a lookup that may be absent",
                        ));
                }
                Value::Int(floor_div(a, b).ok_or_else(overflow)?)
            }
            Rem => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision)
                        .with_help(
                            "guard the divisor, or use `??` on a lookup that may be absent",
                        ));
                }
                Value::Int(a.checked_rem(b).ok_or_else(overflow)?)
            }
            Lt => Value::Bool(a < b),
            Le => Value::Bool(a <= b),
            Gt => Value::Bool(a > b),
            Ge => Value::Bool(a >= b),
            Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
        };
    Ok(value)
}

/// A shift count a machine word cannot answer for.
fn shift_count(by: i64, span: Span) -> crate::error::Raised {
    QuinceError::new(format!("cannot shift by {by}"), span)
        .with_kind(ErrorKind::Value)
        .with_help("a shift count is between 0 and 63 — an int has 64 bits")
}

pub(crate) fn float_op(op: BinaryOp, a: f64, b: f64, span: Span) -> Result<Value> {
    use BinaryOp::*;
    let value = match op {
        // A float has no bits to combine that mean anything. Refused here so the
        // report names the operator rather than leaving the caller to guess.
        BitAnd | BitOr | BitXor | Shl | Shr => {
            return Err(QuinceError::new(
                "a float has no bits to operate on",
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("convert it with `int(x)` first"));
        }
        Add => Value::Float(a + b),
        Sub => Value::Float(a - b),
        Mul => Value::Float(a * b),
        Pow => Value::Float(a.powf(b)),
        // Kept an error rather than yielding infinity, to match integer division.
        Div if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span)
                    .with_kind(ErrorKind::ZeroDivision)
                    .with_help("guard the divisor, or use `??` on a lookup that may be absent")
            );
        }
        Div => Value::Float(a / b),
        FloorDiv if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span)
                    .with_kind(ErrorKind::ZeroDivision)
                    .with_help("guard the divisor, or use `??` on a lookup that may be absent")
            );
        }
        FloorDiv => Value::Float((a / b).floor()),
        Rem if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span)
                    .with_kind(ErrorKind::ZeroDivision)
                    .with_help("guard the divisor, or use `??` on a lookup that may be absent")
            );
        }
        Rem => Value::Float(a % b),
        Lt => Value::Bool(a < b),
        Le => Value::Bool(a <= b),
        Gt => Value::Bool(a > b),
        Ge => Value::Bool(a >= b),
        Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
    };
    Ok(value)
}

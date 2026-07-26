use std::fmt;
use std::rc::Rc;

use crate::ast::FnDecl;
use crate::class::Builtin;
use crate::color::Style;
use crate::error::QuinceError;
use crate::heap::{Heap, ObjId};
use crate::interp::Interp;
use crate::token::Span;

/// A user-defined function together with the scope it closed over.
#[derive(Clone, Debug)]
pub struct Function {
    pub decl: Rc<FnDecl>,
    pub env: ObjId,
}

/// The signature every builtin implements.
///
/// The whole interpreter is threaded through rather than just the heap, so that
/// a builtin can call back into Quince. Nothing does yet, but anything taking a
/// function argument — `sort` with a key, `map`, `filter` — needs to, as does
/// every method on a user-defined class. Widening the signature costs one line
/// per builtin today and grows with the method table, so it is done early.
///
/// Output is reached through the interpreter too, which is what lets tests
/// capture what a program prints.
pub type NativeFn = fn(&mut Interp, &[Value], Span) -> Result<Value, QuinceError>;

/// A method that has found its receiver but has not been called yet.
///
/// Only produced by a bare `x.push`, since `x.push(1)` dispatches without
/// building one. It exists so that a method is an ordinary value — passable,
/// storable, callable later — rather than syntax that only works in call
/// position.
#[derive(Clone, Debug)]
pub struct BoundMethod {
    pub receiver: Value,
    /// A `Native` for a builtin type's method, a `Function` for a user class's.
    /// Holding the `Value` rather than either concrete type is what keeps one
    /// bound-method object serving both.
    pub method: Value,
}

/// A function implemented in Rust.
pub struct Native {
    pub name: &'static str,
    /// `None` for variadic builtins such as `print`.
    pub arity: Option<usize>,
    pub func: NativeFn,
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Native({})", self.name)
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Strings are immutable and cannot form cycles, so they are reference
    /// counted rather than placed in the heap. Keeping them inline means
    /// printing or comparing a string does not need the heap threaded through.
    Str(Rc<str>),
    List(ObjId),
    Dict(ObjId),
    Function(ObjId),
    Native(&'static Native),
    /// Heap-allocated because it holds a receiver, which may be a handle the
    /// collector has to follow.
    BoundMethod(ObjId),
    /// A class, which is a value: it can be bound, passed, and called to build
    /// an instance.
    Class(ObjId),
    Instance(ObjId),
}

impl Value {
    /// The heap object this value refers to, if any.
    ///
    /// The collector's view of a value: everything else is inline and cannot
    /// keep anything alive.
    pub fn handle(&self) -> Option<ObjId> {
        match self {
            Value::List(id)
            | Value::Dict(id)
            | Value::Function(id)
            | Value::BoundMethod(id)
            | Value::Class(id)
            | Value::Instance(id) => Some(*id),
            _ => None,
        }
    }

    /// The type this value belongs to.
    ///
    /// Many-to-one, and deliberately so: a Quince function and a builtin are
    /// both `function`, because nothing a program can do distinguishes them.
    /// This is the one place that decision is recorded, and it is also where
    /// methods are found.
    ///
    /// A handle for every value, not just an instance: the builtin types are
    /// class objects like any other, so the match picks an index into the heap's
    /// table rather than a `&'static`. That is what a program needs in order to
    /// name `int` at all, and eventually to extend it.
    pub fn class(&self, heap: &Heap) -> ObjId {
        let builtin = match self {
            Value::Nil => Builtin::Nil,
            Value::Bool(_) => Builtin::Bool,
            Value::Int(_) => Builtin::Int,
            Value::Float(_) => Builtin::Float,
            Value::Str(_) => Builtin::Str,
            Value::List(_) => Builtin::List,
            Value::Dict(_) => Builtin::Dict,
            Value::Function(_) | Value::Native(_) | Value::BoundMethod(_) => Builtin::Function,
            Value::Class(_) => Builtin::Class,
            Value::Instance(id) => return heap.instance(*id).class,
        };
        heap.builtin_class(builtin)
    }

    /// The name used in type errors and by `type(x)`.
    ///
    /// Never unwrapped to a payload: `type(Username("marc"))` is `Username`, which
    /// is the whole reason someone declared the class.
    pub fn type_name<'h>(&self, heap: &'h Heap) -> &'h str {
        &heap.class(self.class(heap)).name
    }

    /// The value an operator should act on: an instance's payload, if it has one.
    ///
    /// A class extending a builtin *is* that builtin, so `+`, `==`, `len`,
    /// indexing, iteration and printing have to see the string or the list rather
    /// than the object carrying it. Methods do not come through here — they are
    /// substituted at `call_method`, which is the one place a receiver is inserted.
    ///
    /// One level and no more: a conversion only ever produces a base value, so a
    /// payload is never itself an instance.
    ///
    /// An instance with no payload gets itself back. That is what leaves every
    /// class extending nothing behaving exactly as it did — compared by identity,
    /// always truthy, printed as `<Box instance>`.
    pub fn base<'a>(&'a self, heap: &'a Heap) -> &'a Value {
        match self {
            Value::Instance(id) => heap.instance(*id).payload.as_ref().unwrap_or(self),
            _ => self,
        }
    }

    /// Python-style truthiness: `nil`, `false`, zero, and empty collections are
    /// falsy; everything else is truthy.
    pub fn is_truthy(&self, heap: &Heap) -> bool {
        match self.base(heap) {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(id) => !heap.list(*id).is_empty(),
            Value::Dict(id) => !heap.dict(*id).is_empty(),
            Value::Function(_) | Value::Native(_) | Value::BoundMethod(_) => true,
            // An instance carrying no payload is truthy regardless of its fields:
            // there is nothing to ask, since a class cannot yet answer for itself.
            // One extending a builtin was unwrapped above, so `Username("")` is
            // falsy exactly as `""` is.
            Value::Class(_) | Value::Instance(_) => true,
        }
    }

    /// Structural equality.
    ///
    /// Numbers compare across `int` and `float`, since they are one numeric
    /// tower, but no other pair of types is ever equal — `1 == "1"` is `false`
    /// rather than a coercion.
    pub fn equals(&self, other: &Value, heap: &Heap) -> bool {
        match (self.base(heap), other.base(heap)) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => *a as f64 == *b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                if a == b {
                    return true;
                }
                let (a, b) = (heap.list(*a), heap.list(*b));
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.equals(y, heap))
            }
            // Order is not part of a dict's identity, only its contents:
            // `{"a": 1, "b": 2}` equals `{"b": 2, "a": 1}`.
            (Value::Dict(a), Value::Dict(b)) => {
                if a == b {
                    return true;
                }
                let (a, b) = (heap.dict(*a), heap.dict(*b));
                a.len() == b.len()
                    && a.iter().all(|(key, value)| {
                        b.get(key).is_some_and(|other| value.equals(other, heap))
                    })
            }
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
                    return true;
                }
                let (a, b) = (heap.bound_method(*a), heap.bound_method(*b));
                a.method == b.method && a.receiver == b.receiver
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
            _ => false,
        }
    }

    /// How a value prints. `Nil` shows as `nil` so it is visible in output.
    pub fn display(&self, heap: &Heap) -> String {
        self.display_styled(heap, false)
    }

    /// How a value prints with optional ANSI syntax highlighting.
    pub fn display_styled(&self, heap: &Heap, color: bool) -> String {
        match self.base(heap) {
            Value::Nil => Style::DIM.paint("nil", color),
            Value::Bool(b) => Style::YELLOW.paint(b, color),
            Value::Int(n) => Style::CYAN.paint(n, color),
            // Keeps floats distinguishable from ints in output: `1.0`, not `1`.
            Value::Float(n) if n.fract() == 0.0 && n.is_finite() => {
                Style::CYAN.paint(format!("{n:.1}"), color)
            }
            Value::Float(n) => Style::CYAN.paint(n, color),
            Value::Str(s) => Style::GREEN.paint(s, color),
            Value::List(id) => {
                let items: Vec<_> = heap
                    .list(*id)
                    .iter()
                    .map(|v| v.repr_styled(heap, color))
                    .collect();
                format!(
                    "{}{}{}",
                    Style::BOLD.paint("[", color),
                    items.join(", "),
                    Style::BOLD.paint("]", color)
                )
            }
            Value::Dict(id) => {
                let entries: Vec<_> = heap
                    .dict(*id)
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}: {}",
                            key.to_value().repr_styled(heap, color),
                            value.repr_styled(heap, color)
                        )
                    })
                    .collect();
                format!(
                    "{}{}{}",
                    Style::BOLD.paint("{", color),
                    entries.join(", "),
                    Style::BOLD.paint("}", color)
                )
            }
            Value::Function(id) => {
                Style::MAGENTA.paint(format!("<fn {}>", heap.function(*id).decl.name), color)
            }
            Value::Native(native) => {
                Style::MAGENTA.paint(format!("<builtin {}>", native.name), color)
            }
            Value::BoundMethod(id) => {
                let bound = heap.bound_method(*id);
                Style::MAGENTA.paint(
                    format!(
                        "<method {} of {}>",
                        bound.method.callable_name(heap),
                        bound.receiver.type_name(heap)
                    ),
                    color,
                )
            }
            Value::Class(id) => {
                Style::MAGENTA.paint(format!("<class {}>", heap.class(*id).name), color)
            }
            Value::Instance(_) => {
                Style::MAGENTA.paint(format!("<{} instance>", self.type_name(heap)), color)
            }
        }
    }

    /// How a value prints with multiline formatting for large or nested collections.
    pub fn display_pretty(&self, heap: &Heap, color: bool) -> String {
        let unstyled = self.display_styled(heap, false);
        if unstyled.len() <= 80 && !unstyled.contains('\n') {
            return self.display_styled(heap, color);
        }
        match self.base(heap) {
            Value::List(_) | Value::Dict(_) => self.format_pretty(heap, color, 0),
            _ => self.display_styled(heap, color),
        }
    }

    fn format_pretty(&self, heap: &Heap, color: bool, indent: usize) -> String {
        let pad = "    ".repeat(indent);
        let inner_pad = "    ".repeat(indent + 1);
        match self.base(heap) {
            Value::List(id) => {
                let items = heap.list(*id);
                if items.is_empty() {
                    return format!(
                        "{}{}",
                        Style::BOLD.paint("[", color),
                        Style::BOLD.paint("]", color)
                    );
                }
                let is_flat = items.iter().all(|item| {
                    !matches!(item.base(heap), Value::List(_) | Value::Dict(_))
                });

                if is_flat {
                    let mut lines = Vec::new();
                    let mut current_line = String::from(&inner_pad);
                    let mut current_len = inner_pad.len();
                    let max_width = 80;

                    for (i, item) in items.iter().enumerate() {
                        let item_unstyled = item.repr(heap);
                        let item_styled = item.repr_styled(heap, color);
                        let comma = if i + 1 < items.len() { "," } else { "" };
                        let sep_len = if i + 1 < items.len() { 2 } else { 0 };

                        if current_len > inner_pad.len()
                            && current_len + item_unstyled.len() + comma.len() > max_width
                        {
                            lines.push(current_line);
                            current_line = format!("{inner_pad}{item_styled}{comma}");
                            current_len = inner_pad.len() + item_unstyled.len() + comma.len();
                        } else {
                            if current_len > inner_pad.len() {
                                current_line.push(' ');
                            }
                            current_line.push_str(&item_styled);
                            if !comma.is_empty() {
                                current_line.push(',');
                            }
                            current_len += item_unstyled.len() + sep_len;
                        }
                    }
                    if !current_line.is_empty() {
                        lines.push(current_line);
                    }

                    format!(
                        "{}\n{}\n{}{}",
                        Style::BOLD.paint("[", color),
                        lines.join("\n"),
                        pad,
                        Style::BOLD.paint("]", color)
                    )
                } else {
                    let mut lines = Vec::new();
                    for item in items {
                        let formatted = match item.base(heap) {
                            Value::List(_) | Value::Dict(_) => {
                                item.format_pretty(heap, color, indent + 1)
                            }
                            _ => format!("{inner_pad}{}", item.repr_styled(heap, color)),
                        };
                        lines.push(formatted);
                    }
                    format!(
                        "{}\n{}\n{}{}",
                        Style::BOLD.paint("[", color),
                        lines.join(",\n"),
                        pad,
                        Style::BOLD.paint("]", color)
                    )
                }
            }
            Value::Dict(id) => {
                let dict = heap.dict(*id);
                if dict.is_empty() {
                    return format!(
                        "{}{}",
                        Style::BOLD.paint("{", color),
                        Style::BOLD.paint("}", color)
                    );
                }
                let mut lines = Vec::new();
                for (key, val) in dict.iter() {
                    let key_str = key.to_value().repr_styled(heap, color);
                    let val_str = match val.base(heap) {
                        Value::List(_) | Value::Dict(_) => {
                            val.format_pretty(heap, color, indent + 1)
                        }
                        _ => val.repr_styled(heap, color),
                    };
                    lines.push(format!("{inner_pad}{key_str}: {val_str}"));
                }
                format!(
                    "{}\n{}\n{}{}",
                    Style::BOLD.paint("{", color),
                    lines.join(",\n"),
                    pad,
                    Style::BOLD.paint("}", color)
                )
            }
            _ => format!("{pad}{}", self.display_styled(heap, color)),
        }
    }

    /// The name to print for something callable, which is the only thing the
    /// three callable forms have in common.
    pub fn callable_name<'h>(&self, heap: &'h Heap) -> &'h str {
        match self {
            Value::Native(native) => native.name,
            Value::Function(id) => &heap.function(*id).decl.name,
            other => other.type_name(heap),
        }
    }

    /// How a value prints when nested inside a collection, where strings need
    /// quoting to stay distinguishable from bare identifiers. Also what error
    /// messages use when they name a value rather than a type.
    pub fn repr(&self, heap: &Heap) -> String {
        self.repr_styled(heap, false)
    }

    /// How a value prints inside collections with optional ANSI colors.
    pub fn repr_styled(&self, heap: &Heap, color: bool) -> String {
        // A payload-carrying instance reprs as its base type, so `[Username("marc")]`
        // shows `["marc"]`. Nothing in the output distinguishes it from a plain
        // string, which is the same trade Python makes: `repr` stays the literal you
        // would write, and `type(x)` is how you ask what class it is.
        match self.base(heap) {
            Value::Str(s) => Style::GREEN.paint(format!("{s:?}"), color),
            other => other.display_styled(heap, color),
        }
    }
}

/// Only used by tests and assertions; the evaluator compares through `equals`
/// so it can reach the heap.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            (Value::Native(a), Value::Native(b)) => std::ptr::eq(*a, *b),
            (Value::BoundMethod(a), Value::BoundMethod(b)) => a == b,
            (Value::Class(a), Value::Class(b)) => a == b,
            (Value::Instance(a), Value::Instance(b)) => a == b,
            _ => false,
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(Rc::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::Dict;
    use crate::heap::Object;

    static DUMMY: Native = Native {
        name: "dummy",
        arity: None,
        func: |_interp, _args, _span| Ok(Value::Nil),
    };

    /// Whether `value` belongs to exactly `expected`, compared by handle so that
    /// two types sharing a name would still fail.
    fn is_builtin(value: &Value, expected: Builtin, heap: &Heap) -> bool {
        value.class(heap) == heap.builtin_class(expected)
    }

    #[test]
    fn type_names_are_stable() {
        let mut heap = Heap::new();
        assert_eq!(Value::Nil.type_name(&heap), "nil");
        assert_eq!(Value::Bool(true).type_name(&heap), "bool");
        assert_eq!(Value::Int(1).type_name(&heap), "int");
        assert_eq!(Value::Float(1.0).type_name(&heap), "float");
        assert_eq!(Value::from("a").type_name(&heap), "string");

        let list = Value::List(heap.alloc(Object::List(vec![])));
        let dict = Value::Dict(heap.alloc(Object::Dict(Dict::new())));
        assert_eq!(list.type_name(&heap), "list");
        assert_eq!(dict.type_name(&heap), "dict");
    }

    #[test]
    fn every_value_maps_to_its_own_type() {
        // `class` is a hand-written table, so a value pointing at the wrong
        // entry is a plausible mistake that `type_name` alone would not catch:
        // two types could share a name and still be distinct.
        let mut heap = Heap::new();
        let list = Value::List(heap.alloc(Object::List(vec![])));
        let dict = Value::Dict(heap.alloc(Object::Dict(Dict::new())));

        assert!(is_builtin(&Value::Int(1), Builtin::Int, &heap));
        assert!(is_builtin(&list, Builtin::List, &heap));
        assert!(is_builtin(&dict, Builtin::Dict, &heap));
        assert!(!is_builtin(&list, Builtin::Dict, &heap));
    }

    #[test]
    fn builtins_and_quince_functions_share_one_type() {
        // The many-to-one case: nothing a program can do tells them apart, so
        // they must not report different types.
        let heap = Heap::new();
        assert_eq!(Value::Native(&DUMMY).type_name(&heap), "function");
        assert!(is_builtin(&Value::Native(&DUMMY), Builtin::Function, &heap));
    }

    #[test]
    fn truthiness_follows_python() {
        let mut heap = Heap::new();
        let empty = Value::List(heap.alloc(Object::List(vec![])));
        let full = Value::List(heap.alloc(Object::List(vec![Value::Int(1)])));

        assert!(!Value::Nil.is_truthy(&heap));
        assert!(!Value::Bool(false).is_truthy(&heap));
        assert!(!Value::Int(0).is_truthy(&heap));
        assert!(!Value::Float(0.0).is_truthy(&heap));
        assert!(!Value::from("").is_truthy(&heap));
        assert!(!empty.is_truthy(&heap));

        assert!(Value::Bool(true).is_truthy(&heap));
        assert!(Value::Int(1).is_truthy(&heap));
        assert!(Value::Int(-1).is_truthy(&heap));
        assert!(Value::from("a").is_truthy(&heap));
        assert!(full.is_truthy(&heap));
    }

    #[test]
    fn numbers_compare_across_int_and_float() {
        let heap = Heap::new();
        assert!(Value::Int(1).equals(&Value::Float(1.0), &heap));
        assert!(Value::Float(1.0).equals(&Value::Int(1), &heap));
        assert!(!Value::Int(1).equals(&Value::Float(1.5), &heap));
    }

    #[test]
    fn unrelated_types_are_never_equal() {
        // Strong typing: no coercion sneaks in through `==`.
        let heap = Heap::new();
        assert!(!Value::Int(1).equals(&Value::from("1"), &heap));
        assert!(!Value::Int(1).equals(&Value::Bool(true), &heap));
        assert!(!Value::Nil.equals(&Value::Bool(false), &heap));
    }

    #[test]
    fn lists_compare_structurally() {
        let mut heap = Heap::new();
        let a = Value::List(heap.alloc(Object::List(vec![Value::Int(1), Value::from("x")])));
        let b = Value::List(heap.alloc(Object::List(vec![Value::Int(1), Value::from("x")])));
        let c = Value::List(heap.alloc(Object::List(vec![Value::Int(2)])));
        assert!(a.equals(&b, &heap));
        assert!(!a.equals(&c, &heap));
    }

    #[test]
    fn identical_handles_short_circuit_comparison() {
        // Guards the self-referential case from running forever.
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id)
            .expect("never frozen here")
            .push(Value::List(id));
        assert!(Value::List(id).equals(&Value::List(id), &heap));
    }

    #[test]
    fn floats_stay_distinguishable_from_ints_when_printed() {
        let heap = Heap::new();
        assert_eq!(Value::Int(1).display(&heap), "1");
        assert_eq!(Value::Float(1.0).display(&heap), "1.0");
        assert_eq!(Value::Float(1.5).display(&heap), "1.5");
    }

    #[test]
    fn strings_print_bare_but_quote_inside_lists() {
        let mut heap = Heap::new();
        assert_eq!(Value::from("hi").display(&heap), "hi");
        let list = Value::List(heap.alloc(Object::List(vec![Value::from("hi"), Value::Int(2)])));
        assert_eq!(list.display(&heap), r#"["hi", 2]"#);
    }

    #[test]
    fn short_character_lists_print_on_single_line_in_display_pretty() {
        let mut heap = Heap::new();
        let items: Vec<Value> = "marc@gmail.com"
            .chars()
            .map(|c| Value::from(c.to_string().as_str()))
            .collect();
        let list = Value::List(heap.alloc(Object::List(items)));

        let printed = list.display_pretty(&heap, false);
        assert_eq!(
            printed,
            r#"["m", "a", "r", "c", "@", "g", "m", "a", "i", "l", ".", "c", "o", "m"]"#
        );
    }
}

use std::fmt;
use std::rc::Rc;

use crate::ast::FnDecl;
use crate::class::Builtin;
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
    /// The type this always produces, or `None` for one whose answer depends on
    /// what it was given.
    ///
    /// Here so that the inference pass can read it. Before this existed every
    /// call into the library was `Unknown` to it, which meant the editor fell
    /// back to guessing and guessed wrong — `"a,b".split(",")` is a list, and a
    /// heuristic reading the literal at the front of the line called it a
    /// string.
    ///
    /// `None` is a real answer and has to stay available. `abs` keeps the
    /// int-ness of what it was handed, `dict.get` returns whatever was stored,
    /// and `io.line` is a string until input runs out and then it is `nil`.
    /// Naming a type for those would be the same lie in a more authoritative
    /// place — the whole value of the field is that it is trusted, so it must
    /// only be filled in where it is certain.
    pub returns: Option<Builtin>,
    /// What this function is for, in the words a reader wants at the moment
    /// they hover over it.
    ///
    /// Beside the implementation rather than in `lsp.rs`, which is where it
    /// used to live as a hand-written table of ten entries — a table that could
    /// not describe the other forty-two and had no way to notice it was wrong.
    /// A field with no default is what makes documenting a new builtin part of
    /// adding one.
    pub doc: &'static str,
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
    /// An imported module, which is a value for the same reasons a class is: it
    /// can be bound, passed, and stored. The handle is an [`crate::env::Globals`]
    /// — a module and a top-level scope are the same object, so `math.floor` is a
    /// name looked up in a scope and nothing new had to be built to hold one.
    Module(ObjId),
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
            | Value::Instance(id)
            | Value::Module(id) => Some(*id),
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
            Value::Module(_) => Builtin::Module,
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

    // -- the base family ---------------------------------------------------
    //
    // What a value *is*, decided by matching on it and reaching no further. The
    // `_base` in the name is [`Value::base`]: a payload is unwrapped, and the
    // class is never asked, so none of these can run Quince code, allocate, or
    // fail.
    //
    // Which is why they are the ones error messages use. A message about a value
    // must not consult the class it belongs to — an `op string` that itself
    // raises would mean a second error while reporting the first, and when the
    // broken op *is* the bug, a loop. See the note at the `frozen` helper in
    // interp.rs: a worse message beats that trade every time.
    //
    // Everything else goes through the [`crate::interp::Interp`] methods of the
    // same names, which do ask the class, and so take `&mut Interp` and return a
    // `Result`.
    //
    // Equality is deliberately not among them. It is the one question no message
    // asks — a report names a value, it never compares two — so a base half would
    // exist only to be a second implementation of `==`, and the two would drift.
    // `Interp::equals` is the only one.

    /// Python-style truthiness: `nil`, `false`, zero, and empty collections are
    /// falsy; everything else is truthy.
    pub fn is_truthy_base(&self, heap: &Heap) -> bool {
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
            // A module is always truthy, empty or not. `if math` asks whether the
            // module exists, and by the time there is a value to ask about, it
            // does — an import that found nothing raised instead of binding.
            Value::Class(_) | Value::Instance(_) | Value::Module(_) => true,
        }
    }

    /// How a value prints, deciding it by matching and asking nothing.
    ///
    /// The renderer error messages use. Unstyled and single-line, because no
    /// message needs colour or a collection broken over lines — and small for the
    /// same reason, since the cost of a second implementation is two places that
    /// decide how a float looks. `Interp::display` is the real one, and
    /// `the_base_renderer_agrees_with_this_one` holds the two together.
    pub fn display_base(&self, heap: &Heap) -> String {
        match self.base(heap) {
            Value::Nil => "nil".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            // Keeps floats distinguishable from ints in output: `1.0`, not `1`.
            Value::Float(n) if n.fract() == 0.0 && n.is_finite() => format!("{n:.1}"),
            Value::Float(n) => n.to_string(),
            Value::Str(s) => s.to_string(),
            Value::List(id) => {
                let items: Vec<_> = heap.list(*id).iter().map(|v| v.repr_base(heap)).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Dict(id) => {
                let entries: Vec<_> = heap
                    .dict(*id)
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}: {}",
                            key.to_value().repr_base(heap),
                            value.repr_base(heap)
                        )
                    })
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Value::Function(id) => format!("<fn {}>", heap.function(*id).decl.name),
            Value::Native(native) => format!("<builtin {}>", native.name),
            Value::BoundMethod(id) => {
                let bound = heap.bound_method(*id);
                format!(
                    "<method {} of {}>",
                    bound.method.callable_name(heap),
                    bound.receiver.type_name(heap)
                )
            }
            Value::Class(id) => format!("<class {}>", heap.class(*id).name),
            Value::Module(id) => match heap.globals(*id).name() {
                Some(name) => format!("<module {name}>"),
                // The starting module is never imported, so nothing names it and
                // nothing holds it as a value. Reachable only if that stops being
                // true, and a bare `<module>` is the honest thing to say if it is.
                None => "<module>".to_string(),
            },
            // Not the base's type name but this value's: an instance carrying no
            // payload is its own base, and what it should say is its class.
            Value::Instance(_) => format!("<{} instance>", self.type_name(heap)),
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

    /// How a value prints when nested inside a collection, where a string needs
    /// quoting to stay distinguishable from a bare identifier. What error
    /// messages use when they name a value rather than a type.
    pub fn repr_base(&self, heap: &Heap) -> String {
        // A payload-carrying instance reprs as its base type, so `[Username("marc")]`
        // shows `["marc"]`. Nothing in the output distinguishes it from a plain
        // string, which is the same trade Python makes: `repr` stays the literal you
        // would write, and `type(x)` is how you ask what class it is.
        match self.base(heap) {
            Value::Str(s) => format!("{s:?}"),
            _ => self.display_base(heap),
        }
    }
}

/// Only used by tests and assertions; the evaluator compares through
/// `Interp::equals` so it can reach the heap and the class.
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
            (Value::Module(a), Value::Module(b)) => a == b,
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
        returns: None,
        doc: "A stand-in for a test, which needs a native and not what it does.",
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

        assert!(!Value::Nil.is_truthy_base(&heap));
        assert!(!Value::Bool(false).is_truthy_base(&heap));
        assert!(!Value::Int(0).is_truthy_base(&heap));
        assert!(!Value::Float(0.0).is_truthy_base(&heap));
        assert!(!Value::from("").is_truthy_base(&heap));
        assert!(!empty.is_truthy_base(&heap));

        assert!(Value::Bool(true).is_truthy_base(&heap));
        assert!(Value::Int(1).is_truthy_base(&heap));
        assert!(Value::Int(-1).is_truthy_base(&heap));
        assert!(Value::from("a").is_truthy_base(&heap));
        assert!(full.is_truthy_base(&heap));
    }

    #[test]
    fn floats_stay_distinguishable_from_ints_when_printed() {
        let heap = Heap::new();
        assert_eq!(Value::Int(1).display_base(&heap), "1");
        assert_eq!(Value::Float(1.0).display_base(&heap), "1.0");
        assert_eq!(Value::Float(1.5).display_base(&heap), "1.5");
    }

    #[test]
    fn strings_print_bare_but_quote_inside_lists() {
        let mut heap = Heap::new();
        assert_eq!(Value::from("hi").display_base(&heap), "hi");
        let list = Value::List(heap.alloc(Object::List(vec![Value::from("hi"), Value::Int(2)])));
        assert_eq!(list.display_base(&heap), r#"["hi", 2]"#);
    }

}

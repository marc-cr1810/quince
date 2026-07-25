use std::fmt;
use std::rc::Rc;

use crate::ast::FnDecl;
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
}

impl Value {
    /// The heap object this value refers to, if any.
    ///
    /// The collector's view of a value: everything else is inline and cannot
    /// keep anything alive.
    pub fn handle(&self) -> Option<ObjId> {
        match self {
            Value::List(id) | Value::Dict(id) | Value::Function(id) => Some(*id),
            _ => None,
        }
    }

    /// The name used in type errors. Kept in one place so messages stay
    /// consistent as variants are added.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Function(_) | Value::Native(_) => "function",
        }
    }

    /// Python-style truthiness: `nil`, `false`, zero, and empty collections are
    /// falsy; everything else is truthy.
    pub fn is_truthy(&self, heap: &Heap) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(id) => !heap.list(*id).is_empty(),
            Value::Dict(id) => !heap.dict(*id).is_empty(),
            Value::Function(_) | Value::Native(_) => true,
        }
    }

    /// Structural equality.
    ///
    /// Numbers compare across `int` and `float`, since they are one numeric
    /// tower, but no other pair of types is ever equal — `1 == "1"` is `false`
    /// rather than a coercion.
    pub fn equals(&self, other: &Value, heap: &Heap) -> bool {
        match (self, other) {
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
            _ => false,
        }
    }

    /// How a value prints. `Nil` shows as `nil` so it is visible in output.
    pub fn display(&self, heap: &Heap) -> String {
        match self {
            Value::Nil => "nil".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            // Keeps floats distinguishable from ints in output: `1.0`, not `1`.
            Value::Float(n) if n.fract() == 0.0 && n.is_finite() => format!("{n:.1}"),
            Value::Float(n) => n.to_string(),
            Value::Str(s) => s.to_string(),
            Value::List(id) => {
                let items: Vec<_> = heap.list(*id).iter().map(|v| v.repr(heap)).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Dict(id) => {
                let entries: Vec<_> = heap
                    .dict(*id)
                    .iter()
                    .map(|(key, value)| {
                        format!("{}: {}", key.to_value().repr(heap), value.repr(heap))
                    })
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
            Value::Function(id) => format!("<fn {}>", heap.function(*id).decl.name),
            Value::Native(native) => format!("<builtin {}>", native.name),
        }
    }

    /// How a value prints when nested inside a collection, where strings need
    /// quoting to stay distinguishable from bare identifiers. Also what error
    /// messages use when they name a value rather than a type.
    pub fn repr(&self, heap: &Heap) -> String {
        match self {
            Value::Str(s) => format!("{s:?}"),
            other => other.display(heap),
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
    use crate::heap::Object;

    #[test]
    fn type_names_are_stable() {
        let heap = Heap::new();
        let _ = &heap;
        assert_eq!(Value::Nil.type_name(), "nil");
        assert_eq!(Value::Int(1).type_name(), "int");
        assert_eq!(Value::Float(1.0).type_name(), "float");
        assert_eq!(Value::from("a").type_name(), "string");
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
        heap.list_mut(id).push(Value::List(id));
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
}

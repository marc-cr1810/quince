//! The types a value can belong to.
//!
//! Every value names a type, and a type is where behaviour that cannot be
//! decided by matching on a `Value` will hang — methods first, then the
//! protocol slots a user-defined class overrides. Builtin types are static, so
//! naming one costs nothing and allocates nothing.
//!
//! A class defined in Quince is an object instead, which is the only difference
//! [`Class`] exists to absorb. Protocol slots — indexing, iteration, `len`,
//! `in` — are still matches on `Value`; see Dispatch in DESIGN.md for why.

use std::collections::HashMap;

use crate::dict::Dict;
use crate::heap::{Heap, ObjId};
use crate::interp::{
    CHARS, ENDS_WITH, JOIN, KEYS, LOWER, PUSH, REMOVE, REPLACE, SPLIT, STARTS_WITH, TRIM, UPPER,
    VALUES,
};
use crate::value::{Native, Value};

/// The type a value belongs to.
///
/// Two representations rather than one, because a builtin type is known at
/// compile time and allocates nothing, while a user class is an object with a
/// lifetime. Method lookup is the only thing that has to care about the
/// difference, and it collapses them immediately — see [`Class::method`].
#[derive(Clone, Copy, Debug)]
pub enum Class {
    Builtin(&'static BuiltinType),
    User(ObjId),
}

impl Class {
    pub fn name<'h>(&self, heap: &'h Heap) -> &'h str {
        match self {
            Class::Builtin(builtin) => builtin.name,
            Class::User(id) => &heap.class(*id).name,
        }
    }

    /// The method `name`, ready to be bound to a receiver.
    ///
    /// Both arms hand back a `Value`, so everything downstream — binding,
    /// calling, printing, comparing — has one case to handle rather than two.
    pub fn method(&self, name: &str, heap: &Heap) -> Option<Value> {
        match self {
            Class::Builtin(builtin) => builtin.method(name).map(Value::Native),
            Class::User(id) => heap.class(*id).method(name, heap),
        }
    }
}

/// A type built into the language.
#[derive(Debug)]
pub struct BuiltinType {
    /// What the type is called in error messages and in `type(x)`.
    pub name: &'static str,
    /// The methods callable on a value of this type, looked up by name.
    ///
    /// A slice rather than a map: these are single digits long, a linear scan
    /// over contiguous memory beats hashing at that size, and a `static` needs
    /// no construction at startup.
    pub methods: &'static [(&'static str, &'static Native)],
}

impl BuiltinType {
    pub fn method(&self, name: &str) -> Option<&'static Native> {
        self.methods
            .iter()
            .find(|(method, _)| *method == name)
            .map(|(_, native)| *native)
    }
}

/// A class defined in Quince.
///
/// Unlike a [`BuiltinType`] the method table is a map: a class body has no
/// practical size limit, and its entries are `String`s that have to be built at
/// run time regardless.
#[derive(Clone, Debug)]
pub struct UserClass {
    pub name: String,
    /// Each entry is a [`crate::value::Function`] handle, closed over the scope
    /// the class was declared in — which, for a subclass, is the scope holding
    /// `super`.
    pub methods: HashMap<String, ObjId>,
    pub parent: Option<ObjId>,
}

impl UserClass {
    /// The method `name`, searching this class and then its ancestors.
    ///
    /// Overriding falls out of the order rather than being implemented: the
    /// first table to hold the name wins, so a subclass shadows what it
    /// redefines and inherits what it does not.
    ///
    /// The loop terminates because the chain cannot contain a cycle. A parent
    /// has to be evaluated before the class that names it is bound, so a class
    /// can only ever extend one that already exists.
    pub fn method(&self, name: &str, heap: &Heap) -> Option<Value> {
        let mut class = self;
        loop {
            if let Some(id) = class.methods.get(name) {
                return Some(Value::Function(*id));
            }
            class = heap.class(class.parent?);
        }
    }

    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.methods.values().copied());
        worklist.extend(self.parent);
    }
}

/// An object of a user-defined class.
#[derive(Clone, Debug)]
pub struct Instance {
    pub class: ObjId,
    /// Fields are created by assignment rather than declared, so this starts
    /// empty even when the class has an `init`. A [`Dict`] rather than a
    /// `HashMap` so fields keep insertion order and reuse the tracing that
    /// already exists.
    pub fields: Dict,
}

impl Instance {
    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.push(self.class);
        self.fields.trace(worklist);
    }
}

/// The name a constructor has to be given for `Point(1, 2)` to reach it.
pub const INIT: &str = "init";

pub static NIL: BuiltinType = BuiltinType {
    name: "nil",
    methods: &[],
};
pub static BOOL: BuiltinType = BuiltinType {
    name: "bool",
    methods: &[],
};
pub static INT: BuiltinType = BuiltinType {
    name: "int",
    methods: &[],
};
pub static FLOAT: BuiltinType = BuiltinType {
    name: "float",
    methods: &[],
};
pub static STR: BuiltinType = BuiltinType {
    name: "string",
    methods: &[
        ("chars", &CHARS),
        ("ends_with", &ENDS_WITH),
        ("join", &JOIN),
        ("lower", &LOWER),
        ("replace", &REPLACE),
        ("split", &SPLIT),
        ("starts_with", &STARTS_WITH),
        ("trim", &TRIM),
        ("upper", &UPPER),
    ],
};
pub static LIST: BuiltinType = BuiltinType {
    name: "list",
    methods: &[("push", &PUSH)],
};
pub static DICT: BuiltinType = BuiltinType {
    name: "dict",
    methods: &[("keys", &KEYS), ("values", &VALUES), ("remove", &REMOVE)],
};
pub static FUNCTION: BuiltinType = BuiltinType {
    name: "function",
    methods: &[],
};
/// The type of a class itself, which is what `type(Point)` reports. An
/// *instance* of `Point` reports `Point`.
pub static CLASS: BuiltinType = BuiltinType {
    name: "class",
    methods: &[],
};

/// Every builtin type, for the tools that need to enumerate rather than look up.
///
/// [`Value::class`](crate::value::Value::class) maps a value to its type, which
/// is the direction the evaluator needs and the direction that cannot be walked
/// backwards — there is no way to ask Rust for every `static BuiltinType`. So
/// this list is maintained by hand, and the honest question is what a stale one
/// costs.
///
/// Forgetting a type here means its methods go unoffered by REPL completion.
/// Forgetting to update a hand-written list of method *names* meant the REPL
/// offering methods that do not exist, which is how this list came to be. One
/// failure is a gap and the other is a lie, and the lie was the one being told
/// every time a method was added or renamed. Types are added once and rarely;
/// methods are added constantly.
pub static BUILTINS: &[&BuiltinType] = &[
    &NIL, &BOOL, &INT, &FLOAT, &STR, &LIST, &DICT, &FUNCTION, &CLASS,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::Object;
    use crate::value::Native;

    static DUMMY: Native = Native {
        name: "dummy",
        arity: None,
        func: |_interp, _args, _span| Ok(Value::Nil),
    };

    #[test]
    fn builtins_covers_every_type_a_value_can_have() {
        // `Value::class` is the exhaustive match, so anything missing from
        // BUILTINS is a type some value can name and no tool can enumerate.
        // `Instance` is the one variant with no entry here, by definition: its
        // type is a user class, not a builtin.
        let mut heap = Heap::new();
        let values = [
            Value::Nil,
            Value::Bool(true),
            Value::Int(0),
            Value::Float(0.0),
            Value::from("s"),
            Value::List(heap.alloc(Object::List(vec![]))),
            Value::Dict(heap.alloc(Object::Dict(Dict::new()))),
            Value::Native(&DUMMY),
            Value::Class(heap.alloc(Object::Class(UserClass {
                name: "C".to_string(),
                methods: HashMap::new(),
                parent: None,
            }))),
        ];

        for value in values {
            let name = value.type_name(&heap);
            assert!(
                BUILTINS.iter().any(|builtin| builtin.name == name),
                "{name} is missing from BUILTINS"
            );
        }
    }

    #[test]
    fn every_listed_method_is_reachable_by_name() {
        for builtin in BUILTINS {
            for (name, _) in builtin.methods {
                assert!(
                    builtin.method(name).is_some(),
                    "{}.{name} is listed but does not look up",
                    builtin.name
                );
            }
        }
    }
}

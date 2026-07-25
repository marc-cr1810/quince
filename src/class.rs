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
    /// the class was declared in.
    pub methods: HashMap<String, ObjId>,
}

impl UserClass {
    /// `heap` is unused until a class can have a parent, which is where the
    /// lookup stops being a single map hit.
    pub fn method(&self, name: &str, _heap: &Heap) -> Option<Value> {
        self.methods.get(name).copied().map(Value::Function)
    }

    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.methods.values().copied());
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

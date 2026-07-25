//! The types a value can belong to.
//!
//! Every value names a type, and a type is where behaviour that cannot be
//! decided by matching on a `Value` will hang — methods first, then the
//! protocol slots a user-defined class overrides. Builtin types are static, so
//! naming one costs nothing and allocates nothing.
//!
//! User classes arrive with v0.4; see Dispatch in DESIGN.md for the shape.

use crate::interp::{
    CHARS, ENDS_WITH, JOIN, KEYS, LOWER, PUSH, REMOVE, REPLACE, SPLIT, STARTS_WITH, TRIM, UPPER,
    VALUES,
};
use crate::value::Native;

/// A type built into the language.
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

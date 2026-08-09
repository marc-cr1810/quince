//! Which native answers which name, on which type.
//!
//! Seed data, read once at startup by `Class::builtin` and never consulted again —
//! by then the entries are in a real class object beside anything a program added
//! with `extend`.
//!
//! This is the registry, and it is deliberately the only file that has to change
//! when a method is added, renamed, or moved between files. The natives themselves
//! live beside the type they act on: [`super::list`], [`super::string`],
//! [`super::dict`], [`super::convert`].

use crate::builtins::convert::{BOOL_INIT, DICT_INIT, FLOAT_INIT, INT_INIT, LIST_INIT, STR_INIT};
use crate::builtins::dict::{GET, KEYS, REMOVE, VALUES};
use crate::builtins::list::{FILTER, FIND, MAP, PUSH, REVERSE, SORT, SUM};
use crate::builtins::string::{
    CHARS, ENDS_WITH, JOIN, LOWER, REPEAT, REPLACE, SPLIT, STARTS_WITH, TRIM, UPPER,
};
use crate::runtime::class::BuiltinType;

/// `nil` is a keyword, so this type is never bound as a global and nothing can
/// name it to call it. The `init` would be unreachable rather than merely
/// useless, which is why there is none.
pub static NIL: BuiltinType = BuiltinType {
    name: "nil",
    methods: &[],
    init: None,
};
pub static BOOL: BuiltinType = BuiltinType {
    name: "bool",
    methods: &[],
    init: Some(&BOOL_INIT),
};
pub static INT: BuiltinType = BuiltinType {
    name: "int",
    methods: &[],
    init: Some(&INT_INIT),
};
pub static FLOAT: BuiltinType = BuiltinType {
    name: "float",
    methods: &[],
    init: Some(&FLOAT_INIT),
};
pub static STR: BuiltinType = BuiltinType {
    name: "string",
    methods: &[
        ("chars", &CHARS),
        ("ends_with", &ENDS_WITH),
        ("join", &JOIN),
        ("lower", &LOWER),
        ("repeat", &REPEAT),
        ("replace", &REPLACE),
        ("split", &SPLIT),
        ("starts_with", &STARTS_WITH),
        ("trim", &TRIM),
        ("upper", &UPPER),
    ],
    init: Some(&STR_INIT),
};
pub static LIST: BuiltinType = BuiltinType {
    name: "list",
    methods: &[
        ("filter", &FILTER),
        ("find", &FIND),
        ("map", &MAP),
        ("push", &PUSH),
        ("reverse", &REVERSE),
        ("sort", &SORT),
        ("sum", &SUM),
    ],
    init: Some(&LIST_INIT),
};
pub static DICT: BuiltinType = BuiltinType {
    name: "dict",
    methods: &[
        ("get", &GET),
        ("keys", &KEYS),
        ("remove", &REMOVE),
        ("values", &VALUES),
    ],
    init: Some(&DICT_INIT),
};
/// The arbitrary-arity product of v0.9 §3.5.
///
/// No methods and no `init`. A tuple is immutable and its arity is part of its
/// type, so every question anyone asks of one — how long it is, what is at a
/// position, whether it holds a value — is a language operation rather than a
/// method, and there is no value with an arity for a conversion to read.
pub static TUPLE: BuiltinType = BuiltinType {
    name: "tuple",
    methods: &[],
    init: None,
};
/// No `init`: there is no value a function could be made *from*. `fn` is how one
/// comes into being, and it is a declaration rather than a conversion.
pub static FUNCTION: BuiltinType = BuiltinType {
    name: "function",
    methods: &[],
    init: None,
};
/// The type of a class itself, which is what `type(Point)` reports. An
/// *instance* of `Point` reports `Point`.
///
/// `class` is a keyword, so like `nil` this is unreachable by name; `class` is
/// also the declaration that makes one, so there is nothing for an `init` to do.
pub static CLASS: BuiltinType = BuiltinType {
    name: "class",
    methods: &[],
    init: None,
};
/// The type of an imported module, which is what `type(math)` reports.
///
/// No methods, and it will stay that way: everything reachable through a module
/// is a name the module itself declared, found by looking in its scope rather
/// than on its type. A method here would be a name every module has and no
/// module wrote, which is the one thing that could collide with what an imported
/// file chose to call something.
pub static MODULE: BuiltinType = BuiltinType {
    name: "module",
    methods: &[],
    init: None,
};

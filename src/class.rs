//! The types a value can belong to.
//!
//! Every value names a type, and a type is where behaviour that cannot be
//! decided by matching on a `Value` will hang — methods first, then the
//! protocol slots a user-defined class overrides. Builtin types are static, so
//! naming one costs nothing and allocates nothing.
//!
//! Only `name` lives here so far. `methods` arrives with dispatch, and user
//! classes with v0.4; see Dispatch in DESIGN.md for the shape they land in.

/// A type built into the language.
pub struct BuiltinType {
    /// What the type is called in error messages and in `type(x)`.
    pub name: &'static str,
}

pub static NIL: BuiltinType = BuiltinType { name: "nil" };
pub static BOOL: BuiltinType = BuiltinType { name: "bool" };
pub static INT: BuiltinType = BuiltinType { name: "int" };
pub static FLOAT: BuiltinType = BuiltinType { name: "float" };
pub static STR: BuiltinType = BuiltinType { name: "string" };
pub static LIST: BuiltinType = BuiltinType { name: "list" };
pub static DICT: BuiltinType = BuiltinType { name: "dict" };
pub static FUNCTION: BuiltinType = BuiltinType { name: "function" };

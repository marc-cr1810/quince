//! The types a value can belong to.
//!
//! Every value names a type, and a type is where behaviour that cannot be
//! decided by matching on a `Value` will hang — methods first, then the
//! protocol slots a class overrides.
//!
//! There is one representation. A type built into the language and a class
//! written in Quince are the same kind of object, differing only in who filled
//! the method table, because anything a user class can do a builtin has to be
//! able to do too — see One class representation in DESIGN.md. The static
//! [`BuiltinType`] tables survive as *seed* data, read once at startup to build
//! the nine class objects, and never consulted again.

use std::collections::HashMap;

use crate::dict::Dict;
use crate::heap::{Heap, ObjId};
use crate::interp::{
    CHARS, ENDS_WITH, JOIN, KEYS, LOWER, PUSH, REMOVE, REPLACE, SPLIT, STARTS_WITH, TRIM, UPPER,
    VALUES,
};
use crate::value::{Native, Value};

/// A type built into the language.
///
/// An enum rather than a bare list of statics because this is what
/// [`Value::class`](crate::value::Value::class) matches on to find a value's
/// type: a variant is an index into the heap's table of class objects, so the
/// lookup is an array read rather than a name hashed on every method call and
/// every type error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Nil,
    Bool,
    Int,
    Float,
    Str,
    List,
    Dict,
    Function,
    Class,
}

/// Every builtin type, in the order their class objects are allocated.
///
/// Maintained by hand, and what a stale entry costs is worth being precise
/// about: a variant missing here has no class object, so the first value of that
/// type to have its type asked for panics on an out-of-bounds read. Loud and
/// immediate, unlike the silent REPL gap the old hand-written list risked, and
/// `builtins_covers_every_type_a_value_can_have` reaches it from every `Value`
/// variant there is.
pub static BUILTINS: &[Builtin] = &[
    Builtin::Nil,
    Builtin::Bool,
    Builtin::Int,
    Builtin::Float,
    Builtin::Str,
    Builtin::List,
    Builtin::Dict,
    Builtin::Function,
    Builtin::Class,
];

impl Builtin {
    /// The static table this type's class object is built from.
    ///
    /// Exhaustive, so a new variant fails to compile until it has a name and a
    /// method table — the two things it needs to be a type at all.
    pub fn seed(self) -> &'static BuiltinType {
        match self {
            Builtin::Nil => &NIL,
            Builtin::Bool => &BOOL,
            Builtin::Int => &INT,
            Builtin::Float => &FLOAT,
            Builtin::Str => &STR,
            Builtin::List => &LIST,
            Builtin::Dict => &DICT,
            Builtin::Function => &FUNCTION,
            Builtin::Class => &CLASS,
        }
    }

    /// What the type is called in error messages, in `type(x)`, and as a global.
    pub fn name(self) -> &'static str {
        self.seed().name
    }

    /// Where this type's class object sits in the heap's table.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Seed data for one builtin type: what it is called and what it can do.
///
/// Read once, by [`Class::builtin`]. Nothing looks a method up in here at run
/// time — by then the entries are in a real class, beside any a program added.
#[derive(Debug)]
pub struct BuiltinType {
    pub name: &'static str,
    /// A slice rather than a map: these are single digits long, and a `static`
    /// needs no construction at startup.
    pub methods: &'static [(&'static str, &'static Native)],
}

/// A type, whether the language defined it or a program did.
#[derive(Clone, Debug)]
pub struct Class {
    pub name: String,
    /// The methods callable on a value of this type.
    ///
    /// Values rather than function handles, which is what lets one table hold
    /// both kinds of method: a [`Value::Native`] is a `&'static` pointer holding
    /// no handle, so `string`'s `upper` and a Quince `fn` sit side by side and
    /// everything downstream — binding, calling, printing — has one case.
    ///
    /// Entries a Quince class declared are [`Value::Function`] handles, closed
    /// over the scope the class was declared in — which, for a subclass, is the
    /// scope holding `super`.
    pub methods: HashMap<String, Value>,
    pub parent: Option<ObjId>,
    /// The `op init` construction runs, or the one inherited from an ancestor.
    ///
    /// Resolved once when the class is built rather than looked up by name on
    /// every `Point(1, 2)`, and copied down from the parent so no chain is
    /// walked at construction time. That copy is safe for the same reason
    /// [`Class::method`]'s loop terminates: a parent is fully built before the
    /// class naming it exists.
    ///
    /// Held beside `methods` rather than instead of an entry in it, because the
    /// method is still reachable by name — `super.init(msg)` is how a subclass
    /// constructor delegates.
    pub init: Option<Value>,
    /// `Some` if the language defined this type, which is what makes `int(5)` an
    /// error instead of an instance of `int`.
    ///
    /// A builtin has no `op init` and no fields to put anything in, so
    /// construction has nothing to do and the honest answer is to refuse. It is
    /// also where a conversion — `int("5")` — would eventually hang.
    pub builtin: Option<Builtin>,
}

impl Class {
    /// The class object for a builtin type, built from its seed table.
    pub fn builtin(builtin: Builtin) -> Class {
        let seed = builtin.seed();
        Class {
            name: seed.name.to_string(),
            methods: seed
                .methods
                .iter()
                .map(|(name, native)| (name.to_string(), Value::Native(native)))
                .collect(),
            parent: None,
            init: None,
            builtin: Some(builtin),
        }
    }

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
            if let Some(method) = class.methods.get(name) {
                return Some(method.clone());
            }
            class = heap.class(class.parent?);
        }
    }

    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.methods.values().filter_map(Value::handle));
        worklist.extend(self.parent);
        // Always also an entry in `methods`, so no test can fail without this
        // line. It stays for the reason `trace` traces a bound method's callee:
        // the contract is every handle this object holds, not every handle
        // something else happens to reach.
        worklist.extend(self.init.as_ref().and_then(Value::handle));
    }
}

/// An object of a class written in Quince.
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
        // BUILTINS is a type some value can name and no class object exists for.
        // `Instance` is the one variant with no entry here, by definition: its
        // type is a class a program wrote, not a builtin.
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
            Value::Class(heap.alloc(Object::Class(Class {
                name: "C".to_string(),
                methods: HashMap::new(),
                parent: None,
                init: None,
                builtin: None,
            }))),
        ];

        for value in values {
            let name = value.type_name(&heap);
            assert!(
                BUILTINS.iter().any(|builtin| builtin.name() == name),
                "{name} is missing from BUILTINS"
            );
        }
    }

    #[test]
    fn every_seeded_method_is_reachable_on_the_class() {
        // Reaches through the real lookup rather than the seed table, so this
        // covers the copy at startup as well as the listing.
        let heap = Heap::new();
        for builtin in BUILTINS {
            let class = heap.class(heap.builtin_class(*builtin));
            for (name, _) in builtin.seed().methods {
                assert!(
                    class.method(name, &heap).is_some(),
                    "{}.{name} is seeded but does not look up",
                    builtin.name()
                );
            }
        }
    }

    #[test]
    fn every_builtin_sits_at_its_own_index() {
        // `Value::class` reads the heap's table by this index, so a variant
        // ordered differently from BUILTINS would silently report another type.
        for (index, builtin) in BUILTINS.iter().enumerate() {
            assert_eq!(builtin.index(), index, "{} is misplaced", builtin.name());
        }
    }

    #[test]
    fn no_two_builtins_share_a_name() {
        for builtin in BUILTINS {
            let same = BUILTINS
                .iter()
                .filter(|other| other.name() == builtin.name())
                .count();
            assert_eq!(same, 1, "{} is named twice", builtin.name());
        }
    }
}

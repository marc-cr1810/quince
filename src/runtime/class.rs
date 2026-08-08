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
use std::rc::Rc;

use crate::builtins::types::{BOOL, CLASS, DICT, FLOAT, FUNCTION, INT, LIST, MODULE, NIL, STR};
use crate::runtime::dict::Dict;
use crate::runtime::heap::{Heap, ObjId};
use crate::runtime::value::{Native, Value};
use crate::syntax::ast::{FieldDecl, OPS, Op, Openness, Visibility};

/// A type built into the language.
///
/// An enum rather than a bare list of statics because this is what
/// [`Value::class`](crate::runtime::value::Value::class) matches on to find a value's
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
    Module,
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
    Builtin::Module,
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
            Builtin::Module => &MODULE,
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

    /// The op a class defines to say how its values convert to this type.
    ///
    /// `int(x)` asks *x*'s class for [`Op::Int`], which is what lets a class
    /// choose what it becomes rather than being told by its payload. Exhaustive,
    /// so a new builtin cannot be added without answering — and `None` is a real
    /// answer twice over: there is no value a `function` or a `class` could be
    /// made from, so there is nothing for a class to override.
    pub fn conversion(self) -> Option<Op> {
        match self {
            Builtin::Bool => Some(Op::Bool),
            Builtin::Str => Some(Op::Str),
            Builtin::Int => Some(Op::Int),
            Builtin::Float => Some(Op::Float),
            Builtin::List => Some(Op::List),
            Builtin::Dict => Some(Op::Dict),
            // `nil` has no conversion to override: it is a keyword, not a global,
            // so `nil(x)` does not parse in the first place.
            // A module is produced by `import` and by nothing else. There is no
            // value it could be made from, so there is nothing for a class to
            // override — the same answer `function` and `class` give.
            Builtin::Nil | Builtin::Function | Builtin::Class | Builtin::Module => None,
        }
    }

    /// Whether this builtin type natively implements/supports a specific [`Op`].
    pub fn natively_supports_op(self, op: Op) -> bool {
        match self {
            Builtin::Int | Builtin::Float => matches!(
                op,
                Op::Add
                    | Op::Sub
                    | Op::Mul
                    | Op::Div
                    | Op::FloorDiv
                    | Op::Rem
                    | Op::Pow
                    | Op::Neg
                    | Op::Eq
                    | Op::Cmp
                    | Op::Lt
                    | Op::Gt
                    | Op::Bool
                    | Op::Int
                    | Op::Float
                    | Op::Str
                    | Op::Init
            ),
            Builtin::Str => matches!(
                op,
                Op::Add
                    | Op::Eq
                    | Op::Cmp
                    | Op::Lt
                    | Op::Gt
                    | Op::Len
                    | Op::Get
                    | Op::Contains
                    | Op::Iter
                    | Op::Str
                    | Op::Bool
                    | Op::Init
            ),
            Builtin::List => matches!(
                op,
                Op::Add
                    | Op::Eq
                    | Op::Len
                    | Op::Get
                    | Op::Set
                    | Op::Contains
                    | Op::Iter
                    | Op::List
                    | Op::Bool
                    | Op::Str
                    | Op::Init
            ),
            Builtin::Dict => matches!(
                op,
                Op::Eq
                    | Op::Len
                    | Op::Get
                    | Op::Set
                    | Op::Contains
                    | Op::Iter
                    | Op::Dict
                    | Op::Bool
                    | Op::Str
                    | Op::Init
            ),
            Builtin::Bool => matches!(
                op,
                Op::Bool | Op::Str | Op::Int | Op::Float | Op::Eq | Op::Init
            ),
            Builtin::Nil | Builtin::Function | Builtin::Class | Builtin::Module => {
                matches!(op, Op::Bool | Op::Str | Op::Eq)
            }
        }
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
    /// What calling the type does — `int("42")` reaches this.
    ///
    /// `None` for a type that cannot be built from a value, which is the honest
    /// answer for `function` and `class`: there is no argument from which one
    /// could be made.
    pub init: Option<&'static Native>,
}

/// A class's [`Op`] table, boxed.
///
/// Boxed because `Object` is an enum in an arena of `Option<Object>`, so its
/// largest variant sizes *every* heap slot — a list, an env, an instance. Inlining
/// the table took that from 104 bytes to 592 when there were twenty-one of them,
/// making every object in the heap five times its size to carry a table only
/// classes have. There are twenty-three now, and the argument only grows.
///
/// The array is boxed rather than the whole [`Class`], which would be the easier
/// change: `name` and `methods` are read on every method call, and putting them
/// behind the same indirection would charge dispatch for a table it never touches.
/// This way only a slot read pays the pointer chase.
pub type Slots = Box<[Option<Value>; Op::COUNT]>;

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
    /// The methods the language calls on the program's behalf, one entry per
    /// [`Op`], indexed by [`Op::index`].
    ///
    /// An array rather than a field each, so that adding an op is a variant and a
    /// line in [`OPS`] with nothing here to touch. That is the whole reason this
    /// shape was chosen — see DESIGN.md.
    ///
    /// Resolved once when the class is built rather than looked up by name on
    /// every `Point(1, 2)` or every `if x`. Hashing `"bool"` on the hottest path
    /// in the language is exactly the trap DESIGN.md's Slots are cached fields
    /// section names, and an array index is the way out of it.
    ///
    /// Inheritance is a copy-down from the parent at creation, so no chain is
    /// walked at use. Safe for the same reason [`Class::method`]'s loop
    /// terminates: a parent is fully built before the class naming it exists.
    ///
    /// A `None` slot is not a hole to report — it means the language behaves as
    /// it did before slots existed, which for most classes is all of them.
    ///
    /// Held beside `methods` rather than instead of entries in it, because an op
    /// is still reachable by name — `super.init(msg)` is how a subclass
    /// constructor delegates.
    pub slots: Slots,
    /// `Some` if the language defined this type.
    ///
    /// What it decides is what construction *yields*. A class a program wrote
    /// yields the instance it allocated; a builtin yields whatever its `init`
    /// returned, allocating nothing, because an `int` is not an object with
    /// fields. So `int("42")` is a conversion and `Point(1, 2)` is a
    /// constructor, through one code path.
    pub builtin: Option<Builtin>,
    /// Which of the two doors the declaration closed: `final`, `complete`,
    /// `sealed`, or neither.
    ///
    /// Kept as the word the program wrote rather than as two bools, so a refusal
    /// can quote the modifier back. Neither check lives here — each belongs where
    /// the attaching happens, which is two different files away from this one.
    ///
    /// Not inherited. `inherit_slots` copies a parent's slots down and this is
    /// not one: a class is open until its own declaration says otherwise.
    ///
    /// Always [`Openness::Open`] for a builtin. `extend int` is the feature the
    /// whole class representation was collapsed for, and `class MyStr extends
    /// string` works — see DESIGN.md.
    pub openness: Openness,
    /// The fields the body declared, in the order written.
    ///
    /// Kept as declarations rather than as values because an initializer is an
    /// expression and has to run once per instance: evaluating them when the
    /// class was built would give every instance the same list to push onto.
    /// [`Class::field_env`] is the scope they run in.
    ///
    /// Not inherited. A subclass's instance is initialized by walking the chain
    /// at construction, so a parent's fields are found where they were declared
    /// rather than copied down — the opposite choice from `slots`, and for the
    /// opposite reason: slots are read on every `if x` and these are read once.
    pub fields: Vec<Rc<FieldDecl>>,
    /// The scope a field's initializer is evaluated in — the one the class was
    /// declared in, which is what a method closes over too.
    ///
    /// `None` for a builtin and for any class with no fields to initialize.
    pub field_env: Option<ObjId>,
    /// How far the class's own name reaches: whether an importing module sees
    /// it. Nothing to do with the visibility of its members.
    pub visibility: Visibility,
}

impl Class {
    /// The class object for a builtin type, built from its seed table.
    pub fn builtin(builtin: Builtin) -> Class {
        let seed = builtin.seed();
        let mut slots = Class::empty_slots();
        // Not also an entry in `methods`, unlike a user class's `op init`. A
        // conversion takes no receiver, so as a method it would be wrong:
        // `(5).init(7)` has no meaning to give.
        slots[Op::Init.index()] = seed.init.map(Value::Native);
        Class {
            name: seed.name.to_string(),
            methods: seed
                .methods
                .iter()
                .map(|(name, native)| (name.to_string(), Value::Native(native)))
                .collect(),
            parent: None,
            slots,
            builtin: Some(builtin),
            openness: Openness::Open,
            fields: Vec::new(),
            field_env: None,
            visibility: Visibility::Public,
        }
    }

    /// The field `name`, searching this class and then its ancestors.
    ///
    /// Same order as [`Class::method`] and for the same reason: the first
    /// declaration found wins, so a subclass shadows the field it redeclares.
    pub fn field(&self, name: &str, heap: &Heap) -> Option<Rc<FieldDecl>> {
        let mut class = self;
        loop {
            if let Some(field) = class.fields.iter().find(|field| field.name == name) {
                return Some(Rc::clone(field));
            }
            class = heap.class(class.parent?);
        }
    }

    /// Whether `name` names a method or field this class declared, or inherited.
    ///
    /// What tells a member reached through the dot apart from one an `op init`
    /// invented: only a *declared* member carries a visibility, so only a
    /// declared member can be refused.
    pub fn declares(&self, name: &str, heap: &Heap) -> bool {
        self.method(name, heap).is_some() || self.field(name, heap).is_some()
    }

    /// A slot table with nothing in it, which is every class before its own
    /// declaration fills one and every builtin but for `init`.
    pub fn empty_slots() -> Slots {
        Box::new([const { None }; Op::COUNT])
    }

    /// The method this class uses for `op`, if it has one.
    ///
    /// An array read, and no chain walked: what an ancestor declared was copied
    /// down when this class was built.
    pub fn slot(&self, op: Op) -> Option<&Value> {
        self.slots[op.index()].as_ref()
    }

    /// Copies every slot this class did not declare down from `parent`.
    ///
    /// One loop rather than a case per op, which is what makes adding one free.
    /// `init` goes through here like the rest — if the op that existed first did
    /// not fit the array, the array would be the wrong shape.
    ///
    /// Takes the parent's table rather than the parent, so the caller can clone
    /// twenty-one slots instead of a whole class with its method map.
    pub fn inherit_slots(&mut self, parent: &Slots) {
        for op in OPS {
            if self.slots[op.index()].is_none() {
                self.slots[op.index()] = parent[op.index()].clone();
            }
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

    /// Returns a list of all method names defined on this class and its ancestors.
    pub fn method_names(&self, heap: &Heap) -> Vec<String> {
        let mut names = Vec::new();
        let mut class = self;
        loop {
            for key in class.methods.keys() {
                if !names.contains(key) {
                    names.push(key.clone());
                }
            }
            if let Some(parent_id) = class.parent {
                class = heap.class(parent_id);
            } else {
                break;
            }
        }
        names
    }

    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.methods.values().filter_map(Value::handle));
        worklist.extend(self.parent);
        // A slot a class declared is always also an entry in `methods`, so no
        // test can fail without this line. It stays for the reason `trace`
        // traces a bound method's callee: the contract is every handle this
        // object holds, not every handle something else happens to reach.
        //
        // A slot *inherited* from a parent is a different matter — it is a
        // handle to a function in the parent's table, and the parent is traced
        // just above, so it survives either way.
        worklist.extend(self.slots.iter().flatten().filter_map(Value::handle));
        // The scope a field initializer runs in. Usually the same one the
        // methods closed over, and so already reached through them — but a class
        // with fields and no methods holds it alone, and that is the case a
        // collection between declaring the class and constructing one would
        // otherwise sweep out from under the next `Point()`.
        worklist.extend(self.field_env);
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
    /// The value a builtin ancestor's `init` produced, for a class that extends
    /// one — the string a `class Email extends string` *is*.
    ///
    /// `None` for a class descending from no builtin, which is every class that
    /// does not say so, and also for the window between allocating an instance
    /// and its `super.init` running.
    ///
    /// Not a field, though a field is where a wrapper class would keep it. A
    /// field is assignable and shadowable, so `e.value = 5` could leave an
    /// `Email` that is not a string, and `string`'s methods would then be
    /// looking at an int. This is reachable only through `super.init`, once.
    pub payload: Option<Value>,
}

impl Instance {
    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.push(self.class);
        self.fields.trace(worklist);
        worklist.extend(self.payload.as_ref().and_then(Value::handle));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::env::Globals;
    use crate::runtime::heap::Object;
    use crate::runtime::value::Native;

    static DUMMY: Native = Native {
        name: "dummy",
        arity: None,
        params: &[],
        returns: None,
        doc: "A stand-in for a test, which needs a native and not what it does.",
        func: |_interp, _args, _span| Ok(Value::Nil),
    };

    /// A type that can be made from a value is exactly a type a class can say it
    /// converts to.
    ///
    /// Both halves are worth pinning. Comparing the *names* rather than the pair
    /// catches a mismapping without listing the pairs again: `Float` answering
    /// `Op::Int` would make `float(x)` run a class's `op int`, silently, and no
    /// other test would notice. And a builtin with a conversion but no `init` —
    /// or the reverse — is a type a class could override its way into but the
    /// language cannot construct.
    #[test]
    fn a_builtin_converts_exactly_when_a_class_can_answer_for_it() {
        for builtin in BUILTINS {
            match builtin.conversion() {
                Some(op) => {
                    assert_eq!(
                        op.name(),
                        builtin.name(),
                        "`{}` converts through `op {}`",
                        builtin.name(),
                        op.name()
                    );
                    assert!(
                        builtin.seed().init.is_some(),
                        "`{}` can be overridden but not constructed",
                        builtin.name()
                    );
                }
                None => assert!(
                    builtin.seed().init.is_none(),
                    "`{}` can be constructed but not overridden",
                    builtin.name()
                ),
            }
        }
    }

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
                fields: Vec::new(),
                field_env: None,
                visibility: Visibility::Public,
                name: "C".to_string(),
                methods: HashMap::new(),
                parent: None,
                slots: Class::empty_slots(),
                builtin: None,
                openness: Openness::Open,
            }))),
            Value::Module(heap.alloc(Object::Globals(Globals::module("m", None)))),
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
    fn a_seeded_init_reaches_the_class_and_is_named_for_its_type() {
        // The name matters because it is what an arity error quotes: `int(1, 2)`
        // should be told about `int`, not about `init`.
        let heap = Heap::new();
        for builtin in BUILTINS {
            let class = heap.class(heap.builtin_class(*builtin));
            match (builtin.seed().init, class.slot(Op::Init)) {
                (Some(native), Some(Value::Native(installed))) => {
                    assert!(
                        std::ptr::eq(native, *installed),
                        "{}'s init is not the one it was seeded with",
                        builtin.name()
                    );
                    assert_eq!(
                        native.name,
                        builtin.name(),
                        "{}'s init is named for something else",
                        builtin.name()
                    );
                }
                (None, None) => {}
                (seeded, installed) => panic!(
                    "{} seeded {seeded:?} but carries {installed:?}",
                    builtin.name()
                ),
            }
        }
    }

    #[test]
    fn a_builtin_init_is_not_also_a_method() {
        // Deliberately unlike a user class, whose `op init` is reachable as
        // `super.init`. A conversion takes no receiver, so `(5).init(7)` has no
        // meaning to give and had better not find one.
        let heap = Heap::new();
        for builtin in BUILTINS {
            let class = heap.class(heap.builtin_class(*builtin));
            assert!(
                class.method("init", &heap).is_none(),
                "{}.init resolves as a method",
                builtin.name()
            );
        }
    }

    #[test]
    fn adding_an_op_cannot_grow_a_heap_object() {
        // `Object`'s largest variant sizes every slot in the arena, so an inline
        // slot table charges every list and every env for a table only a class
        // has — 104 bytes to 592, when this was first written inline. Boxing is
        // what fixes it, and this is that fix stated as a property: the table is
        // one pointer wide however many ops there are.
        // One pointer wide is the whole invariant: if the table is behind an
        // indirection then no number of ops can move `Object` at all.
        assert_eq!(size_of::<Slots>(), size_of::<usize>());
        // A loose ceiling rather than the exact 104, so this fails on the mistake
        // it is about — an inline table, which was 592 — and not on an unrelated
        // field being added to some other variant.
        assert!(
            size_of::<Object>() < 256,
            "Object is {} bytes; something is inline that should not be",
            size_of::<Object>()
        );
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

    /// A native may sit in the `init` slot and in no other.
    ///
    /// `call_op` and `op_returned` both read an op's body span straight off a
    /// `Value::Function`, and both call the alternative unreachable rather than
    /// answering with a span they made up. This is the half of that reasoning a
    /// test can hold: the day a seed table learns to name an `op` of its own,
    /// this fails here rather than as a panic in whichever report went looking
    /// for a body that was never there.
    #[test]
    fn a_native_fills_no_slot_but_init() {
        for builtin in BUILTINS {
            let class = Class::builtin(*builtin);
            for op in OPS.iter().copied() {
                let Some(filled) = class.slot(op) else {
                    continue;
                };
                assert!(
                    op == Op::Init,
                    "{} seeds `op {}`, which no report can find a body for",
                    builtin.name(),
                    op.name()
                );
                assert!(
                    matches!(filled, Value::Native(_)),
                    "{}'s `op init` should be a native",
                    builtin.name()
                );
            }
        }
    }
}

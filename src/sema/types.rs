//! What a type is, in the answer the inference pass gives.
//!
//! [`Type`] has three states and not two: `Unknown` is a real answer, and most
//! of a dynamically typed program is `Unknown`. The rest of this file is the
//! lookups that turn a builtin table into a type — what a native returns, what a
//! conversion produces, what an operator on two builtins answers with.
//!
//! **v0.7 tranche 2 landed here.** [`Type`] carries type arguments, so
//! `list[int]` can be expressed at all — the item everything in v0.7 through
//! v0.10 waited on. Nothing *produces* one yet: there is no syntax to write an
//! annotation until tranche 3 and no typed container until tranche 4, so every
//! type the pass currently infers still has an empty argument list. What this
//! file has is the shape, the spelling ([`Type`]'s `Display`), and invariance
//! falling out of structural equality rather than being implemented.
//!
//! Still to come here: the matching table in v0.7 §4.1 — which annotation holds
//! which values, `float` widening an `int` — and after it v0.7's alias
//! substitution and v0.9's bounds.
//!
//! The reified header §3.9 wants on every allocation carrying arguments is
//! *not* here, and deliberately: nothing can carry one until there are typed
//! containers to put them on. It belongs with tranche 4, which is also where
//! its O(1) comparison requirement first has a caller.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::builtins::stdlib;
use crate::runtime::class::BUILTINS;
use crate::runtime::value::{Native, Value};
use crate::sema::infer::ClassInfo;
use crate::syntax::ast::BinaryOp;


/// A type's name, shared rather than copied.
///
/// Shared and not `String`, because a type is cloned far more often than it is
/// built — every `join`, every lookup that hands one back — and a name is
/// immutable once written. Not a numeric intern handle, which was considered
/// and is the wrong trade *here*: an interner is only worth its global state
/// when equality is on a hot path, and this type is the compile-time pass's.
/// The run-time `is` check §3.9 promises in O(1) reads a reified descriptor off
/// the allocation rather than one of these, so that is where the question
/// actually lives — tranche 4, with the containers that raise it.
///
/// `Arc` and not `Rc` for one reason: [`symbols::globals`] caches the builtin
/// symbols in a `OnceLock`, and a static holding a `Type` has to be `Sync`. The
/// atomic is paid once per clone of a compile-time value and never on a path a
/// program runs.
///
/// An alias so that swapping the representation later touches this line and the
/// two constructors below, and no call site.
///
/// [`symbols::globals`]: crate::sema::symbols::globals
pub type Name = Arc<str>;

/// An instance of a class, with whatever type arguments were written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassType {
    pub name: Name,
    /// `list[int]`'s `int`, in the order written. Empty for a type taking none,
    /// which is every type until tranche 4 gives the containers parameters.
    ///
    /// A `Vec` rather than a fixed arity because the arities differ — `list[T]`
    /// takes one, `dict[K, V]` two, and v0.9's parameter packs take any number.
    pub args: Vec<Type>,
}

/// What the pass worked out about a value.
///
/// Three states, not two. A module is not a class — `math` has members and no
/// methods, and answering `Class("module")` for it would be true and useless,
/// since what a caller wants to know is *which* module so it can list what is
/// in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Type {
    /// Nothing was decidable, or two decidable answers disagreed.
    #[default]
    Unknown,
    /// An instance of the named class — a builtin type such as `int`, or one
    /// the program declared — with its type arguments.
    Class(ClassType),
    /// A stdlib module, by name. A module loaded from a file is `Unknown`:
    /// nothing here reads the filesystem, which is what makes cross-file
    /// inference a later piece of work rather than a flag on this one.
    Module(Name),
}

impl Type {
    /// An instance of the named class, taking no type arguments.
    ///
    /// The overwhelmingly common case and so the short name: every type the
    /// language had before v0.7 is one of these, and `list[int]` is the
    /// exception that says more.
    pub fn class(name: impl AsRef<str>) -> Type {
        Type::Class(ClassType {
            name: Arc::from(name.as_ref()),
            args: Vec::new(),
        })
    }

    /// An instance of the named class, with type arguments — `list[int]`.
    pub fn generic(name: impl AsRef<str>, args: Vec<Type>) -> Type {
        Type::Class(ClassType {
            name: Arc::from(name.as_ref()),
            args,
        })
    }

    /// The class this is an instance of, or `None` for anything else.
    ///
    /// The *name* alone, so a caller asking "is this a list" is not made to
    /// care whether it is a `list[int]`. Everything written before type
    /// arguments existed asks this question and still means it.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            Type::Class(class) => Some(&class.name),
            _ => None,
        }
    }

    /// The type arguments this was written with, which is empty for most types.
    pub fn args(&self) -> &[Type] {
        match self {
            Type::Class(class) => &class.args,
            _ => &[],
        }
    }

    /// The module this names, or `None` for anything else.
    pub fn module_name(&self) -> Option<&str> {
        match self {
            Type::Module(name) => Some(name),
            _ => None,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Type::Unknown)
    }

    /// The one answer both of these give, or `Unknown` when they give two.
    ///
    /// Deliberately not a lattice with a `nil`-shaped bottom or a union in the
    /// middle. A variable holding an int on one path and a string on the other
    /// has no type this pass could report that would be worth reporting, and
    /// the v0.7 annotations are where a program gets to *say* it meant both.
    pub(crate) fn join(self, other: Type) -> Type {
        if self == other { self } else { Type::Unknown }
    }

    /// The module called `name`.
    pub fn module(name: impl AsRef<str>) -> Type {
        Type::Module(Arc::from(name.as_ref()))
    }
}

/// How a type is written, which is how a report and a hover both name one.
///
/// One implementation rather than a `format!` at each site, because `list[int]`
/// has to read the same in an inlay hint, a signature, and the error that
/// refuses it — and because the nesting is recursive and doing that by hand
/// once is doing it wrong twice.
impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Written as the annotation a program would have to write to mean
            // it, and there is none — `Unknown` is what the pass concluded, not
            // something anybody typed. The wildcard is the nearest true thing.
            Type::Unknown => write!(f, "_"),
            Type::Module(name) => write!(f, "module {name}"),
            Type::Class(class) if class.args.is_empty() => write!(f, "{}", class.name),
            Type::Class(class) => {
                write!(f, "{}[", class.name)?;
                for (index, arg) in class.args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, "]")
            }
        }
    }
}

/// What a stdlib module's member holds.
///
/// Read off `stdlib::MODULES` rather than from a list kept here, for the reason
/// the completion tables were made to: a second copy of the library's contents
/// is a copy that will be wrong. A constant is *built* to find out what it is,
/// which is exact rather than a guess — `math.pi` is a float because building
/// it produces one.
pub(crate) fn module_member(module: &str, name: &str) -> Type {
    let Some(module) = stdlib::module_named(module) else {
        return Type::Unknown;
    };
    match module.members.iter().find(|(member, _)| *member == name) {
        Some((_, stdlib::Member::Fn(_))) => Type::class("function"),
        Some((_, stdlib::Member::Const(build))) => of_value(&build()),
        None => Type::Unknown,
    }
}

/// The native a builtin type declares as its method called `name`.
///
/// Only the builtins: a class the program wrote has its methods in the AST, and
/// those are answered by walking it. Reached through `BUILTINS` and the seed
/// tables the classes are built from, so a method added to a type is found here
/// without this file knowing it exists.
pub(crate) fn builtin_method(class: &str, name: &str) -> Option<&'static Native> {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name() == class)?
        .seed()
        .methods
        .iter()
        .find_map(|(method, native)| (*method == name).then_some(*native))
}

/// The native answering for `class`'s method called `name`, searching the
/// classes it descends from.
///
/// A class extending `list` inherits the list's methods, so `Stack().push` has
/// to find `push` on the builtin two links up. Free rather than a method,
/// because both the walk and the finished table need it and they hold their
/// classes in the same shape.
pub(crate) fn builtin_ancestor(
    classes: &HashMap<String, ClassInfo>,
    class: &str,
    name: &str,
) -> Option<&'static Native> {
    let mut current = class.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        if let Some(native) = builtin_method(&current, name) {
            return Some(native);
        }
        current = classes.get(&current)?.parent.clone()?;
    }
}

/// What a native hands back, which is `Unknown` for one that does not say.
///
/// The `None` case is not a shortfall to be filled in later. `dict.get` answers
/// with whatever was stored and `io.line` answers with a string until input runs
/// out — declaring a type for either would be a guess wearing the authority of
/// a table.
pub(crate) fn returned_by(native: &Native) -> Type {
    match native.returns {
        Some(builtin) => Type::class(builtin.name()),
        None => Type::Unknown,
    }
}

/// The class of a value that names its own class.
///
/// Only the variants that carry no handle, because naming the rest needs a heap
/// and there is none here. Every stdlib constant is one of these, and one that
/// is not answers `Unknown` rather than being reached for.
pub(crate) fn of_value(value: &Value) -> Type {
    match value {
        Value::Nil => Type::class("nil"),
        Value::Bool(_) => Type::class("bool"),
        Value::Int(_) => Type::class("int"),
        Value::Float(_) => Type::class("float"),
        Value::Str(_) => Type::class("string"),
        _ => Type::Unknown,
    }
}

/// Whether `name` is a builtin type that can be called to make one.
///
/// `int(x)`, `string(x)`, `bool(x)` and the three collections. Asked of
/// `Builtin::conversion` rather than of a list written here, so the day a
/// builtin type gains or loses a constructor this follows it. `nil` and `class`
/// answer no, and would anyway: both are keywords, so neither can be called.
pub(crate) fn builtin_constructor(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name() == name && builtin.conversion().is_some())
        .map(|builtin| builtin.name())
}

/// What an operator produces, when the language rather than a class decides.
///
/// A class declaring `op add` may answer with anything, so only operands that
/// are builtins are decidable — with the comparisons excepted, which the
/// evaluator forces to a bool whatever an `op cmp` returned.
pub(crate) fn binary(op: BinaryOp, lhs: &Type, rhs: &Type) -> Type {
    use BinaryOp::*;

    // Every comparison ends in a bool. `==` is defined for every pair of values,
    // `<` and its family read whatever `op cmp` answered for its sign, and `in`
    // does the same to `op contains`.
    if matches!(op, Eq | Ne | Lt | Le | Gt | Ge | In) {
        return Type::class("bool");
    }

    let (Some(lhs), Some(rhs)) = (lhs.class_name(), rhs.class_name()) else {
        return Type::Unknown;
    };

    match (op, lhs, rhs) {
        // True division leaves the integers behind: `1 / 2` is `0.5`.
        (Div, "int" | "float", "int" | "float") => Type::class("float"),
        (_, "int", "int") => Type::class("int"),
        (_, "int" | "float", "int" | "float") => Type::class("float"),
        (Add, "string", "string") => Type::class("string"),
        (Add, "list", "list") => Type::class("list"),
        _ => Type::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_without_arguments_is_written_as_its_name() {
        assert_eq!(Type::class("int").to_string(), "int");
        assert_eq!(Type::module("math").to_string(), "module math");
        // `Unknown` is what the pass concluded, not something anybody wrote, so
        // the wildcard is the nearest true spelling of it.
        assert_eq!(Type::Unknown.to_string(), "_");
    }

    #[test]
    fn arguments_are_written_inside_brackets_and_nest() {
        let ints = Type::generic("list", vec![Type::class("int")]);
        assert_eq!(ints.to_string(), "list[int]");

        let scores = Type::generic("dict", vec![Type::class("string"), Type::class("int")]);
        assert_eq!(scores.to_string(), "dict[string, int]");

        // Recursive, which is the whole reason this is one implementation and
        // not a `format!` per site.
        let nested = Type::generic("list", vec![scores]);
        assert_eq!(nested.to_string(), "list[dict[string, int]]");
    }

    #[test]
    fn the_name_is_answered_without_the_arguments() {
        // Everything written before type arguments existed asks "is this a
        // list", and still means it. A `list[int]` has to keep saying `list`
        // or every one of those callers silently changes meaning.
        let ints = Type::generic("list", vec![Type::class("int")]);
        assert_eq!(ints.class_name(), Some("list"));
        assert_eq!(Type::class("list").class_name(), Some("list"));
        assert_eq!(Type::module("math").class_name(), None);
        assert_eq!(Type::Unknown.class_name(), None);

        assert_eq!(ints.args(), &[Type::class("int")]);
        assert!(Type::class("list").args().is_empty());
        assert!(Type::Unknown.args().is_empty());
    }

    #[test]
    fn arguments_are_part_of_what_makes_two_types_the_same() {
        let ints = Type::generic("list", vec![Type::class("int")]);
        let strings = Type::generic("list", vec![Type::class("string")]);
        let bare = Type::class("list");

        // §4.1's invariance, which falls out of structural equality rather than
        // being implemented: a `list[int]` is not a `list[string]`, and neither
        // is the unparameterised `list`.
        assert_ne!(ints, strings);
        assert_ne!(ints, bare);
        assert_eq!(ints, Type::generic("list", vec![Type::class("int")]));

        // And so `join` separates them, which is what stops a variable holding
        // both from being reported as either.
        assert_eq!(ints.clone().join(strings), Type::Unknown);
        assert_eq!(ints.clone().join(bare), Type::Unknown);
        assert_eq!(ints.clone().join(ints.clone()), ints);
    }
}

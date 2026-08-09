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
use crate::runtime::heap::Heap;
use crate::runtime::value::{Native, Value};
use crate::sema::infer::ClassInfo;
use crate::syntax::ast::{BinaryOp, TypeExpr, TypeName};
use crate::syntax::token::Span;


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
    /// Whether `nil` holds as this — the `?` in `int?`.
    ///
    /// Part of the type and not a wrapper around it, because §4.1 treats `int`
    /// and `int?` as two annotations rather than one modified: they hold
    /// different values, they are written in one breath, and a wrapper would
    /// make every reader of a type unwrap before asking anything.
    pub nullable: bool,
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
            nullable: false,
        })
    }

    /// An instance of the named class, with type arguments — `list[int]`.
    pub fn generic(name: impl AsRef<str>, args: Vec<Type>) -> Type {
        Type::Class(ClassType {
            name: Arc::from(name.as_ref()),
            args,
            nullable: false,
        })
    }

    /// The same type, admitting `nil` — `int` becomes `int?`.
    pub fn nullable(self) -> Type {
        match self {
            Type::Class(class) => Type::Class(ClassType {
                nullable: true,
                ..class
            }),
            other => other,
        }
    }

    /// Whether `nil` holds as this.
    ///
    /// `Unknown` admits it: the pass has not been told what the name holds, and
    /// refusing `nil` would be a claim it has no basis for. §3.2's table.
    pub fn admits_nil(&self) -> bool {
        match self {
            Type::Class(class) => class.nullable,
            Type::Unknown => true,
            Type::Module(_) => false,
        }
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
            Type::Class(class) => {
                write!(f, "{}", class.name)?;
                if !class.args.is_empty() {
                    write!(f, "[")?;
                    for (index, arg) in class.args.iter().enumerate() {
                        if index > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{arg}")?;
                    }
                    write!(f, "]")?;
                }
                if class.nullable {
                    write!(f, "?")?;
                }
                Ok(())
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

/// The type an annotation states.
///
/// The bridge from what the program wrote to what the pass works in — the whole
/// reason §2 calls annotations "the mechanism by which a program turns an
/// `Unknown` into a stated fact". Without this the pass would keep inferring
/// from the initializer and quietly disagree with the declaration beside it,
/// which is what `what_the_pass_claims_is_what_the_programs_produce` catches.
///
/// `any` becomes `Unknown`, which is not quite the same thing and is the honest
/// answer available today: §3.2 distinguishes what the pass *concluded* from
/// what the program *said*, and [`Type`] has no way to carry the difference
/// until it gains nullability. Both admit every value, so nothing downstream is
/// misled — only an inlay hint would want to tell them apart, and that is
/// tranche 7.
pub fn stated(ty: &TypeExpr) -> Type {
    match &ty.name {
        TypeName::Any => Type::Unknown,
        // A value, not a type. The inference pass has no representation for one
        // and does not need it yet: `Buffer[int, 16]` and `Buffer[int, 32]`
        // differ where it matters — in the header `is` reads — and this pass
        // only ever reports a type back to an editor. §3.3, and it is the one
        // place a const argument is answered less precisely than it is checked.
        TypeName::Const(_) => Type::Unknown,
        // Several types where one goes, and this pass has one answer to give.
        // A pack is only ever `Unknown` *unsubstituted*: once a class is built
        // its arguments are ordinary types, and it is those the editor sees.
        TypeName::Pack(_) => Type::Unknown,
        TypeName::Named(name) => {
            let stated = Type::generic(name, ty.args.iter().map(stated).collect());
            match ty.nullable {
                true => stated.nullable(),
                false => stated,
            }
        }
    }
}

/// Whether `value` holds as the annotation `ty`, per v0.7 §4.1.
///
/// The one statement of the matching rules. Every check the milestone
/// enforces — a binding, an argument, a return, and in tranche 4 a container's
/// elements — asks this, so the table is written once and a program cannot find
/// a boundary that answers differently from the others.
///
/// The rules that are not a plain "same class", in the order they are decided:
///
/// - **`nil` needs a `?`.** Non-nullable by default is the whole point, so this
///   is checked before anything about the class.
/// - **`any` admits everything but `nil`**, and `any?` everything.
/// - **`float` accepts an int and widens it.** `1 + 2.0` is a float everywhere
///   in the evaluator, so `let x: float = 0` being refused would be a rule that
///   contradicts the next line. Narrowing is not symmetric: `let n: int = 3.7`
///   stays an error, because it would have to choose a rounding and `int(x)` is
///   how a program says which.
/// - **A subclass holds as its parent.** `UserClass` admits an instance of it or
///   of anything descending from it.
/// - **Arguments go through [`arguments_admit`]**, shared with `is` so the two
///   cannot drift. Every elided argument reads as the `any?` §3.10 says it is,
///   which makes `list` and `list[any?]` one type and `dict[K]` and
///   `dict[K, any?]` one type.
/// - **A container nothing described is walked**, because at a boundary it is
///   raw material: `let xs: list[int] = [1, 2]` is the annotation *deciding* the
///   type rather than agreeing with one, and the walk is what stamps the header.
///   `is` answers differently there and says so — see [`Interp::has_type`].
///
/// [`Interp::has_type`]: crate::interp::Interp
pub fn holds(ty: &TypeExpr, value: &Value, heap: &Heap) -> bool {
    if matches!(value, Value::Nil) {
        return ty.admits_nil();
    }
    let name = match &ty.name {
        // Anything that is not `nil`, and `nil` was answered above.
        TypeName::Any => return true,
        // A const argument names no class, so no value is an instance of it.
        // Unreachable in practice — the parser only produces one inside an
        // argument list, and this is asked of the head — but "no value holds as
        // `16`" is the honest answer if it ever is.
        TypeName::Const(_) => return false,
        // An unsubstituted pack — a `Ts...` outside any binding, which is what
        // a bare `CustomTuple()` leaves. Unconstrained, like the `any?` an
        // unbound type parameter stands for: §3.1's defaulting, one arity up.
        TypeName::Pack(_) => return true,
        TypeName::Named(name) => name.as_str(),
    };

    let actual = value.type_name(heap);
    // An int is a float when a float was asked for, and never the other way.
    if name == "float" && actual == "int" {
        return true;
    }
    if actual != name && !descends_from(heap, value, name) {
        return false;
    }

    // A tuple is answered from its elements and never from its header, which is
    // the one place §3.5's "arity is part of the type" parts company with every
    // other container. A `list[int]` can be empty and still be a list of ints,
    // so only the header knows what it was built to hold; a tuple's elements
    // *are* its type, all of them, always. There is nothing a header could add
    // and nothing it could correct.
    if let ("tuple", Value::Tuple(id)) = (name, value.base(heap)) {
        return tuple_holds(ty, heap.tuple(*id), heap);
    }

    // The reified header first, where the allocation carries one. A container
    // that crossed an annotated boundary was *built to hold* those types, and
    // that is what it is: `let xs: list[int] = []` is a list of ints while it is
    // empty, and asking its elements would answer that it is equally a
    // `list[string]`. The header is the only thing that knows better.
    if let Some(id) = value.base(heap).handle()
        && let Some(held) = heap.descriptor(id)
    {
        return arguments_admit(ty, &held.args);
    }

    // The elements, for a container nothing has described. That is every
    // container built from a literal and passed straight on, and it is what
    // stamps the header in the first place — see `Heap::describe`.
    match (name, value.base(heap)) {
        ("list", Value::List(id)) if !ty.args.is_empty() => heap
            .list(*id)
            .iter()
            .all(|item| holds(&ty.args[0], item, heap)),
        ("dict", Value::Dict(id)) => {
            let dict = heap.dict(*id);
            let keys_hold = ty.args.first().is_none_or(|key| {
                dict.keys().all(|k| holds(key, &k, heap))
            });
            // The `dict[K]` shorthand leaves values entirely unconstrained —
            // `_?` and not `_`. A shorthand meaning "I only care about the keys"
            // that then refused a `nil` value would be a trap, so the elided
            // parameter is the top type and not the non-nil one. §3.10.
            keys_hold
                && ty.args.get(1).is_none_or(|v| {
                    dict.values().all(|held| holds(v, held, heap))
                })
        }
        _ => true,
    }
}

/// Whether `items` hold as the tuple annotation `ty` — v0.9 §3.5.
///
/// Two rules, and the first is the one that makes a tuple a product type rather
/// than a short list: **the arity has to match exactly**, because
/// `tuple[int, int]` and `tuple[int, int, int]` are unrelated types and neither
/// admits the other. The second is positional — argument `i` against element
/// `i` — with no padding and no elision, for the same reason. A `dict[K]` may
/// leave the values unconstrained because a dict has two arguments whatever it
/// holds; a tuple with one argument written is a tuple of one thing.
///
/// A bare `tuple` admits any tuple, and `tuple[]` admits only the empty one.
/// [`TypeExpr::applied`] is what tells the two apart.
fn tuple_holds(ty: &TypeExpr, items: &[Value], heap: &Heap) -> bool {
    if !ty.applied {
        return true;
    }
    // A `tuple[Ts...]` nothing has expanded: the arity is not merely unmatched,
    // it is unknown, so there is no arity to compare against. The same answer
    // an unsubstituted pack gets everywhere — unconstrained. §3.4.
    if ty.args.iter().any(|arg| matches!(arg.name, TypeName::Pack(_))) {
        return true;
    }
    ty.args.len() == items.len()
        && ty
            .args
            .iter()
            .zip(items)
            .all(|(arg, item)| holds(arg, item, heap))
}

/// The type an elided argument stands for: `any?`, the top type.
///
/// §3.10's `dict[K]` is shorthand for `dict[K, _?]`, and v0.9 §3.1 says the same
/// of an unbounded `[T]` — an argument nobody wrote constrains nothing, and it
/// has to admit `nil` or the shorthand becomes a trap. One function rather than
/// seven sites deciding it, which is what let `is` and an annotation disagree
/// about what `list` and `list[any?]` were.
///
/// Cheap enough to build per comparison: an empty `Vec` does not allocate.
fn unconstrained() -> TypeExpr {
    TypeExpr {
        name: TypeName::Any,
        args: Vec::new(),
        applied: false,
        nullable: true,
        frozen: false,
        span: Span::new(0, 0),
    }
}

/// Whether a container built to hold `has` may be asked for as `want`.
///
/// Both sides are read out to the longer of the two argument lists, with every
/// position neither of them wrote filled in as [`unconstrained`]. That single
/// step is what makes `list` and `list[any?]` one type, `dict[K]` and
/// `dict[K, any?]` one type, and a container nothing described a `list[any?]` —
/// three questions that used to be three separate special cases answering
/// slightly differently.
///
/// The comparison is by position, which assumes the two name the same container.
/// Every caller has already established that.
pub fn arguments_admit(want: &TypeExpr, has: &[TypeExpr]) -> bool {
    let elided = unconstrained();
    (0..want.args.len().max(has.len())).all(|index| {
        admits((
            want.args.get(index).unwrap_or(&elided),
            has.get(index).unwrap_or(&elided),
        ))
    })
}

/// Whether a container built to hold `has` may be used where `want` is asked.
///
/// Invariant, with one exception that is not a hole in it: `any` is the top type
/// and accepts whatever is there, so `list[any]` still means "a list of
/// anything". That is safe because the *header* is what a write is checked
/// against, not the annotation it arrived through — `xs.push("s")` inside
/// `fn f(xs: list[any])` is refused on the strength of the `list[int]` the
/// caller passed, because [`Heap::describe`] is write-once and the first
/// annotation a container crosses is its type for good.
///
/// Nothing else widens: `list[int]` is not a `list[int?]`, because a `nil`
/// written through the second would be a `nil` read out of the first.
///
/// [`Heap::describe`]: crate::runtime::heap::Heap::describe
fn admits((want, has): (&TypeExpr, &TypeExpr)) -> bool {
    match want.name {
        // `any` does not admit `nil`, so it does not admit a container that may
        // hold one either. `any?` is the spelling for that.
        TypeName::Any => want.nullable || !has.nullable,
        // A const argument compares by equality, which `same_as` already does:
        // `TypeName` derives `PartialEq` and `Const(Int(16))` is not
        // `Const(Int(32))`. That single line is the whole of §3.3's promise
        // that the two are different types, and it is why a const argument was
        // put where a name goes rather than beside it.
        // A pack compares by equality like a const argument does, and for the
        // same reason: by the time two headers are compared both sides have
        // been substituted, so an unexpanded pack here is one nothing bound.
        TypeName::Named(_) | TypeName::Const(_) | TypeName::Pack(_) => want.same_as(has),
    }
}

/// Whether a type argument satisfies the bound its parameter declared —
/// v0.9 §3.2.
///
/// **Not [`admits`].** That one asks whether a container built to hold `has` may
/// be *asked for* as `want`, and it is invariant because a `list[int]` handed
/// out as a `list[int?]` would let a `nil` be written into it. A bound asks
/// something weaker and older: whether this type is one of those. `int` widens
/// to `float` here for the same reason `let x: float = 3` does, and a subclass
/// satisfies a bound naming its parent for the same reason `let a: Animal =
/// Dog()` does. Both are §4.1, and neither is a new relation.
///
/// The two are separate functions because they gave different answers to
/// `(float, int)` and both answers were right.
///
/// `descends` answers whether its first argument's class descends from its
/// second's, which is a question about a hierarchy that this file cannot see —
/// the resolver knows it by name, the evaluator by handle, and they are given
/// as a closure rather than picking one.
pub fn satisfies(
    bound: &TypeExpr,
    arg: &TypeExpr,
    descends: &dyn Fn(&str, &str) -> bool,
) -> bool {
    // An argument that admits `nil` satisfies only a bound that does. `T:
    // float` is a promise about what a `T` is, and `nil` is not a float
    // whatever else it is.
    if arg.nullable && !bound.nullable {
        return false;
    }
    let bound_name = match &bound.name {
        // The unbounded `[T]`, spelled. `any?` constrains nothing and `any`
        // constrains only absence, which the check above has already made.
        TypeName::Any => return true,
        // A bound is a type and a const argument is a value, so the only way to
        // be here is a const argument reaching a *type* parameter — which is
        // the mismatch `built_generic` reports in its own words.
        TypeName::Const(_) => return false,
        // A pack stands for a number of types, and a bound is a claim about
        // one. §3.4 gives a pack no bound, so this is only reachable through a
        // `Ts...` written where a bound goes — which no parameter accepts.
        TypeName::Pack(_) => return false,
        TypeName::Named(name) => name.as_str(),
    };
    let TypeName::Named(arg_name) = &arg.name else {
        // `any` as an *argument* satisfies only an `any` bound, which the arm
        // above already answered. Anything narrower is a claim the argument
        // does not make.
        return false;
    };
    if arg_name == bound_name {
        // Same head, so the question moves to the arguments, where invariance
        // *is* the rule: `Box[list[int]]` does not satisfy `T: list[any]`.
        return arguments_admit(bound, &arg.args);
    }
    // §4.1's one widening, and the hierarchy.
    if bound_name == "float" && arg_name == "int" {
        return true;
    }
    descends(arg_name, bound_name)
}

/// What to say when [`satisfies`] answers no.
///
/// Beside the rule rather than at the two places that report it, for the reason
/// [`refusal`] sits beside [`holds`]: the resolver and the evaluator both raise
/// this, and a message assembled twice is a message that drifts.
///
/// The widening is named only where it could apply. "and `int` where it says
/// `float`" under a bound of `Animal` is true of the language and irrelevant to
/// the reader, which is the kind of help that teaches people to stop reading
/// help.
pub fn bound_help(whose: &str, param: &str, bound: &TypeExpr) -> String {
    let written = bound.written();
    let widens = match &bound.name {
        TypeName::Named(name) if name == "float" => ", and `int`, which widens to it",
        _ => "",
    };
    format!(
        "`{whose}`\u{2019}s `{param}` accepts `{written}`, a class descending from it{widens}"
    )
}

/// Whether `value`'s class is `name` or descends from it.
fn descends_from(heap: &Heap, value: &Value, name: &str) -> bool {
    let mut current = Some(value.class(heap));
    while let Some(id) = current {
        if heap.class(id).name == name {
            return true;
        }
        current = heap.class(id).parent;
    }
    false
}

/// Why `value` does not hold as `ty`, and what to write instead.
///
/// Beside [`holds`] because the two have to agree about what was wrong, and a
/// message assembled at the raise site is a message that drifts from the rule.
/// `what` names the boundary — a binding, a parameter, a return — and leads the
/// sentence, so the caret is not the only thing saying where.
///
/// The `nil` case is separated because it is a different mistake with a
/// different fix. "expected `int`, found nil" would send someone looking for the
/// wrong value; what they need to know is that the annotation forbids absence
/// and that one character admits it.
pub fn refusal(ty: &TypeExpr, value: &Value, heap: &Heap, what: &str) -> (String, Option<String>) {
    let written = ty.written();
    if matches!(value, Value::Nil) && !ty.admits_nil() {
        return (
            format!("{what} is `{written}`, which does not admit `nil`"),
            Some(match ty.name {
                // `any` is the stated top type *minus* `nil`, so the fix is the
                // same character and the resulting type has its own name.
                TypeName::Any => "write `any?` for the type that admits everything".to_string(),
                TypeName::Named(_) | TypeName::Const(_) | TypeName::Pack(_) => {
                    format!("write `{written}?` if it may be absent")
                }
            }),
        );
    }

    let actual = value.type_name(heap);
    // What the value *is*, said as precisely as anything knows. A container that
    // crossed an annotated boundary carries what it was built to hold, and
    // "this is a list" is a poor answer to "why is this not a `list[int]`" when
    // the reason is that it is a `list[string]`. Only where the header adds
    // something: a bare `list` reads as itself.
    let described = value
        .base(heap)
        .handle()
        .and_then(|id| heap.descriptor(id))
        .filter(|held| !held.args.is_empty())
        .map(|held| format!("`{}`", held.written()))
        // A tuple says its length instead, and says it whether or not anything
        // described it. "this is a tuple" is a useless answer to "why is this
        // not a `tuple[int, string]`" when the reason is that it has one
        // element — the arity is the type, per §3.5, so it is what the sentence
        // has to name.
        .or_else(|| match value.base(heap) {
            Value::Tuple(id) => {
                let len = heap.tuple(*id).len();
                let plural = if len == 1 { "" } else { "s" };
                Some(format!("a tuple of {len} element{plural}"))
            }
            _ => None,
        });
    let precise = described.clone().unwrap_or_else(|| an(actual).to_string());
    let message = format!("{what} is `{written}`, but this is {precise}");
    // The specific advice where there is some, and the general shape of the fix
    // otherwise. An annotation refused is always two things a reader might have
    // meant — the value is wrong, or the annotation is — and naming both is
    // what a `help:` line is for. Restating the message would not be.
    let help = match (&ty.name, actual) {
        // §4.1's asymmetry: the conversion exists, the language just will not
        // pick the rounding on the program's behalf.
        (TypeName::Named(name), "float") if name == "int" => {
            "write `int(x)` to say which way it should round".to_string()
        }
        _ => format!(
            "either give it {}, or widen the annotation to admit {precise}",
            an_article(&written),
        ),
    };
    (message, Some(help))
}

/// The same, for a type quoted back rather than named.
fn an_article(written: &str) -> String {
    match written.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => format!("an `{written}`"),
        false => format!("a `{written}`"),
    }
}

/// `a` or `an`, for a type name read aloud in a sentence.
fn an(name: &str) -> String {
    match name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => format!("an {name}"),
        false => format!("a {name}"),
    }
}

// ---------------------------------------------------------------------------
// Type-parameter substitution — v0.9 §3.1, §3.4, §3.7.
//
// Here rather than beside either caller, because there are two and they are in
// different halves of the compiler: the evaluator substitutes a class's
// parameters into an annotation when a method on a generic instance runs, and
// the resolver substitutes an alias's parameters into its body when a use site
// is rewritten. One operation, so one implementation — a second copy of the
// span and qualifier rules below would be a second copy to get wrong.
// ---------------------------------------------------------------------------

/// A type parameter and what it was bound to, in the order the class declared
/// them.
///
/// A slice of pairs rather than a map: a parameter list is one, two, or three
/// long in every program anyone will write, and a linear scan over three
/// entries beats hashing a string.
pub type Bindings = [(String, TypeExpr)];

/// An annotation with every type parameter replaced by what it is bound to.
///
/// `list[T]` becomes `list[int]` on a `Stack[int]`, and the result is an
/// ordinary annotation that the checks already in the evaluator can be pointed
/// at without knowing generics exist. That is the whole trick: a `T` is not a
/// new kind of type, it is a type not yet written down.
///
/// Spans survive substitution — the replacement carries the *use*'s span and
/// not the argument's, so a refusal underlines the parameter the program wrote
/// rather than a bracket in another declaration.
///
/// Answers with a clone when there is nothing to replace, which is every
/// annotation in a non-generic class. A `Cow` was the obvious alternative and
/// is not worth it: the caller needs an owned `TypeExpr` either way, since the
/// checks it feeds take one by reference and outlive nothing.
pub fn substituted(ty: &TypeExpr, bindings: &Bindings) -> TypeExpr {
    if bindings.is_empty() {
        return ty.clone();
    }
    let mut out = ty.clone();
    // A pack in an argument position *splices*: `tuple[Ts...]` with `Ts` bound
    // to three types becomes a `tuple` with three arguments, not one argument
    // that is itself a tuple. This is the whole of what makes a pack different
    // from a parameter, and it is why the argument walk is a `flat_map`.
    out.args = ty
        .args
        .iter()
        .flat_map(|arg| expanded(arg, bindings))
        .collect();
    if let TypeName::Pack(name) = &ty.name {
        // A pack standing where a whole type goes — `args: Ts...` — is the
        // collected tuple itself. Nothing to splice into, so the binding is
        // used as it is.
        let Some((_, bound)) = bindings.iter().find(|(param, _)| param == name) else {
            return out;
        };
        let mut replaced = bound.clone();
        replaced.span = ty.span;
        replaced.nullable = replaced.nullable || ty.nullable;
        replaced.frozen = replaced.frozen || ty.frozen;
        return replaced;
    }
    let TypeName::Named(name) = &ty.name else {
        return out;
    };
    let Some((_, bound)) = bindings.iter().find(|(param, _)| param == name) else {
        return out;
    };
    // A parameter takes no arguments of its own — `T[int]` is not a form — so
    // whatever was bound replaces the name *and* the empty argument list.
    let mut replaced = bound.clone();
    replaced.span = ty.span;
    // The two qualifiers belong to the use and not to the binding. `T?` admits
    // `nil` whatever `T` is, and a `const T` freezes what crosses it — both are
    // words the body wrote about this position, and the argument has no say in
    // either.
    replaced.nullable = replaced.nullable || ty.nullable;
    replaced.frozen = replaced.frozen || ty.frozen;
    replaced
}

/// One argument, substituted — as however many types it turns into.
///
/// One for everything but a bound pack, which is the reason this hands back a
/// list at all. See [`substituted`].
fn expanded(arg: &TypeExpr, bindings: &Bindings) -> Vec<TypeExpr> {
    let TypeName::Pack(name) = &arg.name else {
        return vec![substituted(arg, bindings)];
    };
    match bindings.iter().find(|(param, _)| param == name) {
        // The arguments the pack took, spliced in where it was written. Each is
        // already a finished type — a pack binds to what a construction was
        // given, and a type argument cannot itself be a pack.
        Some((_, bound)) => bound.args.clone(),
        // Nothing bound it, so it stands for itself. A bare `CustomTuple()`
        // leaves this, and `holds` reads an unsubstituted pack as constraining
        // nothing.
        None => vec![arg.clone()],
    }
}

#[cfg(test)]
mod substitution_tests {
    use super::*;
    use crate::syntax::token::Span;

    fn named(name: &str) -> TypeExpr {
        TypeExpr {
            applied: false,
            name: TypeName::Named(name.to_string()),
            args: Vec::new(),
            nullable: false,
            frozen: false,
            span: Span::new(3, 4),
        }
    }

    fn generic(name: &str, args: Vec<TypeExpr>) -> TypeExpr {
        TypeExpr {
            applied: !args.is_empty(),
            args,
            ..named(name)
        }
    }

    #[test]
    fn a_bare_parameter_becomes_its_argument() {
        let bindings = vec![("T".to_string(), named("int"))];
        assert_eq!(substituted(&named("T"), &bindings).written(), "int");
    }

    #[test]
    fn a_parameter_nested_in_a_container_is_reached() {
        let bindings = vec![("T".to_string(), named("int"))];
        let ty = generic("list", vec![named("T")]);
        assert_eq!(substituted(&ty, &bindings).written(), "list[int]");
    }

    #[test]
    fn substitution_keeps_the_span_of_the_use() {
        let bindings = vec![(
            "T".to_string(),
            TypeExpr {
                span: Span::new(99, 100),
                ..named("int")
            },
        )];
        // The report is about the `T` the body wrote, which is at 3..4 — the
        // argument's own span is in another declaration entirely.
        assert_eq!(substituted(&named("T"), &bindings).span, Span::new(3, 4));
    }

    #[test]
    fn the_question_mark_belongs_to_the_use() {
        let bindings = vec![("T".to_string(), named("int"))];
        let optional = TypeExpr {
            nullable: true,
            ..named("T")
        };
        assert!(substituted(&optional, &bindings).admits_nil());
        // And does not leak back the other way: a `T` written plain is not
        // nullable because some other position wrote `T?`.
        assert!(!substituted(&named("T"), &bindings).admits_nil());
    }

    #[test]
    fn an_argument_that_is_itself_generic_substitutes_whole() {
        let bindings = vec![("T".to_string(), generic("list", vec![named("int")]))];
        let ty = generic("dict", vec![named("string"), named("T")]);
        assert_eq!(
            substituted(&ty, &bindings).written(),
            "dict[string, list[int]]"
        );
    }

    #[test]
    fn a_name_no_parameter_claims_is_left_alone() {
        let bindings = vec![("T".to_string(), named("int"))];
        assert_eq!(substituted(&named("Point"), &bindings).written(), "Point");
        // Including one that merely starts the same way: matching is on the
        // whole name and not on a prefix.
        assert_eq!(substituted(&named("Total"), &bindings).written(), "Total");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The annotation `src` would parse to, for a test asserting about one.
    fn annotation(src: &str) -> TypeExpr {
        let tokens = crate::syntax::lexer::Lexer::new(src)
            .tokenize()
            .expect("the annotation lexes");
        crate::syntax::parser::Parser::new(tokens)
            .parse_type_for_test()
            .expect("the annotation parses")
    }

    #[test]
    fn an_annotation_reads_back_as_it_was_written() {
        for src in ["int", "int?", "list[int]", "dict[string, int]", "any", "any?"] {
            assert_eq!(annotation(src).written(), src);
        }
        // `_` is the other spelling of `any`, and a report quotes the type back
        // rather than the characters — there is one type and it has one name.
        assert_eq!(annotation("_").written(), "any");
        assert_eq!(annotation("_?").written(), "any?");
        assert_eq!(annotation("const list[int]").written(), "const list[int]");
    }

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
    fn a_stated_annotation_becomes_the_type_it_names() {
        assert_eq!(stated(&annotation("int")), Type::class("int"));
        assert_eq!(stated(&annotation("int?")), Type::class("int").nullable());
        assert_eq!(
            stated(&annotation("list[int]")),
            Type::generic("list", vec![Type::class("int")])
        );
        // `any` has no `Type` of its own yet — §3.2 distinguishes what the pass
        // concluded from what the program said, and `Type` cannot carry the
        // difference until it has somewhere to put it. Both admit every value.
        assert_eq!(stated(&annotation("any")), Type::Unknown);
        assert_eq!(stated(&annotation("_")), Type::Unknown);
    }

    #[test]
    fn only_a_nullable_type_admits_nil() {
        assert!(!Type::class("int").admits_nil());
        assert!(Type::class("int").nullable().admits_nil());
        // `Unknown` admits it: the pass has not been told, and refusing would be
        // a claim it has no basis for.
        assert!(Type::Unknown.admits_nil());
        assert!(!Type::module("math").admits_nil());

        assert_eq!(Type::class("int").nullable().to_string(), "int?");
        assert_eq!(
            Type::generic("list", vec![Type::class("int")])
                .nullable()
                .to_string(),
            "list[int]?"
        );
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

    #[test]
    fn a_bound_is_ordinary_matching_and_not_a_second_relation() {
        // No hierarchy: every case here is decidable without one.
        let flat = |_: &str, _: &str| false;
        let holds = |bound: &str, arg: &str| {
            satisfies(&annotation(bound), &annotation(arg), &flat)
        };

        assert!(holds("float", "float"));
        // §4.1's one widening, and it is the reason `satisfies` is not
        // `admits`: that one answers `false` here, correctly, because a
        // `list[int]` handed out as a `list[float]` could be written into.
        assert!(holds("float", "int"));
        assert!(!holds("int", "float"));
        assert!(!holds("float", "string"));

        // An unbounded parameter is `any?`, which constrains nothing.
        assert!(holds("_?", "int"));
        assert!(holds("_?", "string"));
        assert!(holds("_?", "int?"));
        // `any` is the top type *minus* absence, so it still rules one thing out.
        assert!(holds("_", "int"));
        assert!(!holds("_", "int?"));

        // A bound that does not admit `nil` is not satisfied by an argument
        // that does — `T: float` promises what a `T` is, and `nil` is not one.
        assert!(!holds("float", "float?"));
        assert!(holds("float?", "float"));

        // Same head, so the question moves to the arguments, where invariance
        // is the rule.
        assert!(holds("list[int]", "list[int]"));
        assert!(!holds("list[int]", "list[string]"));
        assert!(holds("list[any?]", "list[int]"));
    }

    #[test]
    fn a_bound_admits_a_subclass_of_what_it_names() {
        // The hierarchy is the caller's to know: the resolver reads it by name
        // and the evaluator by handle, and this file sees neither.
        let chain = |name: &str, ancestor: &str| (name, ancestor) == ("Dog", "Animal");
        assert!(satisfies(&annotation("Animal"), &annotation("Dog"), &chain));
        assert!(!satisfies(&annotation("Dog"), &annotation("Animal"), &chain));
        assert!(!satisfies(&annotation("Animal"), &annotation("int"), &chain));
    }

    #[test]
    fn the_bound_help_names_the_widening_only_where_it_applies() {
        // Help that is true of the language and irrelevant to the reader is how
        // people learn to stop reading help.
        let float = bound_help("NumberBox", "T", &annotation("float"));
        assert!(float.contains("`int`, which widens to it"), "{float}");
        let animal = bound_help("Pen", "T", &annotation("Animal"));
        assert!(!animal.contains("int"), "{animal}");
    }
}

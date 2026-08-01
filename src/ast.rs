use std::rc::Rc;

use crate::token::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    /// True division: always produces a float, as in Python 3.
    Div,
    /// Floor division: `int // int` stays an int.
    FloorDiv,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Membership: a dict key, a list element, or a substring.
    In,
}

/// Kept apart from `BinaryOp` because these short-circuit: the evaluator must
/// not eagerly evaluate the right operand, and a shared variant would make that
/// easy to get wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    And,
    Or,
}

/// Where the resolver decided a name lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// `hops` scopes out from the current one, then slot `index`. The runtime
    /// scope chain mirrors lexical nesting exactly, which is what makes a
    /// static hop count valid.
    Local { hops: u16, index: u16 },
    /// Not found in any enclosing local scope, so it is looked up by name at
    /// run time. Globals stay dynamic because the REPL defines them a line at
    /// a time, and because a program may call a function declared further down.
    Global,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

/// A variable reference, before and after resolution.
///
/// `slot` is `None` as the parser leaves it and `Some` once the resolver has
/// run. The evaluator treats `None` as a bug in the pipeline rather than a
/// condition to handle.
#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub name: String,
    pub slot: Option<Slot>,
}

impl Var {
    pub fn new(name: impl Into<String>) -> Self {
        Var {
            name: name.into(),
            slot: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Nil,
    Var(Var),
    List(Vec<Expr>),
    /// Key-value pairs in source order, which is the order the dict keeps.
    Dict(Vec<(Expr, Expr)>),
    Unary {
        op: UnaryOp,
        rhs: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Logical {
        op: LogicalOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    /// `xs[a:b]`, with either bound omissible: `xs[:b]`, `xs[a:]`, `xs[:]`.
    ///
    /// A separate node rather than an `Index` holding a range, because there is
    /// no range value in the language and inventing one to carry two optional
    /// ints would be a worse trade than a second node.
    Slice {
        target: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
    },
    Field {
        target: Box<Expr>,
        name: String,
    },
    /// `super.name`, which is a lookup that starts at the parent class but
    /// binds to the current receiver.
    ///
    /// A node of its own rather than a `Field` over a `super` variable, because
    /// those two halves come from different places: the class to search from is
    /// in the scope wrapped around the methods, and the receiver to bind to is
    /// the enclosing method's `self`. Both are ordinary variable references, so
    /// the resolver handles them without knowing what they mean.
    Super {
        name: String,
        parent: Var,
        receiver: Var,
    },
    /// `target` is restricted to an assignable form by the parser, so the
    /// evaluator can assume it is an ident, index, or field access.
    Assign {
        target: Box<Expr>,
        value: Box<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
    /// How many slots this block's scope needs, filled in by the resolver, so
    /// the scope can be allocated at its final size in one go. For a function
    /// body this counts the parameters too, which occupy the first slots.
    pub slot_count: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub span: Span,
    /// Whether this is the `self` the parser inserted, rather than a parameter
    /// someone wrote.
    ///
    /// A flag rather than a comparison against [`SELF`], because the name is
    /// only unambiguous while `self` is a keyword — an invariant that lives in
    /// the lexer and would be silently assumed here. The parser knows which
    /// parameter it invented, so it says so.
    pub receiver: bool,
}

/// The receiver's name inside a method body.
///
/// `self` is bound as an ordinary parameter that the parser inserts, rather
/// than as something the evaluator injects. Everything downstream then treats
/// it as a local: the resolver gives it a slot, a closure nested in a method
/// captures it through the scope chain like any other name, and `read` needs no
/// special case. What the keyword buys is the error when it is used outside a
/// method, which would otherwise read `undefined variable`.
pub const SELF: &str = "self";

/// The parent class's name inside a method body.
///
/// Bound the same way, but as a slot in a scope wrapped around the methods of a
/// class that extends another, rather than as a parameter — its value is fixed
/// when the class is declared, not per call. A closure nested in a method
/// reaches it through the same chain that carries [`SELF`].
pub const SUPER: &str = "super";

/// A method the language calls on the program's behalf.
///
/// These are the methods nobody writes a call to: `Point(1, 2)` reaches `init`,
/// `len(x)` reaches `len`, `if x` reaches `bool`, `a + b` reaches `add`.
/// Declared with `op` rather than `fn` so that being one is stated rather than
/// inferred from the name, which is what makes the misspelling an error instead
/// of a method nothing ever calls.
///
/// A closed set on purpose. `Op::from_name` is the only way in, so `op lenght`
/// cannot compile, and every member has to be listed in [`OPS`] to be reachable
/// — see `every_listed_op_round_trips_through_its_name`.
///
/// The declaration order is load-bearing. [`Op::index`] is the discriminant, and
/// it indexes `Class::slots`, so [`OPS`] has to list the members in the same
/// order — two ops sharing an index would not fail loudly the way a missing
/// [`crate::class::BUILTINS`] entry does, it would silently let one override the
/// other. `every_op_indexes_its_own_slot` is what holds that up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    /// Runs on a freshly built instance. `Point(1, 2)` reaches it.
    Init,

    // The conversions, each named for the type it produces. That is also how the
    // language already spells them — `string` and `bool` are globals bound to
    // classes, and calling a class runs its `init` — so `string(x)` and
    // `print(x)` reach one op rather than needing two names for one question.
    /// `if x`, `!x`, `&&`, `||`, and `bool(x)`.
    Bool,
    /// `print(x)`, `string(x)`, and `x` printed inside a collection.
    Str,
    /// `int(x)`.
    Int,
    /// `float(x)`.
    Float,
    /// `list(x)`.
    List,
    /// `dict(x)`.
    Dict,

    /// `==` and `!=`, and `needle in aList`, which compares items.
    ///
    /// Defining it costs the class its use as a dict key: see [`crate::dict::Key`],
    /// which cannot ask a class anything.
    Eq,
    /// All four of `<`, `<=`, `>` and `>=`, from one method answering `-1`, `0`
    /// or `1` — C++'s `<=>`, which is where the shape comes from.
    ///
    /// It is the only op that can answer `<=` and `>=`. A class declaring just
    /// [`Op::Lt`] gets `<` and nothing else, exactly as writing `operator<` in
    /// C++ leaves `a <= b` a compile error rather than deriving it. Deriving it
    /// would mean assuming the order is total, which is the assumption `<=>`
    /// exists to let a class refuse.
    Cmp,
    /// `<`, which beats [`Op::Cmp`] for that one operator.
    ///
    /// Worth having beside `cmp` for the same reason C++ keeps `operator<`: a
    /// class may know how to answer one comparison cheaply — a length, a tag —
    /// without being able to place itself in a total order at all.
    Lt,
    /// `>`, which beats [`Op::Cmp`] for that one operator.
    Gt,

    /// `a + b`.
    Add,
    /// `a - b`.
    Sub,
    /// `a * b`.
    Mul,
    /// `a / b`.
    Div,
    /// `a // b`.
    FloorDiv,
    /// `a % b`.
    Rem,
    /// `-x`.
    Neg,

    /// `len(x)`.
    Len,
    /// `x[i]`, one index at a time.
    ///
    /// Not `x[a:b]`: there is no value in the language that means "1 to 3", so
    /// there is nothing to hand an op that takes one argument. Slicing a class
    /// that declares this is refused rather than quietly reaching past it to the
    /// list or string underneath.
    Get,
    /// `x[i] = v`.
    Set,
    /// `needle in x`, where `x` is the haystack. The other side of `in` is
    /// [`Op::Eq`]'s, since searching a list compares its items.
    Contains,
    /// `for item in x`. Returns a list — see Iteration in DESIGN.md for why it
    /// is eager.
    Iter,
}

/// Every [`Op`], for validating a declaration, for listing them in the error
/// when one does not exist, and for walking a class's slots.
///
/// In discriminant order, which [`Op`] explains is not a nicety.
pub static OPS: &[Op] = &[
    Op::Init,
    Op::Bool,
    Op::Str,
    Op::Int,
    Op::Float,
    Op::List,
    Op::Dict,
    Op::Eq,
    Op::Cmp,
    Op::Lt,
    Op::Gt,
    Op::Add,
    Op::Sub,
    Op::Mul,
    Op::Div,
    Op::FloorDiv,
    Op::Rem,
    Op::Neg,
    Op::Len,
    Op::Get,
    Op::Set,
    Op::Contains,
    Op::Iter,
];

/// What happens when a binary operator's *right* operand is the one whose class
/// defines the op.
///
/// Not uniform, and not a preference. `3 == Money(3)` has to reach `Money`'s
/// `eq`, or `==` is asymmetric depending on which side you wrote — indefensible.
/// But `2 - Money(3)` reaching `Money`'s `sub` computes `3 - 2` and is wrong by
/// a sign, with nothing to catch it. So arithmetic asks the left operand only,
/// and a reflected `cmp` has to invert the answer it gets.
///
/// Held as data on the op rather than as branches inside `binary`, so that
/// adding one states its rule in the same exhaustive match that gives it a name,
/// and so a reflected `op radd` later is an addition rather than a redesign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflect {
    /// Call it as written, because the op is symmetric.
    Same,
    /// Call it and negate the answer, because the operands arrived swapped.
    Negate,
    /// Do not ask the right operand at all. Either reflecting it would be wrong,
    /// or — for every op that is not a binary operator — there is no second
    /// operand to reflect.
    Never,
}

impl Op {
    /// How many slots a class has, so `Class::slots` sizes itself from the list
    /// rather than from a number someone has to remember to bump.
    pub const COUNT: usize = OPS.len();

    /// The name written after `op`.
    ///
    /// An exhaustive match, so a new member cannot be added without giving it
    /// one — the same guard [`crate::error::ErrorKind::class_name`] uses.
    pub fn name(self) -> &'static str {
        match self {
            Op::Init => "init",
            Op::Bool => "bool",
            Op::Str => "string",
            Op::Int => "int",
            Op::Float => "float",
            Op::List => "list",
            Op::Dict => "dict",
            Op::Eq => "eq",
            Op::Cmp => "cmp",
            Op::Lt => "lt",
            Op::Gt => "gt",
            Op::Add => "add",
            Op::Sub => "sub",
            Op::Mul => "mul",
            Op::Div => "div",
            Op::FloorDiv => "floordiv",
            Op::Rem => "rem",
            Op::Neg => "neg",
            Op::Len => "len",
            Op::Get => "get",
            Op::Set => "set",
            Op::Contains => "contains",
            Op::Iter => "iter",
        }
    }

    /// Where this op's slot sits on a class.
    ///
    /// The discriminant, so the lookup is an array read rather than a name
    /// hashed on every `if` — the same reason [`crate::class::Builtin::index`]
    /// exists, and the trap DESIGN.md's Slots are cached fields section names.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Whether the right operand's class may answer for this op, and what has to
    /// happen to the answer if it does. See [`Reflect`].
    ///
    /// Exhaustive on purpose: a new op cannot be added without deciding, and
    /// `Never` is the honest answer for one that has no second operand.
    pub fn reflect(self) -> Reflect {
        match self {
            Op::Eq => Reflect::Same,
            Op::Cmp => Reflect::Negate,
            // C++ does reach the right operand for `==` and for `<=>`, and does
            // not for a plain `operator<`. The asymmetry is not an oversight
            // there and is not one here: `<=>` answers a question about a pair
            // and so can be read backwards, while `op lt` answers about `<`
            // specifically and has no second reading.
            Op::Lt
            | Op::Gt
            | Op::Init
            | Op::Bool
            | Op::Str
            | Op::Int
            | Op::Float
            | Op::List
            | Op::Dict
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::FloorDiv
            | Op::Rem
            | Op::Neg
            | Op::Len
            | Op::Get
            | Op::Set
            | Op::Contains
            | Op::Iter => Reflect::Never,
        }
    }

    /// How many parameters the op takes, not counting `self`, or `None` for one
    /// that takes what its class chooses.
    ///
    /// Checked where the declaration is parsed, which is the only place it can be
    /// checked *well*: the parameter list is in hand, so the error points at the
    /// parameters rather than at some later `if x` that tried to call them. It is
    /// also what lets the calls the language makes on a program's behalf be
    /// arity-free — nothing has to carry a span to report a mismatch that cannot
    /// happen.
    ///
    /// `init` is the one exception, and has to be: a constructor's parameters are
    /// the class's own business, and `Point(1, 2)` already reports a mismatch
    /// against the source that spelled the call.
    pub fn arity(self) -> Option<usize> {
        match self {
            Op::Init => None,

            // A conversion answers a question about the receiver, so there is
            // nothing else to pass.
            Op::Bool
            | Op::Str
            | Op::Int
            | Op::Float
            | Op::List
            | Op::Dict
            | Op::Neg
            | Op::Len
            | Op::Iter => Some(0),

            // The other operand, the key, or the needle.
            Op::Eq
            | Op::Cmp
            | Op::Lt
            | Op::Gt
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Div
            | Op::FloorDiv
            | Op::Rem
            | Op::Get
            | Op::Contains => Some(1),

            // `x[i] = v` is the only one with two.
            Op::Set => Some(2),
        }
    }

    pub fn from_name(name: &str) -> Option<Op> {
        OPS.iter().copied().find(|op| op.name() == name)
    }
}

/// What an [`StmtKind::Import`] binds.
///
/// The two forms differ in what lands in the scope and in nothing else: both
/// name one module, and both load it exactly once. `from` is not a second
/// mechanism, it is a choice about how many names to spend.
#[derive(Clone, Debug, PartialEq)]
pub enum ImportNames {
    /// `import math` — the module itself, under the name it was imported by.
    Module,
    /// `from math import floor, ceil` — each name, bound to what the module
    /// declared under it.
    Names(Vec<ImportName>),
}

/// One name in a `from … import` list.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportName {
    pub name: String,
    /// Its own span, so a module that declares three of the four names asked for
    /// can have the caret put under the fourth.
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: String,
    /// Where the name was written, which the body's span cannot stand in for: a
    /// report about a *declaration* should underline the word being declared,
    /// not the twenty lines under it.
    pub name_span: Span,
    /// For a method, `self` is `params[0]`; see [`SELF`].
    pub params: Vec<Param>,
    pub body: Block,
    /// Set when the declaration used `op`, which the parser allows only inside a
    /// class body — so a plain function always leaves this `None`.
    pub op: Option<Op>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Which keyword introduced a binding.
///
/// One field rather than a pair of bools, because half the combinations two
/// bools can express are not forms the language has — a rebindable name holding
/// a frozen value, say. The two questions anyone asks of it are derived instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindKind {
    /// `let`: the name may be reassigned.
    Let,
    /// `final`: the name is bound once. The object it names is untouched, so a
    /// `final` list still grows — see [`BindKind::Const`] for the other one.
    Final,
    /// `const`: the name is bound once *and* the value is frozen, deeply, and
    /// through every other name that already reaches it.
    Const,
}

impl BindKind {
    pub fn mutable(&self) -> bool {
        matches!(self, BindKind::Let)
    }

    pub fn freezes(&self) -> bool {
        matches!(self, BindKind::Const)
    }

    /// How the keyword is written, for error messages that quote it back.
    pub fn word(&self) -> &'static str {
        match self {
            BindKind::Let => "let",
            BindKind::Final => "final",
            BindKind::Const => "const",
        }
    }
}

/// What a class declaration leaves open.
///
/// There are exactly two ways to attach behaviour to a type from outside — a
/// subclass, and an `extend` block — so there are four states, and each has its
/// own word rather than being spelled by stacking modifiers. See DESIGN.md.
///
/// | | inherit | `extend` |
/// |---|---|---|
/// | [`Openness::Open`] | yes | yes |
/// | [`Openness::Final`] | no | yes |
/// | [`Openness::Complete`] | yes | no |
/// | [`Openness::Sealed`] | no | no |
///
/// The two predicates below are exhaustive matches on purpose: a fifth variant
/// cannot be added without answering for both doors, which is the only way the
/// table above and the code can be made to stay in step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Openness {
    /// `class Point { … }`.
    Open,
    /// `final class Point { … }` — no subclass, but its vocabulary may grow.
    Final,
    /// `complete class Point { … }` — the method table is done; subclasses are
    /// still welcome, since a subclass adds nothing to the class it descends
    /// from.
    Complete,
    /// `sealed class Point { … }` — neither door. A composite rather than a
    /// third door: `sealed` is `final` and `complete` at once, given its own
    /// word so the common case reads as one.
    Sealed,
}

impl Openness {
    /// Whether a class may name this one after `extends`.
    pub fn closes_inheritance(self) -> bool {
        matches!(self, Openness::Final | Openness::Sealed)
    }

    /// Whether an `extend` block may add a method to this one.
    pub fn closes_extension(self) -> bool {
        matches!(self, Openness::Complete | Openness::Sealed)
    }

    /// The keyword as written, for a report that quotes it back. `None` is the
    /// declaration that used no modifier and so has nothing to quote.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Openness::Open => None,
            Openness::Final => Some("final"),
            Openness::Complete => Some("complete"),
            Openness::Sealed => Some("sealed"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Expr(Expr),
    /// `let`, `final`, and `const` share a node; [`BindKind`] distinguishes
    /// them so a reassignment can be rejected with the original binding in hand.
    Let {
        name: String,
        value: Expr,
        bind: BindKind,
        /// Where the binding goes. Always `Local { hops: 0, .. }` or `Global`.
        slot: Option<Slot>,
    },
    /// Shared so a closure can hold the body without deep-copying it each time
    /// the declaration is executed.
    Fn {
        decl: Rc<FnDecl>,
        /// Where the function's own name is bound, as for `Let`.
        slot: Option<Slot>,
    },
    /// A class declaration. The methods are ordinary functions whose first
    /// parameter is the receiver, so nothing about calling one is special.
    Class {
        name: String,
        /// The class this one extends, resolved in the enclosing scope like any
        /// other name — a superclass is an ordinary value, not a static label.
        parent: Option<Var>,
        /// Where the parent was named, kept for the same reason `Extend` keeps
        /// `target_span`: the statement's own span covers the whole body, and a
        /// report about the *parent* should underline the word naming it rather
        /// than the twenty lines that follow. [`Var`] carries no span of its own.
        parent_span: Option<Span>,
        methods: Vec<Rc<FnDecl>>,
        /// Which of the two doors the declaration closed, if either.
        openness: Openness,
        /// Where the class's own name is bound, as for `Let`.
        slot: Option<Slot>,
    },
    /// Methods added to a type that already exists.
    ///
    /// The type is named by an ordinary [`Var`], not a static label, so
    /// `extend int` and `extend Money` take the same path — and a name that turns
    /// out to hold something other than a class is an error at run time, where
    /// every other "this is not what you thought" is.
    ///
    /// No `parent` and no `slot`: an extension declares no type and binds no
    /// name. Nothing changes about the class it names except what can be found
    /// *beside* it — see `Interp::extensions`.
    Extend {
        target: Var,
        /// Where the type was named, kept because the statement's own span covers
        /// the whole body — and a report about the *type* should underline the
        /// word that names it, not the twenty lines that follow.
        target_span: Span,
        /// Never an `op`, which the parser refuses. An extension may add to a
        /// type; it may not change how the language dispatches on it.
        methods: Vec<Rc<FnDecl>>,
    },
    /// `import math`, or `from math import floor, ceil`.
    ///
    /// No `slot`, unlike every other binding form. An import is valid only at
    /// the top level — the resolver refuses one anywhere else — and a top-level
    /// name has no slot to reserve, so the evaluator declares into the importing
    /// module's scope by name. The field would be `Some(Slot::Global)` at every
    /// import there will ever be.
    Import {
        /// The module named after `import`, or after `from`.
        module: String,
        /// Where it was named, so a report about the *module* underlines the
        /// word naming it and not the list of names that follows.
        module_span: Span,
        names: ImportNames,
    },
    If {
        cond: Expr,
        then: Block,
        /// A `Block` for `else`, or another `If` for `else if`.
        otherwise: Option<Box<Stmt>>,
    },
    While {
        cond: Expr,
        body: Block,
    },
    For {
        var: String,
        iter: Expr,
        body: Block,
        /// The loop variable lives in the body's scope, since a fresh one is
        /// made per iteration.
        slot: Option<Slot>,
    },
    Return(Option<Expr>),
    /// `try { … } catch e { … }`.
    ///
    /// The handler is mandatory. A `try` with no `catch` would only be useful
    /// with a `finally` to pair it with, and there is deliberately no `finally` —
    /// see Errors as values in DESIGN.md.
    ///
    /// The two blocks are separate scopes, and the try block's bindings are not
    /// visible to the handler on purpose: a `let` inside `try` may not have run
    /// when the error fired, so sharing one scope would let the handler read a
    /// slot that was never written.
    Try {
        body: Block,
        /// The name the caught error is bound to. It lives in the handler's
        /// scope, taking slot 0 there, exactly as a `for` loop variable does.
        binding: String,
        handler: Block,
        slot: Option<Slot>,
    },
    /// `throw expr`, where `expr` must evaluate to an instance of `Error`.
    ///
    /// The restriction is checked at the `throw` rather than at the `catch`, so
    /// the error names the mistake instead of surfacing later as a missing field
    /// on whatever was thrown.
    Throw(Expr),
    Block(Block),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_states_are_the_four_combinations() {
        // The table in `Openness`'s docs, written once more where a change to
        // either predicate has to disagree with it. `Sealed` being exactly the
        // other two at once is the claim worth pinning: it is a spelling of the
        // pair, not a third door.
        let table = [
            (Openness::Open, false, false, None),
            (Openness::Final, true, false, Some("final")),
            (Openness::Complete, false, true, Some("complete")),
            (Openness::Sealed, true, true, Some("sealed")),
        ];
        for (openness, inheritance, extension, word) in table {
            assert_eq!(openness.closes_inheritance(), inheritance, "{openness:?}");
            assert_eq!(openness.closes_extension(), extension, "{openness:?}");
            assert_eq!(openness.word(), word, "{openness:?}");
        }

        // Every state reached, so the four rows above are the whole table and not
        // four of five.
        let states: Vec<_> = table
            .iter()
            .map(|(o, ..)| (o.closes_inheritance(), o.closes_extension()))
            .collect();
        for combination in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(states.contains(&combination), "{combination:?} unreachable");
        }
    }

    #[test]
    fn a_modifier_is_spelled_the_way_it_is_written() {
        // The words the parser matches and the words a report quotes back are the
        // same list, so a rename cannot land in one and not the other.
        for openness in [Openness::Final, Openness::Complete, Openness::Sealed] {
            let word = openness.word().expect("a modifier has a word");
            assert!(
                crate::token::KEYWORDS.contains(&word),
                "`{word}` is not a reserved word, so it cannot be a modifier"
            );
        }
    }

    #[test]
    fn every_listed_op_round_trips_through_its_name() {
        for op in OPS {
            assert_eq!(
                Op::from_name(op.name()),
                Some(*op),
                "{} does not look up",
                op.name()
            );
        }
    }

    #[test]
    fn no_two_ops_share_a_name() {
        let mut names: Vec<&str> = OPS.iter().map(|op| op.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two ops answer to the same name");
    }

    #[test]
    fn a_name_that_is_not_an_op_does_not_look_up() {
        assert_eq!(Op::from_name("innit"), None);
        assert_eq!(Op::from_name(""), None);
    }

    #[test]
    fn every_op_indexes_its_own_slot() {
        // `index` is the discriminant and `OPS` is written by hand, so the two
        // can drift. What that costs is why this test exists: two ops landing on
        // one slot means declaring `op add` silently overrides `op sub`, with
        // nothing anywhere raising a word about it. A missing `BUILTINS` entry
        // panics; this would not.
        for (position, op) in OPS.iter().enumerate() {
            assert_eq!(
                op.index(),
                position,
                "`{}` is listed at {position} but indexes {}",
                op.name(),
                op.index()
            );
        }
        assert_eq!(
            Op::COUNT,
            OPS.len(),
            "a class would have the wrong number of slots"
        );
    }

    #[test]
    fn only_the_comparisons_are_reflected() {
        // Pins the argument in `Reflect`'s doc comment, which is the kind of
        // thing a later op gets wrong by copying its neighbour: reflecting `sub`
        // computes `b - a` and is wrong by a sign. Written as a full partition so
        // that adding an op forces a decision here as well as in `reflect`.
        //
        // `lt` and `gt` sit with `sub` rather than with `cmp`, which is the line
        // C++ draws too — `operator<=>` has a reversed candidate and `operator<`
        // does not.
        for op in OPS {
            let expected = match op {
                Op::Eq => Reflect::Same,
                Op::Cmp => Reflect::Negate,
                _ => Reflect::Never,
            };
            assert_eq!(op.reflect(), expected, "`{}` reflects wrongly", op.name());
        }
    }

    #[test]
    fn arity_is_what_the_language_passes() {
        // Spelled out rather than derived, because this is the one property of an
        // op that nothing else can check: the parser refuses a declaration that
        // disagrees with `arity`, so `arity` and the call the language makes are
        // the only two statements of the truth, and they are steps apart. A
        // number wrong here is a class that cannot declare a working op at all.
        for op in OPS {
            let expected = match op {
                // A constructor's parameters are the class's own.
                Op::Init => None,
                Op::Set => Some(2),
                Op::Eq
                | Op::Cmp
                | Op::Lt
                | Op::Gt
                | Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Div
                | Op::FloorDiv
                | Op::Rem
                | Op::Get
                | Op::Contains => Some(1),
                Op::Bool
                | Op::Str
                | Op::Int
                | Op::Float
                | Op::List
                | Op::Dict
                | Op::Neg
                | Op::Len
                | Op::Iter => Some(0),
            };
            assert_eq!(op.arity(), expected, "`{}` takes the wrong count", op.name());
        }
    }
}

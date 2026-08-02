//! The operators the language has, and the slots a class fills to answer them.
//!
//! One table, [`OPS`], is the closed set. A slot is an index into it, so
//! dispatch is an array read rather than a name hashed on every call, and adding
//! an operator means adding a row here and nowhere else. v0.7's bitwise slots
//! and v0.10's `range` and `next` are that row.

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
    /// Defining it costs the class its use as a dict key: see [`crate::runtime::dict::Key`],
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
    /// hashed on every `if` — the same reason [`crate::runtime::class::Builtin::index`]
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
#[cfg(test)]
mod tests {
    use super::*;

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

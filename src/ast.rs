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

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: String,
    /// For a method, `self` is `params[0]`; see [`SELF`].
    pub params: Vec<Param>,
    pub body: Block,
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
        methods: Vec<Rc<FnDecl>>,
        /// Where the class's own name is bound, as for `Let`.
        slot: Option<Slot>,
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
    Block(Block),
}

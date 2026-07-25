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
    Field {
        target: Box<Expr>,
        name: String,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Expr(Expr),
    /// `let` and `const` share a node; `mutable` distinguishes them so a
    /// reassignment can be rejected with the original binding in hand.
    Let {
        name: String,
        value: Expr,
        mutable: bool,
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

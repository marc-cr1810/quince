//! The tree the parser produces and every pass after it walks.
//!
//! Three files. This one holds the shapes a program is made of — expressions,
//! statements, and the blocks that hold them; [`op`] holds the operator table a
//! class answers through; [`decl`] holds the declaration furniture, which is
//! where the modifier words and type annotations of v0.7 onward land.
//!
//! `decl` and `op` are re-exported here, so an `Op` is `ast::Op` from anywhere
//! else in the crate. Splitting the file is a fact about where the source lives,
//! not a third path for a caller to learn.

pub mod decl;
pub mod op;

pub use decl::{
    BindKind, FnDecl, ImportName, ImportNames, Openness, Param, SELF, SUPER,
};
pub use op::{BinaryOp, LogicalOp, OPS, Op, Reflect, UnaryOp};

use std::rc::Rc;

use crate::syntax::doc::Doc;
use crate::syntax::token::Span;

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
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

/// Which keyword introduced a binding.
///
/// One field rather than a pair of bools, because half the combinations two
/// bools can express are not forms the language has — a rebindable name holding
/// a frozen value, say. The two questions anyone asks of it are derived instead.

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
        /// The `##` block written above it. A binding has no parameters and no
        /// return, so this carries a summary and the parser refuses the rest.
        doc: Option<Doc>,
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
        /// The `##` block written above it. A summary only, as for a binding —
        /// a class takes no arguments; its `op init` does, and documents them.
        doc: Option<Doc>,
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

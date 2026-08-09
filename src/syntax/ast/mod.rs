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
    BindKind, ConstArg, FieldDecl, FnDecl, ImportName, ImportNames, Openness, Param, ParamKind,
    SELF, SUPER, TypeExpr, TypeName, TypeParam, Visibility, written_params,
};
pub use op::{BinaryOp, LogicalOp, OPS, Op, Reflect, ShortAssignOp, UnaryOp};

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
    /// `(101, "Bob", true)` — an arbitrary-arity product, v0.9 §3.5.
    ///
    /// Zero, one, or many. The one-element case is written `(42,)` and the
    /// trailing comma is not optional there, because `(42)` is a parenthesised
    /// int and has been since v0.1 — the comma is the whole of what makes a
    /// tuple, and a language where one form silently became the other would
    /// have no way to write the grouping.
    Tuple(Vec<Expr>),
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
        args: Vec<CallArg>,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    /// `Pair[int, string]` — a generic class supplied with its arguments.
    ///
    /// Only the two-or-more case. `Stack[int]` is an [`ExprKind::Index`] and
    /// stays one, because nothing in the grammar can tell it from `xs[i]`:
    /// both are a name, a bracket, and one expression. Which of the two it
    /// means is decided by what the target turns out to hold, at run time,
    /// where `extend` already settles the same question — see [`StmtKind::Extend`].
    ///
    /// So this node is not "type application" and the single-argument one
    /// "indexing". They are one operation, and this variant exists only because
    /// a comma inside brackets had no meaning until now and the parser had
    /// nowhere to put a second expression.
    TypeArgs {
        target: Box<Expr>,
        args: Vec<Expr>,
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
        /// Written `?.` rather than `.`, which answers `nil` for the whole
        /// remaining chain when the receiver is `nil`.
        ///
        /// The *whole* chain and not this link alone, which is the only reading
        /// that makes `a?.b.c` mean what it looks like — `a` being `nil` should
        /// not then raise on reaching `.c` of a `nil`. [`ExprKind::Chain`] is
        /// what bounds "the rest".
        optional: bool,
    },
    /// A postfix chain containing at least one `?.`.
    ///
    /// A wrapper the parser adds around the completed chain, and the thing that
    /// says where short-circuiting stops. Without it there is no node that knows
    /// `a?.b.c` is one expression rather than a `.c` applied to whatever
    /// `a?.b` produced — and the difference is exactly whether the `.c` runs.
    ///
    /// Absent from every chain that has no `?.` in it, so nothing a program
    /// wrote before v0.7 gains a node.
    Chain(Box<Expr>),
    /// `lhs ?? rhs` — the right side, but only when the left is `nil`.
    ///
    /// Not a [`BinaryOp`], because it short-circuits: the right side is an
    /// alternative rather than an operand, and evaluating it when the left
    /// answered would run whatever the program wrote there. The same reason
    /// [`ExprKind::Logical`] is separate from [`ExprKind::Binary`].
    Coalesce {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `value is T` — whether `value` has that type, as a bool.
    Is {
        value: Box<Expr>,
        ty: TypeExpr,
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
    /// `target op= value` — v0.8 §3.7.
    ///
    /// A node rather than a desugaring into [`ExprKind::Assign`] over an
    /// [`ExprKind::Binary`], because the rule is `a = a op b` *with the target
    /// evaluated once*: `d[f()] += 1` calls `f` a single time, and a rewrite
    /// that mentioned the target twice would call it twice. Evaluating once is
    /// the whole reason the form exists, so it is the thing the tree records.
    ///
    /// There is no separate in-place operator slot. `op` is the binary
    /// operator's own, so a class defining `op add` gets `+=` for free — see
    /// §3.7's decision.
    AssignOp {
        target: Box<Expr>,
        op: BinaryOp,
        value: Box<Expr>,
    },
    /// `target and= value`, `target or= value`, and `target ??= value`.
    ///
    /// Separate from [`ExprKind::AssignOp`] rather than a fourteenth operator in
    /// it, because the rule is a different rule. A compound assignment always
    /// computes `a op b`; these three look at what the target already holds and
    /// may answer with it, leaving `value` unevaluated and the target unwritten.
    /// `count ??= expensive()` not calling `expensive` is the whole point, and a
    /// node shared with `+=` would make eager evaluation the natural way to
    /// implement it. The same reason [`ExprKind::Coalesce`] is not a
    /// [`BinaryOp`].
    ///
    /// The target is still evaluated exactly once, as for [`ExprKind::AssignOp`]:
    /// `d[f()] ??= 0` calls `f` a single time whether or not it assigns.
    AssignShort {
        target: Box<Expr>,
        op: ShortAssignOp,
        value: Box<Expr>,
    },
}

/// One argument at a call site, positional or named.
///
/// A struct rather than a bare [`Expr`] because v0.8 §3.6 lets a caller target a
/// parameter by name — `connect("host", timeout: 5000)` — and which parameter an
/// argument fills is a property of the *call*, not of the value it computes.
#[derive(Clone, Debug, PartialEq)]
pub struct CallArg {
    /// The parameter this argument names, and where the name was written.
    ///
    /// `None` is the ordinary positional argument, which is every argument
    /// written before v0.8. The span is carried because every refusal this form
    /// has — a name no parameter answers to, a parameter filled twice — is about
    /// the name rather than about the value after it.
    pub name: Option<(String, Span)>,
    pub value: Expr,
}

impl CallArg {
    /// An argument written without a name, which is what every call site
    /// building one by hand produces.
    pub fn positional(value: Expr) -> CallArg {
        CallArg { name: None, value }
    }
}

/// One name a destructuring binding introduces.
///
/// Shaped like the head of a [`StmtKind::Let`] and for the same reasons: the
/// span is where the name was written, so a report about one element of the
/// pattern underlines that element, and the slot is filled in by the resolver.
#[derive(Clone, Debug, PartialEq)]
pub struct Destructured {
    pub name: String,
    pub span: Span,
    pub slot: Option<Slot>,
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
        /// Where the name was written.
        ///
        /// Not derivable from the statement's span, which starts at whichever
        /// word came first — `let`, `final`, `const`, or a visibility in front
        /// of one — so anything wanting to point at the *name* has to be told.
        /// An inlay hint goes immediately after it.
        name_span: Span,
        value: Expr,
        /// Whether the declaration wrote no `= value` and this is the one the
        /// language supplied — `nil` for an unannotated binding, and a call to
        /// the annotated type's zero-argument constructor for the rest.
        ///
        /// Synthesized at the parser so that nothing downstream has an
        /// `Option<Expr>` to unwrap, and marked so that the resolver can refuse
        /// a type with no default to supply. v0.8 §3.4.
        defaulted: bool,
        bind: BindKind,
        /// What the name holds, if the declaration said.
        ty: Option<TypeExpr>,
        /// Whether an importing module sees this name. Meaningful only at the
        /// top level — the parser refuses a visibility word on a binding inside
        /// a function, where there is no importer to hide it from.
        visibility: Visibility,
        /// Where the binding goes. Always `Local { hops: 0, .. }` or `Global`.
        slot: Option<Slot>,
        /// The `##` block written above it. A binding has no parameters and no
        /// return, so this carries a summary and the parser refuses the rest.
        doc: Option<Doc>,
    },
    /// `let (lat, lon, label) = point`, and `let (head, ...tail) = t` — v0.9
    /// §3.5.
    ///
    /// A statement of its own rather than a [`StmtKind::Let`] whose name is a
    /// pattern, because the two differ in the thing everything downstream cares
    /// about: how many names are bound. A `Let` reserves one slot and carries
    /// one annotation; this reserves several and carries none, and folding them
    /// would give every reader of a binding an `Option<Vec<…>>` to unwrap.
    ///
    /// §3.5 calls it a binding form, and it is one: [`BindKind`] means here
    /// exactly what it means on a `let`, and applies to every name at once.
    Destructure {
        /// The names written before the `...`, in order, one per element.
        names: Vec<Destructured>,
        /// The `...tail`, when one was written, holding however many elements
        /// are left over — as a tuple, since that is what was taken apart.
        ///
        /// An `Option` beside the list rather than a flag on its last entry, so
        /// that "one rest name, at the end" is the only shape this can be in.
        rest: Option<Destructured>,
        value: Expr,
        bind: BindKind,
        /// Whether an importing module sees these names, as for a `let`.
        visibility: Visibility,
    },
    /// Shared so a closure can hold the body without deep-copying it each time
    /// the declaration is executed.
    Fn {
        decl: Rc<FnDecl>,
        /// Where the function's own name is bound, as for `Let`.
        slot: Option<Slot>,
        /// Whether this declaration *joins* what is already bound under the
        /// name rather than replacing it — v0.8 §3.5.
        ///
        /// Set by the resolver for the second and later `fn` of a name in one
        /// scope, and only there. Deciding it statically is what keeps the REPL
        /// honest: a second entry redefining `f` is a fresh compilation and a
        /// fresh scope, so it replaces, where two `fn f` in one file overload.
        overload: bool,
    },
    /// A class declaration. The methods are ordinary functions whose first
    /// parameter is the receiver, so nothing about calling one is special.
    Class {
        name: String,
        /// The type parameters the header declared — `class Stack[T]`'s `T`,
        /// in the order written. Empty for a class taking none, which is every
        /// class written before v0.9.
        ///
        /// On the statement rather than inside the body, because they qualify
        /// the *name*: `Stack` alone is not a type once this is non-empty, and
        /// what a use of it has to supply is read off here.
        params: Vec<TypeParam>,
        /// The class this one extends, resolved in the enclosing scope like any
        /// other name — a superclass is an ordinary value, not a static label.
        parent: Option<Var>,
        /// Where the parent was named, kept for the same reason `Extend` keeps
        /// `target_span`: the statement's own span covers the whole body, and a
        /// report about the *parent* should underline the word naming it rather
        /// than the twenty lines that follow. [`Var`] carries no span of its own.
        parent_span: Option<Span>,
        methods: Vec<Rc<FnDecl>>,
        /// The fields declared in the body, in the order written — which is the
        /// order they are initialized in when an instance is built.
        ///
        /// A class may still gain a field its body never named, by an `op init`
        /// assigning one. Declaring is what lets a field carry a [`Visibility`];
        /// it is not what brings the field into existence.
        fields: Vec<FieldDecl>,
        /// Which of the two doors the declaration closed, if either.
        openness: Openness,
        /// How far the class's own name reaches — whether an importing module
        /// sees it. Nothing to do with the visibility of what is inside it.
        visibility: Visibility,
        /// Where the class's own name is bound, as for `Let`.
        slot: Option<Slot>,
        /// The `##` block written above it. A summary only, as for a binding —
        /// a class takes no arguments; its `op init` does, and documents them.
        doc: Option<Doc>,
    },
    /// `alias ScoreTable = dict[string, int]`.
    ///
    /// A resolution-time substitution that introduces no type: the alias and
    /// what it abbreviates are one type, `is` cannot tell them apart, and a
    /// report prints whichever the program wrote. It has no run-time existence
    /// at all, which is why the evaluator does nothing with it.
    Alias {
        name: String,
        /// Where the name was written, for a report about the declaration.
        name_span: Span,
        /// `alias Pair[T] = tuple[T, T]`'s `T` — v0.9 §3.7. Empty for the
        /// unparameterised alias v0.7 gave, which is every alias written
        /// before this milestone.
        ///
        /// Only [`ParamKind::Type`] with no bound reaches here; the parser
        /// refuses the other forms, because an alias is substituted away
        /// before anything could check a bound or read a value.
        params: Vec<TypeParam>,
        ty: TypeExpr,
        /// Whether an importing module sees it.
        visibility: Visibility,
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
        /// `extend list[int]`'s `list[int]` — v0.9 §3.6. `None` when no
        /// brackets were written, which is every `extend` before this
        /// milestone and every one that means "on all of them".
        ///
        /// The whole type and not just the arguments, so that the resolver can
        /// hand it to the same arity and bound checks an annotation goes
        /// through, and so that a report can quote it back as written. Its head
        /// is `target`'s name by construction — the parser reads one word and
        /// builds both.
        constraint: Option<TypeExpr>,
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

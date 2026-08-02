//! The walk that fills a [`Types`](super::Types).
//!
//! One pass over the tree, in two phases: [`Infer::declare`] collects every
//! class and function before any body is looked at — so a call to a function
//! declared further down still has an answer — and then each body is walked.
//!
//! Nothing here returns a `Result`. A type that cannot be worked out is the
//! ordinary condition of a dynamically typed program, and `Type::Unknown` is the
//! answer for it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::builtins::stdlib;
use crate::sema::infer::{Binding, ClassInfo, FILE, lookup, module_native};
use crate::sema::symbols::{Kind, Symbol, described_by, module_symbols, symbol_for};
use crate::sema::types::{
    ClassType, Type, binary, builtin_ancestor, builtin_constructor, module_member, returned_by, stated,
};
use crate::syntax::ast::{
    Block, Expr, ExprKind, FnDecl, ImportNames, LogicalOp, SELF, Stmt, StmtKind, UnaryOp,
};
use crate::syntax::token::Span;


#[derive(Default)]
pub(crate) struct Infer {
    pub(crate) classes: HashMap<String, ClassInfo>,
    pub(crate) functions: HashMap<String, Rc<FnDecl>>,
    /// Return types worked out so far, keyed by the identity of the
    /// declaration rather than by its name.
    ///
    /// Two methods in two classes are both called `at`, and a function may be
    /// shadowed by a local — a name is not a key. The `Rc` a declaration is
    /// held behind is.
    pub(crate) returns: HashMap<usize, Type>,
    /// Declarations whose return type is being worked out right now.
    ///
    /// `fn down(n) { return down(n - 1) }` is a program, and a pass that
    /// followed it would not stop. Meeting a declaration already in here
    /// answers `Unknown`, which is also the true answer: the recursive arm
    /// carries no information about the type.
    pub(crate) computing: HashSet<usize>,
    /// Classes whose fields are being worked out right now, for the same reason
    /// and against the same shape: `self.next = Node()` inside `Node`.
    pub(crate) computing_fields: HashSet<String>,
    pub(crate) fields: HashMap<String, HashMap<String, Type>>,
    pub(crate) function_returns: HashMap<String, Type>,
    pub(crate) method_returns: HashMap<(String, String), Type>,
    pub(crate) exprs: HashMap<u32, Type>,
    pub(crate) bindings: Vec<Binding>,
    /// Enclosing block spans, innermost last.
    pub(crate) scopes: Vec<Span>,
    /// The classes whose bodies are being walked, innermost last, so `self` has
    /// something to mean.
    pub(crate) receivers: Vec<String>,
}

impl Infer {
    /// Finds every class and every function first, so a call to one declared
    /// further down is not a call to nothing.
    ///
    /// The same reason the resolver has a `declare_all`: a Quince file is not
    /// read top to bottom by the program that runs it, and a pass that pretended
    /// otherwise would answer `Unknown` for exactly the forward references the
    /// language went out of its way to allow.
    pub(crate) fn declare(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Fn { decl, .. } => {
                    self.functions.insert(decl.name.clone(), Rc::clone(decl));
                    self.declare(&decl.body.stmts);
                }
                StmtKind::Class {
                    name,
                    parent,
                    methods,
                    fields,
                    ..
                } => {
                    let info = ClassInfo {
                        parent: parent.as_ref().map(|var| var.name.clone()),
                        methods: methods
                            .iter()
                            .map(|decl| (decl.name.clone(), Rc::clone(decl)))
                            .collect(),
                        fields: fields
                            .iter()
                            .map(|field| (field.name.clone(), field.visibility))
                            .collect(),
                        span: stmt.span,
                    };
                    self.classes.insert(name.clone(), info);
                }
                // An extension adds to a class that already exists, so its
                // methods join that entry. `extend list` makes an entry with no
                // parent, which is not a claim that the program declared
                // `list` — the builtin's own methods are still found through
                // `builtin_ancestor`, and this only records what was added
                // beside them.
                StmtKind::Extend { target, methods, .. } => {
                    // An `extend` block is not a declaration, so its span is not
                    // the class's — an entry made here covers the extension, and
                    // one made by a real `class` above keeps its own.
                    let info = self
                        .classes
                        .entry(target.name.clone())
                        .or_insert_with(|| ClassInfo {
                            parent: None,
                            methods: HashMap::new(),
                            fields: HashMap::new(),
                            span: stmt.span,
                        });
                    for decl in methods {
                        info.methods.insert(decl.name.clone(), Rc::clone(decl));
                    }
                }
                StmtKind::If { then, otherwise, .. } => {
                    self.declare(&then.stmts);
                    if let Some(other) = otherwise {
                        self.declare(std::slice::from_ref(other.as_ref()));
                    }
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                    self.declare(&body.stmts)
                }
                StmtKind::Try { body, handler, .. } => {
                    self.declare(&body.stmts);
                    self.declare(&handler.stmts);
                }
                StmtKind::Block(block) => self.declare(&block.stmts),
                _ => {}
            }
        }
    }

    /// Works out what every class stores in its fields.
    ///
    /// Before the main walk rather than during it, because `p.x` may be written
    /// above the class that gives `x` a type — and a field whose type depended
    /// on where in the file it was asked about would be a worse answer than no
    /// answer at all. Sorted so that two classes whose fields refer to each
    /// other resolve the same way on every run rather than by hash order.
    pub(crate) fn fields(&mut self) {
        let mut names: Vec<String> = self.classes.keys().cloned().collect();
        names.sort();
        for name in names {
            self.fields_of(&name);
        }
    }

    /// Everything the class's own methods assign to `self`.
    ///
    /// Joined across every assignment, so a field set to an int in `init` and
    /// to a string in a setter is `Unknown` — which is what it is. Only the
    /// class's own methods are read; an inherited field is found by walking up
    /// afterwards, so a subclass does not silently claim its parent's.
    fn fields_of(&mut self, class: &str) {
        if self.fields.contains_key(class) {
            return;
        }
        if !self.computing_fields.insert(class.to_string()) {
            // Reached from inside its own computation. Leaving no entry behind
            // is what makes that `Unknown` rather than a loop.
            return;
        }
        let methods: Vec<Rc<FnDecl>> = match self.classes.get(class) {
            Some(info) => info.methods.values().cloned().collect(),
            None => Vec::new(),
        };

        // The declared fields first, so one an `init` also assigns to is joined
        // against its declaration rather than replacing it.
        let mut found: HashMap<String, Type> = match self.classes.get(class) {
            Some(info) => info
                .fields
                .keys()
                .map(|name| (name.clone(), Type::Unknown))
                .collect(),
            None => HashMap::new(),
        };
        self.receivers.push(class.to_string());
        for decl in methods {
            self.scopes.push(decl.body.span);
            for param in &decl.params {
                let ty = match (param.receiver, &param.ty) {
                    (true, _) => Type::class(class),
                    (false, Some(stated_as)) => stated(stated_as),
                    (false, None) => Type::Unknown,
                };
                self.bind(&param.name, Kind::Parameter, ty, decl.body.span.start);
            }
            self.assignments_to_self(&decl.body.stmts, &mut found);
            self.scopes.pop();
        }
        self.receivers.pop();

        self.computing_fields.remove(class);
        self.fields.insert(class.to_string(), found);
    }

    /// Collects `self.name = value` out of a statement list, into `found`.
    fn assignments_to_self(&mut self, stmts: &[Stmt], found: &mut HashMap<String, Type>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Expr(expr) | StmtKind::Throw(expr) | StmtKind::Return(Some(expr)) => {
                    self.assignments_in(expr, found);
                }
                StmtKind::Let { value, .. } => self.assignments_in(value, found),
                StmtKind::If { cond, then, otherwise } => {
                    self.assignments_in(cond, found);
                    self.assignments_to_self(&then.stmts, found);
                    if let Some(other) = otherwise {
                        self.assignments_to_self(std::slice::from_ref(other.as_ref()), found);
                    }
                }
                StmtKind::While { cond, body } => {
                    self.assignments_in(cond, found);
                    self.assignments_to_self(&body.stmts, found);
                }
                StmtKind::For { iter, body, .. } => {
                    self.assignments_in(iter, found);
                    self.assignments_to_self(&body.stmts, found);
                }
                StmtKind::Try { body, handler, .. } => {
                    self.assignments_to_self(&body.stmts, found);
                    self.assignments_to_self(&handler.stmts, found);
                }
                StmtKind::Block(block) => self.assignments_to_self(&block.stmts, found),
                _ => {}
            }
        }
    }

    fn assignments_in(&mut self, expr: &Expr, found: &mut HashMap<String, Type>) {
        if let ExprKind::Assign { target, value } = &expr.kind
            && let ExprKind::Field { target: receiver, name, .. } = &target.kind
            && matches!(&receiver.kind, ExprKind::Var(var) if var.name == SELF)
        {
            let ty = self.expr(value);
            let ty = match found.remove(name) {
                Some(previous) => previous.join(ty),
                None => ty,
            };
            found.insert(name.clone(), ty);
        }
        for child in children(expr) {
            self.assignments_in(child, found);
        }
    }

    pub(crate) fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Throw(expr) => {
                self.expr(expr);
            }
            StmtKind::Let { name, value, bind, ty, doc, .. } => {
                // The initializer is walked either way, so every expression in
                // it has a recorded type — a hover inside one, and the static
                // check comparing it against the annotation, both need that.
                // Walking only when unannotated left the right-hand side of
                // `let x: int = "s"` invisible to the pass.
                let found = self.expr(value);
                // What the program said beats what the initializer looks like:
                // `let x: float = 3` stores a float, so a pass reporting `int`
                // would be describing a value that never existed.
                let ty = match ty {
                    Some(stated_as) => stated(stated_as),
                    None => found,
                };
                let symbol = Symbol::new(name, Kind::Variable, ty)
                    .declared_with(bind.word())
                    .with_doc(doc.clone());
                self.bind_symbol(symbol, stmt.span.start);
            }
            StmtKind::Fn { decl, .. } => {
                let scope = self.scope().start;
                let returns = self.function(decl, None);
                self.function_returns
                    .insert(decl.name.clone(), returns.clone());
                self.bind_symbol(symbol_for(decl, Kind::Function, returns), scope);
            }
            StmtKind::Class { name, methods, fields, doc, .. } => {
                // The name holds the class object, not an instance of it —
                // `Point` is a `class` and `Point()` is a `Point`. Saying
                // otherwise would make every mention of a class name look like
                // a value of it, which is the mistake the capital-letter
                // heuristic makes.
                let scope = self.scope().start;
                let symbol = Symbol::new(name, Kind::Class, Type::class("class"))
                    .returning(Type::class(name.clone()))
                    .with_doc(doc.clone());
                self.bind_symbol(symbol, scope);
                // A field's initializer is ordinary code and needs a type
                // recorded for it, the same as a binding's — a hover inside one
                // and the static check both read it.
                for field in fields {
                    self.expr(&field.value);
                }
                self.methods(name, methods);
            }
            StmtKind::Extend { target, methods, .. } => self.methods(&target.name, methods),
            // Substituted away before this pass runs, so there is no name here
            // to record and nothing that could still refer to one.
            StmtKind::Alias { .. } => {}

            StmtKind::Import { module, names, .. } => {
                let known = stdlib::module_named(module).is_some();
                match names {
                    ImportNames::Module => {
                        let ty = if known {
                            Type::module(module)
                        } else {
                            Type::Unknown
                        };
                        self.bind(module, Kind::Module, ty, stmt.span.start);
                    }
                    ImportNames::Names(names) => {
                        // The member's own symbol, so `from math import floor`
                        // leaves `floor` with the documentation and parameters
                        // `math.floor` has. Binding only its type would have
                        // made the shorter spelling the worse one.
                        let members = module_symbols(module);
                        for name in names {
                            match members.iter().find(|symbol| symbol.name == name.name) {
                                Some(symbol) if known => {
                                    self.bind_symbol(symbol.clone(), stmt.span.start)
                                }
                                _ => self.bind(
                                    &name.name,
                                    Kind::Variable,
                                    Type::Unknown,
                                    stmt.span.start,
                                ),
                            }
                        }
                    }
                }
            }
            StmtKind::If { cond, then, otherwise } => {
                self.expr(cond);
                // The smart cast. `if val is string { … }` re-binds `val` for
                // the block, narrowed — and narrowing *is* a re-binding, because
                // the lookup already prefers the innermost scope covering an
                // offset. Nothing new had to be invented to scope it.
                //
                // Only the `then` branch. The `else` branch knows the test
                // failed, which narrows nothing this pass can express: `not a
                // string` is a type the language cannot write down.
                self.scopes.push(then.span);
                if let Some((name, ty)) = self.narrowed(cond) {
                    let kind = lookup(&self.bindings, &name, then.span.start)
                        .map_or(Kind::Variable, |index| self.bindings[index].symbol.kind);
                    self.bind(&name, kind, ty, then.span.start);
                }
                self.stmts(&then.stmts);
                self.scopes.pop();
                if let Some(other) = otherwise {
                    self.stmt(other);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For { var, iter, body, .. } => {
                let element = self.element_of(iter);
                self.scopes.push(body.span);
                self.bind(var, Kind::Variable, element, body.span.start);
                self.stmts(&body.stmts);
                self.scopes.pop();
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            StmtKind::Try { body, binding, handler, .. } => {
                self.block(body);
                self.scopes.push(handler.span);
                // The caught value is an instance of some error class, and
                // which one is the throw's business rather than the catch's.
                // `Error` would be a guess that reads as knowledge.
                self.bind(binding, Kind::Variable, Type::Unknown, handler.span.start);
                self.stmts(&handler.stmts);
                self.scopes.pop();
            }
            StmtKind::Block(block) => self.block(block),
        }
    }

    /// Walks the methods a `class` or an `extend` block declares.
    fn methods(&mut self, class: &str, methods: &[Rc<FnDecl>]) {
        self.receivers.push(class.to_string());
        for decl in methods {
            let ty = self.function(decl, Some(class.to_string()));
            self.method_returns
                .insert((class.to_string(), decl.name.clone()), ty);
        }
        self.receivers.pop();
    }

    fn block(&mut self, block: &Block) {
        self.scopes.push(block.span);
        self.stmts(&block.stmts);
        self.scopes.pop();
    }

    /// Walks a function body, binding its parameters, and answers with what it
    /// returns.
    fn function(&mut self, decl: &Rc<FnDecl>, class: Option<String>) -> Type {
        self.scopes.push(decl.body.span);
        for param in &decl.params {
            // A parameter carries no information at all until v0.7 lets one be
            // annotated — except the receiver, which is the one parameter the
            // language itself filled in.
            let ty = match (param.receiver, &class, &param.ty) {
                (true, Some(class), _) => Type::class(class.clone()),
                // What the declaration said, which is the whole point of it
                // being written: a parameter was `Unknown` until v0.7 because
                // it is whatever the caller passed, and an annotation is the
                // caller being told what that has to be.
                (false, _, Some(stated_as)) => stated(stated_as),
                _ => Type::Unknown,
            };
            let symbol = Symbol::new(&param.name, Kind::Parameter, ty)
                .with_doc(described_by(decl, &param.name));
            self.bind_symbol(symbol, decl.body.span.start);
        }
        // `super` is a name in a method of a class that extends one, bound to
        // the parent — the resolver puts it in a scope wrapped around the
        // methods, and this is the same fact said where a lookup can find it.
        if let Some(parent) = class.as_ref().and_then(|class| self.classes.get(class))
            .and_then(|info| info.parent.clone())
        {
            self.bind(
                crate::syntax::ast::SUPER,
                Kind::Variable,
                Type::class(parent),
                decl.body.span.start,
            );
        }
        self.stmts(&decl.body.stmts);
        self.scopes.pop();
        self.return_of(decl)
    }

    /// What calling this declaration produces.
    ///
    /// The join of every `return` in its body, with a body that returns nothing
    /// anywhere answering `nil`. A body with *some* returns and a path that
    /// falls off the end is taken at its word rather than joined with the `nil`
    /// that path produces — the alternative is a flow analysis, and this pass
    /// is the floor such a thing would stand on rather than a first draft of it.
    fn return_of(&mut self, decl: &Rc<FnDecl>) -> Type {
        let key = Rc::as_ptr(decl) as usize;
        if let Some(ty) = self.returns.get(&key) {
            return ty.clone();
        }
        if !self.computing.insert(key) {
            return Type::Unknown;
        }
        // A declared return needs no walk: the language enforces it at run
        // time, so what the body does cannot disagree with what it says.
        let ty = match &decl.returns {
            Some(stated_as) => stated(stated_as),
            None => {
                let mut returns: Option<Type> = None;
                self.returned(&decl.body.stmts, &mut returns);
                returns.unwrap_or_else(|| Type::class("nil"))
            }
        };
        self.computing.remove(&key);
        self.returns.insert(key, ty.clone());
        ty
    }

    /// Joins every `return` in a body, without descending into functions
    /// declared inside it — those return for themselves.
    fn returned(&mut self, stmts: &[Stmt], found: &mut Option<Type>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Return(value) => {
                    let ty = match value {
                        Some(value) => self.expr(value),
                        // `return` with nothing after it hands back `nil`, so
                        // it joins like any other answer rather than being
                        // skipped: a function returning a `Point` on one path
                        // and bare-`return`ing on another does not return a
                        // `Point`.
                        None => Type::class("nil"),
                    };
                    *found = Some(match found.take() {
                        Some(previous) => previous.join(ty),
                        None => ty,
                    });
                }
                StmtKind::If { then, otherwise, .. } => {
                    self.returned(&then.stmts, found);
                    if let Some(other) = otherwise {
                        self.returned(std::slice::from_ref(other.as_ref()), found);
                    }
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                    self.returned(&body.stmts, found)
                }
                StmtKind::Try { body, handler, .. } => {
                    self.returned(&body.stmts, found);
                    self.returned(&handler.stmts, found);
                }
                StmtKind::Block(block) => self.returned(&block.stmts, found),
                _ => {}
            }
        }
    }

    /// What a `for` loop's variable holds.
    ///
    /// A list literal is read for its items, which is the one case where the
    /// element type is written down in the file. Iterating a string yields
    /// strings. Anything else — a list held in a variable, a class with `op
    /// iter` — is `Unknown`, because a list is not a `list[T]` until v0.7 says
    /// so.
    fn element_of(&mut self, iter: &Expr) -> Type {
        let ty = self.expr(iter);
        if let ExprKind::List(items) = &iter.kind {
            let mut joined: Option<Type> = None;
            for item in items {
                let item = self.of(item);
                joined = Some(match joined.take() {
                    Some(previous) => previous.join(item),
                    None => item,
                });
            }
            return joined.unwrap_or_default();
        }
        match ty.class_name() {
            Some("string") => Type::class("string"),
            _ => Type::Unknown,
        }
    }

    /// Records `name` in the innermost scope, visible from `from` onward.
    /// What a condition proves about a name, if it proves anything.
    ///
    /// `val is string` narrows `val`, and so does the left of an `&&` — `if x is
    /// string && len(x) > 0` is the form that makes the guard worth writing, and
    /// it would be a strange rule that narrowed the first and not the second.
    ///
    /// Only a bare name is narrowed. `user.name is string` proves something too,
    /// but a field is not a binding this pass can shadow — and a narrowing that
    /// survived an intervening assignment to `user` would be worse than none.
    fn narrowed(&mut self, cond: &Expr) -> Option<(String, Type)> {
        match &cond.kind {
            ExprKind::Is { value, ty } => match &value.kind {
                ExprKind::Var(var) => Some((var.name.clone(), stated(ty))),
                _ => None,
            },
            ExprKind::Logical {
                op: LogicalOp::And,
                lhs,
                ..
            } => self.narrowed(lhs),
            _ => None,
        }
    }

    fn bind(&mut self, name: &str, kind: Kind, ty: Type, from: u32) {
        self.bind_symbol(Symbol::new(name, kind, ty), from);
    }

    fn bind_symbol(&mut self, symbol: Symbol, from: u32) {
        let scope = self.scope();
        self.bindings.push(Binding {
            symbol,
            scope,
            from,
        });
    }

    fn scope(&self) -> Span {
        self.scopes.last().copied().unwrap_or(FILE)
    }

    /// What an expression already walked evaluates to.
    fn of(&self, expr: &Expr) -> Type {
        self.exprs.get(&expr.span.start).cloned().unwrap_or_default()
    }

    /// Walks an expression, records what it evaluates to, and answers with it.
    fn expr(&mut self, expr: &Expr) -> Type {
        let ty = self.decide(expr);
        self.exprs.insert(expr.span.start, ty.clone());
        ty
    }

    fn decide(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            ExprKind::Int(_) => Type::class("int"),
            ExprKind::Float(_) => Type::class("float"),
            ExprKind::Str(_) => Type::class("string"),
            ExprKind::Bool(_) => Type::class("bool"),
            ExprKind::Nil => Type::class("nil"),

            // `is` answers a bool whatever it was asked about.
            ExprKind::Is { .. } => Type::class("bool"),
            // A chain containing a `?.` produces what it produces, or `nil` —
            // so the honest answer is the chain's type made nullable.
            ExprKind::Chain(inner) => self.expr(inner).nullable(),
            // The left side without its `nil`, joined with the right. Both
            // arms can be reached, so agreeing is what makes an answer.
            ExprKind::Coalesce { lhs, rhs } => {
                let fallback = self.expr(rhs);
                match self.expr(lhs) {
                    Type::Class(class) => Type::Class(ClassType {
                        nullable: false,
                        ..class
                    })
                    .join(fallback),
                    // `Unknown` on the left says nothing about what survives it.
                    _ => Type::Unknown,
                }
            }

            // A literal's elements are right there, so the element type is
            // decidable and worth deciding: without it `let xs: list[int] =
            // ["a"]` looks like agreement to anything comparing types, and the
            // mistake is visible in the source.
            //
            // Elements that disagree answer with the bare `list`, not
            // `list[_]`. "A list of something I cannot name" is the true
            // statement; an `Unknown` argument would compare unequal to every
            // annotation and turn an absence of knowledge into a contradiction.
            // An empty literal says nothing for the same reason.
            ExprKind::List(items) => match joined(items.iter().map(|item| self.expr(item))) {
                Some(element) => Type::generic("list", vec![element]),
                None => Type::class("list"),
            },
            ExprKind::Dict(pairs) => {
                let keys = joined(pairs.iter().map(|(key, _)| self.expr(key)));
                let values = joined(pairs.iter().map(|(_, value)| self.expr(value)));
                match (keys, values) {
                    (Some(key), Some(value)) => Type::generic("dict", vec![key, value]),
                    _ => Type::class("dict"),
                }
            }

            ExprKind::Var(var) => lookup(&self.bindings, &var.name, expr.span.start)
                .map(|index| self.bindings[index].symbol.ty.clone())
                .unwrap_or_default(),

            ExprKind::Unary { op, rhs } => {
                let rhs = self.expr(rhs);
                match op {
                    // `!x` asks for truthiness, and truthiness is a bool
                    // whatever the class of `x` had to say about it.
                    UnaryOp::Not => Type::class("bool"),
                    // `~x` reaches `op bit_not`, which may answer with anything
                    // — so only the int, where the language decides, is known.
                    UnaryOp::BitNot => match rhs.class_name() {
                        Some("int") => Type::class("int"),
                        _ => Type::Unknown,
                    },
                    // `-x` reaches `op neg` first, and whatever that answers is
                    // the answer — so only the numbers, where the language
                    // decides, are decidable here.
                    UnaryOp::Neg => match rhs.class_name() {
                        Some("int") => Type::class("int"),
                        Some("float") => Type::class("float"),
                        _ => Type::Unknown,
                    },
                }
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                binary(*op, &lhs, &rhs)
            }

            // Both operands are answers: `a || b` hands back the operand rather
            // than a bool, so that it reads as a default. Two operands of one
            // class make one; anything else is two answers and so is neither.
            ExprKind::Logical { lhs, rhs, .. } => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                lhs.join(rhs)
            }

            ExprKind::Call { callee, args } => {
                for arg in args {
                    self.expr(arg);
                }
                self.call(callee)
            }

            // Indexing a string gives a string. A list holds whatever was put
            // in it and a dict holds whatever was mapped to, so neither says.
            ExprKind::Index { target, index } => {
                let target = self.expr(target);
                self.expr(index);
                match target.class_name() {
                    Some("string") => Type::class("string"),
                    _ => Type::Unknown,
                }
            }

            // A slice of a sequence is a sequence of the same kind, which is
            // more than indexing gives: `xs[1:]` is a list even when nothing is
            // known about what is in it.
            ExprKind::Slice { target, start, end } => {
                let target = self.expr(target);
                for bound in [start, end].into_iter().flatten() {
                    self.expr(bound);
                }
                match target.class_name() {
                    Some("string") => Type::class("string"),
                    Some("list") => Type::class("list"),
                    _ => Type::Unknown,
                }
            }

            ExprKind::Field { target, name, .. } => {
                let target = self.expr(target);
                self.read(&target, name)
            }

            ExprKind::Super { name, .. } => match self.parent() {
                Some(parent) => self.read(&Type::class(parent), name),
                None => Type::Unknown,
            },

            ExprKind::Assign { target, value } => {
                let ty = self.expr(value);
                self.expr(target);
                // A name assigned a second type holds neither from here on.
                // Recorded against the binding rather than against this
                // expression, because it is the *name* the answer changes for.
                if let ExprKind::Var(var) = &target.kind {
                    self.reassign(&var.name, target.span.start, &ty);
                }
                ty
            }
        }
    }

    /// Narrows a binding to what it holds once it has been assigned again.
    fn reassign(&mut self, name: &str, offset: u32, ty: &Type) {
        if let Some(index) = lookup(&self.bindings, name, offset) {
            let previous = std::mem::take(&mut self.bindings[index].symbol.ty);
            self.bindings[index].symbol.ty = previous.join(ty.clone());
        }
    }

    /// The class whose body is being walked.
    fn receiver(&self) -> Option<String> {
        self.receivers.last().cloned()
    }

    /// The class that one extends.
    fn parent(&self) -> Option<String> {
        let class = self.receiver()?;
        self.classes.get(&class)?.parent.clone()
    }

    /// A field or method read off a value without calling it: `p.x`, `math.pi`.
    ///
    /// A method named but not called is a `function` — the value is the method,
    /// not what it would produce — which is why this and [`Self::call`] answer
    /// differently for the same two names.
    fn read(&mut self, target: &Type, name: &str) -> Type {
        match target {
            Type::Class(class) => {
                let class = &*class.name;
                if self.method(class, name).is_some()
                    || builtin_ancestor(&self.classes, class, name).is_some()
                {
                    return Type::class("function");
                }
                self.field(class, name)
            }
            Type::Module(module) => module_member(module, name),
            Type::Unknown => Type::Unknown,
        }
    }

    /// What `class` stores under `name`, working its fields out on demand and
    /// searching its parents.
    fn field(&mut self, class: &str, name: &str) -> Type {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return Type::Unknown;
            }
            self.fields_of(&current);
            if let Some(ty) = self.fields.get(&current).and_then(|fields| fields.get(name)) {
                return ty.clone();
            }
            match self.classes.get(&current).and_then(|info| info.parent.clone()) {
                Some(parent) => current = parent,
                None => return Type::Unknown,
            }
        }
    }

    /// The declaration of `class`'s method called `name`, searching parents.
    fn method(&self, class: &str, name: &str) -> Option<Rc<FnDecl>> {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            let info = self.classes.get(&current)?;
            if let Some(decl) = info.methods.get(name) {
                return Some(Rc::clone(decl));
            }
            current = info.parent.clone()?;
        }
    }

    /// What calling `callee` produces.
    fn call(&mut self, callee: &Expr) -> Type {
        match &callee.kind {
            ExprKind::Var(var) => {
                self.expr(callee);
                // A class name in call position makes one of that class. This
                // is the one place a bare name means an instance, and it is
                // decided by the declaration rather than by the spelling —
                // `point()` builds a `point` if that is what was declared.
                if self.classes.contains_key(&var.name) {
                    return Type::class(var.name.clone());
                }
                if let Some(builtin) = builtin_constructor(&var.name) {
                    return Type::class(builtin);
                }
                match self.functions.get(&var.name).cloned() {
                    Some(decl) => self.return_of(&decl),
                    // A builtin global — `len`, `print`, `type` — which says
                    // what it hands back on the native itself.
                    None => match crate::builtins::BUILTINS
                        .iter()
                        .find(|native| native.name == var.name)
                    {
                        Some(native) => returned_by(native),
                        None => Type::Unknown,
                    },
                }
            }

            ExprKind::Field { target, name, .. } => {
                let target = self.expr(target);
                // The callee is the method itself, and the call is what it
                // produces. Both get recorded, at their own spans.
                let member = self.read(&target, name);
                self.exprs.insert(callee.span.start, member);
                match &target {
                    // The program's own method first, so a class extending a
                    // builtin that overrides `sort` is answered by what it
                    // wrote rather than by the table it inherited from.
                    Type::Class(class) => match self.method(&class.name, name) {
                        Some(decl) => self.return_of(&decl),
                        None => builtin_ancestor(&self.classes, &class.name, name)
                            .map_or(Type::Unknown, returned_by),
                    },
                    Type::Module(module) => {
                        module_native(module, name).map_or(Type::Unknown, returned_by)
                    }
                    Type::Unknown => Type::Unknown,
                }
            }

            ExprKind::Super { name, .. } => match self.parent() {
                Some(parent) => match self.method(&parent, name) {
                    Some(decl) => self.return_of(&decl),
                    None => Type::Unknown,
                },
                None => Type::Unknown,
            },

            _ => {
                self.expr(callee);
                Type::Unknown
            }
        }
    }
}

/// Every expression written inside this one.
///
/// The one type every element agrees on, or `None` if they do not — which
/// includes there being no elements to ask.
///
/// Collected eagerly rather than short-circuiting, because each element still
/// has to be walked for its own type to be recorded even once the answer is
/// settled.
fn joined(types: impl Iterator<Item = Type>) -> Option<Type> {
    types
        .reduce(|found, next| found.join(next))
        .filter(Type::is_known)
}

/// One level, so a caller that wants the whole tree recurses. A `Vec` of borrows
/// rather than an iterator because the shapes differ enough that an iterator
/// would be a hand-written enum of six cases.
fn children(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil
        | ExprKind::Var(_)
        | ExprKind::Super { .. } => Vec::new(),
        ExprKind::Chain(inner) => vec![inner],
        ExprKind::Is { value, .. } => vec![value],
        ExprKind::Coalesce { lhs, rhs } => vec![lhs, rhs],
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Dict(pairs) => pairs.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::Unary { rhs, .. } => vec![rhs],
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::Call { callee, args } => std::iter::once(callee.as_ref()).chain(args).collect(),
        ExprKind::Index { target, index } => vec![target, index],
        ExprKind::Slice { target, start, end } => std::iter::once(target.as_ref())
            .chain([start, end].into_iter().flatten().map(|bound| bound.as_ref()))
            .collect(),
        ExprKind::Field { target, .. } => vec![target],
        ExprKind::Assign { target, value } => vec![target, value],
    }
}

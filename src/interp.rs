use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use crate::ast::Slot;
use crate::ast::{BinaryOp, Block, Expr, ExprKind, LogicalOp, Op, Stmt, StmtKind, UnaryOp, Var};
use crate::class::{BUILTINS as BUILTIN_TYPES, Builtin, Class, Instance};
use crate::dict::{Dict, Key, NotAKey};
use crate::env::{self, AssignError, Env, Globals};
use crate::error::{ERROR_KINDS, ErrorKind, QuinceError};
use crate::heap::{Heap, ObjId, Object};
use crate::token::{Span, TokenKind};
use crate::value::{BoundMethod, Function, Native, Value};

/// Guards against a runaway recursion taking the process down with a native
/// stack overflow, which a language should never expose to its users.
///
/// This is only a guarantee in combination with [`STACK_SIZE`]. On its own it
/// is a number that has to be *smaller* than what the host stack can hold, and
/// a host stack is not something a program gets to assume: 8 MiB on a typical
/// Linux main thread, 2 MiB for a spawned one, 128 KiB under musl. Run the
/// interpreter through [`with_stack`] and the pair means something.
const MAX_DEPTH: usize = 250;

/// The stack the interpreter is entitled to assume.
///
/// Sized against measurement, not taste. `MAX_DEPTH` levels of Quince recursion
/// cost under 3 MiB of native stack in a debug build — the expensive profile,
/// since release frames are smaller — so this leaves roughly five times the
/// worst case observed. The margin is deliberately fat: what a frame costs
/// moves with edits to `eval` that have nothing to do with recursion, and it
/// moved by half once already. Overshooting costs nothing that matters, as
/// thread stacks are reserved lazily; undershooting is a SIGSEGV in place of an
/// error message.
pub const STACK_SIZE: usize = 16 * 1024 * 1024;

/// Runs `f` on a thread with [`STACK_SIZE`] available.
///
/// Every entry point into the language should go through this, and it wraps the
/// whole pipeline rather than just evaluation: the parser and the resolver
/// recurse per nesting level too, and dropping a deeply nested AST recurses
/// even when nothing else does.
pub fn with_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, f)
            .expect("should be able to spawn the interpreter thread")
            .join()
            // Propagates the original panic instead of wrapping it, so a
            // failure reads the same as it would have without the thread.
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

/// The base class every error extends, defined in Quince rather than in Rust.
///
/// That is not a compromise for want of better machinery — it is what makes
/// `class MyError extends Error` work with no new machinery at all, reusing the
/// `extends` chain and the method lookup that classes already have. A subclass
/// that declares no `init` inherits this one, so `TypeError("boom")` builds a
/// perfectly ordinary instance with a `message`.
///
/// `kind` is set from `type(self)`, which is already the receiver's class name —
/// so a user's `class ParseError extends Error` that calls `super.init(message)`
/// reports `ParseError` without anything here knowing it exists.
///
/// Written in the language it defines, `op` and all: this compiles through the
/// same parser as user code, so the prelude cannot drift from what a program is
/// allowed to write.
const BASE_ERROR: &str = "\
class Error {
    op init(message) {
        self.message = message
        self.kind = type(self)
    }
}
";

/// The message field, which is for humans.
const MESSAGE: &str = "message";

/// The kind field, which is what a program should match on.
///
/// Message strings get reworded; a kind is the half that stays put, and it is
/// what a typed `catch e: TypeError` will eventually filter on.
const KIND: &str = "kind";

/// Why a statement stopped executing.
enum Flow {
    Normal,
    Return(Value),
}

/// What `x.name` found.
///
/// The distinction is only about the receiver: a method gets one prepended when
/// called, a field does not. Every other difference between them — how they are
/// printed, whether they are callable at all — is a property of the value.
enum Attr {
    Field(Value),
    Method(Value),
}

impl Attr {
    fn value(&self) -> &Value {
        match self {
            Attr::Field(value) | Attr::Method(value) => value,
        }
    }
}

pub struct Interp {
    pub heap: Heap,
    globals: ObjId,
    /// Every scope currently being executed, innermost last.
    ///
    /// A called function's scope hangs off the *closure* it came from, not off
    /// the caller, so the caller's scope is unreachable from the callee. Each
    /// active frame therefore has to be a root in its own right.
    scopes: Vec<ObjId>,
    /// Values a Rust frame is holding across a safe point, which no walk of the
    /// heap could find. See [`Interp::collect_if_needed`].
    temps: Vec<Value>,
    depth: usize,
    out: Box<dyn Write>,
    /// The class each [`ErrorKind`] reifies into, captured once at startup.
    ///
    /// Held here rather than looked up in globals at `catch` time because `Error`
    /// and its subclasses are ordinary globals, which a program is free to
    /// shadow — the same exposure `print` and `len` already have. What a handler
    /// binds must not depend on whether someone rebound the name, so the handles
    /// are taken before any user code can run.
    error_classes: Vec<(ErrorKind, ObjId)>,
}

impl Interp {
    pub fn new() -> Self {
        Interp::with_output(Box::new(std::io::stdout()))
    }

    /// Output is injected so tests can capture what a program prints.
    pub fn with_output(out: Box<dyn Write>) -> Self {
        let mut heap = Heap::new();
        let globals = heap.alloc(Object::Globals(Globals::new()));
        for native in BUILTINS {
            heap.globals_mut(globals)
                .declare(native.name, Value::Native(native), false);
        }
        // The types themselves, so a program can name one: print it, reach a
        // method through it, and in time extend it. Immutable, exactly as
        // `print` is.
        //
        // A name the lexer has already claimed is skipped rather than the two
        // cases being written out, because the reason is the lexer's and it can
        // change: `nil` and `class` are keywords, so nothing could ever read a
        // global under those names. Their class objects still exist and still
        // answer method calls — they just cannot be spelled.
        for builtin in BUILTIN_TYPES {
            let name = builtin.name();
            if TokenKind::keyword(name).is_some() {
                continue;
            }
            let class = heap.builtin_class(*builtin);
            heap.globals_mut(globals)
                .declare(name, Value::Class(class), false);
        }
        let mut interp = Interp {
            heap,
            globals,
            scopes: Vec::new(),
            temps: Vec::new(),
            depth: 0,
            out,
            error_classes: Vec::new(),
        };
        interp.install_error_classes();
        interp
    }

    /// Declares `Error` and one subclass per [`ErrorKind`], then remembers them.
    ///
    /// The subclasses are generated from [`ERROR_KINDS`] rather than spelled out,
    /// so adding a kind cannot leave its class undeclared — the failure that
    /// would otherwise wait until something raised that kind and a `catch` went
    /// looking for a global that was never bound.
    fn install_error_classes(&mut self) {
        // Taken from the enum rather than written out, so the name in
        // `BASE_ERROR` and the name subclasses extend cannot drift apart.
        let base = ErrorKind::Runtime.class_name();
        let mut source = String::from(BASE_ERROR);
        for kind in ERROR_KINDS {
            let name = kind.class_name();
            if name != base {
                // An empty body on purpose: `init` comes from `Error` through the
                // same lookup a user's subclass uses, so there is nothing to say.
                source.push_str(&format!("class {name} extends {base} {{}}\n"));
            }
        }

        let program = crate::compile(&source).expect("the error prelude should compile");
        self.run(&program)
            .expect("the error prelude only declares classes");

        // The method bodies outlive `program`: a class holds `Function` objects
        // holding `Rc<FnDecl>`, so dropping the statements leaves the ASTs alive.
        self.error_classes = ERROR_KINDS
            .iter()
            .map(|kind| {
                let name = kind.class_name();
                match self.heap.globals(self.globals).get(name) {
                    Some(Value::Class(id)) => (*kind, *id),
                    _ => unreachable!("the prelude declares `{name}` as a class"),
                }
            })
            .collect();
    }

    /// The class an error of `kind` reifies into.
    ///
    /// A linear scan over a list this short beats hashing it, and it is reached
    /// only when an error is actually caught.
    fn error_class(&self, kind: ErrorKind) -> ObjId {
        self.error_classes
            .iter()
            .find(|(against, _)| *against == kind)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| {
                // `Thrown` carries its own class and never asks; anything else
                // missing means `install_error_classes` and `ERROR_KINDS` drifted.
                panic!("no class installed for {kind:?}")
            })
    }

    pub fn run(&mut self, program: &[Stmt]) -> Result<(), QuinceError> {
        for stmt in program {
            self.exec(stmt, self.globals)?;
        }
        Ok(())
    }

    /// Evaluates a program, returning the value of a trailing expression so the
    /// REPL can echo it.
    pub fn run_repl(&mut self, program: &[Stmt]) -> Result<Option<Value>, QuinceError> {
        let mark = self.temps.len();
        let mut last = None;
        for stmt in program {
            let value = match &stmt.kind {
                StmtKind::Expr(expr) => {
                    self.collect_if_needed();
                    Some(self.eval(expr, self.globals)?)
                }
                _ => {
                    self.exec(stmt, self.globals)?;
                    None
                }
            };
            // The value waiting to be echoed lives in a Rust local across every
            // later statement, so it is rooted like any other temporary. Today
            // this cannot actually be observed — a value is only ever echoed
            // when its statement was the last one, and nothing runs after that
            // — so it is here to keep the rule "a value held across a safe
            // point is rooted" true without exception.
            self.temps.truncate(mark);
            self.temps.extend(value.clone());
            last = value;
        }
        self.temps.truncate(mark);
        Ok(last)
    }

    pub fn get_globals(&self) -> Vec<(String, Value)> {
        self.heap
            .globals(self.globals)
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    pub fn set_global(&mut self, name: impl Into<String>, value: Value) {
        self.heap
            .globals_mut(self.globals)
            .declare(name, value, true);
    }

    // -- garbage collection ------------------------------------------------

    /// Collects, if the heap has grown enough to be worth it.
    ///
    /// **Only ever call this between statements.** A tree-walking evaluator
    /// keeps live values in Rust locals — the left operand of a `+` while the
    /// right one is still being evaluated, say — and the collector cannot see
    /// the Rust stack. Between statements that set is small and explicit: the
    /// active scopes, plus the handful of frames that deliberately hold a value
    /// across a nested statement, which push it onto `temps`.
    ///
    /// The alternative, collecting inside `alloc`, would mean rooting every
    /// intermediate value in every expression. That is what a bytecode VM gets
    /// for free by keeping its operands on a stack it owns, and it is a good
    /// reason to want one.
    fn collect_if_needed(&mut self) {
        if !self.heap.should_collect() {
            return;
        }
        let mut roots = Vec::with_capacity(self.scopes.len() + self.temps.len() + 1);
        roots.push(self.globals);
        roots.extend(&self.scopes);
        roots.extend(self.temps.iter().filter_map(Value::handle));
        self.heap.collect(&roots);
    }

    // -- statements --------------------------------------------------------

    fn exec(&mut self, stmt: &Stmt, env: ObjId) -> Result<Flow, QuinceError> {
        // The one safe point in the evaluator. Every statement passes through
        // here, and nothing above it on the Rust stack holds an unrooted value.
        self.collect_if_needed();

        match &stmt.kind {
            StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(Flow::Normal)
            }

            StmtKind::Let {
                name,
                value,
                bind,
                slot,
            } => {
                let value = self.eval(value, env)?;
                // Freezing before binding, so that a `const` can never be
                // observed thawed — not that anything runs in between, but the
                // order is the thing a reader checks first.
                if bind.freezes() {
                    self.heap.freeze(&value);
                }
                self.bind(slot, name, value, bind.mutable(), env);
                Ok(Flow::Normal)
            }

            StmtKind::Fn { decl, slot } => {
                // Declared before the body ever runs, and closing over the scope
                // it is declared in, so the function can call itself.
                let func = self.heap.alloc(Object::Function(Function {
                    decl: Rc::clone(decl),
                    env,
                }));
                self.bind(slot, &decl.name, Value::Function(func), false, env);
                Ok(Flow::Normal)
            }

            StmtKind::Class {
                name,
                parent,
                methods,
                slot,
            } => {
                // Read before the class's own name is bound, so `class A extends
                // A` is an undefined variable rather than a cycle in the chain
                // that `Class::method` walks.
                let parent = match parent {
                    Some(parent) => match self.read(parent, env, stmt.span)? {
                        // A builtin with no `init` is the one parent that cannot
                        // work, and for a reason that needs no second list to
                        // record: extending a builtin means `super.init` writes
                        // the value the subclass *is*, and where there is no
                        // conversion there is nothing for it to call. That rules
                        // out `function` and `class`, exactly the two that refuse
                        // to be constructed on their own.
                        Value::Class(id)
                            if self.heap.class(id).builtin.is_some()
                                && self.heap.class(id).init.is_none() =>
                        {
                            let builtin = self.heap.class(id).name.clone();
                            return Err(QuinceError::new(
                                format!("`{name}` cannot extend `{builtin}`"),
                                stmt.span,
                            )
                            .with_kind(ErrorKind::Type)
                            .with_help(format!(
                                "there is no value a {builtin} could be made from, so `super.init` would have nothing to call"
                            )));
                        }
                        Value::Class(id) => Some(id),
                        other => {
                            return Err(QuinceError::new(
                                format!(
                                    "a class can only extend a class, but `{}` is {}",
                                    parent.name,
                                    other.type_name(&self.heap)
                                ),
                                stmt.span,
                            ));
                        }
                    },
                    None => None,
                };

                // A subclass wraps its methods in a scope holding the parent, so
                // `super` is an ordinary local wherever it appears — including
                // inside a closure nested in a method. The resolver pushed the
                // matching scope, so the hop counts already account for it.
                let enclosing = match parent {
                    Some(id) => {
                        let scope = self.heap.alloc(Object::Env(Env::new(Some(env), 1)));
                        self.heap.env_mut(scope).set(0, Value::Class(id));
                        scope
                    }
                    None => env,
                };

                // Methods close over that scope, exactly as a `fn` at the same
                // position would. Nothing here reaches a safe point, so the
                // functions are safe unrooted until the class owning them exists.
                let mut table = HashMap::with_capacity(methods.len());
                let mut own_init = None;
                for decl in methods {
                    let func = Value::Function(self.heap.alloc(Object::Function(Function {
                        decl: Rc::clone(decl),
                        env: enclosing,
                    })));
                    if decl.op == Some(Op::Init) {
                        own_init = Some(func.clone());
                    }
                    table.insert(decl.name.clone(), func);
                }

                // Inherited rather than searched for: a class that declares no
                // `op init` of its own constructs with its parent's, which is
                // what `class TypeError extends Error {}` relies on.
                let init =
                    own_init.or_else(|| parent.and_then(|id| self.heap.class(id).init.clone()));

                let class = self.heap.alloc(Object::Class(Class {
                    name: name.clone(),
                    methods: table,
                    parent,
                    init,
                    builtin: None,
                }));
                self.bind(slot, name, Value::Class(class), false, env);
                Ok(Flow::Normal)
            }

            StmtKind::If {
                cond,
                then,
                otherwise,
            } => {
                if self.eval(cond, env)?.is_truthy(&self.heap) {
                    self.exec_block(then, env)
                } else if let Some(other) = otherwise {
                    self.exec(other, env)
                } else {
                    Ok(Flow::Normal)
                }
            }

            StmtKind::While { cond, body } => {
                while self.eval(cond, env)?.is_truthy(&self.heap) {
                    if let Flow::Return(value) = self.exec_block(body, env)? {
                        return Ok(Flow::Return(value));
                    }
                }
                Ok(Flow::Normal)
            }

            StmtKind::For {
                iter, body, slot, ..
            } => self.exec_for(slot, iter, body, env),

            StmtKind::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval(expr, env)?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(value))
            }

            StmtKind::Try {
                body,
                handler,
                slot,
                ..
            } => self.exec_try(body, handler, slot, env),

            StmtKind::Throw(value) => {
                // Evaluating the operand can fail for reasons of its own, and that
                // error is the one to report — not whatever `throw` would have made
                // of a value it never got.
                let raised = self.eval(value, env)?;
                Err(self.throw(raised, stmt.span))
            }

            StmtKind::Block(block) => self.exec_block(block, env),
        }
    }

    /// Runs a `try` block, handing an error to its handler.
    ///
    /// `catch` needs no unwinding machinery of its own. Every site that pushes a
    /// scope, a temp, or a frame binds the result before it pops rather than
    /// propagating with `?`, so by the time a handler runs, `scopes`, `temps`, and
    /// `depth` are already back to their depth at the `try`. That discipline
    /// exists for the collector, and this feature is affordable because of it.
    ///
    /// What changes is not the correctness but the consequence of getting it
    /// wrong: an error used to be fatal, so a site that forgot to restore leaked
    /// roots into a process about to exit. A caught error resumes with those
    /// stacks still deep, which turns the same latent bug into unbounded growth —
    /// see `a_loop_that_catches_does_not_grow_the_heap`.
    fn exec_try(
        &mut self,
        body: &Block,
        handler: &Block,
        slot: &Option<Slot>,
        env: ObjId,
    ) -> Result<Flow, QuinceError> {
        let err = match self.exec_block(body, env) {
            // A `return` inside a `try` travels as `Flow::Return`, a value in the
            // `Ok` channel, and errors travel in the `Err` channel. It passes
            // through untouched because it is not the kind of thing a handler can
            // see. `finally` is precisely the feature that would force those two
            // channels to meet, and there deliberately is none.
            Ok(flow) => return Ok(flow),
            Err(err) => err,
        };

        let index = match resolved(slot) {
            Slot::Local { index, .. } => index,
            Slot::Global => unreachable!("a catch binding is always a local"),
        };

        // A thrown payload crossed the unwind rooted by nothing, surviving only
        // because collection happens between statements and unwinding executes
        // none. Nothing between here and the `set` below reaches a safe point —
        // `alloc` does not collect — so it is still alive to be bound. A
        // `finally` would have run statements during that unwind, and this is the
        // line where that would have become a use-after-free.
        let caught = self.reify(&err);
        let scope = self
            .heap
            .alloc(Object::Env(Env::new(Some(env), handler.slot_count)));
        self.heap.env_mut(scope).set(index, caught);
        self.exec_scoped(&handler.stmts, scope)
    }

    /// Raises `value`, which has to be an instance of `Error`.
    ///
    /// Checked here rather than at the `catch` so the error names the mistake.
    /// Allowing anything to be thrown would mean a handler binding a bare `int`,
    /// and the failure would surface as a missing field on it — a complaint about
    /// `int` methods, several lines from the `throw` that caused it, with the
    /// thrown value nowhere in the message.
    ///
    /// It also keeps a promise worth having: everything a handler binds has a
    /// `message` and a `kind`, because everything it binds extends `Error`.
    /// Returns the error to raise rather than raising it, because the caller is
    /// the one that owes an `Err` either way — evaluating the operand can fail on
    /// its own, and those two failures must not be confused for each other.
    fn throw(&mut self, raised: Value, span: Span) -> QuinceError {
        let Value::Instance(id) = raised else {
            return QuinceError::new(
                format!(
                    "`throw` needs an instance of `Error`, but was given {}",
                    raised.type_name(&self.heap)
                ),
                span,
            )
            .with_kind(ErrorKind::Type);
        };

        let class = self.heap.instance(id).class;
        if !self.descends_from_error(class) {
            return QuinceError::new(
                format!(
                    "`throw` needs an instance of `Error`, but `{}` does not extend it",
                    self.heap.class(class).name
                ),
                span,
            )
            .with_kind(ErrorKind::Type);
        }

        // Both read for the report an uncaught throw prints. The class is what it
        // reports *as*, so a `ParseError` says its own name rather than `Error`.
        //
        // A subclass that overrides `init` without calling `super.init` has no
        // `message`, so the class name stands in — a worse message, but never a
        // second error raised while reporting the first one.
        let name = self.heap.class(class).name.clone();
        let message = match self
            .heap
            .instance(id)
            .fields
            .get(&Key::Str(Rc::from(MESSAGE)))
        {
            Some(Value::Str(message)) => message.to_string(),
            Some(other) => other.display(&self.heap),
            None => name.clone(),
        };
        QuinceError::thrown(id, name, message, span)
    }

    fn exec_for(
        &mut self,
        slot: &Option<Slot>,
        iter: &Expr,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow, QuinceError> {
        let iterable = self.eval(iter, env)?;
        // A class extending `list` or `dict` iterates as one. A string is not
        // iterable to begin with — `chars` is how its characters are reached — so
        // neither is a class extending it, which is the consistent answer rather
        // than a gap.
        let items = match iterable.base(&self.heap) {
            // Snapshotted, so mutating the collection inside the loop cannot
            // invalidate the iteration.
            Value::List(id) => self.heap.list(*id).clone(),
            // A dict iterates over its keys, as in Python. Its values are the
            // half you can already reach, through `d[k]`.
            Value::Dict(id) => self.heap.dict(*id).keys().collect(),
            _ => {
                return Err(QuinceError::new(
                    format!("cannot iterate over {}", iterable.type_name(&self.heap)),
                    iter.span,
                )
                .with_kind(ErrorKind::Type));
            }
        };

        // The snapshot is the one place a Rust frame holds values across whole
        // statements. It has to be rooted: the loop body may drop the original
        // list, and then nothing on the heap refers to these items at all.
        let mark = self.temps.len();
        self.temps.extend(items.iter().cloned());
        let result = self.iterate(slot, items, body, env);
        self.temps.truncate(mark);
        result
    }

    fn iterate(
        &mut self,
        slot: &Option<Slot>,
        items: Vec<Value>,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow, QuinceError> {
        let index = match resolved(slot) {
            Slot::Local { index, .. } => index,
            Slot::Global => unreachable!("a loop variable is always a local"),
        };
        for item in items {
            // A fresh scope per iteration, so a closure made inside the loop
            // captures that iteration's value rather than sharing one binding.
            let scope = self
                .heap
                .alloc(Object::Env(Env::new(Some(env), body.slot_count)));
            self.heap.env_mut(scope).set(index, item);
            if let Flow::Return(value) = self.exec_scoped(&body.stmts, scope)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_block(&mut self, block: &Block, env: ObjId) -> Result<Flow, QuinceError> {
        let scope = self
            .heap
            .alloc(Object::Env(Env::new(Some(env), block.slot_count)));
        self.exec_scoped(&block.stmts, scope)
    }

    /// Turns an error into the value a handler binds.
    ///
    /// The one place an error becomes a Quince object, and the only place that
    /// has the `&mut Heap` to build one — which is the whole reason raising stays
    /// cheap. Half the raise sites hold `&Heap`, and an uncaught error is about
    /// to be printed and discarded, so an error that nobody catches allocates
    /// nothing at all.
    ///
    /// Allocates but reaches no safe point, so the instance is safe unrooted
    /// until the caller stores it.
    fn reify(&mut self, err: &QuinceError) -> Value {
        // A `throw` already built its instance, and the handler binds that same
        // object unchanged and unwrapped — which is what makes a user's own
        // fields survive the round trip.
        if let Some(id) = err.payload {
            return Value::Instance(id);
        }

        let class = self.error_class(err.kind);
        let mut fields = Dict::new();
        fields.insert(
            Key::Str(Rc::from(MESSAGE)),
            Value::from(err.message.as_str()),
        );
        // Set directly rather than by calling `init`, because this is the runtime
        // building the object rather than a program asking for one. The values
        // match what `Error.init` would have produced.
        fields.insert(Key::Str(Rc::from(KIND)), Value::from(err.kind.class_name()));
        Value::Instance(self.heap.alloc(Object::Instance(Instance {
            class,
            fields,
            // `Error` extends nothing, so nothing in the chain a user's own error
            // class sits on has a payload to fill.
            payload: None,
        })))
    }

    /// Whether `class` is `Error` or descends from it.
    ///
    /// Walks the same parent chain `Class::method` does, and terminates for
    /// the same reason: a parent is evaluated before the class naming it is
    /// bound, so the chain cannot contain a cycle.
    fn descends_from_error(&self, class: ObjId) -> bool {
        let base = self.error_class(ErrorKind::Runtime);
        let mut at = Some(class);
        while let Some(id) = at {
            if id == base {
                return true;
            }
            at = self.heap.class(id).parent;
        }
        false
    }

    /// Stores a freshly declared value in the slot the resolver picked for it.
    fn bind(&mut self, slot: &Option<Slot>, name: &str, value: Value, mutable: bool, env: ObjId) {
        match resolved(slot) {
            Slot::Local { index, .. } => self.heap.env_mut(env).set(index, value),
            Slot::Global => self
                .heap
                .globals_mut(self.globals)
                .declare(name, value, mutable),
        }
    }

    /// Runs `stmts` in `scope`, keeping the scope rooted for as long as it is
    /// on the Rust stack.
    ///
    /// Every scope is created and entered here, which is what makes the root
    /// set complete — a scope allocated anywhere else would be collected out
    /// from under the frame using it.
    fn exec_scoped(&mut self, stmts: &[Stmt], scope: ObjId) -> Result<Flow, QuinceError> {
        self.scopes.push(scope);
        let result = self.exec_stmts(stmts, scope);
        self.scopes.pop();
        result
    }

    fn exec_stmts(&mut self, stmts: &[Stmt], env: ObjId) -> Result<Flow, QuinceError> {
        for stmt in stmts {
            if let Flow::Return(value) = self.exec(stmt, env)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Normal)
    }

    // -- expressions -------------------------------------------------------

    /// Evaluates several expressions in order, keeping the results already
    /// computed rooted while the remaining ones run.
    ///
    /// Every sub-expression after the first can reach a safe point — one
    /// argument that calls a function is enough — and until the caller stores
    /// them, the values computed so far exist only in this Rust frame. Without
    /// this, `[mk(), churn()]` collects `mk()`'s list and hands back a handle
    /// into a reused slot.
    ///
    /// The caller gets the values back unrooted, so it must consume them
    /// without reaching another safe point. Every caller allocates or binds
    /// immediately, which is what makes that safe.
    ///
    /// Only values that carry a handle are rooted. An `int` or a `bool` is
    /// inline and cannot be collected, and a string is reference counted
    /// outside the heap — so the common arithmetic path pays a discriminant
    /// check and nothing more.
    fn eval_seq<'e>(
        &mut self,
        exprs: impl IntoIterator<Item = &'e Expr>,
        env: ObjId,
    ) -> Result<Vec<Value>, QuinceError> {
        let exprs = exprs.into_iter();
        let mark = self.temps.len();
        let mut values = Vec::with_capacity(exprs.size_hint().0);
        for expr in exprs {
            match self.eval(expr, env) {
                Ok(value) => {
                    if value.handle().is_some() {
                        self.temps.push(value.clone());
                    }
                    values.push(value);
                }
                Err(err) => {
                    self.temps.truncate(mark);
                    return Err(err);
                }
            }
        }
        self.temps.truncate(mark);
        Ok(values)
    }

    /// [`Interp::eval_seq`] for the two-operand case, which is every binary
    /// operator and every subscript.
    ///
    /// Spelled out rather than delegating, because the `Vec` that version
    /// returns would be a heap allocation on every single arithmetic operation.
    fn eval_pair(
        &mut self,
        first: &Expr,
        second: &Expr,
        env: ObjId,
    ) -> Result<(Value, Value), QuinceError> {
        let first = self.eval(first, env)?;

        // Only the second operand can reach a safe point, and only a handle can
        // be collected once it does.
        let mark = self.temps.len();
        if first.handle().is_some() {
            self.temps.push(first.clone());
        }
        let second = self.eval(second, env);
        self.temps.truncate(mark);

        Ok((first, second?))
    }

    fn eval(&mut self, expr: &Expr, env: ObjId) -> Result<Value, QuinceError> {
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(n) => Ok(Value::Float(*n)),
            ExprKind::Str(s) => Ok(Value::Str(Rc::from(s.as_str()))),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),

            ExprKind::Var(var) => self.read(var, env, expr.span),

            ExprKind::List(items) => {
                let values = self.eval_seq(items, env)?;
                Ok(Value::List(self.heap.alloc(Object::List(values))))
            }

            ExprKind::Dict(entries) => {
                let values = self.eval_seq(entries.iter().flat_map(|(k, v)| [k, v]), env)?;
                let mut dict = Dict::new();
                for pair in values.chunks_exact(2) {
                    // A repeated key overwrites, as in Python — the literal is
                    // just a run of insertions.
                    dict.insert(key_of(&self.heap, &pair[0], expr.span)?, pair[1].clone());
                }
                Ok(Value::Dict(self.heap.alloc(Object::Dict(dict))))
            }

            ExprKind::Unary { op, rhs } => {
                let value = self.eval(rhs, env)?;
                self.unary(*op, value, expr.span)
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let (lhs, rhs) = self.eval_pair(lhs, rhs, env)?;
                self.binary(*op, lhs, rhs, expr.span)
            }

            ExprKind::Logical { op, lhs, rhs } => {
                let lhs = self.eval(lhs, env)?;
                let short_circuits = match op {
                    LogicalOp::And => !lhs.is_truthy(&self.heap),
                    LogicalOp::Or => lhs.is_truthy(&self.heap),
                };
                // Returns the operand itself rather than a bool, so `a || b`
                // works as a default-value idiom.
                if short_circuits {
                    Ok(lhs)
                } else {
                    self.eval(rhs, env)
                }
            }

            ExprKind::Call { callee, args } => {
                // A method call is fused rather than evaluating the callee to a
                // bound method and then calling it. `xs.push(1)` is by far the
                // common form, and going through `ExprKind::Field` would
                // allocate an object per call only to drop it immediately.
                if let ExprKind::Field { target, name } = &callee.kind {
                    return self.eval_method_call(target, name, args, env, callee.span, expr.span);
                }
                // `super.init(name)` is the overwhelmingly common form, and it
                // is fused for the same reason: no bound method to allocate.
                if let ExprKind::Super {
                    name,
                    parent,
                    receiver,
                } = &callee.kind
                {
                    let class = self.super_class(parent, env, callee.span)?;
                    let receiver = self.read(receiver, env, callee.span)?;

                    // `super.init` where the superclass is a builtin is the one
                    // `super` call that is not a method call. A builtin's `init`
                    // is a conversion — no receiver, and deliberately not among
                    // its methods — so the lookup below would not find it and
                    // inserting a receiver would be wrong if it did.
                    if name == Op::Init.name()
                        && let Some(builtin) = self.heap.class(class).builtin
                    {
                        let mark = self.temps.len();
                        self.temps.push(receiver.clone());
                        let values = self.eval_seq(args, env);
                        self.temps.truncate(mark);
                        return self.super_init(class, builtin, &receiver, values?, expr.span);
                    }

                    let method = self.super_method(class, name, callee.span)?;
                    let mark = self.temps.len();
                    self.temps.push(receiver.clone());
                    self.temps.push(method.clone());
                    let values = self.eval_seq(args, env);
                    self.temps.truncate(mark);
                    return self.call_method(receiver, method, values?, expr.span);
                }

                let target = self.eval(callee, env)?;
                // The callee is held across every argument, any of which can
                // reach a safe point, and a closure built by an expression is
                // reachable from nowhere else. Kept out of `eval_seq` so the
                // argument vector stays exactly as long as the call needs.
                let mark = self.temps.len();
                if target.handle().is_some() {
                    self.temps.push(target.clone());
                }
                let values = self.eval_seq(args, env);
                self.temps.truncate(mark);
                self.call(target, values?, expr.span)
            }

            ExprKind::Index { target, index } => {
                let (target, index) = self.eval_pair(target, index, env)?;
                self.index_get(&target, &index, expr.span)
            }

            ExprKind::Slice { target, start, end } => {
                // Three sub-expressions, so neither `eval_pair` nor a hand-
                // rolled pair of marks fits. `eval_seq` already roots each
                // value against the ones evaluated after it, which is exactly
                // the guarantee needed here.
                let mut parts: Vec<&Expr> = Vec::with_capacity(3);
                parts.push(target);
                parts.extend(start.as_deref());
                parts.extend(end.as_deref());

                let mut values = self.eval_seq(parts, env)?.into_iter();
                let target = values.next().expect("the target is always evaluated");
                let start = start.as_ref().map(|_| values.next().expect("a bound"));
                let end = end.as_ref().map(|_| values.next().expect("a bound"));

                self.slice(&target, start.as_ref(), end.as_ref(), expr.span)
            }

            // Only reached when a method is not being called immediately — the
            // fused path above handles `x.m(…)`. Binding it makes a method an
            // ordinary value rather than syntax that works in one position.
            ExprKind::Field { target, name } => {
                let receiver = self.eval(target, env)?;
                match self.attr(&receiver, name, expr.span)? {
                    Attr::Field(value) => Ok(value),
                    // Nothing between here and the allocation is a safe point,
                    // so the receiver needs no rooting on the way in; once it
                    // is inside, `trace` keeps it alive.
                    Attr::Method(method) => Ok(Value::BoundMethod(
                        self.heap
                            .alloc(Object::BoundMethod(BoundMethod { receiver, method })),
                    )),
                }
            }

            // Only reached when the method is not called immediately. Binding it
            // to `self` is the whole point of `super`: the parent's code runs,
            // but on this object.
            ExprKind::Super {
                name,
                parent,
                receiver,
            } => {
                let class = self.super_class(parent, env, expr.span)?;
                let receiver = self.read(receiver, env, expr.span)?;
                let method = self.super_method(class, name, expr.span)?;
                Ok(Value::BoundMethod(self.heap.alloc(Object::BoundMethod(
                    BoundMethod { receiver, method },
                ))))
            }

            ExprKind::Assign { target, value } => {
                let value = self.eval(value, env)?;
                self.assign(target, value, env)
            }
        }
    }

    /// Reads a variable through the slot the resolver assigned it.
    fn read(&mut self, var: &Var, env: ObjId, span: Span) -> Result<Value, QuinceError> {
        match resolved(&var.slot) {
            Slot::Local { hops, index } => {
                let scope = env::ancestor(&self.heap, env, hops);
                self.heap.env(scope).get(index).cloned().ok_or_else(|| {
                    // Declarations are hoisted to the top of their scope, so a
                    // slot can be reached before its `let` has run.
                    QuinceError::new(
                        format!("`{}` is used before it is declared", var.name),
                        span,
                    )
                })
            }
            Slot::Global => self
                .heap
                .globals(self.globals)
                .get(&var.name)
                .cloned()
                .ok_or_else(|| {
                    QuinceError::new(format!("undefined variable `{}`", var.name), span)
                        .with_kind(ErrorKind::Name)
                }),
        }
    }

    fn assign(&mut self, target: &Expr, value: Value, env: ObjId) -> Result<Value, QuinceError> {
        match &target.kind {
            ExprKind::Var(var) => {
                match resolved(&var.slot) {
                    // The resolver already rejected assignment to a `const`
                    // local, so reaching a slot means it is writable.
                    Slot::Local { hops, index } => {
                        let scope = env::ancestor(&self.heap, env, hops);
                        self.heap.env_mut(scope).set(index, value.clone());
                    }
                    Slot::Global => {
                        let name = &var.name;
                        match self
                            .heap
                            .globals_mut(self.globals)
                            .assign(name, value.clone())
                        {
                            Ok(()) => {}
                            Err(AssignError::Undefined) => {
                                return Err(QuinceError::new(
                                    format!("undefined variable `{name}`"),
                                    target.span,
                                ));
                            }
                            Err(AssignError::Immutable) => {
                                return Err(QuinceError::new(
                                    format!("cannot reassign `{name}`"),
                                    target.span,
                                ));
                            }
                        }
                    }
                }
                Ok(value)
            }

            ExprKind::Index {
                target: collection,
                index,
            } => {
                // `value` was evaluated by the caller and is held across two
                // more evaluations, either of which can reach a safe point.
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let evaluated = self.eval_pair(collection, index, env);
                self.temps.truncate(mark);
                let (collection, index) = evaluated?;

                // Written through to the payload for a class extending `dict` or
                // `list`, so `bag['a'] = 1` reaches the dict the object *is*. The
                // `const` check still names `collection`, since freezing applies to
                // the object a program holds.
                match collection.base(&self.heap).clone() {
                    // Assigning to a missing key inserts it, where assigning
                    // past the end of a list stays an error: a list's indices
                    // are positions, and there is no meaningful gap to fill.
                    // The mutation happens inside the `map` so that the borrow
                    // it needs has ended by the time the error — which reads
                    // the heap to name the type — is built.
                    Value::Dict(id) => {
                        let key = key_of(&self.heap, &index, target.span)?;
                        let written = self
                            .heap
                            .dict_mut(id)
                            .map(|entries| entries.insert(key, value.clone()));
                        written.map_err(|_| frozen(&self.heap, &collection, target.span))?;
                    }
                    _ => {
                        let (id, offset) = self.list_index(&collection, &index, target.span)?;
                        let written = self
                            .heap
                            .list_mut(id)
                            .map(|items| items[offset] = value.clone());
                        written.map_err(|_| frozen(&self.heap, &collection, target.span))?;
                    }
                }
                Ok(value)
            }

            // Assigning to a field creates it if it is not there, which is the
            // only way an instance ever gets one — there is no declaration
            // form, so `init` assigning to `self.x` is what defines `x`.
            ExprKind::Field {
                target: object,
                name,
            } => {
                let mark = self.temps.len();
                self.temps.push(value.clone());
                let object = self.eval(object, env);
                self.temps.truncate(mark);

                match object? {
                    Value::Instance(id) => {
                        let key = Key::Str(Rc::from(name.as_str()));
                        let written = self
                            .heap
                            .instance_mut(id)
                            .map(|instance| instance.fields.insert(key, value.clone()));
                        written
                            .map_err(|_| frozen(&self.heap, &Value::Instance(id), target.span))?;
                        Ok(value)
                    }
                    other => Err(QuinceError::new(
                        format!("cannot set a field on {}", other.type_name(&self.heap)),
                        target.span,
                    )),
                }
            }

            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
            )),
        }
    }

    /// Reads `target[index]`, dispatching on what is being subscripted.
    ///
    /// A class extending a builtin is subscripted as one, and yields the base
    /// type: `Username("marc")[0]` is the string `"m"`.
    fn index_get(&self, target: &Value, index: &Value, span: Span) -> Result<Value, QuinceError> {
        match target.base(&self.heap) {
            Value::Dict(id) => {
                let key = key_of(&self.heap, index, span)?;
                self.heap.dict(*id).get(&key).cloned().ok_or_else(|| {
                    QuinceError::new(
                        format!("key {} is not in the dict", index.repr(&self.heap)),
                        span,
                    )
                    .with_kind(ErrorKind::Key)
                })
            }
            // Indexed by character, not by byte, because `len` already counts
            // characters — a subscript that disagreed with the length would be
            // indefensible. The cost is a walk, since the storage is UTF-8.
            Value::Str(text) => {
                let chars: Vec<char> = text.chars().collect();
                let offset = resolve_index(&self.heap, index, chars.len(), "string", span)?;
                Ok(Value::Str(Rc::from(chars[offset].to_string())))
            }

            _ => {
                let (id, offset) = self.list_index(target, index, span)?;
                Ok(self.heap.list(id)[offset].clone())
            }
        }
    }

    /// `target[start:end]`, on a string or a list.
    fn slice(
        &mut self,
        target: &Value,
        start: Option<&Value>,
        end: Option<&Value>,
        span: Span,
    ) -> Result<Value, QuinceError> {
        // Cloned rather than matched in place: the list arm allocates, so the
        // immutable borrow `base` takes of the heap has to be over by then.
        match target.base(&self.heap).clone() {
            Value::Str(text) => {
                let chars: Vec<char> = text.chars().collect();
                let (from, to) = slice_bounds(&self.heap, start, end, chars.len(), span)?;
                Ok(Value::Str(Rc::from(
                    chars[from..to].iter().collect::<String>(),
                )))
            }
            Value::List(id) => {
                let (from, to) =
                    slice_bounds(&self.heap, start, end, self.heap.list(id).len(), span)?;
                let items = self.heap.list(id)[from..to].to_vec();
                Ok(Value::List(self.heap.alloc(Object::List(items))))
            }
            _ => Err(QuinceError::new(
                format!("cannot slice {}", target.type_name(&self.heap)),
                span,
            )),
        }
    }

    /// Resolves a list subscript, accepting Python-style negative indices.
    fn list_index(
        &self,
        target: &Value,
        index: &Value,
        span: Span,
    ) -> Result<(ObjId, usize), QuinceError> {
        let Value::List(id) = target.base(&self.heap) else {
            return Err(QuinceError::new(
                format!("cannot index {}", target.type_name(&self.heap)),
                span,
            ));
        };
        let offset = resolve_index(&self.heap, index, self.heap.list(*id).len(), "list", span)?;
        Ok((*id, offset))
    }

    fn call(&mut self, target: Value, args: Vec<Value>, span: Span) -> Result<Value, QuinceError> {
        match target {
            Value::Native(native) => {
                if let Some(arity) = native.arity {
                    check_arity(native.name, arity, args.len(), span)?;
                }
                // `args` lives in this frame and nothing roots it. That is safe
                // only while no builtin reaches a safe point; the first one to
                // call back into Quince has to root them here first.
                (native.func)(self, &args, span)
            }

            Value::Function(id) => {
                let func = self.heap.function(id).clone();
                check_arity(&func.decl.name, func.decl.params.len(), args.len(), span)?;

                if self.depth >= MAX_DEPTH {
                    return Err(QuinceError::new(
                        format!("recursion limit of {MAX_DEPTH} calls exceeded"),
                        span,
                    )
                    .with_kind(ErrorKind::Recursion));
                }

                // Parameters are the body scope's first slots, in order, so
                // binding them needs no names at all.
                let scope = self.heap.alloc(Object::Env(Env::new(
                    Some(func.env),
                    func.decl.body.slot_count,
                )));
                for (index, arg) in args.into_iter().enumerate() {
                    self.heap.env_mut(scope).set(index as u16, arg);
                }

                self.depth += 1;
                let result = self.exec_scoped(&func.decl.body.stmts, scope);
                self.depth -= 1;

                match result? {
                    Flow::Return(value) => Ok(value),
                    Flow::Normal => Ok(Value::Nil),
                }
            }

            Value::BoundMethod(id) => {
                let bound = self.heap.bound_method(id).clone();
                self.call_method(bound.receiver, bound.method, args, span)
            }

            // Calling a class builds an instance and hands it to `init`, which
            // is why `init` returns nothing useful: the object already exists
            // by the time it runs.
            Value::Class(id) => {
                // Except for a builtin, where there is no object to build. An
                // int has nowhere to keep a field, so its `init` returns the
                // value instead of filling one in, and the call yields that. The
                // difference is only in what construction *produces* — the class
                // is a class like any other, and its `init` is found the same way.
                if let Some(builtin) = self.heap.class(id).builtin {
                    return self.construct_builtin(id, builtin, args, span);
                }

                let instance_id = self.heap.alloc(Object::Instance(Instance {
                    class: id,
                    fields: Dict::new(),
                    // Filled below, either by an `op init` calling `super.init` or
                    // by the conversion this class inherited. Not here, because
                    // only the constructor knows what value to convert.
                    payload: None,
                }));
                let instance = Value::Instance(instance_id);

                // The `op init` the class resolved when it was built, not a
                // lookup of the name `init` — a method merely *called* `init` is
                // an ordinary method, and construction must not reach it.
                match self.heap.class(id).init.clone() {
                    // A native here is a conversion, inherited by a class that
                    // declares no `op init` of its own: nothing was written to run,
                    // so construction does what an `op init` forwarding to
                    // `super.init` would have done. `Username("marc")` needs no
                    // constructor to be a string, and writing the one that only
                    // forwards is boilerplate the language can supply.
                    Some(init @ Value::Native(_)) => {
                        let builtin = self
                            .builtin_base(id)
                            .expect("only a builtin's seed puts a native in `init`");
                        self.set_payload(instance_id, init, builtin, args, span)
                            .map_err(|err| {
                                // The arity comes from the conversion, so it says
                                // `string` where the call site said `Username`.
                                // True, and worth saying rather than rewriting:
                                // this class is built from what its base is.
                                let name = self.heap.class(id).name.clone();
                                err.with_help(format!(
                                    "`{name}` extends `{}`, so it is built from whatever a {} is",
                                    builtin.name(),
                                    builtin.name()
                                ))
                            })?;
                    }
                    // `init` runs Quince code, so it reaches safe points, and
                    // the instance needs no root here: it sits in slot 0 of the
                    // constructor's scope, which `exec_scoped` roots for the
                    // whole body.
                    //
                    // That holds only because `self` cannot be reassigned — the
                    // resolver refuses it, see `Param::receiver`. This used to
                    // push the instance onto `temps` for exactly that reason,
                    // and the root came out when the language rule went in.
                    Some(init) => {
                        self.call_method(instance.clone(), init, args, span)?;
                    }
                    // No constructor, so the only correct call passes nothing.
                    None => {
                        let name = self.heap.class(id).name.clone();
                        // Marking is what makes a constructor a constructor, so
                        // a plain `fn init` is the likeliest reason to be here
                        // with arguments in hand. Saying so turns the one mistake
                        // the marking rule allows into an instruction.
                        let unmarked = self
                            .heap
                            .class(id)
                            .method(Op::Init.name(), &self.heap)
                            .is_some();
                        check_arity(&name, 0, args.len(), span).map_err(|err| {
                            if unmarked {
                                err.with_help(format!(
                                    "`{name}` has a method `init`, but only `op init` runs when a class is constructed"
                                ))
                            } else {
                                err
                            }
                        })?;
                    }
                }
                Ok(instance)
            }

            other => Err(QuinceError::new(
                format!("{} is not callable", other.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)),
        }
    }

    /// Calling a builtin type: `int("42")`, `list()`.
    ///
    /// No instance is allocated. A builtin's `init` is a conversion, so it takes
    /// the call's arguments and nothing else, and the value it returns is the
    /// result of the call. That is why this reaches `call` rather than
    /// `call_method` — there is no receiver to insert.
    fn construct_builtin(
        &mut self,
        id: ObjId,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, QuinceError> {
        let Some(init) = self.heap.class(id).init.clone() else {
            // Only `function` reaches this: `nil` and `class` are keywords, so
            // neither is bound as a global and nothing can name them to call.
            return Err(
                QuinceError::new(format!("cannot make {}", an(builtin.name())), span)
                    .with_kind(ErrorKind::Type)
                    .with_help(format!(
                        "there is no value a {} could be made from — `fn` is how one is written",
                        builtin.name()
                    )),
            );
        };
        self.call(init, args, span)
    }

    /// `super.init(…)` where the superclass is a builtin: the conversion runs,
    /// and what it produces becomes the receiver's payload.
    ///
    /// The conversion is the same one `string(x)` reaches, so the arities and the
    /// errors are the ones already written — `super.init("abc")` on an `int`
    /// ancestor raises the same `ValueError` as `int("abc")` does, at the
    /// `super.init` rather than at the constructor call.
    fn super_init(
        &mut self,
        class: ObjId,
        builtin: Builtin,
        receiver: &Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, QuinceError> {
        let Value::Instance(id) = receiver else {
            unreachable!("`super` binds the enclosing method's receiver, always an instance");
        };
        let init = self
            .heap
            .class(class)
            .init
            .clone()
            .expect("a builtin reached as a superclass is one that converts");
        self.set_payload(*id, init, builtin, args, span)
    }

    /// Runs a conversion and keeps what it produced as `id`'s payload.
    ///
    /// Both ways a payload comes to exist end here: an explicit `super.init(…)`,
    /// and the implicit construction a class declaring no `op init` gets. They are
    /// the same operation on purpose — an implicit `op init` is not a second rule,
    /// only the observation that a class inheriting a conversion as its
    /// constructor should run it as one.
    fn set_payload(
        &mut self,
        id: ObjId,
        init: Value,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, QuinceError> {
        // Checked before the conversion runs, so a second write is refused on the
        // strength of the first rather than after quietly replacing it. The
        // resolver requires that *a* `super.init` is written but cannot say how
        // many run — a call in each arm of an `if` is one call — so this is where
        // "once" is actually enforced.
        if self.heap.instance(id).payload.is_some() {
            let name = self.heap.class(self.heap.instance(id).class).name.clone();
            return Err(QuinceError::new(
                format!("`super.init` was already called for this {name}"),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "{} is given its {} once, and this would replace it",
                an(&name),
                builtin.name()
            )));
        }

        // A conversion is a native and natives reach no safe point, so the
        // instance behind `id` cannot move or be collected across this call.
        let value = self.call(init, args, span)?;
        match self.heap.instance_mut(id) {
            Ok(instance) => {
                instance.payload = Some(value);
                Ok(Value::Nil)
            }
            // Freezing happens to a value a constructor already returned, so
            // reaching this means `init` was called a second time by hand, on an
            // object that never got a payload the first time.
            Err(_) => Err(frozen(&self.heap, &Value::Instance(id), span)),
        }
    }

    /// The builtin a class descends from, if it descends from one at all.
    ///
    /// Walks the same chain `Class::method` walks, and terminates for the same
    /// reason: a parent is read before its subclass's name is bound, so the chain
    /// cannot contain a cycle.
    fn builtin_base(&self, class: ObjId) -> Option<Builtin> {
        let mut class = self.heap.class(class);
        loop {
            if let Some(builtin) = class.builtin {
                return Some(builtin);
            }
            class = self.heap.class(class.parent?);
        }
    }

    /// Evaluates `receiver.name(args)`.
    ///
    /// Kept out of `eval` to stop the arm's locals widening a frame that
    /// recurses once per node in the tree. Measured against this program's
    /// recursion limit it made no difference — debug frames are dominated by
    /// things other than one arm — but it keeps `eval` readable.
    fn eval_method_call(
        &mut self,
        target: &Expr,
        name: &str,
        args: &[Expr],
        env: ObjId,
        callee_span: Span,
        span: Span,
    ) -> Result<Value, QuinceError> {
        let receiver = self.eval(target, env)?;
        let attr = self.attr(&receiver, name, callee_span)?;

        // The receiver is held across every argument, any of which can reach a
        // safe point — the same hazard as an ordinary callee, and the same fix.
        // The attribute needs it too: a field holding a function is reachable
        // only through that field, which an argument is free to overwrite.
        let mark = self.temps.len();
        for value in [&receiver, attr.value()] {
            if value.handle().is_some() {
                self.temps.push(value.clone());
            }
        }
        let values = self.eval_seq(args, env);
        self.temps.truncate(mark);
        let values = values?;

        match attr {
            Attr::Method(method) => self.call_method(receiver, method, values, span),
            // A field that happens to hold a function; it never took a receiver.
            Attr::Field(value) => self.call(value, values, span),
        }
    }

    /// Calls `method` with `receiver` in front of `args`.
    ///
    /// The receiver is passed as argument zero for both kinds of method, which
    /// is what lets one `Native` serve as a free function and a method at once,
    /// and what lets a user method be an ordinary function whose first
    /// parameter the parser wrote.
    fn call_method(
        &mut self,
        receiver: Value,
        method: Value,
        mut args: Vec<Value>,
        span: Span,
    ) -> Result<Value, QuinceError> {
        // Arity is reported against what the call site can actually write. The
        // receiver is one of the declared arguments but has no syntax as one,
        // so quoting the declared count would ask for an argument the user has
        // no way to supply. Every method has a receiver, making the subtraction
        // safe.
        let declared = match &method {
            Value::Native(native) => native.arity,
            Value::Function(id) => Some(self.heap.function(*id).decl.params.len()),
            // `attr` only ever produces the two above.
            other => panic!("expected a method, found {other:?}"),
        };
        if let Some(arity) = declared {
            check_arity(
                method.callable_name(&self.heap),
                arity.saturating_sub(1),
                args.len(),
                span,
            )?;
        }

        // A native was written against a `Value` and matches on its variant, so
        // an instance of a class extending a builtin has to arrive as the value
        // it *is* rather than as the object holding it — `e.upper()` reaches
        // `string`'s `upper`, which knows nothing about classes. A method written
        // in Quince gets the instance, because `self.domain` is the whole reason
        // it was written. Those two cases are exactly `Native` and `Function`:
        // every user method is a function, and a native only ever comes from a
        // builtin's seed.
        //
        // The one substitution in the interpreter, and the reason it is only one:
        // every path that gives a method a receiver comes through here.
        let receiver = match (&method, &receiver) {
            (Value::Native(_), Value::Instance(id)) => match &self.heap.instance(*id).payload {
                Some(payload) => payload.clone(),
                None => return Err(self.no_payload(*id, span)),
            },
            _ => receiver,
        };

        args.insert(0, receiver);
        self.call(method, args, span)
    }

    /// Looks up `name` on `receiver`, without calling anything.
    ///
    /// Fields shadow methods, following Python: a field is per-object and a
    /// method is per-class, so the more specific one wins. It also means a
    /// field holding a function is called as an ordinary function rather than
    /// silently acquiring a receiver it was never written to take.
    fn attr(&self, receiver: &Value, name: &str, span: Span) -> Result<Attr, QuinceError> {
        if let Value::Instance(id) = receiver
            && let Some(value) = self
                .heap
                .instance(*id)
                .fields
                .get(&Key::Str(Rc::from(name)))
        {
            return Ok(Attr::Field(value.clone()));
        }

        // A class hands back its methods unbound, so `Point.dist(p)` works and
        // a method really is a function with the receiver written out.
        if let Value::Class(id) = receiver {
            return match self.heap.class(*id).method(name, &self.heap) {
                Some(method) => Ok(Attr::Field(method)),
                None => Err(self.no_attr(receiver, name, span)),
            };
        }

        let class = receiver.class(&self.heap);
        match self.heap.class(class).method(name, &self.heap) {
            Some(method) => Ok(Attr::Method(method)),
            None => Err(self.no_attr(receiver, name, span)),
        }
    }

    /// The class `super` searches from, read through the slot the resolver
    /// assigned it in the scope wrapped around the methods.
    ///
    /// Separate from the receiver, which comes from the enclosing method's
    /// parameters — the two halves of `super` live in different scopes, and
    /// neither is searched for by name.
    fn super_class(&mut self, parent: &Var, env: ObjId, span: Span) -> Result<ObjId, QuinceError> {
        match self.read(parent, env, span)? {
            Value::Class(id) => Ok(id),
            _ => unreachable!("`super` is only ever bound to a class"),
        }
    }

    /// Looks `name` up starting *at* the superclass, which is what stops an
    /// override from calling itself: `Dog.speak` reaching for `super.speak`
    /// must not find `Dog.speak` again.
    fn super_method(&mut self, id: ObjId, name: &str, span: Span) -> Result<Value, QuinceError> {
        match self.heap.class(id).method(name, &self.heap) {
            Some(method) => Ok(method),
            None => Err(QuinceError::new(
                format!("{} has no method `{name}`", self.heap.class(id).name),
                span,
            )),
        }
    }

    /// A builtin's method reached through an instance that has no payload yet.
    ///
    /// The resolver refuses the ordinary way to arrive here — an `op init` with no
    /// `super.init` — but it works on names in one pass, so `final S = string`
    /// followed by `class X extends S` gets past it. This is what that costs, and
    /// it is a report rather than the panic a native would otherwise hit.
    fn no_payload(&self, id: ObjId, span: Span) -> QuinceError {
        let class = self.heap.instance(id).class;
        let name = self.heap.class(class).name.clone();
        // A native is only ever found on an instance by walking the class chain to
        // a builtin, since every method written in Quince is a `Function`. So
        // there is always a builtin to name, and the span says which use of it
        // asked — a method call, or the inherited conversion a class with no
        // `op init` of its own ends up with as its constructor.
        let builtin = self
            .builtin_base(class)
            .expect("a native method is only reachable through a builtin in the chain");
        QuinceError::new(
            format!("`{name}` was never given {}", an(builtin.name())),
            span,
        )
        .with_kind(ErrorKind::Type)
        .with_help(format!(
            "`{name}` extends `{}`, so its `op init` must call `super.init` before it is used",
            builtin.name()
        ))
    }

    fn no_attr(&self, receiver: &Value, name: &str, span: Span) -> QuinceError {
        // An instance can grow fields at run time, so a missing name there is a
        // different mistake from a missing method on a builtin type.
        let what = match receiver {
            Value::Instance(_) => "no field or method",
            _ => "no method",
        };
        QuinceError::new(
            format!("{} has {what} `{name}`", receiver.type_name(&self.heap)),
            span,
        )
        .with_kind(ErrorKind::Attr)
    }

    // -- operators ---------------------------------------------------------

    fn unary(&self, op: UnaryOp, value: Value, span: Span) -> Result<Value, QuinceError> {
        // `not` asks only for truthiness, which unwraps a payload for itself.
        if let UnaryOp::Not = op {
            return Ok(Value::Bool(!value.is_truthy(&self.heap)));
        }
        // Negation acts on the number, so a class extending `int` is unwrapped to
        // it and `-Count(5)` is `-5` rather than a `Count`. The error still names
        // the class, because that is the value the line was written about.
        match value.base(&self.heap) {
            Value::Int(n) => n.checked_neg().map(Value::Int).ok_or_else(|| {
                QuinceError::new("integer overflow", span).with_kind(ErrorKind::Overflow)
            }),
            Value::Float(n) => Ok(Value::Float(-n)),
            _ => Err(QuinceError::new(
                format!("cannot negate {}", value.type_name(&self.heap)),
                span,
            )),
        }
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> Result<Value, QuinceError> {
        use BinaryOp::*;

        // Equality is defined for every pair of types, so it never fails.
        match op {
            Eq => return Ok(Value::Bool(lhs.equals(&rhs, &self.heap))),
            Ne => return Ok(Value::Bool(!lhs.equals(&rhs, &self.heap))),
            In => return self.contains(&rhs, &lhs, span),
            _ => {}
        }

        // Dispatch on what the operands *are*, so a class extending a builtin is
        // operated on as one — and the result is the base type, not the subclass.
        // `Username("marc") + "!"` is a string: preserving the class would mean
        // re-running its `op init`, so a validating constructor would run on every
        // concatenation.
        //
        // Reporting still uses `lhs` and `rhs`. The dispatch wants the base type
        // and the message wants the name the line was written with, and they are
        // different questions: `Username("a") - 1` is refused because a string
        // cannot be subtracted, but naming `Username` is what says which value.
        let (a, b) = (lhs.base(&self.heap).clone(), rhs.base(&self.heap).clone());

        // `+` is the one operator shared between numbers and the collections.
        if let (Add, Value::Str(a), Value::Str(b)) = (op, &a, &b) {
            return Ok(Value::Str(Rc::from(format!("{a}{b}"))));
        }

        // Concatenation builds a new list rather than extending the left one,
        // matching `+` on strings. `push` is there for growing in place.
        if let (Add, Value::List(a), Value::List(b)) = (op, &a, &b) {
            let mut items = self.heap.list(*a).clone();
            items.extend_from_slice(self.heap.list(*b));
            return Ok(Value::List(self.heap.alloc(Object::List(items))));
        }

        if let (Value::Str(x), Value::Str(y)) = (&a, &b) {
            return match op {
                Lt => Ok(Value::Bool(x < y)),
                Le => Ok(Value::Bool(x <= y)),
                Gt => Ok(Value::Bool(x > y)),
                Ge => Ok(Value::Bool(x >= y)),
                _ => Err(type_error(&self.heap, op, &lhs, &rhs, span)),
            };
        }

        match (&a, &b) {
            // Both ints: stay an int, and refuse to wrap on overflow.
            (Value::Int(x), Value::Int(y)) => int_op(op, *x, *y, span),

            // Any float involved promotes the whole operation.
            (Value::Float(_), Value::Int(_))
            | (Value::Int(_), Value::Float(_))
            | (Value::Float(_), Value::Float(_)) => float_op(op, as_float(&a), as_float(&b), span),

            _ => Err(type_error(&self.heap, op, &lhs, &rhs, span)),
        }
    }

    /// `needle in haystack`.
    ///
    /// An unhashable needle is an error rather than a plain `false`, for the
    /// same reason `d[[]]` is: a value that could never have been inserted is a
    /// mistake in the program, and answering `false` would hide it.
    fn contains(&self, haystack: &Value, needle: &Value, span: Span) -> Result<Value, QuinceError> {
        // Both sides unwrap: a subclass of `list` can be searched, and a subclass
        // of `string` can be the part searched for. `equals` and `key_of` unwrap the
        // needle for themselves, so only the string arm needs it named.
        let found = match haystack.base(&self.heap) {
            Value::Dict(id) => self
                .heap
                .dict(*id)
                .contains(&key_of(&self.heap, needle, span)?),
            Value::List(id) => self
                .heap
                .list(*id)
                .iter()
                .any(|item| item.equals(needle, &self.heap)),
            Value::Str(text) => match needle.base(&self.heap) {
                Value::Str(part) => text.contains(part.as_ref()),
                _ => {
                    return Err(QuinceError::new(
                        format!(
                            "cannot look for {} in a string",
                            needle.type_name(&self.heap)
                        ),
                        span,
                    ));
                }
            },
            _ => {
                return Err(QuinceError::new(
                    format!("cannot use `in` on {}", haystack.type_name(&self.heap)),
                    span,
                ));
            }
        };
        Ok(Value::Bool(found))
    }
}

impl Default for Interp {
    fn default() -> Self {
        Interp::new()
    }
}

/// A slot the resolver failed to fill would mean the resolver never ran, which
/// is a wiring bug rather than anything a program can cause.
fn resolved(slot: &Option<Slot>) -> Slot {
    slot.expect("the resolver must run before evaluation")
}

/// Converts a value to a dict key, explaining why if it cannot be one.
///
/// The payload unwrap belongs here rather than in `Key::from_value`, which has no
/// heap to reach a payload through — and this is the only caller that is not a
/// test. It is also not a separate decision from `equals`: if `Username("marc")`
/// equals `"marc"` then the two must hash alike, or a dict holds two equal keys in
/// different buckets. So a subclass is hashable exactly when its base is, and a
/// dict cannot tell the two apart — `keys()` hands back the base type.
fn key_of(heap: &Heap, value: &Value, span: Span) -> Result<Key, QuinceError> {
    Key::from_value(value.base(heap)).map_err(|reason| {
        let message = match reason {
            NotAKey::Unhashable => format!(
                "a {} cannot be a dict key, because it is compared by identity",
                value.type_name(heap)
            ),
            NotAKey::Nan => "NaN cannot be a dict key, because it is not equal to itself".into(),
        };
        QuinceError::new(message, span)
    })
}

fn as_float(value: &Value) -> f64 {
    match value {
        Value::Int(n) => *n as f64,
        Value::Float(n) => *n,
        _ => unreachable!("as_float is only reached for numbers"),
    }
}

/// Rounds toward negative infinity, which Rust's `/` does not — it truncates
/// toward zero, so `-7 / 2` is `-3` there but `-4` here.
///
/// `div_euclid` is not the same thing: it keeps the remainder non-negative, so
/// it disagrees whenever the divisor is negative.
fn floor_div(a: i64, b: i64) -> Option<i64> {
    let quotient = a.checked_div(b)?;
    if a % b != 0 && (a < 0) != (b < 0) {
        quotient.checked_sub(1)
    } else {
        Some(quotient)
    }
}

/// Integer arithmetic. Reports overflow rather than wrapping.
fn int_op(op: BinaryOp, a: i64, b: i64, span: Span) -> Result<Value, QuinceError> {
    use BinaryOp::*;
    let overflow = || QuinceError::new("integer overflow", span).with_kind(ErrorKind::Overflow);

    let value =
        match op {
            Add => Value::Int(a.checked_add(b).ok_or_else(overflow)?),
            Sub => Value::Int(a.checked_sub(b).ok_or_else(overflow)?),
            Mul => Value::Int(a.checked_mul(b).ok_or_else(overflow)?),
            // True division always leaves the integers behind, so `1 / 2` is `0.5`
            // rather than `0`. `//` is there when an int is wanted.
            Div => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision));
                }
                Value::Float(a as f64 / b as f64)
            }
            FloorDiv => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision));
                }
                Value::Int(floor_div(a, b).ok_or_else(overflow)?)
            }
            Rem => {
                if b == 0 {
                    return Err(QuinceError::new("division by zero", span)
                        .with_kind(ErrorKind::ZeroDivision));
                }
                Value::Int(a.checked_rem(b).ok_or_else(overflow)?)
            }
            Lt => Value::Bool(a < b),
            Le => Value::Bool(a <= b),
            Gt => Value::Bool(a > b),
            Ge => Value::Bool(a >= b),
            Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
        };
    Ok(value)
}

fn float_op(op: BinaryOp, a: f64, b: f64, span: Span) -> Result<Value, QuinceError> {
    use BinaryOp::*;
    let value = match op {
        Add => Value::Float(a + b),
        Sub => Value::Float(a - b),
        Mul => Value::Float(a * b),
        // Kept an error rather than yielding infinity, to match integer division.
        Div if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span).with_kind(ErrorKind::ZeroDivision)
            );
        }
        Div => Value::Float(a / b),
        FloorDiv if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span).with_kind(ErrorKind::ZeroDivision)
            );
        }
        FloorDiv => Value::Float((a / b).floor()),
        Rem if b == 0.0 => {
            return Err(
                QuinceError::new("division by zero", span).with_kind(ErrorKind::ZeroDivision)
            );
        }
        Rem => Value::Float(a % b),
        Lt => Value::Bool(a < b),
        Le => Value::Bool(a <= b),
        Gt => Value::Bool(a > b),
        Ge => Value::Bool(a >= b),
        Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
    };
    Ok(value)
}

/// The error for a mutation the heap refused.
///
/// It names `const` rather than saying only "frozen", because freezing has
/// exactly one cause in the language and the reader's next question is always
/// what did this. The value it names may be several steps from the `const` that
/// froze it — that is what "deeply" means.
fn frozen(heap: &Heap, value: &Value, span: Span) -> QuinceError {
    QuinceError::new(
        format!("cannot modify `const` {}", value.type_name(heap)),
        span,
    )
    .with_kind(ErrorKind::Frozen)
}

fn type_error(heap: &Heap, op: BinaryOp, lhs: &Value, rhs: &Value, span: Span) -> QuinceError {
    use BinaryOp::*;
    let verb = match op {
        Add => "add",
        Sub => "subtract",
        Mul => "multiply",
        Div | FloorDiv => "divide",
        Rem => "take the remainder of",
        Lt | Le | Gt | Ge => "compare",
        Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
    };
    QuinceError::new(
        format!(
            "cannot {verb} {} and {}",
            lhs.type_name(heap),
            rhs.type_name(heap)
        ),
        span,
    )
    .with_kind(ErrorKind::Type)
}

/// Resolves a subscript against a length, accepting Python-style negatives.
///
/// Shared by lists and strings so the two cannot drift apart on what `-1`
/// means or on how an out-of-range index reads.
fn resolve_index(
    heap: &Heap,
    index: &Value,
    len: usize,
    what: &str,
    span: Span,
) -> Result<usize, QuinceError> {
    let Value::Int(raw) = index else {
        return Err(QuinceError::new(
            format!(
                "{what} index must be an int, found {}",
                index.type_name(heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type));
    };

    let offset = if *raw < 0 { *raw + len as i64 } else { *raw };
    if offset < 0 || offset >= len as i64 {
        return Err(QuinceError::new(
            format!("index {raw} is out of range for a {what} of length {len}"),
            span,
        )
        .with_kind(ErrorKind::Index));
    }
    Ok(offset as usize)
}

/// Resolves slice bounds, which are **clamped rather than checked**.
///
/// `xs[:100]` asks for at most a hundred, not for a hundred that must exist, so
/// clamping is what makes "take the first n" writable without a length test
/// first. A subscript keeps erroring, because a single out-of-range index can
/// only be a mistake. An inverted range yields nothing rather than erroring,
/// for the same reason.
fn slice_bounds(
    heap: &Heap,
    start: Option<&Value>,
    end: Option<&Value>,
    len: usize,
    span: Span,
) -> Result<(usize, usize), QuinceError> {
    let len = len as i64;
    let resolve = |bound: Option<&Value>, default: i64| -> Result<i64, QuinceError> {
        let Some(bound) = bound else {
            return Ok(default);
        };
        let Value::Int(raw) = bound else {
            return Err(QuinceError::new(
                format!("slice bounds must be ints, found {}", bound.type_name(heap)),
                span,
            )
            .with_kind(ErrorKind::Type));
        };
        Ok(if *raw < 0 { *raw + len } else { *raw })
    };

    let from = resolve(start, 0)?.clamp(0, len);
    let to = resolve(end, len)?.clamp(0, len);
    Ok((from as usize, to.max(from) as usize))
}

fn check_arity(name: &str, expected: usize, found: usize, span: Span) -> Result<(), QuinceError> {
    if expected == found {
        return Ok(());
    }
    let plural = if expected == 1 { "" } else { "s" };
    Err(QuinceError::new(
        format!("`{name}` takes {expected} argument{plural}, but {found} were given"),
        span,
    )
    .with_kind(ErrorKind::Type))
}

// -- builtins --------------------------------------------------------------

/// The globals every program starts with.
///
/// Anything that acts on one particular type is a method instead, reached
/// through that type's table in `class.rs`. What remains here either applies to
/// every type (`len`, `type`) or to none of them (`print`).
static BUILTINS: &[&Native] = &[&PRINT, &LEN, &TYPE];

static PRINT: Native = Native {
    name: "print",
    arity: None,
    func: |interp, args, _span| {
        let parts: Vec<_> = args
            .iter()
            .map(|value| value.display(&interp.heap))
            .collect();
        writeln!(interp.out, "{}", parts.join(" ")).expect("failed to write output");
        Ok(Value::Nil)
    },
};

static LEN: Native = Native {
    name: "len",
    arity: Some(1),
    // Not a method, so it does not come through `call_method`'s substitution and
    // has to unwrap for itself. The error names the class rather than its base:
    // `len` failing on a `Box` should say `Box`.
    func: |interp, args, span| match args[0].base(&interp.heap) {
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::List(id) => Ok(Value::Int(interp.heap.list(*id).len() as i64)),
        Value::Dict(id) => Ok(Value::Int(interp.heap.dict(*id).len() as i64)),
        _ => Err(QuinceError::new(
            format!(
                "`len` does not apply to {}",
                args[0].type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

/// The in-place counterpart to `+`, which builds a new list.
///
/// `args[0]` is the receiver, so a method's declared arity is one more than the
/// number of arguments written at the call site.
pub static PUSH: Native = Native {
    name: "push",
    arity: Some(2),
    func: |interp, args, span| match &args[0] {
        Value::List(id) => {
            let pushed = interp
                .heap
                .list_mut(*id)
                .map(|items| items.push(args[1].clone()));
            pushed.map_err(|_| frozen(&interp.heap, &args[0], span))?;
            Ok(Value::Nil)
        }
        other => Err(QuinceError::new(
            format!(
                "`push` needs a list, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

pub static KEYS: Native = Native {
    name: "keys",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let keys: Vec<_> = interp.heap.dict(*id).keys().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(keys))))
        }
        other => Err(QuinceError::new(
            format!(
                "`keys` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

pub static VALUES: Native = Native {
    name: "values",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let values: Vec<_> = interp.heap.dict(*id).values().cloned().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(values))))
        }
        other => Err(QuinceError::new(
            format!(
                "`values` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

/// Removing a key that is not there is an error, for the same reason reading one
/// is: silently doing nothing hides the typo that caused it.
pub static REMOVE: Native = Native {
    name: "remove",
    arity: Some(2),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let key = key_of(&interp.heap, &args[1], span)?;
            let removed = interp
                .heap
                .dict_mut(*id)
                .map(|entries| entries.remove(&key));
            removed
                .map_err(|_| frozen(&interp.heap, &args[0], span))?
                .ok_or_else(|| {
                    QuinceError::new(
                        format!("key {} is not in the dict", args[1].repr(&interp.heap)),
                        span,
                    )
                    .with_kind(ErrorKind::Key)
                })
        }
        other => Err(QuinceError::new(
            format!(
                "`remove` needs a dict, but was given {}",
                other.type_name(&interp.heap)
            ),
            span,
        )
        .with_kind(ErrorKind::Type)),
    },
};

// -- string methods --------------------------------------------------------

/// The receiver of a string method.
///
/// Dispatch guarantees the type: the method was reached through
/// `class::STR`'s table, which nothing but a string can name.
fn text(args: &[Value]) -> &Rc<str> {
    match &args[0] {
        Value::Str(text) => text,
        other => unreachable!("a string method received {other:?}"),
    }
}

/// A string argument, or an error naming what arrived instead.
fn text_arg(
    heap: &Heap,
    args: &[Value],
    at: usize,
    name: &str,
    span: Span,
) -> Result<Rc<str>, QuinceError> {
    match &args[at] {
        Value::Str(text) => Ok(Rc::clone(text)),
        other => Err(QuinceError::new(
            format!(
                "`{name}` needs a string, but was given {}",
                other.type_name(heap)
            ),
            span,
        )),
    }
}

pub static UPPER: Native = Native {
    name: "upper",
    arity: Some(1),
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).to_uppercase()))),
};

pub static LOWER: Native = Native {
    name: "lower",
    arity: Some(1),
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).to_lowercase()))),
};

pub static TRIM: Native = Native {
    name: "trim",
    arity: Some(1),
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(text(args).trim()))),
};

pub static STARTS_WITH: Native = Native {
    name: "starts_with",
    arity: Some(2),
    func: |interp, args, span| {
        let prefix = text_arg(&interp.heap, args, 1, "starts_with", span)?;
        Ok(Value::Bool(text(args).starts_with(prefix.as_ref())))
    },
};

pub static ENDS_WITH: Native = Native {
    name: "ends_with",
    arity: Some(2),
    func: |interp, args, span| {
        let suffix = text_arg(&interp.heap, args, 1, "ends_with", span)?;
        Ok(Value::Bool(text(args).ends_with(suffix.as_ref())))
    },
};

pub static REPLACE: Native = Native {
    name: "replace",
    arity: Some(3),
    func: |interp, args, span| {
        let from = text_arg(&interp.heap, args, 1, "replace", span)?;
        if from.is_empty() {
            return Err(QuinceError::new(
                "`replace` needs something to look for, but was given \"\"".to_string(),
                span,
            ));
        }
        let to = text_arg(&interp.heap, args, 2, "replace", span)?;
        Ok(Value::Str(Rc::from(
            text(args).replace(from.as_ref(), to.as_ref()),
        )))
    },
};

/// Splitting on `""` is an error rather than yielding the characters, because
/// the two are different requests and `chars` already answers the second one.
pub static SPLIT: Native = Native {
    name: "split",
    arity: Some(2),
    func: |interp, args, span| {
        let sep = text_arg(&interp.heap, args, 1, "split", span)?;
        if sep.is_empty() {
            return Err(QuinceError::new(
                "`split` needs a separator, but was given \"\" — use `chars` instead".to_string(),
                span,
            ));
        }
        let parts: Vec<Value> = text(args)
            .split(sep.as_ref())
            .map(|part| Value::Str(Rc::from(part)))
            .collect();
        Ok(Value::List(interp.heap.alloc(Object::List(parts))))
    },
};

pub static CHARS: Native = Native {
    name: "chars",
    arity: Some(1),
    func: |interp, args, _span| {
        let chars: Vec<Value> = text(args)
            .chars()
            .map(|c| Value::Str(Rc::from(c.to_string())))
            .collect();
        Ok(Value::List(interp.heap.alloc(Object::List(chars))))
    },
};

/// The separator is the receiver, as in Python: `", ".join(xs)`. Reads badly
/// once, but it is the string that decides how the pieces are put together.
pub static JOIN: Native = Native {
    name: "join",
    arity: Some(2),
    func: |interp, args, span| {
        let Value::List(id) = &args[1] else {
            return Err(QuinceError::new(
                format!(
                    "`join` needs a list, but was given {}",
                    args[1].type_name(&interp.heap)
                ),
                span,
            ));
        };

        let mut parts = Vec::with_capacity(interp.heap.list(*id).len());
        for item in interp.heap.list(*id) {
            match item {
                Value::Str(part) => parts.push(Rc::clone(part)),
                other => {
                    return Err(QuinceError::new(
                        format!(
                            "`join` needs a list of strings, but found {}",
                            other.type_name(&interp.heap)
                        ),
                        span,
                    ));
                }
            }
        }
        Ok(Value::Str(Rc::from(parts.join(text(args)))))
    },
};

static TYPE: Native = Native {
    name: "type",
    arity: Some(1),
    func: |interp, args, _span| Ok(Value::Str(Rc::from(args[0].type_name(&interp.heap)))),
};

// -- conversion ------------------------------------------------------------
//
// A builtin type's `op init`, reached by calling the type: `int("42")`. These
// take no receiver, unlike every other native in this file, because there is
// nothing to receive — the value they return *is* what the construction
// produced. See `Class::init` and `construct_builtin`.
//
// Two error kinds, and the split is deliberate. `ErrorKind::Type` is for an
// argument the conversion never accepts, where the fix is at the call:
// `int([1])`. `ErrorKind::Value` is for one it does accept carrying data it
// cannot use, where the call is right and the data is not: `int("abc")`.
// Naming that difference is the whole reason `Value` was added as a kind.

/// The argument named as a type, for a conversion that cannot accept it at all.
fn not_convertible(heap: &Heap, to: &str, value: &Value, span: Span) -> QuinceError {
    QuinceError::new(
        format!("cannot make {} from {}", an(to), an(value.type_name(heap))),
        span,
    )
    .with_kind(ErrorKind::Type)
}

/// A type name as it reads in a message, so one reads as English rather than as
/// a template.
///
/// `nil` takes no article: it is the one type name that is also the name of its
/// only value, so "from nil" names the value and "from a nil" names nothing.
fn an(noun: &str) -> String {
    if noun == crate::class::Builtin::Nil.name() {
        return noun.to_string();
    }
    let article = match noun.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => "an",
        false => "a",
    };
    format!("{article} {noun}")
}

/// Rejects a float that no integer can represent, which `as` would otherwise
/// answer with a saturated bound — silently, and almost never usefully.
fn checked_trunc(f: f64, span: Span) -> Result<i64, QuinceError> {
    if f.is_nan() {
        return Err(
            QuinceError::new("cannot make an int from NaN", span).with_kind(ErrorKind::Value)
        );
    }
    // Compared before truncating: truncation of a value this large is a no-op,
    // so doing it first would not bring anything into range.
    //
    // The two comparisons are deliberately not symmetric. `i64::MIN` is -2^63,
    // which an `f64` holds exactly, so a float equal to it converts. `i64::MAX`
    // is 2^63-1, which an `f64` cannot hold — `i64::MAX as f64` rounds *up* to
    // 2^63 — so a float that equal is already out of range, and `>` rather than
    // `>=` here would let it through to saturate silently.
    if f < i64::MIN as f64 || f >= i64::MAX as f64 {
        return Err(
            QuinceError::new(format!("{f} is too large to be an int"), span)
                .with_kind(ErrorKind::Overflow),
        );
    }
    Ok(f.trunc() as i64)
}

/// Named for the type rather than for `init`, because this native's name is what
/// an arity error quotes and `int(1, 2)` should be told about `int`.
pub static INT_INIT: Native = Native {
    name: "int",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Int(n) => Ok(Value::Int(*n)),
        // Toward zero, unlike `//`, which floors. `int` follows the same rule as
        // Python and Rust's `as`; `//` deliberately does not, so that `-7 // 2`
        // stays the mathematical quotient.
        Value::Float(f) => Ok(Value::Int(checked_trunc(*f, span)?)),
        // Surrounding whitespace is dropped, since a number read from a file or
        // a prompt arrives with it and stripping is what the caller would do.
        Value::Str(text) => text.trim().parse::<i64>().map(Value::Int).map_err(|_| {
            QuinceError::new(
                format!("cannot make an int from {}", args[0].repr(&interp.heap)),
                span,
            )
            .with_kind(ErrorKind::Value)
        }),
        // `1` and `0`, as every language with this conversion has it. This is not
        // a crack in the rule that a bool is not a number: `1 + true` stays an
        // error, because nobody asked for a conversion there.
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        other => Err(not_convertible(&interp.heap, "int", other, span)),
    },
};

pub static FLOAT_INIT: Native = Native {
    name: "float",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Int(n) => Ok(Value::Float(*n as f64)),
        Value::Str(text) => text.trim().parse::<f64>().map(Value::Float).map_err(|_| {
            QuinceError::new(
                format!("cannot make a float from {}", args[0].repr(&interp.heap)),
                span,
            )
            .with_kind(ErrorKind::Value)
        }),
        Value::Bool(b) => Ok(Value::Float(*b as i64 as f64)),
        other => Err(not_convertible(&interp.heap, "float", other, span)),
    },
};

/// Total, and the same text `print` writes — there is no value without a
/// rendering, so this cannot fail and needs no error kind at all.
pub static STR_INIT: Native = Native {
    name: "string",
    arity: Some(1),
    func: |interp, args, _span| Ok(Value::Str(Rc::from(args[0].display(&interp.heap)))),
};

/// Exactly the test `if` applies, exposed as a value. Also total.
pub static BOOL_INIT: Native = Native {
    name: "bool",
    arity: Some(1),
    func: |interp, args, _span| Ok(Value::Bool(args[0].is_truthy(&interp.heap))),
};

/// `list()` is empty and `list(xs)` copies. Nothing else converts: `list("ab")`
/// could mean characters, and `list({"a": 1})` could mean keys, entries, or
/// values. Both are refused rather than guessed at, with the method that means
/// the likely thing named in the help.
pub static LIST_INIT: Native = Native {
    name: "list",
    arity: None,
    func: |interp, args, span| match args {
        [] => Ok(Value::List(interp.heap.alloc(Object::List(Vec::new())))),
        // Shallow, as in Python: the new list holds the same elements rather
        // than copies of them, so a nested list stays shared.
        [Value::List(id)] => {
            let items = interp.heap.list(*id).clone();
            Ok(Value::List(interp.heap.alloc(Object::List(items))))
        }
        [Value::Str(_)] => Err(not_convertible(&interp.heap, "list", &args[0], span)
            .with_help("`chars` splits a string into its characters")),
        [Value::Dict(_)] => Err(not_convertible(&interp.heap, "list", &args[0], span)
            .with_help("`keys` or `values` picks which half of a dict to take")),
        [other] => Err(not_convertible(&interp.heap, "list", other, span)),
        _ => Err(too_many("list", args.len(), span)),
    },
};

pub static DICT_INIT: Native = Native {
    name: "dict",
    arity: None,
    func: |interp, args, span| match args {
        [] => Ok(Value::Dict(interp.heap.alloc(Object::Dict(Dict::new())))),
        [Value::Dict(id)] => {
            let entries = interp.heap.dict(*id).clone();
            Ok(Value::Dict(interp.heap.alloc(Object::Dict(entries))))
        }
        [other] => Err(not_convertible(&interp.heap, "dict", other, span)),
        _ => Err(too_many("dict", args.len(), span)),
    },
};

/// `check_arity` states one exact count, which these two conversions do not
/// have — they take nothing or one thing.
fn too_many(name: &str, found: usize, span: Span) -> QuinceError {
    QuinceError::new(
        format!("`{name}` takes 0 or 1 arguments, but {found} were given"),
        span,
    )
    .with_kind(ErrorKind::Type)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(interp: &Interp, name: &str) -> Option<Value> {
        interp.heap.globals(interp.globals).get(name).cloned()
    }

    fn run(source: &str) -> Interp {
        let program = crate::compile(source).expect("the test program should parse");
        let mut interp = Interp::with_output(Box::new(Vec::new()));
        interp.run(&program).expect("the test program should run");
        interp
    }

    #[test]
    fn a_type_name_is_a_global_unless_the_lexer_claimed_it() {
        // The exception set is derived from `TokenKind::keyword`, so it can grow
        // without anyone touching this file — a type named after a future keyword
        // would silently stop being bound. Pinned here so that becomes a failure,
        // and stated as two lists so the reason stays legible.
        let interp = Interp::with_output(Box::new(Vec::new()));

        for builtin in BUILTIN_TYPES {
            let name = builtin.name();
            let bound = global(&interp, name);
            match TokenKind::keyword(name) {
                Some(_) => assert!(
                    bound.is_none(),
                    "`{name}` is a keyword, so no global could ever be read under it"
                ),
                None => assert_eq!(
                    bound,
                    Some(Value::Class(interp.heap.builtin_class(*builtin))),
                    "`{name}` should be bound to its own class"
                ),
            }
        }

        // The two that are keywords today. Written out so that one of them
        // ceasing to be a keyword is a decision rather than a diff.
        assert!(global(&interp, "nil").is_none());
        assert!(global(&interp, "class").is_none());
    }

    /// Which builtins can be extended, decided by the one thing that decides it:
    /// whether there is a conversion for `super.init` to call. Enumerated rather
    /// than spot-checked, so a builtin added later cannot land on either side of
    /// this line by accident.
    #[test]
    fn a_builtin_can_be_extended_exactly_when_it_converts() {
        for builtin in BUILTIN_TYPES {
            // `nil` and `class` cannot be written after `extends` at all — one is
            // a keyword, the other is not bound as a global — so the two that are
            // reachable here are the constructible ones and `function`.
            if matches!(builtin, Builtin::Nil | Builtin::Class) {
                continue;
            }
            let source = format!(
                "class Sub extends {} {{\n op init(x) {{ super.init(x) }}\n}}",
                builtin.name()
            );
            let program = crate::compile(&source).expect("should parse");
            let mut interp = Interp::with_output(Box::new(Vec::new()));
            let result = interp.run(&program);

            match builtin.seed().init {
                Some(_) => assert!(
                    result.is_ok(),
                    "`{}` converts, so it can be extended: {result:?}",
                    builtin.name()
                ),
                None => assert_eq!(
                    result.expect_err("no conversion, so no subclass").message,
                    format!("`Sub` cannot extend `{}`", builtin.name())
                ),
            }
        }
    }

    /// The payload is unobservable from Quince until the operators land, so the
    /// value `super.init` stored is checked here instead — and it has to be the
    /// converted value, not the argument.
    #[test]
    fn super_init_stores_what_the_conversion_produced() {
        let interp = run("class Count extends int {\n\
                          op init(x) { super.init(x) }\n\
                          }\n\
                          final n = Count(\"42\")\n");

        let Some(Value::Instance(id)) = global(&interp, "n") else {
            panic!("`n` should be an instance");
        };
        assert_eq!(interp.heap.instance(id).payload, Some(Value::Int(42)));
    }

    /// An implicit `op init` is the inherited conversion run as one, so the payload
    /// it stores has to be the converted value rather than the argument — the same
    /// assertion as for an explicit `super.init`, reached without writing one.
    #[test]
    fn declaring_no_op_init_still_converts() {
        let interp = run("class Count extends int {}\nfinal n = Count(\"42\")\n");

        let Some(Value::Instance(id)) = global(&interp, "n") else {
            panic!("`n` should be an instance");
        };
        assert_eq!(interp.heap.instance(id).payload, Some(Value::Int(42)));
    }

    /// Equality and hashing are one decision, and this is the half a corpus case
    /// cannot state: two keys that compare equal must reach the same bucket, so the
    /// dict has to end up with one entry rather than two that happen to print alike.
    #[test]
    fn a_payload_hashes_as_the_value_it_equals() {
        let interp = run("class Username extends string {}\n\
                          final d = {}\n\
                          d[Username(\"marc\")] = 1\n\
                          d[\"marc\"] = 2\n");

        let Some(Value::Dict(id)) = global(&interp, "d") else {
            panic!("`d` should be a dict");
        };
        let dict = interp.heap.dict(id);
        assert_eq!(dict.len(), 1, "an equal key must not make a second entry");
        assert_eq!(
            dict.get(&Key::Str(Rc::from("marc"))),
            Some(&Value::Int(2)),
            "the second write should have replaced the first"
        );
    }

    /// A subclass gets its payload from the ancestor that has one, however far up
    /// the chain that is, because `super`'s receiver is always the original `self`.
    #[test]
    fn a_payload_is_written_through_an_inherited_init() {
        let interp = run("class Email extends string {\n\
                          op init(s) { super.init(s) }\n\
                          }\n\
                          class Work extends Email {}\n\
                          final e = Work(\"a@b.com\")\n");

        let Some(Value::Instance(id)) = global(&interp, "e") else {
            panic!("`e` should be an instance");
        };
        assert_eq!(
            interp.heap.instance(id).payload,
            Some(Value::from("a@b.com"))
        );
        // The subclass, not the class whose `init` ran.
        assert_eq!(
            interp.heap.class(interp.heap.instance(id).class).name,
            "Work"
        );
    }

    #[test]
    fn truncation_rejects_what_no_int_can_hold() {
        let span = Span::new(0, 1);

        assert_eq!(checked_trunc(3.7, span), Ok(3));
        assert_eq!(checked_trunc(-3.7, span), Ok(-3));
        assert_eq!(checked_trunc(-0.5, span), Ok(0));

        // `as` would answer these with a saturated bound, silently. The boundary
        // is worth pinning in both directions because it is not symmetric, and
        // the asymmetry is easy to get wrong: this test caught a `>` that should
        // have been `>=` and was quietly saturating 2^63 to `i64::MAX`.
        assert_eq!(
            checked_trunc(i64::MAX as f64, span).unwrap_err().kind,
            ErrorKind::Overflow,
            "i64::MAX as f64 rounds up to 2^63, which is out of range"
        );
        assert_eq!(
            checked_trunc(9223372036854774784.0, span),
            Ok(9223372036854774784),
            "the largest float below 2^63 is in range and must convert"
        );
        assert_eq!(
            checked_trunc(i64::MIN as f64, span),
            Ok(i64::MIN),
            "-2^63 is exact as an f64, so the low bound converts"
        );
        assert_eq!(
            checked_trunc(f64::INFINITY, span).unwrap_err().kind,
            ErrorKind::Overflow
        );
        // A NaN is not out of range, it is not a number at all — which is a
        // different mistake, and gets a different kind.
        assert_eq!(
            checked_trunc(f64::NAN, span).unwrap_err().kind,
            ErrorKind::Value
        );
    }

    #[test]
    fn a_conversion_separates_the_wrong_type_from_the_wrong_value() {
        // The whole reason `ErrorKind::Value` exists. Both of these are `int`
        // refusing an argument, but one is fixed at the call and the other is
        // fixed wherever the string came from.
        let cases = [
            ("int([1])", ErrorKind::Type),
            ("int(nil)", ErrorKind::Type),
            ("int(\"abc\")", ErrorKind::Value),
            ("float(\"abc\")", ErrorKind::Value),
        ];

        for (source, expected) in cases {
            let program = crate::compile(source).expect("should parse");
            let mut interp = Interp::with_output(Box::new(Vec::new()));
            let err = interp.run(&program).expect_err("should be refused");
            assert_eq!(err.kind, expected, "{source}");
        }
    }

    #[test]
    fn a_conversion_is_reached_through_the_class_a_name_is_bound_to() {
        // Not a special form in `eval`: `int` is an ordinary global holding an
        // ordinary class, so it converts just as well through another name.
        let interp = run("final make = int\nfinal n = make(\"42\")");
        assert_eq!(global(&interp, "n"), Some(Value::Int(42)));
    }

    #[test]
    fn a_loop_does_not_grow_the_heap_without_bound() {
        // Two allocations an iteration — the scope and the list — so without a
        // collector this settles at several thousand live objects.
        let interp = run("let i = 0\nwhile i < 2000 {\n let x = [1, 2, 3]\n i = i + 1\n}");

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert!(
            interp.heap.live() < 600,
            "heap grew to {} objects",
            interp.heap.live()
        );
    }

    #[test]
    fn a_loop_that_catches_does_not_grow_the_heap() {
        // `catch` does not create the hazard here so much as stop hiding it.
        // Every site that pushes a scope, a temp, or a frame restores it before
        // propagating, but while an error was fatal a site that forgot would leak
        // roots into a process about to exit, where nothing could observe it. A
        // caught error resumes with those stacks still deep, so the same latent
        // bug becomes unbounded growth — which is what this measures.
        let interp = run("let i = 0\n\
             while i < 2000 {\n\
             \x20 try {\n\
             \x20  throw Error(\"x\")\n\
             \x20 } catch e {\n\
             \x20  i = i + 1\n\
             \x20 }\n\
             }");

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert!(
            interp.heap.live() < 600,
            "heap grew to {} objects",
            interp.heap.live()
        );
        // All three stacks back to their depth at the `try`.
        assert!(
            interp.scopes.is_empty(),
            "{} scopes left behind",
            interp.scopes.len()
        );
        assert!(
            interp.temps.is_empty(),
            "{} temps left behind",
            interp.temps.len()
        );
        assert_eq!(interp.depth, 0, "depth left at {}", interp.depth);
    }

    #[test]
    fn a_thrown_payload_survives_the_unwind() {
        // The instance travels inside `QuinceError` through frames that root
        // nothing: no scope and no `temps` entry refers to it for the whole
        // unwind. It survives only because collection happens between statements
        // and unwinding executes none.
        //
        // Churning first puts the heap over its collection threshold, so a safe
        // point crossed on the way out would actually free the payload rather
        // than merely being allowed to. Reading `e.n` afterwards is what would
        // fail. This is the invariant a `finally` would have broken, by running
        // statements during the unwind — see DESIGN.md.
        let interp = run("class Deep extends Error {\n\
             \x20   op init(message, n) {\n\
             \x20       super.init(message)\n\
             \x20       self.n = n\n\
             \x20   }\n\
             }\n\
             fn churn(k) {\n\
             \x20   let scratch = []\n\
             \x20   let i = 0\n\
             \x20   while i < k {\n\
             \x20       scratch.push([i])\n\
             \x20       i = i + 1\n\
             \x20   }\n\
             \x20   return len(scratch)\n\
             }\n\
             fn go(d) {\n\
             \x20   if d <= 0 {\n\
             \x20       throw Deep(\"bottom\", 42)\n\
             \x20   }\n\
             \x20   return go(d - 1)\n\
             }\n\
             churn(3000)\n\
             let got = 0\n\
             try {\n\
             \x20   go(50)\n\
             } catch e {\n\
             \x20   got = e.n\n\
             }");

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert!(
            matches!(global(&interp, "got"), Some(Value::Int(42))),
            "the payload did not survive: got {:?}",
            global(&interp, "got")
        );
    }

    #[test]
    fn a_captured_scope_survives_collection() {
        // The closure is reachable only through `f`, and its captured scope only
        // through the closure. Tracing has to follow both links.
        let interp = run("fn make() {\n\
             let n = [1, 2, 3]\n\
             fn get() { return n }\n\
             return get\n\
             }\n\
             let f = make()\n\
             let i = 0\n\
             while i < 2000 {\n let junk = [0]\n i = i + 1\n }\n\
             let survived = f()");

        assert!(interp.heap.collections > 0, "the collector never ran");
        let Some(Value::List(id)) = global(&interp, "survived") else {
            panic!("the closure did not return its captured list");
        };
        assert_eq!(interp.heap.list(id).len(), 3);
    }

    #[test]
    fn the_iteration_snapshot_survives_the_list_it_came_from() {
        // The first iteration overwrites every element, so the lists the *later*
        // iterations still have to visit are reachable only from the snapshot
        // held in `exec_for`'s Rust frame.
        let interp = run("let items = [[1], [2], [3]]\n\
             let total = 0\n\
             for pair in items {\n\
             items[0] = 0\n\
             items[1] = 0\n\
             items[2] = 0\n\
             let i = 0\n\
             while i < 400 {\n let junk = [0]\n i = i + 1\n }\n\
             total = total + len(pair)\n\
             }");

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "total"), Some(Value::Int(3)));
    }

    /// Churns enough objects to force several collections, then returns `value`.
    fn churn(value: &str) -> String {
        format!(
            "fn churn() {{\n\
             let i = 0\n\
             while i < 3000 {{ let junk = [0]; i = i + 1 }}\n\
             return {value}\n\
             }}\n"
        )
    }

    #[test]
    fn a_list_element_survives_evaluating_a_later_one() {
        // `mk()`'s list lives only in `eval_seq`'s Rust-local `Vec` while
        // `churn()` runs. Unrooted, its slot was reused by a scope and `len`
        // panicked with "expected a list, found Env".
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             {}\
             let pair = [mk(), churn()]\n\
             let kept = len(pair[0])",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    #[test]
    fn an_operand_survives_evaluating_the_other() {
        // Structural equality reads both lists out of the heap, so a collected
        // left operand is a panic rather than a wrong answer.
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             {}\
             let same = mk() == churn()",
            churn("[1, 2, 3]")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "same"), Some(Value::Bool(true)));
    }

    #[test]
    fn the_left_operand_survives_evaluating_the_right() {
        // The path `+` on lists takes, and the reason the rooting had to land
        // before concatenation did.
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             {}\
             let kept = len(mk() + churn())",
            churn("[4]")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(4)));
    }

    #[test]
    fn a_slice_target_survives_evaluating_its_bounds() {
        // The list being sliced sits in a Rust frame while the bounds run, and
        // a bound is an arbitrary expression that can reach a safe point. Pins
        // that `Slice` goes through `eval_seq` rather than hand-rolling it.
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3, 4] }}\n\
             {}\
             let kept = len(mk()[1:churn()])",
            churn("3")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(2)));
    }

    #[test]
    fn a_bound_method_keeps_its_receiver_alive() {
        // The list is reachable from nowhere but the bound method: no variable
        // names it, and it is not inside any other object. If `trace` skipped
        // the receiver, `push` would later write through a handle whose slot
        // had been reused.
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             let m = mk().push\n\
             {}\
             let junk = churn()\n\
             m(4)",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");

        let Some(Value::BoundMethod(id)) = global(&interp, "m") else {
            panic!("`m` should be a bound method");
        };
        let Value::List(list) = interp.heap.bound_method(id).receiver else {
            panic!("the receiver should be a list");
        };
        assert_eq!(
            interp.heap.list(list).len(),
            4,
            "the push should have landed"
        );
    }

    #[test]
    fn an_instance_survives_its_own_constructor() {
        // Slot 0 is the root, and it stays pointing at the instance because
        // `self` cannot be reassigned. This passed with a `temps` root too, and
        // still passes now that the root is gone — what makes it hold is the
        // resolver rule, which `self_cannot_be_reassigned` pins down.
        let interp = run(&format!(
            "{}\
             class C {{\n\
             op init(n) {{\n\
             self.n = n\n\
             let junk = churn()\n\
             }}\n\
             }}\n\
             let c = C(7)\n\
             let kept = c.n",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
    }

    #[test]
    fn a_class_survives_the_instance_that_is_all_that_names_it() {
        // Nothing refers to the class but the instance's `class` handle: the
        // name it was declared under went out of scope with `mk`. Reaching it
        // is what `type` and every later method lookup depend on.
        let interp = run(&format!(
            "fn mk() {{\n\
             class Hidden {{ fn who() {{ return 42 }} }}\n\
             return Hidden()\n\
             }}\n\
             let obj = mk()\n\
             {}\
             let junk = churn()\n\
             let kept = obj.who()\n\
             let name = type(obj)",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(42)));
        assert_eq!(global(&interp, "name"), Some(Value::from("Hidden")));
    }

    #[test]
    fn a_parent_class_survives_the_subclass_that_names_it() {
        // `Base` goes out of scope with `build`, leaving the subclass's `parent`
        // handle as the only thing that reaches it. Method lookup walks that
        // chain, so losing it turns an inherited call into a panic.
        let interp = run(&format!(
            "fn build() {{\n\
             class Base {{ fn greet() {{ return 42 }} }}\n\
             class Sub extends Base {{}}\n\
             return Sub()\n\
             }}\n\
             let obj = build()\n\
             {}\
             let junk = churn()\n\
             let kept = obj.greet()",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(42)));
    }

    #[test]
    fn the_scope_holding_super_survives_with_the_methods_that_close_over_it() {
        // A subclass's methods close over a scope whose only slot is the parent
        // class. Nothing names that scope, so it is reachable only as a method's
        // captured environment — and `super.speak()` reads straight out of it.
        //
        // No new root was needed for it, which is the point: making `super` a
        // captured local rather than a field on the class means the collector
        // work was already done. This is the test that would notice if that
        // stopped being true — deleting `Function`'s env tracing fails it.
        let interp = run(&format!(
            "fn build() {{\n\
             class Base {{ fn speak() {{ return 1 }} }}\n\
             class Sub extends Base {{ fn speak() {{ return super.speak() + 1 }} }}\n\
             return Sub()\n\
             }}\n\
             let obj = build()\n\
             {}\
             let junk = churn()\n\
             let kept = obj.speak()",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(2)));
    }

    #[test]
    fn a_field_survives_collection_with_its_instance() {
        // The list is reachable only through the field, so this is the instance
        // half of what `Dict::trace` already does for a dict.
        let interp = run(&format!(
            "class Box {{ op init() {{ self.items = [1, 2, 3] }} }}\n\
             let b = Box()\n\
             {}\
             let junk = churn()\n\
             let kept = len(b.items)",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    /// The payload half of the same guarantee, which needs a payload that is
    /// actually a handle: a string is an `Rc` and a collection cannot touch it, so
    /// only a list or dict ancestor puts anything at risk. The payload is not a
    /// field, so `Dict::trace` does not cover it and `Instance::trace` must.
    #[test]
    fn a_payload_survives_collection_with_its_instance() {
        let interp = run(&format!(
            "class Bag extends dict {{ op init(d) {{ super.init(d) }} }}\n\
             let b = Bag({{\"a\": 1, \"b\": 2}})\n\
             {}\
             let junk = churn()\n\
             let kept = b.keys()",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        let Some(Value::List(id)) = global(&interp, "kept") else {
            panic!("`keys` returns a list");
        };
        assert_eq!(interp.heap.list(id).len(), 2);
    }

    #[test]
    fn a_method_held_in_a_field_survives_evaluating_the_arguments() {
        // A field holding a function is reachable only through that field, and
        // an argument is free to overwrite it. A *method* is safe without this
        // — the rooted receiver reaches its class, and the class its methods —
        // so the hazard belongs to fields alone. The closure has to be a local
        // one: a top-level `fn` is a global, and globals are always rooted.
        let interp = run(&format!(
            "class Holder {{}}\n\
             fn build() {{\n\
             fn seven(n) {{ return 7 }}\n\
             let h = Holder()\n\
             h.f = seven\n\
             return h\n\
             }}\n\
             {}\
             fn clear(h) {{\n\
             h.f = nil\n\
             return churn()\n\
             }}\n\
             let h = build()\n\
             let kept = h.f(clear(h))",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
    }

    #[test]
    fn a_receiver_survives_evaluating_the_arguments() {
        // The receiver exists only in `eval_method_call`'s Rust frame while the
        // argument runs, and evaluating an argument reaches a safe point. A
        // dict receiver rather than a list because `remove` returns something
        // drawn from it, so a collected receiver is a wrong answer and not only
        // a panic.
        let interp = run(&format!(
            "fn mk() {{ return {{\"k\": 7}} }}\n\
             {}\
             let got = mk().remove(churn())",
            churn("\"k\"")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "got"), Some(Value::Int(7)));
    }

    #[test]
    fn an_argument_survives_evaluating_a_later_argument() {
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             fn first(a, b) {{ return a }}\n\
             {}\
             let kept = len(first(mk(), churn()))",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    #[test]
    fn the_callee_survives_evaluating_the_arguments() {
        // A closure built by an expression is reachable from nowhere but the
        // Rust frame until the call actually begins.
        let interp = run(&format!(
            "fn make() {{ fn id(x) {{ return x }} return id }}\n\
             {}\
             let kept = make()(churn())",
            churn("7")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(7)));
    }

    #[test]
    fn a_dict_entry_survives_evaluating_a_later_one() {
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             {}\
             let d = {{\"a\": mk(), \"b\": churn()}}\n\
             let kept = len(d[\"a\"])",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    #[test]
    fn an_assigned_value_survives_evaluating_its_target() {
        // `xs[churn()] = mk()` evaluates the value first, then the target.
        let interp = run(&format!(
            "fn mk() {{ return [1, 2, 3] }}\n\
             {}\
             let xs = [0, 0]\n\
             xs[churn()] = mk()\n\
             let kept = len(xs[1])",
            churn("1")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    #[test]
    fn a_dict_survives_collection_with_its_contents() {
        let interp = run(&format!(
            "let d = {{\"kept\": [1, 2, 3]}}\n\
             {}\
             let ignored = churn()\n\
             let kept = len(d[\"kept\"])",
            churn("0")
        ));

        assert!(interp.heap.collections > 0, "the collector never ran");
        assert_eq!(global(&interp, "kept"), Some(Value::Int(3)));
    }

    #[test]
    fn an_unreachable_recursive_function_is_collected() {
        // A function whose scope holds the function: the cycle that rules out
        // reference counting. Redefining `f` should still reclaim the old one.
        let interp = run("let i = 0\nwhile i < 2000 {\n fn f() { return f }\n i = i + 1\n}");

        assert!(
            interp.heap.live() < 600,
            "heap grew to {} objects",
            interp.heap.live()
        );
    }
}

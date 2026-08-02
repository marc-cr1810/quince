//! The tree-walking evaluator.
//!
//! One `Interp`, one `impl` block, ten files. The struct and its lifecycle are
//! here; each of the others takes one job the evaluator has:
//!
//! | file | what it answers |
//! |---|---|
//! | [`exec`] | running a statement |
//! | [`eval`] | evaluating an expression, reading and writing a name |
//! | [`index`] | `x[i]`, `x[i] = v`, `x[a:b]` |
//! | [`call`] | calling a function, a method, a native, a class |
//! | [`object`] | finding a member on a value |
//! | [`ops`] | the operators, and the questions the language asks without a call |
//! | [`module`] | `import`, and the two kinds of module |
//! | [`error`] | `throw`, `catch`, and the reports the evaluator raises itself |
//! | [`show`] | rendering a value, which can run a program's `op string` |
//!
//! The split is by *question* rather than by node type, because that is the seam
//! the milestones after v0.6 cut along: v0.7's assignment checks are one file,
//! v0.8's overload dispatch is one file, v0.10's pattern matching is one more
//! beside `exec`. Splitting by AST node would have put each of those in all of
//! them.
//!
//! Nothing here is a second `Interp`. A method sits in the file that names the job
//! it does, and Rust puts the pieces back together.

pub mod call;
pub mod error;
pub mod eval;
pub mod exec;
pub mod index;
pub mod module;
pub mod object;
pub mod ops;
pub mod show;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::builtins::{BUILTINS, stdlib};
use crate::error::{ERROR_KINDS, ErrorKind, ModuleSource, Result};
use crate::runtime::class::BUILTINS as BUILTIN_TYPES;
use crate::runtime::env::Globals;
use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{Slot, Stmt, StmtKind};
use crate::syntax::token::TokenKind;

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
            .spawn_scoped(scope, || {
                FLOOR.set(here());
                f()
            })
            .expect("should be able to spawn the interpreter thread")
            .join()
            // Propagates the original panic instead of wrapping it, so a
            // failure reads the same as it would have without the thread.
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

/// How much of [`STACK_SIZE`] a program may spend before recursion is refused.
///
/// The rest is what the descent between two checks needs, plus unwinding and
/// building the report. Only a call is checked, and one Quince call is a dozen
/// native frames whose size nobody is tracking, so the margin is where the
/// imprecision goes. Four mebibytes is roughly a hundred times the worst step
/// measured.
const STACK_RESERVE: usize = 4 * 1024 * 1024;

thread_local! {
    /// Where the stack the interpreter was given began, or 0 if it was not given
    /// one.
    ///
    /// Set by [`with_stack`], which is the only place that knows how large the
    /// stack is — and knowing that is the whole point. An interpreter driven
    /// without it still has [`MAX_DEPTH`], which is what everything had before
    /// this existed.
    static FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Roughly where the stack pointer is now.
///
/// The address of a local, which is within a frame or two of the real thing —
/// and a frame or two does not matter against a reserve measured in mebibytes.
#[inline(never)]
fn here() -> usize {
    let anchor = 0u8;
    &anchor as *const u8 as usize
}

/// Whether recursing again would risk running out of stack.
///
/// The counterpart to [`MAX_DEPTH`], and needed because that is a count while
/// this is the thing the count stands for. What a call costs in native stack
/// depends on the shape of the expression it sits in: `f()` inside a `return` is
/// a handful of frames, while a call inside a printed value is that plus the
/// renderer, and the second reaches the end of the stack in far fewer calls. A
/// counter tuned for the cheap shape cannot see that; an address can.
///
/// Downward growth is assumed, and `saturating_sub` is what makes the assumption
/// harmless rather than wrong: a platform growing the other way reports no use at
/// all and leaves `MAX_DEPTH` in charge.
fn out_of_stack() -> bool {
    let floor = FLOOR.get();
    if floor == 0 {
        return false;
    }
    floor.saturating_sub(here()) > STACK_SIZE - STACK_RESERVE
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
/// The error hierarchy's root, as Quince rather than as Rust.
///
/// Public so that the inference pass can read it the way it reads any other
/// program. `Error` takes a message and every listed kind extends it, and the
/// editor should not learn that from a second copy written somewhere else — the
/// point of declaring it in Quince was that there is one statement of it.
pub const BASE_ERROR: &str = "\
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

/// Where a file is in its loading.
///
/// Two states and not three: a module that failed to load is *removed*, so a
/// later import of it tries again and fails the same way rather than being told
/// it is part of a cycle.
///
/// Both carry the scope, and `Loading` carries it for a reason worth naming: a
/// module's top-level statements run through `exec`, which is the collector's
/// safe point, and nothing else refers to that scope while it is being filled.
/// A `Loading` state without a handle would be a scope collected out from under
/// the file still executing into it.
enum ModuleState {
    Loading(ObjId),
    Loaded(ObjId),
}

impl ModuleState {
    fn scope(&self) -> ObjId {
        match self {
            ModuleState::Loading(id) | ModuleState::Loaded(id) => *id,
        }
    }
}

/// What to call a file in a report — its name, not the path it was found at.
///
/// A report that named the full path would be different on every machine, which
/// is the property the corpus already depends on for the starting file.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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
    pub(crate) temps: Vec<Value>,
    /// The class whose method is running, one entry per Quince call in progress.
    ///
    /// What a visibility check asks "who is reaching". A plain `fn` pushes
    /// `None`, which is what makes top-level code an outsider to every class —
    /// and the stack is what makes a method called *from* another class's method
    /// answer for itself rather than for its caller.
    ///
    /// Pushed only for a [`Value::Function`] call. A native never reaches a
    /// member through the dot, so there is nothing for it to be inside of.
    reaching: Vec<Option<ObjId>>,
    /// Whether an optional link in the postfix chain being evaluated found
    /// `nil`, and so whether the rest of that chain is to be skipped.
    ///
    /// `a?.b.c` short-circuits as a whole rather than link by link, which is the
    /// only reading under which it means what it looks like — the alternative
    /// raises on reaching `.c` of a `nil`. Every postfix node checks this before
    /// doing its own work, and `ExprKind::Chain` clears it, which is what bounds
    /// "the rest of the chain" to the chain that contained the `?.`.
    ///
    /// A flag rather than a sentinel `Value`, because a sentinel would be a
    /// variant every match in the evaluator has to answer for in order to
    /// describe a state no program can observe.
    short_circuit: bool,
    depth: usize,
    pub(crate) out: Box<dyn Write>,
    /// Where `io.line` reads from.
    ///
    /// Injected for the same reason `out` is: a test that cannot supply input
    /// cannot check what a program does with it, and `io.line` would have been
    /// the one member of the library with no case behind it. The corpus feeds a
    /// case its `.in` file through here.
    input: Box<dyn BufRead>,
    /// The generator `random` draws from.
    ///
    /// Here rather than in a static so that two interpreters in one process do
    /// not share a stream — the corpus runs cases in parallel, and a global
    /// generator would make every one of them depend on the order they ran in.
    ///
    /// Seeded to a fixed number, so a program is reproducible unless it says
    /// otherwise. See `stdlib::RANDOM`.
    rng: u64,
    /// Methods `extend` added, keyed by the class they were added to and the
    /// name they were added under.
    ///
    /// Deliberately *not* inserted into `Class::methods`. C# 14's extension
    /// members resolve statically and never touch the type, so an extension is
    /// visible only where its namespace was imported. Quince cannot copy the
    /// mechanism — `x.example()` has no compile-time receiver type, which is what
    /// being dynamically typed means — but the *scoping* is separable from the
    /// resolution, and keeping it reachable costs one lookup on a miss. Writing
    /// into `methods` is faster and shorter and forecloses it permanently: once
    /// an extension is indistinguishable from a declared method, there is nothing
    /// left for a module to scope.
    ///
    /// A root, because the functions in here are heap objects reachable from
    /// nowhere else — the class does not hold them, which is the entire point.
    extensions: HashMap<(ObjId, String), Value>,
    /// Every stdlib module built so far, keyed by its name.
    ///
    /// A cache, and the thing that makes a module *one* object: `import math` in
    /// two places has to hand back the same scope, or two copies of `math.pi`
    /// would exist and `math` imported twice would not equal itself.
    ///
    /// A root, for the same reason `extensions` is one — between the build and
    /// the binding there is nothing else holding it, and after the binding it is
    /// held only by whichever modules imported it, which is not the same as
    /// being held forever.
    stdlib_modules: HashMap<&'static str, ObjId>,
    /// Every file loaded as a module, keyed by its path as the filesystem
    /// resolved it.
    ///
    /// `Loading` while its statements are running, which is the whole of cycle
    /// detection: reaching an entry that is still `Loading` means the import
    /// chain came back to a file it had not finished.
    ///
    /// Rooted like `stdlib_modules`, and for the same reason.
    files: HashMap<PathBuf, ModuleState>,
    /// The chain of files currently being loaded, outermost first, so a cycle
    /// can be reported as the path it took rather than as the one file that
    /// closed it.
    loading: Vec<PathBuf>,
    /// The text of each loaded module, for the reports raised inside it.
    ///
    /// Keyed by the module's scope rather than its path, because the frame that
    /// tags an error has a scope in hand and would have to walk back to a path.
    /// See [`QuinceError::in_module`].
    module_sources: HashMap<ObjId, Rc<ModuleSource>>,
    /// The top-level names each loaded module declared but did not export.
    ///
    /// Read off the module's AST before it runs, rather than recorded as each
    /// declaration executes: visibility is a property of the declaration and not
    /// of the binding, and a `private fn` inside an `if` that never ran is still
    /// not exported. Keyed by scope, as `module_sources` is, because the lookup
    /// that consults it has one in hand.
    ///
    /// Only file modules appear. A stdlib module is a static table with no
    /// visibility words in it, so everything it declares is exported.
    module_private: HashMap<ObjId, HashSet<String>>,
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
        Interp::with_io(
            Box::new(std::io::stdout()),
            Box::new(std::io::BufReader::new(std::io::stdin())),
        )
    }

    /// Output is injected so tests can capture what a program prints.
    ///
    /// Input is left empty, which is what a case that never reads should see:
    /// `io.line` answers `nil` at end of input, so a program that reads by
    /// mistake gets that rather than a terminal the test cannot reach.
    pub fn with_output(out: Box<dyn Write>) -> Self {
        Interp::with_io(out, Box::new(std::io::empty()))
    }

    /// Both halves injected, for the cases that feed a program its input.
    pub fn with_io(out: Box<dyn Write>, input: Box<dyn BufRead>) -> Self {
        let mut heap = Heap::new();
        let globals = heap.alloc(Object::Globals(Globals::new()));
        for native in BUILTINS {
            heap.globals_mut(globals)
                .declare(native.name, Value::Native(native), false);
        }
        // The types themselves, so a program can name one: print it, reach a
        // method through it, and name it in an `extend`. Immutable, exactly as
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
            reaching: Vec::new(),
            short_circuit: false,
            depth: 0,
            out,
            input,
            rng: stdlib::DEFAULT_SEED,
            extensions: HashMap::new(),
            stdlib_modules: HashMap::new(),
            files: HashMap::new(),
            loading: Vec::new(),
            module_sources: HashMap::new(),
            module_private: HashMap::new(),
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
        // `ERROR_KINDS` lists the catchable kinds and only those, so every name
        // it asks for is there. `only_the_uncatchable_kinds_are_unlisted` is what
        // keeps that true.
        let base = ErrorKind::Runtime.code();
        let mut source = String::from(BASE_ERROR);
        for kind in ERROR_KINDS {
            let name = kind.class_name().expect("a listed kind names a class");
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
                let name = kind.class_name().expect("a listed kind names a class");
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

    pub fn run(&mut self, program: &[Stmt]) -> Result<()> {
        for stmt in program {
            self.exec(stmt, self.globals)?;
        }
        Ok(())
    }

    /// Evaluates a program, returning the value of a trailing expression so the
    /// REPL can echo it.
    pub fn run_repl(&mut self, program: &[Stmt]) -> Result<Option<Value>> {
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

    /// Reads one line of the program's input, for `io.line`.
    ///
    /// On [`Interp`] because `input` is private and a native has the interpreter
    /// and nothing else.
    pub fn read_line(&mut self, into: &mut String) -> std::io::Result<usize> {
        self.input.read_line(into)
    }

    /// The next number from the program's generator, for `random`.
    pub fn next_random(&mut self) -> u64 {
        stdlib::next_u64(&mut self.rng)
    }

    /// Restarts the generator, for `random.seed`.
    pub fn set_seed(&mut self, seed: u64) {
        self.rng = seed;
    }

    /// Tells the starting module which file it was read from, which is what its
    /// imports resolve against.
    ///
    /// Set by the caller rather than taken by `new`, because the REPL has no
    /// file and reads a program that was typed. Its imports resolve against the
    /// working directory, which is the only answer that means anything for input
    /// from a terminal.
    pub fn set_path(&mut self, path: PathBuf) {
        self.heap.globals_mut(self.globals).set_path(path);
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
        let mut roots = Vec::with_capacity(
            self.scopes.len()
                + self.temps.len()
                + self.extensions.len()
                + self.stdlib_modules.len()
                + 1,
        );
        roots.push(self.globals);
        roots.extend(&self.scopes);
        roots.extend(self.temps.iter().filter_map(Value::handle));
        // The one root the interpreter holds that nothing else can reach: an
        // extension's function is not in the class's table, so the class does not
        // keep it alive. Its captured scope comes along through the function.
        roots.extend(self.extensions.values().filter_map(Value::handle));
        // A module is reachable from whoever imported it, but the cache outlives
        // any particular importer — and `from math import floor` binds the
        // function without binding the module it came from, which leaves the
        // scope holding it reachable from here and nowhere else.
        roots.extend(self.stdlib_modules.values());
        // A loaded file is reachable from whoever imported it, but only while
        // something still holds the module value — `from util import helper`
        // binds the function and not the scope it came from, and the registry is
        // what keeps that scope alive so a second import gets the same one. A
        // file still loading is reachable from nothing at all.
        roots.extend(self.files.values().map(ModuleState::scope));
        self.heap.collect(&roots);
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

use std::io::Write;
use std::rc::Rc;

use crate::ast::Slot;
use crate::ast::{BinaryOp, Block, Expr, ExprKind, LogicalOp, Stmt, StmtKind, UnaryOp, Var};
use crate::dict::{Dict, Key, NotAKey};
use crate::env::{self, AssignError, Env, Globals};
use crate::error::QuinceError;
use crate::heap::{Heap, ObjId, Object};
use crate::token::Span;
use crate::value::{Function, Native, Value};

/// Guards against a runaway recursion taking the process down with a native
/// stack overflow, which a language should never expose to its users.
const MAX_DEPTH: usize = 250;

/// Why a statement stopped executing.
enum Flow {
    Normal,
    Return(Value),
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
        Interp {
            heap,
            globals,
            scopes: Vec::new(),
            temps: Vec::new(),
            depth: 0,
            out,
        }
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
                mutable,
                slot,
            } => {
                let value = self.eval(value, env)?;
                self.bind(slot, name, value, *mutable, env);
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

            StmtKind::Block(block) => self.exec_block(block, env),
        }
    }

    fn exec_for(
        &mut self,
        slot: &Option<Slot>,
        iter: &Expr,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow, QuinceError> {
        let iterable = self.eval(iter, env)?;
        let items = match iterable {
            // Snapshotted, so mutating the collection inside the loop cannot
            // invalidate the iteration.
            Value::List(id) => self.heap.list(id).clone(),
            // A dict iterates over its keys, as in Python. Its values are the
            // half you can already reach, through `d[k]`.
            Value::Dict(id) => self.heap.dict(id).keys().collect(),
            other => {
                return Err(QuinceError::new(
                    format!("cannot iterate over {}", other.type_name()),
                    iter.span,
                ));
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
                    dict.insert(key_of(&pair[0], expr.span)?, pair[1].clone());
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

            ExprKind::Field { target, name } => {
                let value = self.eval(target, env)?;
                Err(QuinceError::new(
                    format!("{} has no field `{name}`", value.type_name()),
                    expr.span,
                ))
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
                                    format!("cannot assign to constant `{name}`"),
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

                match &collection {
                    // Assigning to a missing key inserts it, where assigning
                    // past the end of a list stays an error: a list's indices
                    // are positions, and there is no meaningful gap to fill.
                    Value::Dict(id) => {
                        let key = key_of(&index, target.span)?;
                        self.heap.dict_mut(*id).insert(key, value.clone());
                    }
                    _ => {
                        let (id, offset) = self.list_index(&collection, &index, target.span)?;
                        self.heap.list_mut(id)[offset] = value.clone();
                    }
                }
                Ok(value)
            }

            // The parser only admits assignable targets, so this is a field.
            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
            )),
        }
    }

    /// Reads `target[index]`, dispatching on what is being subscripted.
    fn index_get(&self, target: &Value, index: &Value, span: Span) -> Result<Value, QuinceError> {
        match target {
            Value::Dict(id) => {
                let key = key_of(index, span)?;
                self.heap.dict(*id).get(&key).cloned().ok_or_else(|| {
                    QuinceError::new(
                        format!("key {} is not in the dict", index.repr(&self.heap)),
                        span,
                    )
                })
            }
            _ => {
                let (id, offset) = self.list_index(target, index, span)?;
                Ok(self.heap.list(id)[offset].clone())
            }
        }
    }

    /// Resolves a list subscript, accepting Python-style negative indices.
    fn list_index(
        &self,
        target: &Value,
        index: &Value,
        span: Span,
    ) -> Result<(ObjId, usize), QuinceError> {
        let Value::List(id) = target else {
            return Err(QuinceError::new(
                format!("cannot index {}", target.type_name()),
                span,
            ));
        };
        let Value::Int(raw) = index else {
            return Err(QuinceError::new(
                format!("list index must be an int, found {}", index.type_name()),
                span,
            ));
        };

        let len = self.heap.list(*id).len();
        let offset = if *raw < 0 { *raw + len as i64 } else { *raw };
        if offset < 0 || offset >= len as i64 {
            return Err(QuinceError::new(
                format!("index {raw} is out of range for a list of length {len}"),
                span,
            ));
        }
        Ok((*id, offset as usize))
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
                    ));
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

            other => Err(QuinceError::new(
                format!("{} is not callable", other.type_name()),
                span,
            )),
        }
    }

    // -- operators ---------------------------------------------------------

    fn unary(&self, op: UnaryOp, value: Value, span: Span) -> Result<Value, QuinceError> {
        match (op, value) {
            (UnaryOp::Not, value) => Ok(Value::Bool(!value.is_truthy(&self.heap))),
            (UnaryOp::Neg, Value::Int(n)) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| QuinceError::new("integer overflow", span)),
            (UnaryOp::Neg, Value::Float(n)) => Ok(Value::Float(-n)),
            (UnaryOp::Neg, other) => Err(QuinceError::new(
                format!("cannot negate {}", other.type_name()),
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

        // `+` is the one operator shared between numbers and the collections.
        if let (Add, Value::Str(a), Value::Str(b)) = (op, &lhs, &rhs) {
            return Ok(Value::Str(Rc::from(format!("{a}{b}"))));
        }

        // Concatenation builds a new list rather than extending the left one,
        // matching `+` on strings. `push` is there for growing in place.
        if let (Add, Value::List(a), Value::List(b)) = (op, &lhs, &rhs) {
            let mut items = self.heap.list(*a).clone();
            items.extend_from_slice(self.heap.list(*b));
            return Ok(Value::List(self.heap.alloc(Object::List(items))));
        }

        if let (Value::Str(a), Value::Str(b)) = (&lhs, &rhs) {
            return match op {
                Lt => Ok(Value::Bool(a < b)),
                Le => Ok(Value::Bool(a <= b)),
                Gt => Ok(Value::Bool(a > b)),
                Ge => Ok(Value::Bool(a >= b)),
                _ => Err(type_error(op, &lhs, &rhs, span)),
            };
        }

        match (&lhs, &rhs) {
            // Both ints: stay an int, and refuse to wrap on overflow.
            (Value::Int(a), Value::Int(b)) => int_op(op, *a, *b, span),

            // Any float involved promotes the whole operation.
            (Value::Float(_), Value::Int(_))
            | (Value::Int(_), Value::Float(_))
            | (Value::Float(_), Value::Float(_)) => {
                let (a, b) = (as_float(&lhs), as_float(&rhs));
                float_op(op, a, b, span)
            }

            _ => Err(type_error(op, &lhs, &rhs, span)),
        }
    }

    /// `needle in haystack`.
    ///
    /// An unhashable needle is an error rather than a plain `false`, for the
    /// same reason `d[[]]` is: a value that could never have been inserted is a
    /// mistake in the program, and answering `false` would hide it.
    fn contains(&self, haystack: &Value, needle: &Value, span: Span) -> Result<Value, QuinceError> {
        let found = match haystack {
            Value::Dict(id) => self.heap.dict(*id).contains(&key_of(needle, span)?),
            Value::List(id) => self
                .heap
                .list(*id)
                .iter()
                .any(|item| item.equals(needle, &self.heap)),
            Value::Str(text) => match needle {
                Value::Str(part) => text.contains(part.as_ref()),
                other => {
                    return Err(QuinceError::new(
                        format!("cannot look for {} in a string", other.type_name()),
                        span,
                    ));
                }
            },
            other => {
                return Err(QuinceError::new(
                    format!("cannot use `in` on {}", other.type_name()),
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
fn key_of(value: &Value, span: Span) -> Result<Key, QuinceError> {
    Key::from_value(value).map_err(|reason| {
        let message = match reason {
            NotAKey::Unhashable(name) => {
                format!("a {name} cannot be a dict key, because it is compared by identity")
            }
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
    let overflow = || QuinceError::new("integer overflow", span);

    let value = match op {
        Add => Value::Int(a.checked_add(b).ok_or_else(overflow)?),
        Sub => Value::Int(a.checked_sub(b).ok_or_else(overflow)?),
        Mul => Value::Int(a.checked_mul(b).ok_or_else(overflow)?),
        // True division always leaves the integers behind, so `1 / 2` is `0.5`
        // rather than `0`. `//` is there when an int is wanted.
        Div => {
            if b == 0 {
                return Err(QuinceError::new("division by zero", span));
            }
            Value::Float(a as f64 / b as f64)
        }
        FloorDiv => {
            if b == 0 {
                return Err(QuinceError::new("division by zero", span));
            }
            Value::Int(floor_div(a, b).ok_or_else(overflow)?)
        }
        Rem => {
            if b == 0 {
                return Err(QuinceError::new("division by zero", span));
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
        Div if b == 0.0 => return Err(QuinceError::new("division by zero", span)),
        Div => Value::Float(a / b),
        FloorDiv if b == 0.0 => return Err(QuinceError::new("division by zero", span)),
        FloorDiv => Value::Float((a / b).floor()),
        Rem if b == 0.0 => return Err(QuinceError::new("division by zero", span)),
        Rem => Value::Float(a % b),
        Lt => Value::Bool(a < b),
        Le => Value::Bool(a <= b),
        Gt => Value::Bool(a > b),
        Ge => Value::Bool(a >= b),
        Eq | Ne | In => unreachable!("handled before the numeric dispatch"),
    };
    Ok(value)
}

fn type_error(op: BinaryOp, lhs: &Value, rhs: &Value, span: Span) -> QuinceError {
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
        format!("cannot {verb} {} and {}", lhs.type_name(), rhs.type_name()),
        span,
    )
}

fn check_arity(name: &str, expected: usize, found: usize, span: Span) -> Result<(), QuinceError> {
    if expected == found {
        return Ok(());
    }
    let plural = if expected == 1 { "" } else { "s" };
    Err(QuinceError::new(
        format!("`{name}` takes {expected} argument{plural}, but {found} were given"),
        span,
    ))
}

// -- builtins --------------------------------------------------------------

static BUILTINS: &[&Native] = &[&PRINT, &LEN, &TYPE, &PUSH, &KEYS, &VALUES, &REMOVE];

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
    func: |interp, args, span| match &args[0] {
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::List(id) => Ok(Value::Int(interp.heap.list(*id).len() as i64)),
        Value::Dict(id) => Ok(Value::Int(interp.heap.dict(*id).len() as i64)),
        other => Err(QuinceError::new(
            format!("`len` does not apply to {}", other.type_name()),
            span,
        )),
    },
};

/// The in-place counterpart to `+`, which builds a new list.
///
/// A free function rather than a method because there is no method dispatch
/// yet; when strings and lists grow methods this becomes `xs.push(x)`.
static PUSH: Native = Native {
    name: "push",
    arity: Some(2),
    func: |interp, args, span| match &args[0] {
        Value::List(id) => {
            interp.heap.list_mut(*id).push(args[1].clone());
            Ok(Value::Nil)
        }
        other => Err(QuinceError::new(
            format!("`push` needs a list, but was given {}", other.type_name()),
            span,
        )),
    },
};

static KEYS: Native = Native {
    name: "keys",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let keys: Vec<_> = interp.heap.dict(*id).keys().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(keys))))
        }
        other => Err(QuinceError::new(
            format!("`keys` needs a dict, but was given {}", other.type_name()),
            span,
        )),
    },
};

static VALUES: Native = Native {
    name: "values",
    arity: Some(1),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let values: Vec<_> = interp.heap.dict(*id).values().cloned().collect();
            Ok(Value::List(interp.heap.alloc(Object::List(values))))
        }
        other => Err(QuinceError::new(
            format!("`values` needs a dict, but was given {}", other.type_name()),
            span,
        )),
    },
};

/// Removing a key that is not there is an error, for the same reason reading one
/// is: silently doing nothing hides the typo that caused it.
static REMOVE: Native = Native {
    name: "remove",
    arity: Some(2),
    func: |interp, args, span| match &args[0] {
        Value::Dict(id) => {
            let key = key_of(&args[1], span)?;
            interp.heap.dict_mut(*id).remove(&key).ok_or_else(|| {
                QuinceError::new(
                    format!("key {} is not in the dict", args[1].repr(&interp.heap)),
                    span,
                )
            })
        }
        other => Err(QuinceError::new(
            format!("`remove` needs a dict, but was given {}", other.type_name()),
            span,
        )),
    },
};

static TYPE: Native = Native {
    name: "type",
    arity: Some(1),
    func: |_interp, args, _span| Ok(Value::Str(Rc::from(args[0].type_name()))),
};

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

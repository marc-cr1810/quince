use std::io::Write;
use std::rc::Rc;

use crate::ast::{BinaryOp, Block, Expr, ExprKind, LogicalOp, Stmt, StmtKind, UnaryOp};
use crate::env::{self, AssignError, Env};
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
        let globals = heap.alloc(Object::Env(Env::new(None)));
        for native in BUILTINS {
            heap.env_mut(globals)
                .declare(native.name, Value::Native(native), false);
        }
        Interp {
            heap,
            globals,
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
        let mut last = None;
        for stmt in program {
            last = match &stmt.kind {
                StmtKind::Expr(expr) => Some(self.eval(expr, self.globals)?),
                _ => {
                    self.exec(stmt, self.globals)?;
                    None
                }
            };
        }
        Ok(last)
    }

    // -- statements --------------------------------------------------------

    fn exec(&mut self, stmt: &Stmt, env: ObjId) -> Result<Flow, QuinceError> {
        match &stmt.kind {
            StmtKind::Expr(expr) => {
                self.eval(expr, env)?;
                Ok(Flow::Normal)
            }

            StmtKind::Let {
                name,
                value,
                mutable,
            } => {
                let value = self.eval(value, env)?;
                self.heap
                    .env_mut(env)
                    .declare(name.clone(), value, *mutable);
                Ok(Flow::Normal)
            }

            StmtKind::Fn(decl) => {
                // Declared before the body ever runs, and closing over the scope
                // it is declared in, so the function can call itself.
                let func = self.heap.alloc(Object::Function(Function {
                    decl: Rc::clone(decl),
                    env,
                }));
                self.heap
                    .env_mut(env)
                    .declare(decl.name.clone(), Value::Function(func), false);
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

            StmtKind::For { var, iter, body } => self.exec_for(var, iter, body, env),

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
        var: &str,
        iter: &Expr,
        body: &Block,
        env: ObjId,
    ) -> Result<Flow, QuinceError> {
        let iterable = self.eval(iter, env)?;
        let items = match iterable {
            // Snapshotted, so mutating the list inside the loop cannot invalidate
            // the iteration.
            Value::List(id) => self.heap.list(id).clone(),
            other => {
                return Err(QuinceError::new(
                    format!("cannot iterate over {}", other.type_name()),
                    iter.span,
                ));
            }
        };

        for item in items {
            // A fresh scope per iteration, so a closure made inside the loop
            // captures that iteration's value rather than sharing one binding.
            let scope = self.heap.alloc(Object::Env(Env::new(Some(env))));
            self.heap.env_mut(scope).declare(var, item, true);
            if let Flow::Return(value) = self.exec_stmts(&body.stmts, scope)? {
                return Ok(Flow::Return(value));
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_block(&mut self, block: &Block, env: ObjId) -> Result<Flow, QuinceError> {
        let scope = self.heap.alloc(Object::Env(Env::new(Some(env))));
        self.exec_stmts(&block.stmts, scope)
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

    fn eval(&mut self, expr: &Expr, env: ObjId) -> Result<Value, QuinceError> {
        match &expr.kind {
            ExprKind::Int(n) => Ok(Value::Int(*n)),
            ExprKind::Float(n) => Ok(Value::Float(*n)),
            ExprKind::Str(s) => Ok(Value::Str(Rc::from(s.as_str()))),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Nil => Ok(Value::Nil),

            ExprKind::Ident(name) => env::lookup(&self.heap, env, name)
                .ok_or_else(|| QuinceError::new(format!("undefined variable `{name}`"), expr.span)),

            ExprKind::List(items) => {
                let values = items
                    .iter()
                    .map(|item| self.eval(item, env))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::List(self.heap.alloc(Object::List(values))))
            }

            ExprKind::Unary { op, rhs } => {
                let value = self.eval(rhs, env)?;
                self.unary(*op, value, expr.span)
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.eval(lhs, env)?;
                let rhs = self.eval(rhs, env)?;
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
                let values = args
                    .iter()
                    .map(|arg| self.eval(arg, env))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call(target, values, expr.span)
            }

            ExprKind::Index { target, index } => {
                let target_value = self.eval(target, env)?;
                let index_value = self.eval(index, env)?;
                let (id, offset) = self.list_index(&target_value, &index_value, expr.span)?;
                Ok(self.heap.list(id)[offset].clone())
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

    fn assign(&mut self, target: &Expr, value: Value, env: ObjId) -> Result<Value, QuinceError> {
        match &target.kind {
            ExprKind::Ident(name) => match env::assign(&mut self.heap, env, name, value.clone()) {
                Ok(()) => Ok(value),
                Err(AssignError::Undefined) => Err(QuinceError::new(
                    format!("undefined variable `{name}`"),
                    target.span,
                )),
                Err(AssignError::Immutable) => Err(QuinceError::new(
                    format!("cannot assign to constant `{name}`"),
                    target.span,
                )),
            },

            ExprKind::Index {
                target: list,
                index,
            } => {
                let list_value = self.eval(list, env)?;
                let index_value = self.eval(index, env)?;
                let (id, offset) = self.list_index(&list_value, &index_value, target.span)?;
                self.heap.list_mut(id)[offset] = value.clone();
                Ok(value)
            }

            // The parser only admits assignable targets, so this is a field.
            _ => Err(QuinceError::new(
                "cannot assign to this expression",
                target.span,
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
                (native.func)(&mut self.heap, &mut self.out, &args, span)
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

                let scope = self.heap.alloc(Object::Env(Env::new(Some(func.env))));
                for (param, arg) in func.decl.params.iter().zip(args) {
                    self.heap
                        .env_mut(scope)
                        .declare(param.name.clone(), arg, true);
                }

                self.depth += 1;
                let result = self.exec_stmts(&func.decl.body.stmts, scope);
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
        &self,
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
            _ => {}
        }

        // `+` is the one operator shared between numbers and strings.
        if let (Add, Value::Str(a), Value::Str(b)) = (op, &lhs, &rhs) {
            return Ok(Value::Str(Rc::from(format!("{a}{b}"))));
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
}

impl Default for Interp {
    fn default() -> Self {
        Interp::new()
    }
}

fn as_float(value: &Value) -> f64 {
    match value {
        Value::Int(n) => *n as f64,
        Value::Float(n) => *n,
        _ => unreachable!("as_float is only reached for numbers"),
    }
}

/// Integer arithmetic. Division truncates toward zero and, like the rest of
/// these, reports overflow rather than wrapping.
fn int_op(op: BinaryOp, a: i64, b: i64, span: Span) -> Result<Value, QuinceError> {
    use BinaryOp::*;
    let overflow = || QuinceError::new("integer overflow", span);

    let value = match op {
        Add => Value::Int(a.checked_add(b).ok_or_else(overflow)?),
        Sub => Value::Int(a.checked_sub(b).ok_or_else(overflow)?),
        Mul => Value::Int(a.checked_mul(b).ok_or_else(overflow)?),
        Div => {
            if b == 0 {
                return Err(QuinceError::new("division by zero", span));
            }
            Value::Int(a.checked_div(b).ok_or_else(overflow)?)
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
        Eq | Ne => unreachable!("equality is handled before dispatch"),
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
        Rem if b == 0.0 => return Err(QuinceError::new("division by zero", span)),
        Rem => Value::Float(a % b),
        Lt => Value::Bool(a < b),
        Le => Value::Bool(a <= b),
        Gt => Value::Bool(a > b),
        Ge => Value::Bool(a >= b),
        Eq | Ne => unreachable!("equality is handled before dispatch"),
    };
    Ok(value)
}

fn type_error(op: BinaryOp, lhs: &Value, rhs: &Value, span: Span) -> QuinceError {
    use BinaryOp::*;
    let verb = match op {
        Add => "add",
        Sub => "subtract",
        Mul => "multiply",
        Div => "divide",
        Rem => "take the remainder of",
        Lt | Le | Gt | Ge => "compare",
        Eq | Ne => unreachable!("equality is defined for every type"),
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

static BUILTINS: &[&Native] = &[&PRINT, &LEN, &TYPE];

static PRINT: Native = Native {
    name: "print",
    arity: None,
    func: |heap, out, args, _span| {
        let parts: Vec<_> = args.iter().map(|value| value.display(heap)).collect();
        writeln!(out, "{}", parts.join(" ")).expect("failed to write output");
        Ok(Value::Nil)
    },
};

static LEN: Native = Native {
    name: "len",
    arity: Some(1),
    func: |heap, _out, args, span| match &args[0] {
        Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
        Value::List(id) => Ok(Value::Int(heap.list(*id).len() as i64)),
        other => Err(QuinceError::new(
            format!("`len` does not apply to {}", other.type_name()),
            span,
        )),
    },
};

static TYPE: Native = Native {
    name: "type",
    arity: Some(1),
    func: |_heap, _out, args, _span| Ok(Value::Str(Rc::from(args[0].type_name()))),
};

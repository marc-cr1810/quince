# Quince — Design

A dynamically-typed scripting language, implemented in Rust.

Successor in spirit to [Zephyr](https://github.com/marc-cr1810/zephyr) (C++17). Same
goals — Python-like readability, functional + OO, async — but rebuilt on a foundation
that makes the runtime safe and the AST work pleasant.

- **Extension:** `.qn`
- **Binary:** `quince`
- **crates.io:** `quince` · **GitHub:** `quince-lang`

## Why Rust

An interpreter is mostly enum-heavy tree manipulation and a hot dispatch loop, which
maps well onto Rust:

- Algebraic enums + exhaustive `match` for AST nodes, values, and opcodes. Adding a
  node type produces a compiler-generated checklist of every site to update.
- No segfaults in the runtime. Memory bugs in a tree-walker or GC surface as
  confusing failures in *user* programs, which is the worst place to debug them.
- Cargo gives testing, benchmarking, and fuzzing for free — a language needs a very
  large test corpus, and this makes that cheap.

The cost is the GC problem (below), which is the one place Rust is more work than C++.

## Object model — arena + handles

The decision that shapes everything else. Rust's borrow checker fights a traditional
mark-and-sweep tracing GC, because cyclic references are exactly what a GC exists to
collect and exactly what Rust dislikes.

**Approach:** allocate all heap objects in an arena (`Vec<Object>`) and refer to them
by index-based handles (`ObjId(u32)`), not by pointer.

```rust
pub struct ObjId(u32);          // a handle, Copy, no borrow
pub struct Heap { objs: Vec<Object> }
```

This sidesteps the borrow checker entirely: handles are `Copy`, cycles are just
integers pointing at each other, and a tracing collector becomes a straightforward
mark phase over the arena. It also keeps objects contiguous (cache-friendly) and makes
the heap trivially serializable for debugging.

Rejected alternatives:
- `Rc<RefCell<T>>` — ergonomic at first, but leaks reference cycles, and every
  cycle-leak is a user-visible bug we can't fix later without a rewrite.
- A `gc`/`gc-arena` crate — viable, but adds a large conceptual dependency at the
  centre of the design before we know our own requirements.

Handles get us to a real collector later without changing the object representation.

### Collection

Mark and sweep, over the arena. A freed slot becomes a hole rather than being
removed, so live handles never shift, and the holes are reused by later
allocations. A collection triggers once the live set passes a threshold, which
then grows to twice the survivors — a program with a genuinely large heap should
not collect on every statement.

The hard part is not the marking, it's the **root set**. A tree-walking
interpreter keeps live values in Rust locals: while `a + b` is evaluating `b`,
the value of `a` exists only as a local variable in `eval`, and no scan of the
heap can find it. Collecting at an arbitrary allocation would free it.

So collection happens at exactly one **safe point** — the top of `exec`, before
each statement. There, the live set is small and nameable:

- the globals scope,
- every scope currently being executed. A callee's scope hangs off the *closure*
  it came from, not off its caller, so the caller's scope is unreachable from the
  callee and each active frame has to be a root of its own. `exec_scoped` is the
  only place a scope is entered, which is what keeps this list complete.
- **every value an expression is holding while a sibling sub-expression runs.**

That last one was originally believed to be a single special case — the snapshot
`for` takes of the list it iterates. It is not, and the reason is worth writing
down, because the mistake is easy to make twice.

Restricting collection to statement boundaries does *not* mean an expression
never sees one. A call is an expression that runs statements, so any
sub-expression that calls a function reaches a safe point, with the results of
its earlier siblings sitting unrooted in an `eval` frame. `[mk(), churn()]` was
enough: `mk()`'s list was collected during `churn()`, its slot reused by a
scope, and the handle left pointing at the wrong kind of object. In a less
lucky allocation order it would have returned a plausible wrong answer instead
of panicking.

The fix is `eval_seq` / `eval_pair`, which every multi-operand form now goes
through — list literals, binary operators, subscripts, call arguments, the
callee, and the value in an indexed assignment. Each roots what it has already
computed for as long as it has more to evaluate.

Two things keep the cost of that down. Only values carrying a handle are
rooted, since an `int` cannot be collected and a string is reference counted
outside the heap; and the two-operand case is written out by hand, because
returning a `Vec` from every arithmetic operation is a heap allocation per
`+`. What is left is about 15% on `fib(25)`, against no measurable cost — a
slight gain, in fact — on an allocation-heavy loop.

Every root has a test that fails when that root alone is removed. This is
checked by actually deleting each one and running the suite, because an
untested collector is indistinguishable from a broken one, and because the bug
above sat behind a green suite for two releases.

The cost of this design is that a tight expression can allocate without ever
collecting; `[[[[…]]]]` is bounded by the source text, so the exposure is a
program's nesting depth rather than its runtime. A bytecode VM would fix all of
this properly, because its operands live on a stack the collector owns rather
than on Rust's. The rooting above is precisely the bookkeeping a VM gets for
free, and having had to write it by hand — after getting it wrong — is the
strongest argument yet for building one.

## Architecture

```
source (.qn)
  → lexer      tokens
  → parser     AST
  → resolver   scope/binding resolution
  → interp     tree-walking evaluator
```

Tree-walking first. A bytecode VM is the likely v2 (see Roadmap), but the AST
interpreter is the fastest path to a language you can actually *use*, and it doubles
as the reference implementation to test the VM against.

### Modules

Flat `src/*.rs`, matching the wrapt layout.

| Module | Responsibility |
|---|---|
| `main.rs` | entry point, wires CLI to the pipeline |
| `cli.rs` | clap definitions — `run`, `repl` |
| `lexer.rs` | source → `Token` stream, tracks spans |
| `token.rs` | `Token`, `TokenKind` |
| `ast.rs` | `Expr`, `Stmt` node definitions |
| `parser.rs` | recursive-descent + Pratt for expression precedence |
| `resolver.rs` | binds every name to a slot, or to the global scope |
| `value.rs` | `Value` enum, `ObjId`, heap object types |
| `heap.rs` | the arena, allocation, mark-and-sweep collection |
| `interp.rs` | tree-walking evaluator |
| `env.rs` | scopes and variable binding |
| `error.rs` | `QuinceError` with spans, user-facing diagnostics |

Hand-written lexer and parser, no parser-generator dependency. For a language whose
syntax we control and will change often, hand-rolled recursive descent stays easier to
evolve and produces far better error messages.

## Resolution

Between parsing and evaluation, every name is rewritten to a `(hops, index)`
pair: walk out `hops` scopes, then read slot `index`. A local scope becomes a
flat `Vec` of slots instead of a `HashMap<String, Binding>`, so reading a
variable stops hashing a string against a chain of maps. Parameters take a
function scope's first slots in order, so a call binds them without consulting
their names at all.

The hop count is only valid because the runtime scope chain mirrors lexical
nesting exactly — one runtime scope per lexical block, no more and no fewer.
That invariant is now load-bearing, and anything that adds a scope has to add
one in both places.

**Globals stay dynamic**, looked up by name. The REPL introduces them a line at
a time, and a program may call a function declared further down the file, so
neither can be pinned to a slot. This is the same split CPython makes, and it is
why an undefined *global* is still a run-time error.

Declarations are collected before a scope's bodies are resolved, so a nested
function can call a sibling declared below it — mutual recursion between nested
functions works, which is how the name-keyed evaluator behaved. The cost is that
a slot can be reached before its `let` has run, which is reported as "used
before it is declared" rather than reading a stale value.

Two errors moved from run time to compile time, and are now caught even in code
that never executes:

- assigning to a `const` local
- declaring the same name twice in one scope

The second is a **language change**: redeclaring used to shadow silently. With
slots the two declarations are separate storage, so a closure created between
them would quietly keep the older one. Making it an error is the restrictive
choice on purpose — an error can be relaxed later, a semantics cannot. Shadowing
across nested scopes is untouched.

The resolver is also the first half of the work a bytecode compiler needs, so
none of it is throwaway if the VM happens.

### Errors are a feature

Every token and AST node carries a `Span` from the start. Error messages are a core
part of a language's UX, and spans are effectively impossible to retrofit — the cost
is trivial up front and enormous later.

## Language sketch

Starting point, to be revised as it gets used:

```
fn greet(name) {
    return "hello, " + name
}

let x = 42
const PI = 3.14159

if x > 10 {
    print(greet("world"))
}

for item in [1, 2, 3] {
    print(item)
}
```

- Dynamic typing, optional annotations later (as Zephyr has)
- `let` / `const` bindings; a name may be declared only once per scope, but may
  shadow one from an enclosing scope
- Braces, not significant whitespace — simpler to parse, fewer edge cases
- `#` line comments, which leaves `//` free for floor division and makes a `#!`
  shebang line a comment for free
- Expression-oriented where practical

### Statement termination

Statements end at a newline; a `;` is accepted but never required. Rather than emit
newline tokens, the lexer records `newline_before` on each token, so line structure is
available to the parser without every match site having to skip newlines.

The classic hazard here is a leading `(` continuing the previous line:

```
let a = b
(c)          // a call to `b`, or a new statement?
```

A `(` or `[` that starts a line is treated as a new statement, never a continuation.
`.` is exempt, so method chains can still be broken across lines:

```
value
    .trim()
    .upper()
```

## Type system

Dynamic, and **strongly** typed — the same combination as Python, and as Zephyr.
Dynamic means values carry their types, not variables. Strong means the runtime
refuses nonsense rather than guessing: `"3" + 4` is an error, never `"34"` or `7`.

The sequencing matters more than the individual choices. Strong-versus-weak is
irreversible — allowing coercion now would make every program written against it a
migration problem later. Annotations are additive and can wait.

**Settled now, because they touch every operation:**

- No implicit coercion between strings, numbers, and bools.
- Numeric promotion within the numeric tower: `int + float` is a `float`. This is not
  weak typing; the two are one kind of thing.
- `int` arithmetic is checked. Overflow is a runtime error, not a silent wrap — the
  same instinct behind Zephyr's overflow protection, for the cost of `checked_add`.
- Comparison follows the same rule: `1 == 1.0` is true, `1 == "1"` is false.
- Two division operators, as in Python 3. `/` is true division and always yields a
  float, so `7 / 2` is `3.5` and even `4 / 2` is `2.0`. `//` floors, and keeps ints
  as ints: `7 // 2` is `3`.
- `//` floors toward negative infinity rather than truncating toward zero, so
  `-7 // 2` is `-4`. Rust's own `/` truncates and its `div_euclid` keeps the
  remainder non-negative, so neither is floor division — it is implemented by hand.
- Division by zero is an error for floats as well as ints, rather than yielding
  infinity — the same instinct as the overflow rule.
- Truthiness is Python's: `nil`, `false`, zero, empty string, and empty list are
  falsy. (Reversible, unlike the rest of this list — Lua and Ruby treat only `nil`
  and `false` as falsy, which surprises people less.)

**Deferred, because they are additive:**

- Optional annotations, enforced at runtime: `let x: int = 5`. This is Zephyr's
  gradual typing, and the `:` token already exists in the lexer waiting for it.
- Sized integers (`i8`…`u64`) with promotion rules. These are the expensive part of
  Zephyr's model — promotion × overflow × every arithmetic operation — and they
  arrive with the annotation system rather than before it.

### Known future conflict: `{`

When dict literals arrive (v0.3), `if x { }` becomes ambiguous — `{` could open the
body or a dict. Rust hit the same problem and solved it by banning struct literals in
condition position. The likely fix here is the same restriction, decided when dicts
land rather than pre-emptively.

## Roadmap

**v0.1 — walking skeleton**
Lex, parse, and evaluate arithmetic; `print`; `let` bindings; a working REPL.
Goal: `quince run hello.qn` prints something.

**Done.** `quince run examples/hello.qn` prints `hello, world`. The evaluator went
past the v0.1 line and covers the whole parsed grammar: control flow, functions,
closures, and lists, which is most of v0.2 and v0.3 as well.

Scopes live in the heap alongside other objects. A closure captures the scope it was
defined in, and that scope holds the closure — so every recursive function is a
cycle, and `Rc<RefCell<Env>>` would leak on essentially all of them.

Garbage collection landed early, out of roadmap order, because it constrains the
shape of the evaluator (see Collection above) and retrofitting a root set is far
worse than growing one. A loop churning six million objects now peaks at 2.9 MB
instead of growing without bound.

The resolver landed next, for the same reason — it changes what a scope *is*,
and every later pass would have had to be rewritten around it. `fib(25)` went
from 0.17s to 0.10s, against CPython's 0.03s on the same machine.

Still missing: `try`/`catch`, dicts, classes, and string methods. Lists have no
concatenation or `append`, so building one incrementally means preallocating and
assigning by index. The REPL is line-at-a-time and continues reading when a parse
fails at end of input, which is a heuristic rather than a real incremental
parser.

Deferred from the lexer, both cheap to add: hex/binary/octal literals (Zephyr has
them) and block comments (whose nesting behaviour is a real decision).
The parser stops at the first error; multi-error recovery needs synchronisation
points and can wait until the grammar stops moving.

**v0.2 — real language**
Control flow (`if`/`while`/`for`), functions, closures, proper scoping.

**v0.3 — data**
Lists, dicts, strings with methods, indexing, iteration.

**v0.4 — objects**
Classes, methods, inheritance, `self`.

**v0.5 — robustness**
`try`/`catch` and span-accurate diagnostics everywhere. GC is done.

**Later**
Bytecode VM, async/await, module system, sized integer types — all things Zephyr has,
deferred until the core is solid.

## Testing

- Unit tests inline per module (lexer, parser).
- A `tests/` corpus of `.qn` programs paired with expected output, run as integration
  tests. This is the suite that matters — it's what catches regressions as the
  evaluator changes shape, and it should grow with every feature.

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
through — list and dict literals, binary operators, subscripts, call arguments,
the callee, and the value in an indexed assignment. Each roots what it has
already computed for as long as it has more to evaluate.

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
| `dict.rs` | insertion-ordered map, and the values admitted as keys |
| `class.rs` | the type every value belongs to, and where its behaviour is found |
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

let counts = {"a": 1, "b": 2}
counts["c"] = 3
for key in counts {
    print(key, counts[key])
}

let all = [1, 2] + [3]
push(all, 4)
if 4 in all { print("built", all) }
```

- Dynamic typing, optional annotations later (as Zephyr has)
- `let` / `const` bindings; a name may be declared only once per scope, but may
  shadow one from an enclosing scope
- Lists and dicts, both mutable and both structurally compared; `in` for membership
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

### Collections

Lists and dicts are both heap objects, both mutable, both compared structurally, and
both falsy when empty.

`+` concatenates lists into a **new** list, leaving both operands alone, exactly as it
does for strings. `push` is the in-place counterpart. Having only one of the two is
what made building a list from a loop require preallocating and assigning by index,
which was ugly enough to be worth fixing before dicts landed.

Dicts keep **insertion order**. It is observable — printing, iterating, and `keys`
all expose it — and a test corpus comparing exact output needs it to be deterministic.
Updating an existing key keeps its original position; only a genuinely new key goes to
the back. Python made both calls the same way. The cost is that removal is linear,
since closing the gap renumbers everything after it, which is the right trade while
lookup is overwhelmingly the common operation.

Iterating a dict yields its **keys**, as in Python. Yielding pairs would need tuples,
which do not exist yet, and the values are already reachable through `d[k]`.

Keys are restricted to `nil`, bools, ints, floats, and strings. Lists, dicts, and
functions are excluded because they are mutable or compared by identity, and a key
that can change out from under the map it is filed in has no good failure mode. That
exclusion pays for itself twice: it also means a key can never hold a handle, so the
collector only traces a dict's values.

Two consequences follow from rules the language had already committed to:

- `1` and `1.0` are the same key, because `1 == 1.0` is true. A lookup that disagreed
  with `==` would be indefensible, so integral floats are folded into int keys.
- `nan` is rejected as a key, because it is not equal to itself and could therefore be
  inserted and never found again. It is an error rather than a silent trap, matching
  the treatment of overflow and division by zero. Infinities are fine.

`in` tests membership and works on all three collections — a dict key, a list element
(structurally), or a substring. An unhashable value on the left of `in d` is an error
rather than `false`, for the same reason `d[[]]` is: answering `false` would hide the
mistake that produced it.

### Strings

**Indexed by character, not by byte.** `len` had already committed the language to
counting characters, so a byte subscript would have made `len` and `[]` describe
different strings — the same kind of contradiction as `1` and `1.0` being separate dict
keys, and no more defensible. `"héllo"[1]` is `é`.

The cost is real and worth naming: storage is UTF-8, so a subscript walks the string.
`while i < len(s) { s[i] }` is quadratic. That is the price of the subscript agreeing
with the length, and it is fixable later behind the same semantics — an index cache, or
a representation that stores its own char boundaries — where a byte subscript could
never have been fixed at all. `chars` returns a list for anyone who wants to pay the
walk once.

The alternative considered was omitting `s[i]` entirely, on the grounds that O(n) hidden
behind O(1) syntax is exactly the sort of lie this document refuses elsewhere, and that
omitting is the reversible direction. It lost to the argument that a scripting language
whose strings cannot be indexed is answering a question nobody asked.

**Slices are clamped; subscripts are checked.** `s[:100]` on a five-character string is
the whole string, not an error, because a slice asks for *at most* that many — clamping
is what lets "take the first n" be written without a length test in front of it. A
single out-of-range subscript stays an error, because it cannot be anything but a
mistake. An inverted range is empty rather than an error, for the same reason.

Negative bounds count from the end, which lists already did.

Lists slice too, with identical rules, and the result is a new list rather than a view.
A view would need borrow tracking that the object model has no way to express, and
would make `xs[1:3].push(9)` mutate `xs` at a distance.

Methods are `chars`, `ends_with`, `join`, `lower`, `replace`, `split`, `starts_with`,
`trim`, and `upper`. `join` takes the separator as receiver — `", ".join(parts)` — which
reads oddly the first time and is right: the separator is what decides how pieces go
together. Splitting on `""` is an error rather than a synonym for `chars`, since they
are different requests and answering one with the other hides the confusion.

### The `{` conflict that did not happen

This section used to predict that dict literals would make `if x { }` ambiguous, and
that Quince would have to copy Rust's fix of banning struct literals in condition
position. Dicts have landed, and no such restriction was needed.

Rust's ambiguity comes from the *shape* of its struct literal: `Name { … }` is a
postfix form, so in `if x { }` the parser genuinely cannot tell whether `x { }` is one
expression or two things. A Quince dict literal is `{ … }` standing alone, with
nothing before it. Once a condition has finished parsing, `{` is neither an infix nor
a postfix operator, so expression parsing always stops there and the block gets it.
`if a == {"k": 1} { }` parses correctly for the same reason: the dict is in operand
position, where a block could never appear.

The two forms compete in exactly one place — the start of a statement — and that is
settled unconditionally in favour of the block, since `statement` dispatches on `{`
before any expression parsing begins. So a bare dict literal cannot be a statement.
That costs nothing (a dict statement discards its own value) but produces a baffling
error, so the parser special-cases it: a `:` where a statement should have ended
reports that a `{` at the start of a statement opens a block, and suggests parentheses.

Worth keeping as a lesson rather than deleting: the anticipated problem was real for
Rust and imaginary here, and deciding it when dicts actually landed — rather than
pre-emptively adopting Rust's restriction — is what avoided importing a limitation
Quince had no reason to have.

## Dispatch — where behaviour lives

Written before the code, which is unusual for this document and deliberate here.
Methods, string operations, and classes all need the same mechanism, and it is far
cheaper to agree on its shape once than to grow three versions of it and reconcile
them. The section directly above is the argument for *not* deciding early; the
difference is that this decision has three known callers rather than one imagined one.

Two questions get conflated, and separating them settles most of the design:

- Can Quince programs define new types? — a language question.
- Can new types be added to the Rust source easily? — an implementation question.

**They are independent, and only the first one matters to users.** Every user-defined
type shares a single Rust representation:

```rust
Value::Instance(ObjId)   // Object::Instance { class: ObjId, fields: Dict }
Value::Class(ObjId)      // Object::Class(Class)
```

Two `Object` variants, and the language above them can define types without limit.
`Dict` is already the right field store, and `trace` for both is a handful of lines.
So the closed enum costs user types nothing, while keeping the property that makes the
collector safe: adding an `Object` variant fails to compile until `trace` handles it.

### The cost that is real

Adding dicts touched eighteen lines naming `Value::Dict` or `Object::Dict` across
`value.rs`, `heap.rs`, and `interp.rs`, outside `dict.rs` itself. None of it was hard;
it was simply everywhere. That is affordable at five builtin types and stops being
affordable the moment behaviour has to be looked up rather than matched on — which is
what a user-defined type is.

The missing indirection is one that methods need anyway: **every value must be able to
name its type as a value.**

```rust
pub enum Class {
    Builtin(&'static BuiltinType),   // known statically, allocates nothing
    User(ObjId),                     // Object::Class, with parent: Option<ObjId>
}

pub struct BuiltinType {
    pub name: &'static str,
    pub methods: &'static [(&'static str, &'static Native)],
}

impl Value {
    pub fn class(&self) -> Class { /* one exhaustive match */ }
}
```

`type_name` collapses into this rather than sitting beside it. Method lookup becomes a
single path: a linear scan of a static slice for builtins — at this size that beats a
`HashMap` and stays const-constructible — and a map lookup walking `parent` for user
classes. Inheritance falls out of the lookup instead of being bolted onto it later,
which is the reason to fix the table's shape *before* classes rather than after.

`NativeFn` already takes `&[Value]`, so a method is a native whose receiver is
`args[0]`. Free functions and methods share one signature, and the table holds the
`Native` that already exists.

### What stays a match

Not everything should move into the table. `handle`, `is_truthy`, `equals`, `display`,
and `trace` are small, total, and exhaustively checked. Turning them into function
pointers would buy nothing — their behaviour genuinely is known statically for every
builtin — and would cost the compile error that appears when a variant is added.
Exhaustiveness is the feature; spending it for symmetry is a bad trade. When user
classes want to override equality or printing, that is one arm inside an existing
match, not a redesign.

Indexing, iteration, `len`, and `in` are the genuine judgment call, since they are
protocols a user class will eventually want to implement. They stay matches until
classes exist, and then gain protocol slots on `Class` alone, leaving the builtin path
untouched.

### The signature that has to break

`NativeFn` receives `&mut Heap` but not the interpreter, so a builtin cannot call back
into Quince. Nothing today needs to. `sort(list, key)`, `map`, `filter`, any
user-supplied comparator, and every method on a user class all do. The parameter wants
to become `&mut Interp`, with output reachable through it.

That change is cheap across seven builtins and expensive across a populated method
table, so it belongs to the same work that introduces the table — not before it, and
not after.

### Sequencing

None of this gets built speculatively. It arrives as the machinery that makes
`x.push(1)` work.

This section originally went further and said `Class::User` should be present from the
start, as a variant nothing constructs, so classes would slot into a shape already
built for them. **That did not survive contact.** Written that way it is dead code: no
constructor, no caller, and nothing that fails if it is wrong. A commit whose content
cannot be reviewed — because nothing exercises it — is worse than the later diff it was
meant to avoid, and it contradicts the paragraph directly above it.

So `Value::class()` returns a `&'static BuiltinType` today, and the `Class` enum arrives
with user classes. The cost is one signature change in v0.4: `class()` will need `&Heap`
too, because an instance stores its class as a handle. That is affordable precisely
because the indirection exists — there is one call site, in method dispatch.

The general lesson is the one the `{` section already records, arriving from the other
direction: a design written ahead of the code is worth having, and is still a
prediction. Where it turned out to be wrong it gets corrected, not quietly satisfied.

### What landed

Methods on builtin types work. `push` belongs to `list`; `keys`, `values`, and `remove`
belong to `dict`; and they are no longer globals — the doc comment on `push` had
promised that since it was written.

`x.m(a)` is fused: it dispatches without allocating, because that form is overwhelmingly
the common one. A bare `x.m` allocates a `BoundMethod` holding the receiver, which makes
a method an ordinary value rather than syntax that only works in call position, and
which the collector has to trace — in `[1, 2].push` the bound method is the only thing
keeping the list alive.

The receiver counts as `args[0]`, so a method's declared arity is one more than the call
site writes. Arity errors subtract it back out; reporting the declared number would ask
for an argument that has no syntax.

### The recursion limit was never a guarantee

Adding the method-call arm raised the native stack one Quince call frame consumes by
roughly half, in a debug build, with no change to the size of `Value` or `Object`. That
was enough to turn the recursion-limit case from a clean error into a SIGSEGV — which
exposed something older and worse than the regression itself.

`MAX_DEPTH` promised, in its own doc comment, to keep a runaway recursion from taking
the process down with a native stack overflow. It could not keep that promise. It is a
count of interpreter frames; whether those frames fit is a question about a stack nobody
had chosen. A Linux main thread gets 8 MiB and the limit fires comfortably. A spawned
thread gets 2 MiB. Under musl the default is 128 KiB, where `quince run` aborted with no
diagnostic at all — and with `ulimit -s 256`, so did glibc.

The fix is to stop inheriting that number. `with_stack` runs the pipeline on a thread
sized by `STACK_SIZE`, so `MAX_DEPTH` is calibrated against a known quantity on every
platform and in both profiles. It wraps parsing and resolution as well as evaluation,
since recursive descent recurses per nesting level and dropping a deeply nested AST
recurses even when nothing else does.

Two numbers that must stay in step are a standing hazard, so a test holds them there: it
runs a non-terminating recursion from a 128 KiB thread and requires the limit to report
it. That test fails by aborting the process, which is the correct volume for this
failure.

The margin is deliberately large — 250 frames measured under 3 MiB, `STACK_SIZE` is
16 MiB. Thread stacks are reserved lazily, so overshooting costs nothing that matters,
and the thing being defended against is a frame size that moves when someone edits a
match arm in `eval` for reasons that have nothing to do with recursion. It moved by half
once already, which is how this was found.

## Classes

The prediction above held. `Class` arrived as the two-variant enum it was sketched as,
`class()` took its `&Heap`, and inheritance will hang off `Class::method` rather than
being bolted beside it. What the sketch understated is the reach of `type_name`: `class()`
did have one call site, but the name it produces has twenty-six, so widening it touched
every error message that mentions a type, plus three free functions that had never needed
the heap before. Mechanical, and an afternoon of nothing — but "one call site" was the
wrong thing to have counted.

### `self` is a parameter the parser writes

The receiver is implicit at the call site and at the declaration — `fn init(x, y)`, not
`fn init(self, x, y)` — and the parser closes the gap by inserting `self` as parameter
zero of every method. Below the parser there is no such thing as a method: the resolver
gives `self` a slot like any parameter, `read` has no special case, and a closure nested
in a method captures the receiver through the ordinary scope chain because the receiver
is an ordinary local in an enclosing scope.

The alternative — binding `self` in the evaluator when a method is called — needs a rule
for what `self` means inside a nested function, and either answer is a new mechanism.
Here the question does not arise.

`self` is still a keyword, and that buys exactly one thing: using it outside a method is
a resolver error naming the mistake, rather than `undefined variable \`self\`` naming the
symptom. It costs the ability to have a variable called `self`, which is not a loss.

### One receiver convention, two kinds of method

A builtin method takes its receiver as `args[0]`. A user method takes it as slot zero of
its scope. These are the same thing: `call_method` prepends the receiver and hands the
whole list to `call`, which binds arguments to slots in order. So the arity subtraction
that natives already needed applies unchanged, and `BoundMethod` holds a `Value` rather
than a `&'static Native` — one bound-method object, both kinds of callee.

That is also why `Point.dist(p)` works. Reached through the class rather than an
instance, a method is handed back unbound, and it really is a function whose first
parameter is written out.

### What a class does not get

Fields are created by assignment, never declared, so `init` is the only reason an
instance has any. Fields shadow methods, as in Python: a field is per-object and a
method is per-class, so the more specific one wins — and a field holding a function is
called without a receiver it was never written to take.

Instances compare by identity. Structural equality would mean deciding that an object is
its fields, which stops being true the moment one of them is mutable. `is_truthy`,
`display`, and the indexing and iteration protocols stay matches on `Value` with one
instance arm each; letting a class override them is the protocol-slot work, and it
arrives whole or not at all.

`init` cannot return anything useful, because the instance already exists by the time it
runs. That has a collector consequence: `self` is an assignable parameter, so a body that
writes `self = nil` drops the only heap-visible reference to the object under
construction, and `call` has to root it across the constructor to hand it back.

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

Dicts came next, which finished the data half of v0.3 along with list `+`, `push`,
and `in`. Adding them turned up a use-after-free in the collector that had been
there since it landed — see Collection — so the root set grew to cover intermediate
expression values at the same time.

Still missing: `try`/`catch`. `push`, `keys`, `values`, and `remove`
began as free functions standing in for methods; dispatch landed and they moved onto
their types, leaving `print`, `len`, and `type` as the only globals. There are no
tuples, which is why iterating a dict yields keys rather than pairs. The REPL is
line-at-a-time and continues reading when a parse fails at end of input, which is a
heuristic rather than a real incremental parser.

Deferred from the lexer, both cheap to add: hex/binary/octal literals (Zephyr has
them) and block comments (whose nesting behaviour is a real decision).
The parser stops at the first error; multi-error recovery needs synchronisation
points and can wait until the grammar stops moving.

**v0.2 — real language**
Control flow (`if`/`while`/`for`), functions, closures, proper scoping.

**v0.3 — data**
Lists, dicts, strings with methods, indexing, iteration.

Lists and dicts are done, with indexing, iteration, concatenation, and membership.
Dispatch is done too — see Dispatch above — so `list` and `dict` have methods and the
globals that stood in for them are gone.

Strings are done too — indexing, slicing, and nine methods — so **v0.3 is complete**,
with one asterisk: iterating a dict yields keys rather than key/value pairs, because
there is no tuple to yield. That waits on tuples, not on anything in this milestone.
See Strings above for what the character-versus-byte decision cost and bought.

Slicing was the only part that was not just filling in the table: it needed a `Slice`
node, since there is no range value in the language and inventing one to carry two
optional ints would have been the worse trade.

**v0.4 — objects**
Classes, methods, inheritance, `self`.

Classes, methods, fields, and `self` are done — see Classes above. A class is a value:
callable to build an instance, storable in a list, passable to a function. Inheritance
is the remaining piece.

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

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
| `repl.rs` | the interactive prompt — editing, highlighting, completion |
| `color.rs` | ANSI styles, and the decision to emit them at all |
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

- reassigning a `final` or `const` local
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
final PI = 3.14159

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
- `let` / `final` / `const` bindings; a name may be declared only once per scope,
  but may shadow one from an enclosing scope
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

It is a parameter with one exception: it cannot be reassigned. `self.x = 1` is fine —
only the name is pinned, exactly the `final` distinction — but `self = nil` is refused,
because the receiver is not a binding the method owns. Rebinding it can only ever be a
mistake, and the mistake was previously silent until some later line failed with a
message about `nil` that named neither `self` nor the assignment that caused it.

`Param::receiver` carries this rather than a comparison against the name `self`. The
comparison would be correct today, since `self` is a keyword and so the parser is the
only source of a parameter with that name — but that is an invariant living in the lexer
and silently assumed in the resolver. The parser knows which parameter it invented.

### One receiver convention, two kinds of method

A builtin method takes its receiver as `args[0]`. A user method takes it as slot zero of
its scope. These are the same thing: `call_method` prepends the receiver and hands the
whole list to `call`, which binds arguments to slots in order. So the arity subtraction
that natives already needed applies unchanged, and `BoundMethod` holds a `Value` rather
than a `&'static Native` — one bound-method object, both kinds of callee.

That is also why `Point.dist(p)` works. Reached through the class rather than an
instance, a method is handed back unbound, and it really is a function whose first
parameter is written out.

### Inheritance, and where `super` lives

`class Dog extends Animal`. The first spelling tried was `class Dog < Animal`, which
Lox uses and which needs no new keyword — and which reads, in a language that already
has `<` as a comparison, like `Dog` is *less than* `Animal`. A reserved word is the
cheaper of the two costs.

Overriding is not implemented so much as fallen out of. `UserClass::method` walks the
parent chain and returns the first table holding the name, so a subclass shadows what it
redefines and inherits what it does not. `init` goes through the same lookup, which is
why a subclass that declares none uses its parent's. The loop needs no cycle guard: a
parent is evaluated before the class naming it is bound, so `class A extends A` is an
undefined variable and a chain can only ever point at classes that already exist.

`super` is the interesting half. It needs two things that come from different places —
the class to start searching from, and the receiver to bind the result to — and the
receiver is just the enclosing method's `self`. For the class, a subclass's methods are
closed over a one-slot scope holding the parent, built when the class is declared.

That choice pays twice. `super` becomes an ordinary local, so a closure nested in a
method reaches it through the same chain that carries `self`, with no rule to write
down. And the collector needed no new root for it: a captured scope is already kept
alive by the function that captured it. Storing the parent on the `Class` and consulting
a call stack instead would have been a new mechanism and a new root, to reach the same
place.

The lookup deliberately starts *at* the parent rather than at the parent's class of the
receiver. `Dog.speak` calling `super.speak()` must not find `Dog.speak` again.

`super` on its own is not an expression — the parser requires `super.name`. There is
nothing useful to do with the parent class as a bare value that naming the class
directly would not do better, and demanding the `.name` puts the error on the `super`
instead of somewhere downstream.

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
runs. It also needs no root: the instance sits in slot 0 of the constructor's scope, which
`exec_scoped` roots for the whole body, and slot 0 keeps naming it because `self` cannot
be reassigned.

That is worth stating as a dependency rather than a coincidence. `call` used to push the
instance onto `temps`, precisely because a body writing `self = nil` would drop the only
heap-visible reference to the object under construction. Pinning `self` removed the
hazard, so the root went with it — a language rule doing the work a defensive root was
doing before. `self_cannot_be_reassigned` in `resolver.rs` is what holds the other end up.

## Bindings — `let`, `final`, `const`

Three keywords answering two questions: may the name be pointed somewhere else, and may
the object it names be changed. `let` allows both, `final` allows only the second,
`const` allows neither.

The keyword that used to mean `final` was called `const`, and the rename came out of
noticing that it promised something it never delivered:

```
let ys = [1, 2]
const xs = ys
ys.push(3)      # xs is now [1, 2, 3]
```

`xs` changed without ever appearing on the left of an assignment. A word meaning
"constant" that a two-line program can falsify is worse than no word, so the
binding-only form took the name that only ever claimed to bind: `final`.

### Freezing is a property of the object

That left `const` free to mean what it says, and only one implementation of it is
coherent. Constness cannot live on the binding, because a binding is not where mutation
happens; it has to live on the object. So `const xs = ys` freezes the list `ys` names,
and `ys.push(3)` now fails too — a variable that never said `const`, refused because of a
line elsewhere that mentioned it once.

That is genuinely surprising, and it is the honest option. The alternative is to copy,
which breaks identity and *still* is not deep, since the copy's elements are the original's
elements. Rust reaches the other answer — `let xs = vec![]; xs.push(1)` really is an error
there — but only because mutability is a property of the access path and ownership tracks
every path. That is the entire borrow checker, and it is not a thing to bolt onto a
dynamic language. Freezing is at least monotone: an object never thaws, so it can only
surprise once.

Deep, for the same reason: an immutable list of mutable lists is not an immutable value.
The walk is the collector's — freeze on pop, skip what is already frozen — which is what
makes a cyclic structure terminate.

### Freezing follows data, not code

`reachable_data` in `heap.rs` is deliberately not `trace`. A closure's captured scope is
shared with whatever created it, and at the top level it *is* the globals, so following
`Function::env` would let one `const` freeze an unrelated function's locals — or every
binding in the program. A function reached from a frozen list keeps working; what is
frozen is the list's ability to stop pointing at it. The same holds for a class reached
from an instance: fields are data, methods are not.

Two exhaustive matches over `Object` now exist, and a new variant has to answer both.
They ask different questions — "what does this keep alive" and "what does this own" — and
the temptation to write the second in terms of the first is exactly the bug.

### Enforcement is in the type, not in the callers

`list_mut`, `dict_mut`, and `instance_mut` return `Result<_, Frozen>`, and `get_mut` is
private. There are five places in the evaluator that mutate a heap object, which is few
enough to check by hand and far too many to keep checking by hand for the rest of the
project. A `pub fn get_mut` returning `&mut Object` would make `const` advisory the first
time someone added a sixth.

The error names `const` rather than only saying "frozen", because freezing has exactly
one cause and the reader's next question is always what did this. The value it names may
be several steps from the `const` that froze it — that is what deep means.

## The REPL

`rustyline` handles line editing, history, and the terminal. Hand-rolling that would be
the one place in this project where writing it ourselves buys nothing: there is no design
being expressed in cursor movement and termios, unlike the grammar, where owning the code
is the whole point. The dependency stops at the terminal — everything about what the input
*means* stays ours.

### The parser decides when input is finished

`impl Validator for QuinceHelper {}` is empty, and deliberately so. rustyline's validator
hook wants to answer "is this input complete" from the text alone, which in practice means
counting delimiters — a second parser, worse than the real one, disagreeing with it at the
edges. The decision instead lives at the `compile` call in the loop: if parsing fails at a
span at or past the end of the buffer, the input is unfinished and the prompt reads another
line; a failure anywhere earlier is a real error, reported against the accumulated buffer.

Only the parser knows what complete means, and this keeps exactly one thing knowing it. It
is still a heuristic rather than an incremental parser, and the residual wart is honest: an
error that genuinely occurs at the end of the input is indistinguishable from an unfinished
one, so the prompt keeps asking for more. ctrl-c clears the buffer, which is the escape
hatch, and the reason it clears rather than exits.

### Brace counting drives only cosmetics

`count_open_braces` sets the indent rustyline pre-fills on a continuation line, and the
matching auto-dedent when a line starts with `}`. It walks characters and is blind to
braces inside strings and comments, so `"{"` inflates the indent by one.

That is tolerable precisely because of the split above. The counter can only ever produce
wrong *indentation*; it can never produce a wrong parse, because nothing semantic asks it
anything. Keeping the sloppy counter and the exact decision in separate jobs is what makes
the sloppy one safe to keep. The same blindness is in `find_matching_brackets`, with the
same justification — a bracket highlighted in the wrong place is a cosmetic bug.

### Highlighting re-lexes, and degrades to a prefix

The highlighter runs the real lexer over the line on every keystroke. When the line does
not lex — and it will not, constantly, since an open quote is a lex error until it closes —
it retries on the text before the error span rather than giving up. Everything typed so far
keeps its colour instead of the line flopping to plain the moment a string is opened.

Re-lexing per keystroke is affordable because a line is short and a human is slow. This is
one of the few places where the eventual bytecode VM changes nothing.

### Completion cannot borrow the interpreter

rustyline owns the `Helper`, so the helper cannot hold a reference to the `Interp` the loop
is mutating. Global names are copied into an `Arc<Mutex<Vec<String>>>` once per iteration
instead, which costs a `Vec<String>` per line entered — not per keystroke — and buys
completion that knows about variables defined a moment ago.

Method completion started as a hand-written literal and had drifted before it was a day
old: it offered `pop`, `insert`, `clear`, `slice`, `contains`, and `len`, none of which
exist, along with `to_uppercase` and `to_lowercase`, whose real names are `upper` and
`lower`. It omitted `chars`, `upper`, and `lower`, which are real. Those are Rust's names,
which is the tell — the list was written from memory rather than read off the types.

It now derives from `class::BUILTINS`, so completion cannot name a method the language
does not have. This is the Bindings argument in a different costume: a duplicate that has
to be kept in step by hand will not be, and the fix is to remove the duplicate rather than
to be more careful with it.

What that leaves is a smaller hand-written list — `BUILTINS` itself, since Rust cannot be
asked for every `static BuiltinType`. The trade is deliberate. Forgetting a type there
means its methods go unoffered; forgetting a method name in the old list meant the prompt
lying about the API. A gap is not a lie, and types are added once where methods are added
constantly. A test asserts `BUILTINS` covers every type `Value::class` can return.

### Meta-commands

Commands begin with `:` and are recognised only when the buffer is empty, so a line
continuing an expression is never intercepted:

| Command | Effect |
|---|---|
| `:help` | list the commands |
| `:vars` | globals with their values and types |
| `:type <expr>` | evaluate, report the type, discard the value |
| `:ast <expr>` | dump the resolved AST |
| `:tokens <expr>` | dump the token stream with spans |
| `:load <file>` | run a file into the current session |
| `:time <expr>` | evaluate and report elapsed time |
| `:clear` | clear the screen and reset the interpreter |

`:ast` and `:tokens` exist because a language implementation's best debugging tool is the
ability to ask what it actually parsed, and putting that behind a rebuild is friction paid
on every question. They dump the pipeline's own structures, so they cannot drift from it.

`:type` and `:time` evaluate their argument, with the side effects that implies — `:time
xs.push(1)` grows the list. `:clear` resets state as well as the screen, which is two jobs
under one name and should probably become two commands the first time someone loses a
session to it.

## Errors as values — `try`, `catch`, `throw`

**Done.** This section was the decision record written before the code, so that what got
built is the thing that was argued for rather than the first thing that compiled. It held:
the unwind discipline needed no changes, reification stayed at the `catch`, and `throw`
landed with it. Two things the sketch did not settle are recorded below — what `throw`
accepts, and what `kind` reifies into.

```
try {
    risky()
} catch e {
    print(e.message)
}
```

`catch e` takes no parentheses, matching `if cond {` and `for x in xs {` — nothing else in
the grammar parenthesises a header, and this should not start.

### Unwinding is already correct, and that is why this is affordable

Three stacks have to be restored when an error unwinds past them: `scopes`, `temps`, and
`depth`. All three already are. Every site that pushes binds the result *before* it pops
rather than propagating with `?` — `exec_scoped` pushes the scope, runs, pops, then returns
the `Result`; `call` does the same around `depth`; `eval_seq` spells out an `Err` arm that
truncates before returning; `eval_pair` truncates before it unwraps `second?`.

That discipline exists for the collector, not for this feature, and it means `catch` needs
no unwinding machinery of its own — by the time a handler runs, all three stacks are back
to their depth at the `try`.

What changes is not the correctness but the *consequence* of getting it wrong. Today an
error is fatal, so a site that forgot to restore would leak roots into a process that is
about to exit, and nothing could observe it. A caught error resumes with those stacks
still deep, and `while true { try { churn() } catch e { } }` turns a latent leak into
unbounded growth. `catch` does not create the hazard; it removes the thing that has been
hiding it. That earns a test in the shape of `a_loop_does_not_grow_the_heap_without_bound`
— catch in a loop, assert the heap stays bounded and all three stacks return to baseline.

### The error becomes a value only if someone catches it

The obvious design is to make every raised error a heap object at the point it is raised.
It is wrong twice over. Half the raise sites hold `&self`, not `&mut self` — `index_get`,
`attr`, `no_attr`, and the free functions `frozen` and `type_error` all take `&Heap` —
so allocating at raise time means widening a dozen signatures to `&mut` for the benefit of
the rare path. And an uncaught error is about to be printed and thrown away, so allocating
for it is work done exclusively for the case that discards it.

So `QuinceError` stays the Rust struct it is while unwinding, and is *reified* into a
Quince value at the `catch`, which is the one place that has `&mut self` and the only place
the value can be observed. Uncaught errors never allocate at all.

Reification needs to know what class to build, which is the one change the propagating type
does need: a `kind` alongside `message` and `span`. Message strings are for humans and
programs should never match on them — a `kind` is what lets `catch` eventually filter, and
retrofitting one after programs are written against message text is not a thing that can be
done quietly.

### `throw`, and why it is not deferred

`try`/`catch` without `throw` is a smaller feature, and shipping it first is tempting. It
is the wrong order. Catch-only means the caught thing can be a builtin type of the
implementation's choosing; adding `throw` afterwards means user values arrive at `catch e`,
and whatever `e` was before has to change to accommodate them. That is a language break
dressed as a feature addition, and the corpus grows every week.

So `Error` is a class, allocated as a `UserClass` at startup and bound as a global. That is
not a compromise for want of better machinery — it is what makes `class MyError extends
Error` work with no new machinery at all, reusing the `extends` chain and method lookup
that v0.4 already built. `QuinceError` carries an optional payload for the user-thrown
case, so a thrown instance is what the handler binds, unchanged and unwrapped.

The cost is honest: `Error` is an ordinary global, so a program can shadow it. That is the
same exposure `print` and `len` already have, and inventing a protected namespace for one
name is worse than the thing it prevents. What a `catch` reifies into is *not* exposed to
it, though: the class handles are captured at startup, before any user code runs, so
rebinding the name changes what `Error(...)` builds and not what a handler binds.

`Error` is defined in Quince rather than in Rust — a nine-line prelude compiled at startup.
That is what makes `class MyError extends Error` need no machinery at all: `init` is found
through the same parent walk a user's own subclass uses, so a subclass declaring no `init`
takes a message, and `Bare("oops").message` works without `Bare` saying anything.

### `throw` takes an instance of `Error`, and nothing else

The sketch left this open, and the case that settled it is `throw 10` followed by a handler
reading `e.message`. Binding whatever was thrown is the transparent choice, and it costs
this: `e` is an `int`, the field access falls through to the int's method table, and the
program dies with ``int has no method `message` `` pointing at the handler — with the
thrown `10` nowhere in the message and the `throw` that caused it several lines away.

So the check is at the `throw`, where the error can name the mistake. It also buys an
invariant worth having: everything a handler binds has a `message` and a `kind`, because
everything it binds extends `Error`. Bind-anything breaks that and makes every handler
defensive about what it just caught.

The objection to refusing — that the check is itself a raise, from inside the machinery that
raises — turned out to be nothing. It is an ordinary raise site with an ordinary kind, and
it is catchable, because nothing about it is reentrant. Python refuses `raise 10` for the
same reason and it is the right call.

That restriction also narrowed the propagating type. A payload can only ever be an instance,
so `QuinceError::payload` is an `Option<ObjId>` rather than an `Option<Value>` — which keeps
`QuinceError` `PartialEq`, which `Value` is not.

### `kind` names a class, and the classes are generated

`kind` had one job in the sketch — tell reification what class to build — and it does that
by naming one: `ErrorKind::Index` reifies into `IndexError`. The classes are generated from
`ERROR_KINDS` at startup as `class IndexError extends Error {}`, so adding a kind cannot
leave its class undeclared. Getting that wrong would otherwise wait until something raised
the new kind and a handler went looking for a global nobody bound.

`class_name` is an exhaustive match, which is what makes this hold: a new variant cannot be
added without naming its class in the same edit.

The instance carries `kind` as a field, and `Error.init` sets it from `type(self)` — which is
already the receiver's class name. So a user's `class ParseError extends Error` that calls
`super.init(message)` reports `ParseError` without the prelude knowing it exists. Reification
sets both fields directly rather than calling `init`, because that path is the runtime
building an object rather than a program asking for one.

Only some of the forty-odd raise sites are classified. `new` fills in `Runtime`, so the rest
kept compiling untouched and read as the base `Error` — a gap rather than a lie, and one that
closes a site at a time. The ones a program is likely to catch are done: type, name,
attribute, index, key, frozen, recursion, zero division, overflow.

### The payload crosses the unwind unrooted

A thrown instance travels inside `QuinceError` through frames that root nothing. It is
reachable from no scope and no `temps` entry for the whole unwind, and it survives only
because collection happens between statements and unwinding executes none. `alloc` does not
collect — deliberately, see Collection — so even the handler's own scope allocation cannot
take the payload out from under it before it is bound.

That is three separate invariants holding one value alive. They are all already true and
none of them are stated anywhere near each other, which is exactly how this breaks later:
the day `alloc` learns to collect, this is the code that fails, and it will fail as a
use-after-free in a `catch` that has nothing obviously to do with allocation. If the payload
ever outlives a single unwind, it goes in `temps` and stops depending on any of it.

### The recursion limit stays catchable

Making one error uncatchable means `catch e` no longer means catch `e`, and every program
using a handler for cleanup acquires a hole in it. The uniform rule is worth more than the
footgun it admits.

And the footgun is mostly Python's, not ours. There, catching `RecursionError` resumes at
the depth where it fired, still one call from the ceiling. Here `depth` is decremented by
every `call` frame the error unwinds through, so a handler at the `try`'s depth resumes at
the `try`'s depth — a `try` at depth 5 around a runaway recursion runs its handler at depth
5, with the whole stack available again. That falls straight out of the unwind discipline
above and is a reason to keep that discipline exactly as it is.

### What this does not bring

**No `finally`.** It earns its keep by releasing things, and the language holds no
releasable resource — no files, no sockets, no locks. It waits for I/O.

The second reason was going to be that adding it means deciding what a `return` inside a
`finally` does to a `return` inside its `try`. That reason does not survive contact with the
field, and the correction is worth recording because it changes what to build later rather
than whether to build it now.

Five languages have `finally`, and the ones that allowed abrupt exit from it all regret it.
Java defines the `finally` block's reason for leaving as *replacing* the try block's, so a
`return` there discards a pending exception silently — SpotBugs and Error Prone both flag it.
JavaScript and Ruby's `ensure` do the same. Python is walking it back: [PEP
765](https://peps.python.org/pep-0765/) withdraws `return`/`break`/`continue` that exit a
`finally`, with CPython emitting a `SyntaxWarning`, on the grounds that the semantics are
surprising — its second attempt, after PEP 601 was rejected. C# is the one that got it right
first: leaving a `finally` by `return`, `break`, or `goto` is a *compile error*, and Swift
forbids the same for `defer`. That rule collapses the whole precedence question to nothing,
and it costs a static check.

So the hard question is one a language may simply refuse to answer, and if `finally` ever
lands here it should refuse it in the resolver. What remains is masking — a `finally` that
throws while an error is already in flight — which every language patched late and none
patched first: Java's `addSuppressed` arrived in 7 with try-with-resources, Python chains
through `__context__`, and JavaScript's `using` proposal reinvented suppression twenty years
after Java. With `kind` in place, Quince's version is a `cause` field and one change to
`report`, cheap if designed alongside `kind` rather than bolted on.

The larger signal is that everyone is migrating *away* from `finally` toward scoped
ownership — C++ RAII, Rust's `Drop`, `defer` in Go, Zig, and Swift, `with` in Python, `using`
in C# and JavaScript. Quince is written in one of the languages that decided a cleanup block
was the manual version of something the type can do once. So when there is a file to close,
the thing to reach for is probably a `with`-shaped form over a `close` protocol slot, landing
on the protocol-slot work rather than as a second mechanism beside it.

Both RAII languages also make a destructor that throws *during* unwinding fatal —
`std::terminate` in C++, abort in Rust. Neither was willing to own two errors in flight at
once, which is the same masking problem seen from the other end.

One honest cost of waiting: `Flow` has two arms today, so the precedence table `finally`
would need has three rows. `break` and `continue` make it five arms, and every one needs an
answer. The table is smallest now, so this is worth revisiting when loop control flow lands
rather than drifting until I/O shows up.

**No typed `catch`.** `catch e` binds everything raised. `catch e: TypeError` is a
*narrowing* of a form that already parses, so it stays addable later without breaking a
line of existing code — which is the same reasoning that made redeclaration an error rather
than a shadow. The `kind` field is what keeps that door open.

**No interaction with `return` whatsoever**, which is worth stating because it looks like
there should be one. `return` travels as `Flow::Return`, a value in the `Ok` channel;
errors travel in the `Err` channel. A `return` inside a `try` passes through the handler
untouched because it is not the kind of thing a handler can see. `finally` is precisely the
feature that would force those two channels to meet, which is a second reason it waits.

### Resolution

`catch e` declares a binding, so the catch block is a scope like any other and `e` is its
first slot — the same treatment `for x in xs` gets. The `try` block is a separate scope,
and its bindings are deliberately not visible to the handler: a `let` inside `try` may not
have run when the error fired, so the handler could otherwise read a slot that was never
written. That is the "used before it is declared" case the resolver already reports, and
keeping the scopes separate means it cannot arise here at all.

Both blocks are real scopes in both the resolver and the evaluator, which is the invariant
from Resolution above — one runtime scope per lexical block, no more and no fewer.

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

Still missing: the protocol slots that would let a user class decide its own truthiness,
printing, equality, indexing, or iteration. `try`/`catch`/`throw` landed in v0.5. `push`, `keys`,
`values`, and `remove` began as free functions standing in for methods; dispatch landed
and they moved onto their types, leaving `print`, `len`, and `type` as the only globals.
There are no tuples, which is why iterating a dict yields keys rather than pairs. The
REPL grew line editing, history, highlighting, completion, and meta-commands on top of
`rustyline` — see The REPL above — but the continuation rule is still the original
heuristic, not an incremental parser.

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

**Done.** See Classes above. A class is a value — callable to build an instance,
storable in a list, passable to a function — and `extends` plus `super` complete the
milestone. What v0.4 does *not* bring is protocol slots: a class cannot yet decide its
own truthiness, printing, equality, indexing, or iteration. Those are one coherent
piece of work and are listed under v0.5 rather than left implied here.

**v0.5 — robustness**
`try`/`catch` and span-accurate diagnostics everywhere. GC is done.

`try`/`catch`/`throw` is **done** — see Errors as values above. It went first of the three,
because it settles what a `QuinceError` *is* before protocol slots start threading one
through `display`, `is_truthy`, and `equals` at every call site. The diagnostics sweep goes
last, when nothing else is still moving spans around.

The unwind discipline turned out to be the whole feature: because every site that pushes a
scope, a temp, or a frame restores it before propagating, `catch` needed no unwinding
machinery of its own. What the milestone actually added was a reason to *test* that
discipline — while an error was fatal, a site that forgot to restore leaked roots into a
process about to exit, where nothing could observe it.
`a_loop_that_catches_does_not_grow_the_heap` is what observes it now.

`a_thrown_payload_survives_the_unwind` guards the other half, and it is the one to keep an
eye on. A thrown instance is rooted by nothing for the whole unwind, alive only because
collection happens between statements and unwinding executes none. The test was checked by
forcing a collection at the `catch` with the payload unrooted, which fails it — so it is a
real guard and not a passing assertion. It is also the test that would fail the day `alloc`
learns to collect, or the day anything runs statements mid-unwind.

An uncaught error reports its kind as `error[IndexError]: …`, in that bracket rather than as
a `TypeError: …` prefix, because this report already borrows rustc's shape — the `-->`, the
gutter, the caret — and rustc puts its code in exactly that position.

The bracket is omitted when it would say nothing: an unclassified error reads `error: …`
exactly as it did before kinds existed, and so does a literal `throw Error("…")`, since
`error[Error]` is noise. That leaves the bracket meaning "the kind is known", which is a
standing nudge to classify the raise sites that still are not. A `throw` reports the class
that was actually thrown, so a `ParseError` says `error[ParseError]` — which is why
`QuinceError` carries a `label` beside its `kind`: the name lives on the instance's class and
`report` has no heap to look it up in, so it is captured at the raise.

What v0.5 still owes: protocol slots, then the diagnostics sweep.

`final` and `const` landed here first, out of order, because they were a rename with a
feature hiding inside it — see Bindings above. Renaming a keyword only gets cheaper the
earlier it happens, and the corpus was 59 files at the time.

The REPL work landed here too, and is robustness of a different kind: it is the surface
where the language gets used before it is finished, and `:ast` and `:tokens` shorten the
loop on every question asked of the parser from here on.

Protocol slots belong here too — the point at which `is_truthy`, `display`, `equals`,
indexing, and iteration stop being closed matches over `Value` and gain one arm that
asks the class. Deferred to a single pass on purpose: doing them one at a time means
five separate decisions about what a class may override and no way to keep them
consistent.

**Later**
Bytecode VM, async/await, module system, sized integer types — all things Zephyr has,
deferred until the core is solid.

## Testing

- Unit tests inline per module (lexer, parser).
- A `tests/` corpus of `.qn` programs paired with expected output, run as integration
  tests. This is the suite that matters — it's what catches regressions as the
  evaluator changes shape, and it should grow with every feature.

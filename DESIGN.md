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
| `stdlib.rs` | the modules the language ships — `math`, `io`, `time`, `random` |
| `lsp.rs` | the language server — diagnostics, completion, hover, symbols |

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

That rule reached exactly one of the five places a name can be declared twice,
and the reason is worth keeping because it is the shape this kind of gap always
has. The check lived in `declare_slot`, which returns early at the top level —
correctly, since a global needs no slot — and a class body is not a scope at all,
so it never went near the function. Four of the five silently kept the second
declaration:

| where | before | now |
| --- | --- | --- |
| `fn` inside a function | error | error |
| `fn` at the top level | second one wins | error |
| a method in a class | second one wins | error |
| an `op` in a class | second one wins | error |
| a `fn` in an `extend` | second one wins | error |

The two new checks sit where each has what it needs. A class or `extend` body is
checked in the parser, which is holding the declarations already — the same
argument that put the `op innit` check there. The top level is checked in the
resolver, against a set of names kept per *resolver* rather than per process: at
a prompt, writing the function again is how you fix it, and each REPL entry is
its own `compile`, so the set starts empty every time.
`a_repl_entry_may_redefine_what_an_earlier_one_declared` is what holds that open.

`fn` and `op` share one table, so `op string` beside `fn string` is the same
collision and gets the same answer. They are not a pair of overloads: one is
reached by `print`, the other by writing `x.string()`, and one name cannot hold
both.

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
all.push(4)
if 4 in all { print("built", all) }
```

- Dynamic typing, optional annotations later (as Zephyr has)
- `let` / `final` / `const` bindings; a name may be declared only once per scope
  — or per class body, or per `extend` body — but may shadow one from an
  enclosing scope
- Lists and dicts, both mutable and both structurally compared; `in` for membership
- Braces, not significant whitespace — simpler to parse, fewer edge cases
- `#` line comments, which leaves `//` free for floor division and makes a `#!`
  shebang line a comment for free
- Expression-oriented where practical

### String literals

Either quote delimits a string, and the two styles differ in nothing else:

```
print('hello' == "hello")   # true
print("it's")               # the other quote is ordinary text inside a literal
print('say "hi"')
print('it\'s')              # both quotes escape in both styles
```

There is no character type, so `'a'` is a one-character string rather than
something else — which is the whole reason both styles can be the same token.
`TokenKind::Str` records no delimiter, so nothing downstream can tell them apart,
and `repr` normalises to double quotes when printing a string inside a collection.

Accepting `\'` inside a double-quoted literal is redundant, but the alternative
is an escape that is an error in one style and not the other, which is a rule
nobody would remember. Moving a literal between styles never breaks it.

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

**The last sentence did not survive either**, and for a reason worth stating: slots on
user classes alone are an asymmetry, and the requirement is the opposite one — a builtin
type and a user-defined type should be able to do the same things, so that `extend int`
is a feature the language can grow into rather than one its object model forbids. See
One class representation below for what replaces this.

The paragraph above it is half wrong too. Overriding printing really is one arm inside
an existing match — but `is_truthy`, `equals`, and `display` are infallible and take
`&Heap`, and calling a user method needs `&mut Interp` and can fail. So all three move
onto `Interp` and start returning `Result`, which is the same break the section below
predicts for `NativeFn`, arriving from the other direction and going unnoticed here.

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

Overriding is not implemented so much as fallen out of. `Class::method` walks the
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

All of that still holds for a class extending nothing, which is what `Value::base` returning
`self` for a payload-less instance preserves. A class extending a builtin is the exception, and
not by overriding anything: the operator reaches the value the object *is*, so it is `string`'s
`==` and `string`'s truthiness doing the work — see The slots below.

`init` cannot return anything useful, because the instance already exists by the time it
runs. A builtin's `init` is the exception and not really one: it is a conversion, there is no
instance, and what it returns is the whole result — see Calling a type converts. It also
needs no root: the instance sits in slot 0 of the constructor's scope, which
`exec_scoped` roots for the whole body, and slot 0 keeps naming it because `self` cannot
be reassigned.

That is worth stating as a dependency rather than a coincidence. `call` used to push the
instance onto `temps`, precisely because a body writing `self = nil` would drop the only
heap-visible reference to the object under construction. Pinning `self` removed the
hazard, so the root went with it — a language rule doing the work a defensive root was
doing before. `self_cannot_be_reassigned` in `resolver.rs` is what holds the other end up.

## One class representation

The requirement that settles this: **a builtin type and a user-defined type should be
able to do the same things.** Not as symmetry for its own sake — it is what makes
`extend int { fn abs() { … } }` a feature the language can grow into later rather than
one the object model has already forbidden.

Dispatch is most of the way there already. `Class::method` hands back a `Value` for both
arms, and `call_method` handles a `Native` and a `Function` identically, so `"hi".upper()`
and `p.dist()` travel the same path today. Three things are not:

- `BuiltinType.methods` is a `&'static` slice, so it cannot grow, by construction.
- a builtin type is not a value. There is no `int` to write, so there is nothing for an
  extension to attach to.
- `Class` being an enum means a protocol slot has to live on one arm or be stated twice.

So the enum collapses. Builtin types become ordinary heap class objects at startup,
seeded from the static tables and bound as globals. `methods` becomes
`HashMap<String, Value>` rather than `HashMap<String, ObjId>`, which is the load-bearing
change: `Value::Native` is a `&'static` pointer holding no handle, so a native method and
a Quince method sit in one table with nothing to reconcile, and `trace` pushes only the
`Function` entries.

The static `BUILTINS` tables stay, as the seed. Adding a builtin type still fails to
compile until it is listed, and `Value::class` stays an exhaustive match — into a table of
`ObjId` rather than to a `&'static`. Exhaustiveness is the property the Dispatch section
defends, and it survives.

### Slots are cached fields, not lookups

The trap: if `is_truthy` becomes "look up `bool` on my class", every `if` in the language
hashes the string `"bool"`. That is the hottest path there is, and it would spend exactly
what the linear-scan decision above was protecting.

So a slot is a field on the class, resolved once when the class is built, and a builtin
whose slot is `None` falls through to the match that already exists — costing nothing
until someone extends it. Inheritance is a copy-down at creation rather than a chain walk
at use, which is safe for the same reason `Class::method`'s loop terminates: a parent
is fully built before the class naming it exists.

### The guard was already there

Once `int` is a global holding a class, `int.bool = fn() { … }` looks expressible, and it
would make `0` truthy program-wide. The first plan was to ship builtin classes frozen.

The guard protects nothing. Assigning to a field of a class — *any* class, one a program
declared included — already raises `cannot set a field on class`. Freezing would have been
a check on a path nothing can reach: the same dead weight as the declaration-time arity
check written for `op` and deleted before it shipped, and caught the same way, by probing
instead of reasoning.

The refusal is load-bearing in the other direction. Because `=` cannot reach a class,
extension has to arrive as a deliberate construct rather than as a side effect of
assignment — which is what lets it validate anything at all.

### What the collapse settled

Three decisions it forced, none of them recoverable from the diff:

- **Calling a type was refused.** `int(5)` would otherwise build an `Object::Instance` whose
  class is `int`: an object reporting a builtin type name that no arithmetic path accepts
  and no other int resembles. `cannot construct \`int\`` instead — a placeholder held open
  for the conversion that has since replaced it, below.
- **`nil` and `class` are not bound.** Both are keywords, so nothing could ever read a
  global under those names. The skip is taken from `TokenKind::keyword` rather than written
  out, so a type name that becomes a keyword later self-corrects. Their class objects exist
  and answer method calls; they cannot be spelled, and so cannot be extended — meaningless
  for `nil`, and a much larger question for the metaclass.
- **The heap roots its own classes.** `collect` adds them to whatever the caller passes.
  They are reachable from nowhere the caller can see, and a program that never mentions
  `int` still needs `int`'s class the moment a type error asks for a name. The only root
  the heap contributes on its own behalf.

### A type's name belongs to the type

`let string = "x"` used to work, and silently won: a global is bound by name, so the type
was replaced and every later mention of `string` meant a string. Worse, this was already
true of classes a program declared — `class Point {}` then `let Point = 5` stole the name
long before the builtin types were values, and nothing complained.

So a name that belongs to a type is not available to anything else, checked in the resolver
before the program starts. `declare` is the single choke point every binding form passes
through — `let`, `final`, `const`, `fn`, a parameter, a loop variable — so one check covers
all of them, and a test names them all to keep it that way.

Two details worth keeping. The resolver reads a statement list's `class` declarations
before any of its bindings, so the collision is refused whichever order the two were
written in; without that pass only the half written second would be caught. And `resolve`
now runs `declare_all` over the top level, which it never did — the top level has no scope,
so `scoped` never reached it, which is exactly why globals were stealable. Slots are
unaffected: `declare_slot` returns early without a scope, because a global is still bound
by name at run time.

The reserved set is flat and never popped, so a class declared inside a function reserves
its name for the rest of the program. Over-broad, deliberately, on `declare`'s own
reasoning: an error can be relaxed later, a semantics cannot.

`print` and `len` stay shadowable. The rule is about the type vocabulary, not about every
name the language happens to bind first — a function is a value a program may reasonably
want to replace, and a type is what `int(5)` and a future `extend int` have to be able to
read without asking what is in scope.

One thing ruled out rather than deferred: `type(x)` keeps returning a **string**. It is
tempting to hand back the class once `int` is a value, but `Error`'s constructor does
`self.kind = type(self)` and the corpus asserts a string, so the change would break
working code for a nicety.

`extends` on a builtin was refused at class-definition time, and that refusal was deferral
rather than a decision — it has since been lifted, below. What it was not is optional. This
case was written here as "semantically empty" before it was implemented, and it was worse
than that: `class MyStr extends string {}` then `MyStr().upper()` reached `string`'s `upper`
— a native that matches on `Value::Str` and treats anything else as `unreachable!` — and
panicked the interpreter. Ordinary Quince code, a hard crash, and it only became
expressible because the types became values: until then `extends string` was an undefined
variable and the hole was closed by accident. Holding it shut in one place rather than
guarding every native is what made lifting it cheap: the refusal became one substitution at
the same choke point, not a guard to unpick in nineteen functions.

### `extend`, and the one thing it must not do

The feature this was all for:

```quince
extend int {
    fn double() { return self * 2 }
}
print(7.double())   # 14
```

`extend` rather than `extends`, which already means inheritance in a class header; reusing
it for a block that declares nothing would be a pun on a keyword that has a job.

Most of it already works. `5.foo` lexes — the number lexer only takes `.` as a decimal
point when a digit follows — and reaches method lookup today, which is what
`int has no method \`foo\`` is. `BoundMethod.receiver` is a `Value`, not a handle, so a
method can bind to an `int`. `self` is argument zero, so a body already runs with a
non-object receiver: `class C { fn twice() { return self * 2 } }` then `C.twice(5)` returns
`10` on a build with no extension support in it at all. The single missing piece was a
method table that can grow, which is what the collapse above delivered.

**But `extend` must not insert into the class's own table.** C# 14's extension members
resolve statically and never touch the type, so an extension is visible only where its
namespace was imported. Quince cannot copy the mechanism — `x.example()` has no
compile-time receiver type, and that is what being dynamically typed means — but the
*scoping* is separable from the resolution, and it is the part worth keeping reachable.
So extensions live in a table keyed by class and name, consulted after the class's own
methods. Identical behaviour today, one extra lookup on a miss, and when the module system
lands that table can become per-module. Mutating `methods` directly is faster and cheaper
to write, and it forecloses scoping permanently: once an extension is indistinguishable
from a declared method there is nothing left to scope. This is the one place worth paying
a lookup to keep an option open.

Three refusals, and `op` is what makes the second one mechanical:

- **Shadowing an existing method.** Every caller that relied on the real one becomes
  silently wrong.
- **Defining an `op`.** `extend int { op bool() { … } }` would change what `if 0` means
  program-wide. An extension may add to a type; it may not change how the language
  dispatches on it.
- **Setting a field.** Already refused — `cannot set a field on int` — and correctly: an
  int has nowhere to put one, and extension should not invent storage.

The collision rule diverges from C#, which silently prefers the real member. That choice is
driven by a BCL that must keep growing without breaking callers; Quince has nine builtin
types with single-digit method counts, so a loud error whose fix is a rename beats a silent
behaviour change. Worth revisiting only if the builtin surface ever gets large.

The honest cost, recorded so it is not rediscovered: until modules exist, `extend int` in
any file changes `int` for the whole program, with no scoping and no undo. That is the
bargain Ruby and Objective-C made. The extension table keeps the door open; it does not
close the gap today.

### What landed, and what the plan above did not say

All of it holds — the keyword, the separate table, the three refusals, and every claim
about what already worked. One sentence above is wrong, though, and it is the one that
sounds most like a conclusion: *"the single missing piece was a method table that can
grow"*. The collapse did deliver one, and `extend` deliberately does not use it. What was
missing was a **second** table, kept apart from the first on purpose. A growable
`Class::methods` is what makes the wrong implementation easy, not what makes the right one
possible.

Four decisions the plan left open, each of which had to be made to write the code:

- **Both walks cover the whole chain, and the methods walk finishes first.** "Consulted
  after the class's own methods" is ambiguous the moment inheritance exists: is an
  extension on `int` consulted before or after a method declared on a subclass of `int`?
  After. A declared method always wins, wherever on the chain it was declared, or adding
  `extend int { fn name() { … } }` could quietly take over a `name` that a class three
  levels down had declared for itself. That ordering is invisible to a corpus case — both
  answers print something plausible — so it is a Rust test.
- **Extending twice with the same name is refused too**, which is the first refusal read
  honestly: an extension replacing an extension is exactly as silent as one replacing a
  method. That covers two `extend` blocks; one *block* declaring a name twice is the
  parser's, and was the gap that turned out to be four gaps — see Resolution above.
- **Every name in a block is checked before any is added.** A block whose third method
  collides leaves the type as it found it. The alternative is a program that reports an
  error *and* changes behaviour, which is the worst of both.
- **No `super` in an extension body.** An extension cannot shadow, so a method it adds is
  never an override, and there is nothing above it for `super` to mean. It falls out of not
  pushing the scope, so there is no check to maintain.

And the thing the plan did not mention at all: **the extension table is a root.** An
extension's function is in no class's table — the whole point — so nothing reachable from
the globals refers to it. Deleting that one line does not degrade anything gracefully; it
panics with `handle points at a collected object` the first time a collection runs between
the `extend` and the call. `an_extension_survives_collection` is what measures it, checked
by deleting the line rather than by reasoning about it.

The `op` refusal is in the parser, not the evaluator, for the reason `op innit` is: the
keyword and its span are in hand before a body is parsed, so the error points at the `op`
itself. It is also the one refusal that cannot be reached at run time — an `op` never gets
as far as the table.

### The two doors, and the four words

Every type above is open, and there are exactly two ways to attach behaviour to one from
outside: a subclass, and an `extend` block. Two doors is four states, and each gets its own
word.

| | inherit | `extend` |
|---|---|---|
| `class Point` | yes | yes |
| `final class Point` | no | yes |
| `complete class Point` | yes | no |
| `sealed class Point` | no | no |

The alternative was two orthogonal modifiers that stack — `final complete class Point` for
the last row — which is one fewer word for the same expressiveness and was rejected on how
it reads. A composite word costs a redundancy that has to be admitted rather than hidden:
`sealed` **is** `final` and `complete` at once, so a reader has three words to learn where
two would do. What it buys is that every common case is one word, and the three sit on a
scale — each of `final` and `complete` closes a door, `sealed` closes both — rather than
being an unordered set of flags.

`Openness` carries the state as the word the program wrote, not as two bools, so a refusal
can quote the modifier back: `sealed` and `final` both close the hierarchy, and a report
that said `final` to a program that wrote `sealed` would be naming a keyword the author
never typed. The two predicates are exhaustive matches, so a fifth variant cannot be added
without answering for both doors — which is the only thing keeping the table above and the
code in step, along with the test that walks it.

**On `final`.** It is a second meaning for a word that already has a job: `final xs = [1, 2]`
pins a name and leaves the list mutable. That is the objection raised against spelling
extension blocks `extends` a section ago, and it has to be answered rather than waved at.
Two things answer it. The meanings are one idea at two levels — **`final` fixes the shape of
a declaration, and `const` freezes data** — and neither `final` says anything about contents:
a `final` binding's list still grows, and a `final` class's instances still take fields. And
it is the pun Java already made, for these two meanings exactly, which is the difference
between a word carrying two senses and a word carrying a surprise.

**On `sealed`.** C# uses it for the hierarchy door alone, so using it for both is a widening
rather than an inversion — nobody reads `sealed` and expects a subclass to work. Java 17 and
Kotlin use it for a *permitted-subclass list*, which is a different feature wearing the same
word; that is a cost, and it is the reason the row it names here is the one where both
answers are no, which is the reading every use of the word shares.

**On what it cost.** One token of lookahead, the only lookahead in the parser, and only for
`final` — it is the one modifier that is also a binding form, so `final class` has to be
told from `final xs = …` before `let_stmt` eats the keyword. `complete` and `sealed`
introduce nothing else and need none. Two new reserved words, which is four edits in
`token.rs`: everything downstream — the REPL completer, the highlighter, the LSP token map
— reads `KEYWORDS`. Adding them is what turned up that `extend` had never been in that list,
so it was highlighted but never offered, and the test that would have caught it iterates the
same list it was missing from.

**Both doors are worth closing, but not equally, and not forever.** A subclass can break an
invariant a class holds about itself — `op eq`, ordering, arithmetic. An extension cannot:
it may not shadow a method, define an `op`, or add storage. So `complete` is the weaker of
the two, and it gets weaker still: when modules land, the extension table becomes
per-module and `extend` stops being a program-wide change. `final` will be wanted regardless;
`complete` may turn out to be the word nobody reaches for. That is the honest reading, and
it is why `final` — not `sealed` — is the one spelled with the word every other language
already uses for it.

Where each refusal lives follows from what it is about. The hierarchy one joins the parent
`match`, beside the one that turns back `class Callable extends function`; the extension one
goes *first* in `may_extend`, ahead of the shadowing and double-extension checks, because it
is the only one of the three about the type rather than about the name being added. A block
whose first method collides with nothing is still refused, and refused for the type's reason.

Both are checked at run time, and not because it was easier. `extends` names an ordinary
variable read in the enclosing scope — a parameter can hold a class — so the resolver has no
answer to give. That the parser demands an identifier there makes a static check look
available; it is not.

Three things a modifier deliberately is not:

- **Not inherited, and not about ancestors.** `inherit_slots` copies a parent's slots down
  and this is not one. All three modifiers allow `extends`: they say what may attach to a
  class from below and beside it, never what it may descend from. The direction that could
  go wrong is upward — `sealed class Dog extends Animal {}` must leave `Animal` open — which
  is what `a_modifier_closes_one_class_and_not_its_ancestors` measures.
- **Never true of a builtin.** `extend int` is the feature the whole class representation was
  collapsed for, and `class MyStr extends string` works. A program cannot redeclare `int`, so
  no syntax reaches one; `Class::builtin` writes `Open` and that is the whole story.
- **Not on `Error`.** The prelude generates `class TypeError extends Error {}` and a program
  is documented to write `class ParseError extends Error`. Closing the error hierarchy for
  tidiness would break both — the second loudly, the first before any program ran at all.
  `try_catch.qn` is what stands in the way.

## Calling a type converts

`int(5)` works, and it needed no new vocabulary. The refusal above was a placeholder, and
the thing it was holding a place for turned out to already exist:

```quince
print(int("42"))     # 42
print(int(3.7))      # 3
print(float(3))      # 3.0
print(string([1, 2]))# [1, 2]
print(bool(0))       # false
print(list())        # []
print(list([1, 2]))  # [1, 2]
```

`Class.init` is an `Option<Value>` and `Value::Native` is a `Value`, so a builtin's seed
table names an init exactly the way it names `upper`. One rule changes: **construction
yields what `init` produced.** A class a program wrote allocates an instance and returns
that, because its init fills in `self` and returns nothing useful; a builtin allocates
nothing and returns the value, because an int has nowhere to keep a field. `Class.builtin`
decides which, and that is now the *only* thing it decides.

A conversion takes no receiver — the value it returns is the whole result — so
`construct_builtin` reaches `call` rather than `call_method`, which is the function that
inserts one. For the same reason a builtin's init is *not* also an entry in `methods`,
unlike a user class's `op init` that `super.init` finds by name: as a method it would be
wrong, and `(5).init(7)` has no meaning to give. The natives are named for their type
rather than for `init`, because that name is what an arity error quotes and `int(1, 2)`
should be told about `int`.

### Conversion is not uniform, and pretending otherwise would cost more

`T(x)` looks like one feature and is not. Four types convert from anything sensible; two
take nothing or a copy of themselves; three cannot be called at all.

| | takes | why |
|---|---|---|
| `int`, `float` | int, float, string, bool | one numeric tower plus parsing |
| `string` | anything | it is `display`; total |
| `bool` | anything | it is `is_truthy`; total |
| `list`, `dict` | nothing, or one of itself | see below |
| `function` | — | there is no value one could be made from |
| `nil`, `class` | — | keywords, so never bound as globals |

`list("ab")` could mean the characters; `list({"a": 1})` could mean the keys, the values, or
the entries. Guessing would bake a wrong answer in permanently, so both are refused with the
method that means the likely thing named in the help. Restricting the argument to *nothing
or a copy* is what makes the constructor unambiguous without answering the question — and
it is the same answer subclassing will need, where `super.init()` on a `list` ancestor means
"start empty".

The copy is shallow, as in Python: `list([inner])` holds the same `inner`, not a copy of it.

### `ValueError`, and why the kind is worth a variant

`int([1])` and `int("abc")` are both `int` refusing an argument, and they are different
mistakes. A list is something `int` never accepts, so the call is wrong: `TypeError`. A
string is exactly what it accepts, and *that* string is not a number, so the call is right
and the data is not: `ValueError`. The fix is at the call site for one and wherever the
string came from for the other, which is worth saying in the label rather than only in the
message.

Two edges are pinned by tests rather than left to `as`. A NaN is a `ValueError` — not out
of range, not a number at all — and a float too large is an `OverflowError`. The bound is
asymmetric and the asymmetry is easy to get wrong: `i64::MIN` is -2^63, which an `f64` holds
exactly, so a float equal to it converts; `i64::MAX` is 2^63-1, which an `f64` cannot hold,
so `i64::MAX as f64` rounds *up* to 2^63 and a float that equal is already out of range. The
first version of the guard used `>` there and silently saturated 2^63 to `i64::MAX` — the
exact failure the guard exists to prevent, found by the test that pins the boundary.

`float("nan")` and `float("inf")` both parse, which is how a NaN enters a program at all
now that `0.0 / 0.0` is an error. Consistent with the dict-key rule already in place, which
refuses a NaN key because it is not equal to itself.

### What conversion settles about bools

`int(true)` is `1`, as in Python, JavaScript and C#. That is not a crack in the rule that a
bool is not a number: `1 + true` stays an error, because nobody asked for a conversion
there. A conversion is a request, and arithmetic is not.

## Extending a builtin

```quince
class Email extends string {
  op init(str) {
    const split = str.split('@')
    if len(split) != 2 { throw Error("Invalid email address") }
    super.init(str)
    self.username = split[0]
    self.domain = split[1]
  }
}

final e = Email('marc@example.com')
print(e.domain)     # example.com
print(e.upper())    # MARC@EXAMPLE.COM
print(type(e))      # Email
```

An instance gained a **payload**: `Option<Value>`, holding the value a builtin ancestor's
`init` produced. `None` for every class that does not descend from a builtin, which is every
class that does not say so.

Not a field, though a field is where a wrapper class would keep it. A field is assignable
and shadowable, so `e.value = 5` could leave an `Email` that is not a string, and `string`'s
methods would then be looking at an int. The payload is reachable from Quince through
exactly one piece of syntax, once.

### `super.init` on a builtin is not a method call

With a user parent, `super.init(x)` runs Quince code against the same `self`, filling
fields. With a builtin parent there is no Quince code and no receiver: `string`'s `init` is
the conversion, which takes the call's arguments and *returns* a string. So the rule is

> `super.init(args…)` with a builtin superclass runs that builtin's conversion on `args…`
> and stores the result as `self`'s payload.

which makes the whole conversion table available through `super.init` at no cost — the
arities are the conversions' arities, and `super.init('abc')` on an `int` ancestor raises the
same `ValueError` as `int('abc')`, at the `super.init` rather than at the constructor call.

It also means the lookup has to be split out rather than falling through: a builtin's `init`
is deliberately not among its methods, because `(5).init(7)` has no meaning to give, so
`Class::method("init")` does not find it and inserting a receiver would be wrong if it did.

Which builtins can be extended follows from this and needs no second list: exactly the ones
that convert. `function` and `class` have no `init`, so `super.init` would have nothing to
call, and `extends function` stays refused with that as the reason. `nil` and `class` cannot
be written after `extends` at all — one is a keyword, the other is not bound as a global.

### One substitution, at the one place that gives a method a receiver

`e.upper()` finds `UPPER` on `string` by walking the parent chain, and `UPPER` matches on
`Value::Str`. The fix is one line at `call_method`, and the discriminator was already there:

- the method is a `Function` → written in Quince → pass the **instance**, because
  `self.domain` is why it was written.
- the method is a `Native` → came from a builtin's seed → pass the **payload**.

Every user method is a function and a native only ever comes from a builtin seed, so those
two cases are exactly "written in Quince" and "written in Rust". Nineteen natives are
untouched, `text()` still treats a non-string as unreachable, and `Value::BoundMethod` and
`super.upper()` come through the same place and so need nothing of their own.

An earlier note in this file claimed lifting the refusal meant touching every native. That
was wrong, and checking rather than reasoning is what corrected it: `args.insert(0, receiver)`
appears once in the interpreter.

The consequence worth stating: **a builtin method returns the base type, not the subclass.**
`type(e.upper())` is `string`. Same as Python, and for a reason — preserving the subclass
would mean re-running `op init`, so `upper()` would re-validate an already-valid address on
every call.

### The check that a payload gets written

An `op init` that forgets `super.init` builds an object that looks finished and fails at the
first method call. Two static rules, both in the resolver, so the class is never stored:

```quince
class Bad extends string {
  op init(str) { self.raw = str }        # `Bad`'s `op init` never calls `super.init`
}
class Stack extends list {}              # `Stack` extends `list` but has no `op init`
class Odd extends string {
  op init(s) { super.init(s) }
  fn reset(s) { super.init(s) }          # `super.init` is only valid inside `op init`
}
```

The second exists because the first has nothing to inspect when no `op init` is declared —
and an inherited one is enough, since it has already been checked. The third is what keeps
the first sound: without it `fn reset() { super.init(…) }` satisfies a scan of the class
while leaving construction empty. It is also right on its own terms, which is why it applies
to a plain user hierarchy too — `super.init` on an object that already finished re-runs a
constructor, whatever the parent is.

Answering the hierarchy questions needs `parents: HashMap<String, String>` and
`inits: HashSet<String>` beside the `types` set, walked with a visited guard: `class A
extends B` with `class B extends A` is a cycle in what was *written*, refused by the
evaluator — a parent is read before the subclass's name is bound — but this walk runs first
and has to terminate on its own.

**Calls are counted, not paths.** The resolver checks that a `super.init` is *written*, not
how many run, so this is accepted:

```quince
class Num extends int {
  op init(x) { if x < 0 { super.init(0) } else { super.init(x) } }
}
```

Refusing it would mean path-sensitive analysis for a rule that has an exact runtime answer
for free: the payload is checked before it is written, so a second `super.init` is refused on
the strength of the first rather than after quietly replacing it. Static "at least one",
runtime "exactly one" — each where it is cheap and precise.

**The runtime backstop cannot be removed.** The static check works on names in one pass, so
`final S = string` followed by `class X extends S` gets past it: `S` is not a class name and
nothing records what it holds. That path reaches a native with no payload, and reports

```
error[TypeError]: `X` was never given a string
  = help: `X` extends `string`, so its `op init` must call `super.init` before it is used
```

rather than panicking inside `text()`. The name walk is wrong in one direction only, by
construction — it can miss a class that owes a `super.init`, never accuse one that does not.

### Declaring no `op init` is what asks for the implicit one

```quince
class Username extends string {}
final u = Username('marc')       # payload "marc"
```

The boilerplate this removes — `op init(str) { super.init(str) }` — said nothing, and the
machinery for it was already in place: a class declaring no `op init` inherits its parent's,
so `Username.init` was *already* `string`'s conversion. Construction was simply misreading it
as a method and inserting a receiver, which is why the arity came out one short. Recognising a
native there and running it as the conversion it is makes construction and `super.init` the
same operation, reached two ways.

Arity comes from the conversion for free, and so does everything else:

```quince
Username()                       # `string` takes 1 argument, but 0 were given
class Stack extends list {}
Stack()                          # payload [] — list()'s 0-argument reading
Stack([1, 2])                     # payload [1, 2] — the copy reading
```

The error names `string` where the call site said `Username`, which is true rather than ideal:
the arity really does come from the conversion. The help line adds the class, rather than the
message being rewritten to hide where the number came from.

**Declaring an `op init` is what takes construction over**, and then `super.init` is owed
again. Not laziness about the check: `op init(a, b)` gives no answer to which argument an
implicit conversion would take, so "declare nothing and you are built as your base" and
"declare `op init` and you say" are the only two coherent positions.

This also deleted the rule that a class extending a builtin must have an `op init` somewhere
in its chain — the case it existed for is now the ordinary one — and with it the resolver's
`inits` set. It closes most of the aliasing hole by accident, too: `final S = string` then
`class X extends S {}` works, because the inheritance is by value and never consulted a name.

## Where an operator finds a payload

**Not the protocol slots.** Nothing here lets a class *decide* an operator's behaviour — `Op`
still has one member, and `class Point { op add(other) { … } }` is not a thing that can be
written. What this does is let a class extending a builtin reach *its base's* operators:
`Username('marc') == 'marc'` works because `string`'s `==` does the comparing, and
`Box(1) + Box(1)` is still refused with no way to change that. The slots came next and are
what lifts that refusal — see What a class may answer for below.

Methods had one choke point. Operators have none: nothing routes through a single place the
way `args.insert(0, receiver)` does. So there is one helper, applied deliberately:

```rust
/// The value an operator should act on: an instance's payload, if it has one.
pub fn base<'a>(&'a self, heap: &'a Heap) -> &'a Value
```

Thirteen call sites — `is_truthy`, `equals`, `display_styled`, `repr_styled` and
`format_pretty` in `value.rs`; `binary`, `unary`, `index_get`, `list_index`, `slice`,
`contains`, the `for` snapshot and `LEN` in `interp.rs`; plus `key_of`. An instance with no
payload gets itself back, which is what leaves every class extending nothing behaving exactly
as it did: compared by identity, always truthy, printed as `<Box instance>`.

```quince
class Username extends string {
    fn email_address() { return self + '@gmail.com' }
}
final u = Username('marc')
print(u)                   # marc
print(u.email_address())   # marc@gmail.com
print(len(u))              # 4
print(u[0])                # m
print(u[1:3])              # ar
print(type(u))             # Username
print(type(u + '!'))       # string
```

`self + '@gmail.com'`, not `self.raw`. There is no `raw` and there should not be: a field is
assignable, so `u.raw = 5` would leave a `Username` that is not a string. A class extending
`string` *is* one, so `self` is how you use it as one.

### Dispatch on the base, report on the class

These are different questions and the code asks them separately:

```quince
print(u - 1)               # cannot subtract Username and int
```

The dispatch needed the base type to decide that subtraction does not apply; the message needs
the name the line was written with to say which value. `type_name` is never unwrapped for the
same reason — `type(u)` is `Username`, which is the whole point of having declared it.

### `==` and hashing are one decision

```quince
print(u == 'marc')             # true
print(u == Username('marc'))   # true  — false before this, by identity
print(u == Slug('marc'))       # true  — transitivity leaves no choice

final d = {}
d[Username('marc')] = 1
d['marc'] = 2
print(len(d))                  # 1
print(d.keys())                # ["marc"]
```

If `u == 'marc'` then the two must hash alike, or a dict holds equal keys in different
buckets. So `equals` and `key_of` cannot be decided separately, and the unwrap goes in
`key_of` rather than `Key::from_value` — which has no heap to reach a payload through, and
whose only non-test caller is `key_of`. `Key` needed no new variant.

Two consequences, both accepted rather than worked around: a subclass's extra fields are
**invisible to `==`**, and a subclass used as a dict key **comes back as its base type**. The
alternative is a type that borrows `string`'s methods and is never equal to a string, which is
a wrapper, not a string. Python has both properties for `str` subclasses.

`repr` follows the same rule, so `[u]` prints `["marc"]` and nothing in the output
distinguishes it from a plain string. `repr` stays the literal you could paste back, and
`type(x)` is how you ask what class something is.

### Two behaviour changes worth naming

Truthiness now follows the payload, where every instance used to be unconditionally true:

```quince
print(bool(Username('')))      # false
if Username('') { }            # does not run
```

That is Python's answer and it follows from being a string, but code using an instance as an
existence check changes meaning.

And a string is **not** iterable in Quince — `chars` is how its characters are reached — so
neither is a class extending one. `for c in u` is refused exactly as `for c in 'marc'` is.
That is consistency rather than a gap; a `list` or `dict` ancestor iterates normally.

### What the tests had to be written around

The payload has no observable value except through what reaches it, so two invariants are
asserted in Rust rather than by printing:

- what `super.init` stored, and what the implicit init stored, since `int`, `float` and `bool`
  have no methods at all.
- that an equal key lands in the *same bucket*. Printing cannot tell one entry from two that
  happen to look alike, so the test reads `dict.len()`.

The collector test uses a `dict` ancestor deliberately: a string payload is an `Rc` rather
than a handle, so a string subclass proves nothing about `Instance::trace`. Written with a
string first, it passed with the trace line deleted.

## `op` marks what the language calls

`init` was a magic string. `class Point { fn innit(x, y) { … } }` compiled to a class with
no constructor and no complaint, because being the constructor was inferred from the name.
Every slot added multiplies that: `fn lenght`, `fn to_str`, `fn equals` — each one an
override that silently is not one.

So a method the language calls is declared with `op` instead of `fn`:

```quince
class Stack {
    op init(items) { self.items = items }
    fn peek()      { return self.items[len(self.items) - 1] }
}
```

`Op` is a closed set and `Op::from_name` is the only way in, so `op innit` is an error
where it is written. It is checked in the parser rather than the resolver because
everything the check needs is local — the name and its span, both in hand before the body
is parsed. `op` at the top level is refused too: there is nothing out there for one to
belong to.

An unmarked method keeps its name. `fn init` is an ordinary method, callable as `c.init()`
and free to return a value, which a constructor cannot. That is the permissive rule of the
two available, and it is the one that keeps `len`, `str`, `get`, `eq`, and `iter` usable as
ordinary method names — reserving eight good names to catch one mistake is the worse trade.

The mistake it allows is paid for at the point of failure instead. Constructing a class
whose `init` is unmarked reports the arity error it was always going to report, plus a
help line naming the actual cause. Which is the whole argument for marking: not that `op`
reads better, but that a misspelling becomes an error and a near-miss becomes an
instruction.

`QuinceError` grew a `help` field for that, rendered as rustc's `= help:` line. It is
deliberately separate from `message`, because `message` is what a `catch` sees and a
handler should not have to skip past advice aimed at a terminal.

### What the marking costs

`init` moved, so every class in the corpus changed — 15 sites across 8 files, plus the
`Error` prelude, which is written in Quince and so migrated like any other program. That
is the cheapest this rename will ever be, and it only ever gets more expensive; the same
argument that moved `const` to `final` at 59 corpus files.

Arity is not checked at the declaration yet. `op` knows how many parameters each operation
takes, so `op eq(a, b)` should be an error in the class body rather than at the first
comparison — but `init` takes any number, so the check would have had nothing to act on.
It arrives with the first fixed-arity operation, for the reason Sequencing above already
records: a commit whose content nothing exercises is worse than the later diff it avoids.

## What a class may answer for

Twenty-two ops beside `init`, landed in four steps: what a class converts to and whether
it is true; what equals it and how it orders; what the arithmetic operators mean to it;
and how it behaves as a collection. A class that declares all of them is indistinguishable
from a builtin at every site the *language* asks a value a question — which is the
requirement the whole design was built to meet. A `Range` holding two numbers answers
`len`, `[i]`, `in` and `for` without holding a collection at all.

### One shape, at every site

```rust
if let Some(method) = self.slot(value, Op::Bool) {
    let answer = self.call_op(method, value, Vec::new())?;
    return match answer.base(&self.heap) {
        Value::Bool(b) => Ok(*b),
        other => Err(self.op_returned(Op::Bool, value, "a bool", other)),
    };
}
// otherwise the payload, otherwise refuse
```

Ask the class, check what came back, fall through to the payload, refuse. Thirteen sites
follow it: `is_truthy`, `display_styled`, `repr_styled`, `construct_builtin`, `equals`,
`compare`, `arith`, `unary`, `index_get`, the index arm of `assign`, `contains`,
`exec_for`, and the `len` native.

Four ask the class in order to *refuse better*, which is the opposite direction and was
not predicted: `key_of` refuses a class that declares `op eq`, `slice` refuses one that
declares `op get`, and `partly_ordered` and `only_asks_the_left` each ask the operand that
was *not* consulted, so they can say why it was not.

Every op is asked **before** the payload is unwrapped, without exception. A class
extending `string` that declares `op string` has to beat the string it carries, or
declaring it would do nothing at all — and the same argument applies to every other op,
so none of them gets to be the exception.

### What an op must answer with

| op | must answer | checked |
| --- | --- | --- |
| `bool` | a bool | yes |
| `string`, `int`, `float`, `list`, `dict` | its own type | yes |
| `eq`, `lt`, `gt`, `contains` | a bool | yes |
| `cmp` | an int, read for its sign | yes |
| `len` | an int | yes |
| `iter` | a list | yes |
| `add` … `rem`, `neg`, `get` | anything | no |
| `set` | anything, and it is discarded | no |

Nothing is coerced. `op bool` answering with an empty list would mean the emptiness of
that list quietly deciding an `if`, one indirection away from anything the reader can see.

The unchecked half is not an oversight. `a + b` has no type it has to be — a class
extending `list` whose `+` appends returns another of itself, which is the entire point of
declaring it — and `x[i] = v` is worth `v` the way every other assignment is, so what
`op set` returns has nowhere to go. `op len` is checked for being an int but not for being
positive: nothing indexes with the answer, so an odd length is the class's own answer to
its own question.

### Comparison follows C++, not either Python

Four ops, and the shape is `operator<=>`:

| you write | you get | reached from the right? |
| --- | --- | --- |
| `op eq` | `==`, `!=`, and searching a list | yes, symmetric |
| `op cmp` | `<` `<=` `>` `>=` | yes, with the sign inverted |
| `op lt` | `<`, beating `cmp` | no |
| `op gt` | `>`, beating `cmp` | no |

`op cmp` is the only op that can answer `<=` and `>=`. A class declaring just `op lt` gets
`<` and nothing else, exactly as writing `operator<` in C++ leaves `a <= b` a compile
error. Deriving `a <= b` from `not (a > b)` would assume the order is total, and a
three-way answer exists precisely so a class can decline to be: `{1} < {2}` and
`{2} < {1}` are both false in Python and the two sets are not equal, which no single
`-1/0/1` can express. That is also why `op lt` and `op gt` exist beside `cmp` rather than
being derived from it.

Python 2's `__cmp__` was this exact shape and Python 3 deleted it in favour of six
methods. The difference that makes it worth having here is that `cmp` is not the *only*
way to answer — a class that cannot place itself in a total order writes `op lt` and stops.

`op eq` and `op cmp` are reached from either side, because `==` cannot depend on which
order it was written in, and a reflected `<=>` only needs its sign turned around.
`op lt` and `op gt` are not, which is C++ again: a reversed candidate exists for
`operator<=>` and not for a plain `operator<`.

Getting `<=` wrong is the one mistake this makes easy, so it has a diagnostic of its own
rather than falling through to `cannot compare`, which reads as if the op had been ignored:

```
× `<=` needs `op cmp`, which Version does not declare
  help: `op lt` answers `<` alone. `op cmp` answers all four comparisons at once,
        returning a negative int, zero, or a positive one
```

### Arithmetic asks the left operand only

`2 - Money(3)` reaching `Money`'s `sub` computes `3 - 2` and is wrong by a sign, with
nothing to catch it. So all seven arithmetic ops are `Reflect::Never` and only the left
operand's class is asked — the same answer C++ gives a class with a member `operator-`
and no free function.

Held as data on the op rather than as branches inside `binary`, so adding one states its
rule in the same exhaustive match that gives it a name. That is what made this step cheap:
`reflect()` had already been written, and the arithmetic wiring changed one file.

Writing the operands the other way round then becomes the easy mistake, and gets the same
treatment `<=` did — the plain type error's advice to "change the types to be compatible"
is advice to go and rewrite a class that is already correct:

```
× `op sub` is Money's, and the value on the left is the one asked
  help: reaching Money from the right would hand it the two values the other way
        round, so it is not asked — convert the int, or swap the operands
```

### `op eq` costs the class its use as a dict key

`==` and hashing were already one decision — see that section above, where a subclass of
`string` had to hash as the string it carries. Declaring `op eq` is the other end of it: a
`Key` holds no handle and so cannot run a method, which means two values the class calls
equal would hash apart and a dict would hold both. Refused at `key_of`, before the payload
unwrap, with the reason stated. Python enforces the same rule by setting `__hash__` to
`None`, and it is not a policy in either language — it is what a hash table is.

### `const` is checked before `op set` runs

A frozen object must not get to *observe* a write that is refused anyway. An `op set` that
logged, counted or raised would have happened, and the assignment it belonged to would
still have failed. So the check is on the instance, before the call, rather than on the
payload the op might eventually write through.

The corpus case proves the ordering by having the op push to a list that is *not* frozen
with the instance, so an op that ran leaves a mark whether or not its own write succeeded.
Moving the check after the call puts a `"b"` in that list, which is what makes the case a
guard rather than a passing assertion.

### `op get` answers one index

Not `x[a:b]`. There is no value in the language meaning "1 to 3" — slicing is a `Slice`
node precisely because inventing a range value to carry two optional ints was the worse
trade, see v0.3 — so there is nothing to hand a one-argument op. Slicing a class that
declares `op get` is therefore refused, rather than reaching past the op to the list
underneath, which is the one thing declaring it said not to do. `Op::Get`'s doc comment
claimed it covered both and was simply wrong; it is now the place that says why it does not.

### The prediction about a list that checks itself

When `op` landed, this file predicted that "every operation after the first is a line
added to a list which already checks itself". Half of that is redeemed and half is
retracted, and the halves are worth separating.

**The table did what it was for.** `Op` is a closed set, and `name`, `arity` and `reflect`
are exhaustive matches over it, so no op could be added without stating what it is called,
how many parameters it takes, and whether the right operand may answer. `Op::COUNT` sizes
the slot array, so nothing had to be remembered. Adding `Op::Lt` and `Op::Gt` really was
lines in a list, and the compiler asked the three questions that mattered.

**The call sites were not lines in a list, and could not have been.** The sites are not
uniform: `binary` has two operands and a reflection question, `len` is a native and unwraps
for itself, `set` has a `const` check that must precede it, `iter` has a return type the
loop then consumes. Three ops needed a diagnostic written specifically for the mistake they
make easy. Measured in source lines, the four steps cost 360, 245, 96 and 131. The 96 is
the arithmetic — seven ops at once, and the cheapest of the four precisely because
`reflect()` already existed and the two sites it needed were already open.

The earlier claim that the thirteen payload-unwrap sites would be the thirteen slot sites
held better: thirteen of the fourteen gained one. `list_index` did not, because `op get` is
asked one level above it. And one site the unwrap list never had appeared — the index arm
of `assign`, which is the only op that *writes*, and the only one whose ordering against
`const` had to be decided.

## Bindings — `let`, `final`, `const`

Three keywords answering two questions: may the name be pointed somewhere else, and may
the object it names be changed. `let` allows both, `final` allows only the second,
`const` allows neither.

`final` has a second use, on a class declaration, and it is the same idea rather than a
reused word: it fixes the shape of a declaration where `const` freezes data. See The two
doors, and the four words above.

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

So `Error` is a class, allocated as a `Class` at startup and bound as a global. That is
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

It answers `Option`, and the `None` arm is the whole compile-time story — see A kind you
cannot catch below.

The instance carries `kind` as a field, and `Error.init` sets it from `type(self)` — which is
already the receiver's class name. So a user's `class ParseError extends Error` that calls
`super.init(message)` reports `ParseError` without the prelude knowing it exists. Reification
sets both fields directly rather than calling `init`, because that path is the runtime
building an object rather than a program asking for one.

Only some of the forty-odd raise sites are classified. `new` fills in `Runtime`, so the rest
kept compiling untouched and read as the base `Error` — a gap rather than a lie, and one that
closes a site at a time. The ones a program is likely to catch are done: type, name,
attribute, index, key, frozen, recursion, zero division, overflow.

The thirty compile-time sites are now done too, and they needed a kind that names no class to
get there — see A kind you cannot catch below.

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

### A label has to say something the message did not

Every report used to end with its own message written twice — once as the sentence at the
top, and once again as a label under the caret, because a diagnostic that supplied no labels
had one synthesised from `message`. It looked like a rich diagnostic and carried the
information of a plain one.

The cost is not the duplication, it is what the duplication trains. The space under a caret
is where a label goes, and a reader who finds the sentence they just read there learns to
skip that space — so the four diagnostics that *do* put something new there get skipped
too. A diagnostic that says everything twice is one that cannot emphasise anything.

So a diagnostic with nothing to add now draws a bare underline and no branch, and the `┬`
connector is drawn only where a branch actually attaches. What is left in the branch rows is
only ever new information: which operand is which in `xs + "tail"`, which side of `500 -
Money(200)` the language asked, which half of `xs.nope()` is the receiver and which the
name. Those read as annotations again rather than as decoration.

This is why most reports have no labels at all and that is the right number. `index 9 is out
of range for a list of length 2` names the index, the length, and the type in the sentence,
and adds a valid range in the help; a label under the caret could only repeat one of them.

### Carets are measured against a line, so the line has to be shown

A span is a byte offset, and a column is that offset counted from the start of *its* line.
The renderer took the line the error's own span fell on, showed that one line, and then
measured every label against it — which is correct exactly when every label is on that line,
and silently wrong otherwise.

    2 │ let y = xs +
      ·         ┬─ ┬   ──┬───
      ·         │  │     ╰── string

The `string` label belongs to line 3. Drawn against line 2 it underlines columns 17 to 23 of
a twelve-character line: past the end of the text, pointing at nothing, in a report whose
whole purpose is to point. Any operand on its own line hit this — a wrapped condition, a
call broken over arguments, a list built across four lines.

Labels are now grouped by line and each line is shown with its own underline and branches, in
source order, with a `⋮` between blocks so two labels three lines apart do not read as
adjacent. A span running past its first line is drawn to the end of that line rather than
wrapping, because the frame has no continuation mark and the first line is where it starts.

The gutter is sized for the highest line number any block will show, not for the error's own,
or the frame goes crooked the moment a label sits ten lines below the caret.

### A kind you cannot catch

Lexing, parsing, and resolving all happen inside `compile`, which runs to completion before
`Interp::run` is called. So nothing those stages raise can reach a `catch` — there is no
frame to unwind to, and there is no program running to have written one. That makes them the
first errors with a kind worth reporting and no class to reify into.

`class_name` answers `Option<&'static str>` and both compile-time kinds answer `None`. The
invariant narrows from "every kind names a class" to "every *catchable* kind names a class",
and the `None` arm is where the compiler holds it. Binding the classes anyway was the
alternative, and it was refused for what it would let someone write: `catch e: SyntaxError`
would be a clause that can never fire, and the language would have no way to say so. A class
that exists only to be uncatchable is worse than no class, because the first thing a
programmer does with a name they can see is use it.

The reporting half is a second function. `code` gives the word inside `error[…]`, the two
compile kinds name themselves there, and every other kind defers to `class_name` — so a class
name is written once and the two answers cannot drift. Splitting them is what lets a syntax
error report as one without pretending to be catchable.

That split is what makes the ordering cheap to change later. When `import` compiles a module
part-way through a run, a compile error becomes something a handler can be standing under,
and the change is one arm moving from `None` to `Some` plus one line in `ERROR_KINDS`.

### Two words, because the grammar is not the program

`Syntax` is text that does not parse. `Declaration` is text that parses and still is not a
program: a name declared twice, `self` where there is no receiver, an `op` at the top level,
`op eq` declaring two parameters where the language passes one.

Python folds both into `SyntaxError` — it reports a name assigned before its `global`
declaration as one — and that was the cheaper option by one word. It was refused for the same
reason `Attr` is separate from `Type`: the two mistakes send you looking in different places.
Told the syntax is wrong, you reread the punctuation on the line, and for a duplicate `let`
there is nothing wrong with the punctuation on the line. The word has to survive being read
by someone who has not read the parser.

The split is not by module, which is the part worth writing down. Five of the parser's
thirteen refusals are declaration errors — `op` outside a class, an unknown `op` name, the
`op` arity check, a duplicate method, an `op` in an extension. They live in the parser
because everything the check needs is in hand there, the keyword and its span before a body
is parsed, and being caught early does not make them grammar. So the parser gets two
constructors and the site says which it is by which one it calls; the lexer and the resolver
get one each, because neither can raise the other sort.

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

## Modules

A module is a `Globals` with a path. That is the whole of it, and finding that out
was most of the design: a module has a name table and so does the top-level scope, so
`Object::Globals` already *was* a module and needed two fields — what it is called and
where it was read from. `Value::Module` points at one, `math.floor` is a name looked up
in a scope, and nothing new had to be built to hold either.

### `Slot::Global` walks to the root of the chain

The single-module assumption lived in exactly one place. A local name resolves to
`(hops, index)` and walks the scope chain; a global skipped the chain and read one field
on the interpreter. Now it walks out to the `Globals` the chain ends at, which is the
module the code was compiled in.

Nothing was recorded on a function to make that work. A `Function` already captures the
scope it was defined in, so a function written in one file and called from another
bottoms out in its own module's names — which is what lexical scoping meant everywhere
else in the language already. The one field was the only place not honouring it.

The cost is a pointer-chase per level of nesting, paid only by global reads, which
already hash a string. If that ever matters the fix is a module handle on `Function` and
a current module on the interpreter, pushed and restored like every other piece of frame
state; it is written down in `module_of` and has not been needed.

### A span belongs to a file

This is the part that could have quietly undone v0.5. A span is an offset into one text.
With two files there are two texts an offset could be into, and rendering one file's
offsets against another's source produces a caret under whatever happens to sit there —
the exact defect the diagnostics sweep existed to remove, arriving by the back door.

So `QuinceError` carries the module its spans belong to, and carries it as an `Rc` to a
shared `ModuleSource` rather than a copy of the text per error.

**It is set on the way out, not at the raise.** A raise site knows what went wrong and
has no idea what file it is in — there are ninety-odd of them and none takes a module.
The frames an error unwinds *through* know exactly, and there are two: the loop running
a module's top-level statements, and the call arm for a user function. The first to set
it wins, which is the innermost, so an error crossing three modules is reported against
the one that raised it. `imports_error` in the corpus pins a caret in a file the case
does not start in, which is the whole mechanism in one `.report`.

### The error classes are shared, not rebuilt

A module's scope is seeded with the builtins, the type classes, and *the same*
`Error` class objects the starting module holds. Not fresh ones: `catch TypeError`
compares the class it was handed against the one the error reified into, so two modules
with two `TypeError` classes would mean a handler that silently never fires. Re-running
the error prelude per module would have bought that bug and paid a compile per module
for it.

### A cycle is refused

Reaching a file that is still loading raises, with the chain that got there —
`alpha.qn → beta.qn → alpha.qn`. Python's alternative is to hand back the half of the
module that has run so far, which converts a structural mistake into a failure somewhere
else entirely, at a name that is mysteriously missing. This language has consistently
refused instead, and the chain is what makes the refusal actionable.

A module that fails to load is *removed* from the registry rather than left marked, so a
second import of it fails the same way rather than being told it is a cycle.

### What an import may name

A file beside the importer, by a bare name, with no path and no extension. Not a search
path, not a package, not a subdirectory — each of those is a decision that wants a
language with modules already in use to decide it, and refusing them now is cheaper than
half-supporting them. `import utils/strings` is caught in the parser, which is where the
`/` still exists; by the time the evaluator has a module name it has only an identifier,
and the reader would have got "expected a newline" instead.

The stdlib is searched first and wins. A file appearing in a directory must not change
what `import math` means, and the reserved set is small, fixed, and listed in
`stdlib::MODULES` — which is what makes that a rule someone can hold in their head
rather than a trap.

**An import is top level only.** A module is loaded once into the scope of the file that
asked for it, so an import inside a function or a loop would be a load whose effect
depended on whether the code ran.

### `from` is not a reserved word

`import` is; `from` is not. Taking it broke `op init(from, to)` in the corpus on the
first run — which is how anyone writes a range — and that cost would have been
permanent. It is recognised at the one position where it can mean anything: the start of
a statement, with an `import` two tokens along. That is the parser's second lookahead,
after the one `final class` needs, and it buys back a word people use.

The TextMate grammar needed the same treatment, and the keyword guard is what said so —
`the_editor_grammar_highlights_nothing_else` failed the moment `from` stopped being
reserved, which is the direction that test was written for and sooner than expected.
`from` is highlighted only where a name and an `import` follow it.

### What the library is, given all that

`import` is what made the library affordable, and it went in first for that reason. With
no module system every library name is a global a program can never use again, and
`math` alone is ten of them. Four modules now cost nothing until they are asked for.

A stdlib module is built from a table into the same `Globals` an imported file produces,
so `math.floor` and `util.helper` are one lookup and the two import forms are one code
path. Members are handed back *unbound*, unlike a class handing back methods — so a
stdlib native takes no receiver, which is the opposite of the natives seeded onto a type
where `upper`'s `args[0]` is the string. That difference is why they live in their own
file.

**`random` is seeded to a fixed number.** A program that does not ask for
unpredictability replays exactly. That makes a bug involving random numbers reproducible
and lets the corpus assert values rather than ranges — the difference between testing
`random` and testing that it returns a number at all. A program wanting otherwise writes
`random.seed(time.now())`, which is the one place two of these modules meet.

**`time` ships one clock.** A monotonic one is the right thing to measure elapsed time
with, but nothing in the language can say "this float may not be compared to that one",
so shipping both would ship two floats that look alike, must not be mixed, and carry
nothing saying which is which.

**`io` paths are relative to the working directory**, deliberately unlike `import`. They
answer different questions: an import names part of the program and travels with it,
while a path names data the program was pointed at, which belongs to whoever ran it.

### The first natives to call Quince code

`map`, `filter`, and `sort` are the first things in the tree to run a program's own
function from inside a builtin, which makes them the first to cross a safe point holding
something of their own. The comment at the `Native` arm of `call` had been predicting
this for a while: `args` lives in a Rust frame and nothing roots it, which is safe only
until a builtin reaches a safe point.

So the receiver, the function, the element in flight, and the list being built all go on
`temps`. The last is the one nothing else could reach — it is not bound to a name until
`map` returns.

`the_list_being_mapped_into_survives_collection` is what holds that, and its first
version was worthless. It churned *before* the map and passed with the rooting deleted:
the collections it counted had all happened already, and by then the threshold had been
raised past what eight small allocations could reach. Moving the churn inside the
callback is what puts a real collection between two pushes into the list. See Collection
above — this is the same trap `a_thrown_payload_survives_the_unwind` was written around,
and it caught a second victim.

`sum` starts at the first element rather than at zero, and the corpus is what forced it:
`["a", "b"].sum()` failed at `0 + "a"`. Starting at zero would have meant `sum` worked
for numbers and quietly refused every class defining `op add`, which is most of the
reason `op add` exists.

`sort` is a merge sort rather than `sort_by`, because comparing can run an `op lt` and so
can fail, and `sort_by` has nowhere to put an error. It is stable as a consequence, which
a class defining its own order has every right to expect.

### What this does not bring

**Function expressions.** `xs.map(fn (x) { return x * 2 })` does not parse: `fn` is a
statement, so a callback must be declared and named first. Closures work — a `fn`
captures the scope it was declared in — so a function returning a function maps fine. But
the short spelling everyone reaches for is not there, and it is a language decision
rather than a library one.

**Cross-file understanding in the editor.** An imported name is unknown to `lsp.rs`,
which is honest and is the inference tranche's problem.

**Scoped extensions.** `Interp::extensions` was kept out of `Class::methods` on the
argument that an extension should one day be visible only where its module was imported.
Modules now exist and that is still not wired up; the note in `extensions` is the record
of why the door was left open.

## Type inference

`infer.rs` walks the AST and answers *what class is this expression* — a literal, a
constructor call, a field, a call whose returns agree — and answers `Unknown` for
everything else. It is a pass beside the resolver, not a stage inside it: the resolver
decides where a name lives and fails a program that gets that wrong, while this decides
what a name holds and cannot fail at all. Nothing in the file returns a `Result`.

That is the whole shape of it. `Unknown` is the ordinary answer for a dynamically typed
program — a parameter is whatever the caller passed, a list holds whatever was put in it
— and the pass exists because the alternative was already in the tree: `lsp.rs` decided a
receiver's class by whether its name started with a capital letter. That guess is right
often enough to be trusted and wrong without saying so, which is the worst combination
available.

### `Unknown` is an answer, not a failure

The type is three states and joining two of them is equality: `int` joined with `int` is
`int`, and anything else is `Unknown`. There is no union in the middle and no `nil`-shaped
bottom. A variable holding an int on one path and a string on the other has no type worth
reporting, and v0.7's annotations are where a program gets to *say* it meant both.

What that costs is visible in `a_bare_return_is_a_nil_that_joins`: a function returning a
`Point` down one branch and bare-`return`ing down the other returns neither. The
alternative — taking the branch that says something and ignoring the one that does not —
is how a checker starts lying.

There is one place the pass is optimistic on purpose. A body with some `return`s and a
path that falls off the end is taken at its word rather than joined with the `nil` that
path produces, because knowing whether a body always returns is a flow analysis, and this
pass is the floor such a thing would stand on rather than a first draft of it.

### A class name is not an instance of it

`Point` is a value of type `class`; only `Point()` is a `Point`. The heuristic could not
draw that line because it decided by spelling, and both halves of the mistake are pinned:
`a_class_name_holds_a_class_and_not_an_instance`, and `a_lowercase_class_is_still_a_class`
for the program that names a type `point` and was getting nothing at all.

That distinction is why `Types::of_path` takes `Point()` rather than `Point`, and why
`lsp.rs` grew a second reader for the text before the cursor. `get_receiver_before_dot`
strips the parentheses, which is right for the heuristics and destroys the only evidence
the pass needs; `receiver_path` keeps them, normalising arguments away because the
arguments say nothing about the type. Two readers rather than one changed reader, so the
floor keeps behaving exactly as it did.

### What the language decides, and what a class decides

`1 / 2` is a float and `a == b` is a bool, because the evaluator says so whatever the
operands were — `compare` reads whatever `op cmp` answered for its sign and hands back a
bool, so a comparison on a class is still decidable. `m + m` on a class declaring `op add`
is `Unknown`, because that op may return anything at all. The split is not a heuristic
about likely programs; it is a reading of `Interp::binary`, and the two tests around it
name which side each case falls on.

The same rule decides the smaller cases. Indexing a string gives a string and indexing a
list gives nothing, because a list is not a `list[T]` until v0.7 says so. A `for` loop
over a list *literal* takes its elements, since that is the one place the element type is
written in the file.

### Nothing is read off a second list

A module's members come from `stdlib::MODULES`, and a constant is *built* to find out what
it is — `math.pi` is a float because building it produces one. The builtin constructors
come from `Builtin::conversion`, so a type that gains or loses one is followed without
this file being touched, and `every_builtin_that_can_be_called_names_a_type` pins the
reading against the list.

A native says what it returns, on the native. `Native` carries `returns: Option<Builtin>`
beside its name and arity, so `"a,b".split(",")` is a list and `math.floor(2.5)` is an
int. Before that field existed every call into the library was `Unknown`, the heuristics
answered instead, and they were wrong in the way heuristics are: `split` returns a list,
and reading the literal at the front of the line called it a string. The editor offered
`.lower()` on a list and hid `.push()`.

`None` is half of what makes the field worth reading. `abs` keeps the type it was handed,
`dict.get` answers with whatever was stored, and `io.line` is a string until input runs
out and then it is `nil`. Naming a type for any of those would be the same guess in a more
authoritative place — the field is believed, so it may only be filled in where it is
certain.

Nothing in Rust can check it. A native's body builds a `Value` at run time, so a wrong
entry compiles and then lies to every editor. `every_declared_return_is_what_the_native_actually_returns`
is the answer: it generates a Quince program calling all forty-two, runs it, and compares
`type(x)` against what each declared. The hand-written part is the list of calls, and the
completeness assertion beside it is what keeps that honest — a native that declares a
return and is never called fails the test rather than going unchecked.

### Cycles and recursion are shapes it can be handed

`fn down(n) { return down(n - 1) }` is a program, and so is a class whose field is one of
itself. Both are guarded by an in-progress set, and both answer `Unknown` on re-entry —
which is also the true answer, since the recursive arm carries no information. An
`extends` cycle is refused at run time and not by the resolver, so the pass can be handed
one of those too; every walk up a parent chain carries a visited set for it.

### The heuristics are gone

They were kept as a floor when the pass landed, on the argument that an editor going
blank between two valid states is worse than one that guesses. Measurement settled it.
`"a,b".split(",")` is a list, the heuristic read the literal at the front of the line and
called it a string, and the editor offered `.lower()` on a list while hiding `.push()`.
A wrong completion is not a weaker right one — it is indexed, scrolled, and believed.

All of it went: `infer_receiver_class`, `infer_method_return_class`, the text scan for
variables, the line-reading signature finder, and the ten-entry hover table. `lsp.rs` lost
a third of its length and every sentence it used to know about the language.

What replaced the floor is that the pass keeps its last good tree, which covers the case
the floor was written for. What is left uncovered is a buffer that has never parsed at
all, and there the editor offers keywords and globals — both always known — and nothing
after a dot. An empty list is the honest answer to a receiver nobody can identify.

Two cases needed real work rather than deletion. A literal receiver — `"abc".`, `[1, 2].`
— is now typed by *lexing* the text and reading the tokens, so `xs[0]` is no longer a list
because it happens to end in a bracket; the token in front of the opening bracket is what
tells an index from a literal. And the error classes are read by inferring over
`BASE_ERROR`, the Quince source the interpreter itself runs, so `TypeError(message)` is
what the editor shows because that is what the prelude says.

### Everything is read off the language

`Native` carries `params`, so signature help says `fn split(separator): list` where it
used to say `fn split(arg1, arg2)`. `TokenKind::doc` explains every keyword, as an
exhaustive match beside `KEYWORDS` — the old table covered ten of twenty-five and had no
way to notice. A `##` block reaches hover, completion, and per-parameter signature
documentation through one `Symbol` type that `infer.rs` hands out and `lsp.rs` only draws.

The rule the milestone kept arriving back at: point at the list where you can, and where
you cannot, fail loudly when the copy is wrong. Three guards came out of this tranche —
every keyword explains itself, every native names the parameters its arity implies, and
every declared return is what the native actually returns.

### The REPL answers from values, not from source

The REPL was in a better position than the editor the whole time and was throwing it away.
A bound name has a *value*, and a value has a class — there is nothing to infer. What it
had instead was three hand-maintained maps rebuilt after every entry: globals as
`(String, String)`, methods as `HashMap<String, Vec<String>>`, fields as another. Between
them they could not say what a member returned, missed every `extend`ed method, and fell
back to offering every method of every type when the receiver was not a plain global —
forty names of which two applied.

One `Snapshot` replaced all three. It reads the live objects: globals with the class of
what is actually bound, members through `Interp::methods_of`, which makes the same two
walks dispatch makes so an extension is offered exactly when it is callable. Fields come
off the instance, because a field exists when something assigned it and not before.

The two surfaces now share `cursor.rs` — the last place either one touches raw text, and
deliberately incapable of guessing. It answers where a name begins, whether parentheses
balance, and which token the text ends on, then hands over to the pass or to the
interpreter. Both surfaces used to answer that question for themselves, which is how one
came to decide a class by its first letter and the other to give up and offer everything.

**`Dog.bark` reaches the method**, which neither surface knew. Checking rather than
assuming settled it — `print(Dog.bark)` writes `<fn bark>` — so a dot on a class object
lists its methods, and not its fields, because only an instance ever assigned one. The
editor had been offering nothing at all there.

### The editor keeps the last thing it understood

The pass needs a tree and typing a `.` is what stops a document having one — which would
have made it useless at precisely the moment it is asked. So `DocumentState` keeps the
previous `Types` when the new text does not parse. The offsets before the cursor are
unchanged, and a scope that contained the cursor still contains it, because the text after
the edit is what went stale and that is not what anyone is asking about.

That is what `typed(&[…])` in the LSP tests is for: a document built from its final text
alone is a document nobody ever has, and a completion test written that way would be
testing a state that cannot occur.

The heuristics stay underneath, unchanged. `Type::Unknown` from the pass is not a weak
answer that a guess outranks — it is the pass saying the guess is all there is — and the
match in `get_completions` is where that ordering is written down.

### The corpus is what checks it

`what_the_pass_claims_is_what_the_programs_produce` runs every case that compiles, then
compares each claim against the runtime class of the global that name ended up holding.
Two hundred claims over forty programs nobody wrote with a checker in mind. A pass allowed
to answer `Unknown` has exactly one way to be wrong, and this is what looks for it — the
unit tests assert that the pass *says* `int`, and this asserts that the value is one.

## Doc comments

`##` at the start of a line is documentation; `#` is a comment and is discarded exactly as
it always was. The lexer gathers the `##` lines it passes and hands them to the next token
it produces; the parser reads them off the declaration keyword and `doc.rs` decides what
they say.

Choosing a second sigil rather than promoting every comment is the whole of the design.
Retaining all comments would have unlocked the formatter in the same change, and it was
refused for one reason: `# TODO: this leaks` would have become published documentation,
and "the comment directly above" is an adjacency guess of exactly the species the
inference pass was written to delete. `##` makes the writer say which is which.

### The tags are checked against the declaration

A block is a summary and then `@param`, `@return`, `@throws`. `@param radius` on a
function whose parameter is `r` is refused, with the caret under that line and a help
naming what it does take.

That refusal is the reason the format is parsed at all rather than stored as prose.
Documentation is a second copy of the signature, and a second copy drifts — the TextMate
grammar drifted by three keywords, the LSP's completions were missing `bool`, and both are
recorded above as one defect wearing two faces. Prose that has stopped describing its
function is worse than none, because it is read and believed. This is the same rule
`op lenght` is refused by: `Tag::from_name` is the only way in, so `@parm` is an error
listing the three that exist rather than a line quietly absorbed into the paragraph above.

The check runs one direction only. A parameter with no `@param` is fine — a rule against
it would mean a half-documented function cannot be written, and what people do about a
rule like that is delete the documentation.

Tags that need a signature are refused where there is none: a `let` and a `class` get a
summary. A class's arguments belong to its `op init`, which documents them like any other
function.

### Docs ride on the token, not in the stream

`Token` carries `doc: Option<DocBlock>`, beside `newline_before`, and for the identical
reason that field exists. A `TokenKind::Doc` in the stream would have to be skipped at
every match site in the parser — a hundred places with no interest in it, any one of which
could forget, each failing as a baffling syntax error. Recording it on the token means the
four declaration sites that care ask for it and nothing else changes.

Each line keeps its own span, which is what lets a report about one tag underline that
tag. `@return` has a struct rather than a bare `String` for no other reason: without a
span its diagnostic pointed at the summary three lines above the mistake.

### What it does not bring

**The editor does not render any of this yet.** The blocks are parsed, checked, and hung
on the AST, and `lsp.rs` still shows a hand-written table of ten builtins. Wiring the two
together is the next tranche, along with deleting that table.

**A native cannot document its parameters.** `Native` records an arity and not the names,
so `@param` on a builtin could not be checked and is not offered — `signature_of` still
renders `fn split(_, _)`. Giving natives their parameter names would fix the signature
help and the documentation at once, and it is the obvious thing to do next to them.

**Comments are still discarded**, so the formatter is no closer. `##` was made additive on
purpose: the lexer change needed for a formatter can land later without disturbing this.

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

The protocol slots have since landed — a user class decides its own truthiness, printing,
equality, ordering, arithmetic, indexing and iteration. `try`/`catch`/`throw` landed in v0.5. `push`, `keys`,
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
piece of work and were listed under v0.5 rather than left implied here — where they
have since landed.

**v0.5 — robustness**
`try`/`catch` and span-accurate diagnostics everywhere. GC is done.

**Done.**

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

`op` landed next, and reordered what follows it. Marking a method as one the language
calls is a prerequisite for the slots rather than a nicety alongside them: the machinery
that validates a closed set of operations wants to exist while that set has one member, so
that every operation after the first is a line added to a list which already checks
itself. It also moved `init` while the corpus was 8 classes.

One class representation landed next, and first, because slots written against the enum
would have been rewritten by it — see One class representation above. It is deliberately
behaviour-neutral apart from the seven type globals it binds: `type(x)` still answers with
a string, no slot moved, and `string.upper("hi")` works with nothing written for it, which
is the cheapest available proof that the two kinds of type really are one thing now.

Conversion landed on top of it, and is the proof the collapse paid: `int("42")` needed no
new keyword, no new dispatch, and no special form — a builtin's seed table names an `init`
the way it names `upper`, and the one rule added is what construction yields. It also added
`ErrorKind::Value`, the first new kind since the enum landed, because `int("abc")` is a
different mistake from `int([1])` and both are catchable. Single-quoted string literals came
with it, being a lexer change the same work wanted.

Subclassing a builtin landed next, and cost less than this file had estimated — see
Extending a builtin above. The estimate was wrong because it had been reasoned rather than
checked: `args.insert(0, receiver)` appears once in the interpreter, so the receiver
substitution is one line at one place and no native was touched. What it did cost was the
static checks, which are the larger half of the change and were not in the estimate at all.

It also settled where a rule belongs when both ends can enforce it. The resolver checks that
a `super.init` is written, because catching it before the class is stored is worth more than
precision; the evaluator checks that only one runs, because it is holding the payload anyway
and a branch makes the syntactic count a lie. Neither check is a weaker version of the other.

The payload unwrap landed next, together with the implicit `op init` — see Where an operator
finds a payload above. Bundling them was the right call and not only for convenience: an
implicit init is what makes `class Username extends string {}` worth writing, and without the
operators there was nothing you could then *do* with one, since `int`, `float` and `bool` have
no methods. Half the feature would have shipped unusable.

This work was called "the slots" in its commit message and in the first draft of this section,
and that was wrong in a way worth recording rather than quietly correcting. Letting a subclass
reach `string`'s `==` and letting a class define its own `==` are different features that
happen to touch the same functions. `Op` still has exactly one member. The mistake was
possible because both get described as "making operators work on instances", and the tell that
it was a mistake is that nothing was added to `OPS`.

The thirteen unwrap sites are also the thirteen sites the slots will need, so whichever of the
two went first, the second revisits them. An earlier draft claimed the ordering avoided
writing the unwrapping twice; it does not, and that claim was never tested. It has since been
checked against what happened: thirteen of the fourteen named sites did gain a slot, and one
the list never had — the index arm of `assign` — appeared with `op set`. The prediction from
`op`'s own section, that every operation after the first would be a line added to a list that
already checks itself, came out half right; What a class may answer for above separates the
half that held from the half that did not.

What was genuinely unexpected is how much the implicit init *removed* — the "extends a builtin
but has no `op init`" rule and the resolver's `inits` set both went, because the case they
guarded became the ordinary one.

The protocol slots were the piece this was all building toward, and they are **done** — a
class decides its own truthiness, printing, conversion, equality, ordering, arithmetic,
indexing and iteration, each as an `op` beside `init`. The predicted shape mostly held: ask
the class, then fall back to the payload, then refuse, at ten of the sites. Three ask the
class in order to refuse *better*, which nothing predicted. See What a class may answer for
above, which is also where the accounting of that prediction lives.

`extend` landed next, and cost less than the slots did because the hard part had already
been decided: methods had one lookup path, so giving extensions their own table meant one
helper and three call sites. The one thing that needed care was the part the plan had not
written down — the table is a root, and nothing else refers to what it holds. See What
landed, and what the plan above did not say.

The diagnostics sweep went last, as planned, and **v0.5 is complete**. It found more than the
classification it was scoped as. Every raise site now carries a kind — the thirty compile-time
ones needed a kind that names no class to get there, see A kind you cannot catch above — but
the two real defects were in the renderer, not in the sites. A label on a line other than the
error's own was drawn against the wrong line and underlined text that was not there, and every
diagnostic repeated its message under its own caret, which is what taught a reader to stop
looking at the space where labels go.

Both were invisible from inside the suite, and that is the part worth keeping: the corpus
compared `err.message`, the one part of a diagnostic with no span in it, so a milestone about
span accuracy had no test that any span was accurate. The `.report` file went in first for
that reason and immediately earned it — it is what turned each of the changes above into a
diff someone could read, and it caught the one report that the classification pass changed
by accident. See Four companion files above.

What the sweep did not do is add labels everywhere. Four diagnostics have them, and the rest
draw a bare caret on purpose: a label is worth a line only when it says something the message
did not, and most messages already name every value they are about.

Subclassing a builtin went before the slots because it is what a user asked for, not because
the ordering saved anything. The two touch the same functions — `is_truthy`, `equals`,
`display`, indexing, `len`, iteration — so either order visits them twice. It has since landed;
Extending a builtin and Where an operator finds a payload above are the record.

Equality was the one open decision, and Python settled it by force rather than by preference:
if an `Email` equals a plain string then it must hash alike, or a dict holds two keys that are
equal and land in different buckets. `equals` and hashing were one decision, not two.

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

They landed in four steps rather than one, and the reason is worth keeping: the *decisions*
were made together — what an op may return, when it is asked, whether the right operand
answers — while the wiring went in a family at a time, each with its own corpus case. A
single commit touching every operator at once would have been unreviewable and would have
put the four return-type rules beyond the reach of the test that proves each one bites.
What a class may answer for above is the record.

One rule for the diagnostics sweep, worth writing down before it starts: **"protocol
slot" is a word for this document, not for a report.** Implementation vocabulary — slot,
protocol, reify, arena, safe point — never appears in a message a Quince programmer
reads, because a diagnostic is read by someone who has not read `class.rs`. Where a closed
set is involved, list its members instead of naming the category: `op` can define: init
is more use than any noun for what `init` is.

The language server landed inside this milestone without the roadmap ever listing it, and
it belongs here for the reason the REPL does: it is a surface the language is used through
before it is finished, and a diagnostic nobody sees until they run the file is a diagnostic
that arrives too late. It runs the same lexer, parser, and resolver the CLI runs and
publishes the same `QuinceError`, so there is one account of what is wrong with a program
and not a second one written for editors. Everything downstream of `KEYWORDS` — the
completer, the highlighter, the token map — reads the one list, which is what made adding
`complete` and `sealed` four edits in `token.rs`.

The extension carries the language's version rather than one of its own. It is not a separate
product with its own release cycle: its semantic token scopes name what `lsp.rs` emits, its
grammar spells out the words the lexer reserves, and it launches a binary built from the same
tree — so a version of its own would be a number changing for reasons the number cannot
express. The cost is a marketplace release for every language release, including ones where
nothing under `editors/` changed; that is cheaper than a bug reported against "extension
0.1.0" with no way to say which language it was matching.

The TextMate grammar is the one thing downstream of the keywords that cannot read `KEYWORDS`,
because a `.tmLanguage.json` is data VS Code reads without ever running our code, and it had
drifted exactly as far as that predicts: `extend`, `complete`, and `sealed` were reserved by
the lexer and unknown to the highlighter, so three keywords rendered as plain identifiers in
the editor the language ships an extension for. The list is now correct and it is still a
copy, which means it will drift again.

The release commit predicted the fix and got it wrong, which is worth keeping rather than
quietly correcting. It said to generate the file from `KEYWORDS` at build time. But the
grammar sorts keywords into four rules by category — control, declaration, the two
receivers, the literals — and a generator would have to be told which category a new word
belongs to, which is the only part of the job that needed a person. The generator saves
nothing and adds a build step that writes into the source tree.

What went in instead is a pair of tests beside `KEYWORDS`, and they cost eleven lines
between them: one asserts every reserved word is highlighted, the other that every
highlighted word is still reserved. The second direction is not symmetry for its own sake —
a word removed from the language keeps its colour and goes on reading as reserved, which is
the same drift wearing the other face. Both were checked by breaking the grammar and
watching them fail, because the first of them is a test that would have passed for the whole
time the bug existed had it been written to iterate the grammar instead of the list.

What it does *not* do is type inference. A document mid-keystroke usually does not parse,
and an editor that goes blank between two valid states is worse than one that guesses, so
`lsp.rs` falls back to reading lines as text — `fn ` at the start of a line is a function —
whenever the AST is unavailable. Those paths are heuristics and are marked as such in the
function names; they are a floor under the AST paths, not a second implementation of them.
The day a real type checker exists it should take both.

**v0.6 — modules, a library, and a language server that knows rather than guesses**

**Done.** Modules, the standard library, the inference pass, doc comments, and two
editing surfaces that read the language instead of guessing at it.

The milestone was scoped as tooling and a library, on the assumption that the module
system stayed deferred and the library worked around its absence with namespace objects.
It did not stay deferred. `import` went in instead, for files as well as for the stdlib,
which deleted the constraint the library was going to be designed around and made the
library *simpler* — a namespace is a module like any other, and nothing is a global until
someone asks for it.

That made the milestone large, and the trade was named before it was taken: if it ran
long, the thing to cut was library domains, never the module system half-built. A
language with an `import` that only reaches the stdlib is a language that looks like it
has modules and does not.

**Modules are done**, and Modules above is the record. The three things worth carrying
forward from it: `Slot::Global` walking the scope chain was the entire change needed for
per-module scope, because the chain already ended where it had to; a span belongs to a
file, and getting that wrong would have quietly undone the v0.5 sweep; and the error
classes are shared across modules, without which `catch` would have compiled, run, and
never fired.

**The library is done** — `math`, `io`, `time`, `random`, and twelve methods across the
three collection types. `map`, `filter`, and `sort` are the first natives to call a
program's own code, and the rooting that needs is the one genuinely dangerous thing in
the milestone.

**The inference pass is done**, and Type inference above is the record. It landed in the
order the milestone argued for and the library did the job the argument gave it: `math.`
completed to nothing at all before there was a `math`, and a receiver's class decided by
its first letter was not visibly wrong until there was a corpus to be wrong about. The
three things worth carrying forward: `Unknown` had to be an answer rather than a failure
or the pass would have had to guess exactly where the heuristics do; the editor keeps the
last tree it understood, because typing the `.` is what stops a document parsing and a
pass that gave up there would be useless at the only moment it is wanted; and the
corpus check — every claim against the runtime class of the value that name ended up
holding — is what makes the whole thing more than a set of assertions about itself.

The natives were the gap it shipped with, and they are closed: `Native` records what it
returns, what it is for, and what its parameters are called, so a call into the library is
understood and the editor's documentation lives beside the code it describes.

**Doc comments went in last and were not on the list.** `##` above a declaration, with
`@param`, `@return` and `@throws` — and the tags checked against the declaration they sit
above, which is the whole reason the format is parsed rather than stored. Documentation is
a second copy of the signature and a second copy drifts; this milestone found that defect
three separate times (the TextMate grammar, the builtin completions, and then every doc
comment nobody would ever have checked) before deciding to refuse it at the source.

**Every guess is gone from both surfaces.** The editor's text heuristics went once the
library made them measurably wrong — `"a,b".split(",")` is a list, and reading the literal
at the front of the line called it a string. The REPL's three hand-maintained maps went
for the opposite reason: it holds values, so there was never anything to infer. What is
left is `cursor.rs`, which reads structure and is deliberately incapable of deciding what
anything means.

Cross-file inference is still after it, not before.

The keyword guard added at the start of the milestone paid for itself twice inside it:
once when `import` arrived, and once when `from` stopped being reserved. Both times it
failed before anything shipped, and the second was the direction that looked speculative
when it was written.

Two hand-written lists turned out to be the same defect wearing different clothes. The
TextMate grammar had drifted by three keywords; the LSP's builtin completions were
missing `bool`. The grammar cannot read `KEYWORDS` — VS Code parses it without running
our code — so it is guarded by a test. The completions can, so they now do. That is the
rule: point at the list where you can, and where you cannot, fail loudly when the copy is
wrong.

**What it cost to finish, and what that bought.** The milestone ran long, and the trade
named at the start — cut library domains, never the module system half-built — never had
to be taken. What did happen is that finishing revealed two things nobody had listed: that
the editor's guesses were wrong rather than merely approximate, and that a language with a
library needs somewhere to write down what the library is for. Both were found by
measuring rather than by argument, which is the reason the tranche has three new guards
and not three new opinions.

The milestone is chosen because the limit on using Quince is no longer the language. v0.5
closed the expressiveness gaps that were worth closing — a class can answer for anything a
builtin can, an error is a value, a diagnostic points at the right token. What stops someone
writing a real program now is that there is nothing to call: `print`, `len`, `type`, seven
type constructors, and the methods on the four builtin types. No file IO, no time, no
random, no math beyond the operators, no way to read an argument the program was run with.

The library and the inference pass are one milestone rather than two because they check each
other. A standard library is the first corpus large enough for the LSP's guesses to be
visibly wrong, and inference is what makes a library discoverable — a completion list for
`f.` is worth more than any amount of documentation for the same method.

**What inference has to be.** The heuristics in `lsp.rs` — `fn ` at the start of a line is a
function, the class of a receiver is whatever the nearest assignment looked like — are a
floor for a document that does not parse, and they should stay exactly that. What sits above
them is a pass that walks the AST and answers "what class is this expression" for the cases
that are decidable: a literal, a constructor call, a method whose body returns one class, a
parameter with no information at all. It belongs beside the resolver, which already walks
every name and already knows what binds where, and it must be allowed to answer "unknown"
without that being a failure — most of a dynamically-typed program is unknown, and a checker
that guesses to avoid saying so is the thing being replaced.

**The constraint the library is under.** There is no module system, so there is nowhere for a
library to live except the global scope, and every name added is a name a program cannot use.
That bounds what belongs in v0.6 to what is worth being global forever — which is a real
bound and not a temporary one, because `print` will still be global after modules land. The
honest reading is that this milestone builds the part of the library that would be builtin in
any case, and that a `math.floor` waits for a `math` to put it in.

**A formatter is listed and not promised.** It needs the lexer to keep comments, which it
discards today — `skip_trivia` throws them away before the parser ever sees
one — and a formatter that deletes every comment in a file is not a tool anyone runs twice.
Retaining them is a change to the token stream that everything downstream reads, so it is
sequenced first if the formatter is done at all, and it is the piece to cut if the milestone
runs long.

**v0.7 — gradual type annotations (`T?`), container generics (`list[T]`), visibility (`pub`, `private`, `protected`), and LSP tooling**

This milestone introduces gradual optional type annotations, generic container bounds, out-parameter references, member/module visibility access control, and rich LSP editor tooling to Quince.

- **Annotations & Explicit Nullability (`T?`)**: Optional type annotations on variable bindings (`let x: int = 8`), parameters (`fn example(x: int, opt: int?)`), and function return signatures (`: string?`). Types are non-nullable by default; `nil` is rejected unless specified with `?`. Reassigning an annotated variable to a non-matching type triggers a `TypeError`. Unannotated bindings (`let x = 8`) remain dynamically typed.
- **Typed Generic Containers (`list[T]`, `dict[K, V]`)**: Enforces element/key/value type bounds on collections (`let nums: list[int] = [1, 2]`). `nums.push("hi")` raises a runtime `TypeError`.
- **Reference Parameters (`ref`, `final ref`, `const ref`)**: Enables out-parameter mutation (`fn inc(ref y: int)`). Plain `ref` requires a mutable `let` lvalue. `final ref` prevents reassignment inside the callee. `const ref` provides read-only references accepting `let`, `final`, or frozen `const` variables.
- **Class Member Visibility (`public`, `private`, `protected`) & Field Declarations**: Class body field declarations with type bounds. `private` restricts access to the declaring class, `protected` permits subclass access, and `public` allows external access. Operator declarations (`op`) must be `public`.
- **Module Visibility & Exports (`pub`)**: Module declarations default to private. Top-level variables, functions, and classes marked with `pub` (`pub fn`, `pub class`) are exported for module consumers.
- **Operator (`op`) Type Contract Validation**: Operator declarations must adhere to built-in protocol return types (e.g. `op string(): int` is rejected as a compile-time resolution error).
- **LSP Type Tooling**: Adds LSP Inlay Hints (`textDocument/inlayHint`) to display inferred/annotated types inline in VS Code, auto-completes type names after `:`, and underlines type mismatch/visibility diagnostics live.

See `V0_7_TYPE_SYSTEM_DESIGN.md` for full design specifications and type matching rules.

**Later**
Bytecode VM, async/await, sized integer types — all things Zephyr has, deferred until the
core is solid. The module system was on this list and came forward into v0.6; what is
left of it is packages, a search path, and subdirectory imports, which want a language
with modules already in use to decide them.

Function expressions belong here now too, and they were not on any list before `map` and
`filter` existed to want them.

## Testing

- Unit tests inline per module (lexer, parser).
- A `tests/` corpus of `.qn` programs paired with expected output, run as integration
  tests. This is the suite that matters — it's what catches regressions as the
  evaluator changes shape, and it should grow with every feature.

### Four companion files, asserting four different things

A case's `.out` holds what the program printed. Its `.err` holds the message it failed
with. Its `.report` holds the whole rendered diagnostic — header, source line, carets,
every label, the help. Its `.in` is the odd one out and the newest: it is an *input*,
what the case reads from standard input, and absent means empty.

`.in` arrived with `io.line`, which would otherwise have been the one member of the
library with no case behind it — a terminal is not something a suite can arrange. It is
optional for a related reason to `.report`: a case that never reads should not have to
say so.

A case is also allowed to be a *directory* rather than a file, holding a `main.qn` and
the modules it imports. The companions are named for the case and sit beside it either
way, so a case growing a second file changes nothing about how it is checked. A directory
case's report names whichever file actually raised — which is usually one of the imported
ones, and is the point of `imports_error`.

The first two were the whole story until the sweep, and between them they left the
milestone's own subject untested. `.err` compares `err.message`, and `message` is the one
part of a diagnostic with no spans in it, so 88 error cases asserted a sentence and not one
of them asserted where the caret landed. A caret could point at the wrong token, at the
whole statement instead of the operand, or at byte zero, and the suite stayed green. The
only span coverage was a handful of unit tests in `error.rs` against errors built by hand,
which check the renderer and cannot check what the interpreter hands it.

`.report` is optional and the other two are not, because they are contracts at different
levels. `message` is what a `catch` sees and what a program may be written against; the
report is what a person reads. Pinning every case's rendering would make every future
change to the renderer an 88-file diff, and most of those files would be asserting the same
three shapes. So a case opts in: create an empty `.report`, run with `QUINCE_BLESS=1`, and
the harness fills it. Blessing never *creates* one, which is the property that matters — it
cannot quietly adopt a diagnostic nobody chose to pin, and a report that changes has to be
read before it is accepted.

The path in a report's location line is the case's own file name rather than its path on
disk, so the files do not bake in where the repository lives.

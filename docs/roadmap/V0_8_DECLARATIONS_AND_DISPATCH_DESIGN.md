# Quince v0.8 — declaration modifiers and typed dispatch

Design for the milestone after v0.7. Everything here was in v0.7's first draft and was moved
out of it: these are features that *read* the parameter types v0.7 adds without being part
of the type system, and together they are their own milestone.

The shape is uniform, which is why they belong together. Each item is a modifier a
declaration may carry and a rule the resolver enforces about it — `const fn`, `override`,
`final`, `explicit` — plus the one change to dispatch those modifiers make possible.

---

## 1. What this milestone adds

1. **`const fn` and `const op`.** Pure, read-only functions and methods, enforced by the
   resolver rather than by convention. §3.1.
2. **`override` and `final` on members.** Explicit overriding, and guarding against it. §3.2.
3. **Implicit constructor coercion, and `explicit` to refuse it.** §3.3.
4. **Default constructor auto-initialization.** `let x: T` with no initializer, and the
   implicit `op init()` that makes it possible. §3.4.
5. **Typed `fn` and `op` overloading.** Several definitions sharing a name, dispatched on
   argument types. §3.5.
6. **Default parameters and keyword arguments.** §3.6. They are here rather than deferred
   because they cannot be decided *after* overloading — see the note at the head of §3.6.
7. **Exponentiation (`**`) and compound assignment (`+=`, `//=`, `<<=`, …).** §3.7. Neither
   is a declaration modifier; both are here because they are the operator surface every
   later document already assumes exists, and nothing else claims them.

The shape stated at the top of this file holds for items 1–5. Items 6 and 7 are here for
sequencing reasons rather than thematic ones, and both are named in §5's tranche list so
that the milestone's weight is visible rather than implied.

---

## 2. What v0.7 leaves in place

- **Parameter and return type annotations.** Every feature here reads them. Without v0.7
  there is nothing for `explicit` to coerce against and nothing for overloading to dispatch
  on, so this milestone does not start early.
- **`const` means deep freeze**, on a binding (`BindKind::Const`) and now on a parameter or
  return (`const T`). §3.1 adds a third position, and v0.7 §3.3 argues they are one idea.
- **Visibility.** Modifier ordering here (`public const fn`) assumes v0.7's keywords exist.
- **`let x: int` with no initializer is refused** in v0.7. §3.4 is what relaxes it, and only
  for types that can answer what their default is.
- **Generics are not here yet.** Examples use non-generic classes; every rule below applies
  unchanged to the generic classes v0.9 adds, and v0.9 says so where it matters.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `const` | exists (bindings, and `const T` in v0.7) | **new use:** `const fn`, `const op` |
| `override` | **new keyword** | explicit member overriding (`override fn name`) |
| `explicit` | **new keyword** | forbids implicit constructor coercion (`explicit op init`) |
| `final` | exists (bindings, fields) | **new use:** guards a method or op against overriding |
| `**` | **new token** | exponentiation, reaching `op pow`. §3.7 |
| `+=` `-=` `*=` `/=` `//=` `%=` `**=` | **new tokens** | compound arithmetic assignment. §3.7 |
| `&=` `\|=` `^=` `<<=` `>>=` | **new tokens** | compound bitwise assignment. §3.7 |

`override` and `explicit` are reserved. Neither appears as an identifier in the corpus.

---

## 3. Syntax

### 3.1 `const fn` and `const op`

Prefixing a function or method declaration with `const` marks it **pure and read-only**:

```quince
class Point {
    public final x: float
    public final y: float

    op init(x: float, y: float) {
        self.x = x
        self.y = y
    }

    # Guarantees no mutation of self or of heap state
    const fn distance_to(other: Point): float {
        let dx = self.x - other.x
        let dy = self.y - other.y
        return math.sqrt(dx * dx + dy * dy)
    }

    const op string(): string {
        return "(" + string(self.x) + ", " + string(self.y) + ")"
    }
}

# A standalone pure function, depending only on its inputs
const fn add(a: int, b: int): int {
    return a + b
}
```

Rules:

- **Modifier ordering.** The canonical order is visibility, then `const`: `public const fn`,
  `private const fn`. The parser accepts any order and normalizes. Omitting visibility
  defaults to `public`, and an `op` is always public, so `const op` is the whole form.
- **Enforced at resolution**, by `sema/resolve/walk.rs`, before anything runs:
  - Field assignment (`self.x = val`) or index mutation (`self.items[0] = val`) inside a
    `const fn` or `const op` is refused.
  - Calling a non-const method on `self` from inside a `const fn` is refused:
    `const method 'distance_to' cannot invoke non-const method 'reset'`.
  - Reassigning a global or any non-local binding is refused.
- **What "pure" means here** is *state*, not effects. A `const fn` cannot mutate receiver
  fields, arguments, or globals. `print` is allowed: I/O alters no heap memory, and a rule
  that made debugging a `const fn` impossible would be a rule people route around.
- **`throw` is allowed**, and so is an early `return`. Control flow is not mutation. This is
  also what lets v0.10's `?` operator appear inside a `const fn`.
- **What it buys.** A `const fn` can be cached and reordered across expressions by the AST
  pass, and by the bytecode VM later. That is the payoff, but it is not the reason to write
  one — the reason is that the resolver will hold you to it.

### 3.2 Overriding and final guards (`override`, `final`)

```quince
class Vector2D {
    public final x: float
    public final y: float

    op init(x: float, y: float) {
        self.x = x
        self.y = y
    }

    # Final operator: no subclass may replace it
    public final op string(): string {
        return "(" + string(self.x) + ", " + string(self.y) + ")"
    }

    public op add(other: Vector2D): Vector2D {
        return Vector2D(self.x + other.x, self.y + other.y)
    }
}

class NamedVector extends Vector2D {
    public final name: string

    op init(name: string, x: float, y: float) {
        super.init(x, y)
        self.name = name
    }

    # `override` is required when replacing a superclass member
    public override op add(other: Vector2D): Vector2D {
        let res = super.add(other)
        return NamedVector(self.name, res.x, res.y)
    }

    # Refused at resolution: op string is final in Vector2D
    public override op string(): string {
        return self.name
    }
}
```

Rules:

- **`override` is required.** Shadowing a superclass `fn` or `op` without writing it is
  refused: `DeclarationError: operator 'add' overrides superclass operator but is missing
  'override' keyword`.
- **`override` on something that overrides nothing is also refused.** Both halves matter:
  the keyword is worthless as documentation if it can be written where it is not true, and
  a typo'd method name is exactly the mistake it should catch.
- **`final` guards a member.** `public final op string()` forbids replacement:
  `TypeError: cannot override final operator 'string' in class 'Vector2D'`.
- **`super` reaches the original.** `super.method(…)` and `super.op_name(…)` run the
  superclass implementation on this object, which is what `super` already does for `init`.
- **`final` on a member and `final` on a binding are the same word for the same idea** —
  this name is bound once, and cannot be rebound. On a field it is the value; on a method it
  is the implementation.

### 3.3 Implicit constructor coercion, and `explicit`

When an annotated binding, field, or parameter names a class `TargetClass`, assigning a
value of type `SourceType` invokes `TargetClass(value)` automatically, provided
`TargetClass` declares a single-parameter `op init(value: SourceType)`:

```quince
class CustomInt {
    private let value: int

    public op init(value: int) {
        self.value = value
    }
}

# Implicitly invokes CustomInt(10)
let i: CustomInt = 10
```

The `explicit` keyword refuses it:

```quince
class DatabaseConnection {
    private let timeout_ms: int

    public explicit op init(timeout_ms: int) {
        self.timeout_ms = timeout_ms
    }
}

let db: DatabaseConnection = 1000                     # TypeError: constructor 'init' is explicit
let db: DatabaseConnection = DatabaseConnection(1000) # accepted
```

Rules:

- **Implicit by default.** A single-parameter `op init` coerces unless it says otherwise.
- **Only single-parameter constructors coerce.** There is no rule that could pick among
  several arguments from one value.
- **Only one step.** Coercion does not chain: if `A` coerces from `B` and `B` from `int`,
  `let a: A = 1` is refused. One implicit call is a convenience; a search is a mystery.
- **The payload is checked first.** If the value does not hold as `SourceType`, coercion is
  refused with the type error, not attempted and failed inside the constructor.
- **`explicit` is the escape hatch** for a constructor whose argument is not a conversion —
  `DatabaseConnection(1000)` reads as a timeout only because the call names the class, and
  `let db: DatabaseConnection = 1000` reads as nothing at all.

**Why implicit-by-default rather than explicit-by-default**, which is the reverse of C++'s
answer: the classes this exists for are the ones that wrap one value and mean it —
`Username extends string`, `Money`, `CustomInt`. Those are also the classes the language
already makes easy, via the implicit `op init` on a builtin subclass. Making the common case
silent and the surprising case marked matches which is which here. C++ chose the other way
because its conversions compose into search, and §3.3's one-step rule is what makes that not
apply.

### 3.4 Default constructor auto-initialization (`let x: T`)

v0.7 refuses `let x: int` with no initializer, because there is no honest default. This
milestone relaxes it for types that can answer:

```quince
class Logger {
    private let entries: list[string]   # auto-initialized to []

    public op init() {
        # entries is already [] before this body runs
    }

    public fn log(msg: string) {
        self.entries.push(msg)
    }
}

let logger: Logger               # auto-initialized via Logger()
let items: list[int]             # []
let config: dict[string, string] # {}

logger.log("ready")
print(items)                     # []
```

Rules:

- **Unannotated and uninitialized** — `let x`, or a field `private let data` — stays what
  v0.7 says it is: a dynamic binding holding `nil`. This rule is about annotations.
- **Implicit default constructor synthesis.** A class declaring **no `op init` at all** gets
  `public op init() {}` synthesized, so it is default-constructible. This is the same
  reasoning that already gives a builtin subclass an implicit `op init`.
- **Suppressed by any parameterized constructor.** Declaring `op init(val: int)` suppresses
  the synthesized one; `let obj: MyClass` is then refused unless a zero-argument
  `op init()` is also written. A class that requires an argument means it.
- **Built-in collections answer for themselves.** `list` → `[]`, `dict` → `{}`.
- **Otherwise refused**, at resolution:
  `TypeError: type 'int' has no default constructor and requires an initializer`.
- **Fields initialize before `init` runs**, which is what makes the `Logger` example above
  work without a line in the constructor.

### 3.5 Typed `fn` and `op` overloading

A class or `extend` block may declare several `fn` methods or `op` handlers sharing a name,
as long as their parameter type signatures are distinct:

```quince
class Vector {
    public final x: float
    public final y: float

    op init(x: float, y: float) {
        self.x = x
        self.y = y
    }

    public op add(other: Vector): Vector {
        return Vector(self.x + other.x, self.y + other.y)
    }

    public op add(scalar: float): Vector {
        return Vector(self.x + scalar, self.y + scalar)
    }
}

extend list {
    public op mul(factor: int): list {
        let result: list = []
        for val in self {
            result.push(val * factor)
        }
        return result
    }
}

let numbers = [1, 2, 3, 4]
print(numbers * 2)     # [2, 4, 6, 8] — matches `factor: int`
print(numbers * 2.4)   # TypeError: no matching `op mul` for parameter types (list, float)
```

Rules:

- **Signature uniqueness.** Declarations sharing a name within a class or `extend` block
  must have distinct parameter type signatures. An identical signature is a duplicate error
  at resolution.
- **Dispatch is on run-time argument types**, exact match before widened match — so an `int`
  argument prefers an `int` parameter over a `float` one, and reaches the `float` overload
  only when there is no `int` one. This is the same widening rule as v0.7 §4.1 and not a
  second one.
- **Ambiguity is refused at resolution, not at the call.** Two overloads that could both
  match some argument by widening — `f(x: float)` and `f(x: int?)` — are refused where they
  are declared. A dispatch failure at run time should mean "nothing matched", never "two
  things did".
- **Unannotated parameters make a signature that matches anything**, and so a class may have
  at most one unannotated overload for a name. It is tried last.
- **No matching signature is a `TypeError`** naming the argument types that matched nothing.
  This applies everywhere: `extend` blocks, classes, and top-level functions.
- **Extensions coexist across modules** as long as their signatures do not collide. A
  collision is found when the second `extend` block is resolved.
- **Overloads are all-or-nothing across a subclass boundary.** An `override` replaces the
  one signature it matches, not the whole set. Overriding `add(other: Vector)` leaves
  `add(scalar: float)` inherited and reachable.

**This is the milestone's one risky item.** It changes dispatch, which every call site goes
through, and it is the only thing here whose cost is not bounded by the resolver. §5
sequences it last for that reason.

### 3.6 Default parameters and keyword arguments

An earlier draft deferred both, on the grounds that defaults interact with §3.5's
overloading in a way §3.5's rules do not cover. That is the reason they are **here** rather
than later: a defaulted parameter changes what "the signatures a name has" means, and
adding it after overloading ships would mean revisiting every rule in §3.5 with programs
already written against them. The interaction is settled below, in the last rule.

Function and method declarations support **default parameter values**, and callers can pass
arguments using **keyword argument syntax (`param_name: expr`)**:

```quince
fn connect(host: string, port: int = 8080, timeout: int = 3000): Connection {
    return Connection(host, port, timeout)
}

# Positional call: uses port 8080, timeout 3000
let c1 = connect("localhost")

# Target specific defaulted parameters by keyword:
let c2 = connect("127.0.0.1", timeout: 5000)

# Pass all arguments by keyword in any order:
let c3 = connect(timeout: 5000, host: "api.domain.com", port: 443)
```

Rules:

- **Defaulted parameters follow mandatory ones.** A mandatory parameter after a defaulted
  one is refused at resolution: there is no call that could reach it positionally.
- **Keyword arguments match declared parameter names.** `param_name: expr`, at any position
  after the last positional argument. A positional argument following a keyword one is
  refused — that ordering has no reading that is not a guess.
- **A parameter may be filled once.** Supplying a parameter positionally and again by
  keyword is an error naming it, rather than last-wins.
- **Defaults are evaluated at the call**, in the callee's declaration scope, each time. An
  expression default (`fn f(xs: list = [])`) therefore builds a fresh list per call, and
  does not carry mutations between them. This is the one place Python's answer is refused
  outright, and it is refused because the alternative is the single most reported footgun in
  that language.
- **`?` on the type does not imply a default.** `fn f(x: int?)` requires an argument; only
  `= nil` makes one optional. An annotation says what a parameter may hold, not whether it
  must be written.

**The overloading interaction**, which is the rule §6 used to defer this feature over:

- **A declaration contributes one signature per callable arity.** `fn f(a: int, b: int = 0)`
  contributes both `(int)` and `(int, int)`, and both are checked against §3.5's duplicate
  and ambiguity rules. So `fn f(a: int)` declared beside it is a **duplicate**, refused where
  the second one is written — not a silently preferred exact match.
- **Overload selection runs before defaults are filled in.** Selection sees the arity the
  call actually wrote; the winning declaration then synthesizes what the call omitted. That
  ordering is what keeps §3.5's "a dispatch failure means nothing matched, never that two
  things did" true in the presence of defaults.
- **A keyword call selects among overloads by name as well as type.** Two overloads reachable
  by the same keyword set with the same types are the ambiguity case above, and are refused
  at declaration like any other.

### 3.7 Exponentiation and compound assignment

Neither is a declaration modifier, and both are in this milestone because they are operator
surface that `BYTECODE_VM_DESIGN.md` and its phase documents already assume — an `OpCode::Pow`
and twelve compound-assignment opcodes are specified there against a language that cannot
parse `**` or `+=`. Scheduling them here is what makes those opcodes reachable.

```quince
print(2 ** 10)      # 1024
print(2.0 ** 0.5)   # 1.4142135623730951

let n = 5
n += 3              # 8
n **= 2             # 64
n //= 5             # 12

let xs = [1, 2]
xs += [3]           # [1, 2, 3] — a new list, exactly as `xs = xs + [3]` is
```

Rules:

- **`**` reaches a new `Op::Pow` slot**, joining `OPS` with the return contract every other
  arithmetic op has. It is **right**-associative — `2 ** 3 ** 2` is `2 ** (3 ** 2)` — which
  is the one place it differs from every other binary operator in the language, and it
  differs because left association would make the operator useless for what it is for.
- **`**` binds tighter than unary minus.** `-2 ** 2` is `-(2 ** 2)`, following Python and
  ordinary mathematical notation.
- **An `int ** negative-int` answers a `float`**, because the integer result does not exist.
  That is the same rule `/` already follows and not a new one. Overflow stays checked.
- **`a op= b` is defined as `a = a op b`**, evaluating the target expression once.
  `d[f()] += 1` calls `f` a single time — which is the whole reason this is a language form
  rather than something a program writes out.
- **Compound assignment reaches the same `op` as the binary operator**, and there is no
  separate in-place slot. A class defining `op add` gets `+=` for free; a class wanting
  in-place mutation writes a method and says so. Adding `op add_assign` beside `op add` would
  double the operator table for a distinction the language has no other place to make.
- **The target must already be bound**, and the usual `final` and `const` rules apply
  unchanged. `n += 1` on a `final n` is refused where `n = n + 1` is.

---

## 4. Enforcement

**At resolution:**
- `const fn` / `const op` mutation: field assignment, index-set, calling a non-const method
  on `self`, reassigning a non-local. §3.1.
- An `fn` or `op` that shadows a superclass member without `override`. §3.2.
- An `override` of a `final` member, or of a member no superclass declares. §3.2.
- Duplicate parameter signatures within a class or `extend` block. §3.5.
- Two overloads that could both match one argument by widening. §3.5.
- More than one unannotated overload for a name. §3.5.
- An uninitialized `let x: T` where `T` has no zero-arity constructor. §3.4.
- A mandatory parameter declared after a defaulted one. §3.6.
- A keyword argument naming no parameter, a parameter filled twice, or a positional
  argument after a keyword one. §3.6.
- A declaration whose defaulted arities collide with another overload's. §3.6.
- Compound assignment to a `final` or `const` binding. §3.7.

**At run time:**
- Overload dispatch: resolving argument types to a matching signature, or raising a
  `TypeError` naming the types that matched nothing. §3.5.
- Implicit constructor coercion, and its refusal when the constructor is `explicit`. §3.3.
- The coerced payload against the constructor's declared parameter type. §3.3.
- Default expressions, evaluated per call in the callee's declaration scope. §3.6.
- `op pow`, and the `int ** negative` promotion to `float`. §3.7.

---

## 5. Work items, in order

**Tranche 1 — `override` and `final`.** Two modifiers, two resolution rules, no new
machinery. It is the smallest item and it makes the class model honest, so it goes first.

**Tranche 2 — `const fn` and `const op`.** The modifier is trivial; the mutation analysis is
not. It needs to walk a body looking for assignment, index-set, and non-const calls, and to
know which of those reach `self`. Self-contained, and nothing else waits on it.

**Tranche 3 — default constructor auto-initialization.** Implicit `op init()` synthesis,
suppression by parameterized constructors, field initialization before the constructor body.
Small, but it touches instantiation, so it wants to land while nothing else is moving there.

**Tranche 4 — implicit coercion and `explicit`.** Depends on tranche 3 having settled what
constructors a class has.

**Tranche 5 — `**` and compound assignment.** Lexer, parser, one new `Op` slot, and the
desugaring in §3.7. Independent of everything above it and of everything below it, which is
why it can sit anywhere; it is here rather than first because it is the item most easily
finished under time pressure and least missed if it is not.

**Tranche 6 — default parameters and keyword arguments.** Before overloading, because
§3.6's arity rule is what overloading's duplicate check has to be written against. Doing it
after would mean writing that check twice.

**Tranche 7 — overloading.** Last, because it is the one that changes dispatch and the one
whose blast radius is every call. Duplicate and ambiguity checking at resolution first, then
run-time selection, then the `extend` cross-module case.

The cut line is after tranche 6. Overloading is the only item here that could be dropped and
leave a coherent language behind — but dropping it means dropping v0.10 §7.1's `op get`
overloading on index-or-`range` with it, so the two have to be cut together or not at all.

---

## 6. Deferred

**Return-type overloading.** Dispatch on what the caller wants rather than what it passes.
It needs bidirectional inference and it is not worth it.

**`const fn` as a memoization guarantee.** §3.1 permits caching; nothing in this milestone
does it. That is a VM item.

---

## 7. Decisions taken

- **Coercion is implicit by default, `explicit` opts out.** The reverse of C++, for the
  reason §3.3 gives: the one-step rule means conversions cannot compose into a search, which
  is what made C++ choose otherwise.
- **Coercion does not chain.** §3.3.
- **`override` is required, and is also refused where it is not true.** §3.2.
- **`final` is one word for one idea**, on bindings, fields, and members alike. §3.2.
- **`const fn` restricts state, not effects.** `print`, `throw`, and early `return` are all
  fine. §3.1.
- **Overload ambiguity is a declaration error, not a call error.** §3.5.
- **Dispatch widening follows v0.7 §4.1**, rather than defining a second rule. §3.5.
- **Default parameters land here, not later.** An earlier draft deferred them over the
  overload interaction; the interaction is the reason they cannot wait. A declaration
  contributes one signature per callable arity, and selection runs before defaults are
  filled in. §3.6.
- **A default expression is evaluated per call**, not once at declaration. §3.6.
- **`**` is right-associative and binds tighter than unary minus**, which makes it the one
  binary operator in the language that does not associate left. §3.7.
- **Compound assignment reaches the binary `op`**, with no separate in-place slot. §3.7.
- **These features are not the type system.** They read v0.7's annotations, which is why
  they are after it, and are not part of it, which is why they are not in it. §3.6 and §3.7
  are the two that do not fit that description, and §1 says so rather than pretending
  otherwise.

---

## 8. What shipped, and where it differs from the above

All seven tranches landed; the cut line after tranche 6 was not needed. Everything in §3 is
implemented as written except for what follows, which is kept here beside the prediction
rather than folded into it — the difference is usually the useful part.

**Two examples in this document contradict each other, and §3.4 won.** §3.3 writes
`private let value: int` inside `CustomInt`, and §3.4 refuses exactly that: `int` has no
default constructor, so a field annotated with it needs an initializer. The corpus case
writes `private let value: int = 0`. The rule is the one §3.4 states; the example above it
was written before the rule was.

**Where the keyword-argument refusals are enforced.** §4 lists "a keyword argument naming no
parameter, a parameter filled twice, or a positional argument after a keyword one" at
resolution. Only the third is: it is decidable from the call alone, and the parser refuses
it. The other two need to know *which declaration the call reaches*, and in a dynamically
typed language that is not known until the callee has been evaluated — `f` is a binding, not
a link. Both are enforced at the call, where the parameter list is in hand and the report
can list the names that do exist. §4's placement assumed a static callee; the language does
not have one.

**Overload duplicate and ambiguity checking is at resolution, not at the parser.** §3.5 says
"refused at resolution", and that is where it is — but it is worth saying why it could not
be earlier, since every other declaration-shape check in this milestone is at the parser.
Aliases are expanded by the resolver, so `fn f(a: ScoreTable)` and `fn f(a: dict[string,
int])` are the same signature and the parser cannot see it.

**Container overloads are told apart, and fixing that fixed a v0.7 bug.** `list[int]` beside
`list[string]` looked ambiguous, because `holds` decided a container by *walking its
elements* — so an empty list satisfied every `list[T]`, and so did an empty list the program
had annotated `list[int]`. The annotation was sitting right there and was not consulted.

`holds` now reads the reified header first, which is what §3.9 stamped it for: a container
that crossed an annotated boundary was **built to hold** those types, and that is what it is.

**The disagreement v0.7 shipped with survived that change.** This section used to claim the
header settled it — that `is` read the header and `holds` did not, so teaching `holds` to read
it made the two agree. It did not. Both read the header; they compared what they found there
by *different rules*. `is` used `same_args_as` (identity), `holds` used `admits` (which knows
`any` is the top type), so `xs is list[any]` stayed `false` for a value a `list[any]` parameter
accepted, and `list` and `list[any?]` — one type under §3.10's elision rule — gave opposite
answers.

Both now go through one function, `sema::types::arguments_admit`, which reads every elided
argument as the `any?` it stands for and then compares by admission. Seven sites were deciding
that separately; one of them, `admitted`, made a `dict[string]` passed as a `dict[string, int]`
accept any write at all. See §3.10 and the v0.7 §3.9 correction.

One thing widens, and only one: `any` is the top type and takes whatever is there, so
`list[any]` still means "a list of anything". That is safe rather than a hole, because a
*write* is checked against the header and not against the annotation it arrived through —
`xs.push("s")` inside `fn f(xs: list[any])` is refused on the strength of the caller's
`list[int]`. Nothing else widens: a `list[int]` is not a `list[int?]`, because a `nil` written
through the second is a `nil` read out of the first.

What is left over is the container nothing described — `total([])`, where the literal is every
element type at once. That is a property of the *argument*, not of the declarations, so it
cannot be decided where they are written and is refused at the call: `more than one `total`
takes (list)`. §3.5's "ambiguity is a declaration error" holds for everything the annotations
can settle, which is the claim it was making; the runtime tie is the backstop for what they
cannot, and a program the resolver accepted still never silently picks between two matches.

A subclass creating a tie the annotations do not show is caught by the same backstop:
`sema::overload` compares written types and cannot know what descends from what.

**An operator reports against the expression; a call reports against the parameter.** §3.5
asks for `TypeError: no matching op mul for parameter types (list, float)`. What is reported
is `cannot multiply list and float` — the sentence every *other* binary type error already
uses — with a label on each operand, the operator marked in between, and a help line naming
what the class does declare. Unifying with the existing report rather than inventing a
second one is deliberate: a reader should not be able to tell from the shape of a diagnostic
whether the class declared the slot and refused the operand or never declared it at all.
Those are the same mistake from where they are standing, and the help line is where the
difference belongs.

The rule behind *where* it lands is worth stating, because it decides every one of these
reports:

- An **ordinary call** refuses at the parameter that would not take the value — "`host` is
  `string`, but this is an int". The reader wrote the argument and can see the declaration,
  and naming the parameter says more than a list of signatures would.
- An **operator** cannot. The parameter is one nobody wrote, and the caret would land inside
  a class rather than on the expression that failed. So the operators check the fit before
  calling, and report at the operand — see `Interp::op_for`.

That covers every operator taking an operand. The binary ones — the arithmetic and bitwise
slots, `lt`, `gt`, and `cmp` — go through `binary_op_for` and land on `type_error`'s shape.
`contains`, `get`, and `set` keep `op_for`, which draws one caret: `x[i]` and `needle in x`
have no pair of operand spans to label, and three labels on one range renders as noise.
Either way the sentence is the same whether the name carries one declaration or several,
which is what makes it a rule rather than a special case.

**`const fn` is blunter than §3.1 about containers.** Assigning through an index is refused
even for a container the call allocated itself. Telling that apart from a caller's container
is an escape analysis, and the resolver numbers slots. §3.1's rule is enforced as written;
what is extra is that `let d = {}` followed by `d["a"] = 1` is refused too.

**Purity reaches into a nested `fn`.** §3.1 does not say, and the two available answers point
opposite ways. `in_init` is cleared for a `fn` nested in an `op init`, because such a
function can run long after construction. `const` is *not* cleared, because a nested function
closes over the receiver and the enclosing locals — letting it mutate them would be the whole
promise escaping through a closure.

**`override` declines to answer for a superclass it cannot see.** `extends` names a binding
and the resolver has evaluated nothing, so a class imported from another module is a chain
this pass cannot walk. Where that happens the stray-`override` refusal is skipped, and only
that half: the check may miss an `override` that should have been written, and never accuses
one that was. The same one-directional wrongness `builtin_base` already carries.

**The REPL resolves each entry against what the session already bound.** Nothing in this
document says how a milestone about declaration rules meets a prompt that compiles one line
at a time, and the answer matters: an entry resolved against an empty world is an entry where
`override`, `final`, default construction, and overload ambiguity all quietly decline to
answer, because the class or the function they are about was declared on an earlier line.
`Interp::declarations` reads that world off what is *bound* — not off accumulated source,
because a REPL is not a file being appended to. Two rules follow:

- A declaration whose parameter types match one already bound **replaces** it. Retyping a
  declaration to change it is what a prompt is for, and it is spelled by writing the same
  signature.
- A declaration that some call would reach equally well as one already bound is **refused**,
  exactly as it is inside one compilation. That is not a redefinition of anything.

**Implicit coercion reaches whichever constructor the value fits.** §3.3 says "provided
`TargetClass` declares a single-parameter `op init(value: SourceType)`", and a class that
declares several constructors still declares one of those. The offer is made by whichever of
them takes one parameter this value holds as; the payload decides, not how many there are.

**A latent bug the milestone had to fix.** A class field's initializer was never resolved, so
`class C { let n = base }` panicked in the evaluator with "the resolver must run before
evaluation". Tranche 3 needs those expressions resolved — a synthesized `Logger()` is one —
and walking them fixed it. The scope they are resolved in is the one `Class::field_env`
evaluates them in, which for a subclass is the scope holding `super`.

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

**At run time:**
- Overload dispatch: resolving argument types to a matching signature, or raising a
  `TypeError` naming the types that matched nothing. §3.5.
- Implicit constructor coercion, and its refusal when the constructor is `explicit`. §3.3.
- The coerced payload against the constructor's declared parameter type. §3.3.

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

**Tranche 5 — overloading.** Last, because it is the one that changes dispatch and the one
whose blast radius is every call. Duplicate and ambiguity checking at resolution first, then
run-time selection, then the `extend` cross-module case.

The cut line is after tranche 4. Overloading is the only item here that could be dropped and
leave a coherent language behind.

---

## 6. Deferred

**Default parameter values.** `fn f(x: int, y: int = 0)`. It interacts with overloading —
two signatures that differ only past a defaulted parameter are ambiguous in a way §3.5's
rules do not cover — and it should be decided once, for functions and for v0.10's enum
variant fields, rather than twice.

**Named arguments at call sites.** Related, and the same reasoning.

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
- **These features are not the type system.** They read v0.7's annotations, which is why
  they are after it, and are not part of it, which is why they are not in it.

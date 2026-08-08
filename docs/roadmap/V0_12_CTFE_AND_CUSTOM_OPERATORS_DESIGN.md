# Quince v0.12 — compile-time evaluation and custom infix operators

Design for the milestone after v0.11. Two features, both of which move work from run time to
resolution time, and neither of which adds a value the language did not already have.

**This document used to hold four features** — CTFE, hygienic macros, type reflection, and
custom operators — in ninety-eight lines. Macros alone are a milestone: hygiene, quoting,
expansion order, and what a diagnostic looks like when it points inside an expansion are four
decisions, none of which that draft made. They moved to **v0.15**, with type reflection, which
is the thing macros are written to consume. What is left here is the two items that are about
the *compiler* rather than about the syntax tree, and they share a tranche list honestly.

---

## 1. What this milestone adds

1. **Compile-time function execution.** A `const fn` invoked in a constant position is
   evaluated during resolution. §3.
2. **Custom infix operators.** `operator |>` registers a symbol, a precedence, and an
   associativity; a class or `extend` block gives it meaning. §4.

---

## 2. What v0.8 through v0.11 leave in place

- **`const fn` already means pure**, enforced by the resolver: no field assignment, no index
  mutation, no non-const call on `self`, no non-local reassignment (v0.8 §3.1). That
  document ends the section with "A `const fn` can be cached and reordered across
  expressions by the AST pass, and by the bytecode VM later. That is the payoff, but it is
  not the reason to write one." **§3 is that payoff arriving**, and it needs no new modifier
  because v0.8 already made the promise the resolver holds you to.
- **`const` bindings are deeply frozen** (DESIGN.md, *Bindings*), which is what makes a
  compile-time result safe to share.
- **`OPS` is a closed set with declared return contracts** (v0.7 §3.7). §4 is the first
  thing in the language that adds to it at *program* rather than at language level, and §4.3
  is careful about what that costs.
- **`op` dispatch reaches the left operand's class** (DESIGN.md, *What a class may answer
  for*), including the right-operand fallback. §4 changes the symbol, not the mechanism.
- **Generic methods are v0.11 §5's.** `op |>[U](…)` in §4.2 is one of those, not a new form.
- **v0.9 §3.3's `const N: int` generic parameters** are the second consumer of §3: an
  argument must be a literal or a `const` binding today, and CTFE is what makes
  `array[int, size()]` — v0.10 §7.3's fixed-size storage — expressible at all.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `operator` | **new keyword** | registers a custom infix symbol. §4.1 |
| `associativity`, `precedence` | **contextual** | field names inside an `operator` block, not reserved |
| `const` | exists | **no new use** — §3 changes what an existing `const fn` may be asked to do |

---

## 3. Compile-time function execution

A `const fn` called in a **constant position** is evaluated during resolution, and the call
is replaced by its result:

```quince
const fn buffer_size(depth: int): int {
    return 1024 * (1 << depth)
}

const CAPACITY: int = buffer_size(3)          # 8192, computed at resolution

let scratch: array[int, buffer_size(2)] = …   # a const generic argument, v0.9 §3.3
```

Rules:

- **A constant position is one of three**: the initializer of a `const` binding, a const
  generic argument (v0.9 §3.3), and an argument to another `const fn` already being
  evaluated. Nowhere else. In particular a `let` initializer is **not** one — folding there
  would make the same source line sometimes run at resolution and sometimes not, decided by
  a keyword three lines up.
- **Every argument must itself be constant**, by the same rule. A `const fn` called with a
  run-time value in a constant position is refused, naming the argument.
- **The evaluator is the interpreter**, run at resolution against a fresh heap with the
  globals resolution has. This is the cheap answer and it is available precisely because the
  language ships a tree-walker; a bytecode compiler would have to decide whether to build a
  second one, and this milestone landing first means it does not have to.
- **A `const fn` that does not terminate must not hang the compiler.** Evaluation runs under
  a step budget, and exhausting it is a resolution error naming the function — not a
  timeout, not a hang. The budget is a fixed number of interpreter steps rather than a wall
  clock, so that a build is reproducible.
- **`throw` inside a compile-time call is a resolution error**, reported at the call site
  with the thrown value's message. It cannot be caught: there is no run time yet to catch it
  in. `try`/`catch` *within* the evaluated call works normally.
- **`print` is refused at compile time**, which is the one place v0.8 §3.1's "restricts
  state, not effects" does not carry over. A `const fn` may print when it is called at run
  time; the same function reached in a constant position cannot, because output whose
  ordering depends on resolution order is not output anyone can use.
- **The result must be a `const`-freezable value** — a primitive, a string, or a deeply
  frozen container of them. A `const fn` returning a class instance is refused in a constant
  position, because the instance would have to survive from resolution into run time and the
  object model has no representation for that. This is the honest bound on the feature and
  it is why §6 defers the rest of it.
- **Evaluation is memoized per call and argument list**, which is free here and is the
  "caching" v0.8 §3.1 promised.

**What this does not do** is make `const fn` mandatory for a compile-time call, or add a
`comptime` keyword. Both were considered. A separate keyword would mean two purity modifiers
with one meaning between them, which is the objection v0.7 §9 records against `pub`.

---

## 4. Custom infix operators

### 4.1 Registering a symbol

```quince
public operator |> {
    associativity: left
    precedence: 5
}

public operator <*> {
    associativity: left
    precedence: 11
}
```

Rules:

- **A symbol is one or more characters from a fixed set** — ``| > < * / + - ^ % & ~ @ ?`` —
  and may not be a symbol the language already lexes (`|`, `<`, `**`, `..`, `?.`, `??`, `->`,
  `=>`, and the rest of `token.rs`). Colliding with one is a resolution error naming it.
  The set deliberately excludes `.`, `:`, `=`, `,`, and every bracket, because those carry
  structure the parser needs before it knows what an expression is.
- **Lexing is maximal munch**, as it already is. Registering `<*>` therefore changes how
  `a <* > b` lexes, which is the cost of the feature and the reason §4.3 scopes it.
- **Precedence is an integer 1–20**, and the built-in operator levels are documented as
  fixed points on that scale so that a registration can be placed relative to them rather
  than guessed. Precedence 0 is reserved for assignment and cannot be taken.
- **Associativity is `left` or `right`.** There is no `none`; a non-associative operator is
  a parse error the language has no way to explain, and left is what every reader expects.
- **Registration is a declaration, not an expression.** It sits at the top level of a module,
  is hoisted like any other declaration, and may not appear inside a function.
- **Two registrations of one symbol must agree.** Identical precedence and associativity is
  a no-op; a disagreement is a resolution error naming both modules.

### 4.2 Giving it meaning

```quince
extend string {
    public op |>[U](f: function(string) -> U): U {
        return f(self)
    }
}

class Matrix {
    public op <*>(other: Matrix): Matrix {
        return self.matmul(other)
    }
}

fn shout(s: string): string { return s.trim().upper() + "!" }

print("  quince  " |> shout)     # QUINCE!

let c = a <*> b
```

- **The `op` name *is* the symbol.** `op |>`, not `op pipe`. A generated name would be a
  second thing to look up, and the whole point is that the operator and its implementation
  are spelled the same.
- **Dispatch is v0.7 §3.7's, unchanged**: the left operand's class, then the right operand's
  fallback, then a `TypeError` naming both types and the symbol.
- **`extend` may define one**, which is the exception to DESIGN.md's rule that an extension
  may not define an `op`. That rule exists so an extension cannot change how the language
  dispatches on a type — `extend int { op bool() }` changing what `if 0` means. A custom
  symbol has no language meaning to change: before the `extend` block, `|>` on a string is
  an error, and after it, it is a call. Nothing that worked differently before works
  differently after, which is the exact test the original refusal was written against.
- **The return contract is whatever the declaration says.** A custom `op` is not in `OPS`
  and so has no fixed contract to validate against — which is the one property a built-in
  slot has that these do not, and §4.3 is the reason that is acceptable.

### 4.3 What this costs, and the scoping that pays for it

A registration changes the **parser**, and the parser is shared by every file in a program.
So `operator |>` in any module changes how every other module lexes and parses — the same
bargain DESIGN.md records for `extend` before modules existed, arriving again and for the
same reason.

Unlike `extend`, this one is scoped from the start, because the cost of not scoping it is
higher: an extension that surprises you produces a wrong answer, and a registration that
surprises you produces a parse error in a file that does not mention the operator.

- **A registration is visible in the module that declares it and in modules that import it**,
  by the `public` rule v0.7 §3.6 already gives every other top-level declaration.
- **The parse is per-module.** A file's operator table is its own registrations plus those it
  imports, and two modules may use one symbol at different precedences as long as neither
  imports the other. This is the property that makes the feature safe and it is the
  expensive half to implement, which is why §5 sequences it as its own tranche.
- **Diagnostics name the registration.** An unparseable expression involving a custom symbol
  reports the symbol, its precedence, and the module that registered it, because the reader's
  first question is which of those they did not know about.

---

## 5. Enforcement

**At resolution:**
- A non-constant argument or expression in a constant position. §3.
- A `const fn` in a constant position that prints, or that returns a non-freezable value. §3.
- A compile-time call exhausting its step budget, or throwing. §3.
- A symbol colliding with a language token, or using a character outside the set. §4.1.
- A precedence outside 1–20, or an `operator` block inside a function. §4.1.
- Two visible registrations of one symbol that disagree. §4.1.
- An `op` for a symbol no visible registration declares. §4.2.

---

## 6. Deferred

**Hygienic macros and type reflection.** Moved to **v0.15**, for the reason at the head of
this file. They are one milestone with each other because a macro that cannot ask what a
type looks like is a macro that can only paste.

**Compile-time heap values.** §3 refuses a `const fn` returning a class instance. Allowing it
means a representation that survives resolution into run time — a serialized heap, in
effect — and that is the same work `.qnc` serialization needs (Phase 1A). It should be done
once, there.

**Custom prefix and postfix operators.** Infix only. A prefix registration competes with
every operand position in the grammar rather than with the gap between two, and the failure
mode is much worse.

**Overloading a built-in operator's precedence.** Registering `+` at a different level. It
would let one module change how every other module reads arithmetic, and there is no version
of that worth having.

**`const fn` evaluation of the standard library.** Nothing in `math` or `text` is marked
`const fn` today; marking them is a mechanical follow-up and not a design question.

---

## 7. Decisions taken

- **CTFE needs no new keyword.** `const fn` already promises purity and the resolver already
  holds you to it. §3.
- **Constant positions are a closed set of three.** A `let` initializer is not one, because
  the same line should not sometimes run at resolution. §3.
- **The compile-time evaluator is the interpreter**, which is available only because the
  tree-walker exists and is a reason to land this before the VM. §3.
- **A step budget, not a timeout**, so builds are reproducible. §3.
- **`print` is refused at compile time**, which is the one place v0.8 §3.1's effects rule
  does not carry. §3.
- **A custom `op` is named by its symbol.** §4.2.
- **`extend` may define a custom operator**, though it may not define a built-in one — the
  original refusal was about changing existing dispatch, and there is none to change. §4.2.
- **Operator registration is per-module and travels by `import`.** §4.3.
- **Infix only, and built-in precedences cannot be redefined.** §6.
- **This milestone is two features, not four.** Macros and reflection are v0.15. Head of file.

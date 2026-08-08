# Quince v0.14 — string interpolation and typed `catch`

Design for the milestone after v0.13. Two ergonomic features, both small, both about code
that programs already write out longhand.

They are one milestone because neither is large enough to be one and both are the same kind
of change: a syntax that desugars into a form the language already has, adding no new value
and no new semantics. That makes this the cheapest milestone in the roadmap and the last
one before macros (v0.15) start rewriting syntax rather than adding it.

**A third feature, the ternary conditional (`c ? t : f`), was in an earlier draft and is
refused rather than deferred.** §5 is the argument, and it is here rather than in a deferred
list because "refused" and "not yet" are different answers and the reasons are worth keeping.

---

## 1. What this milestone adds

1. **String interpolation** — `f"…{expr}…"`. §3.
2. **Typed `catch` branches** — `catch err: IOError`. §4.

---

## 2. What earlier milestones leave in place

- **`try`/`catch`/`throw` is v0.5's**, with an error as a value and `QuinceError` carrying a
  kind and a label (DESIGN.md, *Errors as values*). §4 adds branch selection and nothing else.
- **`is` narrows** (v0.7 §3.9), and a typed `catch` is the same test in a different position,
  which is the whole reason it costs so little.
- **`text.StringBuilder` is v0.13's**, and §3 desugars into it where it pays.
- **Error classes are shared across modules** (DESIGN.md, *Modules*), without which a typed
  `catch` in one file could not name a class thrown in another — the same property that made
  `catch` work at all.
- **`op string` is how a value renders** (v0.7 §3.7). An interpolation hole calls it, and does
  not invent a second formatting path.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `f"…"` | **new literal** | interpolated string. §3 |
| `{{` `}}` | **new escapes** | a literal brace inside an `f` string. §3 |
| `catch x: T` | **new form** | a typed catch branch. §4 |

`f` is not a keyword and is not reserved: the lexer recognizes it only immediately before a
quote, with no space, exactly as `b"…"` (v0.10 §7.4) is recognized. A variable named `f`
stays legal, which matters because it is what everyone calls a function parameter.

---

## 3. String interpolation

```quince
let name = "Alice"
let score = 95.5

print(f"Player {name} scored {score + 4.5}")     # Player Alice scored 100.0

# A literal brace doubles
print(f"{{{name}}}")                              # {Alice}

# Both quote styles interpolate
print(f'path: {path.join(dir, file)}')
```

Rules:

- **A hole holds any expression**, parsed with the ordinary expression parser and terminated
  by the matching `}`. Nesting is by brace counting, and a `}` inside a string literal inside
  a hole does not close it — which follows from the hole being lexed as source rather than
  scanned for a character.
- **`{{` and `}}` are a literal `{` and `}`.** The alternative, backslash-escaping them, would
  make `f"\{"` and `"\{"` mean different things in two literals that differ by one character.
- **A hole is rendered by `op string`**, the same slot `print` and `string(x)` reach. A class
  that decides how it prints decides how it interpolates, with nothing further to write.
- **Desugaring is to concatenation for two holes or fewer, and to a `text.StringBuilder`
  above that.** Both are `string`-producing expressions and the choice is invisible; it is
  stated because it is why this milestone comes after v0.13 rather than before it.
- **`f` composes with neither `b` nor any other prefix.** `bf"…"` is not a thing: bytes have
  no `op string` to call.
- **An `f` string with no holes is an ordinary string**, and the parser says so with a
  warning rather than an error — it is a leftover from editing, not a mistake with a
  consequence.
- **No format specifiers in this milestone.** `{score:.2f}` is not accepted, and §6 defers it.
  A `:` inside a hole is therefore whatever the expression parser makes of it — a dict
  literal's colon, most likely — which is the reading that stays correct if specifiers land
  later, since a specifier would have to be distinguished from that anyway.

**Why `f"…"` rather than bare `"…{x}…"`**: every existing string containing a brace would
change meaning, and `{` appears in every program that prints a dict. A prefix costs one
character and breaks nothing, which is the same trade `b"…"` made.

---

## 4. Typed `catch`

```quince
class ParseError extends Error {
    public final line: int

    op init(message: string, line: int) {
        super.init(message)
        self.line = line
    }
}

try {
    let text = io.read_file("config.qn")
    let ast = parse(text)
} catch err: IOError {
    sys.eprintln("could not read the file: " + err.message)
} catch err: ParseError {
    sys.eprintln(f"line {err.line}: {err.message}")
} catch err {
    sys.eprintln("unexpected: " + string(err))
    throw err
}
```

Rules:

- **A branch matches by `is`.** `catch err: ParseError` catches a `ParseError` and any
  subclass, using v0.7 §3.9's test and not a second one. That also means an interface works:
  `catch err: Retryable` is legal the day v0.11 lands, and needs nothing here.
- **Branches are tried in declaration order**, first match wins.
- **An unannotated `catch err` is the fallback**, matches anything, and must be last. A
  branch after it is unreachable and is refused at resolution — the same rule v0.10 §6.1
  applies to `match` arms, deliberately, because they are the same mistake.
- **A branch whose type is a subclass of an earlier branch's is also unreachable** and is
  refused the same way. `catch err: Error` followed by `catch err: ParseError` catches
  nothing in the second.
- **`err` is scoped to its own branch.** Each branch is its own scope, and two branches may
  bind different names.
- **Nothing matching means nothing is caught**, and the error propagates as if the `try` were
  not there. This is the one behavioural difference from today's `catch`, which catches
  everything, and it is what the feature is for — but it also means a `try` with only typed
  branches can let an error through, so the resolver warns when no untyped branch and no
  `Error` branch is present.
- **`err` is narrowed inside the branch.** `catch err: ParseError` may read `err.line`
  without a cast, which is v0.7's smart casting arriving in a new position.

**What this does not add** is a `finally`. It is deferred in §6, and it is a genuinely
separate feature: `catch` is about which error, and `finally` is about unwinding, which
DESIGN.md records as already being handled by the discipline every scope-pushing site
follows.

---

## 5. Refused: the ternary conditional

An earlier draft added `c ? t : f`. It is refused, and the reasons are the two tokens it
would spend:

**`?` already sits in three places**, and v0.10 §2.1 enumerates them precisely because the
parser has to tell them apart: `T?` in a type, `expr?` propagating an error, and the
two-character `?.` and `??`. A ternary makes a fourth, and it is the one that genuinely
collides — after a complete expression, `f(x)?` and `f(x) ? a : b` differ only in what comes
next, so the propagation operator would need lookahead past an arbitrary expression to a `:`
that may not be there.

**`:` is the most overloaded token in the language**, and v0.10 §7.1.1 *removes slice syntax*
rather than add a third meaning to it: "a third meaning for the most overloaded token in the
language is what §11's rule about one spelling per idea exists to prevent". Adding a ternary
one milestone after paying that price would be spending the same currency the roadmap just
declined to spend.

**What the language should do instead** is make `if` an expression, which `match` already is
(v0.10 §6.1) and which costs no token at all:

```quince
let status = if age >= 18 { "adult" } else { "minor" }
```

That is not scheduled here — it is a change to how `if` is parsed and it interacts with
statement-position `if`, so it wants its own argument. It is recorded in §6 as the thing to
do if the ergonomic gap is felt, so that the next person to want a ternary finds the reason
it was refused *and* the alternative in the same place.

---

## 6. Enforcement

**At resolution:**
- An unterminated hole, or an unmatched `{` or `}` in an `f` string. §3.
- A `catch` branch after the untyped fallback. §4.
- A `catch` branch made unreachable by an earlier, broader one. §4.
- A `catch` naming something that is not a class or interface. §4.

**Warnings at resolution:**
- An `f` string with no holes. §3.
- A `try` whose branches are all typed and none of which is `Error`. §4.

**At run time:**
- Branch selection by `is`. §4.
- `op string` on each hole. §3.

---

## 7. Work items, in order

**Tranche 1 — typed `catch`.** Grammar, branch selection, the reachability checks, narrowing.
It is the smaller of the two and the one with a correctness story, so it goes first.

**Tranche 2 — `f` string lexing and parsing.** The literal, hole extraction, brace escapes.

**Tranche 3 — desugaring and `op string`.** Concatenation, and the `StringBuilder` path above
two holes.

**Tranche 4 — editor support.** Holes highlighted as code rather than as string, completion
inside a hole, and a diagnostic whose span points inside the literal.

Tranche 4 is not optional decoration: an interpolated string whose contents the editor treats
as text is a feature that looks broken the first time anyone uses it, and the highlighting is
the part people see.

---

## 8. Deferred

**Format specifiers** — `{score:.2f}`, width, alignment, padding. It is a small language of
its own, it wants to be shared with a `format` function that does not exist, and §3's rule
about `:` inside a hole is written so that adding them later does not break anything written
before.

**`finally`.** §4.

**`if` as an expression.** §5. The alternative to the ternary, and the thing to reach for if
the gap is felt.

**Interpolation in `b"…"`.** §3.

**Re-throwing with context** — an error that carries the one it wrapped. It wants a field on
`Error` and a rendering rule, and it is a v0.5-shaped decision rather than an ergonomic one.

---

## 9. Decisions taken

- **Interpolation is prefixed `f"…"`**, because bare interpolation would change the meaning
  of every string containing a brace. §3.
- **A literal brace doubles**, rather than being backslash-escaped. §3.
- **A hole renders through `op string`**, with no second formatting path. §3.
- **No format specifiers yet**, and `:` inside a hole is left to the expression parser so
  that adding them stays possible. §3, §8.
- **A `catch` branch matches by `is`**, so subclasses and interfaces both work with no new
  rule. §4.
- **Unreachable `catch` branches are refused**, matching v0.10's rule for `match` arms. §4.
- **Typed-only `try` warns**, because letting an error through is the point of the feature
  and also the way to be surprised by it. §4.
- **The ternary conditional is refused, not deferred**, and `if` as an expression is the
  named alternative. §5.

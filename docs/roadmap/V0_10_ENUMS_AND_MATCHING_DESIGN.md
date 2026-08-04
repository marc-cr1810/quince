# Quince v0.10 — enums, pattern matching, and the containers that need them

Design for the milestone after v0.9. It adds algebraic data types (`enum`), the built-in
`Option[T]` and `Result[T, E]`, error propagation (`?`), exhaustive `match`, `if let`, and
four container types that have been waiting on something: fixed-size `array[T, N]`, binary
`bytes`, hash `set[T]`, and a real `range` object.

Written to the same shape as the three milestone documents before it and against the same
standard: every example here is meant to run, and where a feature reverses a decision the
codebase has already recorded, the reversal is argued rather than assumed.

---

## 1. What this milestone adds

1. **`enum`** — unit and payload-carrying variants, with methods and operators. §5.
2. **`Option[T]` and `Result[T, E]`** as built-in generic enums. §4.
3. **The `?` propagation operator** — unwrap, or early-return the failure. §4.3.
4. **Exhaustive `match`**, as an expression, over enums, tuples, and primitives. §6.
5. **`if let`** for the single-variant case. §6.5.
6. **`range`**, the first value in the language meaning "a to b". §7.1.
7. **A lazy iteration protocol** (`op next`), which `range` forces. §7.2.
8. **`array[T, N]`** — fixed-size contiguous storage, on v0.9's const generics. §7.3.
9. **`bytes`** — a sequence of `u8`, with a `b"…"` literal. §7.4.
10. **`set[T]`**, and the `set`-versus-`dict` literal rules `{…}` now needs. §7.5.
11. **Tagged-union layout and null pointer optimization.** §8.

`tuple` is **not** on this list. An earlier draft claimed it; it is specified in full in
v0.9 §3.5, and what this milestone adds is pattern matching over it (§6.3).

---

## 2. What v0.7 through v0.9 leave in place

This milestone is the fourth of four that began as one document, and it is not startable
without the three before it. Several of their pieces are load-bearing here.

- **`Type` carries arguments, and allocations carry a reified header** — v0.7 tranche 2.
  This, and not v0.9, is what `Option[T]` and `Result[T, E]` are built out of. They are
  *built-in* generics, so they need exactly what `list[T]` and `dict[K, V]` needed and
  nothing more; an earlier draft said this milestone waited on user generics because
  `Option[T]` is a generic enum, and that conflated the two. A built-in generic implemented
  in Rust does not go through `class Name[T]` any more than `list` does.
- **v0.9 is still a prerequisite, for three narrower things:** `tuple`, without which §6.3's
  tuple patterns have nothing to match; `const N: int`, without which `array[T, N]` (§7.3)
  has no arity; and the generic parameter-list grammar, without which a *user* cannot write
  `enum Tree[T]` even though the built-ins are generic. Drop all three and enums could in
  principle precede generics — but tuple patterns are half of what makes `match` worth
  having, so the ordering stands.
- **`T?` and `nil` already mean absence.** `Option[T]` is a second answer to a question v0.7
  answered, and §3 settles that before anything else here is built.
- **`tuple` is v0.9's.** This milestone matches on it, and does not define it.
- **Slicing is `x[a:b]`,** a `Slice` node, because there was no value meaning "1 to 3".
  §7.1 introduces one, and §7.1.1 removes the `:` form in favour of it.
- **`op iter` returns a list and is eager**, by a decision DESIGN.md records and defends.
  §7.2 reverses it, and argues for the reversal there rather than assuming it.
- **`len`, `print`, `type` are globals.** `len(xs)`, never `xs.len()`.
- **Dict keys are `dict::Key`'s closed set.** `set[T]` inherits that constraint (§7.5), and
  enums join `tuple` and classes in the queue behind v0.7 §8's deferred `op hash`.
- **Declaration modifiers and overloading are v0.8's.** Enum methods carry `public`,
  `const`, `override`, and `final` with no new rules (§5.2), and `op get` overloading on
  index-or-`range` (§7.1) is v0.8 §3.5's mechanism, not a new one.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `enum` | **new keyword** | algebraic data type declaration |
| `match` | **new keyword** | exhaustive pattern-matching expression |
| `if let` | **new form** | single-variant pattern unpacking (§6.5) |
| `?` (postfix) | **new position** | error propagation on an expression (`f()?`) |
| `..` | **new** | range construction (`a..b`, `..b`, `a..`, `..`, `a..b..step`), replacing `x[a:b]` |
| `=>` | **new** | separates a match pattern from its arm |
| `b"…"` | **new literal** | `bytes` literal |
| `_` | exists (v0.7 wildcard type) | **new use:** wildcard *pattern* in `match` |
| `{` `}` | exists (dict literals, blocks) | **new use:** `set` literals (§7.5) |

`enum` and `match` are reserved. Neither appears as an identifier in the corpus, so the
`the_editor_grammar_spells_every_keyword` guard and a corpus run are enough to land them.

**`?` now sits in three places** and the parser must tell them apart: `T?` in a *type*
position, `expr?` in an *expression* position, and the two-character `?.` and `??`. They do
not truly collide — a type position never holds an expression, and `?.` and `??` are lexed
as single tokens ahead of a bare `?` — but `let x: int? = f()?` puts all three on one line
and is the first test to write.

---

## 3. The decision that gates the rest

v0.7 gives the language `T?` and `nil`. This milestone gives it `Option[T]` with
`Some`/`None`. Those are two mechanisms for absence, and shipping both without a stated
relationship is the worst available outcome: every API author picks one by coin flip and
every caller learns both.

v0.7 §10 laid out the three coherent answers and **has since settled on answer 1** — `T?`
is sugar for `Option[T]`. This document was written on that assumption and keeps it:

- `int?` and `Option[int]` are one type with two spellings. `nil` is how `Option.None` prints
  and how it is written in a value position.
- `?.` and `??` are sugar over `match`, and behave exactly as v0.7 §3.8 specifies.
- `expr?` (§4.3) works on either spelling, because there is only one thing there.
- `if let Option.Some(x) = …` and `if x is int` narrow the same value, and both are allowed
  to (§6.5).
- v0.7's `d[key] -> V?` needs no change and *gains* something: `Option[Option[int]]` is a
  real type where `int??` was refused, so a dict can finally distinguish a missing key from
  a stored `nil`. Whether `d[key]` should therefore answer `Option[V]` in its written
  signature is the one loose thread, and §10 keeps it.

Had the answer gone to 2 or 3 instead, §4, §6.5, and §7.2 are the sections that would have
changed, and they would have changed substantially. That is why this is §3 and not an
appendix, and it is why v0.7 settled the question before its tranche 3 rather than after
its tranche 4.

---

## 4. Built-in `Option[T]` and `Result[T, E]`

`Option[T]` and `Result[T, E]` are built-in generic enums, and they are **PascalCase**
against the lowercase convention every other built-in type follows (`int`, `list`, `dict`,
`set`, `tuple`, `bytes`, `range`). That is a deliberate exception and it is worth the two
paragraphs.

**The decisive reason is that a lowercase `result` would burn the identifier.** A built-in
type's name is not merely conventional in Quince — the resolver refuses to let a binding
take it, with `` `list` is the name of a type built into the language / a type's name cannot
also be a variable``. So a lowercase `result` would make `let result = …` a hard error in
every program forever, and `tests/cases/extend_op_additive.qn` already contains one. It is
among the most common variable names in programming, and it is exactly the name you reach
for inside the function that builds one:

```quince
fn parse(s: string): Result[int, string] {
    let result = 0              # fine — `Result` is the type, `result` is a variable
    …
    return Result.Ok(result)
}
```

Under the lowercase spelling that function cannot be written at all. No other built-in has
this problem, because nobody names a variable `list` inside the function that calls `list()`.

**The second reason is that it makes a rule rather than an exception.** Every lowercase
built-in is a *class*; `Option` and `Result` would be the only built-in *enums*. Reading the
convention as "built-in classes are lowercase, enums are PascalCase" costs nothing and gains
consistency where it is actually looked at: `Result.Ok` and `HttpStatus.Ok` are the same
shape, and a reader learns one story about how a variant is written rather than two.

What is lost is real and small: casing no longer tells you on sight whether a type is
built-in. It told you that for classes and it still does.

### 4.1 `Option[T]`

```quince
builtin enum Option[T] {
    Some(value: T),
    None
}

fn find_user(id: int): Option[string] {
    if id == 101 {
        return Option.Some("Alice")
    }
    return Option.None
}
```

`Option[T]` is the type `T?` is a spelling of (§3). `find_user` could equally be written
`fn find_user(id: int): string?` with `return nil`, and means the same thing.

### 4.2 `Result[T, E]`

```quince
builtin enum Result[T, E] {
    Ok(value: T),
    Err(error: E)
}

fn parse_port(s: string): Result[int, string] {
    if is_numeric(s) {
        return Result.Ok(int(s))
    }
    return Result.Err("Invalid port number: " + s)
}
```

`Result` gets no short spelling. It is also **not** how the language reports errors:
`try`/`catch`/`throw` landed in v0.5 and stays. `Result` is for a failure the caller is
expected to handle in the ordinary path; `throw` is for one that unwinds. A language with
both owes its reader a sentence on when to reach for which, and that is the sentence.

### 4.3 The `?` propagation operator

The `?` suffix unwraps on success, or **early-returns** from the enclosing function on
failure.

```quince
fn setup_server(config_str: string): Result[int, string] {
    # If parse_port answers Result.Err, `?` early-returns that err immediately.
    let port: int = parse_port(config_str)?
    return Result.Ok(port)
}
```

Rules:

1. **In a function returning `Result[T, E]`:** `expr?` unwraps `Ok(val)`. On `Err(e)` it
   early-returns `Result.Err(e)`, and `e` must hold as the declared `E`.
2. **In a function returning `Option[T]` (equivalently `T?`):** `expr?` unwraps `Some(val)`
   and early-returns `Option.None` on `None`.
3. **Mixing them is refused.** `expr?` on a `Result` inside a function returning `Option[T]`
   would discard the error — a thing to do on purpose, not by omitting two characters.
4. **In a function returning anything else**, `?` is refused at resolution:
   ```text
   DeclarationError: cannot use '?' operator in function returning 'int';
   function must return Result, Option, or T?
   ```
5. **Inside a `const fn`, `?` is allowed.** An early return mutates nothing, and v0.8 §3.1's
   restriction is about state, not control flow.

---

## 5. Enums

### 5.1 Declaration

```quince
enum Event {
    Log(message: string),           # Strictly typed field
    Payload(metadata: any?),        # Explicitly dynamic field
    Heartbeat                       # Unit variant: no payload, no parentheses
}

let e1: Event = Event.Log("Connection established")
let e2: Event = Event.Payload({"ip": "127.0.0.1"})
let e3: Event = Event.Heartbeat
```

The grammar, stated because an earlier draft left three of these to be guessed:

- **A variant's payload is a field list**, spelled `name: Type` exactly as a function's
  parameters are. There is no bare-identifier form: `Payload(metadata)` is refused, because
  it reads as a type to anyone who knows `list[int]` and as a name to anyone who knows
  `fn f(x)`, and the language should not have a form whose meaning depends on which the
  reader learned first. An untyped field is written `metadata: any?`.
- **Variants are comma-separated**, and the list ends at the closing brace or at the first
  `fn`, `op`, or modifier keyword. A trailing comma after the last variant is permitted.
- **A unit variant takes no parentheses**, in declaration or in use — `Event.Heartbeat`,
  never `Event.Heartbeat()`.
- **Field defaults are not in this milestone.** `Ok(code: int = 200)` appeared in an earlier
  draft. It needs rules for what `HttpStatus.Ok()` means and whether a defaulted field may
  be skipped mid-list, which is the whole of default arguments — a feature the language does
  not have for functions either. §10.

### 5.2 Methods and operators

Methods and operators follow the variant list, and everything v0.7 says about them applies
unchanged — visibility, `const`, overloading, `override`, `final`:

```quince
enum HttpStatus {
    Ok(code: int),
    NotFound(path: string),

    public const fn is_success(): bool {
        return match self {
            HttpStatus.Ok(_) => true,
            _ => false
        }
    }

    public const op string(): string {
        return match self {
            HttpStatus.Ok(code) => "HTTP " + string(code),
            HttpStatus.NotFound(path) => "404 " + path
        }
    }
}
```

### 5.3 What an enum is, against the rest of the language

- **Enums may be generic.** `enum Tree[T]` works — `Option[T]` is the proof, and a built-in
  using a form users cannot is not a form worth having.
- **Enums are closed.** No subclassing, and no `extend` block may add a variant.
  Exhaustiveness (§6.4) means nothing otherwise. An `extend` block *may* add methods.
- **An enum is not a dict key**, for the reason v0.7 §4.2 gives about classes: `dict::Key`
  cannot call back into the interpreter. It joins the queue behind `op hash`.
- **`is` works on enums and their instantiations.** `x is Option[int]` is `O(1)` against the
  reified header, as v0.7 §3.9 specifies for every generic.

---

## 6. Pattern matching

### 6.1 `match` as an expression

```quince
fn area(shape: Shape): float {
    return match shape {
        Shape.Circle(r) => 3.14159 * r * r,
        Shape.Rectangle(w, h) => w * h,
        Shape.Point => 0.0
    }
}
```

- **`match` is an expression**, and a statement only in the sense that any expression is.
  Every arm produces a value, and the type of the whole is the join of the arms' types —
  computed the way the inference pass already joins the branches of an `if`.
- **Arms are `Pattern => Expr`, comma-separated.** An arm needing statements uses a block,
  whose value is its last expression.
- **Arms are tried in order** and the first match wins. An arm made unreachable by an earlier
  one — a `_` before a specific variant, or a repeated variant — is refused at resolution
  rather than silently accepted.

### 6.2 Binding a payload

Payload fields are declared by name and may be bound either way:

```quince
match shape {
    Shape.Circle(r) => …,                         # positional, declaration order
    Shape.Rectangle(width: w, height: h) => …,    # by name, any order
    Shape.Point => …                              # unit variant, no parentheses
}
```

Positional binding must bind **every** field or none. Named binding may bind a subset, which
is what earns it a place: a variant with six fields and one interesting field should not
force four `_`s. The two forms may not be mixed within one pattern.

`_` binds nothing, in any position. It is the character v0.7 uses for a wildcard type
argument, in a different grammar position; no ambiguity arises, because a pattern is never
a type.

### 6.3 Tuple patterns

```quince
let point: tuple[int, int] = (0, 5)

let label = match point {
    (0, 0) => "Origin",
    (x, 0) => "X-Axis at " + string(x),
    (0, y) => "Y-Axis at " + string(y),
    (x, y) => "Point(" + string(x) + ", " + string(y) + ")"
}
```

Tuple patterns destructure positionally, the arity is known from the type, and a literal in
a pattern position matches by `==`.

### 6.4 Exhaustiveness

The resolver verifies that a `match` handles every case it could face:

```text
TypeError: match expression is not exhaustive; missing variant 'Shape.Point'
```

What "every case" means depends on the scrutinee:

- **An enum** is exhaustive when every variant is covered, or a `_` arm exists. This is the
  case the checker exists for.
- **`bool`** is exhaustive over `true` and `false`.
- **An unbounded domain** — `int`, `string`, `float`, and any tuple containing one — is
  exhaustive only through a `_` arm or an irrefutable binding arm like `(x, y)`. The checker
  does not reason about ranges, so `match n { 0 => …, 1 => … }` over an `int` is refused
  however the arms are written.
- **A nullable or `Option` scrutinee must cover `None`.** This is where §3 pays: under one
  unified mechanism, `match maybe_user { Option.Some(u) => … }` is refused for the same
  reason a missing enum variant is, and the reader learns one rule instead of two.

### 6.5 `if let`

`if let` extracts one variant's payload without a full `match`:

```quince
if let Result.Ok(port) = parse_port("8080") {
    print("Server listening on port:", port)
} else {
    print("Could not parse a port")
}

if let Option.Some(user) = find_user(101) {
    print("Found user:", user)
}
```

- The binding is scoped to the `if` block only. An `else` cannot see it, because the pattern
  did not match and there is nothing to see.
- **`else` is permitted**, and is an ordinary `else`.
- Patterns are §6.2's, so named and positional binding both work.
- **No `while let` in this milestone.** It is small, and it belongs with the iteration work
  in §7.2 rather than here. §10.
### 6.6 Class & Dict Destructuring

Beyond tuple destructuring (v0.9 §3.5) and enum pattern matching (§6.2), Quince supports class field and dictionary key destructuring in `let` bindings:

```quince
class User {
    public let name: string
    public let age: int
    private let token: string
}

let user = User("Alice", 30, "secret_tok")

# Destructure public fields:
let User { name, age } = user

# Dict key destructuring:
let config = {"host": "localhost", "port": 8080}
let {"host": server_host, "port": server_port} = config
```

Rules:
- **Strict Member Visibility Enforcement.** Class destructuring can **only** extract `public` fields. Destructuring `private` or `protected` fields outside class scope is refused at resolution with a `VisibilityError`.
- **Dict Destructuring.** Unpacks specific literal key values into new local variable bindings.

---

## 7. The containers that were waiting

### 7.1 `range` — the value that means "a to b"

`Op::Get`'s own documentation refuses to slice a class that declares it, *because* "there is
no value in the language that means '1 to 3'"; DESIGN.md records that slicing needed a
`Slice` node for the same reason. `range` is that value, and closing a hole the codebase has
been carrying is the strongest argument for this part of the milestone.

```quince
for i in 0..10 {          # half-open: 0 through 9
    print(i)
}

for i in 0..10..2 {       # 0, 2, 4, 6, 8
    print(i)
}

let numbers: list[int] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
let evens: list[int] = numbers[0..10..2]          # [0, 2, 4, 6, 8]
let front: list[int] = numbers[..4]               # [0, 1, 2, 3]
let back: list[int]  = numbers[-2..]              # [8, 9]
let copy: list[int]  = numbers[..]                # all of it
let odds: list[int]  = numbers[(1..)..2]          # [1, 3, 5, 7, 9]
```

- **`range` is a built-in class** with `start: int?`, `end: int?`, and `step: int`,
  half-open at the end. A `nil` bound means the natural one, which is how the omitted forms
  work and how the existing clamping behaviour is already specified.
- **`..` is a left-associative binary operator**, reaching `op range` on its left operand —
  plus `..b`, `a..`, and `..` as prefix, postfix, and bare forms. Only the binary form
  dispatches; the other three construct a `range` with `nil` bounds directly, there being no
  left operand to ask.
- **The postfix form is recognized when the next token cannot begin an expression** — `)`,
  `]`, `,`, `;`, or the end of a line. That is the rule that makes `numbers[-2..]` and
  `numbers[..]` work, and it is the rule §7.1.2 is a consequence of.
- **`{` counts as a terminator too**, so `for i in 0.. { … }` reads the brace as the loop
  body rather than as the range's end. This is the one place a `{` needs a rule: `{` is
  otherwise only an operand-position token and never competes with a block (§7.5), but a
  bare `a..` is *asking* for a right operand and would take one. Nothing is lost by
  refusing it, because a range bound is an int and `{` can only open a dict or a set.
- **The step is the same operator applied again**, and needs no grammar of its own.
  `int` answers `op range(end: int)` with a `range`; `range` answers `op range(step: int)`
  with itself, stepped. So `0..10..2` is `(0..10)..2` under ordinary left associativity —
  a range of ranges never arises, because `range`'s own overload means "step" and not
  "nest". An earlier draft made this a bespoke three-operand production; it does not need
  to be one.
- **`.step(n)` is the method the operator desugars to**, and stands on its own for the same
  reason `op add` does: `(0..10).step(2)` and `0..10..2` are one implementation with two
  surfaces, exactly as `a + b` and `op add` are. It is also where `.rev()` and anything
  after it will live, none of which want a new operator.
- **Stepping twice is refused.** `0..10..2..3` is a resolution error rather than last-wins,
  because a range has one step and a program that says otherwise means something it cannot
  have.
- **A stepped range with no end is parenthesised** — `(1..)..2`, and `x[(1..)..2]` for the
  stepped-slice case Python spells `x[1::2]`. §7.1.2 is why the parentheses are required
  rather than merely tidy.
- **`container[r]`** where `r` is a `range` reaches `op get`. A class declaring `op get`
  therefore now receives either an index or a `range`, and may overload on the two by
  v0.8 §3.5. This is a real change to `op get`'s contract, and the reason v0.7 §3.7 points
  here.
- **An unbounded range is iterable**, and terminates only on `break`. This falls out of
  §7.2's laziness rather than being designed in, and it is not an error: `for i in 0..` is
  the loop `while true` spells today, with a counter.

#### 7.1.1 `x[a:b]` is removed

Two slicing syntaxes is one too many, and this milestone ends with one — `..`, not `:`.

The `Slice` node goes away, `x[a:b]` becomes `x[a..b]`, and `x[:n]`, `x[n:]`, `x[-2:]`,
`x[:]` become `x[..n]`, `x[n..]`, `x[-2..]`, `x[..]`. Bounds stay clamped, negative indices
keep their meaning, and an inverted range stays empty. This breaks every existing slice in
the corpus, which is a mechanical change to about a dozen lines.

**Why `:` loses**, given it is the incumbent and Python's answer: it cannot survive a
first-class range value. The moment `0..10` is a value that works in expression position, a
set literal and a dict literal collide —

```quince
{1: 10}     # a dict mapping 1 to 10, or a set holding the range 1 to 10?
```

— and there is no local rule that resolves it. Python has no such problem because its slices
are bracket-only syntax rather than values; `range(0, 10)` is a function call precisely
because `:` could not be lifted out of the brackets. Quince also spends `:` twice already,
on dict pairs and on every v0.7 annotation, and a third meaning for the most overloaded
token in the language is what §11's rule about one spelling per idea exists to prevent.

What is lost is real: `x[:n]` reads better than `x[..n]`, and the `..` forms take a moment to
learn. That is the whole cost, and it is paid once.

#### 7.1.2 Why `(1..)..2` needs its parentheses

Because without them it parses, and parses wrong.

`1.. ..2` is four tokens — `1`, `..`, `..`, `2` — and an ordinary precedence parser resolves
them as `1 .. (..2)`: it takes the first `..` as the binary operator, then goes looking for a
right operand, and `..2` is a perfectly good prefix range to find. The result is a range from
`1` to *a range*, which is not what anyone writing it meant. It is not a syntax error either,
which is what makes it worth a section — it would reach `op range` with a `range` argument
and fail somewhere further away than it should.

The postfix form is only recognized where the following token cannot begin an expression, and
`..` can. So the parentheses are not tidiness: `(1..)` is how the postfix form is reached at
all in that position, and `(1..)..2` is the spelling.

This also disposes of `1....2`, which an earlier draft weighed making work by counting the
dots. It cannot mean anything the parser would agree with even if it lexed, since the tokens
it would produce are the ones above. v0.9 spends `...` on variadic packs, so a run of dots is
resolved by maximal munch like every other token — `....` is `...` followed by a stray `.`,
and the error names `(1..)..2` as the fix.

### 7.2 A lazy iteration protocol

`op iter` returns a **list**, eagerly, by a decision DESIGN.md records and defends. `range`
breaks it: `for i in 0..1000000` would materialise a million-element list to walk it once,
and that is not a price a language can charge on its most ordinary loop.

So the protocol becomes lazy:

- **`op iter` returns an iterator** — any object declaring `op next`.
- **`op next` answers the next element, or `None` to stop.** Under §3 that is `Option[T]`,
  which is also what keeps iteration over a sequence containing `nil` possible — the eager
  protocol never had to answer that question.
- **`op iter`'s list contract is kept as a fallback.** A class returning a `list` still
  works, because a `list` is iterable. Every class in the corpus declaring `op iter` today
  keeps working untouched, and that is what makes the reversal affordable.

`op next` joins `OPS` with a return contract of `Option[T]`, and belongs in v0.7 §3.7's table
once it lands. The reversal is recorded as such in §11.

### 7.3 `array[T, N]` — fixed-size contiguous storage

```quince
let vec3: array[float, 3] = [1.0, 2.5, 0.0]
let buffer: array[int, 1024] = array(1024)

print(len(vec3))      # 3
print(vec3[0])        # 1.0
```

- **Signature:** `builtin class array[T: any?, const N: int]`, on v0.9 §3.3's const
  generic parameters. The bound is `any?`, not `any`, so that `array[int?, 4]` is a type.
- **Reified metadata:** the header stores `T` and `N`, so `arr is array[float, 3]` is `O(1)`,
  as v0.7 §3.9 specifies for every generic.
- **Storage role:** `array` is the contiguous primitive `list[T]` is built on — the other
  half of why it is worth adding.

### 7.4 `bytes` — raw binary

```quince
let header: bytes = b"GET / HTTP/1.1\r\n"
print(len(header))              # 16
let slice: bytes = header[0..4] # b"GET "
print(header[0])                # 71 — an int, not a one-byte bytes
```

`bytes` is a contiguous sequence of `u8`. **Indexing yields an `int`; slicing yields
`bytes`.** That asymmetry is deliberate and is the opposite of `string`, where indexing
yields a one-character `string` — because a byte *is* a number and a character is not, and
the character-versus-byte decision DESIGN.md records is precisely what `bytes` exists to let
a program opt out of.

The `b"…"` literal takes a string literal's escapes and refuses non-ASCII source characters:
`b"café"` is an error naming the character, rather than a silent UTF-8 encoding.

### 7.5 `set[T]`

```quince
let active_users: set[string] = {"alice", "bob"}
active_users.add("charlie")

print("alice" in active_users)  # true

let s1: set[int] = {1, 2, 3}
let s2: set[int] = {3, 4, 5}

let union_set = s1 | s2         # {1, 2, 3, 4, 5}
let inter_set = s1 & s2         # {3}
let diff_set  = s1 - s2         # {1, 2}
```

**`T` must be a type a set can hash** — `dict::Key`'s closed set of `nil`, `bool`, `int`,
`float`, `string` — for exactly the reason v0.7 §4.2 gives about dict keys. A set is a dict
without values; it inherits the constraint, and it will inherit the loosening when v0.7 §8's
`op hash` work happens.

**Literal disambiguation.** `{…}` now spells three things:

1. `{1, 2, 3}` — non-empty, no colons → `set`.
2. `{"a": 1}` — colons → `dict`.
3. `{}` → **empty `dict`**, unchanged. This is the one rule decided by compatibility rather
   than symmetry, and it is worth the asymmetry: `{}` means an empty dict in every program
   written so far.
4. `{}` against an annotation — `let s: set[int] = {}` → an empty `set[int]`. The annotation
   is the only thing that can say so.
5. `set()` or `set[T]()` → an empty set, and the spelling to use where there is no annotation.
6. A mix — `{1, "a": 2}` — is a parse error naming both forms.

**A set literal needs no disambiguation against a block**, and neither did a dict literal.
`{` is reachable only where an operand is expected and is never an infix or postfix
operator, so a `{` following a complete expression can only open a block — which is why
`for k in {"x": 9} { … }` parses correctly today and `for x in {1, 2, 3} { … }` will.
Rust needs a restriction here because it has `Name { … }` struct literals, whose brace
follows a path expression; Quince has no such form, so the two uses never compete. The
parser says as much at the `LBrace` arm of `primary`.

`let s: set[int]` with no initializer auto-initializes to an empty set, extending v0.7
v0.8 §3.4's rule (`list` → `[]`, `dict` → `{}`) with `set` → `set()`.

---

## 8. Runtime representation

### 8.1 Tagged union layout

An enum value is a discriminator tag and a payload:

$$\text{Enum} = \{ \text{u8 tag}, [\text{max\_payload\_bytes}] \}$$

- **The tag is a `u8`**, so an enum may carry at most 256 variants. Nothing needs more, and
  the limit is worth stating rather than discovering.
- **Dispatch:** `match` reads the tag and jumps, rather than testing arms in sequence,
  wherever every arm is a distinct variant of one enum.

This describes the layout under the **bytecode VM** (`BYTECODE_VM_DESIGN.md`), which is where
jump-table dispatch pays. In the tree-walking evaluator the object model is the arena and
handles DESIGN.md describes: an enum value is an ordinary heap object carrying a tag and its
fields, and `match` is a linear scan over arms. Both are correct, only the second exists
today, and this section is what the first is aiming at.

### 8.2 Null pointer optimization

For `Option[T]` where `T` is a reference — a class instance, a container, a string — the
runtime encodes `Option.None` as a null reference (`0x0`) and `Option.Some(ref)` as the
handle itself. `Option[Point]` then costs exactly what a handle costs.

**This is what makes §3's answer free.** `Point?` and `Option[Point]` are not merely
equivalent; they are the same bits. Calling them one type gives up nothing.

Where `T` is a value type — `int`, `float`, `bool` — there is no spare null and the
representation falls back to §8.1's tag plus payload. `Option[int]` costs a word more than an
`int`, which is the ordinary price, and is stated so that nobody plans around an optimization
that is not there.

---

## 9. Work items, in order

**Tranche 1 — `range` and iteration.** `..` in all five forms, `op range` on `int` and on
`range`, the `range` class with `.step()`, `op next`, and
`op iter` returning an iterator with the list fallback. First because it is independent of
enums, because it closes the `Op::Get` hole the codebase has been carrying, and because
§7.1.1's removal of `x[a:b]` wants to land before anything else touches indexing.

**Tranche 2 — `enum` declarations.** Parsing, variants, payload fields, methods and
operators, the reified header, `is`. No matching yet: an enum value can be built, stored,
compared, and printed.

**Tranche 3 — `match` and `if let`.** Patterns, binding, arm type-joining, exhaustiveness,
unreachable-arm refusal. The largest single item here, and what justifies tranche 2.

**Tranche 4 — `Option` and `Result`.** The built-in generic enums, and §3's unification of
`T?` with `Option[T]` — which touches v0.7's `?.`, `??`, and `d[key]`. Deliberately after
`match`, so the sugar is defined over a mechanism that already works.

**Tranche 5 — `?` propagation.** Small once tranche 4 exists, and the ergonomic payoff of the
whole milestone.

**Tranche 6 — `set[T]`.** Literal disambiguation, the hash constraint, the operators.

**Tranche 7 — `array[T, N]` and `bytes`.** Last because nothing else here depends on them.
`list[T]` moving onto `array` as its backing store is an optimization that can follow the
milestone rather than gate it.

---

## 10. Deferred

**Field defaults on variants.** §5.1. It is default arguments, which the language does not
have for functions either, and doing it for one and not the other repeats the mistake v0.7
v0.7 §3.6 names about module visibility.

**`while let`.** §6.5. It belongs with iteration, and is small enough to fold into whichever
milestone next touches loops.

**Or-patterns, guards, and range patterns** — `A | B => …`, `x if x > 0 => …`, `0..9 => …`.
Each is a real convenience and none is needed to make `match` worth having. Guards interact
with exhaustiveness in a way that wants its own thinking: a guarded arm cannot count toward
coverage, and a checker that quietly assumes it can is worse than no checker at all.

**Nested patterns** — `Option.Some(Shape.Circle(r))`. The most likely of these to be missed,
and deferred only because §6's rules are stated for one level; the recursion should be
written down deliberately rather than implied.

**Whether `d[key]` writes its signature as `Option[V]` or `V?`.** §3. They are one type, so
this is a question about what the documentation and the LSP show, not about behaviour — but
it should be answered once rather than per-site.

**`op hash`, and enums or tuples as keys.** Inherited from v0.7 §8, unchanged.

**Sized integer types** (`u8`, `i32`). `bytes` yields `int` on indexing precisely so this
milestone does not need them.

---

## 11. Decisions taken

- **`T?` is `Option[T]`.** §3, and the one to reverse if any. v0.7 §10 has the
  alternatives; §8.2 has the reason this one is free.
- **`Option` and `Result` are PascalCase**, against the lowercase built-in convention.
  A lowercase `result` would forbid `let result = …` outright — the resolver refuses to let
  a binding take a built-in type's name — and the corpus already has one. It also makes
  every enum PascalCase, built-in and user alike. §4.
- **`Result` is not `throw`.** Both stay, for different jobs. §4.2.
- **`?` does not silently discard an error.** Mixing `Result` and `Option` across it is
  refused. §4.3 rule 3.
- **Variant payloads are named fields, always.** No bare-identifier form. §5.1.
- **Unit variants take no parentheses.** §5.1.
- **Positional or named binding, not mixed in one pattern.** §6.2.
- **Exhaustiveness is required, and unbounded domains need `_`.** §6.4.
- **Enums are closed** — no subclassing, no variant-adding `extend`. §5.3.
- **Iteration becomes lazy, with `op iter`'s list contract kept as a fallback.** §7.2. This
  reverses a decision DESIGN.md records and defends; that defence was written about a
  language with no `range` in it.
- **`..` is left-associative and `range` overloads it**, so `a..b..step` is `(a..b)..step`
  and needs no bespoke production. `.step(n)` is the method it desugars to. §7.1.
- **A stepped range with no end is parenthesised**: `(1..)..2`. Not tidiness — `1.. ..2`
  parses as `1..(..2)`, a range to a range, and `1....2` does not lex at all. §7.1.2.
- **`{` terminates a postfix `a..`**, so `for i in 0.. {` reads the brace as the body. The
  general `{` question needed no rule — dict literals never competed with blocks either. §7.1, §7.5.
- **`x[a:b]` is removed, not kept as sugar.** `:` cannot survive a first-class range value:
  `{1: 10}` would be both a dict literal and a set of one range. §7.1.1.
- **`{}` stays an empty dict.** §7.5 rule 3.
- **`bytes` indexes to `int` and slices to `bytes`.** §7.4.
- **The tag is a `u8`; 256 variants is the limit.** §8.1.
- **`tuple` belongs to v0.9.** This milestone only matches on it. §1.

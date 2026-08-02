# Quince v0.7 — gradual type annotations, container generics, and visibility

Design for the milestone after v0.6. Revised against the code as it stands at `v0.6.0`,
which is the reason several things below changed: the first draft was written before the
inference pass, doc comments, and the native tables existed, and three of its claims turned
out not to be true of the language it described.

Revised a second time after the document had grown to twenty-two features and stopped being
one milestone. That pass split it in three: **declaration modifiers and dispatch moved to
v0.8** (`V0_8_DECLARATIONS_AND_DISPATCH_DESIGN.md`), and **the generic system moved to v0.9**
(`V0_9_GENERICS_DESIGN.md`). What is left here is the milestone the title names, and
nothing else.

Reference parameters (`ref`, `final ref`, `const ref`) were in the first draft and are
**deferred** — see §8.

---

## 1. What this milestone adds

1. **Gradual type annotations, non-nullable by default.** `let x: int = 8`, and `int?`
   where `nil` is a value the name may hold. §3.2.
2. **The `any` and `_` top types.** `any` for any non-nil value, `any?` for anything at
   all, `_` as the wildcard spelling of both. §3.2, §4.1.
3. **`const T` value qualifiers.** Deeply frozen parameters and return values, at the
   boundary the `const` *binding* already freezes at. §3.3.
4. **Member and module visibility.** `public`, `private`, `protected` on class fields and
   methods, and on top-level declarations for module export. §3.4, §3.6.
5. **Typed and blank `final` fields.** `public final id: string`, assigned once in `init`. §3.5.
6. **Declared `op` return types**, checked against the contract the language already
   enforces at run time. §3.7.
7. **Typed containers.** `list[T]`, `dict[K, V]`, and `dict[K]`, enforced when a collection
   is built and when it is modified. §3.10.
8. **Nullable dict indexing.** `d[key]` answers `V?` rather than raising. This *replaces*
   today's `KeyError` on a missing key and is the one breaking change in the milestone. §3.10.
9. **Null safety ergonomics.** Optional chaining (`?.`) and null coalescing (`??`). §3.8.
10. **Type guarding & smart casting.** `is` for runtime type checks, with block-scoped
    narrowing. §3.9.
11. **Type aliases.** `alias ScoreTable = dict[string, int]`. §3.11.
12. **Bitwise operator slots.** `op bit_and`, `op bit_or`, `op bit_xor`, `op bit_not`,
    `op bit_shl`, `op bit_shr`. §3.7.
13. **LSP type tooling.** Inlay hints, type completion after `:`, visibility- and
    smart-cast-aware completion, and live diagnostics. §6.

Item 12 is the one piece of unrelated cargo, kept because it is six rows in a table and an
hour of work, and because a class that can answer `+` and not `|` is arbitrary.

---

## 2. What v0.6 left in place

This milestone is not starting from nothing, and the pieces it builds on constrain it.

- **`sema/infer/`** answers what class an expression belongs to, with `Unknown` as a real
  answer. Annotations are the mechanism by which a program turns an `Unknown` into a
  stated fact — the pass is where they will be read, not a thing to be written beside it.
- **`Type` is `Class(String)`** and so cannot express `list[int]`. Giving it parameters is
  the first real work item; see §7.
- **`Native` records `returns` and `params`** — what a builtin hands back, and what its
  parameters are called. It does *not* record their types.
- **`syntax/doc.rs`** parses `##` blocks and checks `@param` names against the declaration.
  Annotations and documentation describe the same parameters and must compose.
- **`Symbol`** is what both editing surfaces render. Annotations reach the editor by
  landing there, not by a second path.
- **`len`, `print`, and `type` are globals, not methods.** It is `len(xs)`, never
  `xs.len()`. A class answers for itself with `op len`, which `len` asks first.
- **`list` has `filter`, `find`, `map`, `push`, `reverse`, `sort`, `sum`, and no `pop`.**
  `dict` has `get`, `keys`, `remove`, `values`. Anything an example needs beyond those it
  has to write.
- **Slicing is `x[a:b]`,** on strings and lists, with clamped bounds and negative indices.
  It is a `Slice` node rather than an operator, because — as `Op::Get`'s own comment says —
  there is no value in the language that means "1 to 3". v0.10 introduces one, and replaces
  this syntax with `x[a..b]` when it does.
- **`const` already exists** as a binding kind that freezes deeply (`BindKind::Const`).
  §3.3 extends it rather than redefining it.

---

## 3. Syntax

### 3.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `:` | exists (dict literals) | introduces a type annotation |
| `?` | **new** | marks a type as nullable (`T?`) |
| `?.` | **new** | optional chaining operator |
| `??` | **new** | null coalescing operator |
| `is` | **new keyword** | type check & smart-casting operator |
| `alias` | **new keyword** | type alias declaration (`alias ID = string`) |
| `any` | **new keyword** | dynamic top type annotation (`let x: any`, `dict[string, any]`) |
| `_` | **new symbol** | wildcard type placeholder (`dict[_, User]`, `list[_]`) |
| `const` | exists (`const x = 5`) | **new use:** the `const T` qualifier on parameters and returns |
| `[` `]` | exists (indexing, list literals) | type arguments, in type position |
| `public` `private` `protected` | **new keywords** | visibility |

`?`, `?.`, `??`, `is`, `alias`, `any`, `public`, `private`, `protected` are reserved. None of
them appears as an identifier in the corpus, so the
`the_editor_grammar_spells_every_keyword` guard and a corpus run are enough to land them.

**It is `alias`, not `type`, and that is not a preference.** `type` is one of the language's
three globals — `type(x)` is called 36 times in the corpus — so reserving it would break
every one of those calls. It could be made a *contextual* keyword, recognized only at
statement start before an identifier and `=`, which parses fine. What that costs is the
highlighter: `the_editor_grammar_spells_every_keyword` exists precisely so the editor and
the language agree on the keyword list, and a word that is a keyword on one line and a
function on the next is the one case that test cannot express. A second word is cheaper
than a contextual one. §9.

### 3.2 Annotations on bindings

```quince
let x: int = 8
let opt: int? = 10
let name: string? = nil
final PI: float = 3.14159

let numbers: list[int] = [1, 2, 3]
let names: list[string?] = ["alice", nil, "bob"]
let scores: dict[string, int] = {"alice": 95, "bob": 88}
```

An unannotated binding is unchanged and stays dynamically typed. Declaring `let x` (or a
class field `private let data`) without an annotation or initializer creates a dynamic
binding (`Unknown`) initialized to `nil`.

**`Unknown` is not `any`.** The first draft argued there should be no `any` keyword, on the
grounds that the absence of an annotation already says "anything". That was right about
bindings and wrong about type *arguments*: `dict[string, ???]` has no absence to leave, and
`dict[string]` alone cannot say which of the two parameters was elided. So `any` is a
keyword after all, and the three spellings mean three different things:

| Written | Means | Holds `nil` |
| :--- | :--- | :--- |
| nothing (`let x = …`) | `Unknown` — the pass has not been told, and infers what it can | yes |
| `any` (or `_`) | stated: any value, and the program means to exclude `nil` | no |
| `any?` (or `_?`) | stated: the universal top type | yes |

`Unknown` and `any?` accept the same values. They are still distinct, because `Unknown` is
what the pass *concluded* and `any?` is what the program *said*, and only one of them is
worth an inlay hint (§6).

**Uninitialized annotated bindings.** `let x: int` with no initializer is refused at
resolution — there is no `int` to synthesize and no honest default. The rule that lets
`let items: list[int]` mean `[]` is a *constructor* rule and belongs with the rest of them
in v0.8; until then, an annotated binding is initialized or it is an error.

### 3.3 Annotations on functions

```quince
## The distance from the origin.
##
## @param x how far across
## @param y how far up
## @return the distance, never negative
fn magnitude(x: float, y: float): float {
    return math.sqrt(x * x + y * y)
}

# Returning a deeply frozen (read-only) collection
fn get_config(): const dict[string, string] {
    return {"host": "localhost", "port": "8080"}
}

# Accepting a const parameter guarantees the function will not mutate caller data
fn process(data: const list[int]) {
    # data is deeply frozen; mutation methods (like data.push) are refused
}
```

The annotation and the `@param` describe the same parameter and are not redundant: one
says what may be passed, the other says what it means. `syntax/doc.rs` already refuses a `@param`
naming something the declaration does not take, and hover renders both.

**`const T` value qualifiers.** `const` already exists and already means exactly this: a
`const` binding freezes its value deeply, and through every other name that reaches it
(`BindKind::Const`). What this milestone adds is the same freeze at a boundary a binding
cannot reach — a parameter and a return.

- **`const T` return types:** the function hands back a deeply frozen value. This is what
  lets a class expose an internal collection or a cached table without the caller being
  able to mutate it through the reference it was given.
- **`const T` parameters:** the argument is frozen on entry, so the callee cannot mutate
  caller data through it.
- **`final` is the other axis and is unchanged.** `final x = 5`, `public final id: string`
  — the *name* is bound once; the object it names is untouched, so a `final` list still
  grows. `const` freezes the object.

v0.8 gives `const` one more job (`const fn`) and v0.9 a fourth (`const N: int`), and they
are the same idea read at different altitudes: a `const` binding freezes a value, a
`const fn` freezes the world for the duration of a call, and a `const` generic parameter is
a value frozen so early the resolver can see it. A reader who learns "`const` means this
cannot change, and the position says what *this* is" has learned all four.

### 3.4 Class fields and visibility

```quince
class BankAccount {
    private let balance: int = 0
    public final account_id: string
    protected let owner: string?

    op init(id: string, initial_deposit: int) {
        self.account_id = id
        self.balance = initial_deposit
    }

    public fn deposit(amount: int) {
        if amount > 0 {
            self.balance = self.balance + amount
        }
    }

    private fn audit_log() {
        print("Auditing account:", self.account_id)
    }

    op string(): string {
        return "Account(" + self.account_id + "): " + string(self.balance)
    }
}

let acc = BankAccount("ACC123", 100)
acc.deposit(50)         # fine
print(acc.balance)      # VisibilityError: `balance` is private to `BankAccount`
```

`op init`, not `fn init`. The first draft wrote `fn init` and the example did not run —
the language refuses it with `` `BankAccount` has a method `init`, but only `op init` runs
when a class is constructed``. Being a constructor is stated rather than inferred from the
name; that is the whole reason `op` exists.

Rules:

- **`private`** — reachable only inside methods of the declaring class.
- **`protected`** — that, and methods of its subclasses.
- **`public`** — anywhere. The default when no word is written.
- **An `op` may not be private or protected.** The language calls these on the program's
  behalf, from outside; a private `op string` would be a method `print` is entitled to
  call and forbidden from calling. Refused at declaration.

### 3.5 Blank `final` fields

`public final account_id: string` declares a field with no value, assigned once in `init`.
This is a new binding form and it needs its own rules, which the first draft did not give
it:

- Assignable **only** inside the declaring class's `op init`.
- A second assignment is refused, as reassigning any `final` is.
- An `init` that leaves one unassigned leaves it `nil` — and so a blank `final` whose type
  is not nullable is refused at declaration unless every `op init` assigns it.

The last rule is the one worth arguing about. The alternative — allow it and let the field
be `nil` against its own annotation — makes the annotation a suggestion, which is the
thing this milestone exists to stop.

### 3.6 Module export

```quince
# math_utils.qn
public final PI: float = 3.14159

public fn calculate_area(radius: float): float {
    return PI * radius * radius
}

fn internal_helper(x: float): float {
    return x * 2.0
}
```

```quince
import math_utils
print(math_utils.calculate_area(5.0))   # fine
print(math_utils.internal_helper(5.0))  # VisibilityError
```

**This is a run-time error, not a compile-time one**, and the first draft was wrong to
promise otherwise. A file module's contents are not known until the interpreter loads and
runs it — which is why importing a name a module does not declare is an `AttributeError`
today and not a declaration error. Making it compile-time would mean loading modules
during resolution, which is a larger change than this milestone should carry.

Stdlib modules are the exception: their contents are static tables, so `from math import
nosuch` *could* be refused at compile time. Doing it for one and not the other would make
the same mistake report differently depending on which kind of module it was, so both stay
at run time until both can move.

### 3.7 Declared `op` return types

```quince
op string(): int    # refused: `op string` answers with a string
```

Each `op` has a return contract, and **the language already enforces it at run time** —
`op_returned` fires at nine sites, so an `op cmp` handing back a string is caught today.
What this adds is refusing a *declaration* that disagrees, before the op is ever called.
That is a check on the annotation, not new enforcement, and it should read off the same
table the runtime check uses rather than a second copy of it.

| `op` | answers with | status |
| :--- | :--- | :--- |
| `string` | `string` | enforced today |
| `bool` | `bool` | enforced today |
| `int` | `int` | enforced today |
| `float` | `float` | enforced today |
| `list` | `list` | enforced today |
| `dict` | `dict` | enforced today |
| `len` | `int` | enforced today |
| `cmp` | `int` | enforced today |
| `eq`, `lt`, `gt` | `bool` | enforced today |
| `iter` | `list` | enforced today |
| `contains` | `bool` | enforced today |
| `init`, `add`, `sub`, `mul`, `div`, `floordiv`, `rem`, `neg`, `get`, `set` | anything | exists, no contract |
| `bit_and`, `bit_or`, `bit_xor`, `bit_not`, `bit_shl`, `bit_shr` | anything | **new in v0.7** |

The bottom two rows are not an omission. An arithmetic or bitwise op may answer with
whatever its class means by that operation — a set returning a set from `|`, a matrix
returning a matrix from `*` — which is exactly why the inference pass calls `m + m` on a class
`Unknown` rather than assuming a fixed type. An annotation on one of those is checked
against what the body returns and against nothing else.

The op names in this table are the names in `OPS`, and they are the names to write. In
particular it is `get`, `set`, and `contains` — not `index_get`, `index_set`, or `in`. The
operator a user *writes* is `x[i]`, `x[i] = v`, and `needle in x`; the slot those reach has
one name each, for the reason §9 gives about `Op::from_name` being a closed set.

Two ops arrive later and are noted here so this table is the only place to look: `op range`
(for `..`) and `op next` (for lazy iteration), both in v0.10. Both are also the first two
ops whose *usefulness* depends on being overloadable — `op range` answers a `range` when
asked of an `int` and a stepped `range` when asked of a `range`, which is what lets
`0..10..2` be ordinary left associativity rather than its own grammar. That is v0.8 §3.5's
mechanism, and it is the clearest argument for why overloading is worth having.

v0.10 also changes what `op get` may be *passed*, since a `range` is the first value that
can be handed to it as a slice.

### 3.8 Null safety ergonomics (`?.` and `??`)

With nullable types (`T?`), safe navigation and default fallback eliminate verbose null
checks:

```quince
# Optional chaining: evaluates to nil if the receiver is nil, without an AttributeError
let city: string? = user?.address?.city

# Null coalescing: evaluates the right side if the left is nil
let name: string = user?.name ?? "Anonymous"
```

These are here rather than deferred with the other ergonomics because §3.10 makes `d[key]`
answer `V?`. A milestone that puts a nullable type in the return of the most ordinary
expression in the language, and no way out of it, would be a hostile release.

### 3.9 Type guarding & smart casting (`is`)

The `is` operator checks a value's type at run time. Because generic containers carry
**reified type tags** on their heap allocations, `is` tests both base types and type
arguments in O(1):

```quince
let l1: list[int] = [1, 2, 3]
let l2: list[string] = ["hello", "world", "!"]

l1 is list          # true (base type match)
l2 is list          # true
l1 is list[int]     # true (reified argument match)
l2 is list[string]  # true
l1 is list[string]  # false
l2 is list[int]     # false

let val: string? = fetch_user_name()
if val is string {
    # `val` is smart-cast from `string?` to `string` for this block
    print(len(val))
}
```

**Reified generics and performance:**
- **Every allocation with type arguments carries them.** One mechanism, one header field.
  v0.9's user generics and v0.10's `set`, `array`, `Option`, and `Result` inherit it by
  being built the same way rather than by each adding their own.
- Evaluating `l is list[string]` compares the header's descriptor in O(1). It does not
  perform an O(N) element scan, which is what avoids both Java-style erasure and the cost
  of checking.
- **`is` is exact, not variant.** `l1 is list[any?]` is `false` for a `list[int]`, matching
  §4.1's invariance. A test meaning "some list" is spelled `l1 is list`.

### 3.10 Built-in container types (`list[T]`, `dict[K, V]`)

```quince
# Homogeneous dynamic list
let numbers: list[int] = [10, 20, 30]
numbers.push(40)
print(numbers[0])               # 10

# Key-value hash map
let user_scores: dict[string, int] = {"alice": 95, "bob": 88}
user_scores["charlie"] = 92
let alice_score: int? = user_scores["alice"]
print(alice_score ?? 0)         # 95
```

Rules:

- **Dynamic top types (`any` and `_`):** `any` (keyword) and `_` (wildcard symbol) are two
  spellings of one type, and both take `?`. `_` is the spelling to prefer in a
  type-argument position, where the wildcard reads as "this parameter, unconstrained";
  `any` is the spelling to prefer as a whole annotation:
  ```quince
  # Heterogeneous keys, User values:
  let cache1: dict[_, User] = {}
  let cache2: dict[any, User] = {}

  # String keys, any value at all — what `dict[string]` is shorthand for:
  let config: dict[string, _?] = {}
  ```
- **Single-argument dict shorthand (`dict[K]`):** supplying only a key type parameter types
  keys as `K` and leaves values entirely unconstrained — `_?`, not `_`. A shorthand for "I
  only care about the keys" that then refused a `nil` value would be a trap, so the elided
  parameter is the *top* type and not the non-nil one:
  ```quince
  let config: dict[string] = {"host": "localhost", "port": 8080, "debug": true}
  config["timeout"] = 5.5               # accepted: values are unconstrained
  config[100] = "invalid_key"           # TypeError: expected string for dict key
  ```
- **Key constraint:** `K` must be a type a dict can hash — `nil`, `bool`, `int`, `float`,
  `string`, and nothing else. §4.2 is the full statement of the rule and of why a class is
  not on the list.
- **Safe nullable indexing (`d[key] -> V?`):** reading `d[key]` returns `V?` rather than
  raising on a missing key. `d[key] ?? default` and `if val is V` extract safely.

  **This replaces today's behaviour**, where a missing key raises `KeyError`, and it is the
  one thing in this milestone that breaks a running program. The trade is that `??` and the
  nullable types this milestone is *about* make the total form ergonomic for the first
  time, and a language with `T?` in it that still raises on a lookup is asking the reader
  to hold two absence stories at once. The cost is that `d[key]` alone can no longer tell
  "missing" from "present, holding `nil`" in a `dict[string, int?]` — but the distinction
  is not lost, only spelled in two expressions instead of one: `key in d` tests the key
  set directly, whatever the value stored under it. `d.remove(key)` and `key in d` are
  unaffected.
- **Reified header check (O(1)):** both containers carry element, key, and value
  descriptors, so `d is dict[string, int]` is O(1). §3.9.
- **Invariance:** `list[int]` does not hold as `list[any?]`. §4.1.

`tuple[…]` is not here. It is a variadic-arity type whose checking is the same machinery as
a generic parameter pack, so it lands with the generics in v0.9.

### 3.11 Type aliases (`alias`)

```quince
alias UserID = string
alias ScoreTable = dict[UserID, int]

let scores: ScoreTable = {"USR1": 100, "USR2": 85}
```

An alias is a resolution-time substitution and introduces no new type: `ScoreTable` and
`dict[string, int]` are the same type, `is` cannot tell them apart, and an error message
prints whichever the program wrote. A cycle (`alias A = B`, `alias B = A`) is refused.

**Aliases take no parameters in this milestone.** `alias Pair[T] = tuple[T, T]` is a generic
declaration and waits for v0.9, which is where there is something for it to abbreviate.

---

## 4. The type system

### 4.1 Matching

| Annotation | Holds | Refuses |
| :--- | :--- | :--- |
| `int` | integers | floats, strings, bools, `nil` |
| `int?` | integers, `nil` | floats, strings, bools |
| `float` | floats, **and integers, widened** | strings, bools, `nil` |
| `string` | strings | everything else |
| `string?` | strings, `nil` | everything else |
| `bool` | `true`, `false` | everything else |
| `list[T]` | lists whose every item holds as `T` | non-lists, lists with an item that does not |
| `dict[K, V]` | dicts whose keys hold as `K` and values as `V` | anything else |
| `dict[K]` | that, with values unconstrained | non-dicts, a key that does not hold as `K` |
| `UserClass` | instances of it or a subclass | unrelated classes, `nil` |
| `UserClass?` | that, and `nil` | unrelated classes |
| `any` (or `_`) | any non-nil value | `nil` |
| `any?` (or `_?`) | any value, including `nil` | nothing (universal top type) |
| `const T` | whatever `T` holds, frozen on crossing the boundary | what `T` refuses |

**`float` accepts an int and widens it.** The first draft refused it, and that would have
made the annotation stricter than the language's own arithmetic: `1 + 2.0` is a float
everywhere in the evaluator, so `let x: float = 0` being an error would be a rule that
contradicts the expression on the next line. The value stored is the float, so `type(x)`
agrees with what the annotation says.

Narrowing is not symmetric: `let n: int = 3.7` stays an error, because it would have to
choose a rounding and there is `int(x)` for saying which.

**Containers match invariantly.** `list[int]` does not hold as `list[any?]`. Variance is a
real question and this milestone does not answer it — mutability makes covariance unsound
for `list`, and the machinery that would make it sound (declaration-site variance, or a
read-only view type) is larger than anything here. `list[any?]` remains the way to say "a
list of whatever", and it must be built as one rather than converted from one. §8.

### 4.2 What can be a `dict` key

`K` is constrained by what a dict can hash at all — `nil`, `bool`, `int`, `float`,
`string`, and nothing else. A class instance is not a key today and a class that declares
`op eq` gives up being one. So `dict[Point, int]` is refused at declaration, with the
reason named, rather than accepted and failing on first insertion.

This is `dict::Key`'s closed set, written down. An earlier draft promised a `Hashable`
bound admitting "a custom class implementing `op hash(): int`", and that is a different and
much larger feature: a new op in `OPS`, a `Key` variant that can call back into the
interpreter, and the reversal of a decision `Op::Eq`'s own documentation records — that
defining equality *costs* a class its use as a key, because equal keys that hash apart put
two of the same key in one dict. It is a reasonable thing to want and it is not in this
milestone. §8.

---

## 5. Enforcement

**At resolution** — everything decidable from the source alone:

- A literal that cannot hold — `let x: int = "foo"`. §4.1.
- `dict[K, V]` with a key type that cannot be a key. §4.2.
- An unknown type name, or a type alias that cycles. §3.11.
- A `?` on a type that is already nullable (`int??`).
- An annotated binding with no initializer — `let x: int`. §3.2.
- ~~Visibility violations reachable statically — `acc.balance` outside the class.~~
  **Not done, and deliberately.** This section's own closing paragraph settles it: where
  both could work it goes at run time, because the unannotated case has to be caught there
  anyway and one mistake should not report from two places. A receiver's class is only
  sometimes decidable, so a static check would refuse `acc.balance` in the programs the
  pass happened to understand and stay silent in the rest — the same mistake reporting from
  two places, or from neither, depending on how much inference succeeded. The editor gets
  the static half instead, as completion filtering (§6), where being approximate is free.
- An `op` declared private or protected, or with a conflicting return type. §3.4, §3.7.
- A non-nullable blank `final` that some `op init` leaves unassigned. §3.5.

**At run time** — everything that depends on a value:

- Assignment to an annotated binding, including through a container.
- Building a container against its element bounds.
- Argument passing and `return`, against parameter and return annotations.
- `push`, index-set, and dict-set against element, key, and value bounds.
- `const T` freezing an argument on entry and a return value on the way out. §3.3.
- Visibility on a member reached dynamically, and on a module's exports. §3.6.

The split is the one the language already draws: a mistake that is visible in the source
is refused before the program runs, and one that depends on a value is refused when the
value arrives. Where both could work, it goes at run time, because the unannotated case has
to be caught there anyway and one mistake should not report from two places.

---

## 6. LSP and REPL

Both surfaces read `Symbol`, so annotations reach them by landing there.

- **Inlay hints** (`textDocument/inlayHint`) for what the pass inferred and the program did
  not state — `let x` ⟨`: int`⟩ `= 8`. Hints are for the *unannotated* case; showing one
  where the program already wrote the type is noise.
- **Type completion after `:`** — the builtin types, the classes and aliases in scope, `?`,
  and `[` where the type takes parameters.
- **Live diagnostics** for type and visibility errors, through the existing
  `publish_diagnostics` path.
- **Visibility-aware completion.** A private member is not offered outside its class, and
  a non-exported name is not offered after `from module import`. The editor should not
  suggest what the language will refuse.
- **Smart-cast-aware completion.** Inside `if val is string { … }`, `val` offers `string`'s
  members. The narrowing §3.9 does for the type checker is worth nothing if the editor
  still shows the nullable type's empty member list.
- **The REPL** gets the same, from live values rather than inference — a bound value's
  annotation is known exactly.

---

## 7. Work items, in order

**Tranche 1 — visibility.** ✅ **Landed.** Independent of the type system and immediately
useful. Keywords, a field on declarations, the checks, and completion filtering.

It turned out to be bigger than "small", for a reason worth recording: §3.4 puts `private`
on a class *field*, and before this milestone a class body accepted only `fn` and `op` —
a field existed because an `op init` assigned one, so there was nowhere to write the word.
**Class field declarations therefore landed here**, in their initialized form
(`private let balance = 0`), because visibility with no fields to attach to is half of
§3.4. §3.5's blank `final` still waits for tranche 3, which is what gives it a type to
synthesize a default from.

A declared field is initialized when the instance is built, ancestors first, *before*
`op init` runs — so an `init` assigning the same name overwrites a value already there,
which is what makes `let balance = 0` followed by `self.balance = opening` read the way it
looks.

**Tranche 2 — `Type` gains parameters.** ✅ **Landed, in half.** `Type::Class(String)` is
now a name and a list of arguments, so `list[int]` can be expressed. This is the item the
first draft did not mention and it is the one everything else waits on — here and in both
milestones after.

It rippled through far less than this predicted: nine pattern sites and a handful of
render paths, because `Type::class(name)` kept its signature and `class_name()` kept
answering the bare head. Invariance (§4.1) came free — it is structural equality on the
argument list, not a rule anything implements. Types now have a `Display`, so `list[int]`
renders the same in a signature, a hint, and an error.

**The reified header did not land, and should not have.** Nothing can carry type arguments
yet: there is no syntax to write an annotation until tranche 3 and no typed container until
tranche 4, so every type the pass infers still has an empty argument list. The header
belongs with tranche 4, which is also where its O(1) comparison first has a caller.

**Tranche 3 — annotations and `const T`.** ✅ **Landed, less the natives.** `: T`, `T?`,
`any`, `_`, and `const T` parse on bindings, parameters, returns, and fields; the §4.1
matching table is one function; the run-time checks fire at each of those four boundaries.

Three things worth recording:

- **`float` widening is a conversion, not a test.** §4.1 says the value stored is the
  float, so the check hands a value *back* rather than answering yes or no — otherwise
  `let x: float = 0` binds an int under an annotation reading `float` and `type(x)`
  contradicts the line above it.
- **Annotations reach the inference pass**, which is what §2 meant by them being the
  mechanism that turns an `Unknown` into a stated fact. The corpus cross-check found this
  on its own: it refused `let widened: float = 3` because the pass still said `int`.
  `Type` gained nullability in the same change, for the same reason — `string?` holding
  `nil` was the second thing it caught.
- **An unknown type name is a run-time check**, not the resolution check §5 lists. A class
  is an ordinary binding, so which names are types is not known until they run — the same
  argument §3.6 makes about module exports. It reports as a `NameError` naming the
  annotation rather than as a `TypeError` blaming the value.

**Natives still take no parameter types.** The 52-declaration pass is not done, so a call
into the library is unchecked. That is a smaller scope of the same mechanism rather than a
half-built one: `print(x)` is exactly as checked as it was in v0.6.

**Tranche 4 — containers.** ✅ **Landed.** `list[T]`, `dict[K, V]`, `dict[K]`, the
modification checks, and `d[key]` answering `V?`.

The reified header arrived here, as tranche 2 said it would: a descriptor per allocation,
parallel to `frozen` for the same reasons that is. It is what makes a later `push` or
index-set a lookup rather than a rewalk — the contents are checked once, when the value
crosses an annotated boundary, and the stamp keeps them true afterwards. The first
descriptor wins: a list crossing two annotated boundaries is one list, and re-stamping
would leave which annotation governs depending on assignment order.

**Elements widen, and that is not optional.** `let xs: list[float] = [1, 2]` rewrites the
list in place, because holding ints under an annotation reading `float` is the same
contradiction §4.1 rules out for a plain binding, one level down. It is in place rather
than a copy because the value the program holds is the one that has to change.

The breaking change cost exactly one corpus case, as expected. `err_dict_key` is now
`dict_missing_key`, and covers the whole story rather than the old refusal: `nil` for a
missing key, `in` still distinguishing missing from present-holding-`nil`, and `remove`
still raising — which is what keeps `KeyError` a live kind rather than a dead one.

**Tranche 5 — null safety and `is`.** ✅ **Landed.** `?.`, `??`, the `is` operator, and
block-scoped smart casts — made unavoidable by tranche 4, which shipped `d[key] -> V?` with
no way out of it.

- **`??` binds at 8**: tighter than a comparison, looser than arithmetic. That pair makes
  both ordinary readings come out right — `d[k] ?? 0 == 5` is `(d[k] ?? 0) == 5` because
  the coalesce produces the value being compared, and `d[k] ?? 0 + 1` is `d[k] ?? (0 + 1)`
  because the right side is a default *value*. Right-associative, so a chain of fallbacks
  reads left to right. It is its own node rather than a `BinaryOp` because it
  short-circuits: `d[k] ?? expensive()` must not run `expensive()` when the key was there.
- **`?.` short-circuits the whole chain**, not its own link. `a?.b.c` answers `nil` when
  `a` is `nil` without ever reaching `.c` — the only reading under which it means what it
  looks like. A `Chain` node the parser adds around any postfix chain containing a `?.` is
  what bounds "the rest of the chain"; without it there is no node that knows `a?.b.c` is
  one expression. `?.` still guards only its own receiver, so `u?.addr.city` fails when
  `addr` is `nil`, as it should.
- **`is` is not the annotation check.** It is exact rather than variant (§3.9), reads the
  reified header rather than scanning, and does not widen — `1 is float` is `false`, where
  `let x: float = 1` succeeds, because widening is a conversion a boundary performs and a
  question about a value in hand should answer about the value in hand. An undescribed
  list is not a `list[int]`: nothing ever said it was, and scanning to guess is the O(N)
  the reified header exists to avoid.
- **Smart casting is a re-binding.** `if v is string { … }` binds `v` again, narrowed,
  scoped to the block — the inference pass's lookup already prefers the innermost scope
  covering an offset, so nothing had to be invented to scope it. The left of an `&&`
  narrows too, since `if x is string && len(x) > 0` is the form that makes a guard worth
  writing. Only the `then` branch: the `else` knows the test failed, which narrows to "not
  a string", and that is not a type the language can write down.

One thing this found: [`TypeExpr`] carries a `Span`, so `==` on two identical annotations
written in two places is `false`. `is` compares a descriptor written at the declaration
against a type written elsewhere, so it needs `same_as` — structural, and ignoring `frozen`,
because `const list[int]` and `list[int]` are one type and `const` is about a boundary.

**Tranche 6 — aliases, bitwise slots, and `op` return checking.** The three small ones,
batched because each is a day and none blocks anything.

**Tranche 7 — editor tooling.** Inlay hints, type completion, smart-cast-aware and
visibility-aware completion, once there are types to show.

---

## 8. Deferred

**Reference parameters.** `ref`, `final ref`, `const ref` were in the first draft and are
not in this milestone. They are not a feature; they are a change to the calling
convention. Plain `ref` needs lvalue analysis in the resolver, and the draft's third rule
— a `ref` parameter enforcing the *caller's* annotation — needs a reference to carry that
annotation at run time, which is a new value representation rather than a check. The trade
v0.6 named still holds: cut scope, never leave a half-built mechanism in the language.

**Declaration modifiers and dispatch** — `const fn`, `override`, `final` on members,
`explicit`, implicit constructor coercion, overloading, default constructor
auto-initialization. Moved to **v0.8**, not dropped. They read the parameter types this
milestone adds, but none of them is the type system, and together they are a milestone.

**The generic system** — user generic classes, bounds, const value parameters, variadic
packs, `tuple`, `extend list[T]`, generic aliases. Moved to **v0.9**. It is one mechanism
that has to be finished or not started, and it is half the original document by weight.

**`op hash`, and class instances as dict keys.** §4.2.

**Variance.** Containers match invariantly. §4.1.

**Compile-time module visibility**, until module loading can happen at resolution.

**Cross-file inference**, still after this rather than before it.

---

## 9. Decisions taken in this revision

Recorded because they were open in an earlier draft and someone will want to reverse one.

- **One spelling per modifier.** `public`, `private`, `protected` — not also `pub`,
  `priv`, `prot`. The language spells its keywords out (`final`, `complete`, `sealed`,
  `extends`), and two spellings for one idea means every reader learns both and every file
  picks one by accident. `Op::from_name` and `Tag::from_name` are both closed sets with
  exactly one name each, for the same reason.
- **`float` widens an int**, `int` does not narrow a float. §4.1.
- **`any` is a keyword after all**, and is *not* the same as leaving the annotation off.
  The first draft refused it on the grounds that an unannotated binding already says
  "anything"; that argument does not survive contact with type *arguments*, where there is
  no annotation to omit. §3.2.
- **Op slots keep their own names.** `get`, `set`, `contains` — not `index_get`,
  `index_set`, `in`. §3.7.
- **Containers are invariant.** §4.1.
- **`d[key]` answers `V?` instead of raising.** The one breaking change, taken because a
  language with `T?` in it should have one story about absence and not two. §3.10.
- **Dict keys stay a closed set.** No `op hash` in this milestone. §4.2, §8.
- **`const` gets a new job rather than a new keyword.** §3.3.
- **`alias`, not `type`.** `type` is a global the corpus calls 36 times, so reserving it
  would break every call. A contextual keyword would parse, but not highlight. §3.1.
- **`op init`, not `fn init`.** The first draft's example did not run.
- **Module visibility is a run-time error.** §3.6.
- **This milestone is three milestones.** The document reached twenty-two features and
  stopped being coherent; v0.8 and v0.9 are where the other two thirds went, and §8 says
  which. The test applied was whether a feature is *about* types or merely *reads* them.
- **`ref` deferred.** §8.
- **`T?` is sugar for `Option[T]`.** §10, which used to be this document's one open
  decision and is now the record of how it was settled.
- **Class field declarations landed in tranche 1**, because §3.4 had nowhere to write a
  visibility word without them. §7.
- **An `extend` block is an outsider.** A method it adds reaches what any other outsider
  reaches, and not what the class's own methods do. The alternative would make `extend`
  the way around every `private` in the language, which is not a door worth leaving open.
- **Visibility is lexical, not receiver-based.** A method of `Account` may reach
  `other.balance` on any `Account`, not only on `self` — which is what makes `op eq` and
  a `richer_than` writable at all. What decides is where the code was written.
- **No static `acc.balance` check.** §5.
- **An annotation constrains the name, not the first value bound to it.** The declaration
  stores it on the slot, and every write afterwards is checked by the same function that
  checked the declaration — so `let x: int = 0` followed by `x = "s"` is refused, and
  `let f: float = 0` followed by `f = 5` stores `5.0`. Without this an annotation would be
  a claim about one statement rather than about a name, which is not what anybody writing
  one means.
- **A parameter takes the binding words.** `fn f(const xs: list[int])` and
  `fn f(final n: int)` parse, because a parameter *is* a binding that the caller fills in
  and spelling it differently from `const x = 10` was an inconsistency with no argument
  behind it. §3.3's `const T` value qualifier still exists and still means what it meant;
  the two compose, and `final xs: list[int]` makes both claims about different things —
  the name is bound once, and the list must hold ints.
- **`final` is not `frozen`.** The slot records the binding word beside the annotation
  rather than folding one into the other's `frozen` flag: `final` fixes the name and
  `const T` freezes the value, and overloading one field would make `final` freeze a list
  it was never meant to touch.
- **A type's name is shared, not interned.** Interning to a numeric handle was the plan
  going into tranche 2 and is not what landed. The argument for it was §3.9's O(1)
  comparison — but that requirement belongs to the *reified descriptor* on an allocation,
  which `Type` is not: `Type` is the compile-time pass's, where equality is never hot. A
  shared `Arc<str>` gets the cheap clone, which was the real cost, without the global
  state. `Name` is an alias precisely so tranche 4 can decide the descriptor's
  representation on its own merits, and change this one line if the answer is the same.

---

## 10. Settled: `T?` is sugar for `Option[T]`

v0.10 introduces `Option[T]` with `Some`/`None`, and this milestone has `T?` with `nil`.
Two mechanisms for absence in one language is one too many, and this document carried the
question open for a while. It is answered: **`T?` is the surface syntax, `Option[T]` is
the mechanism.** One mechanism, two spellings, `nil` being how `Option.None` prints. `?.`
and `??` become sugar over `match` when v0.10 lands. v0.10's null pointer optimization
already makes the two bit-identical for reference types, so this costs nothing at run time,
and it is the answer v0.10 was already written on — nothing there needs revisiting.

The two rejected answers, recorded because the reasons are the useful part:

- **Keeping them separate** — `T?` for the cheap local case, `Option[T]` for the case that
  travels — requires teaching a distinction the compiler does not enforce. A rule that
  lives only in the reader's head is a rule the language will not keep.
- **`Option[T]` replacing `T?` outright** is the cleanest end state and was the only answer
  that changed *this* milestone. It was refused because it pulls all of nullability into
  v0.10, leaving v0.7 with annotations and no way to spell absence — which reopens §3.5's
  blank finals, §3.8 entirely, §3.10's indexing, and most of §4.1's table. That is not a
  decision inside this milestone; it is a different milestone.

Its one real advantage — `d[key]` answering `Option[V]`, which distinguishes "missing"
from "present, holding `nil`" — turned out to be smaller than it looked. `key in d` tests
the key set directly and is unaffected by §3.10, so the distinction survives as two
expressions rather than one. Giving up a shorthand is a much cheaper trade than giving up
`T?` for a milestone, which is what decided it.

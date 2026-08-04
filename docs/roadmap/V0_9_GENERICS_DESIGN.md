# Quince v0.9 — user-defined generics, tuples, and parameter packs

Design for the milestone after v0.8. v0.7 gave `Type` parameters and built `list[T]` and
`dict[K, V]` on them; this milestone opens that machinery to user code.

It is deliberately one milestone and not four. A generic system that handles `class Stack[T]`
but not bounds, or bounds but not packs, is the half-built mechanism v0.6's trade names —
each missing piece is discovered by a user writing the thing that needs it, and each is a
grammar change rather than a library addition. Either this lands whole or it waits.

---

## 1. What this milestone adds

1. **User-defined generic classes.** `class Stack[T]`, with inference from the annotation,
   explicit instantiation, and unconstrained defaulting. §3.1.
2. **Type parameter bounds.** `class Container[T: Bound]`. §3.2.
3. **Const generic value parameters.** `class Buffer[const N: int]`. §3.3.
4. **Variadic type packs.** `class CustomTuple[Ts...]`. §3.4.
5. **`tuple[T1, …, TN]`**, the built-in arbitrary-arity product type, with destructuring
   and variadic tail unpacking. §3.5.
6. **Container-constrained extensions.** `extend list[int]`. §3.6.
7. **Generic type aliases.** `alias Pair[T] = tuple[T, T]`. §3.7.
8. **LSP support for all of it** — completion inside `[…]`, hover showing bound arguments. §6.

---

## 2. What v0.7 and v0.8 leave in place

- **`Type` carries arguments, and every allocation carries a reified header.** v0.7 tranche 2
  built this for `list[T]` and `dict[K, V]`. This milestone adds no new mechanism for it —
  user generics record their arguments in the same field, which is what makes
  `s is Stack[int]` O(1) without a line of new code.
- **The matching table**, including invariance. `Stack[int]` does not hold as `Stack[any?]`,
  for the same reason `list[int]` does not. v0.7 §4.1.
- **Dict keys are a closed set.** A generic class is not a key, and `tuple` is not either —
  though `tuple` is the case most likely to force the issue, since a tuple of keys is
  obviously a key. v0.7 §8.
- **Constructors, coercion, and overloading** come from v0.8, and apply to generic classes
  unchanged. §3.1's `Stack[int] = [1, 2, 3]` is v0.8's coercion reaching a generic
  constructor, not a new rule.
- **`const` already has three jobs** — a frozen binding, `const T` at a boundary, `const fn`
  on a declaration. §3.3 adds the fourth, and v0.7 §3.3 argues they are one idea.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `...` | **new** | variadic type pack (`Ts...`) and tail unpacking (`let (h, ...t) = …`) |
| `(` `)` | exists (grouping, calls) | tuple literals and tuple types, incl. `(42,)` and `()` |
| `const` | exists | **new use:** `const N: int` generic value parameter |
| `[` `]` | exists (v0.7 type arguments) | now also on class and alias *declarations* |

---

## 3. Syntax

### 3.1 Generic classes (`class Name[T]`)

```quince
class Stack[T] {
    private let items: list[T] = []

    public fn push(item: T) {
        self.items.push(item)
    }

    # `list` has no `pop` — slicing is how a list gives its last element up.
    public fn pop(): T? {
        let n: int = len(self.items)
        if n == 0 {
            return nil
        }
        let last: T = self.items[n - 1]
        self.items = self.items[:n - 1]
        return last
    }

    public const op len(): int {
        return len(self.items)
    }
}

# Inferred from the annotation — no need to repeat [int] on the right
let int_stack: Stack[int] = Stack()
int_stack.push(42)       # accepted
int_stack.push("hello")  # TypeError: expected int, found string

# Explicit instantiation
let also_int = Stack[int]()

# Unannotated: T defaults to dynamic
let dyn_stack = Stack()
dyn_stack.push(42)       # accepted
dyn_stack.push("hello")  # accepted
```

Rules:

- **Inference from the annotation.** `let s: Stack[int] = Stack()` binds `T` to `int` from
  the left-hand side. Writing it twice is noise, and the annotation is the more reliable of
  the two places to put it.
- **Unconstrained defaulting.** `Stack()` with no annotation and no type argument binds `T`
  to `Unknown`, which is gradual typing behaving as it does everywhere else — an
  unannotated thing is dynamic.
- **Binding is reified.** `Stack[int]()` produces an instance whose header records `int`, so
  `s is Stack[int]` is `true`, `s is Stack[string]` is `false`, and both are O(1).
- **`T` in a method signature is checked against the binding.** A `push(item: T)` on a
  `Stack[int]` refuses a string, at run time, as any other annotated parameter would.
- **A type parameter is a type, not a value.** `T()` — constructing from a parameter — is
  refused. Knowing that `T` has a zero-arity constructor requires a bound that can say so,
  and §3.2's bounds cannot yet. §7.

### 3.2 Bounds (`class Container[T: Bound]`)

```quince
class NumberBox[T: float] {
    private let value: T

    public op init(value: T) {
        self.value = value
    }
}

let b1 = NumberBox[float](1.5)    # valid
let b2 = NumberBox[int](10)       # valid — int widens to float, v0.7 §4.1
let bad = NumberBox[string]("a")  # TypeError: `string` does not satisfy bound `float`
```

Rules:

- **A bound is an ordinary type**, and satisfying it is ordinary matching — the same §4.1
  table, including `float` accepting `int`. There is no second subtyping relation.
- **Checked at resolution**, where the type argument is written.
- **A bound of `any?` is the default**, which is what an unbounded `[T]` means.
- **Bounds do not add members.** `T: float` does not let a method call `float`'s methods on a
  `T`, because the bound is a constraint on the *argument*, not an interface. Reaching a
  member through a type parameter needs a structural or nominal interface, which the
  language does not have. §7.

### 3.3 Const generic value parameters (`const N: int`)

A generic parameter can be a **compile-time constant value** rather than a type:

```quince
class FixedBuffer[T, const CAP: int] {
    private let items: list[T]      # auto-initialized to [], v0.8 §3.4

    public const fn capacity(): int {
        return CAP
    }

    public fn push(item: T) {
        if len(self.items) >= CAP {
            throw Error("FixedBuffer is full")
        }
        self.items.push(item)
    }
}

let buf: FixedBuffer[float, 16] = FixedBuffer()
print(buf.capacity())   # 16
```

Rules:

- **Parameter forms.** A parameter list accepts type parameters (`T`), bounded type
  parameters (`T: Bound`), variadic packs (`Ts...`), and const value parameters
  (`const N: PrimitiveType`). A pack, if present, is last.
- **The argument must be a literal or a `const` binding.** A non-constant expression is
  refused at resolution — that is what "const" is doing in the name.
- **`int`, `bool`, and `string` only.** `float` is excluded because two constants that
  compare equal can have different bit patterns, and a type identity that depends on which
  is not one anyone wants to debug.
- **`N` is in scope in the body as a value**, read-only.
- **Reified, like everything else.** `buf is FixedBuffer[float, 16]` is O(1), and
  `FixedBuffer[float, 16]` and `FixedBuffer[float, 32]` are different types.

The feature that makes this pay is `array[T, N]`, in v0.10 — a genuinely fixed-size block
where `N` is a layout fact rather than a bound to check. It is specified here because
`array` needs the parameter form to already exist, and because a const parameter is useful
on its own the moment a class wants a capacity in its type.

### 3.4 Variadic type packs (`Ts...`)

```quince
class CustomTuple[Ts...] {
    private let data: tuple[Ts...]

    public op init(args: Ts...) {
        self.data = args
    }

    public const fn len(): int {
        return len(self.data)
    }

    public const op get(index: int) {
        return self.data[index]
    }

    public const op string(): string {
        return "CustomTuple" + string(self.data)
    }
}

let t1: CustomTuple[int, string, bool] = CustomTuple(101, "Bob", true)

# Coerced from a tuple literal via op init(args: Ts...), v0.8 §3.3
let t2: CustomTuple[int, string, bool] = (101, "Bob", true)

let bad: CustomTuple[int, string, bool] = (101, "Bob", "NOT_BOOL")
# TypeError: expected bool, found string

print(t1 is CustomTuple[int, string, bool])  # true
print(t1 is CustomTuple[int, string])        # false

let id: int = t1[0]       # resolved to int
let name: string = t1[1]  # resolved to string
```

Rules:

- **Pack expansion.** `Ts...` binds to a sequence of types `(T1, …, TN)`. In
  `op init(args: Ts...)` the parameters are positional and each is checked against its `Ti`.
- **Index type resolution.** `tuple[Ts...][i]` resolves to `Ti` where `i` is a literal. Where
  the index is not a literal, the result is the join of the pack's types — the checker
  cannot know which element, so it answers with what they have in common.
- **One pack per parameter list, in last position.** Two packs cannot be told apart when the
  arguments arrive.
- **A pack may be empty.** `CustomTuple[]` is a type, and `CustomTuple()` builds one.

### 3.5 `tuple[T1, …, TN]`

```quince
let point: tuple[float, float, string] = (12.5, 45.0, "GPS_COORD_1")
let record: tuple[int, string, bool, float] = (101, "Alice", true, 3.14)

# Destructuring
let (lat, lon, label) = point
print(label, "at", lat, lon)

# Variadic tail unpacking
let (head, ...tail) = (1, 2, 3, 4, 5)   # head: 1, tail: (2, 3, 4, 5)

# A single-element tuple needs a trailing comma
let single: tuple[int] = (42,)
let empty: tuple[] = ()
```

Rules:

- **Arbitrary arity**, $N \ge 0$. `tuple[]`, `tuple[T1]`, and up.
- **Immutable.** `t[0] = 5` is refused at resolution. This is what makes a tuple a value
  rather than a short list, and it is why a tuple is the obvious future dict key (v0.7 §8).
- **Arity is part of the type.** `tuple[int, int]` and `tuple[int, int, int]` are unrelated,
  and `t is tuple[int, string]` is O(1) against the reified header.
- **Indexing resolves elementwise**, per §3.4.
- **No default initialization.** `let t: tuple[int, string]` is refused: there is no empty
  value of that type to synthesize, unlike `list` → `[]` in v0.8 §3.4.
- **Destructuring is a binding form**, and every name it introduces obeys the usual `let`
  and `final` rules.

`tuple` is here rather than with the other containers in v0.7 because its checking *is* pack
checking — an arity-N product type and an N-element parameter pack are the same problem, and
solving it twice would be the mistake.

**One consequence worth naming:** iterating a dict can now yield pairs. DESIGN.md records
that `for k in d` yields keys "because there is no tuple to yield", and marks it as the
asterisk on v0.3. That asterisk comes off here — but changing it is a behavioural break for
every existing loop, so it is listed in §7 as a decision this milestone enables rather than
one it takes.

### 3.6 Container-constrained extensions (`extend list[T]`)

```quince
extend list[int] {
    fn sum_squares(): int {
        let total = 0
        for x in self {
            total = total + x * x
        }
        return total
    }
}

let numbers: list[int] = [1, 2, 3]
print(numbers.sum_squares())    # 14

let names: list[string] = ["a", "b"]
names.sum_squares()             # TypeError: `sum_squares` is defined only on `list[int]`
```

**When this is caught:** at run time, from the receiver's reified header, not at resolution.
`names` is annotated here and so the resolver *could* catch it, but a receiver reached
through a parameter, a container, or a dynamic binding could not, and one mistake reporting
from two places at two times is worse than reporting late. The resolver still refuses an
`extend list[int]` block whose target is not a real instantiation.

### 3.7 Generic type aliases

v0.7 §3.11 gives aliases without parameters. With generics they take them:

```quince
alias Pair[T] = tuple[T, T]
alias Lookup[V] = dict[string, V]

let coords: Pair[float] = (1.0, 2.0)
let scores: Lookup[int] = {"alice": 95}
```

An alias is still a resolution-time substitution introducing no new type: `Pair[float]` and
`tuple[float, float]` are the same type, and `is` cannot tell them apart. A cyclic alias —
including one that cycles through its own parameter, `alias A[T] = A[T]` — is refused.

---

## 4. Worked examples

Three containers written in Quince, to check that the feature set is enough to build with
rather than only enough to describe.

### 4.1 A generic hash dictionary

```quince
class DictEntry[K, V] {
    public final key: K
    public let value: V

    public op init(key: K, value: V) {
        self.key = key
        self.value = value
    }
}

class CustomDict[K, V] {
    private let buckets: list[list[DictEntry[K, V]]]
    private let capacity: int

    public op init() {
        self.capacity = 16
        self.buckets = []
        let i = 0
        while i < self.capacity {
            self.buckets.push([])
            i = i + 1
        }
    }

    # A deliberately trivial hash: the language has no `op hash` and no `ord`,
    # so there is nothing better to write here yet. See v0.7 §4.2 and §8.
    private const fn bucket_index(key: K): int {
        let h: int = len(string(key))
        let idx = h % self.capacity
        if idx < 0 {
            idx = idx + self.capacity
        }
        return idx
    }

    public op get(key: K): V? {
        let b = self.buckets[self.bucket_index(key)]
        let i = 0
        while i < len(b) {
            if b[i].key == key {
                return b[i].value
            }
            i = i + 1
        }
        return nil
    }

    public op set(key: K, value: V) {
        let b = self.buckets[self.bucket_index(key)]
        let i = 0
        while i < len(b) {
            if b[i].key == key {
                b[i].value = value
                return
            }
            i = i + 1
        }
        b.push(DictEntry(key, value))
    }
}

let map: CustomDict[string, int] = CustomDict()
map["apples"] = 50
print(map["apples"])            # 50
print(map["bananas"])           # nil
```

### 4.2 A set with operators

Deliberately not called `Set`: v0.10 adds a built-in `set[T]`, and an example class
differing from a builtin only by case is a trap for whoever reads it next.

```quince
class UniqueBag[T] {
    private let elements: list[T]   # auto-initialized to []

    public op init() { }

    public op init(elements: list[T]) {
        let i = 0
        while i < len(elements) {
            self.add(elements[i])
            i = i + 1
        }
    }

    public fn add(item: T) {
        if !(item in self.elements) {
            self.elements.push(item)
        }
    }

    public const op contains(item: T): bool {
        return item in self.elements
    }

    # Union: bag1 | bag2
    public const op bit_or(other: UniqueBag[T]): UniqueBag[T] {
        let res: UniqueBag[T] = UniqueBag()
        let i = 0
        while i < len(self.elements) {
            res.add(self.elements[i])
            i = i + 1
        }
        let j = 0
        while j < len(other.elements) {
            res.add(other.elements[j])
            j = j + 1
        }
        return res
    }

    # Intersection: bag1 & bag2
    public const op bit_and(other: UniqueBag[T]): UniqueBag[T] {
        let res: UniqueBag[T] = UniqueBag()
        let i = 0
        while i < len(self.elements) {
            let item = self.elements[i]
            if item in other {
                res.add(item)
            }
            i = i + 1
        }
        return res
    }
}

let s1: UniqueBag[int] = UniqueBag([1, 2, 3])
let s2: UniqueBag[int] = UniqueBag([3, 4, 5])

print(2 in s1)                  # true
let union_set = s1 | s2         # 1, 2, 3, 4, 5
let inter_set = s1 & s2         # 3
```

### 4.3 An immutable pair

```quince
class Pair[T1, T2] {
    public final first: T1
    public final second: T2

    public op init(first: T1, second: T2) {
        self.first = first
        self.second = second
    }

    public const op eq(other: Pair[T1, T2]): bool {
        return self.first == other.first && self.second == other.second
    }

    public const op string(): string {
        return "(" + string(self.first) + ", " + string(self.second) + ")"
    }
}

let p: Pair[int, string] = Pair(101, "Alice")
print(p)                        # (101, Alice)
print(p == Pair(101, "Alice"))  # true
```

Note what `op eq` costs: this `Pair` can no longer be a dict key, by the rule v0.7 §4.2
records. `tuple[int, string]` has the same problem for a different reason. Both want the
same deferred work.

---

## 5. Enforcement

**At resolution:**
- A type argument that violates its parameter's bound. §3.2.
- A non-constant, wrong-typed, or `float` argument to a `const N` parameter. §3.3.
- The wrong number of type arguments, packs aside. §3.1.
- Two packs in one parameter list, or a pack not in last position. §3.4.
- Indexing a `tuple` outside its arity, or assigning to a tuple element. §3.5.
- A cyclic generic alias. §3.7.
- An `extend list[T]` block whose target is not a real instantiation. §3.6.
- Constructing from a bare type parameter — `T()`. §3.1.
- An uninitialized `let t: tuple[…]`. §3.5.

**At run time:**
- Arguments against method parameters mentioning `T`, once `T` is bound. §3.1.
- Building a generic instance against its arguments, and recording them in the header.
- Coercion from a literal into a generic constructor. §3.4, v0.8 §3.3.
- A method from `extend list[int]` invoked on a receiver whose header disagrees. §3.6.
- Destructuring against arity. §3.5.

---

## 6. LSP

- **Completion inside `[…]`** on a class that declares parameters, showing the bound where
  there is one.
- **Hover shows bound arguments**, not the declaration — `Stack[int]`, not `Stack[T]`, for
  a value whose header says so.
- **Inlay hints for inferred type arguments.** `let s = Stack[int]()` needs no hint;
  `let s: Stack[int] = Stack()` ⟨on the right⟩ is exactly the case the hint is for.
- **Diagnostics for bound violations** on the type argument's own span, not the
  declaration's.

---

## 7. Work items, in order

**Tranche 1 — generic class declarations.** Parameter lists on `class`, `T` in scope in the
body, instantiation with explicit arguments, header recording, `is`. The core, and the only
tranche the rest genuinely require.

**Tranche 2 — inference and defaulting.** `let s: Stack[int] = Stack()`, and bare `Stack()`
defaulting to `Unknown`. Separate from tranche 1 because it touches `sema/infer/` rather than
the class machinery.

**Tranche 3 — bounds.** One resolution check, reusing v0.7 §4.1's matching.

**Tranche 4 — `tuple` and packs.** The two together, because they are one problem: arity-N
products and N-element packs check the same way. Literals, destructuring, tail unpacking,
elementwise index resolution.

**Tranche 5 — const generic parameters.** After packs, because the parameter-list grammar
should stop moving before a fourth parameter form joins it.

**Tranche 6 — `extend list[T]` and generic aliases.** The two small ones.

**Tranche 7 — editor tooling.**

There is no useful cut line in this milestone, which is the point the head of this file makes
about it being one mechanism. The nearest thing to one is dropping tranches 5 and 6 — but tranche 5 is
what v0.10's `array[T, N]` needs, so dropping it moves work rather than removing it.

---

## 8. Deferred

**Interfaces, and bounds that add members.** §3.2's bounds constrain which arguments are
accepted; they do not let a method call anything on a `T`. Making them do so means either
structural typing or a nominal interface, and either is a milestone.

**Constructing from a type parameter** — `T()` inside a generic body. It needs a bound that
can promise a zero-arity constructor, so it waits on the same work.

**Variance.** Inherited from v0.7 §8, unchanged: generics match invariantly.

**Generic functions** — `fn first[T](xs: list[T]): T?`. Genuinely useful and genuinely
separable: generic *classes* need the header machinery, generic *functions* need call-site
inference, and they share almost nothing. Doing both here would double the milestone.

**`op hash`, and generics or tuples as dict keys.** Inherited from v0.7 §8, and §4.3 shows
why it will keep coming up.

**Dict iteration yielding pairs.** §3.5 makes it possible; taking it is a behavioural break
for every existing `for k in d`, and it should be decided on its own rather than as a side
effect of tuples landing.

---

## 9. Decisions taken

- **This is one milestone, with no cut line.** §7, and the reason is at the top of the file.
- **Generics are invariant**, following v0.7 §4.1 rather than defining anything new.
- **Bounds are ordinary types, checked by ordinary matching.** No second subtyping
  relation, and no member access through a parameter. §3.2.
- **Const generic parameters are `int`, `bool`, `string` — not `float`.** §3.3.
- **One pack, last.** §3.4.
- **`tuple` ships here, not with v0.7's containers**, because its checking is pack checking.
  §3.5.
- **Tuples are immutable and their arity is part of their type.** §3.5.
- **`extend list[T]` mismatches are caught at run time**, matching where v0.7 puts every
  other receiver-dependent check. §3.6.
- **Aliases stay substitutions**, even with parameters. §3.7.
- **Dict iteration is not changed here**, though this is what unblocks it. §8.

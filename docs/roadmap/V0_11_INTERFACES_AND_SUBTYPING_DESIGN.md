# Quince v0.11 — interfaces, object hashing, and generic functions

Design for the milestone after v0.10. It closes three deferrals that have been accumulating
since v0.7 and that turn out to be one piece of work: a bound that can *promise* something
(v0.9 §8), a key that is not in `dict::Key`'s closed set (v0.7 §8, v0.9 §8, v0.10 §10), and
a generic function (v0.9 §8).

They are one milestone because the first two are the same mechanism seen twice. An interface
is a name for a set of operations a type answers for; `op hash` is the operation the built-in
containers need and cannot ask for today. And a generic function is what a bound is *for* —
`fn largest[T: Comparable](xs: list[T]): T?` cannot be written until a bound can say that `T`
answers `op cmp`, which is precisely what v0.9 §3.2 records itself as unable to express.

---

## 1. What this milestone adds

1. **`interface` declarations**, and `implements` on a class. §3.1.
2. **Multiple interface implementation and interface inheritance.** A class has at most one
   superclass and any number of interfaces. §3.2.
3. **Bounds that add members.** `T: Comparable` lets a body call what the interface
   declares — the deferral v0.9 §8 records. §3.3.
4. **`op hash(): int`**, and the `hash(x)` global that reaches it. §4.
5. **Hashable dict keys and set elements**, which is what `op hash` was wanted for. §4.2.
6. **Generic functions** — `fn map[T, U](…)`, with call-site inference. §5.
7. **Interface dispatch**, and where it lives in a tree-walking evaluator. §6.
8. **The nullability rules for annotations**, written out as one table. §7.

---

## 2. What v0.7 through v0.10 leave in place

- **Matching is invariant**, by v0.7 §4.1 and v0.9 §9. This milestone adds a second way for
  a type to satisfy an annotation — declaring an interface — and does **not** make containers
  covariant. §7 is where an earlier draft of this document got that wrong, and the table
  there is written to be checkable against v0.7 §4.1 line by line.
- **`Class` is one representation**, builtin and user alike, and a slot is a cached field on
  it rather than a lookup (DESIGN.md, *One class representation*). `op hash` is another slot
  and needs no new mechanism.
- **`op` names are a closed set**, validated against `OPS` with a declared return contract
  (v0.7 §3.7). `op hash` joins it, returning `int`.
- **The evaluator is a tree-walker.** Everything below is specified against the object model
  DESIGN.md describes — an arena and handles, with method lookup walking a parent chain.
  `BYTECODE_VM_DESIGN.md` §4.2 describes the same dispatch under a JIT that does not exist
  yet, and §6 below is explicit about which is which.
- **Generic classes, bounds, packs, and `tuple` are v0.9's.** §5 adds parameters to
  *functions* and reuses everything else.
- **Function types are v0.9 §3.8's.** `function(T) -> U` is what makes §5's examples
  writable.
- **`dict::Key` is a closed set** (v0.7 §4.2). §4.2 is what opens it, and it is the only
  thing in the language that opens it.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `interface` | **new keyword** | a named contract of operations |
| `implements` | **new keyword** | a class declaring which contracts it answers |
| `hash` | **new global** | reaches `op hash`, as `len` reaches `op len`. §4.1 |
| `[` `]` | exists (v0.9 declarations) | now also on `fn` declarations. §5 |

`interface` and `implements` are reserved; neither appears as an identifier in the corpus.
`hash` is **not** reserved — it is a global binding like `print`, and stays shadowable for
the reason DESIGN.md gives about `print` and `len`.

---

## 3. Interfaces

### 3.1 Declaration and implementation

```quince
public interface Printable {
    fn summary(): string
}

public interface Hashable {
    op hash(): int
    op eq(other: any): bool
}

public class User extends Entity implements Printable, Hashable {
    public final id: int
    public final name: string

    op init(id: int, name: string) {
        self.id = id
        self.name = name
    }

    public fn summary(): string {
        return "User(id=" + string(self.id) + ", name=" + self.name + ")"
    }

    public op hash(): int {
        return hash(self.id) ^ hash(self.name)
    }

    public op eq(other: any): bool {
        if !(other is User) {
            return false
        }
        let u: User = other
        return self.id == u.id && self.name == u.name
    }
}
```

Rules:

- **An interface declares signatures and holds no state.** No fields, and no bodies. A
  default implementation is a real feature and is deferred in §10 — it is what turns an
  interface into a mixin, and that question deserves its own argument.
- **An interface may declare `fn` and `op` alike.** `Hashable` above is the motivating case:
  the two operations `dict` needs are both `op`s, and an interface that could not name them
  would be useless for exactly the thing this milestone exists to do.
- **`implements` is a list, `extends` is one class.** Single implementation inheritance, any
  number of contracts — the ordinary answer, taken because multiple *state* inheritance is
  the part that is expensive and the part nobody has asked for.
- **A class must answer every signature it claims**, checked at resolution, with the error
  naming the interface and the missing signature. Inherited members count: a method on the
  superclass satisfies the subclass's `implements`.
- **A signature is satisfied by an exact match**, parameters and return type both. Widening
  is not admitted here even though v0.7 §4.1 admits it at a call boundary, because a class
  claiming `fn f(x: float)` where `Printable` demands `fn f(x: int)` would type-check at the
  declaration and fail at every call made through the interface.
- **Interfaces cannot extend classes**, and a class cannot `implements` a class. The two
  keywords stay disjoint so that `extends` never has to be read twice.
- **`x is Printable` works**, and is the point of the feature. It is a set membership test
  against the class's interface list rather than a chain walk, and is `O(1)` for the same
  reason `is Stack[int]` is: the answer is recorded when the class is built.

### 3.2 Interface inheritance

```quince
public interface Serializable extends Printable, Hashable {
    fn serialize(): bytes
}
```

An interface may extend several parents, and a class implementing it must answer for the
whole transitive set. The graph is acyclic by the same argument DESIGN.md makes about class
parents — a parent interface is resolved before the interface naming it is bound, so a cycle
is an undefined name rather than a check to write.

**Two parents declaring the same signature is fine**, and is one requirement rather than a
conflict, because an interface carries no body for them to disagree about. That is the whole
reason the diamond problem does not arrive with this feature, and it is why §10 defers
default bodies rather than folding them in.

### 3.3 Bounds that add members

v0.9 §3.2 records that a bound constrains which arguments are accepted but does not let a
body reach anything through a `T`, and defers the fix to "either structural typing or a
nominal interface, and either is a milestone". This is that milestone, and the answer is
nominal:

```quince
public interface Comparable {
    op cmp(other: any): int
}

class SortedBag[T: Comparable] {
    private let items: list[T]

    public fn add(item: T) {
        let i = 0
        while i < len(self.items) && self.items[i] < item {   # reaches op cmp through the bound
            i = i + 1
        }
        self.items = self.items[..i] + [item] + self.items[i..]
    }
}
```

- **A bound that names an interface admits its members**; a bound that names a class or a
  builtin keeps v0.9 §3.2's behaviour of constraining the argument and nothing more.
- **The reachable set is exactly what the interface declares**, transitively. A `T:
  Comparable` reaches `op cmp` and nothing else, however the argument was instantiated.
- **Checked at resolution**, where the body is written — which is the first static check in
  the language that is about a type parameter rather than about a value.

---

## 4. `op hash` and hashable containers

### 4.1 The slot, and the global that reaches it

`op hash(): int` joins `OPS` with an `int` return contract. It is reached three ways: by a
`dict` filing a key, by a `set` filing an element, and by the **`hash(x)` global**.

The global is a new name in a language that has kept to three (`print`, `len`, `type`), and
it earns the fourth slot for a reason the other operators do not need: a class implementing
`op hash` almost always implements it by combining the hashes of its fields, so the operation
has to be callable from Quince or the slot cannot be filled by any class with more than one
field. `op len` has `len`, `op string` has `string`, `op bool` has `bool` — `op hash` was the
only slot in the language with no way to invoke it, and §3.1's `User` is what makes that
visible.

- **`hash(x)` answers an `int`** for anything hashable, and raises for anything that is not.
- **It stays shadowable**, like `print` and `len`, by DESIGN.md's rule that the type
  vocabulary is reserved and the function vocabulary is not.

### 4.2 What may now be a key

v0.7 §4.2 restricts `dict` keys to `nil`, `bool`, `int`, `float`, and `string`, because
`dict::Key` cannot call back into the interpreter. That restriction lifts here, and the
shape of the lifting is the interesting part:

- **A value is a valid key when its class answers `op hash` and `op eq`** — that is,
  when it satisfies `Hashable`. Declaring `implements Hashable` is how a class says so.
- **`op hash` and `op eq` must agree**, and the language cannot check it. Two values that
  compare equal and hash differently land in different buckets and the dict holds both. This
  is stated rather than enforced, exactly as Python states it, and it is why `Hashable`
  bundles the two operations into one contract instead of leaving `op hash` free-standing:
  a class cannot claim one without the other.
- **A mutable key is still a mistake** and is still not refused. v0.7's closed set made it
  impossible; opening the set makes it possible again, and the honest reading is that this
  milestone trades a guarantee for an expressiveness the language has wanted since v0.7.
- **`float` keys keep every rule they had.** `1` and `1.0` are one key, and `nan` is
  refused. A user `op hash` cannot reopen either, because both are decided before any class
  is consulted.
- **This is what `set[T]` was waiting for.** v0.10 §7.5 ships `set` restricted to the closed
  set and says it inherits the loosening; this is the loosening.

`tuple` and `enum` become keys the same way — both gain a built-in `op hash` derived from
their elements, which is sound because a tuple is immutable and an enum's payload is bound
at construction. Those are the two cases v0.9 §4.3 and v0.10 §5.3 each predicted would keep
coming up.

---

## 5. Generic functions

Deferred from v0.9 §8 as "genuinely separable: generic *classes* need the header machinery,
generic *functions* need call-site inference". They are here rather than later because §3.3
is what makes them worth more than a type-checked identity function.

```quince
public fn map[T, U](items: list[T], transform: function(T) -> U): list[U] {
    let result: list[U] = []
    for item in items {
        result.push(transform(item))
    }
    return result
}

public fn first[T](items: list[T]): T? {
    if len(items) == 0 {
        return nil
    }
    return items[0]
}

public fn largest[T: Comparable](items: list[T]): T? {
    let best: T? = first(items)
    for item in items {
        if best == nil || item > best {
            best = item
        }
    }
    return best
}
```

Call-site inference, with explicit instantiation available:

```quince
let numbers: list[int] = [1, 2, 3]

let doubled = map(numbers, fn(x: int): int { return x * 2 })     # T = int, U = int
let labels = map[int, string](numbers, fn(x: int): string { return string(x) })
```

Rules:

- **Parameters are declared after the name**, `fn name[T, U](…)`, using v0.9's parameter-list
  grammar unchanged — including bounds (`[T: Comparable]`), const value parameters, and one
  trailing pack.
- **Inference is one pass over the arguments, left to right**, matching each argument's type
  against its parameter's and binding any parameter it meets for the first time. A parameter
  bound twice to different types is an error at the call, naming both.
- **A parameter reachable from no argument must be written.** `fn make[T](): T` can only be
  called as `make[int]()`, and calling it bare is refused at the call site rather than
  silently defaulting — which is the one place this differs from v0.9 §3.1's class rule, and
  it differs because a class has an annotation on its left to infer from and a call has
  nothing.
- **A bare `Unknown` argument binds `Unknown`**, and the call is dynamic from there. Gradual
  typing behaving as it does everywhere else.
- **A generic function is not reified.** There is one function, and the arguments are
  checked as they arrive, exactly as a generic *method* on a `Stack[int]` is. Nothing is
  monomorphized, and a `function(T) -> U` value carries no bindings.
- **Methods may be generic too**, by the same rules, with the class's own parameters in
  scope. A method parameter shadowing a class parameter is refused rather than shadowed —
  `class Stack[T] { fn f[T]() }` can only be a mistake.

---

## 6. Dispatch

Interface dispatch is a lookup like any other, and this milestone puts it in the tree-walker
with the object model that exists today:

- **A class records its transitive interface set** when it is built, as a map from interface
  identity to a table of the members that satisfy it. Copy-down at creation, not a chain walk
  at use — the same decision DESIGN.md records for protocol slots, for the same reason.
- **`x is Printable` reads that map.** `O(1)`.
- **Calling through an interface is the ordinary method lookup.** A call site never knows it
  is calling through an interface, because Quince is dynamically typed and a receiver has no
  compile-time type — which is the same reason `extend` could not copy C# 14's static
  resolution. The interface table exists to answer `is` and to check `implements`, not to
  speed up a call the ordinary path already resolves.

**What `BYTECODE_VM_DESIGN.md` §4.2 describes is the same structure under a compiler that
does not exist yet.** `OpCode::InvokeInterface`, `itables`, and inline offset lookup are
Phase 1B and Phase 3 work, sequenced after every milestone in this file. An earlier draft of
this document specified interface dispatch *only* in those terms, which described a v0.11
that could not be built. Both are correct; only the first exists at v0.11.

---

## 7. Nullability and annotations

An earlier draft of this document carried a table asserting that `list[int]` satisfies
`list[any]` and `Stack[int]` satisfies `Stack[any]`. **Both are wrong**, and they contradict
three settled decisions — v0.7 §4.1, v0.7 §9 ("Containers are invariant"), and v0.9 §9
("Generics are invariant"). They are also unsound in the direction that matters: a function
taking a `list[any]` may push a string into it, so admitting a `list[int]` there would let a
`list[int]` come back holding a string. Covariance is safe for a value that cannot be
written through, and every container in Quince can be.

The rules, restated so there is one place to check them:

**A value satisfies an annotation** by v0.7 §4.1's table, with one addition: a class also
satisfies an interface it implements, transitively.

| Value's type | Annotation | Holds? | Why |
| :--- | :--- | :--- | :--- |
| `int` | `any` | yes | non-nil value, non-nil top type |
| `int` | `any?` | yes | `any?` admits everything |
| `nil` | `any` | **no** | `any` is the *non-nullable* top type, v0.7 §3.2 |
| `nil` | `any?` | yes | |
| `int` | `float` | yes | `float` widens an int, v0.7 §9 |
| `float` | `int` | **no** | narrowing is never implicit |
| `User` | `Printable` | yes | **new here** — `User implements Printable` |
| `User` | `Hashable?` | yes | a non-nil value satisfies a nullable annotation |
| `Printable` | `User` | **no** | an interface does not narrow to an implementor |

**A container satisfies a container annotation only when its arguments are identical.**

| Value's type | Annotation | Holds? | Why |
| :--- | :--- | :--- | :--- |
| `list[int]` | `list[int]` | yes | |
| `list[int]` | `list[any]` | **no** | invariant. v0.7 §4.1 |
| `list[int]` | `list[int?]` | **no** | invariant, and `push(nil)` is why |
| `list[int?]` | `list[int]` | **no** | invariant, and the unsound direction besides |
| `dict[string, int]` | `dict[string, any]` | **no** | invariant |
| `tuple[int, string]` | `tuple[any, any]` | **no** | invariant |
| `Stack[int]` | `Stack[any]` | **no** | invariant. v0.9 §2 |
| `list[User]` | `list[Printable]` | **no** | invariance is not weakened by interfaces |

The last row is the one worth stating explicitly, because interfaces are what makes someone
reach for it. `list[User]` not satisfying `list[Printable]` is the cost of invariance, it is
felt exactly here, and the fix is a generic function — `fn show[T: Printable](xs: list[T])`,
which §5 and §3.3 together make writable, and which is sound because `T` is bound to `User`
for the whole call.

Variance stays deferred, as it has since v0.7 §8. What this milestone changes is that
there is now a written-down reason to want it, which is the useful half of a deferral.

---

## 8. Enforcement

**At resolution:**
- A class claiming an interface it does not fully answer. §3.1.
- A signature that differs from the one the interface declares. §3.1.
- An `interface` declaring a field or a method body. §3.1.
- An `implements` naming a class, or an interface `extends`-ing one. §3.1.
- A member reached through a type parameter that the bound does not declare. §3.3.
- A generic function's type parameter bound to two different types by one call. §5.
- A call omitting a type argument that no argument can infer. §5.
- A method type parameter shadowing its class's. §5.
- `op hash` declared with a return type other than `int`. §4.1.

**At run time:**
- `hash(x)` on a value whose class answers no `op hash`. §4.1.
- A dict or set filing a key whose class answers no `op hash`. §4.2.
- Arguments against parameters mentioning a bound `T`, once bound. §5.

---

## 9. Work items, in order

**Tranche 1 — `op hash` and the `hash` global.** No new grammar, one slot, one global. It is
independent of interfaces and it is what `set[T]` shipped without, so it goes first and is
useful the day it lands.

**Tranche 2 — `interface` and `implements`.** Declaration, the conformance check, the
recorded interface set, `is`. No bounds yet.

**Tranche 3 — interface inheritance.** `interface X extends Y, Z`, and transitive
conformance.

**Tranche 4 — hashable containers.** `dict` and `set` reaching `op hash` and `op eq`, and
the derived hashes for `tuple` and `enum`. After tranche 3 because `Hashable` should exist
as an interface before it is the thing a container demands.

**Tranche 5 — generic functions.** Declaration, call-site inference, explicit instantiation.
Independent of tranches 2–4 and dependent on v0.9.

**Tranche 6 — bounds that add members.** Last, because it is the one item that needs both
halves of the milestone: interfaces from tranche 2 and parameters from tranche 5.

**Tranche 7 — editor tooling.** Completion of interface members through a bound, hover
showing what a class implements, and a diagnostic on the `implements` clause rather than on
the class name.

**The cut line is after tranche 4**, which leaves interfaces and hashing whole and moves
generic functions on. It is a real cut line and not a comfortable one: v0.9 §8 has already
moved generic functions once.

---

## 10. Deferred

**Default method bodies on interfaces.** The feature that turns an interface into a mixin,
and the one that brings the diamond problem with it — §3.2's argument that two parents
declaring one signature is not a conflict holds *because* there is no body to choose between.
Adding bodies means writing a resolution order, and that is a milestone's worth of decision.

**Structural conformance.** A class satisfying `Printable` because it happens to have a
`summary`, without saying so. Nominal is the restrictive direction and can be relaxed;
structural cannot be tightened once programs rely on it.

**Generic interfaces** — `interface Container[T]`. They compose with everything here and
need one more pass over the conformance check; separable, and not needed by anything in this
milestone.

**Variance.** §7. Inherited unchanged from v0.7 §8 and v0.9 §8, now with a stated cost.

**A derived `op hash`** — the language writing one for a class that declares `op eq`. It is
the right default and it needs a rule for which fields participate.

**Return-type inference for generic functions.** `fn make[T](): T` inferring `T` from the
annotation on the left, the way v0.9 §3.1 infers a class's arguments. It is bidirectional
inference and it is deferred for the reason v0.8 §6 defers return-type overloading.

---

## 11. Decisions taken

- **Containers stay invariant, and interfaces do not weaken it.** `list[User]` does not
  satisfy `list[Printable]`. §7 — and the earlier draft that said otherwise is recorded there
  rather than deleted, because the mistake is an easy one to make twice.
- **Interfaces are nominal.** §3.1, §10.
- **Interfaces hold no state and no bodies.** §3.1.
- **One superclass, any number of interfaces.** §3.1.
- **An interface may declare `op`s**, which is what makes `Hashable` expressible at all. §3.1.
- **Conformance is by exact signature**, without v0.7 §4.1's widening. §3.1.
- **`hash` becomes the fourth global**, because `op hash` is otherwise the one slot in the
  language a program cannot invoke. §4.1.
- **`Hashable` bundles `op hash` with `op eq`**, so a class cannot claim one without the
  other. §4.2.
- **Opening `dict::Key` gives up a guarantee**, and the document says so rather than
  presenting it as free. §4.2.
- **A bound naming an interface admits its members; any other bound does not.** §3.3.
- **Generic functions are not monomorphized and carry no reified header.** §5.
- **A type parameter no argument can infer must be written at the call.** §5.
- **Interface dispatch is specified for the tree-walker**, with the JIT structure named as a
  later phase rather than as this milestone's design. §6.

# Quince v0.8.1 — Classes, OOP & Dispatch Reference

Quince v0.8.1 provides an object-oriented paradigm featuring class inheritance, openness control, explicit access visibility, extension blocks, and operator overloading via static operator slots.

---

## 1. Class Declarations & Inheritance

### 1.1 Class Syntax & Headers

Classes are declared using the `class` keyword. A class may inherit from a parent class using `extends`:

```quince
class Animal {
    public let name: string

    op init(name: string) {
        self.name = name
    }

    public fn speak(): string {
        return "..."
    }
}

class Dog extends Animal {
    public let breed: string

    op init(name: string, breed: string) {
        super.init(name)
        self.breed = breed
    }

    public fn speak(): string {
        return "Woof! I am " + self.name
    }
}
```

### 1.2 Initialization & Execution Order

When a class instance is constructed:
1. Ancestor field initializers execute in top-down order (base ancestor down to leaf class).
2. The leaf class field initializers execute.
3. The constructor `op init(...)` executes.

### 1.3 `self` and `super`

- **`self`**: Refers to the current receiver instance within a method or operator block.
- **`super`**: Accesses parent class methods or operators bound to the current receiver (e.g. `super.init(...)` or `super.speak()`).

---

## 2. Class Openness Modifiers

Class headers may be prefixed with an openness modifier to control subclassing and method table extension:

| Modifier    | Subclassing Allowed? (`extends`) | Extension Blocks Allowed? (`extend`) | Purpose / Description                                               |
| :---------- | :------------------------------: | :----------------------------------: | :------------------------------------------------------------------ |
| *(Default)* |               Yes                |                 Yes                  | Fully open class                                                    |
| `final`     |              **No**              |                 Yes                  | Prevents other classes from extending this class                    |
| `complete`  |               Yes                |                **No**                | Freezes the method table; no `extend` block may add methods         |
| `sealed`    |              **No**              |                **No**                | Combines `final` and `complete` (neither subclassing nor extension) |

```quince
final class LeafConfig {
    # Cannot be subclassed via `class Child extends LeafConfig`
}

complete class StableAPI {
    # Cannot be modified via `extend StableAPI`
}

sealed class ImmutableCore {
    # Closed to both inheritance and extensions
}
```

---

## 3. Member Visibility

Class fields and methods support explicit access control:

| Modifier             | Access Scope                                                       |
| :------------------- | :----------------------------------------------------------------- |
| `public` *(default)* | Reachable from anywhere inside or outside the class                |
| `private`            | Reachable **only** inside methods of the declaring class           |
| `protected`          | Reachable inside methods of the declaring class and its subclasses |

```quince
class BankAccount {
    private let balance: int = 0
    public final account_id: string
    protected let owner: string

    op init(id: string, owner: string, opening: int) {
        self.account_id = id
        self.owner = owner
        self.balance = opening
    }

    public fn deposit(amount: int) {
        if amount > 0 {
            self.balance = self.balance + amount
            self.log_transaction("deposit", amount)
        }
    }

    private fn log_transaction(kind: string, amount: int) {
        print("LOG [" + self.account_id + "]: " + kind + " " + string(amount))
    }
}

let acc = BankAccount("ACC_101", "Alice", 500)
acc.deposit(100)        # Allowed (public)
print(acc.account_id)   # Allowed (public)
# print(acc.balance)    # VisibilityError: `balance` is private to `BankAccount`
```

---

## 4. Field Initializers & Blank `final` Fields

### 4.1 Initialized Fields

Fields declared with an initial value are evaluated before `op init` runs:

```quince
class Counter {
    public let count: int = 0 # Evaluated before init
}
```

### 4.2 Blank Fields

A field written with an annotation and no initializer — `public final id: string` — is
**refused** unless the annotated type can say what its default is. `string` cannot, so the
field needs a value:

```quince
class Player {
    public final id: string = ""     # a value, assigned again in `op init`
}
```

This entry described a *blank `final` field* through v0.7 — one assigned exactly once inside
`op init`, checked on every constructor path — and the language never had it: v0.7's parser
required the `=`, so the form did not even lex. v0.8 makes it parse and gives it the rule in
§4.3 rather than the one described here, because "assigned exactly once on every path" is a
definite-assignment analysis and this milestone does not do one. Kept as a correction rather
than deleted, since the feature is worth wanting and the entry is where someone will look
for it.

### 4.3 Fields with No Initializer

A field annotated with a type that can answer what its default is takes that default, before
`op init` runs — see [Default construction](type_system.md#9-default-construction). A field
with neither an annotation nor an initializer holds `nil`.

```quince
class Logger {
    private let entries: list[string]   # auto-initialized to []
    private let tag                     # nil

    public fn log(msg: string) {
        self.entries.push(msg)          # already a list before init runs
    }
}
```

---

## 5. Extension Blocks (`extend`)

The `extend` keyword adds methods to existing classes or built-in primitive types without modifying their original declaration:

```quince
# Extend primitive string type
extend string {
    public fn shout(): string {
        return self.upper() + "!"
    }
}

print("hello".shout()) # Outputs: HELLO!

# Extend custom class
extend BankAccount {
    public fn is_overdrawn(): bool {
        return self.balance < 0
    }
}
```

*Rules*:
- `extend` blocks can add standard methods (`fn`) and operator slots (`op`)
- Extensions cannot be applied to classes declared with `complete` or `sealed`.

---

## 6. Operator Overloading (`op`)

Operators in Quince are handled by dedicated static slots declared with `op`. Constructors MUST be declared as `op init`.

### 6.1 Complete List of Operator Slots (30 Slots)

| Slot Name  | Syntax Trigger          | Parameter Arity | Return Contract | Description                   |
| :--------- | :---------------------- | :-------------: | :-------------: | :---------------------------- |
| `init`     | `MyClass(...)`          |    Flexible     |      None       | Constructor initializer       |
| `bool`     | `if x`, `not x`, `bool(x)` |        0        |     `bool`      | Truthiness conversion         |
| `string`   | `print(x)`, `string(x)` |        0        |    `string`     | String rendering              |
| `int`      | `int(x)`                |        0        |      `int`      | Integer conversion            |
| `float`    | `float(x)`              |        0        |     `float`     | Float conversion              |
| `list`     | `list(x)`               |        0        |     `list`      | List conversion               |
| `dict`     | `dict(x)`               |        0        |     `dict`      | Dict conversion               |
| `eq`       | `a == b`, `a != b`      |        1        |     `bool`      | Equality comparison           |
| `cmp`      | `a <=> b`               |        1        |      `int`      | Three-way ordering (-1, 0, 1) |
| `lt`       | `a < b`                 |        1        |     `bool`      | Less than comparison          |
| `gt`       | `a > b`                 |        1        |     `bool`      | Greater than comparison       |
| `add`      | `a + b`                 |        1        |    Flexible     | Addition                      |
| `sub`      | `a - b`                 |        1        |    Flexible     | Subtraction                   |
| `mul`      | `a * b`                 |        1        |    Flexible     | Multiplication                |
| `div`      | `a / b`                 |        1        |    Flexible     | True division                 |
| `floordiv` | `a // b`                |        1        |    Flexible     | Floor division                |
| `rem`      | `a % b`                 |        1        |    Flexible     | Remainder / Modulo            |
| `neg`      | `-x`                    |        0        |    Flexible     | Unary negation                |
| `len`      | `len(x)`                |        0        |      `int`      | Collection length             |
| `get`      | `x[i]`                  |        1        |    Flexible     | Index read                    |
| `set`      | `x[i] = v`              |        2        |    Flexible     | Index write                   |
| `contains` | `needle in x`           |        1        |     `bool`      | Membership check              |
| `iter`     | `for item in x`         |        0        |     `list`      | Iteration target list         |
| `bit_and`  | `a & b`                 |        1        |    Flexible     | Bitwise AND                   |
| `bit_or`   | `a \| b`                |        1        |    Flexible     | Bitwise OR                    |
| `bit_xor`  | `a ^ b`                 |        1        |    Flexible     | Bitwise XOR                   |
| `bit_not`  | `~a`                    |        0        |    Flexible     | Bitwise NOT                   |
| `bit_shl`  | `a << b`                |        1        |    Flexible     | Bitwise Shift Left            |
| `bit_shr`  | `a >> b`                |        1        |    Flexible     | Bitwise Shift Right           |
| `pow`      | `a ** b`                |        1        |    Flexible     | Exponentiation                |

Every arithmetic slot also answers the matching compound assignment: `a += b` reaches
`op add`, `a **= b` reaches `op pow`, and so on. There is no separate in-place slot — a class
wanting in-place mutation writes a method and says so.

Declaring a slot is a class saying what that operator *means* to it, so an operand it does
not take is a refusal rather than a fall-through to what the underlying type would have done:

```quince
extend list {
    public op mul(factor: int): list { … }
}

print([1, 2, 3] * 2)      # [2, 4, 6]
print([1, 2, 3] * 2.4)
# TypeError: cannot multiply list and float
#   help: `list` declares `op mul` for: (int) — convert the operand, or declare one for
#         these types beside the ones that are there
```

The report is the one every binary type error gets — the same sentence, a label on each
operand, and the operator marked in between — so a reader cannot tell from its shape whether
the class declared the slot and refused the operand or never declared it at all. What the
class *does* take is the help line, which is the one thing the ordinary report has nothing to
say about, and it reads the same whether the slot carries one declaration or several.

`op get`, `op set`, and `op contains` keep a report of their own, because `x[i]` and
`needle in x` have no pair of operand spans to label:

```quince
class Grid {
    op get(i: int) { … }
}
print(Grid()["a"])
# TypeError: `op get` on a Grid does not take (string)
#   help: `Grid` declares `op get` for: (int) — …
```

### 6.2 Return Type Contracts

Slots with a fixed return contract (e.g. `op string(): string`, `op bool(): bool`, `op len(): int`) enforce their return type both statically at declaration and dynamically at run time. Declaring `op string(): int` is refused at resolution time.

---

## 6A. Member Modifiers

Four words may precede `fn` or `op` in a class body, in any order. The canonical order is
visibility first — `public const fn` — and every other order is accepted and normalized.
Writing one twice is refused.

### 6A.1 `override` and `final`

A member that replaces one a superclass declared must say `override`, and a member that says
`override` must actually replace one. Both halves are enforced at resolution: a keyword that
could be written where it is not true is documentation nobody can trust, and a misspelled
method name is exactly the mistake the other half catches.

```quince
class Animal {
    fn speak(): string { return "..." }
    final fn kind(): string { return "animal" }
}

class Dog extends Animal {
    override fn speak(): string { return "woof" }

    # fn speak(): string { … }          # DeclarationError: replaces `Animal`'s and does not say so
    # override fn speek(): string { … } # DeclarationError: `fn speek` overrides nothing
    # override fn kind(): string { … }  # DeclarationError: `fn kind` is final in `Animal`
}
```

`op init` is exempt. Every constructor in a hierarchy replaces its parent's — that is what
`super.init` is for — so requiring the word there would mean writing it on every subclass and
saying nothing when it was written.

`final` on a member and `final` on a binding are the same word for the same idea: this name is
bound once and cannot be rebound. On a field it is the value; on a method it is the
implementation.

Neither word may be written on a plain `fn`, nor inside an `extend` block: an extension adds
to a type and never replaces part of it, so nothing it declares can override or be overridden.

### 6A.2 `const fn` and `const op`

`const` before `fn` or `op` marks the body pure and read-only, and the resolver holds the
declaration to it. What is restricted is **state**, not effects — `print` is allowed, and so
are `throw` and an early `return`.

```quince
class Point {
    op init(x: float, y: float) { self.x = x; self.y = y }

    const fn sum(): float { return self.x + self.y }
    const fn twice(): float { return self.sum() * 2.0 }   # a `const` method may call one

    const op string(): string {
        return "(" + string(self.x) + ", " + string(self.y) + ")"
    }
}
```

Four things are refused inside one:

| Refused | Example |
| :--- | :--- |
| Assigning to a field | `self.n = 1` |
| Assigning through an index | `self.items[0] = 1`, and `d["a"] = 1` for a local `d` too |
| Reassigning a name bound outside it | a global, or a local of an enclosing function |
| Calling a method on `self` that is not `const` | `self.reset()` |

A local the call made itself — a `let`, a parameter, a loop variable — is the function's own
and may be reassigned freely. Index assignment is refused even for a container the call
allocated: telling that apart from a caller's container is an escape analysis, and the strict
answer is the one that can be relaxed later.

The rule runs one way. An ordinary `fn` may call a `const` one.

Purity reaches into a `fn` nested inside a `const fn`, because such a function closes over
the receiver and the enclosing locals — letting it mutate them would be the whole promise
escaping through a closure.

### 6A.3 `explicit`

See [Implicit constructor coercion](type_system.md#8-implicit-constructor-coercion).

---

## 6B. Overloading

A class, an `extend` block, or a scope may declare several `fn`s or `op`s under one name, as
long as their parameter type signatures are distinct.

```quince
class Vector {
    op init(x: float, y: float) { self.x = x; self.y = y }

    public op add(other: Vector): Vector {
        return Vector(self.x + other.x, self.y + other.y)
    }
    public op add(scalar: float): Vector {
        return Vector(self.x + scalar, self.y + scalar)
    }
}

let v = Vector(1.0, 2.0)
print(v + Vector(3.0, 4.0))   # reaches `other: Vector`
print(v + 10.0)               # reaches `scalar: float`
```

**Dispatch is on the run-time argument types, exact match before widened.** An `int` argument
prefers an `int` parameter over a `float` one and reaches the `float` overload only when there
is no `int` one — the same widening rule an annotation follows, not a second one.

**An unannotated parameter matches anything and is tried last**, so a name may have at most
one unannotated overload.

**Ambiguity is refused where it is declared, not where it is called.** Two overloads some
argument would reach equally well — `f(x: float)` beside `f(x: int?)`, which an `int` widens
into either way — are a `DeclarationError` at the second declaration. A dispatch failure at
run time means "nothing matched", never "two things did".

**A defaulted parameter contributes one signature per callable arity.** `fn f(a: int, b: int = 0)`
is both `f(int)` and `f(int, int)`, so `fn f(a: int)` declared beside it is a duplicate.
Selection runs *before* defaults are filled in: it sees the arity the call actually wrote,
and the winning declaration then supplies what the call omitted.

**A keyword call selects by name as well as by type.** A candidate with no parameter of that
name is not one the call could have meant.

**Overloads are all-or-nothing across a subclass boundary.** An `override` replaces the one
signature it matches and leaves the rest inherited:

```quince
class Greeter {
    fn hello(n: int): string { return "base int" }
    fn hello(s: string): string { return "base string" }
}
class Loud extends Greeter {
    override fn hello(n: int): string { return "loud int" }
}
print(Loud().hello(1), Loud().hello("x"))   # loud int base string
```

A class may declare several constructors, told apart the same way:

```quince
class Money {
    op init() { self.cents = 0 }
    op init(cents: int) { self.cents = cents }
    op init(text: string) { self.cents = int(text) }
    op init(whole: int, part: int) { self.cents = whole * 100 + part }
}
print(Money(), Money(5), Money("700"), Money(1, 25), Money(part: 5, whole: 2))

let wallet: Money        # the zero-argument one makes this legal
let coerced: Money = 5   # the one-parameter one makes this a conversion
```

`op init` is the exception to *inheritance*, as it is for `override`: a subclass's
constructors replace its parent's outright rather than joining them.

**Extensions coexist**, within one `extend` block and across modules, as long as their
signatures do not collide. A collision is found when the second block is resolved.

A name a *class* declares still cannot be extended at all — that refusal is about the
extension replacing part of a type, and is unchanged from v0.7.

---

## 7. Method & Operator Dispatch Internals

### 7.1 Method Resolution Chain

When invoking a method `obj.method_name(args)`:
1. The runtime retrieves `obj`'s target class descriptor `Class`.
2. It looks up `method_name` in `Class.methods`.
3. If not found in the leaf class, it recursively walks up `Class.parent` until a matching method is found or the root class is reached.
4. If no method is found, execution halts with a `NameError`.

### 7.2 Parent Method Invocation (`super`)

When executing `super.method_name(...)`:
- The lookup bypasses the receiver's dynamic class and begins directly at `Class.parent` of the class containing the `super` call.
- The receiver (`self`) remains bound to the leaf instance.

### 7.3 Binary Operator Evaluation Pipeline

Binary operators (`+`, `-`, `==`, `<=>`, etc.) follow an explicit dispatch algorithm (`src/syntax/ast/op.rs` and `src/interp/`):

1. **Primary Lookup (Left Operand)**:
   - Check if left operand `a` is a class instance defining the corresponding `op` slot (e.g., `op add`).
   - If defined, execute `a.op_add(b)`.

2. **Secondary Lookup (Right Operand Reflection)**:
   - If `a` does not define the slot, inspect the slot's `Reflect` policy in the static `OPS` dispatch table:
     - **`Reflect::Same`** (e.g., `op eq`): Checks if right operand `b` is a class instance defining `op eq`. If so, executes `b.op_eq(a)`.
     - **`Reflect::Negate`** (e.g., `op cmp`): Checks if right operand `b` defines `op cmp`. If so, executes `b.op_cmp(a)` and negates the result integer (`-res`).
     - **`Reflect::Never`** (e.g., `op add`, `op sub`, `op mul`, `op div`): Does **not** attempt right operand reflection. Immediately halts with a `TypeError`.

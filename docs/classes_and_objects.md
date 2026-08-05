# Quince v0.7 — Classes, OOP & Dispatch Reference

Quince v0.7 provides an object-oriented paradigm featuring class inheritance, openness control, explicit access visibility, extension blocks, and operator overloading via static operator slots.

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

### 4.2 Blank `final` Fields

A `final` field without an initial value (`public final id: string`) is a blank `final` field. 
- Must be assigned **exactly once** inside `op init`.
- Reassignment outside `op init` or a second assignment inside `op init` is refused.
- If a blank `final` field has a non-nullable type, every `op init` constructor path must assign it.

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

### 6.1 Complete List of Operator Slots (29 Slots)

| Slot Name  | Syntax Trigger          | Parameter Arity | Return Contract | Description                   |
| :--------- | :---------------------- | :-------------: | :-------------: | :---------------------------- |
| `init`     | `MyClass(...)`          |    Flexible     |      None       | Constructor initializer       |
| `bool`     | `if x`, `!x`, `bool(x)` |        0        |     `bool`      | Truthiness conversion         |
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

### 6.2 Return Type Contracts

Slots with a fixed return contract (e.g. `op string(): string`, `op bool(): bool`, `op len(): int`) enforce their return type both statically at declaration and dynamically at run time. Declaring `op string(): int` is refused at resolution time.

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

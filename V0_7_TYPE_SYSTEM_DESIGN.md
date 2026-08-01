# Quince v0.7 — Gradual Type Annotations (`T?`), Container Generics, Visibility (`pub`, `private`, `protected`), & LSP Tooling

This document outlines the technical design, grammar additions, runtime enforcement rules, and LSP integration specs for **Quince v0.7**.

---

## 1. Overview & Goals

Quince is a dynamically-typed scripting language with strong runtime typing. In **v0.7**, Quince introduces:
1. **Gradual Optional Type Annotations & Nullability (`T?`)**: Types are non-nullable by default unless marked with `?` (e.g. `int?`, `string?`).
2. **Typed Generic Containers (`list[T]`, `dict[K, V]`)**: Enforces element/key/value type constraints on collections.
3. **Reference Parameters (`ref`, `final ref`, `const ref`)**: Enables out-parameter references with explicit mutability contracts.
4. **Class Field Declarations & Access Control (`public`, `private`, `protected`)**: Declares class fields with type bounds and visibility modifiers.
5. **Module Export Visibility (`pub`)**: Controls which top-level symbols are exported during `import`.
6. **LSP Type Tooling**: Inlay Hints (`textDocument/inlayHint`), type completion, and live type diagnostics.

---

## 2. Syntax & Grammar Specifications

### 2.1 Lexer Tokens
- `:` (`TokenKind::Colon`) — Separator for type annotations.
- `?` (`TokenKind::Question`) — Nullability modifier.
- Visibility Keywords:
  - `pub` / `public` (`TokenKind::Pub`) — Public access / module export modifier.
  - `priv` / `private` (`TokenKind::Priv`) — Private access modifier.
  - `prot` / `protected` (`TokenKind::Prot`) — Protected access modifier.

---

### 2.2 Variable & Container Annotations

```quince
let x: int = 8
let opt: int? = 10
let name: string? = nil
final PI: float = 3.14159

# Generic Typed Containers
let numbers: list[int] = [1, 2, 3]
let names: list[string?] = ["alice", nil, "bob"]
let scores: dict[string, int] = {"alice": 95, "bob": 88}
```

* **Typed List (`list[T]`)**: Enforces that all items pushed or assigned to indices match `T`. `numbers.push("hello")` raises a runtime `TypeError`.
* **Typed Dict (`dict[K, V]`)**: Enforces that keys match `K` and values match `V`. `scores[123] = 90` raises a `TypeError`.
* **Untyped Containers (`list`, `dict`)**: Omission of brackets allows arbitrary `any` elements.

---

### 2.3 Reference Parameters (`ref`, `final ref`, `const ref`)

```quince
let x: int = 10

# 1. Plain `ref`: Read/write reference to a mutable `let` variable
fn increment(ref y: int) {
    y = y + 1
}

# 2. `final ref`: Reference to a `let` or `final` variable; `y` cannot be reassigned inside `example2`
fn inspect(final ref y: int) {
    print(y)
    # y = 5   # Error: cannot reassign `final ref` parameter `y`
}

# 3. `const ref`: Read-only reference accepting `let`, `final`, or `const` (frozen) variables
const MAX_LIMIT: int = 100
fn process(const ref limit: int) {
    print(limit)
    # limit = 200 # Error: cannot assign to read-only `const ref` parameter
}

process(MAX_LIMIT)   # OK: `const ref` accepts frozen `const` variables
increment(MAX_LIMIT) # Error: cannot pass `const` variable to mutable `ref` parameter
```

#### Pass-by-Reference Rules:
1. **LValue Call Requirement**: Calling a `ref` parameter requires an assignable variable (lvalue). `increment(10 + 2)` is a compile-time resolution error.
2. **`const` / `final` Protection**: Passing a `final` or `const` variable to a plain `ref` parameter is rejected at resolution time (`cannot pass final/const variable to mutable reference parameter`).
3. **Aliased Slot Type Safety**: Assigning to a `ref` parameter enforces the caller variable's type annotation even if the `ref` parameter itself is unannotated:
   ```quince
   let x: int = 10
   fn example(ref y) {
       y = "string" # TypeError: cannot assign 'string' to variable 'x' of type 'int'
   }
   example(x)
   ```

---

### 2.4 Class Field Declarations & Access Modifiers (`pub`, `priv`, `prot`)

Classes can declare fields directly in class bodies with type annotations and visibility keywords (using standard 3-letter shorthands `pub`, `priv`, `prot` or full names `public`, `private`, `protected`):

```quince
class BankAccount {
    priv let balance: int = 0
    pub final account_id: string
    prot let owner: string?

    fn init(id: string, initial_deposit: int) {
        self.account_id = id
        self.balance = initial_deposit
    }

    pub fn deposit(amount: int) {
        if amount > 0 {
            self.balance = self.balance + amount
        }
    }

    priv fn audit_log() {
        print("Auditing account:", self.account_id)
    }

    op string(): string {
        return "Account(" + self.account_id + "): " + string(self.balance)
    }
}

let acc = BankAccount("ACC123", 100)
acc.deposit(50)         # OK: public method
print(acc.balance)      # Error: VisibilityError: field 'balance' is private to class 'BankAccount'
```

#### Access Control Rules:
* **`private`**: Accessible ONLY inside methods of the declaring class.
* **`protected`**: Accessible inside methods of the declaring class OR its subclasses (`class SavingsAccount extends BankAccount`).
* **`public`**: Accessible from anywhere (default if keyword omitted).
* **Operator (`op`) Requirement**: Operator methods (`op string`, `op add`, `op len`, etc.) MUST be `public`. Marking an `op` as `private` or `protected` is a compile-time error.

---

### 2.5 Module Export Visibility (`pub`)

By default, top-level declarations in a module file (`math_utils.qn`) are private to that module. Using `pub` explicitly exports a declaration:

```quince
# math_utils.qn

pub final PI: float = 3.14159

pub fn calculate_area(radius: float): float {
    return PI * radius * radius
}

fn internal_helper(x: float): float {
    return x * 2.0
}
```

```quince
# main.qn

import math_utils

print(math_utils.calculate_area(5.0)) # OK: public function
print(math_utils.internal_helper(5.0)) # Error: VisibilityError: 'internal_helper' is private to module 'math_utils'
```

#### Module Visibility Rules:
* `pub let` / `pub final` / `pub const`: Exports global variables.
* `pub fn`: Exports top-level functions.
* `pub class`: Exports class definitions.
* Importing via `from module import name`: Rejects importing non-`pub` names with a compile-time `VisibilityError`.

---

### 2.6 Operator (`op`) Type Contract Validation

Operator methods carry built-in language protocol contracts:
* `op string`: expected return type `string` (or subclass).
* `op bool`: expected return type `bool`.
* `op int`: expected return type `int`.
* `op float`: expected return type `float`.
* `op list`: expected return type `list`.
* `op dict`: expected return type `dict`.
* `op len`: expected return type `int`.
* `op eq`, `op lt`, `op gt`: expected return type `bool`.
* `op cmp`: expected return type `int`.

Annotating an `op` with a conflicting return type (e.g. `op string(): int`) is a compile-time resolution error.

---

## 3. Type System & Compatibility Rules

### 3.1 Type Matching Matrix
| Annotation | Valid Values | Invalid Values (Raises `TypeError`) |
| :--- | :--- | :--- |
| `int` | Integers (`8`, `-42`) | Floats (`8.0`), Strings (`"8"`), `nil`, Bools |
| `int?` | Integers (`8`), `nil` | Floats, Strings, Bools |
| `float` | Floats (`3.14`, `-0.5`) | Integers (`3`), Strings, `nil` |
| `string` / `string?` | Strings (`"hello"`), `nil` (if `?`) | Numbers, Bools, Collections |
| `list[T]` | Lists where every item matches `T` | Non-lists or lists containing non-`T` items |
| `dict[K, V]` | Dicts where all keys match `K` and values match `V` | Non-dicts or dicts with mismatched K/V types |
| `any` | All values including `nil` | None |
| `UserClass` | Instances of `UserClass` or subclasses | Unrelated classes, `nil` (unless `?`) |

---

## 4. Enforcement Mechanics

1. **Resolution & Compile Time**:
   - Static literal assignment checks (`let x: int = "foo"`).
   - Visibility violations (`private`/`protected` member access, importing non-`pub` module symbols).
   - Passing `final`/`const` variables to mutable `ref` parameters.
   - Non-`public` `op` declarations.
2. **Runtime Slot & Call Boundary Checks**:
   - Variable reassignment (`Assign`) and reference write-backs enforce slot type bounds.
   - Container modifications (`push`, index `Set`, dict `Set`) validate element/key/value types.
   - Function parameter passing and explicit/implicit `return` values are validated against annotations.

---

## 5. LSP Tooling & Inlay Hints

1. **Inlay Hints (`textDocument/inlayHint`)**:
   - Displays inline type hints for unannotated variables (e.g. `let x /*: int*/ = 8`) and function parameter/return types.
2. **Editor Diagnostics**:
   - Live squiggly lines for `TypeError`, `VisibilityError`, and invalid `ref` arguments.
3. **Completions & Autocomplete**:
   - Suggests `pub`, `private`, `protected` modifiers, type annotations after `:`, and filters non-exported symbols during module autocompletion.

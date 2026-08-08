# Quince v0.8.1 — Control Flow, Functions & Error Handling

This manual details control flow constructs, lexical functions and closures, docstring validation, and exception handling in Quince v0.8.1.

---

## 1. Control Flow

### 1.1 Conditional Branching (`if` / `else`)

`if` statements evaluate conditions for truthiness. Truthiness is defined as follows:
- `false` and `nil` are falsy.
- All other built-in values (including `0`, `0.0`, `""`, `[]`, `{}`) are truthy.
- Custom class instances decide truthiness via `op bool`.

```quince
let count = 5

if count > 0 {
    print("Positive")
} else if count == 0 {
    print("Zero")
} else {
    print("Negative")
}
```

### 1.2 Loops (`while` and `for`)

- **`while`**: Repeatedly executes a block while the condition evaluates to truthy:
  ```quince
  let i = 0
  while i < 3 {
      print("Step:", i)
      i = i + 1
  }
  ```

- **`for`**: Iterates over containers (`string`, `list`, `dict`) or custom objects defining `op iter`:
  ```quince
  # Iterate over list items
  for item in [10, 20, 30] {
      print(item)
  }

  # Iterate over dict keys
  let scores = {"alice": 95, "bob": 88}
  for key in scores.keys() {
      print(key, "->", scores[key])
  }
  ```

---

## 2. Functions & Closures

### 2.1 Function Declarations

Functions are introduced with `fn`. Parameters and return values may carry optional type annotations and `const` qualifiers:

```quince
fn add(a: int, b: int): int {
    return a + b
}

fn greet(name: string? = nil) {
    print("Hello,", name ?? "guest")
}
```

Functions returning no value implicitly return `nil`.

### 2.2 Closures & Scope Capturing

Functions capture their lexical environment. A function returned from another retains access to variables in its enclosing scope:

```quince
fn make_counter(start: int): fn {
    let count = start
    return fn(): int {
        count = count + 1
        return count
    }
}

let counter = make_counter(10)
print(counter()) # 11
print(counter()) # 12
```

---

## 3. Documentation Comments (`##`)

Documentation blocks use `##` immediately above functions, methods, or classes. `syntax/doc.rs` parses these blocks and checks `@param` tags against parameter declarations:

```quince
## Computes the factorial of a positive integer.
##
## @param n input number
## @return the factorial value
fn factorial(n: int): int {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}
```

If `@param` names a parameter that does not exist in the signature, the compiler issues a diagnostic error.

---

## 4. Exception & Error Handling

### 4.1 `try` / `catch` Blocks

Exceptions raised during execution can be intercepted using `try` / `catch`:

```quince
try {
    let data = io.read("missing_file.txt")
    print(data)
} catch err {
    print("Caught error:", string(err))
}
```

**Scope Isolation Rule**: Variables declared inside the `try` block are **not** visible inside the `catch` block because execution might have failed before those initializations completed.

### 4.2 `throw` Statement

The `throw` statement raises an error. The value thrown must be an instance of `Error` (or a subclass of `Error`):

```quince
class ValidationError extends Error {
    public final field_name: string

    op init(field_name: string, message: string) {
        super.init(message)
        self.field_name = field_name
    }
}

fn validate_age(age: int) {
    if age < 0 {
        throw ValidationError("age", "Age cannot be negative")
    }
}
```

Throwing a non-`Error` instance raises an immediate `TypeError` at the `throw` site.

---

## 5. Standard Error Hierarchy & Kinds

Errors in Quince carry a specific `ErrorKind`:

| ErrorKind          | Triggers & Description                                                                |
| :----------------- | :------------------------------------------------------------------------------------ |
| `TypeError`        | Value passed to an annotated boundary or operation does not match required type       |
| `ValueError`       | Argument type is valid, but specific value is invalid (e.g. `sqrt(-1)`, `int("abc")`) |
| `KeyError`         | Explicitly attempting to `remove` a key that is missing from a dictionary             |
| `NameError`        | Accessing an undefined variable or type annotation name                               |
| `VisibilityError`  | Accessing a `private` or `protected` field/method outside allowed scope               |
| `OverflowError`    | Converting a float to int that exceeds 64-bit integer range                           |
| `IoError`          | Filesystem or input/output operation failed                                           |
| `ArityError`       | Function or operator called with wrong number of arguments                            |
| `DeclarationError` | Invalid class modifier, invalid `op` slot return contract, or duplicate name          |
| `FreezeError`      | Mutating an object marked as `const`                                                  |

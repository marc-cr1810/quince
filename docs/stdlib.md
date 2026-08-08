# Quince v0.8.1 — Standard Library & Built-ins Reference

This document provides a comprehensive API reference for Quince v0.8.1's global built-in functions, type conversions, container methods, and standard modules (`math`, `io`, `random`, `time`), complete with runnable code examples and edge-case exception details.

---

## 1. System Globals

Quince provides three top-level global functions:

### `print(...values)`
- **Parameters**: `values: any?` (variadic)
- **Returns**: `nil`
- **Description**: Formats all arguments into strings using their `op string` implementation, writes them to standard output separated by spaces, and appends a newline.
```quince
print("User:", "Alice", "Age:", 30, "Active:", true)
# Output: User: Alice Age: 30 Active: true
```

### `len(value)`
- **Parameters**: `value: any`
- **Returns**: `int`
- **Description**: Returns character count for `string`, item count for `list`, entry count for `dict`, or delegates to `op len()` for custom classes. Raises `TypeError` if `value` does not support length.
```quince
print(len("hello"))      # Output: 5
print(len([10, 20, 30])) # Output: 3
print(len({"a": 1}))     # Output: 1
```

### `type(value)`
- **Parameters**: `value: any?`
- **Returns**: `string`
- **Description**: Returns the runtime type name of `value` as a string (e.g. `"int"`, `"string"`, `"list"`, `"MyClass"`).
```quince
print(type(42))         # Output: "int"
print(type("text"))     # Output: "string"
print(type([1, 2]))     # Output: "list"
```

---

## 2. Type Constructors & Conversions

Built-in type constructors convert existing values into primitive or container types:

### `int(x)`
- **Parameters**: `x: any`
- **Returns**: `int`
- **Description**: Converts `x` to integer.
  - Float: Truncates toward zero (`int(3.9) -> 3`, `int(-3.9) -> -3`). Raises `OverflowError` if out of 64-bit bounds.
  - String: Trims surrounding whitespace and parses integer (`int(" 42 ") -> 42`). Raises `ValueError` if string is unparseable (`int("abc")`).
  - Bool: `int(true) -> 1`, `int(false) -> 0`.
  - Raises `TypeError` for types that cannot be converted.

### `float(x)`
- **Parameters**: `x: any`
- **Returns**: `float`
- **Description**: Converts `x` to float. Accepts `float`, `int`, `string`, `bool`. Trims and parses strings (`float(" 3.14 ") -> 3.14`). Raises `ValueError` for invalid string numbers.

### `string(x)`
- **Parameters**: `x: any?`
- **Returns**: `string`
- **Description**: Converts any value to string using its `op string` representation. Never fails.

### `bool(x)`
- **Parameters**: `x: any?`
- **Returns**: `bool`
- **Description**: Returns `true` if `x` is truthy, `false` if falsy. Delegates to `op bool()` for class instances. Never fails.

### `list(xs)`
- **Parameters**: `xs: list?` (0 or 1 argument)
- **Returns**: `list`
- **Description**: `list()` creates a new empty list. `list(xs)` creates a shallow copy of `xs`. Refuses string and dict inputs (raises `TypeError` suggesting `chars()`, `keys()`, or `values()`).

### `dict(d)`
- **Parameters**: `d: dict?` (0 or 1 argument)
- **Returns**: `dict`
- **Description**: `dict()` creates a new empty dictionary. `dict(d)` creates a shallow copy of `d`.

---

## 3. Native Methods on Built-in Types

### 3.1 `string` Methods

```quince
# repeat(n: int): string
print("ab".repeat(3)) # "ababab"
# "ab".repeat(-1)     # Raises ValueError

# upper(): string / lower(): string
print("Hello".upper()) # "HELLO"
print("Hello".lower()) # "hello"

# trim(): string
print("  hello \n".trim()) # "hello"

# starts_with(prefix: string): bool / ends_with(suffix: string): bool
print("report.csv".starts_with("rep")) # true
print("report.csv".ends_with(".csv"))  # true

# replace(from: string, to: string): string
print("banana".replace("a", "o")) # "bonono"
# "test".replace("", "x")          # Raises ValueError

# split(separator: string): list[string]
let parts = "a,b,c".split(",") # ["a", "b", "c"]
# "a,b".split("")              # Raises ValueError (use chars())

# chars(): list[string]
let ch = "hi".chars() # ["h", "i"]

# join(items: list[string]): string
let joined = ", ".join(["alice", "bob"]) # "alice, bob"
# ", ".join([1, 2])                      # Raises TypeError (convert items to string first)
```

### 3.2 `list` Methods

```quince
# push(item: T): nil
let nums: list[int] = [1, 2]
nums.push(3) # nums is now [1, 2, 3]

# reverse(): list[T]
let rev = [1, 2, 3].reverse() # [3, 2, 1]

# find(item: any): int
print(["a", "b", "c"].find("b")) # 1
print(["a", "b", "c"].find("z")) # -1

# sum(): any
print([10, 20, 30].sum())         # 60
print(["a", "b", "c"].sum())       # "abc"
print([].sum())                    # 0

# map(f: fn): list
fn double(x: int): int { return x * 2 }
print([1, 2, 3].map(double)) # [2, 4, 6]

# filter(f: fn): list
fn is_even(x: int): bool { return x % 2 == 0 }
print([1, 2, 3, 4].filter(is_even)) # [2, 4]

# sort(): list[T]
print([3, 1, 4, 2].sort()) # [1, 2, 3, 4]
```

### 3.3 `dict` Methods

```quince
let scores = {"alice": 95, "bob": 88}

# keys(): list[K]
print(scores.keys()) # ["alice", "bob"]

# values(): list[V]
print(scores.values()) # [95, 88]

# get(key: K, default: any): any
print(scores.get("alice", 0))   # 95
print(scores.get("charlie", 0)) # 0

# remove(key: K): V
let removed = scores.remove("bob") # 88
# scores.remove("missing")          # Raises KeyError
```

---

## 4. Standard Library Modules

### 4.1 `math` Module

```quince
import math

print(math.pi) # 3.141592653589793
print(math.e)  # 2.718281828459045

print(math.floor(3.7))  # 3 (returns int)
print(math.ceil(3.2))   # 4 (returns int)
print(math.round(3.5))  # 4 (returns int)

print(math.abs(-5))     # 5 (returns int)
print(math.abs(-5.5))   # 5.5 (returns float)

print(math.sqrt(16.0))  # 4.0
# math.sqrt(-1.0)       # Raises ValueError

print(math.pow(2, 3))   # 8.0

print(math.min(10, 20)) # 10
print(math.max(10, 20)) # 20
```

### 4.2 `io` Module

```quince
import io

# Write text to a file
io.write("output.txt", "Hello World\n")

# Append text to a file
io.append("output.txt", "Second Line\n")

# Check file existence
if io.exists("output.txt") {
    # Read entire file as string
    let text = io.read("output.txt")

    # Read file lines into list[string]
    let file_lines = io.lines("output.txt")
    print("Line count:", len(file_lines))
}

# Read a line from standard input
let user_input: string? = io.line()
```

### 4.3 `random` Module

```quince
import random

# Seed the generator for exact reproducible sequence
random.seed(42)

# Random integer between low and high (inclusive)
let die_roll = random.int(1, 6) # e.g. 4

# Random float in [0.0, 1.0)
let val = random.float()

# Pick one item randomly from a list
let choice = random.choice(["apple", "banana", "cherry"])
```

*Default Seed Behavior*: When `random.seed()` is not called, Quince initializes the generator with a deterministic seed (`0x2545F4914F6CDD1D`). This ensures that test runs and benchmarks yield reproducible results across runs. Call `random.seed(time.now())` when non-deterministic randomness is required.

### 4.4 `time` Module

```quince
import time

# Seconds since Unix epoch as float
let start_time = time.now()

# Pause execution thread
time.sleep(0.1) # Sleeps for 100 milliseconds

let elapsed = time.now() - start_time
print("Elapsed seconds:", elapsed)
```

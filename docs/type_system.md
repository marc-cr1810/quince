# Quince v0.7 — Type System Reference

Quince v0.7 features a gradually-typed type system that combines static editor diagnostics with fast, sound runtime boundary enforcement. Types are optional: unannotated code is dynamically typed, while annotations impose static checks in the editor and boundary enforcement at run time.

---

## 1. Gradual Typing Architecture

### 1.1 `Unknown` vs Stated Annotations

The semantic analysis pass (`sema/`) tracks type information across expressions:

- **`Unknown`**: Represents an unannotated binding or an expression whose type cannot be statically inferred. `Unknown` allows any runtime operation and relies on runtime checks.
- **Stated Type**: An explicit annotation (e.g. `let x: int = 5`). The type checker verifies initializing expressions and function boundaries against stated type claims.

### 1.2 Static Warnings vs Runtime Enforcement

Quince draws a deliberate distinction between static reporting and runtime execution:
- **Static Diagnostics**: Type mismatches visible in the source (e.g., `let x: int = "hello"`) generate warnings in the Language Server and editor squiggles. If static inference is uncertain (containing `Unknown`), static warnings are suppressed to avoid false positives.
- **Runtime Enforcement**: Type annotations are enforced at runtime boundaries (variable assignment, container mutation, function entry/exit). If an invalid value reaches an annotated boundary, execution halts with a `TypeError`.

---

## 2. Type Annotations & Taxonomy

### 2.1 Primitive Types & Automatic Float Widening

Quince provides four primitive types:
- `int`: Signed 64-bit integers (`i64`).
- `float`: Double-precision 64-bit floats (`f64`).
- `string`: Immutable UTF-8 string sequences (`Rc<str>`).
- `bool`: Truth values (`true` or `false`).

**Integer Widening Rule**: A float annotation (`float`) accepts an integer value and automatically converts (widens) it to a float at the assignment boundary.
```quince
let ratio: float = 4        # Accepted: automatically widened to 4.0
let n: int = 3.14           # Refused (TypeError): narrowing requires explicit int(3.14)
```

### 2.2 Nullability (`T` vs `T?`)

Types in Quince are **non-nullable by default**. A variable annotated as `string` or `int` cannot hold `nil`. Append `?` to declare a nullable type:

```quince
let name: string = "Alice"   # Cannot be nil
let age: int? = nil          # May hold an integer or nil
```

Attempting to assign `nil` to a non-nullable type raises a `TypeError` at the boundary. Applying double nullability (e.g., `int??`) is refused at resolution time.

### 2.3 Top Types (`any`, `any?`, `_`)

Quince provides explicit top types to express dynamic values in type position:

| Annotation | Description        | Admits `nil`? | Standard Usage Context                                   |
| :--------- | :----------------- | :-----------: | :------------------------------------------------------- |
| `any`      | Non-nil top type   |      No       | Whole binding or item constraint excluding `nil`         |
| `any?`     | Universal top type |      Yes      | Universal value holder                                   |
| `_`        | Wildcard symbol    |      No       | Container argument shorthand (e.g., `list[_]`)           |
| `_?`       | Nullable wildcard  |      Yes      | Unconstrained container value (e.g., `dict[string, _?]`) |

```quince
let unconstrained: any? = nil
let non_null_item: any = "data"
let wildcard_list: list[_] = [1, "two", true]
```

---

## 3. Container Generics

### 3.1 `list[T]`

A list container carries a reified type descriptor `T`:

```quince
let numbers: list[int] = [10, 20, 30]
numbers.push(40)               # Verified at boundary
let items: list[any?] = [1, "foo", nil]
```

Attempting to `push` or insert an element that does not match `T` raises a `TypeError`.

### 3.2 `dict[K, V]` and Single-Argument Shorthand `dict[K]`

Dictionaries specify both key (`K`) and value (`V`) types:

```quince
let user_ages: dict[string, int] = {"alice": 30, "bob": 25}
```

**Single-Argument Shorthand**: Writing `dict[K]` is shorthand for `dict[K, _?]` (keys are constrained to `K`, values are completely unconstrained).

```quince
let config: dict[string] = {"host": "localhost", "port": 8080, "debug": true}
config["timeout"] = 30.0       # Accepted: value is unconstrained
```

### 3.3 Dict Key Constraints

Key types (`K`) are restricted to primitive hashable types: `nil`, `bool`, `int`, `float`, and `string`. Custom class instances cannot be used as dictionary keys. Declaring `dict[Point, int]` is refused at compile/resolution time.

### 3.4 Safe Nullable Indexing (`d[key] -> V?`)

Reading a dictionary key (`d[key]`) returns `V?` (a nullable value) rather than raising a missing key error:

```quince
let scores: dict[string, int] = {"alice": 95}
let score: int? = scores["bob"]   # Evaluates to nil without raising KeyError
print(score ?? 0)                  # Outputs 0 using null coalescing
```

To verify whether a key exists in the dictionary regardless of whether its stored value is `nil`, use the `in` operator:

```quince
if "bob" in scores {
    print("Key is present!")
}
```

*Note*: Explicitly calling `scores.remove("missing_key")` raises a `KeyError`.

---

## 4. `const T` Qualifiers & Value Freezing

The `const T` qualifier enforces deep immutability at parameters and return boundaries:

```quince
# Function guarantees it will not mutate caller data
fn process_dataset(data: const list[int]) {
    # data is deeply frozen; data.push(5) raises a FreezeError
}

# Function returns a deeply frozen view of internal data
class Configuration {
    private let settings: dict[string, string] = {"env": "prod"}

    public fn get_settings(): const dict[string, string] {
        return self.settings
    }
}
```

Difference between qualifiers:
- **`let` / `final`**: Controls whether the *binding name* can be reassigned.
- **`const T`**: Controls whether the *underlying object value* can be mutated.

---

## 5. Type Aliases (`alias`)

Type aliases allow creating descriptive names for complex types:

```quince
alias UserID = string
alias ScoreTable = dict[UserID, int]

let user_scores: ScoreTable = {"USR_01": 100}
```

- Aliases are substituted at resolution time and introduce no runtime overhead.
- `is` checks and error diagnostics resolve aliases to their target type or print the alias name as declared.
- Cyclic alias definitions (e.g. `alias A = B`, `alias B = A`) are detected and reported as resolution errors.

---

## 6. Type Guarding & Smart Casting (`is`)

### 6.1 The `is` Operator

The `is` operator performs exact runtime type checks against values and container descriptors:

```quince
let item: any? = [1, 2, 3]

if item is list[int] {
    print("Exact list[int] container match!")
}
```

**O(1) Performance**: Because container allocations carry reified type headers, `item is list[int]` checks the header descriptor in O(1) time without performing element scans.

### 6.2 Block-Scoped Smart Casting

When an `if`, `while`, or `&&` condition tests a variable with `is`, Quince narrows (smart-casts) the variable's type within the guarded scope:

```quince
let val: string? = fetch_input()

if val is string {
    # `val` is narrowed from `string?` to `string` in this block
    print(val.upper()) # Reaches string methods without optional chaining
}

if val is string && len(val) > 0 {
    # Narrowed on the right-hand side of && as well
    print(val)
}
```

---

## 7. Compiler Internals & Memory Layout

### 7.1 Value Representation (`Value`)

In the interpreter runtime (`src/runtime/value.rs`), all Quince values are represented by the `Value` enum:

- `Value::Int(i64)`: Inline 64-bit integer.
- `Value::Float(f64)`: Inline 64-bit float.
- `Value::Bool(bool)`: Inline boolean.
- `Value::Nil`: Inline nil singleton.
- `Value::Str(Rc<str>)`: Reference-counted UTF-8 string slice.
- `Value::List(ObjId)`: Heap handle pointing to a list object.
- `Value::Dict(ObjId)`: Heap handle pointing to a dictionary object.
- `Value::Instance(ObjId)`: Heap handle pointing to a class instance object.
- `Value::Class(Rc<Class>)`: Shared reference to class metadata.
- `Value::Native(&'static Native)`: Static descriptor for standard library C-native functions.
- `Value::Closure(Rc<Closure>)`: Shared reference to captured lexical environment and AST function code.

### 7.2 Heap Architecture & Object Handles (`ObjId`)

Compound objects (`List`, `Dict`, `Instance`) are allocated on an arena heap managed by `Heap` (`src/runtime/heap.rs`). References between objects use lightweight 32-bit `ObjId` handles instead of raw Rust pointers.

### 7.3 Reified Container Descriptors

When a container object is allocated:
- `Object::List`: Stores both `items: Vec<Value>` and an optional reified type descriptor `elem_type: Type`.
- `Object::Dict`: Stores `entries: Dict` along with `key_type: Type` and `val_type: Type` descriptors.

Because the type descriptor is stored directly in the heap header of the container, runtime operations such as `xs is list[int]` compare the stored `elem_type` descriptor in $O(1)$ constant time, avoiding full element array traversals.

### 7.4 Generic Invariance Under Mutability

Container generics in Quince are **invariant**: `list[int]` is **not** a subtype of `list[any?]`.

**Rationale**: If `list[int]` were treated as a subtype of `list[any?]`, a function receiving `list[any?]` could push a `string` into the list. This would corrupt the internal `list[int]` container, violating non-null integer type safety when read elsewhere.

### 7.5 Deep Freezing Mechanics

Heap objects carry a `frozen: bool` flag:
- When a value is assigned to a `const` variable or passed across a `const T` parameter boundary, the interpreter recursively marks the heap object and all nested references as `frozen`.
- Attempting to call mutating operations (such as `list.push()`, `dict.remove()`, or setting a field on a class instance) checks `heap.is_frozen(obj)`.
- If frozen, the interpreter halts execution and raises a `FreezeError`.

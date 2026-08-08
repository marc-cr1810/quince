# Quince v0.8.1 — Type System Reference

Quince v0.8.1 features a gradually-typed type system that combines static editor diagnostics with fast, sound runtime boundary enforcement. Types are optional: unannotated code is dynamically typed, while annotations impose static checks in the editor and boundary enforcement at run time.

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

Because it is shorthand and not silence, it is a claim like any other. A `dict[string, int]`
is a `dict[string]` — `int` is one of the things `any?` admits — and a `dict[string]` is **not**
a `dict[string, int]`:

```quince
fn scores(d: dict[string, int]): int { return len(d) }
scores(config)
# TypeError: `d` is `dict[string, int]`, but this is `dict[string]`
```

That refusal is what keeps the shorthand honest. The header a container carries is written
once, so `config` stays a `dict[string]` however it is passed; admitting it as a
`dict[string, int]` would leave every write through that name unchecked against `int`.

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

`const` is one word for one idea at three positions: on a binding the name is bound once and
the value frozen, on a parameter or return the value is frozen as it crosses the boundary,
and on a `fn` or `op` the body may not change state at all — see
[`const fn`](classes_and_objects.md#6a2-const-fn-and-const-op).

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

The `is` operator asks what a value **is**, reading a container's reified header rather than
its elements:

```quince
let ints: list[int] = [1, 2, 3]

if ints is list[int] {
    print("A list of ints.")
}
```

**One rule, shared with annotations.** `is` compares type arguments by exactly the table §11.4
gives, so a question and a parameter never disagree:

```quince
print(ints is list[int])       # true
print(ints is list[any])       # true  — `any` is the top type
print(ints is list[any?])      # true
print(ints is list)            # true  — the same type as `list[any?]`
print(ints is list[int?])      # false — invariant; a `nil` written through
print(ints is list[string])    # false
```

An argument nobody wrote is `any?` (§3.2), so `list` and `list[any?]` are one type spelled two
ways and answer alike. The same holds for `dict` and `dict[K]`.

**Two things `is` does that an annotation does not**, and both come of it asking what a value
already is rather than what it may become:

```quince
print(1 is float)              # false — an annotation widens; `1` is still an int
print([1, 2] is list[int])     # false — nothing has said what it holds
print([1, 2] is list[any?])    # true  — so it is the top container type
```

A container nothing has described has no element type yet. A `let` is what gives it one, so
`let xs: list[int] = [1, 2]` is still accepted — the annotation is *deciding* the type, not
agreeing with one. After that line `xs is list[int]` is true.

**O(1) Performance**: because container allocations carry reified type headers, `ints is
list[int]` compares the header in O(1) time. It never scans elements — which is why an
undescribed container answers from the elided-argument rule instead of by looking.

### 6.2 Block-Scoped Smart Casting

When an `if`, `while`, or `and` condition tests a variable with `is`, Quince narrows (smart-casts) the variable's type within the guarded scope:

```quince
let val: string? = fetch_input()

if val is string {
    # `val` is narrowed from `string?` to `string` in this block
    print(val.upper()) # Reaches string methods without optional chaining
}

if val is string and len(val) > 0 {
    # Narrowed on the right-hand side of `and` as well
    print(val)
}
```

`is not` does **not** narrow. What a failed type test proves is a fact about the *other*
branch, which this pass has no way to express — so `if val is not string { }` leaves `val`
at `string?` throughout.

---

## 8. Implicit Constructor Coercion

An annotation naming a class that declares a **single-parameter `op init`** is a standing
offer to convert: a value of the constructor's parameter type is built rather than refused.

```quince
class CustomInt {
    private let value: int = 0
    public op init(value: int) { self.value = value }
}

let i: CustomInt = 10            # implicitly CustomInt(10)

fn doubled(c: CustomInt): int { … }
print(doubled(21))               # the parameter is a boundary too
```

It applies at every boundary an annotation is checked at — a binding, a field, a parameter,
and a return.

*Rules*:
- **Implicit by default.** A single-parameter `op init` coerces unless it says otherwise.
- **Only single-parameter constructors coerce.** There is no rule that could pick among
  several arguments from one value. A class declaring several constructors still offers the
  conversion through whichever of them takes one parameter the value fits.
- **Only one step.** Coercion does not chain: if `A` is built from a `B` and `B` from an
  `int`, `let a: A = 1` is refused as an `int` that is not an `A`.
- **The payload is checked first**, against the constructor's own parameter type, so a value
  that does not hold is reported as a type error rather than failing inside the constructor.
- **A builtin does not coerce.** `let s: string = 5` is still refused: a builtin's `init` is
  a *conversion*, and admitting those would silently stringify every wrong argument.

### 8.1 `explicit`

A constructor whose argument is not a conversion says so, and the implicit form is then
refused:

```quince
class DatabaseConnection {
    private let timeout_ms: int = 0
    public explicit op init(timeout_ms: int) { self.timeout_ms = timeout_ms }
}

let db: DatabaseConnection = DatabaseConnection(1000)   # accepted
# let db: DatabaseConnection = 1000                     # TypeError: constructor is `explicit`
```

`DatabaseConnection(1000)` reads as a timeout only because the call names the class;
`let db: DatabaseConnection = 1000` reads as nothing at all. `explicit` may be written only
on a one-parameter `op init` — there is nothing anywhere else for it to turn off.

**Why implicit by default**, which is the reverse of C++'s answer: the classes this exists
for are the ones that wrap one value and mean it. The one-step rule is what stops conversions
composing into a search, which is what made C++ choose otherwise.

---

## 9. Default Construction

A declaration with no `= value` takes the default its type answers with.

```quince
let logger: Logger               # Logger()
let items: list[int]             # []
let config: dict[string, string] # {}
let anything                     # nil — no annotation, no rule
```

Which types can answer:

| Type | Default |
| :--- | :--- |
| `list`, `list[T]` | `[]` |
| `dict`, `dict[K, V]` | `{}` |
| A class whose first `op init` up the chain requires no arguments | that constructor |
| A class declaring no `op init` at all | a synthesized `op init() {}` |
| Anything else — `int`, `float`, `string`, `bool`, `any` | refused at resolution |

A parameterized constructor suppresses the synthesized one: declaring `op init(val: int)`
makes `let obj: MyClass` a `DeclarationError`, because a class that requires an argument means
it. Declaring `op init()` beside it brings the default back.

There is no honest default for an `int` — zero is a value somebody chose — which is why the
refusal exists rather than a guess.

---

## 10. Default Parameters & Keyword Arguments

```quince
fn connect(host: string, port: int = 8080, timeout: int = 3000): Connection { … }

connect("localhost")                                   # 8080, 3000
connect("127.0.0.1", timeout: 5000)                    # targets one defaulted parameter
connect(timeout: 5000, host: "api.domain.com", port: 443)   # all by name, any order
```

*Rules*:
- **Defaulted parameters follow mandatory ones.** A mandatory parameter after a defaulted one
  is refused: there is no call that could reach it positionally.
- **Keyword arguments match declared parameter names**, at any position after the last
  positional argument. A positional argument following a named one is refused — that ordering
  has no reading that is not a guess.
- **A parameter may be filled once.** Supplying it positionally and again by name is an error
  naming it, rather than last-wins.
- **Defaults are evaluated at the call**, in the callee's declaration scope, every time. So
  `fn f(xs: list = [])` builds a fresh list per call and carries no mutation between them.
  This is the one place Python's answer is refused outright.
- **`?` on the type does not imply a default.** `fn f(x: int?)` requires an argument; only
  `= nil` makes one optional.
- **A builtin takes its arguments positionally.** `len(value: [1])` is refused: a native's
  parameters are a static table with nothing there to default.

---

## 11. Compiler Internals & Memory Layout

### 11.1 Value Representation (`Value`)

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

### 11.2 Heap Architecture & Object Handles (`ObjId`)

Compound objects (`List`, `Dict`, `Instance`) are allocated on an arena heap managed by `Heap` (`src/runtime/heap.rs`). References between objects use lightweight 32-bit `ObjId` handles instead of raw Rust pointers.

### 11.3 Reified Container Descriptors

When a container object is allocated:
- `Object::List`: Stores both `items: Vec<Value>` and an optional reified type descriptor `elem_type: Type`.
- `Object::Dict`: Stores `entries: Dict` along with `key_type: Type` and `val_type: Type` descriptors.

Because the type descriptor is stored directly in the heap header of the container, runtime operations such as `xs is list[int]` compare the stored `elem_type` descriptor in $O(1)$ constant time, avoiding full element array traversals.

### 11.4 Generic Invariance Under Mutability

Container generics in Quince are **invariant**, with one exception. A value carries the type
arguments it was built to hold — the reified descriptor of §11.3 — and that is what every
boundary compares against:

```quince
let scores: dict[string, int] = {}

fn flags(d: dict[string, bool]): int { return len(d) }
flags(scores)
# TypeError: `d` is `dict[string, bool]`, but this is `dict[string, int]`
```

Emptiness makes no difference. `let xs: list[int] = []` is a list of ints while it is empty,
because the annotation said so — the elements are not consulted for a container that carries
a header, and a container without one is checked by walking them.

**The exception is `any`**, which is the top type and accepts whatever is there, so
`list[any]` still means "a list of anything":

```quince
fn count(xs: list[any]): int { return len(xs) }
let ints: list[int] = [1, 2]
print(count(ints))          # 2
```

**Why that is safe rather than a hole**: a *write* is checked against the header, not against
the annotation the value arrived through. So the corruption invariance exists to prevent is
already prevented one level down:

```quince
fn grow(xs: list[any]) { xs.push("a string") }
grow(ints)
# TypeError: the item is `int`, but this is a string
```

Nothing else widens. A `list[int]` is not a `list[int?]`, because a `nil` written through the
second is a `nil` read out of the first.

### 11.5 Deep Freezing Mechanics

Heap objects carry a `frozen: bool` flag:
- When a value is assigned to a `const` variable or passed across a `const T` parameter boundary, the interpreter recursively marks the heap object and all nested references as `frozen`.
- Attempting to call mutating operations (such as `list.push()`, `dict.remove()`, or setting a field on a class instance) checks `heap.is_frozen(obj)`.
- If frozen, the interpreter halts execution and raises a `FreezeError`.

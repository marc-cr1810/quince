# Quince v0.15: Generic Functions, Typed `catch` Blocks & Ternary Conditional Operator

## Executive Summary

Quince v0.15 completes the core language surface by introducing **Generic Functions (`fn map[T, U]`)**, **Typed `catch` Blocks (`catch err: Type`)**, and the **Ternary Conditional Operator (`c ? t : f`)**.

---

## 1. Generic Functions (`fn name[T1, ..., TN]`)

### 1.1 Declaration Syntax
Standalone functions declare generic type parameters within square brackets after the function name:

```quince
pub fn map[T, U](items: list[T], transform: function(T) -> U): list[U] {
    let result: list[U] = []
    for item in items {
        result.push(transform(item))
    }
    return result
}

pub fn first[T](items: list[T]): T? {
    if len(items) == 0 {
        return nil
    }
    return items[0]
}
```

### 1.2 Call-Site Type Parameter Inference
Type parameters `[T, U]` are automatically inferred from argument types when invoked:

```quince
let numbers: list[int] = [1, 2, 3]
let doubled = map(numbers, fn(x: int) -> int { return x * 2 })
# Inferred: T = int, U = int
```

Explicit type argument instantiation is also supported:

```quince
let strings = map[int, string](numbers, fn(x: int) -> string { return string(x) })
```

---

## 2. Typed `catch` Blocks

### 2.1 Multi-Branch Exception Filtering
Quince `try / catch` supports chained typed `catch` branches to filter caught exceptions by error class or type annotation:

```quince
class IOError extends Error {}
class JSONParseError extends Error {
    public let line: int
}

try {
    let content = io.read_file("config.json")
    let data = json.parse(content)
} catch err: IOError {
    sys.eprintln("File access failed: " + err.message)
} catch err: JSONParseError {
    sys.eprintln(f"JSON error on line {err.line}: {err.message}")
} catch err {
    sys.eprintln("Uncaught general error: " + string(err))
}
```

### 2.2 Scoping & Runtime Matching Rules
1. `catch err: Type` matches if the thrown exception value is an instance of `Type` or a subclass of `Type` (checked via `is`).
2. Each `catch` block introduces a local lexical scope where `err` is bound to the caught exception value.
3. An unannotated `catch err` serves as the wildcard fallback branch.

---

## 3. Ternary Conditional Operator (`c ? t : f`)

### 3.1 Expression Syntax
```quince
let status = age >= 18 ? "Adult" : "Minor"
let label = score >= 90 ? "A" : score >= 80 ? "B" : "C"
```

### 3.2 Precedence & Short-Circuiting
- **Precedence:** Binds tighter than assignment (`=`), looser than null coalescing (`??`).
- **Short-circuiting:** Evaluates `condition`. If truthy, only `true_expr` is evaluated; otherwise, only `false_expr` is evaluated.

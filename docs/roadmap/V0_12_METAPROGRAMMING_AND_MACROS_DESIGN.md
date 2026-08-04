# Quince v0.12: Metaprogramming, Compile-Time Function Execution & AST Macros

## Executive Summary

Quince v0.12 introduces **Compile-Time Function Execution (CTFE)**, **Hygienic AST Macros (`macro`)**, and **Type Reflection (`type_of(T)`)**.

Key Features:
1. **CTFE (`const fn`)**: Pure functions marked as `const fn` can be executed during static resolution to compute constants at compile time.
2. **Hygienic Macros (`macro`)**: Syntactic macro expansions operating on Quince AST nodes (`Expr`, `Stmt`) using `quote` and `unquote` syntax.
3. **Type Reflection (`type_of(T)`)**: Compile-time reflection API providing field names, variant lists, and method signatures without runtime reflection overhead.

---

## 1. Compile-Time Function Execution (CTFE)

Functions marked as `const fn` are evaluated during the compilation pass when invoked in constant positions (e.g. array dimensions `array[int, compute_size()]` or constant initializers):

```quince
const fn compute_buffer_size(depth: int): int {
    return 1024 * (1 << depth)
}

const BUFFER_CAPACITY: int = compute_buffer_size(3) # Evaluated at compile-time (8192)
```

---

## 2. Hygienic AST Macros (`macro`)

Macros allow programmatic generation and transformation of AST nodes prior to Cranelift IR compilation:

```quince
pub macro assert_eq(left: Expr, right: Expr) {
    quote {
        let l = unquote(left)
        let r = unquote(right)
        if l != r {
            throw Error("Assertion failed: " + string(l) + " != " + string(r))
        }
    }
}
```

---

## 3. Type Reflection API (`type_of(T)`)

Static type reflection yields metadata at compile-time:

```quince
let info = type_of(User)
for let field in info.fields {
    print("Field: " + field.name + " of type " + field.type_name)
}
```

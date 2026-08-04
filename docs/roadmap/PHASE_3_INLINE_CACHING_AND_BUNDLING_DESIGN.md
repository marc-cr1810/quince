# Phase 3: Morphic Inline Caching (IC) & Executable Bundling Architecture

## Executive Summary

Phase 3 introduces **Morphic Inline Caching (IC)** for property access and interface calls, along with the single-file **Executable Bundler (`quince build --bundle`)**.

---

## 1. Morphic Inline Caching (IC)

Property access (`GetProperty`) and polymorphic interface calls (`InvokeInterface`) use inline cache slots:

```rust
pub struct InlineCacheSlot {
    pub cached_class_id: u32,
    pub resolved_offset: u32,
}
```

- **Mono-morphic Call Sites**: When `class_id` matches the cached ID, execution jumps directly to `resolved_offset` in $O(1)$ time, skipping dynamic hash table lookups.
- **Poly-morphic Fallback**: Polymorphic call sites transition to a 4-entry inline cache array before falling back to generic lookup.

---

## 2. Executable Bundler (`quince build --bundle`)

`quince build --bundle main.qn -o main`:
- Concatenates the lightweight pre-compiled `quince-runtime` binary stub with a serialized `.qnc` bytecode payload.
- Enables sub-second native binary output without requiring host C linkers or LLVM dependencies.

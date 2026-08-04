# Phase 2: IEEE 754 NaN-Tagging & `Weak[T]` Memory System

## Executive Summary

Phase 2 replaces 24-byte `Value` enum allocations with an **8-Byte IEEE 754 NaN-Tagged Value Layout (`Value(u64)`)** and introduces **Weak References (`Weak[T]`)** to support cycle-free data structures.

---

## 1. IEEE 754 NaN-Tagging Bit Pattern (8 Bytes)

Floating-point numbers use standard double precision. Non-float values are packed into the quiet NaN payload space (`0x7FF8000000000000`):

```text
64                             48                   0
┌──────────────────────────────┬────────────────────┐
│ 1111111111111000 | Tag (4b)  │ Payload (48-bit)   │
└──────────────────────────────┴────────────────────┘
```

- **Tags**:
  - `0x1`: `Nil`
  - `0x2`: `True`
  - `0x3`: `False`
  - `0x4`: 32-bit `Int` payload
  - `0x5`: Heap `ObjId` handle
  - `0x6`: Small String Optimization (SSO inline string $\le 6$ bytes)

---

## 2. `Weak[T]` Non-Tracing Reference Handles

- `Weak[T]` holds a non-owning handle to a heap object.
- **Cycle Prevention**: Resolving `weak_ref.upgrade(): Option[T]` returns `nil` when the referenced object has been collected by GC.

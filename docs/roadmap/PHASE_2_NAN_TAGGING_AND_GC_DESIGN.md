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

**`Weak[T]` is language surface owned by no milestone** — a built-in generic type with a
method, in the same category as `list[T]` or `range`. `BYTECODE_VM_DESIGN.md` §12 lists it as
unscheduled. The representation below is this phase's; the type, its interaction with `is`,
and what happens when a `Weak[T]` outlives its arena need a milestone document first.

Worth noting that Quince's collector does not *need* weak references the way a reference
counted runtime does — DESIGN.md's whole argument for arena-and-handles is that cycles are
just integers pointing at each other and a mark phase collects them. `Weak[T]` is a caching
and observer-list convenience here, not a correctness requirement, which is an argument for
scheduling it on its own merits rather than as a rider on a GC rework.

- `Weak[T]` holds a non-owning handle to a heap object.
- **Cycle Prevention**: Resolving `weak_ref.upgrade(): Option[T]` returns `nil` when the referenced object has been collected by GC.

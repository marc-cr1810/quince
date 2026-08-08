# Phase 6B: Hardware SIMD Vector Acceleration Architecture

## Executive Summary

Phase 6B introduces first-class **128-Bit Hardware SIMD Vector Types (`float32x4`, `int32x4`)**, emitting native CPU vector instructions (x86_64 SSE2/AVX2, ARM NEON).

**The types themselves are language surface owned by no milestone**, per
`BYTECODE_VM_DESIGN.md` §12: two built-in types means two `Value` variants, entries in `OPS`
for their arithmetic, a rule for how they interact with the numeric tower (`float32x4 + 1.0`
— broadcast, or refused?), and a decision about whether the element type is `f32` when the
language has no sized numeric types at all. That last one is the real blocker: sized integers
are in DESIGN.md's *Later*, and `int32x4` cannot be spelled honestly before them.

---

## 1. Native SIMD Types & Vector Operations

```quince
let v1 = float32x4(1.0, 2.0, 3.0, 4.0)
let v2 = float32x4(5.0, 6.0, 7.0, 8.0)
let result = v1 + v2 # Emits single CPU `addps` instruction!
```

- **Cranelift Codegen Mapping**: Maps directly to Cranelift `I32x4` and `F32x4` vector types, lowering to native hardware vector registers.

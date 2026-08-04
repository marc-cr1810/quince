# Quince v0.14: Compiler Utility Standard Library Modules (`text`, `collections`, `binary`)

## Executive Summary

Quince v0.14 delivers high-performance compiler data structures and binary buffer utilities needed for the **Phase 5 Self-Hosting Bootstrapping** milestone.

---

## 1. The `text` Module (`import text`)

* `StringBuilder`: Amortized $O(1)$ string concatenation buffer for native machine code and AST generation.
* `char` Class Methods: `char.is_alphanumeric()`, `char.is_whitespace()`, `char.is_digit()`.
* Terminal Formatting: ANSI color styling methods (`text.red()`, `text.bold()`, `text.reset()`) for compiler diagnostic rendering.

---

## 2. The `collections` Module (`import collections`)

* **`Interner` (String Symbol Table)**: $O(1)$ symbol interning (`interner.intern("name"): SymbolId`) allowing token and AST node comparisons to operate on integer IDs instead of heap string comparisons.
* **`IndexMap[K, V]`**: Insertion-ordered hash map preserving symbol declaration ordering.
* **`BitSet`**: Compact bitvector structure for liveness analysis and basic block control-flow graph reachability.

---

## 3. The `binary` Module (`import binary`)

* **`ByteBuffer`**: Low-level binary emitter supporting little-endian/big-endian byte writing (`buf.write_u8()`, `buf.write_u32_le()`, `buf.write_f64_le()`). Used for `.qnc` bytecode binary serialization and machine code codegen emission.

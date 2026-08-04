# Phase 1A: Bytecode IR Emitter, Tooling & Caching Architecture

## Executive Summary

Phase 1A implements the AST-to-Bytecode Intermediate Representation (IR) compiler (`src/compiler/`), the serialized `.qnc` binary format, the disassembler (`quince --dump bytecode`), and the co-located `.qn_cache/` caching engine.

---

## 1. Bytecode IR Compilation Pipeline

The IR compiler translates parsed AST statement and expression nodes into a linear stream of compact `OpCode` byte sequences:

```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    pub spans: Vec<Span>,
}
```

- **Span Preservation**: Every opcode emitted records its exact byte-offset `Span` in `chunk.spans`, enabling runtime exception unwinding and source diagnostics without carrying AST pointers.

---

## 2. Disassembler Tooling (`quince --dump bytecode`)

The CLI disassembler renders disassembly of compiled functions and modules:

```text
== Disassembly: main ==
0000    12 OpCode::Constant      0 ('Hello World')
0003    │  OpCode::SetLocal       0
0005    13 OpCode::GetLocal       0
0007    │  OpCode::Call           1
0009    │  OpCode::Return
```

---

## 3. Serialized Binary `.qnc` & Co-Located `.qn_cache/`

- **Header Specification**:
  - `[0..4]`: Magic bytes (`QNBC`)
  - `[4..36]`: Source SHA-256 digest
  - `[36..44]`: Source modification timestamp (`mtime`)
  - `[44..]`: Serialized constant pool, opcode stream, and span table.
- **Cache Invalidation**: On `import`, Quince checks `.qn_cache/<module>.qnc`. If the source SHA-256 or `mtime` matches, pre-compiled bytecode loads in **< 0.5ms**.

# Phase 1B: Cranelift JIT Core Execution Engine

## Executive Summary

Phase 1B implements the **Cranelift JIT Core Engine** (`src/codegen/cranelift/`), lowering Bytecode IR basic blocks directly into native RAM machine instructions (`x86_64` / `AArch64`) via `cranelift-jit`.

---

## 1. Cranelift IR Translation Architecture

The codegen module translates `Chunk` opcodes into Cranelift IR (`cranelift_codegen::ir::Function`):

- **Virtual Register Allocation**: Operands map directly to Cranelift SSA variables (`cranelift_frontend::FunctionBuilder`).
- **Callframe Stack Layout**: Native C-ABI stack frame layout allocated per function invocation.
- **Reference Parameters (`ref`)**: `PassRef` emits pointer offsets into caller stack frames; `StoreRef` writes validated values back into caller variables.

---

## 2. Unwinding & Resource Deinitialization (`op deinit`)

When a function scope exits or throws an exception:
1. Lowers scope cleanup guards using `op deinit`.
2. Emits cleanup blocks before stack frame teardown.
3. Restores host register states cleanly upon uncaught exceptions.

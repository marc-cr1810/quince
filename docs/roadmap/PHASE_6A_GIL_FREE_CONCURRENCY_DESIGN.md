# Phase 6A: GIL-Free Multi-Threaded Concurrency Architecture

## Executive Summary

Phase 6A introduces **GIL-Free Multi-Core Parallelism**, **Structured Concurrency (`parallel { spawn ... }`)**, **Coroutines (`async`/`await`)**, and **`shared` RWLock Objects**.

**This phase is mostly a language milestone wearing a runtime phase's clothes.** `async`,
`await`, `parallel`, `spawn`, `spawn move`, and `shared` are six new keywords, a colouring
rule for which functions may await, a cancellation model, and a decision about what a
`shared` object's field access means — none of which is a codegen question, and all of which
`BYTECODE_VM_DESIGN.md` §12 lists as unscheduled. The runtime work here (per-thread arenas,
work stealing, safepoint polls) is real and is this phase's; the surface above it needs a
`v0.x` document first, and DESIGN.md's *Later* says so. What follows is the runtime sketch,
not that design.

---

## 1. Concurrency Primitives

- **`async` / `await`**: Coroutine suspension yielding state across non-blocking I/O tasks.
- **`parallel { spawn ... }`**: Structured concurrency scope waiting for all spawned tasks before exiting.
- **`shared` Objects**: Wraps class field accesses in automated background reader-writer locks (`SharedMutex`).
- **`spawn move`**: Zero-copy transfer of heap allocation ownership to worker threads.

---

## 2. Lock-Free Per-Thread Local Arenas

- **No Global Interpreter Lock (GIL)**: Threads execute native machine code independently.
- **Per-Thread Local Arenas**: Garbage collection triggers per-thread without global Stop-The-World pauses.
- **Global Const Arena**: Atomic ref-counted arena for deeply frozen `const` heap structures.

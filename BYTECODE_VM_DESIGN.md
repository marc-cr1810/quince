# Quince — Bytecode VM & Native Compilation Architecture

This design document outlines the technical proposals, architectural specifications, and execution plan for transitioning **Quince** from its current AST Tree-Walk interpreter to a high-performance **Bytecode Virtual Machine (VM)**, along with a native **Ahead-Of-Time (AOT) LLVM Compilation** pipeline, **Native Dynamic Modules**, and a **Self-Hosting Bootstrapping** roadmap.

---

## 1. Executive Summary

Quince currently executes programs via an AST tree-walking interpreter (`src/interp/`). While ergonomic and ideal for language bootstrapping, tree-walking incurs significant host stack recursion overhead, CPU cache misses, and restricted garbage collection safe-points.

The proposed architecture introduces:
1. **A Bytecode Virtual Machine**: Converts AST nodes into a compact, linear instruction stream executed on a fast, stack-based VM loop.
2. **Advanced Runtime Optimizations**: Implements NaN-tagging (8-byte values), Morphic Inline Caching (IC), string index caching, and opcode specialization via static type inference.
3. **Native Compilation Pipeline**: Uses bytecode as a 1-to-1 Intermediate Representation (IR) bridge to compile Quince programs into standalone native binaries via LLVM/Cranelift.
4. **Native Dynamic Modules (`.qnx`)**: Allows performance-critical Quince scripts to be compiled into native dynamic libraries and imported seamlessly without changing client code.
5. **Self-Hosting Roadmap**: Enables the Quince compiler, toolchain, and Language Server (LSP) to be written natively in Quince.

---

## 2. AST Tree-Walker vs. Bytecode Virtual Machine

### 2.1 Current AST Tree-Walker Bottlenecks
* **Host Stack Recursion**: Every statement and expression calls recursive Rust methods (`eval`, `exec`). Guarding against stack overflow requires explicit thread stack reservation (`STACK_SIZE = 16 MiB`) and stack-pointer depth checks (`MAX_DEPTH = 250`, `here()`).
* **GC Safe-Point Restrictions**: Temporary values during sub-expression evaluation (e.g., the left operand of `a + b` while evaluating `b`) live in unmanaged Rust stack locals. Consequently, garbage collection can only safely trigger between top-level statements (`collect_if_needed`).
* **24-Byte Value Overhead**: `Value` is a 24-byte Rust `enum`, reducing CPU L1/L2 cache line efficiency during stack and collection operations.

### 2.2 Bytecode VM Advantages
* **Linear Execution & Cache Friendliness**: AST nodes are compiled into flat byte arrays (`Vec<u8>`). CPU instruction caches hit sequentially instead of pointer-chasing heap objects.
* **Explicit VM Value Stack**: Temporary values for sub-expression evaluation live on an explicit array (`Vec<Value>`) owned by the VM.
* **Unrestricted Garbage Collection**: Because all live evaluation values reside predictably on `vm.stack` and call frames, GC allocations can trigger safely at *any instruction step* without needing special `eval_seq` temporary rooting arrays.
* **Zero Native Stack Depth Limits**: Native thread stack limits are eliminated; recursion is bounded strictly by allocated VM stack memory.

---

## 3. Bytecode Virtual Machine Specification

### 3.1 Instruction Set Architecture (ISA / Opcodes)
Quince defines a byte-enum set of opcodes matching its language semantics:

```rust
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum OpCode {
    // Stack & Constant Manipulation
    Constant,   // Load constant by 16-bit index: [Constant, idx_hi, idx_lo]
    Nil, True, False,
    Pop, Dup,

    // Operators & Math
    Add, Sub, Mul, Div, Mod, Neg, Not,
    Equal, Less, Greater,

    // Specialized Numeric Opcodes (Emitted via Type Inference)
    AddInt, SubInt, MulInt, DivInt,
    AddFloat, SubFloat, MulFloat, DivFloat,

    // Fast Variable Access (using Quince Resolver Slots)
    GetLocal, SetLocal,     // Local slot index: [GetLocal, slot_u8]
    GetUpvalue, SetUpvalue, // Lexically captured upvalue index: [GetUpvalue, idx_u8]
    GetGlobal, SetGlobal,   // Top-level / module scope lookups: [GetGlobal, name_idx_u16]

    // Control Flow
    Jump, JumpIfFalse, Loop, // Jump with 16-bit relative offset

    // Calls & Functions
    Call, Return, Closure, Yield, Await,

    // Object-Oriented Operations & Polymorphic Dispatch
    GetProperty, SetProperty, GetSuper, Invoke, Class, Extend,

    // Collections & Modules
    BuildList, BuildDict, Import,
}
```

### 3.2 Chunks, Operand Encoding, and Source Spans
Instead of holding `Rc<FnDecl>` AST pointers inside `Function` objects, compiled functions hold bytecode chunks. Line and character spans are mapped in a parallel array to maintain Quince's exact diagnostic reporting:

```rust
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    pub spans: Vec<Span>, // Preserves precise location for Quince diagnostic reports
}

impl Chunk {
    pub fn write_op(&mut self, op: OpCode, span: Span) {
        self.code.push(op as u8);
        self.spans.push(span);
    }

    pub fn write_u16(&mut self, value: u16, span: Span) {
        self.code.push((value >> 8) as u8);
        self.code.push((value & 0xFF) as u8);
        self.spans.push(span);
        self.spans.push(span);
    }
}
```

### 3.3 Example AST-to-Bytecode Compilation Traces

#### Example A: `let sum = a + b`
```
0000 | GetLocal      slot 1 (a)
0002 | GetLocal      slot 2 (b)
0004 | Add           (or AddInt if statically typed)
0005 | SetLocal      slot 3 (sum)
```

#### Example B: `while condition { body }`
```
0000 | <condition evaluation>
0005 | JumpIfFalse   offset 0x0010  ──┐ (Patched after body compilation)
0008 | <body>                         │
0015 | Loop          offset 0x0017  ◄─┼── Jumps back to condition (0000)
0018 | Pop                          ◄─┘
```

### 3.4 Call Frames and Execution State
The VM evaluation loop replaces `Interp::eval`:

```rust
pub struct CallFrame {
    pub function: ObjId, // Points to CompiledFunction in Heap
    pub ip: usize,       // Instruction Pointer
    pub slots: usize,    // Index in VM value stack where this frame's locals begin
}

pub struct VM {
    pub heap: Heap,
    pub frames: Vec<CallFrame>,
    pub stack: Vec<Value>,
    pub open_upvalues: Option<ObjId>, // Linked list of open upvalues sorted by stack index
    pub globals: ObjId,
}
```

### 3.5 Upvalue Closing Algorithm & Lexical Scoping
To support nested closures without heap-allocating every local variable scope:

1. **Open Upvalue**: While a function frame is active, an upvalue references an index directly on `vm.stack`.
2. **Linked List Tracking**: All open upvalues are kept in a single-linked list on `VM` sorted descending by stack index.
3. **Closing Upvalues on Return**: When a stack frame exits (`OpCode::Return`), `vm.close_upvalues(frame.slots)` iterates through open upvalues pointing to stack positions $\ge$ `frame.slots`, copies their values onto the heap (`Object::Upvalue`), and updates references to point to the heap object.

### 3.6 Serialization Format (`.qnc`), Co-Located Caching & Disassembler (`--dump bytecode`)
To support pre-compiled bytecode caching, sub-millisecond startup, and debugging, Quince defines a binary chunk specification and caching policy:

* **File Header Specification**:
  * **Magic Bytes**: `QNBC` (`[0x51, 0x4E, 0x42, 0x43]`) + 2-byte Major/Minor Version.
  * **Source Validation Header**: SHA-256 hash of the source `.qn` text + 64-bit source modification timestamp (`mtime`) for instant cache invalidation checks.
  * **Constant Pool**: Table of serialized literal values (`Int`, `Float`, `Str`, `Function`).
  * **Chunk Code Stream**: Raw opcode bytes with line-span compression.

* **Co-Located Cache Storage (`.qn_cache/`)**:
  * To avoid filename collisions between different projects and eliminate central directory cache bloat, bytecode cache files are stored co-located in a `.qn_cache/` folder beside the source script:
    ```
    my_project/
    ├── main.qn
    ├── utils.qn
    └── .qn_cache/
        ├── main.qnc
        └── utils.qnc
    ```
  * **Automatic Git Isolation**: Upon creating a `.qn_cache/` directory, Quince automatically generates a `.qn_cache/.gitignore` file containing `*` to keep workspace git status clean.
  * **Cache Validation Lifecycle**: On `quince main.qn`, the runtime checks `.qn_cache/main.qnc`. If the file exists and its SHA-256/mtime header matches `main.qn`, the VM loads bytecode in **< 0.5ms**, bypassing Lexer, Parser, and Resolver passes. If modified, it re-compiles and overwrites the `.qnc` cache seamlessly.

* **CLI Tooling**: `quince --dump bytecode script.qn` invokes the disassembler, printing opcode offsets, instruction names, constant parameters, and source file line references for compiler development.

### 3.7 Reference Parameter Calling Conventions (`ref`, `ref x: const T`, `final ref`)
To support zero-overhead pass-by-reference and caller slot mutation without heap-allocating local variable scopes:

1. **Stack Reference Opcodes (`OpCode::PassRef`)**:
   * When a call site passes a variable by reference (`foo(ref y)`), the compiler emits `OpCode::PassRef <slot_offset>`.
   * Instead of copying `y`'s value onto `vm.stack`, the VM pushes an indirect reference cell pointing directly to `vm.stack[caller_frame + slot_offset]`.

2. **Callee Slot Writing (`OpCode::StoreRef`)**:
   * Re-assigning a `ref` parameter inside the callee body (`x = new_val`) executes `OpCode::StoreRef`.
   * `StoreRef` validates `new_val` against `x`'s type annotation (`coerced()`) and writes the updated value directly into the caller's stack slot index.

3. **Reference Modifiers and Contracts**:
   * **`ref x: T`** (*Mutable Reference*): Callee can mutate object contents and re-assign the caller's variable slot.
   * **`ref x: const T` / `const ref`** (*Read-Only Reference*): Passes a zero-copy stack reference; object mutation and variable slot re-assignment are refused with a `ConstError` / `FrozenError`.
   * **`final ref x: T`** (*Binding-Locked Reference*): Callee can mutate object contents (`x.push()`), but variable slot re-assignment (`x = new_val`) is refused at compile-time/runtime.

### 3.8 Async / Await & Concurrency Architecture (`parallel { spawn ... }`)
In a tree-walking interpreter, suspending an asynchronous function call stack requires complex continuation passing. In a Bytecode VM, `async`/`await` coroutines and multi-core task scheduling become remarkably lightweight:

```rust
pub struct Task {
    pub frame: CallFrame,
    pub stack_snapshot: Vec<Value>,
    pub state: TaskState, // Pending, Resolved(Value), Rejected(QuinceError)
}
```

* **Suspending (`OpCode::Await`)**: If an awaited promise is pending, the VM pops the active `CallFrame` and saves its stack slice onto a heap-allocated `Task` object.
* **Resuming**: When the I/O event loop signals completion, the `Task` frame is pushed back onto `vm.frames` and execution resumes seamlessly from `frame.ip`.

#### 1. Structured Concurrency (`parallel { spawn ... }`)
Quince enforces structured concurrency to prevent background task leaks and orphaned threads:

```quince
# Concurrent execution block: waits for spawned tasks to finish before exiting!
parallel {
    spawn fetch_user_data(user_id)
    spawn fetch_user_orders(user_id)
}
```

* **Lifetime Guarantees**: Execution blocks at the end of `parallel { ... }` until all child tasks finish. If a task raises an unhandled error, sibling tasks in the block are cancelled cleanly and the exception propagates to the parent scope.

#### 2. Task Handles, `.join()`, `.detach()`, and Application Shutdown
Outside `parallel` blocks, `spawn` returns a `Task` handle for explicit lifecycle management:

```quince
# Spawning returns a Task handle
final bg_task = spawn logger_daemon()

# Join later during shutdown phase
let exit_code = await bg_task.join()

# Or fire-and-forget:
spawn metrics_collector().detach()
```

* **Detached Exception Safety**: Unhandled exceptions in `.detach()`ed tasks route to a global handler (`runtime.on_unhandled_error`), printing a full diagnostic backtrace to `stderr` rather than failing silently.
* **Graceful Exit**: On main application shutdown, the runtime automatically waits for active non-detached `Task` handles to finish cleanly.

#### 3. Automatic Background Reader-Writer Locking (`shared` Objects)
To eliminate manual lock boilerplate (`Mutex.lock(...)`), objects marked as `shared` automatically use background `SharedMutex` (RWLock) synchronization:

```quince
final cache = shared Cache()

# Automatic Shared Read Lock: 100+ threads read concurrently in parallel with ZERO blocking!
let user = cache.get("user_100")

# Automatic Exclusive Write Lock: Pauses readers briefly for the duration of the mutation
cache.set("user_101", "Marc")
```

#### 4. Zero-Copy Move Semantics (`spawn move`)
To pass large mutable data structures into another thread with **zero CPU copy overhead**, Quince supports `move` ownership transfer:

```quince
let large_dataset = load_big_file()

# `move` transfers ownership of `large_dataset` to the task with 0 memory copying
final task = spawn move process_data(large_dataset)
```

#### 5. Dual-Arena Memory & GIL-Free Multi-Core Work Stealing
* **No Global Interpreter Lock (GIL)**: Task execution is distributed across a pool of OS worker threads without a GIL.
* **Dual-Arena Heap System**:
  * **Per-Thread Local Arenas**: Allocates short-lived, thread-local objects. Garbage collection runs independently on each thread with zero global "Stop-The-World" pauses.
  * **Global Const Arena**: Deeply frozen `const` objects are stored in a global, atomic-refcounted arena that safely outlives individual worker thread exits.

#### 6. Safe Thread & Resource Cleanup
Quince guarantees clean unwinding and resource reclamation when a thread exits (normally, on exception, or during shutdown):

1. **Automatic Resource Destruction (`op drop`)**: On thread unwind, the VM walks the call frames from top to bottom and invokes `op drop` on local file descriptors, sockets, and connections in reverse allocation order.
2. **Automatic Lock Release**: If a dying thread holds a `shared` object lock, the unwinder releases the lock automatically to prevent deadlocks or poisoned mutexes.
3. **`O(1)` Local Arena Reclaim**: When a worker thread exits, its entire local arena heap is reclaimed in a single `O(1)` memory deallocation with zero memory leaks.
4. **Channel Disconnect Signaling**: When a task holding a `Channel` endpoint exits, connected receivers receive a `ChannelError::Closed` signal immediately.

#### 7. Infinite Loop Interruption & `quince::ThreadTimeoutError`
If a task is stuck in a loop during application shutdown:

1. **Safepoint Polls (`quince_gc_poll`)**: Loop headers automatically check `runtime.is_cancelling()`. When set, the VM unwinds stack frames safely.
2. **Timeout Safeguard & Quince Error Diagnostic**: If a task remains blocked past the shutdown grace period (e.g. in a blocking C-FFI call), the runtime forces exit and prints a diagnostic matching Quince's exact `src/error/render.rs` specification:

```text
Error: quince::ThreadTimeoutError

 × TaskId(12) failed to exit gracefully within 3.0s timeout
  ╭─[scripts/worker.qn:45:5]
45 │     while true {
  ·     ────────────
  ╰────
  help: ensure long-running loops include `await` points or check `runtime.is_cancelled()`
```

### 3.8 Stack Trace Reconstruction & Exception Unwinding
When a `QuinceError` is thrown, the VM reconstructs rich diagnostic backtraces without relying on native host symbol tables:

1. The VM walks `vm.frames` from top to bottom.
2. For each frame, it reads `frame.ip` and queries `function.chunk.spans[frame.ip]`.
3. Maps exact source code lines, module filenames, and carets directly onto rendered diagnostic reports (`QuinceError::report`).

---

## 4. Performance Optimization Roadmap

### 4.1 NaN-Tagging / Value Packing (8-Byte Values)
* **Goal**: Reduce `Value` size from 24 bytes to 8 bytes.
* **IEEE 754 Bit Layout**:
  * Double-precision floats use standard IEEE 754 representations.
  * Quiet NaNs (`0x7FF8000000000000`) contain 51 unused payload bits.
  * Bools, `Nil`, Ints, and heap pointers (`ObjId`) are encoded into these unused bits:
    * `0x7FF8000000000000` $\rightarrow$ `Nil`
    * `0x7FF8000000000001` / `2` $\rightarrow$ `False` / `True`
    * `0x7FFC0000xxxxxxxx` $\rightarrow$ 32-bit `ObjId` handle
    * `0x7FFF0000xxxxxxxx` $\rightarrow$ Signed 32-bit Integer

```rust
// 8-byte NaN-Tagged Value representation
#[derive(Clone, Copy, PartialEq)]
pub struct Value(u64);
```

* **Impact**: Triples CPU cache density for value stacks, list payloads, and dict values, reducing peak heap memory usage by 40%–60%.

### 4.2 Morphic Inline Caching (IC) for Properties & Methods
* **Goal**: Convert $O(N)$ hash/parent walk property accesses into $O(1)$ constant time lookups.
* **Mechanism**:
  1. `OpCode::GetProperty <name_idx>` allocates a 2-slot inline cache entry in the bytecode stream: `[GetProperty, name_idx_u16, cached_class_u32, field_offset_u16]`.
  2. On execution, the VM compares the receiver instance's `class` handle against `cached_class`.
  3. **Morphic Cache Hit**: Accesses the instance field vector directly at `field_offset` in $O(1)$ time.
  4. **Cache Miss**: Performs full hash table / parent class lookup, updates `cached_class` and `field_offset`, and continues.

### 4.3 String Optimizations (SSO & Index Offset Caching)
* **Index Offset Caching**: Caches UTF-8 character boundary offsets to eliminate $O(N^2)$ string indexing bottlenecks in loops (`while i < len(s) { s[i] }`).
* **Small String Optimization (SSO) & Interning**: Encodes short strings (up to 6 bytes) directly inline within 8-byte NaN-tagged values. Interns identifier strings and dict keys to enable pointer-identity equality checks.

### 4.4 Opcode Specialization via Type Inference
* Leverages Quince's static type inference pass (`inference.rs`) to emit monomorphic opcodes (`OP_ADD_INT`, `OP_ADD_FLOAT`) when operand types are statically known, bypassing dynamic method and class slot resolution.

---

## 5. Performance Expectations & Benchmarks

On recursive and numerical CPU-bound benchmarks (e.g. `fib(25)`):

| Metric / Benchmark | Today (Quince AST v0.6) | Quince + Bytecode VM (Phase 1) | Quince + Full Suite (VM + NaN-Tag + IC) | CPython 3.11 / 3.12 | Lua 5.4 (Interpreter) |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Execution Architecture** | AST Tree-Walker | Stack Bytecode VM | Specialized Bytecode VM | Adaptive Bytecode VM | Register Bytecode VM |
| **`fib(25)` Time** | `~0.10s` (Baseline 1.0x) | `~0.025s` (**~4x faster**) | **`~0.010s – 0.012s`** (**~8-10x faster**) | `~0.030s` (~3.3x faster than Quince today) | `~0.008s` (~12x faster) |
| **Memory per Value** | 24 bytes | 24 bytes | **8 bytes** | 8 bytes (`PyObject*`) | 8 – 16 bytes |
| **Function Call Overhead** | High (Host stack recursion) | Low (VM Frame Stack) | **Very Low** | Low | Extremely Low |
| **Property Access (`x.y`)** | $O(N)$ Hash/Parent walk | $O(1)$ Hash table | **$O(1)$ Direct Array Index (IC)** | Specialized Inline Cache | $O(1)$ Table Lookup |

---

## 6. Compilation & Native Binary Architecture

Bytecode provides an ideal **Intermediate Representation (IR)** bridge to compile Quince code to native machine code via **Cranelift** (pure-Rust JIT/AOT engine) or **LLVM**.

```
                           ┌───────────────────────────┐
                           │    quince run script.qn   │ (Instant Dev / REPL)
                           └─────────────┬─────────────┘
                                         │
                              Compiles to Bytecode
                                         │
       ┌─────────────────────────────────┼─────────────────────────────────┐
       ▼                                 ▼                                 ▼
┌───────────────────────────┐  ┌───────────────────────────┐  ┌───────────────────────────┐
│ Option 1: Bundled Binary  │  │ Option 2: Cranelift AOT   │  │ Option 3: Cranelift JIT   │
│   `quince build --bundle` │  │  `quince build --aot`     │  │  (Runtime Acceleration)   │
├───────────────────────────┤  ├───────────────────────────┤  ├───────────────────────────┤
│ • Packs VM Runtime +      │  │ • Compiles Bytecode to    │  │ • JIT-compiles hot loops  │
│   Bytecode Chunks into    │  │   native ELF/Mach-O/PE    │  │   & functions in RAM      │
│   a single executable.    │  │   object files via        │   directly while VM runs. │
│ • Sub-second build time.  │  │   `cranelift-object`.     │  │ • Pure Rust, zero system  │
│ • Zero external tools.    │  │ • Builds standalone binaries│   toolchain dependencies. │
│                           │  │   & `.qnx` shared modules.│                           │
└───────────────────────────┘  └───────────────────────────┘  └───────────────────────────┘
```

### 6.1 Option 1: Self-Executing Single Binary Bundling (`--bundle`)
* Embeds the pre-compiled Bytecode `Chunk` alongside the lightweight VM runtime into a single standalone executable.
* **Build Time**: Sub-second (milliseconds). Requires no local C/LLVM toolchain installation.

### 6.2 Option 2: Native AOT Compilation & Dynamic Shared Libraries (`cranelift-object`)
* **Bytecode-to-Cranelift Translation**: Operates as a compiler pass over `Chunk` opcodes:
  * Bytecode opcodes are translated into Cranelift IR (`clif`) basic blocks.
  * Local variables and VM stack slots map to Cranelift virtual registers or `Variable` abstractions.
* **Native Binary & Shared Module (`.qnx`) Construction**:
  * `cranelift-object` emits native object files (`.o` / `.obj`) formatted as ELF, Mach-O, or PE.
  * The driver passes the object file to the system linker (`cc`, `clang`, or `lld`) to output:
    * **Executable Binaries**: `quince build main.qn -o main`
    * **Shared Dynamic Modules**: `quince build --lib matrix.qn -o matrix.qnx` (or `.so`/`.dylib`)
* **Runtime C-ABI Interface**: Compiled Cranelift machine code calls back into Quince's C-ABI runtime helpers:

```c
// C-ABI functions exported by Quince Runtime Library
extern "C" {
    pub fn quince_alloc_list(heap: *mut Heap, capacity: usize) -> u64;
    pub fn quince_call_method(interp: *mut Interp, receiver: u64, name: *const i8, args: *const u64, count: usize) -> u64;
    pub fn quince_gc_poll(interp: *mut Interp);
    pub fn quince_raise_runtime_error(interp: *mut Interp, span_id: u32, kind: ErrorKind);
}
```

### 6.3 Option 3: Cranelift JIT Compilation (`cranelift-jit`)
* **In-Memory JIT Execution**: Functions executed frequently in the VM (hot functions) are passed to `cranelift-jit` at runtime, compiled to native machine code in RAM in milliseconds, and invoked via function pointers directly from the VM loop.
* **100% Pure Rust**: Requires zero external C++ LLVM installations or system dev kits—`cargo add cranelift-codegen` handles codegen out of the box.

### 6.4 Source Location Propagation & Identical Error Diagnostics
Cranelift-compiled machine code (both JIT and AOT) preserves 100% of Quince's rich terminal diagnostic reports:

1. **Source Location Mapping (`SourceLoc`)**:
   During bytecode translation, every emitted Cranelift IR instruction is tagged with a `SourceLoc` corresponding to the opcode's `Span`:
   ```rust
   // Tag Cranelift instruction with Quince Span index
   builder.set_srcloc(cranelift_codegen::ir::SourceLoc::new(span_id));
   ```
2. **Runtime Error Traps & Guards**:
   When a runtime exception occurs (e.g. division by zero, invalid subscript, type error), the generated machine code executes a guard check and calls `quince_raise_runtime_error`:
   ```rust
   #[no_mangle]
   pub extern "C" fn quince_raise_runtime_error(interp: *mut Interp, span_id: u32, kind: ErrorKind) {
       let span = interp.get_span(span_id);
       let err = QuinceError::new(kind, span);
       err.report(&interp.source_code); // Route straight to src/error/render.rs!
   }
   ```
### 6.5 Self-Hosting Codegen & Hybrid Fallback Architecture

To balance build speed, self-containment, and universal platform support, self-hosted Quince uses a **Hybrid Codegen Dispatcher**:

```
                       ┌─────────────────────────────────────┐
                       │  Quince Codegen Dispatcher (`.qn`)  │
                       └──────────────────┬──────────────────┘
                                          │
                  Is Target Architecture Natively Supported?
                                          │
                  ┌───────────────────────┴───────────────────────┐
                  YES                                             NO
                  ▼                                               ▼
┌───────────────────────────────────┐           ┌───────────────────────────────────┐
│ Primary Internal Quince Backend   │           │ Cranelift Fallback Engine (FFI)   │
│ • Ultra-fast zero-dependency      │           │ • Seamless fallback via           │
│   machine code emission for       │           │   `libcranelift` wrapper.         │
│   supported targets (x86_64/ARM64)│           │ • Universal target coverage       │
│ • Sub-millisecond build speeds.   │           │   (Cortex-M, RISC-V, etc.).       │
└───────────────────────────────────┘           └───────────────────────────────────┘
```

1. **Phase A (Universal Cranelift FFI Wrapper)**:
   * Self-hosted Quince binds to `libcranelift` via `import ffi`.
   * `quince/codegen/cranelift.qn` translates Quince Bytecode IR into Cranelift C-API instructions.
   * **Advantage**: Instant production-grade AOT/JIT cross-compilation for x86_64, AArch64, ARM Cortex-M, and RISC-V with minimal code.
2. **Phase B (Hybrid Codegen & Cranelift Fallback)**:
   * Internal native encoders are implemented incrementally in Quince (starting with tier-1 targets like `x86_64` and `aarch64`).
   * When building for supported tier-1 targets, Quince uses its native internal encoder for sub-millisecond, zero-dependency builds.
   * When cross-compiling to specialized, embedded, or niche architectures (e.g. ARM Cortex-M microcontrollers, RISC-V), the dispatcher **automatically falls back to Cranelift**, guaranteeing 100% target coverage on any system.
3. **Phase C (100% Native Pure Quince Codegen)**:
   * All machine code instruction encoders (x86_64, AArch64, ARM Cortex-M, RISC-V) and object file formatters (ELF, Mach-O, PE) are written 100% in Quince.
   * The compiler requires zero external dynamic libraries, C toolchains, or third-party codegen dependencies for any target platform.

---

## 7. Native Dynamic Modules (`.qnx`), Auto-Import Resolution & C FFI

Quince scripts can interact with compiled native binaries as ordinary modules without modifying importing code, while C, C++, Rust, and Python can call Quince `.qnx` modules directly.

### 7.1 Drop-In Acceleration & Auto-Import Resolution

The Quince module loader automatically resolves module imports (`import matrix` or `from matrix import dot_product`), selecting the fastest available valid binary without requiring any changes to user code.

#### Resolution Algorithm
When resolving `import matrix`:
1. **Binary Check**: Look for `matrix.qnx` (or `.so`/`.dll`/`.dylib`) in the module search path.
2. **ABI & Target OS Validation**: Read the `.qnx` binary header (`QNX` magic bytes + ABI version hash + target OS/arch). If the binary was built for a different architecture or incompatible runtime version, safely ignore it and fall back to source.
3. **Timestamp / Stale Binary Check**: Compare `matrix.qnx` modification timestamp (`mtime`) against `matrix.qn`.
   * **Fresh Binary**: If `matrix.qnx` is newer than `matrix.qn`, load the native shared library instantly via `dlopen`/`libloading` for full C-speed execution.
   * **Stale Binary**: If `matrix.qn` was edited more recently than `matrix.qnx`, automatically fall back to executing `matrix.qn` in the Bytecode VM (or trigger background re-compilation) so edits take effect immediately.

#### Syntax Parity Across `.qn` and `.qnx`
All import statement formats work identically regardless of whether the target module is an interpreted `.qn` script or a compiled `.qnx` native library:

```quince
# Form 1: Module namespace import
import matrix
let res = matrix.dot_product(v1, v2)

# Form 2: Selective symbol import
from matrix import dot_product, transpose
let res = dot_product(v1, v2)

# Form 3: Wildcard import
from matrix import *
```

### 7.2 Safety & Process Isolation Guarantees
Loading and executing `.qnx` native dynamic modules maintains full runtime safety:
* **Memory Safety (No Segfaults)**: Quince heap objects are referenced by index-based handles (`ObjId`). Native compiled instructions perform bounds checks against the arena `Heap`, preventing raw pointer corruption or segfaults.
* **Header Hash Verification**: Invalid, corrupt, or incompatible binary files are rejected safely at load time.
* **Type & Arity Enforcement**: Parameter types and arities are verified at the boundary; invalid arguments produce a clean `TypeError` diagnostic rather than crashing the process.

### 7.3 Bi-Directional Interoperability (C / Rust / Python calling Quince `.qnx`)

Because `.qnx` files are standard native shared libraries exporting standard C-ABI (`extern "C"`) functions:

#### 1. Calling Quince `.qnx` from C / C++
```c
#include <stdio.h>
#include <dlfcn.h>

typedef int64_t (*add_fn)(int64_t, int64_t);

int main() {
    void* handle = dlopen("./math.qnx", RTLD_LAZY);
    add_fn quince_add = (add_fn)dlsym(handle, "add");
    printf("Result: %ld\n", quince_add(10, 20)); // Outputs: 30
    return 0;
}
```

#### 2. Calling Quince `.qnx` from Rust
```rust
use libloading::{Library, Symbol};

fn main() {
    unsafe {
        let lib = Library::new("./math.qnx").unwrap();
        let add: Symbol<unsafe extern "C" fn(i64, i64) -> i64> = lib.get(b"add").unwrap();
        println!("Result: {}", add(10, 20));
    }
}
```

#### 3. Calling Quince `.qnx` from Python
```python
import ctypes
lib = ctypes.CDLL("./math.qnx")
lib.add.argtypes = [ctypes.c_int64, ctypes.c_int64]
lib.add.restype = ctypes.c_int64
print(lib.add(10, 20)) # 30
```

### 7.4 Foreign Function Interface (`import ffi`)
In addition to Quince `.qnx` dynamic modules, Quince scripts can load external C libraries (`libcurl`, `sqlite3`, `raylib`) dynamically without writing C wrappers:

```quince
# Example: Direct C FFI in Quince
import ffi

final lib = ffi.load("sqlite3")
final open = lib.bind("sqlite3_open", [string, pointer], int)
```

### 7.5 Python Ecosystem Interoperability (`import py:module`)
Quince provides native, seamless interoperability with the entire Python library ecosystem (`numpy`, `pandas`, `requests`, `torch`, `matplotlib`) using `libpython` C-API marshaling:

```quince
# Import Python packages using the `py:` namespace prefix!
import py:numpy as np
import py:requests as requests
import py:torch as torch

# 1. Web request using Python's requests library
let res = requests.get("https://api.github.com")
print("Status Code:", res.status_code)

# 2. Vectorized computation using Python's NumPy
let matrix = np.array([10, 20, 30, 40])
print("Mean:", matrix.mean())

# 3. Machine learning using PyTorch
let tensor = torch.tensor([1.0, 2.0, 3.0])
print("CUDA Available?", torch.cuda.is_available())
```

#### Under the Hood Mechanics
1. **Dynamic CPython Embedding**: When the compiler sees `import py:module`, the runtime links to `libpython3` via C-FFI.
2. **Automatic Bidirectional Marshaling**:
   * Quince `int`, `float`, `string`, `bool` $\leftrightarrow$ Python `PyLong`, `PyFloat`, `PyUnicode`, `PyBool`.
   * Quince `list` and `dict` $\leftrightarrow$ Python `PyList` and `PyDict`.
3. **Zero Wrapper Boilerplate**: Developers do not need to write C bindings or wrapper layers; Python functions, attributes, and classes are invoked dynamically.

---

## 8. Self-Hosting & Bootstrapping Roadmap ("Quince in Quince")

With a Bytecode VM and LLVM AOT compilation, Quince can become a **fully self-hosted language**.

```
┌─────────────────────────────────────────────────────────────────┐
│ Stage 0: Rust Seed Compiler (`quince-rust`)                      │
│   • Parses and compiles Quince source code                       │
└────────────────────────────────┬────────────────────────────────┘
                                 │ Runs `compiler.qn`
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│ Stage 1: Quince Compiler written in Quince (`compiler.qn`)      │
│   • Lexer, Parser, Resolver, Bytecode Gen written in Quince    │
└────────────────────────────────┬────────────────────────────────┘
                                 │ Compiled via `quince-rust --aot`
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│ Stage 2: Native Self-Hosted Compiler (`quince-native`)          │
│   • Standalone native binary, zero Rust dependency              │
└─────────────────────────────────────────────────────────────────┘
```

### 8.1 Self-Hosted Compiler Architecture
The self-hosted compiler codebase in Quince would be structured cleanly:

```
compiler/
├── lexer.qn      # Tokenizer and span tracking
├── parser.qn     # Recursive descent + Pratt parser
├── resolver.qn   # Variable scoping & slot assignment
├── inference.qn  # Static type inference pass
├── bytecode.qn   # Opcode generation & chunk emitter
├── llvm.qn       # LLVM IR builder & AOT codegen
└── main.qn       # CLI driver
```

### 8.2 Metaprogramming, CTFE, and Macros
Self-hosting opens the door for **Compile-Time Function Execution (CTFE)** and **Macros**:
* Because the compiler is written in Quince, user macros can execute standard Quince functions *during compilation* to transform AST nodes before emitting bytecode.

```quince
# Macro proposal enabled by Self-Hosting
macro assert_eq(a, b) {
    return quote {
        if $a != $b {
            throw Error("Assertion failed: " + string($a) + " != " + string($b))
        }
    }
}
```

---

## 9. Implementation Phasing Roadmap

| Phase | Milestone Target | Key Deliverables |
| :--- | :--- | :--- |
| **Phase 1** | **Bytecode VM Core** | Bytecode emitter (`compiler.rs`), VM loop (`vm.rs`), `Chunk`, basic stack opcodes, lexical upvalues, disassembler (`--dump bytecode`). |
| **Phase 2** | **NaN-Tagging & IC** | 8-byte NaN-tagged `Value`, inline cache slots for `GetProperty` / `Invoke`, string index caching. |
| **Phase 3** | **Single-File Bundling** | `quince build --bundle` command, `.qnc` binary format, executable stub bundler. |
| **Phase 4** | **Async VM & LLVM AOT** | Bytecode task suspension (`Yield`/`Await`), LLVM IR lowering pass, C-ABI runtime exports, `.qnx` dynamic library loader. |
| **Phase 5** | **Self-Hosting (Stage 1 & 2)** | Port compiler pipeline to Quince (`compiler/*.qn`), bootstrap self-hosted native compiler binary, CTFE macros. |

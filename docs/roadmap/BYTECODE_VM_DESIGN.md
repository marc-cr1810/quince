# Quince — Bytecode IR & Cranelift Native Compilation Architecture

This design document outlines the comprehensive technical proposals, architectural specifications, and execution plan for transitioning **Quince** from its current AST Tree-Walk interpreter to a **Cranelift-Powered Native Execution Architecture** (using **Cranelift JIT** for instant in-memory native execution, **Cranelift AOT** for standalone native binaries, **Native Dynamic Modules (`.qnx`)**, and a **Self-Hosting Bootstrapping** roadmap).

---

## 1. Executive Summary

Quince currently executes programs via an AST tree-walking interpreter (`src/interp/`). While ergonomic and ideal for language bootstrapping, tree-walking incurs significant host stack recursion overhead, CPU cache misses, and restricted garbage collection safe-points.

Rather than implementing a slow, software-interpreted bytecode loop in Rust, **Quince uses Cranelift JIT (`cranelift-jit`) as its primary runtime execution engine**.

The proposed architecture introduces:
1. **Bytecode IR (Intermediate Representation)**: Converts AST nodes into a compact, linear byte-enum stream that acts as the Intermediate Representation (IR) bridge for code generation.
2. **Direct Cranelift JIT Execution (`cranelift-jit`)**: Compiles Bytecode IR basic blocks directly into native machine code in RAM (`x86_64` / `AArch64`) in a few milliseconds upon module load, executing functions via raw C-ABI function pointers at native CPU speeds.
3. **Native AOT Compilation Pipeline (`cranelift-object`)**: Uses the same IR to build standalone native executables (`quince build --aot`) and native dynamic shared libraries (`.qnx`) via system linkers.
4. **Advanced Runtime Optimizations**: Implements 8-byte NaN-tagging, Morphic Inline Caching (IC) for property access, string index caching, and opcode specialization via static type inference.
5. **Rich Language Operators**: First-class support for exponentiation (`**`), floor division (`//`), and all compound assignment operators (`+=`, `-=`, `*=`, `/=`, `//=`, `**=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`).
6. **Polymorphic Dispatch**: Support for single class inheritance and multiple interface implementation with $O(1)$ interface table (`itable`) vtable offsets.
7. **GIL-Free Multi-Threaded Concurrency**: Structured concurrency (`parallel`), async/await coroutines, `shared` RWLock objects, and dual-arena lock-free memory.
8. **Self-Hosting Roadmap**: Enables the Quince compiler, toolchain, and Language Server (LSP) to be written natively in Quince.

---

## 2. AST Tree-Walker vs. Cranelift Direct JIT Engine

### 2.1 Current AST Tree-Walker Bottlenecks
* **Host Stack Recursion**: Every statement and expression calls recursive Rust methods (`eval`, `exec`). Guarding against stack overflow requires explicit thread stack reservation (`STACK_SIZE = 16 MiB`) and stack-pointer depth checks (`MAX_DEPTH = 250`).
* **GC Safe-Point Restrictions**: Temporary values during sub-expression evaluation live in unmanaged Rust stack locals. Consequently, garbage collection can only safely trigger between top-level statements (`collect_if_needed`).
* **24-Byte Value Overhead**: `Value` is a 24-byte Rust `enum`, reducing CPU L1/L2 cache line efficiency during stack and collection operations.

### 2.2 Cranelift Direct JIT Advantages
* **Native Machine Code Execution**: Bypasses software VM dispatch loops entirely. Functions compile directly into CPU instructions (`mov`, `add`, `jmp`, `call`) executed natively on hardware.
* **Pure Rust & Zero C Dependencies**: Uses `cranelift-codegen` and `cranelift-jit`, eliminating external C++ LLVM dependencies while compiling scripts to RAM in milliseconds.
* **Unrestricted Garbage Collection Safe-Points**: Generated machine code emits poll calls (`quince_gc_poll`) at loop headers and allocation points.
* **Linear Execution & Cache Friendliness**: Local variables map directly to Cranelift virtual registers, CPU registers, or aligned native stack frames.

---

## 3. Bytecode IR & Operator Specification

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

    // Core Arithmetic & Operators
    Add, Sub, Mul, Div, FloorDiv, Mod, Pow, Neg, Not,
    Equal, Less, Greater,

    // Compound Assignment Opcodes
    AddAssign, SubAssign, MulAssign, DivAssign, FloorDivAssign,
    ModAssign, PowAssign, BitAndAssign, BitOrAssign, BitXorAssign,
    BitShlAssign, BitShrAssign,

    // Specialized Numeric Opcodes (Emitted via Type Inference)
    AddInt, SubInt, MulInt, DivInt, FloorDivInt, PowInt,
    AddFloat, SubFloat, MulFloat, DivFloat, PowFloat,

    // Fast Variable Access (using Quince Resolver Slots)
    GetLocal, SetLocal,     // Local slot index: [GetLocal, slot_u8]
    GetUpvalue, SetUpvalue, // Lexically captured upvalue index: [GetUpvalue, idx_u8]
    GetGlobal, SetGlobal,   // Top-level / module scope lookups: [GetGlobal, name_idx_u16]

    // Reference Parameters & Slot References (`ref`, `ref x: const T`)
    PassRef, StoreRef,

    // Control Flow
    Jump, JumpIfFalse, Loop, // Jump with 16-bit relative offset

    // Calls & Functions
    Call, Return, Closure, Yield, Await,

    // Object-Oriented Operations & Interface Polymorphic Dispatch
    GetProperty, SetProperty, GetSuper, Invoke, InvokeInterface, Class, Extend,

    // Resource Management
    Deinit, // Invokes `op deinit` on local scope unwind

    // Collections & Modules
    BuildList, BuildDict, BuildSet, Import,
}
```

### 3.1.1 Why Bytecode IR? (The Role of OpCodes in a Cranelift JIT Architecture)
Even though Cranelift JIT compiles code directly to native machine instructions, `OpCode` serves as Quince's **compact, high-level Intermediate Representation (IR)**:

1. **Fast IR Serialization & Disk Caching (`.qnc` / `.qn_cache/`)**:
   - Re-parsing raw ASTs or re-generating complex Cranelift IR on every run adds startup latency.
   - `OpCode` bytes serialize into flat `.qnc` binary files in milliseconds. On subsequent runs, Quince loads `.qnc` bytecode from `.qn_cache/` directly into Cranelift JIT in **< 0.5ms**.
2. **Platform & CPU Architecture Independence**:
   - `OpCode` instructions are 100% target-agnostic (`GetLocal`, `AddInt`, `InvokeInterface`).
   - The compiler frontend targets standard language semantics without needing to manage target CPU registers (`rax`, `x0`), stack alignments, or target-specific assembly ISAs.
3. **Developer Tooling & Disassembly (`quince --dump bytecode`)**:
   - Provides a clean, human-readable IR output for debugging compiler passes. Inspecting `GetLocal 0; AddInt` is far more readable than thousands of lines of raw x86_64/AArch64 machine instructions.
4. **Single-File Executable Bundling (`quince build --bundle`)**:
   - Allows instant single-binary packaging by appending serialized `.qnc` bytecode chunks to the runtime stub without requiring local C compilers or LLVM.
5. **Self-Hosting Bootstrapping (`compiler/*.qn`)**:
   - Emitting simple 1-byte IR tokens into a vector (`chunk.write_op(OpCode::AddInt)`) inside a self-hosted compiler written in Quince is vastly simpler and cleaner than binding directly to complex C-FFI backend builder APIs.

### 3.2 Operator Syntaxes & Method Slot Mapping

| Operator | Syntax Example | Desugared Method Slot | Operational Semantics |
| :--- | :--- | :--- | :--- |
| `+` | `a + b` | `op add` | Addition / Concatenation |
| `-` | `a - b` | `op sub` | Subtraction |
| `*` | `a * b` | `op mul` | Multiplication / List Repetition |
| `/` | `a / b` | `op div` | Floating-point Division |
| `//` | `a // b` | `op floordiv` | Integer Floor Division |
| `%` | `a % b` | `op rem` | Modulo / Remainder |
| `**` | `a ** b` | `op pow` | Exponentiation (Power) |
| `+=` | `a += b` | `op add` / assign | In-place or reassignment addition |
| `-=` | `a -= b` | `op sub` / assign | In-place or reassignment subtraction |
| `*=` | `a *= b` | `op mul` / assign | In-place or reassignment multiplication |
| `/=` | `a /= b` | `op div` / assign | In-place division assignment |
| `//=` | `a //= b` | `op floordiv` / assign | In-place floor division assignment |
| `%=` | `a %= b` | `op rem` / assign | In-place modulo assignment |
| `**=` | `a **= b` | `op pow` / assign | In-place exponentiation assignment |
| `&=` `|=` `^=` | `a &= b`, `a \|= b` | `op bit_and`, etc. | Compound bitwise operations |
| `<<=` `>>=` | `a <<= b`, `a >>= b` | `op bit_shl`, etc. | Compound bitwise shifts |

### 3.3 Chunks, Operand Encoding, and Source Spans
Instead of holding AST pointers, functions hold bytecode chunks with line/span arrays for diagnostic reporting:

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

### 3.4 CallFrame Layout
```rust
pub struct CallFrame {
    pub closure: Gc<ObjClosure>,
    pub ip: usize,          // Instruction Pointer index in chunk.code
    pub slots_offset: usize, // Base stack index for frame's local variables
}
```

### 3.5 Lexical Upvalues & Dynamic Closures
To support first-class closures and variable capturing across lexical scopes:

```rust
pub struct ObjUpvalue {
    pub location: usize,       // Index on stack while open
    pub closed: Option<Value>, // Captured heap value once closed
}

pub struct ObjClosure {
    pub function: Gc<ObjFunction>,
    pub upvalues: Vec<Gc<ObjUpvalue>>,
}
```
* **Open Upvalues**: Point directly to active local stack slots while the enclosing function frame is executing.
* **Closing Upvalues**: When an enclosing frame returns, `close_upvalues` copies stack values into heap `ObjUpvalue.closed` fields.

### 3.6 Reference Parameter Calling Conventions (`ref`, `ref x: const T`)
* **`OpCode::PassRef <slot_offset>`**: Pushes an indirect stack reference cell pointing to caller's stack slot.
* **`OpCode::StoreRef`**: Validates coerced types and writes directly into the caller's stack slot.

### 3.7 Disassembler Utility (`quince --dump bytecode`)
The CLI disassembler outputs structured IR code listing:

```text
== Disassembly: main ==
0000    12 OpCode::Constant      0 ('Hello World')
0003    │  OpCode::SetLocal       0
0005    13 OpCode::GetLocal       0
0007    │  OpCode::Call           1
0009    │  OpCode::Return
```

### 3.8 Binary IR Serialization (`.qnc`) & Co-Located Caching (`.qn_cache/`)
* **Binary Header**: Magic bytes (`QNBC`), source SHA-256 text digest, compiler timestamp, constant pool, span offset map.
* **Co-Located Cache Store**: Pre-compiled `.qnc` files are stored in `.qn_cache/` beside source files with an auto-generated `.gitignore`. Cold start execution completes in **< 0.5ms**.

### 3.9 Stack Trace Reconstruction & Exception Unwinding
When an exception occurs, the unwinder queries `chunk.spans[frame.ip]` across active call frames, rendering full diagnostic context via `src/error/render.rs`.

---

## 4. Low-Level Polymorphic Method Dispatch Architecture

### 4.1 Class Virtual Tables (`vtable`)
* **Single Inheritance**: Superclass method indices are invariant in derived subclasses (`vtable[method_slot]`).

### 4.2 Interface Implementation Tables (`itables`)
* **Interface Method Dispatch (`OpCode::InvokeInterface`)**: Each class object maintains an array of interface tables (`itables: Vec<ITable>`):
  ```rust
  pub struct ITable {
      pub interface_id: u32,
      pub method_pointers: Vec<*const u8>,
  }
  ```
* **Cranelift IR Emission**: Lowered to an inline hash/offset lookup against `class.itables` to fetch and execute native machine code function pointers directly.

---

## 5. Performance Optimization Roadmap

### 5.1 IEEE 754 NaN-Tagging (8-Byte Values)
Reduces `Value` enum size from 24 bytes to 8 bytes (`u64`):
* Double floats use standard IEEE 754 values.
* Quiet NaNs (`0x7FF8000000000000`) contain 51 unused payload bits.
* `Nil`, `True`, `False`, 32-bit `Int`, and 32-bit `ObjId` handles are packed into these bits.
* Decreases memory footprint by 40%–60% and doubles L1 cache efficiency.

### 5.2 Morphic Inline Caching (IC)
`GetProperty` and `InvokeInterface` instructions use 2-slot inline cache operands:
1. Slot 0: Cached Class ID pointer.
2. Slot 1: Direct field/method offset index.
Yields $O(1)$ fast-path execution bypassing hash lookups.

### 5.3 String Index & Small String Optimization (SSO)
* Character boundary offsets are cached to make string indexing $O(1)$.
* Short strings ($\le 6$ bytes) are packed inline inside 8-byte NaN values without heap allocations.

---

## 6. Standalone Executable Bundling Architecture (`--bundle`)

`quince build --bundle main.qn -o main`:
* Concatenates the lightweight Quince runtime stub with pre-compiled `.qnc` bytecode payloads into a standalone single-file executable.
* Instant sub-second build times without requiring C compilers or system toolchains.

---

## 7. Cranelift JIT/AOT Native Compilation Pipeline

### 7.1 Direct Cranelift JIT Execution (`cranelift-jit`)
Compiles Bytecode IR basic blocks directly into executable RAM machine code in milliseconds upon module load, calling functions directly via C-ABI pointers.

### 7.2 Native Standalone AOT Executables (`cranelift-object`)
`quince build --aot main.qn` emits native ELF, Mach-O, or PE object files via `cranelift-object` and links binaries using system linkers (`cc`, `lld`).

### 7.3 Native Dynamic Modules (`.qnx`) & Auto-Import Resolution
* `quince build --lib math.qn -o math.qnx` compiles Quince code into native shared libraries.
* On `import math`, checks for `math.qnx`. If timestamp (`mtime`) is newer than source, loads instantly via `dlopen`/`libloading`.

### 7.4 Source Location Propagation (`SourceLoc`) & Diagnostic Parity
Emitted Cranelift IR instructions are tagged with `builder.set_srcloc(SourceLoc::new(span_id))`. Runtime exception guards execute `quince_raise_runtime_error(span_id)`, preserving 100% diagnostic reporting parity.

---

## 8. Unified Module Import System & Foreign Function Interop

Quince features a **Unified Import System** where standard `import` syntax transparently resolves Quince source files (`.qn`), compiled native modules (`.qnx`), system C dynamic libraries (`.so`/`.dylib`/`.dll`), and CPython modules (`py:`).

```
                             ┌────────────────────────────────┐
                             │     import target_module       │
                             └───────────────┬────────────────┘
                                             │
                       Does module name start with `py:` prefix?
                                             │
                   ┌─────────────────────────┴─────────────────────────┐
                   YES                                                 NO
                   ▼                                                   ▼
 ┌───────────────────────────────────┐               ┌───────────────────────────────────┐
 │ CPython Bridge (`import py:numpy`)│               │ Native Module Resolution Pipeline │
 ├───────────────────────────────────┤               ├───────────────────────────────────┤
 │ • Binds dynamically via CPython   │               │ 1. Check for `target.qnx`         │
 │   `libpython3` C-API.             │               │    (Fresh compiled native module) │
 │ • Dynamically marshals functions, │               │ 2. Check for `libtarget.so/dylib` │
 │   tensors, lists, and dicts.      │               │    (Transparent System C FFI)     │
 └───────────────────────────────────┘               │ 3. Fall back to `target.qn`       │
                                                     │    (Compiles source via JIT)      │
                                                     └───────────────────────────────────┘
```

### 8.2 Import Syntax Grammar & Aliasing Rules (`as`)

Quince supports module-level imports, selective symbol imports, and flexible symbol aliasing via `as`:

#### 1. Module Level Imports & Aliasing (`import ... as ...`)
```quince
import filesystem                     # Binds `filesystem` module
import filesystem as fs              # Binds `fs` module alias
import raylib as rl                  # Binds `rl` C FFI module alias
import py:numpy as np                # Binds `np` CPython module alias
```

#### 2. Selective Symbol Imports & Aliasing (`from ... import ... as ...`)
Works transparently across Quince modules (`.qn`/`.qnx`), system C dynamic libraries (`.so`/`.dylib`/`.dll`), and CPython modules (`py:`):
```quince
# Quince module selective import with symbol aliasing
from filesystem import exists as file_exists, read_text as read_txt

# System C FFI dynamic symbol selective import
from raylib import InitWindow, CloseWindow, SetTargetFPS

# CPython bridge selective import with symbol aliasing
from py:numpy import array as np_array, zeros, ones
```

#### 3. Combined Module & Selective Import (`from module as alias import ...`)
Allows binding the module handle **and** bringing specific symbols directly into the current scope in a single statement:
```quince
from filesystem as fs import exists, read_text
# -> Binds `fs` as the module handle (fs.write_text)
# -> Binds `exists` and `read_text` directly into scope!

from py:numpy as np import array, zeros
# -> Binds `np` module handle (np.ones)
# -> Binds `array` and `zeros` directly into scope!
```

---

## 9. Multi-Threaded Concurrency & GIL-Free Parallelism Architecture

### 9.1 Async / Await Coroutine Suspension
`OpCode::Await` saves native register states and stack frames into heap-allocated `Task` handles when awaiting pending I/O.

### 9.2 Structured Concurrency (`parallel { spawn ... }`)
Parent `parallel` blocks wait for all child tasks to complete before exiting. Unhandled errors in child tasks trigger cancellation across siblings.

### 9.3 Task Handles (`.join()`, `.detach()`)
`.join()` awaits task resolution; `.detach()` routes unhandled exceptions to `runtime.on_unhandled_error`.

### 9.4 Automatic Background Reader-Writer Locking (`shared` Objects)
`shared Cache()` wraps field access in background `SharedMutex` (RWLock) primitives for concurrent parallel reads.

### 9.5 Zero-Copy Move Semantics (`spawn move`)
Transfers ownership of large mutable heap structures into worker threads with zero memory copying.

### 9.6 Dual-Arena Memory System & GIL-Free Work Stealing
* **No GIL**: Worker threads execute without a Global Interpreter Lock.
* **Per-Thread Local Arenas**: Independent thread-local GCs with zero global Stop-The-World pauses.
* **Global Const Arena**: Atomic ref-counted global arena for deeply frozen `const` values.

### 9.7 Safe Thread & Resource Cleanup (`op deinit`)
Unwinds frames using `op deinit` on local descriptors/sockets, releases locks automatically, and signals `ChannelError::Closed` on channel disconnect.

### 9.8 Safepoint Polls (`quince_gc_poll`) & `quince::ThreadTimeoutError`
Cranelift JIT emits safepoint polls at loop headers to check `runtime.is_cancelling()`. Stuck threads past grace period raise `quince::ThreadTimeoutError`.

---

## 10. Self-Hosting & Bootstrapping Roadmap

### 10.1 Stage 0: Seed Compiler (`quince-rust`)
Maintain the Rust-based compiler frontend and runtime harness as the bootstrap seed compiler.

### 10.2 Stage 1: Compiler Written in Quince (`compiler/*.qn`)
Port the Quince compiler to Quince (`lexer.qn`, `parser.qn`, `resolver.qn`, `inference.qn`, `bytecode.qn`, `cranelift.qn`, `main.qn`) and compile using `quince-rust --aot`.

### 10.3 Stage 2: Native Self-Hosted Binary (`quince-native`)
Produce a standalone native binary written in Quince with zero Rust dependencies, unlocking CTFE and macros.

### 10.4 Hybrid Codegen & Fallback Architecture
For Tier-1 targets (`x86_64`/`AArch64`), Quince uses internal native encoders. For niche/embedded targets (RISC-V, ARM Cortex-M), the compiler automatically falls back to Cranelift via `libcranelift`, guaranteeing 100% target platform coverage.

---

## 11. Granular Implementation Phasing Roadmap

| Sub-Phase | System Milestone | Key Deliverables & Architectural Focus |
| :--- | :--- | :--- |
| **Phase 1A** | **Bytecode IR Emitter & Tooling** | AST-to-IR compiler (`src/compiler/`), `Chunk` structure, span location maps, disassembler (`quince --dump bytecode`), `.qnc` binary format, and `.qn_cache/` co-located caching store (<0.5ms cold start). |
| **Phase 1B** | **Cranelift JIT Execution Core** | Translating Bytecode IR basic blocks into `cranelift-jit` RAM machine code, C-ABI callframe stack slots, `ref` parameter slots (`PassRef`, `StoreRef`), and `op deinit` unwinding. |
| **Phase 2** | **8-Byte NaN-Tagging & Dual-Arena GC** | 8-byte IEEE 754 NaN value packing (`Value(u64)`) and `Weak[T]` non-tracing reference GC handles. |
| **Phase 3** | **Inline Caching & Executable Bundling** | Fast-path Morphic Inline Caching (IC) for properties and `itable` interface dispatch, plus single-file executable bundler (`quince build --bundle`). |
| **Phase 4A** | **Cranelift AOT & Native Modules (`.qnx`)** | `quince build --aot` standalone binary compiler (via `cranelift-object`), `@export` / `@inline` pragmas, and `.qnx` auto-import shared library resolution. |
| **Phase 4B** | **Transparent C FFI & Python Bridge** | Transparent C dynamic library imports (`import raylib` / `.so`), selective imports & symbol aliasing (`from X import S as A`), and CPython C-API bridge (`import py:numpy`). |
| **Phase 5** | **Self-Hosting Bootstrapping** | Stage 0 (`quince-rust`) $\rightarrow$ Stage 1 (`compiler/*.qn`) $\rightarrow$ Stage 2 Native Binary (`quince-native`). |
| **Phase 6A** | **GIL-Free Multi-Threaded Concurrency** | Coroutines (`async`/`await`), structured concurrency (`parallel { spawn ... }`), `shared` RWLock objects, zero-copy `spawn move`, and per-thread local GC arenas. |
| **Phase 6B** | **Hardware SIMD Vector Acceleration** | Native 128-bit `float32x4` / `int32x4` vector primitive types compiling to SSE2 / AVX2 / ARM NEON native CPU vector instructions. |


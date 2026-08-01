# Quince — Bytecode VM & Native Compilation Architecture

This design document outlines the technical proposals, architectural specifications, and execution plan for transitioning **Quince** from its current AST Tree-Walk interpreter to a high-performance **Bytecode Virtual Machine (VM)**, along with a native **Ahead-Of-Time (AOT) LLVM Compilation** pipeline, **Native Dynamic Modules**, and a **Self-Hosting Bootstrapping** roadmap.

---

## 1. Executive Summary

Quince currently executes programs via an AST tree-walking interpreter (`src/interp.rs`). While ergonomic and ideal for language bootstrapping, tree-walking incurs significant host stack recursion overhead, CPU cache misses, and restricted garbage collection safe-points.

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

### 3.6 Serialization Format (`.qnc`) & Disassembler (`--dump bytecode`)
To support pre-compiled bytecode caching and debugging, Quince defines a binary chunk specification:

* **File Header Magic**: `QNBC` (`[0x51, 0x4E, 0x42, 0x43]`) + 2-byte Major/Minor Version.
* **Constant Pool**: Table of serialized literal values (`Int`, `Float`, `Str`, `Function`).
* **Chunk Code Stream**: Raw opcode bytes with line-span compression.
* **CLI Tooling**: `quince --dump bytecode script.qn` invokes the disassembler, printing opcode offsets, instruction names, constant parameters, and source file line references for compiler development.

### 3.7 Async / Await & Coroutine Suspension Mechanics
In a tree-walking interpreter, suspending an asynchronous function call stack requires complex continuation passing. In a Bytecode VM, `async`/`await` suspension becomes remarkably lightweight:

```rust
pub struct Task {
    pub frame: CallFrame,
    pub stack_snapshot: Vec<Value>,
    pub state: TaskState, // Pending, Resolved(Value), Rejected(QuinceError)
}
```

* **Suspending (`OpCode::Await`)**: If an awaited promise is pending, the VM pops the active `CallFrame` and saves its stack slice onto a heap-allocated `Task` object.
* **Resuming**: When the I/O event loop signals completion, the `Task` frame is pushed back onto `vm.frames` and execution resumes seamlessly from `frame.ip`.

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

Bytecode provides an ideal **Intermediate Representation (IR)** bridge to compile Quince code to native binaries via LLVM or Cranelift.

```
                          ┌───────────────────────────┐
                          │    quince run script.qn   │ (Instant Dev / REPL)
                          └─────────────┬─────────────┘
                                        │
                             Compiles to Bytecode
                                        │
                 ┌──────────────────────┴──────────────────────┐
                 ▼                                             ▼
  ┌─────────────────────────────┐               ┌─────────────────────────────┐
  │ Option 1: Bundled Binary    │               │ Option 2: Native AOT (LLVM) │
  │   `quince build --bundle`   │               │     `quince build --aot`    │
  ├─────────────────────────────┤               ├─────────────────────────────┤
  │ • Packs VM Runtime +        │               │ • Compiles Bytecode to      │
  │   Compiled Bytecode into    │               │   LLVM IR ──► Native Machine│
  │   a single executable file. │               │   Code (.exe / ELF).        │
  │ • Sub-second build time.    │               │ • Max speed / no VM loop.   │
  │ • Zero external toolchain.  │               │ • Requires LLVM / linker.   │
  └─────────────────────────────┘               └─────────────────────────────┘
```

### 6.1 Option 1: Self-Executing Single Binary Bundling (`--bundle`)
* Embeds the pre-compiled Bytecode `Chunk` alongside the lightweight VM runtime into a single standalone executable.
* **Build Time**: Sub-second (milliseconds). Requires no local C/LLVM toolchain installation.

### 6.2 Option 2: Native AOT Compilation via LLVM (`--aot`)
* **Bytecode-to-LLVM Translation**: Operates as a compiler pass over `Chunk` opcodes:
  * Each opcode translates directly into LLVM IR basic blocks.
  * Stack operations translate into LLVM virtual register assignments or local memory slots.
* **Runtime C-ABI Interface**: Compiled LLVM machine code calls back into Quince's C-ABI runtime helpers:

```c
// C-ABI functions exported by Quince Runtime Library
extern "C" {
    pub fn quince_alloc_list(heap: *mut Heap, capacity: usize) -> u64;
    pub fn quince_call_method(interp: *mut Interp, receiver: u64, name: *const i8, args: *const u64, count: usize) -> u64;
    pub fn quince_gc_poll(interp: *mut Interp);
}
```

---

## 7. Native Dynamic Modules (`.qnx`) & C FFI

Quince scripts can interact with compiled native binaries as ordinary modules without modifying importing code.

### 7.1 Drop-In Acceleration Workflow
1. Write code in Quince (`matrix.qn`). Import and run dynamically via `import matrix`.
2. Compile bottleneck script to a native dynamic library:
   ```bash
   quince build --lib matrix.qn -o matrix.qnx
   ```
3. The client script (`main.qn`) keeps `import matrix` unchanged. The runtime detects `matrix.qnx` and loads native C-speed functions dynamically via `libloading` / `dlopen`.

### 7.2 Native C FFI (Foreign Function Interface)
In addition to `.qnx` Quince dynamic modules, the AOT compilation infrastructure enables direct C library binding (`libcurl`, `sqlite3`, `raylib`) without writing C wrappers:

```quince
# Example: Direct C FFI in Quince
import ffi

final lib = ffi.load("sqlite3")
final open = lib.bind("sqlite3_open", [string, pointer], int)
```

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

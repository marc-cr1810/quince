# Phase 4A: Cranelift AOT Compilation & Native Dynamic Modules (`.qnx`)

## Executive Summary

Phase 4A implements **Native Standalone AOT Compilation (`quince build --aot`)**, `@export` / `@inline` pragma attributes, and **Compiled Native Dynamic Modules (`.qnx`)**.

---

## 1. Cranelift AOT Compilation Pipeline (`cranelift-object`)

`quince build --aot main.qn -o main`:
- Emits native system object files (`.o` / `.obj`) using `cranelift-object`.
- Invokes system linkers (`cc` / `lld` / `link.exe`) to build zero-dependency native ELF, Mach-O, or PE binary executables.

---

## 2. Pragma Attributes (`@export`, `@inline`)

- **`@export`**: Exposes function signatures with standard C-ABI linkage for native binary exports.
- **`@inline`**: Directs Cranelift code generator to inline function basic blocks into calling sites.

---

## 3. Native Dynamic Modules (`.qnx`)

- `quince build --lib module.qn -o module.qnx`: Compiles Quince modules into shared dynamic libraries (`.so` / `.dylib` / `.dll`).
- **Auto-Import**: `import module` automatically checks for `module.qnx`. If present and newer than source, loads natively via `dlopen`/`libloading`.

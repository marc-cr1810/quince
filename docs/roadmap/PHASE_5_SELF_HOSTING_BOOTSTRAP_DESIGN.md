# Phase 5: Self-Hosting Bootstrapping Roadmap

## Executive Summary

Phase 5 transitions the Quince toolchain to a **Self-Hosted Compiler written in Quince**, eliminating dependency on the initial Rust seed compiler.

---

## 1. Bootstrapping Sequence (3 Stages)

1. **Stage 0: Seed Compiler (`quince-rust`)**:
   - The Rust-based compiler harness (`src/`) compiles early Quince code.
2. **Stage 1: Quince-in-Quince Compiler (`compiler/*.qn`)**:
   - The Quince compiler frontend and code generator are written in Quince (`lexer.qn`, `parser.qn`, `resolver.qn`, `codegen.qn`).
   - Compiled to native binary using `quince-rust --aot compiler/main.qn -o quince-stage1`.
3. **Stage 2: Self-Hosted Binary (`quince-native`)**:
   - `quince-stage1` compiles its own source code (`compiler/*.qn`) to produce `quince-native`.
   - `quince-native` is verified via byte-for-byte binary diff against `quince-stage1`.

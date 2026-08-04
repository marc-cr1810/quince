# Quince v0.7 — Tooling, LSP & CLI Reference

This manual details the command-line interface, Language Server Protocol (LSP) capabilities, editor integrations, compiler architecture, and test suite layout in Quince v0.7.

---

## 1. Command-Line Interface (CLI)

The single `quince` binary manages code execution, interactive sessions, project creation, and editor integration.

### 1.1 CLI Commands

#### `quince run <FILE>`
Executes a Quince source file (`.qn`):
```bash
quince run program.qn
```
- `--dump <STAGE>`: Halts compilation after a specific pipeline stage and prints output. Supported stages:
  - `--dump tokens`: Dumps lexer token stream.
  - `--dump ast`: Dumps parsed Abstract Syntax Tree.

#### `quince repl`
Launches an interactive Read-Eval-Print Loop session:
```bash
quince repl
```
- Variables, classes, and function declarations persist across REPL lines.
- Type annotations are enforced interactively.

#### `quince init [PATH]`
Initializes a new Quince project structure in `PATH` (defaults to current working directory).

#### `quince lsp`
Runs the Language Server daemon:
```bash
quince lsp --stdio
```
- Uses standard I/O (JSON-RPC) for communication with text editors.

### 1.2 Global Options

- `--color <CHOICE>`: Controls terminal color output (`auto`, `always`, `never`). Default is `auto`.
- `--version`: Prints compiler version (`quince 0.7.0`).
- `--help`: Displays subcommand help text.

---

## 2. Language Server Protocol (LSP)

Quince features a native LSP server implemented directly in the compiler codebase (`src/lsp/`), sharing the exact parser and semantic analysis engine (`sema`) with the evaluator.

### 2.1 LSP Features

- **Inlay Hints (`textDocument/inlayHint`)**: Renders inline type hints for unannotated bindings (`let x` $\langle\text{: int}\rangle$ `= 5`) where static type inference is certain. Hints are omitted when bindings carry explicit annotations or evaluate to `Unknown`.
- **Type Completion After `:`**: Recommends primitive types (`int`, `float`, `string`, `bool`), user classes, type aliases, and container signatures (`list[...]`, `dict[...]`) immediately following a type annotation colon.
- **Visibility-Aware Completion**: Filters out `private` or `protected` members when completing property access (`obj.`) outside permitted class scopes.
- **Smart-Cast-Aware Completion**: Inside `if val is string { ... }`, completing `val.` offers string type methods (`upper()`, `lower()`, etc.) instead of nullable container methods.
- **Hover Documentation (`textDocument/hover`)**: Displays type signatures, `const` qualifiers, docstrings (`##` blocks), and `@param` descriptions.
- **Live Diagnostics (`publishDiagnostics`)**: Reports static type warnings, syntax errors, and resolution issues as you type in your editor.

---

## 3. Editor Integration (VS Code)

The official VS Code extension resides in [`editors/vscode`](../editors/vscode):

### 3.1 Installation from Source

```bash
cd editors/vscode
npm install
npx vsce package
code --install-extension quince-vscode-*.vsix
```

### 3.2 Server Resolution Logic

The extension locates the `quince` LSP binary using the following priority order:
1. Custom executable path specified in VS Code setting `quince.lspPath`.
2. Workspace build binary in `target/debug/quince` or `target/release/quince`.
3. System `PATH`.

---

## 4. Compiler Pipeline & Interpreter Architecture

### 4.1 Pipeline Stages

The Quince execution engine processes source code through four distinct stages:

```
Source Code (.qn)
      │
      ▼
 1. Lexer (src/syntax/token.rs)       --> TokenStream
      │
      ▼
 2. Parser (src/syntax/parser.rs)     --> AST (Abstract Syntax Tree)
      │
      ▼
 3. Semantic Analysis (src/sema/)     --> Type Inference & Symbol Tables
      │
      ▼
 4. Evaluator (src/interp/)           --> AST Interpreter & Heap Execution
```

### 4.2 Heap GC & Temporary Rooting (`interp.temps`)

During AST evaluation, native Rust builtin functions often allocate heap objects while executing user callbacks (e.g. `list.map(f)` or `list.filter(f)`). To prevent garbage collection or lifetime corruption during allocation safe points:

- The interpreter maintains a root stack `interp.temps: Vec<Value>`.
- Before executing a native callback, builtins record a stack mark: `let mark = interp.temps.len()`.
- Intermediate values, lists, and closures are pushed onto `interp.temps`.
- On completion or error exit, `interp.temps.truncate(mark)` cleans up temporary roots reliably.

### 4.3 Diagnostic & Error Reporting Pipeline

Diagnostics use the `QuinceError` struct (`src/error/`):
- **Span Tracking**: Every AST node carries a source location `Span` (start byte, end byte).
- **Line & Column Calculation**: Spans are converted to 1-indexed line and column numbers during error rendering.
- **Actionable Hints**: Errors support attachable hint messages via `.with_help("...")`, which are printed alongside diagnostic error messages in both terminal output and LSP diagnostics.

---

## 5. Test Suite Architecture

Quince uses a corpus-driven testing strategy located in [`tests/cases`](../tests/cases):

### 5.1 Test File Pairings

Adding a test case requires dropping `.qn` files into `tests/cases/` without modifying Rust code:
- `test_name.qn`: The Quince source program.
- `test_name.out`: Expected standard output produced by `print()`.
- `test_name.err`: Expected error message substring raised by the program.
- `test_name.report`: Expected full terminal report (including diagnostic formatting).

### 5.2 Running Tests

Execute unit tests and all corpus test cases:

```bash
cargo test
cargo clippy --all-targets
```

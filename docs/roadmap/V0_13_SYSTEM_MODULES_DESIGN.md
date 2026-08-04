# Quince v0.13: System Standard Library Modules (`sys`, `path`, `io`)

## Executive Summary

Quince v0.13 expands the built-in standard library with **`sys`**, **`path`**, and extended **`io`** modules, following Quince's flat module import model (`import sys`, `import path`, `import io`).

---

## 1. The `path` Module (`import path`)

Provides cross-platform file path manipulation:

* `path.join(head: string, tail: string): string`: Normalizes and joins path components (`"src/main.qn"`).
* `path.extension(p: string): string?`: Returns the file extension (e.g. `"qn"`, `"qnx"`).
* `path.parent(p: string): string?`: Returns the parent directory path.
* `path.is_absolute(p: string): bool`: Checks whether the path is absolute.

---

## 2. The `sys` Module (`import sys`)

Provides system environment inspection, process management, and diagnostic execution:

* `sys.args(): list[string]`: Returns command-line invocation arguments.
* `sys.exec(cmd: string, args: list[string]): ProcessResult`: Spawns external processes (e.g. system linkers `cc` / `lld`) and returns exit status, stdout, and stderr.
* `sys.exit(code: int)`: Terminates the process with exit code.
* `sys.eprintln(msg: string)`: Writes formatted diagnostic text directly to `stderr`.
* `sys.env(name: string): string?`: Reads environment variables.

---

## 3. The `io` Module Extensions (`import io`)

Extends existing `io` module features:

* `io.read_dir(path: string): list[string]`: Returns directory entry paths for scanning source files.
* `io.remove(path: string)`: Deletes a file or empty directory.

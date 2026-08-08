# Quince v0.13 — the standard library a compiler needs

Design for the milestone after v0.12. Six modules and one language change, and the whole of
it is scoped by a single question: **what does `compiler/*.qn` need in order to exist?**

This was two documents — `sys`/`path`/`io` in one and `text`/`collections`/`binary` in the
other — and they are merged because they are one answer. Both were written for Phase 5's
self-hosting bootstrap, neither is large enough to be a milestone, and splitting them meant
two documents whose deferred sections would each say "see the other one".

**No new language semantics.** Every module here is Quince written against the library
surface v0.6 established, plus natives where the runtime has to be reached. That is the test
applied to each item: if it needs a language change, it does not belong in this milestone —
which is why §8 is short and why import aliasing (§7) is called out as the exception rather
than folded in quietly.

---

## 1. What this milestone adds

1. **`path`** — cross-platform path manipulation. §2.
2. **`sys`** — arguments, environment, exit, stderr, and subprocesses. §3.
3. **`io` extensions** — directory listing and removal. §4.
4. **`text`** — `StringBuilder`, character classification, terminal styling. §5.
5. **`collections`** — `Interner`, `IndexMap[K, V]`, `BitSet`. §6.
6. **`binary`** — `ByteBuffer`, little- and big-endian emission. §6.4.
7. **Import aliasing** — `import x as y`, `from x import a as b`. §7. The one language change,
   and it is here because `BYTECODE_VM_DESIGN.md` §8.2 already assumes it exists.

---

## 2. What earlier milestones leave in place

- **Modules are v0.6's**, flat and imported by name, with `public` deciding what is exported
  (v0.7 §3.6). Packages, search paths, and subdirectory imports remain what DESIGN.md's
  roadmap calls "what is left of modules", and this milestone does not touch them.
- **`bytes` is v0.10 §7.4's**, and `binary.ByteBuffer` is built on it. That dependency is the
  reason this milestone cannot precede v0.10.
- **`set[T]`, `dict`, and `list` are the containers.** `collections` adds three that the
  built-ins cannot be, and not a fourth spelling of one they can.
- **`op hash` is v0.11's**, without which `Interner` and `IndexMap` would be restricted to
  the closed key set.
- **There is no `char` type**, and there never has been — DESIGN.md, *String literals*:
  "There is no character type, so `'a'` is a one-character string". §5.2 is written against
  that, and an earlier draft of this milestone that proposed a `char` class contradicted a
  decision the language has held since v0.1.

### 2.1 The `io` module already exists

v0.6 shipped `io` with file reading, writing, and `io.line`. §4 extends it; it does not
redefine it.

---

## 3. `path`

```quince
import path

print(path.join("src", "main.qn"))        # "src/main.qn"
print(path.extension("src/main.qn"))      # "qn"
print(path.parent("src/main.qn"))         # "src"
print(path.is_absolute("/usr/bin"))       # true
```

- `path.join(head: string, tail: string): string` — normalizes separators and collapses
  `.` segments. It does **not** touch the filesystem, and it does not resolve `..` against
  anything real, because that would need one.
- `path.extension(p: string): string?` — without the dot, `nil` when there is none.
- `path.parent(p: string): string?` — `nil` at a root.
- `path.stem(p: string): string` and `path.file_name(p: string): string?`.
- `path.is_absolute(p: string): bool`.

**Separators are normalized to `/` on output**, on every platform, and both `/` and `\` are
accepted on input under Windows. A library that answered differently per platform would make
every test in the corpus platform-dependent, and the corpus is the thing that keeps this
library honest.

---

## 4. `sys`

```quince
import sys

for arg in sys.args() {
    print(arg)
}

let home: string? = sys.env("HOME")
sys.eprint("compiling…")

let result = sys.exec("cc", ["-o", "main", "main.o"])
if result.code != 0 {
    sys.eprint(result.stderr)
    sys.exit(1)
}
```

- `sys.args(): list[string]` — the arguments the program was run with, **not** including the
  interpreter or the script path. A program asking for its own name asks `sys.program()`.
- `sys.env(name: string): string?`
- `sys.exit(code: int)` — returns nothing, because it does not return. It unwinds nothing:
  an `op deinit` does not exist and a `catch` cannot see this.
- `sys.eprint(…)` and `sys.eprintln(…)` — the `stderr` counterparts of `print`. Named for
  `print` rather than for any other language's spelling, since `print` is the word this
  language uses.
- `sys.exec(cmd: string, args: list[string]): ProcessResult` — runs a program to completion
  and collects its output.

**`ProcessResult` is a class in `sys`**, and an earlier draft named it in a signature without
ever defining it:

```quince
public class ProcessResult {
    public final code: int
    public final stdout: string
    public final stderr: string
}
```

- **`exec` waits, and captures.** Streaming output, stdin, and a spawned handle that outlives
  the call are all deferred (§8) — the compiler needs to invoke a linker and read what it
  said, and that is the whole requirement.
- **A command that cannot be run throws**, rather than answering a `ProcessResult` with a
  made-up code. Failing to start and exiting non-zero are different events.
- **Output is decoded as UTF-8**, with invalid sequences replaced rather than throwing. A
  linker's diagnostics are not worth losing to an encoding error.

---

## 5. `io` extensions

- `io.read_dir(p: string): list[string]` — entry paths, not names, so the result is usable
  without rejoining. Order is unspecified by the filesystem and **sorted** before it is
  returned, because a compiler that compiles files in directory order is a compiler whose
  output depends on the filesystem.
- `io.remove(p: string)` — a file or an empty directory. Recursive removal is deliberately
  absent: it is the one filesystem operation whose bug destroys work, and nothing in the
  bootstrap needs it.
- `io.exists(p: string): bool`, `io.is_dir(p: string): bool`, `io.make_dir(p: string)`.

---

## 6. `text`, `collections`, and `binary`

### 6.1 `text.StringBuilder`

```quince
import text

let out = text.StringBuilder()
out.push("fn ")
out.push(name)
out.push("() {\n")
print(out.build())
```

Amortized `O(1)` append. It exists because string `+` builds a new string every time, and a
code generator appending a hundred thousand fragments is the case that makes that quadratic.
`build()` consumes the buffer and answers a `string`.

### 6.2 Character classification

There is no `char` type (§2), so these are `string` methods, added by `text` through an
`extend string` block:

```quince
import text

print("7".is_digit())          # true
print(" ".is_whitespace())     # true
print("q".is_alphanumeric())   # true
```

Each answers for **every** character in the receiver, and `""` answers `false`. That is the
rule that makes `"77".is_digit()` true and makes the methods useful on a token rather than
only on a one-character slice — and it is stated because "is this string a digit" has two
plausible readings and a lexer wants this one.

### 6.3 Terminal styling

`text.red(s)`, `text.bold(s)`, `text.dim(s)`, and the rest wrap a string in ANSI codes and
answer a new string. `text.styled(): bool` reports whether styling should be emitted at all,
answering the same question `color.rs` answers for the Rust implementation — a TTY check and
`NO_COLOR`. The styling functions consult it, so a program that pipes its output gets plain
text without asking.

### 6.4 `collections` and `binary`

- **`Interner`** — `intern(s: string): int` and `resolve(id: int): string?`. Token and AST
  comparisons become integer comparisons. It is the one item here whose absence is felt as
  a constant factor across an entire self-hosted compiler rather than at one site.
- **`IndexMap[K, V]`** — insertion-ordered, with `O(1)` lookup and *no* removal. Quince's
  `dict` already keeps insertion order (DESIGN.md, *Collections*), so `IndexMap` earns its
  place only by what it adds: index-addressed access, `map.at(i)`, which a symbol table
  wants and a `dict` cannot give. Removal is omitted because it would renumber those
  indices, which is the same reason `dict`'s removal is linear.
- **`BitSet`** — `set`, `clear`, `test`, `union`, `intersect`, `count`. For liveness and
  reachability, where a `set[int]` costs a hash per bit.
- **`ByteBuffer`** in `binary` — `write_u8`, `write_u16_le`, `write_u32_le`, `write_u64_le`,
  `write_f64_le`, and big-endian counterparts, plus `bytes()` to finish. Backed by v0.10's
  `bytes`. It is what writes a `.qnc` file (Phase 1A) and what a code generator emits into.

**Endianness is always explicit in the name.** There is no `write_u32` that picks by host,
because a file format that differs by machine is not a file format.

---

## 7. Import aliasing

The one language change, and it is here rather than deferred because
`BYTECODE_VM_DESIGN.md` §8.2 specifies three import forms the language cannot currently
parse, and a design document assuming syntax that no milestone schedules is the gap this
revision of the roadmap exists to close.

```quince
import collections as c
from path import join, extension as ext
from collections as c import Interner
```

- **`import m as n`** binds the module under `n` and not under `m`.
- **`from m import a, b as c`** binds the named symbols directly, and does **not** bind the
  module. This is v0.6's `from` reaching an aliasing form; `from` remains a contextual
  keyword recognized only at the start of a statement with an `import` after it, exactly as
  `token.rs` documents.
- **`from m as n import a, b`** binds both — the module under `n` *and* the named symbols.
  It is in `BYTECODE_VM_DESIGN.md` §8.2, so it is specified here; it is also the form most
  likely to be judged unnecessary later, and it is the one item in this milestone that
  could be dropped without anything else noticing.
- **A selective import may only name `public` declarations**, checked where the module is
  loaded, by v0.7 §3.6's existing run-time rule.
- **Aliasing does not create a second binding.** `import path as p` makes `path` an ordinary
  free name again, available to a program that wants it for something else.

---

## 8. Enforcement

**At resolution:**
- An alias colliding with a declaration in the importing scope. §7.
- A `from … import` naming a symbol twice, or aliasing to a type's name (DESIGN.md, *A type's
  name belongs to the type*). §7.

**At run time:**
- A selective import naming a symbol the module does not export. §7.
- `sys.exec` on a command that cannot be started. §4.
- `io.remove` on a non-empty directory, which is refused rather than recursed. §5.

---

## 9. Work items, in order

**Tranche 1 — `path` and `sys`.** Pure natives, no new types except `ProcessResult`, and the
two modules everything else in the bootstrap calls.

**Tranche 2 — `io` extensions.** Small, and beside tranche 1 by subject.

**Tranche 3 — import aliasing.** The language change, sequenced early so the rest of the
library can be written against the form it will ship with.

**Tranche 4 — `text`.** `StringBuilder` first, then classification, then styling.

**Tranche 5 — `collections` and `binary`.** `Interner`, `IndexMap`, `BitSet`, `ByteBuffer`.

**The cut line is after tranche 3.** Tranches 4 and 5 are performance and convenience for a
self-hosted compiler that does not exist yet, and Phase 5 is where their absence would
actually bite. Tranches 1–3 are what a Quince *program* is missing today.

---

## 10. Deferred

**Process streaming, stdin, and spawned handles.** §4. `exec` waits and captures. A compiler
driving a linker needs nothing more, and a real process API wants concurrency (Phase 6A) to
be interesting.

**Recursive directory removal.** §5, and deliberately.

**A `regex` module.** Wanted, large, and not needed by a hand-written lexer.

**A `json` module.** Same reasoning, and it wants v0.10's enums to be worth writing.

**Packages, search paths, and subdirectory imports.** Inherited from DESIGN.md's roadmap
unchanged. §7 aliases what the current flat model can already find.

**Sized integer types.** `ByteBuffer`'s methods name widths in their own names precisely so
that this milestone does not need `u8` and `u32` to exist. Inherited from v0.10 §10.

---

## 11. Decisions taken

- **Two milestones became one.** Both were library work for the same consumer and neither
  filled a milestone. Head of file.
- **No `char` type.** Classification is `string` methods over every character, because
  DESIGN.md settled the character question in v0.1. §6.2.
- **`ProcessResult` is a declared class**, not a name in a signature. §4.
- **A command that fails to start throws; a command that fails reports a code.** §4.
- **`io.read_dir` sorts**, so a compiler's output does not depend on the filesystem. §5.
- **Endianness is always in the method name.** §6.4.
- **`IndexMap` earns its place by index access, not by ordering**, which `dict` already has.
  §6.4.
- **Import aliasing is a language change and is named as one.** §7, §1.
- **`from … import` does not bind the module**, and `from … as … import …` binds both. §7.
- **The cut line is after the language change**, not before it. §9.

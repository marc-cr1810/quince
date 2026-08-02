# Quince

A gradually-typed scripting language, with an interpreter, a REPL, and a
language server in one binary.

```quince
alias ScoreTable = dict[string, int]

fn best(scores: ScoreTable): string? {
    let leader: string? = nil
    let top: int = 0
    for name in scores.keys() {
        let score: int = scores[name] ?? 0
        if score > top {
            top = score
            leader = name
        }
    }
    return leader
}

print(best({"alice": 95, "bob": 88}) ?? "nobody")
```

Types are optional and checked where they are written. An unannotated program is
dynamically typed and always was; an annotation is a claim the language enforces
at the boundary the value crosses, and the editor reports before it runs.

## Install

```bash
cargo install --git https://github.com/marc-cr1810/quince
```

That puts `quince` on your `PATH`. From a checkout, `cargo install --path .`
does the same thing.

Building without installing is `cargo build --release`, which leaves the binary
at `target/release/quince`.

Quince needs a Rust toolchain to build. It has no other dependencies.

## Use

```bash
quince run program.qn     # run a file
quince repl               # an interactive session
quince lsp                # the language server, over stdio
```

`quince --help` lists the rest.

## Editor support

The VS Code extension lives in [`editors/vscode`](editors/vscode) and provides
syntax highlighting, completion, hover, go-to-definition, inlay hints, and live
diagnostics — all of it from `quince lsp`, so the editor and the language never
disagree about what a program means.

To install it from a checkout:

```bash
cd editors/vscode
npm install
npx vsce package
code --install-extension quince-vscode-*.vsix
```

The extension finds the language server by looking, in order, at the
`quince.lspPath` setting, a `target/debug` or `target/release` build in the
workspace, and then `PATH`. A `cargo install` is enough; a checkout you are
working on is picked up without one, so a rebuild is all it takes to test a
change.

## Documentation

`DESIGN.md` is the language's design and the reasoning behind it. The milestone
documents beside it — `V0_7_TYPE_SYSTEM_DESIGN.md` and the rest — record what
each release added and, more usefully, which alternatives were rejected and why.

## Development

```bash
cargo test      # unit tests, plus every program in tests/cases
cargo clippy --all-targets
```

`tests/cases` is the corpus: a `.qn` file and a companion holding what it should
print (`.out`), the error it should raise (`.err`), or the whole rendered
diagnostic (`.report`). Adding a case is dropping in two files — no Rust
changes.

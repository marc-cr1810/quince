# Quince v0.7 — Language Documentation

Welcome to the official documentation for **Quince v0.7**, a gradually-typed scripting language designed with non-nullable defaults, reified container generics, rich object-oriented dispatch, and built-in editor tooling in a single binary.

---

## Language Philosophy

Quince is designed around four core technical principles:

1. **Gradual Typing Without Dynamic Penalty**: An unannotated Quince program is dynamically typed (`Unknown`) and behaves like an imperative scripting language. Adding type annotations introduces static diagnostic checks in your editor and strict, low-overhead boundary enforcement at run time.
2. **Null Safety by Default**: Primitive types (`int`, `float`, `string`, `bool`) and class types cannot hold `nil` unless explicitly marked with `?` (e.g. `string?`). Safe navigation (`?.`) and null coalescing (`??`) make handling optional values ergonomic.
3. **Explicit, Non-Ambiguous Dispatch**: Methods use receiver-based dispatch. Built-in system calls (`len`, `print`, `type`) and language operators (`+`, `==`, `[]`) hook into dedicated operator slots (`op`) with defined signatures and return type contracts.
4. **Single Binary Tooling**: The compiler, interpreter, interactive REPL, and Language Server (LSP) are packaged into a single `quince` executable with zero external runtime dependencies.

---

## Quickstart & Installation

### Building & Installing

Quince requires a Rust toolchain to build:

```bash
# Clone and install from local repository
cargo install --path .

# Build release binary without installing
cargo build --release
```

The resulting executable is located at `target/release/quince` (or on your `PATH` after `cargo install`).

### Running Code

```bash
# Execute a Quince source file
quince run program.qn

# Launch an interactive REPL
quince repl

# Start the Language Server daemon over stdio
quince lsp --stdio
```

---

## Quince v0.7 Code Tour

Below is a complete, runnable Quince v0.7 program demonstrating variable bindings, type annotations, classes, visibility, operator overloading, container generics, null safety, standard library usage, and error handling:

```quince
import math
import io

alias ScoreTable = dict[string, int]

## Represents a player in a game session.
class Player {
    public final id: string
    private let score: int = 0
    protected let alias_name: string?

    op init(id: string, name: string?) {
        self.id = id
        self.alias_name = name
    }

    public fn add_points(points: int) {
        if points > 0 {
            self.score = self.score + points
        }
    }

    public fn current_score(): int {
        return self.score
    }

    # Custom string representation for print() and string()
    op string(): string {
        let display = self.alias_name ?? self.id
        return display + " (Score: " + string(self.score) + ")"
    }
}

# Functions take explicit type annotations and const qualifiers
fn calculate_leaderboard(players: const list[Player]): ScoreTable {
    let table: ScoreTable = {}
    for player in players {
        table[player.id] = player.current_score()
    }
    return table
}

fn main() {
    let p1 = Player("usr_101", "Alice")
    let p2 = Player("usr_102", "Bob")

    p1.add_points(150)
    p2.add_points(200)

    let roster: list[Player] = [p1, p2]
    let scores = calculate_leaderboard(roster)

    for id in scores.keys() {
        let score: int? = scores[id]
        print("Player ID:", id, "-> Score:", score ?? 0)
    }

    # Safe navigation & smart casting
    let sample: Player? = roster[0]
    if sample is Player {
        print("First player:", string(sample))
    }
}

main()
```

---

## Documentation Map

Explore the detailed topic manuals:

- **[Grammar & Syntax](grammar.md)**: Lexical tokens, EBNF syntax rules, statement structure, and the complete operator precedence matrix.
- **[Type System](type_system.md)**: Gradual annotations, non-nullable vs nullable (`T?`), top types (`any`, `_`), `const T` value qualifiers, container generics (`list[T]`, `dict[K, V]`), type aliases (`alias`), and smart casting (`is`).
- **[Classes & Object-Oriented Programming](classes_and_objects.md)**: Class headers, inheritance (`extends`), openness modifiers (`complete`, `sealed`, `final`), member visibility (`public`, `private`, `protected`), extension blocks (`extend`), and the 29 `op` slots.
- **[Control Flow & Error Handling](control_flow_and_errors.md)**: Loops (`while`, `for`), functions & closures (`fn`), doc comments (`##`), `try`/`catch`, `throw`, and built-in `Error` kinds.
- **[Standard Library & Built-ins](stdlib.md)**: System globals (`print`, `len`, `type`), conversion constructors (`int`, `float`, `string`, `bool`, `list`, `dict`), string/list/dict methods, and standard library modules (`math`, `io`, `random`, `time`).
- **[Tooling & Language Server](tooling_and_architecture.md)**: CLI subcommands (`run`, `repl`, `lsp`, `init`), LSP diagnostic feedback, inlay hints, editor autocompletion, and VS Code integration.

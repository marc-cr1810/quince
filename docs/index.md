# Quince v0.8.1 — Language Documentation

Welcome to the official documentation for **Quince v0.8.1**, a gradually-typed scripting language designed with non-nullable defaults, reified container generics, rich object-oriented dispatch, and built-in editor tooling in a single binary.

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

## Quince v0.8.1 Code Tour

Below is a complete, runnable Quince v0.8.1 program demonstrating variable bindings, type annotations, classes, visibility, operator overloading, container generics, null safety, standard library usage, and error handling:

```quince
import math
import io

alias ScoreTable = dict[string, int]

## Represents a player in a game session.
class Player {
    public final id: string = ""
    private let score: int = 0
    protected let alias_name: string? = nil

    op init(id: string, name: string? = nil) {
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

## New in v0.8

v0.8 adds the words a declaration may carry and the one change to dispatch they make
possible. Everything here *reads* v0.7's parameter types without being part of the type
system.

```quince
class Vector {
    op init(x: float = 0.0, y: float = 0.0) {     # defaults, so `let v: Vector` works
        self.x = x
        self.y = y
    }

    # Several declarations may share a name, dispatched on argument types.
    public op add(other: Vector): Vector { return Vector(self.x + other.x, self.y + other.y) }
    public op add(scalar: float): Vector { return Vector(self.x + scalar, self.y + scalar) }

    # `const` marks the body pure: no field assignment, no non-const call on `self`.
    const op string(): string { return "(" + string(self.x) + ", " + string(self.y) + ")" }

    # No subclass may replace this one.
    final fn dimensions(): int { return 2 }
}

class Named extends Vector {
    # Replacing a superclass member has to say so, and saying so where nothing is
    # replaced is refused too.
    override op add(other: Vector): Vector { return super.add(other) }
}

let origin: Vector                        # default construction
print(origin + 1.0, origin + Vector())    # overload dispatch
print(2 ** 10, Vector(y: 3.0))            # exponentiation, keyword arguments

let total = 1
total <<= 4                               # compound assignment, target evaluated once
print(total)
```

- **`const fn` / `const op`** — purity the resolver enforces. [Reference](classes_and_objects.md#6a2-const-fn-and-const-op)
- **`override` and `final` on members** — overriding declared, and forbidden. [Reference](classes_and_objects.md#6a1-override-and-final)
- **Implicit constructor coercion, and `explicit`** — [Reference](type_system.md#8-implicit-constructor-coercion)
- **Default construction** — `let logger: Logger`. [Reference](type_system.md#9-default-construction)
- **Overloading** — [Reference](classes_and_objects.md#6b-overloading)
- **Default parameters and keyword arguments** — [Reference](type_system.md#10-default-parameters--keyword-arguments)
- **`**` and compound assignment** — [Reference](grammar.md#2-operator-precedence--associativity)

---

## New in v0.8.1 — the logical operators are words

`&&`, `||`, and `!` are gone. The three logical operators are `and`, `or`, and `not`, which
puts them beside `is` and `in` — words the language already read as operators — and leaves
`&` and `|` meaning exactly one thing each, so there is no longer a pair to mistype one half
of. `!` survives only inside `!=`.

```quince
let ready = loaded and not failed         # `and`, `or`, `not`
let name = supplied or "anonymous"        # answers with an operand, not a bool

if "carol" not in scores { }              # `not in`, the negation of `in`
if value is not string { }                # `is not`, the negation of `is`

# `not` binds looser than a comparison, so the word reads as the word:
# this asks whether the two differ.
if not a == b { }

let count: int? = nil
count ??= expensive()                     # assigns only when nil — and only then calls
flag and= still_valid()                   # assigns only when flag is truthy
name or= fallback()                       # assigns only when name is falsy

let i = 0
i++                                       # a statement, not an operator
++i                                       # the same statement
```

`++` and `--` produce no value, which is what makes the prefix and postfix spellings mean
the same thing: both are `i += 1`. `x = i++` is a syntax error rather than a puzzle. They
desugar to the compound assignment, so `d[key()]++` calls `key` exactly once.

`and=`, `or=`, and `??=` are written like compound assignments and are not one: each reads
the target first and assigns only if what it found does not already answer. The right side
may never run, and neither may the write.

- **Word operators, `not in`, `is not`, and the precedence of `not`** — [Reference](grammar.md#2-operator-precedence--associativity)
- **`++`, `--`, and the short-circuiting assignments** — [Reference](grammar.md#2-operator-precedence--associativity)

---

## Documentation Map

Explore the detailed topic manuals:

- **[Grammar & Syntax](grammar.md)**: Lexical tokens, EBNF syntax rules, statement structure, and the complete operator precedence matrix.
- **[Type System](type_system.md)**: Gradual annotations, non-nullable vs nullable (`T?`), top types (`any`, `_`), `const T` value qualifiers, container generics (`list[T]`, `dict[K, V]`), type aliases (`alias`), smart casting (`is`), implicit constructor coercion, default construction, and default parameters with keyword arguments.
- **[Classes & Object-Oriented Programming](classes_and_objects.md)**: Class headers, inheritance (`extends`), openness modifiers (`complete`, `sealed`, `final`), member visibility (`public`, `private`, `protected`), extension blocks (`extend`), the 30 `op` slots, the member modifiers (`override`, `final`, `const`, `explicit`), and overloading.
- **[Control Flow & Error Handling](control_flow_and_errors.md)**: Loops (`while`, `for`), functions & closures (`fn`), doc comments (`##`), `try`/`catch`, `throw`, and built-in `Error` kinds.
- **[Standard Library & Built-ins](stdlib.md)**: System globals (`print`, `len`, `type`), conversion constructors (`int`, `float`, `string`, `bool`, `list`, `dict`), string/list/dict methods, and standard library modules (`math`, `io`, `random`, `time`).
- **[Tooling & Language Server](tooling_and_architecture.md)**: CLI subcommands (`run`, `repl`, `lsp`, `init`), LSP diagnostic feedback, inlay hints, editor autocompletion, and VS Code integration.

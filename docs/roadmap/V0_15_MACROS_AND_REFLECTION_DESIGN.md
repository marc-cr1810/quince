# Quince v0.15 — hygienic macros and type reflection

Design for the last language milestone before the execution-engine work begins
(`BYTECODE_VM_DESIGN.md` and its phases). Two features, and they are one milestone because a
macro that cannot ask what a type looks like can only paste text, which is the version of
macros nobody wants.

**This was one third of a ninety-eight-line v0.12** alongside CTFE and custom operators. It
was split out because macros are not a feature you can specify in a page: hygiene, the
quoting form, when expansion happens relative to resolution, and what a diagnostic looks
like when the error is inside an expansion are four decisions, and a document that makes none
of them is a document that cannot be implemented from.

It is **last** rather than earlier for a reason worth stating: a macro system is the hardest
thing in a language to change after programs depend on it, and every milestone before this
one is still moving the syntax macros would be generating.

---

## 1. What this milestone adds

1. **`macro` declarations** operating on AST nodes, with `quote` and `unquote`. §3.
2. **Hygiene** — a name introduced by an expansion cannot capture or be captured. §4.
3. **`type_of(T)`**, compile-time reflection over a type's fields, variants, and
   signatures. §5.

---

## 2. What earlier milestones leave in place

- **CTFE is v0.12's.** A macro runs at compile time on AST values; a `const fn` runs at
  compile time on ordinary values. They share the evaluator v0.12 §3 specifies, including
  its step budget and its refusal to print.
- **Every AST node carries a `Span`** (DESIGN.md, *Errors are a feature*). §4.3 is entirely
  built on that, and the feature would not be worth attempting without it.
- **Enums, generics, and interfaces** are what §5's reflection has to describe. Reflection
  landing before v0.11 would have described a type system that was still growing shapes.
- **The resolver runs after the parser and before evaluation.** §3.2 places expansion
  between them, which is the only position that lets a macro produce code the resolver then
  checks like any other.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `macro` | **new keyword** | a compile-time AST transformation |
| `quote` | **new keyword** | an AST literal |
| `unquote` | **new keyword** | splices a value into a `quote` |
| `Expr`, `Stmt`, `Block`, `Ident` | **new built-in types** | the AST values a macro takes and returns |
| `!` | exists (`!x`) | **new position:** postfix on a name, marking a macro call. §3.1 |
| `type_of` | **new global** | compile-time reflection. §5 |

---

## 3. Macros

### 3.1 Declaration and invocation

```quince
public macro assert_eq(left: Expr, right: Expr): Stmt {
    return quote {
        let l = unquote(left)
        let r = unquote(right)
        if l != r {
            throw Error("assertion failed: " + string(l) + " != " + string(r))
        }
    }
}

assert_eq!(total, 42)
```

- **A macro is called with `!`**, and an ordinary function is not. This is Rust's answer and
  it is taken for Rust's reason: a macro's arguments are not evaluated, so a reader who
  cannot tell a macro call from a function call cannot tell whether `f(g())` calls `g`. The
  alternative — making them indistinguishable, as Lisp does — is only livable in a language
  where everything is a form, and Quince is not one.
- **Parameters are typed by AST kind** — `Expr`, `Stmt`, `Block`, `Ident` — and the parser
  checks the argument against the kind before the macro runs, so a mistake is reported at
  the call rather than inside the expansion.
- **A macro declares what it returns**, `Expr` or `Stmt` or `Block`, and the return kind
  decides where the call is legal. An `Expr` macro in statement position is fine; a `Stmt`
  macro in expression position is refused at the call.
- **Variadic macros take a pack** — `macro dbg(args: Expr...)` — reusing v0.9 §3.4's form
  rather than inventing a repetition syntax. This is the deliberate small answer to what is
  the largest single feature in Rust's macro system, and §7 records what it gives up.
- **A macro body is ordinary Quince**, run under v0.12 §3's compile-time evaluator with its
  step budget. It may call `const fn`s, build AST values, and read `type_of`.

### 3.2 When expansion happens

**After parsing, before resolution**, in one pass over the tree, innermost first.

- The parser produces a tree containing macro *calls*, which it can do without knowing what
  any macro means because `name!(…)` is unambiguous.
- Expansion replaces each call with the tree the macro returned, then resolution runs on the
  result and sees no macros at all.
- **Expansion is bounded.** A macro that expands to a call to itself is caught by a depth
  limit, reported as a resolution error naming the macro and the chain — not by exhausting
  the evaluator's step budget, which would name the wrong thing.
- **A macro cannot see the resolver's answers**, and so cannot ask what type an argument
  expression has. That is the price of expanding first, it is the same price Rust pays, and
  the alternative — interleaving expansion with resolution — is a loop with no fixed point.
  `type_of` (§5) works on a *type* named in the source, which is why it is a separate
  feature and not a method on `Expr`.

### 3.3 `quote` and `unquote`

- **`quote { … }` is an AST literal.** Its contents are parsed, not evaluated, and the value
  is an `Expr`, `Stmt`, or `Block` depending on what was written.
- **`unquote(e)` splices**, and `e` must already be an AST value. It is the only thing
  evaluated inside a `quote`.
- **`unquote` of a list splices in sequence**, which is how a pack parameter is consumed:
  `quote { f(unquote(args)) }` produces one call with every argument.

---

## 4. Hygiene

An unhygienic macro is a macro whose expansions break when the call site happens to use a
name the macro also uses. Quince's answer:

- **A name introduced by an expansion is distinct from any name at the call site**, even
  when spelled identically. `assert_eq!`'s `let l` cannot collide with a caller's `l`, and a
  caller cannot read the macro's `l` either.
- **A name a macro *receives* keeps its call-site meaning.** `unquote(left)` resolves where
  it was written, so `assert_eq!(count, 0)` sees the caller's `count` even if the macro body
  declares one.
- **This is implemented as a syntax context on `Ident`**, set when the expansion produces a
  name and compared alongside the string during resolution. Two identifiers are the same
  binding when their text *and* their context agree.
- **Globals are the exception**, deliberately. A macro body calling `print` or `Error` gets
  the call site's binding, because a macro that could not use the standard library without
  qualifying every name would not be usable. The cost is that a caller who shadows `Error`
  changes what `assert_eq!` throws, and the mitigation is that DESIGN.md already refuses to
  let a program shadow a *type* name.

### 4.1 What a macro may not do

- **Introduce a binding the caller can see.** A macro expanding to `let x = 1` gives the
  caller nothing named `x` — that is hygiene working, not a bug, and a macro that wants to
  bind a caller-visible name takes an `Ident` parameter and uses it.
- **Declare a class, interface, enum, or module.** Declaration macros are deferred in §7:
  they interact with resolution's declaration-collecting pass, which runs before expansion
  would have produced them.
- **Escape the compile-time evaluator's limits.** No I/O, no printing, and the step budget
  applies.

### 4.2 Visibility and import

A `public macro` is exported like any other top-level declaration (v0.7 §3.6), and
`import`ing a module makes its macros callable. A macro is *not* a value: it cannot be
assigned, passed, or stored, because it does not exist after expansion.

### 4.3 Diagnostics through an expansion

This is the part that decides whether the feature is usable.

- **Every node a macro produces carries a span**, and it is the span of the `quote` text it
  came from — inside the macro — not the call site.
- **A diagnostic inside an expansion shows both**: the error at its span in the macro body,
  and a second location line naming the call that expanded it. This is the shape rustc uses
  and it is the only one that answers both questions a reader has.
- **`unquote`d nodes keep their original spans**, so an error in an argument points at the
  argument the caller wrote, with no mention of the macro at all. That is the common case
  and it should be the quiet one.

---

## 5. Type reflection (`type_of`)

```quince
let info = type_of(User)

print(info.name)                      # "User"
for field in info.fields {
    print(field.name, field.type_name, field.visibility)
}
for iface in info.interfaces {
    print(iface.name)
}
```

- **`type_of(T)` takes a type, not a value**, and answers a `TypeInfo` at compile time. For
  a value, `type(x)` already exists and answers a string.
- **`TypeInfo` carries** the name, the fields (name, type name, visibility), the method and
  `op` signatures, the interfaces implemented, the superclass if any, the type parameters,
  and — for an enum — the variants with their payload fields.
- **It is a compile-time value**, subject to v0.12 §3's freezability rule: it is built from
  strings and frozen containers and nothing else, which is what lets a macro read it.
- **A `TypeInfo` reachable at run time is refused**, for the same reason a `const fn` may not
  return an instance. If a program wants type metadata at run time, a macro generates the
  code that carries it — which is the whole reason these two features share a milestone.

---

## 6. Work items, in order

**Tranche 1 — AST values.** `Expr`, `Stmt`, `Block`, `Ident` as built-in types wrapping the
existing nodes, with `op string` so one can be printed. Nothing consumes them yet, and this
is the tranche that decides how much of the AST is exposed.

**Tranche 2 — `quote` and `unquote`.** Parsing a quoted region, splicing, and list splicing.

**Tranche 3 — `macro` declaration and expansion.** The expansion pass between parser and
resolver, `name!(…)` calls, kind checking, the depth limit.

**Tranche 4 — hygiene.** The syntax context on `Ident` and the resolver comparing it. Its
own tranche because it touches name resolution, and because it is the item most likely to be
wrong in a way tests written by its author do not catch.

**Tranche 5 — diagnostics through expansions.** §4.3. Deliberately not folded into tranche
3: a macro system whose errors are unreadable is one people stop using, and separating it
is what keeps it from being the thing that gets cut.

**Tranche 6 — `type_of`.** Independent of tranches 1–5 and useful without them.

**The cut line is after tranche 5.** `type_of` is severable; hygiene and diagnostics are not,
and a macro system shipped without either is worse than no macro system.

---

## 7. Deferred

**Repetition syntax.** Rust's `$(…),*`. Quince takes a pack parameter instead (§3.1), which
covers the common case — a variadic argument list — and does not cover matching structure
inside an argument. That is the largest thing given up here and it is given up deliberately:
a pattern language inside a macro is a second language to learn and a second grammar to keep.

**Declaration macros.** A macro expanding to a `class`, `enum`, or `interface`. §4.1 says
why: declarations are collected before expansion, and reordering those two passes is a change
to how resolution works rather than an addition to it.

**Attribute macros** — `@derive(Hashable)` on a declaration. The most-wanted thing after
declaration macros, and blocked on the same pass ordering.

**Run-time reflection.** §5 refuses it. A program that needs metadata at run time generates
it with a macro.

**A macro that asks for an argument's type.** §3.2 says why it cannot: expansion runs before
resolution, and interleaving them has no fixed point.

---

## 8. Decisions taken

- **Macros are called with `!`.** A reader must be able to see that arguments may not be
  evaluated. §3.1.
- **Expansion runs between parsing and resolution**, innermost first, so a macro's output is
  checked like any other code and a macro cannot ask about types. §3.2.
- **Macros are hygienic, with globals exempt.** §4.
- **A macro is not a value.** §4.2.
- **A produced node's span is in the macro; an `unquote`d node's span is at the call.** §4.3.
- **Parameters and returns are typed by AST kind**, so a mistake is reported at the call.
  §3.1.
- **Variadics use v0.9's packs rather than a repetition syntax.** §3.1, §7.
- **`type_of` is compile-time only and takes a type**, where `type(x)` takes a value and
  stays a string. §5.
- **This is a milestone of its own**, not a third of one. Head of file.

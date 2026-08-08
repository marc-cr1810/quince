# Quince v0.8.1 — word logical operators, increments, and short-circuiting assignment

A point release on a closed milestone, and the only document here that was written *after*
its code. Both facts are irregular and both are deliberate, so they are stated first rather
than discovered.

**Why a point release.** Nothing here is a new capability. `a and b` computes what `a && b`
computed, `i++` counts what `i += 1` counted, and `k not in d` answers what `!(k in d)`
answered. What changes is how the language is *spelled*, and a spelling change earns a
version because it breaks source, not because it adds power. Every numbered milestone from
v0.9 on is a capability; putting this among them would misrepresent it and would cost seven
documents of renumbering to say something untrue.

**Why the document is late.** `roadmap/README.md` says a milestone's plan is written before
its code, and this one was not: the change arrived as a request against a finished v0.8, was
built, and is being written up now. The convention is right and this is a departure from it.
The cost is visible in §8, which is shorter than it would be — a plan written first is worth
most where it turned out wrong, and there is less of that to record when the plan is written
last. §7 is therefore the load-bearing section here, since the decisions are the part that
outlives the diff.

---

## 1. What this milestone adds

1. **`and`, `or`, and `not` replace `&&`, `||`, and `!`.** The symbolic forms are removed,
   not aliased. §3.1.
2. **`not in` and `is not`**, the negations of `in` and `is`. §3.2.
3. **`not` binds looser than the comparisons**, unlike the `!` it replaces. §3.3.
4. **`++` and `--`, as statements**, in both prefix and postfix spelling, meaning the same
   thing. §3.4.
5. **`and=`, `or=`, and `??=`**, the three assignments whose right side may not run. §3.5.

Items 1–3 are one change seen from three sides: once the operator is a word, the word has to
read as the word, which is what makes §3.2 worth having and §3.3 mandatory. Items 4 and 5 are
independent of them and of each other, and are here because they are operator surface and
nothing else claims it — the same reason v0.8 §3.7 carried `**`.

---

## 2. What v0.8 leaves in place

- **`ExprKind::Logical`, and its separation from `ExprKind::Binary`.** The short-circuit
  already had a node of its own. §3.1 changes which token reaches it and nothing else, which
  is why the whole of §3.1 is a lexer change and two rows of a precedence table.
- **`ExprKind::AssignOp`, and the rule that its target is evaluated once.** §3.4 desugars
  onto it and inherits that rule rather than restating it. §3.5 deliberately does *not*
  reuse it, for the reason §7 gives.
- **`ExprKind::Coalesce`.** `??` is unchanged; §3.5 adds its assignment form beside it.
- **`op bool`.** A class decides its own truthiness, and every operator here that asks a
  truth question asks through it — including `and=` and `or=`, which are new callers of a
  slot that already existed.
- **The bitwise operators.** `&`, `|`, `^`, `~`, `<<`, `>>` and their compound assignments
  are untouched. §3.1's whole argument is about what happens to `&` and `|` when the pair
  they were half of stops existing.
- **`is` narrows.** v0.7's smart cast is unchanged, and §3.2 is careful not to extend it.

### 2.1 New tokens and keywords

| Token / Keyword | Status | Purpose |
| :--- | :--- | :--- |
| `and` | **new keyword** | short-circuit conjunction, replacing `&&`. §3.1 |
| `or` | **new keyword** | short-circuit disjunction, replacing `\|\|`. §3.1 |
| `not` | **new keyword** | negation, replacing `!`; first word of `not in`. §3.1, §3.2 |
| `&&` `\|\|` | **removed** | — |
| `!` | **removed as an operator** | survives only inside `!=`. §3.1 |
| `++` `--` | **new tokens** | increment and decrement statements. §3.4 |
| `and=` `or=` | **new tokens** | short-circuiting assignment. §3.5 |
| `??=` | **new token** | nil-coalescing assignment. §3.5 |

`and`, `or`, and `not` are reserved. None appeared as an identifier in the corpus, and the
prefixes that look like them — `android`, `ordinal`, `nothing` — are unaffected, since the
lexer matches whole words. That is pinned by a test rather than asserted here.

---

## 3. Syntax

### 3.1 The logical operators are words

```quince
let ready = loaded and not failed
let name = supplied or "anonymous"
```

They join `is` and `in`, which the language already read as words in operator position. That
is the weaker half of the argument. The stronger half is what leaving does to the characters
left behind:

```quince
let mask = flags & 0x0F        # bitwise, and now unambiguously so
```

While `&&` and `&` both existed, `a & b` where `a && b` was meant was a bug the language
could not see: both are legal, both produce a value, and the wrong one is wrong only
sometimes. Removing `&&` removes the mistake rather than diagnosing it. The lexer comment
that used to apologise for the pair — `&` was once refused outright with "did you mean
`&&`?" — is gone with them.

Removal rather than aliasing, because two spellings of one operator is the redundancy this
codebase argues against everywhere else: `extend` exists so as not to be a pun on `extends`,
the visibility words are spelled out rather than abbreviated, and `TokenKind::AssignOp` is
one variant rather than thirteen. A language that had already paid those costs to keep one
spelling per idea would not then keep two for conjunction.

**`!` survives only inside `!=`.** Written alone it is a lex error naming `not`, rather than
a token the parser has no rule for — the report should underline the character, not whatever
followed it. This also leaves `!` free for the postfix macro-call position v0.15 wants, which
was a contended character and is now an unused one.

### 3.2 `not in` and `is not`

```quince
if "carol" not in scores { }
if value is not string { }
```

Both are two-token forms recognised in the precedence climb, and both desugar to a `Not` over
the node that already exists — `not (a in b)` and `not (a is T)`. There is no `BinaryOp::NotIn`
and no negated `Is`, so every pass that understood `in` and `is` understands these without
being told: the resolver, the checker, the inferencer, the evaluator, and the four LSP
walkers needed no change between them.

`not` is a prefix operator everywhere else, so the parser reads it as the first half of
`not in` only when an `in` follows *and* an operand is already in hand. A prefix `not` cannot
be in that position, so the two readings never compete.

**`is not` does not narrow.** `if v is string` narrows `v` inside the block; `if v is not
string` narrows nothing. That is not an omission. What a failed type test proves is a fact
about the *other* branch, and the inference pass narrows the block a condition guards rather
than the one it skips — so the honest answer is to decline. The desugaring gives this for
free: the narrowing check matches `ExprKind::Is` and a `Not` wrapped around one does not
match. Worth knowing before writing `is not` in a guard and expecting the cast.

### 3.3 The precedence of `not`

`not` is the one unary operator that binds *looser* than the comparisons — above `and` and
`or`, below everything else. `-` and `~` stay where they were.

```quince
not a in b      # not (a in b)
not a == b      # not (a == b)
not a and b     # (not a) and b
```

`!` sat with the other unary operators, where C puts it and where `!a == b` means
`(!a) == b`. That is defensible for a symbol: `!` is punctuation, it visibly attaches to what
follows it, and C programmers have the grouping memorised. It is indefensible for a word.
Nobody reads "not a in b" as asking whether the negation of `a` is in `b`, and an operator
that is spelled to be read has to group the way it reads. Python resolves it the same way and
for the same reason.

This is the one place where making the operators words forced a change to what a program
means, rather than only to how it is written. It is called out here because it is the change
most likely to surprise someone porting code, and because §3.2 depends on it: without it,
`not a in b` and `a not in b` would be different questions, and having two spellings that
disagree would be worse than having one.

### 3.4 `++` and `--` are statements

```quince
i++        # all four
++i        # mean
i--        # exactly
--i        # `i += 1` or `i -= 1`
```

They produce no value. That is the decision, and everything else follows from it: with
nothing to evaluate to, prefix and postfix have nothing left to differ *about*, so the two
spellings collapse into one meaning and `x = i++` — the form that makes C's version a
recurring bug — is a syntax error rather than a puzzle.

Both forms desugar to `ExprKind::AssignOp` with a synthesized `1`. No new node, and the
target is evaluated exactly once for free, because that is the rule the compound assignment
already carries: `d[key()]++` calls `key` a single time.

Statement-hood is enforced structurally rather than by a check. The statement dispatcher
takes every `++` that opens a statement, and the expression-statement path takes every one
that ends a statement; a `++` anywhere else is never consumed, so it is still sitting there
when the enclosing form asks for its `)` or its `{`. All four failure sites — a prefix in
operand position, and the three ways a postfix can be left over — report the same refusal.

The lexical cost is the one C pays: `--` munches maximally, so `a - -b` needs its space, and
`a--b` is a decrement of `a` followed by a stray `b`.

**`i += 1` still exists and is still the way to write it inside an expression.** This form is
ergonomics, not capability, which is why refusing it in expression position costs nothing.

### 3.5 `and=`, `or=`, and `??=`

```quince
count ??= expensive()      # assigns only when count is nil — and only then calls
flag and= still_valid()    # assigns only when flag is truthy
name or= fallback()        # assigns only when name is falsy
```

Written like compound assignments and deliberately not implemented as ones. `a op= b` for a
`BinaryOp` means `a = a op b`: always combine, always write. These three read the target
first, and what they find decides whether the right side is evaluated at all and whether
anything is written. `count ??= expensive()` not calling `expensive` is the entire point of
the form, and a node shared with `+=` would make eager evaluation the natural implementation
and the correct one the special case.

So they are `ExprKind::AssignShort` carrying a `ShortAssignOp`, kept apart from `AssignOp`
for exactly the reason `LogicalOp` is kept apart from `BinaryOp` — a separation this codebase
already made once, for this hazard, one layer down.

One node for the three rather than three, because there is one rule: read the target, test
what was found, and assign only if the test says the target has not already answered. The
three differ only in the test — falsy, truthy, `nil`.

**Not assigning is not the same as assigning what is already there.** When the form
short-circuits it writes nothing at all, rather than writing the value back over itself. The
difference is invisible on a plain local and is not invisible on a `final` field or through a
declared type, where a redundant write would be checked and could be refused. The evaluator
distinguishes them explicitly.

The target is still evaluated exactly once, assigning or not: `d[f()] ??= 0` calls `f` a
single time on both paths.

`and=` and `or=` are the only compound assignments spelled with letters, so they are munched
in the lexer's identifier path rather than its symbol path. The adjacency rule is the one the
symbol path uses: `and=` is the assignment, `and =` is not, and `and==` is a word followed by
a comparison.

---

## 4. Enforcement

**At lexing:**
- A bare `!` outside `!=`, refused with a pointer to `not`. §3.1.

**At parsing:**
- `++` or `--` reached where an operand is expected, or left over after an expression that
  is not a whole statement. §3.4.
- `++` or `--` applied to something that is not a name, an index, or a field. §3.4.
- A short-circuiting assignment to a target that is not assignable, refused exactly where
  `=` and `+=` are. §3.5.

**At resolution:**
- `n++` on a `final` or `const` binding, refused because it *is* a compound assignment and
  inherits v0.8 §3.7's rule unchanged. No new check.

**At run time:**
- `op bool` on the target of an `and=` or `or=`. §3.5.
- The declared type of a target that a short-circuiting assignment actually writes, checked
  by the assignment path that was already there.

Nothing here is a new *kind* of enforcement. Every rule above is either a lexical refusal or
a reuse of a check v0.7 or v0.8 installed, which is the clearest evidence available that this
milestone adds spelling rather than semantics.

---

## 5. Work items, in order

**Tranche 1 — the word operators.** `and`, `or`, `not` reserved and lexed; `&&`, `||`, and
prefix `!` removed; the two rows of the precedence table repointed. Largest blast radius,
smallest conceptual content, and everything else is easier once the corpus has stopped using
the old spellings.

**Tranche 2 — `not`'s precedence.** Separated from tranche 1 because it is the only part that
changes what an existing program *means*, and it should be possible to see that alone in a
diff.

**Tranche 3 — `not in` and `is not`.** Needs tranche 1's tokens and tranche 2's binding power
to agree with the prefix form.

**Tranche 4 — `++` and `--`.** Independent of 1–3. Parser and lexer only; no new AST.

**Tranche 5 — `and=`, `or=`, `??=`.** The only tranche with a new AST node, and so the only
one that touches the resolver, the checker, the inferencer, the evaluator, and the LSP
walkers. Last, because it is the one that can be cut.

**The cut line is after tranche 4.** Tranches 1–3 are one change and cannot be split; tranche
4 is small and self-contained; tranche 5 is the one that would have waited.

---

## 6. Deferred

**`not` as an overloadable op.** A class answers `not x` through `op bool`, as it always did.
A separate negation slot would let a class disagree with its own truthiness, which is a way
to write a bug and not a way to write a type.

**Chained comparison.** `a < b < c` reads as mathematics and means something else in most
languages that allow it. It belongs with a document that can weigh it properly.

**`xor`.** `^` is the bitwise operator and there is no short-circuiting version to want, since
exclusive-or must evaluate both sides. A word for it would be a word for `!=` on bools.

**Custom prefix and postfix operators.** v0.12 §"Custom prefix and postfix operators" already
refuses these for infix-only registration. Nothing here changes that answer.

---

## 7. Decisions taken

- **The symbolic forms are removed, not aliased.** Two spellings of one operator is the
  redundancy this codebase argues against everywhere else. §3.1.
- **The point of removing `&&` and `||` is what it does to `&` and `|`.** Retiring the pair
  retires the typo class; a diagnostic could only have described it. §3.1.
- **`!` is refused at the lexer, not carried as a token nothing accepts.** The report should
  underline the character. It also frees the position v0.15 wants. §3.1.
- **`not` binds looser than the comparisons.** The one semantic change in the milestone, and
  the one that makes a word-spelled operator group the way it reads. §3.3.
- **`not in` and `is not` desugar rather than getting nodes.** Every later pass then handles
  them without knowing they exist. §3.2.
- **`is not` does not narrow.** What a failed type test proves is about the other branch, and
  the inference pass does not narrow the branch a condition skips. §3.2.
- **`++` and `--` produce no value.** Which is what collapses prefix and postfix into one
  meaning, and what makes `x = i++` an error instead of a decision. §3.4.
- **They desugar to `AssignOp` rather than getting a node.** Evaluate-once comes for free,
  and so does every existing refusal about writing to a `final`. §3.4.
- **Statement-hood is structural, not a check.** The two statement paths consume every legal
  `++`; an illegal one is simply never consumed. §3.4.
- **The short-circuiting assignments do *not* reuse `AssignOp`.** `a op= b` is `a = a op b`
  for a `BinaryOp` and is not that for these three. Sharing the node would make eager
  evaluation the easy implementation. §3.5.
- **One node for all three of them.** One rule — read, test, maybe assign — and three tests.
  §3.5.
- **A short-circuited assignment writes nothing**, rather than writing the existing value
  back. Distinguishable through a declared type or a `final` field. §3.5.
- **This is a point release, not a milestone.** Nothing here is a capability the language
  lacked; `a and b`, `i++`, and `k not in d` each compute what an existing form computed. A
  spelling change earns a version because it breaks source, not because it adds power. See
  the head of this file.

---

## 8. What shipped, and where it differs from the above

All five tranches landed; the cut line after tranche 4 was not needed. This section is thin
for the reason the head of this file gives — the document was written after the code, so
there was no standing prediction for the code to depart from. What follows is the part worth
recording anyway.

**The precedence table in `grammar.md` was wrong before this milestone touched it, in three
places.** It had `??` as the loosest operator above assignment, when `COALESCE_BP` puts it
*tighter* than every comparison; it had `in` filed with `==` and `!=`, when it binds with the
relational operators; and it listed `is` on a level of its own above the comparisons, when it
shares theirs exactly. The table was rewritten against the implementation rather than
extended, so the rows this milestone changed and the rows it merely corrected are mixed
together in one diff. Whether the table or the code was the original intent is not something
this milestone established, and it is worth someone deciding deliberately.

**`not`'s precedence was not part of the original request.** The request was to replace the
symbols with words; the placement of `not` came out of a test asserting the old grouping and
failing to read sensibly once the operator had a name. The change is in §3.3 and §7 as though
it had been planned, which it was not — it was discovered.

**Nothing downstream of the parser changed for items 1–4.** The resolver, checker,
inferencer, evaluator, and LSP walkers were touched only by tranche 5's new node. That was
the expectation and it is worth confirming, because it is the whole argument for desugaring
in §3.2 and §3.4.

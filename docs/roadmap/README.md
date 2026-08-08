# Quince — design and roadmap

Two things live here, and they are read for different reasons.

**`DESIGN.md` is the record.** It is the decision journal for the language as it exists —
why the object model is an arena of handles, what the collector's root set is and what it
cost to get right, how dispatch works, what `extend` may not do, and why. Where a prediction
in it turned out to be wrong, the correction sits beside the prediction rather than replacing
it, because the mistake is usually the useful part. Its **Roadmap** section is the index of
everything below.

**Everything else is a plan.** One document per milestone, written before the code, to the
shape `DESIGN.md`'s better sections established: what the milestone adds, what earlier
milestones leave in place, the syntax, what is enforced and where, the work in tranches with
a stated cut line, what is deferred and why, and the decisions taken — recorded because
someone will want to reverse one.

---

## Language milestones

| | Milestone | Document |
| --- | --- | --- |
| v0.1–v0.6 | walking skeleton → modules, library, inference | `DESIGN.md` § Roadmap |
| v0.7 | done — annotations, `T?`, `list[T]`, visibility, LSP | `V0_7_TYPE_SYSTEM_DESIGN.md` |
| **v0.8** | **done** — declaration modifiers, overloading, defaults, `**` | `V0_8_DECLARATIONS_AND_DISPATCH_DESIGN.md` |
| **v0.8.1** | **done** — `and`/`or`/`not`, `not in`, `is not`, `++`/`--`, `??=` | `V0_8_1_WORD_OPERATORS_DESIGN.md` |
| v0.9 | user generics, `tuple`, packs, function types | `V0_9_GENERICS_DESIGN.md` |
| v0.10 | enums, `match`, `Option`/`Result`, `range`, containers | `V0_10_ENUMS_AND_MATCHING_DESIGN.md` |
| v0.11 | interfaces, `op hash`, generic functions | `V0_11_INTERFACES_AND_SUBTYPING_DESIGN.md` |
| v0.12 | compile-time evaluation, custom infix operators | `V0_12_CTFE_AND_CUSTOM_OPERATORS_DESIGN.md` |
| v0.13 | the standard library a compiler needs, import aliasing | `V0_13_STANDARD_LIBRARY_DESIGN.md` |
| v0.14 | string interpolation, typed `catch` | `V0_14_INTERPOLATION_AND_TYPED_CATCH_DESIGN.md` |
| v0.15 | hygienic macros, type reflection | `V0_15_MACROS_AND_REFLECTION_DESIGN.md` |

v0.7 through v0.10 began as a single document and are four milestones; each says so at its
head, and v0.7 §9 records the test that split them.

**v0.8.1 is a point release and not a milestone**, which is why it is numbered the way it is.
Every other row adds a capability; that one only changes how existing capabilities are
spelled — `and` computes what `&&` computed — and it earns a version because it breaks
source rather than because it adds power. It is also the one document here written *after*
its code, which its head states rather than hides.

A finished milestone keeps its plan and gains a section at the end saying where the code
and the plan differ — v0.8 §8 is the first. The plan is not rewritten to match: a prediction
that turned out wrong is worth more beside its correction than deleted.

## Execution engine

The language is one sequence and how it runs is another. `BYTECODE_VM_DESIGN.md` is the
architecture — bytecode IR, Cranelift JIT and AOT, NaN tagging, inline caching, FFI,
self-hosting — and each phase below refines one part of it.

| Phase | Milestone | Document |
| --- | --- | --- |
| 1A | Bytecode IR emitter, `.qnc`, disassembler, caching | `PHASE_1A_BYTECODE_IR_EMITTER_DESIGN.md` |
| 1B | Cranelift JIT core | `PHASE_1B_CRANELIFT_JIT_CORE_DESIGN.md` |
| 2 | NaN tagging and the GC rework | `PHASE_2_NAN_TAGGING_AND_GC_DESIGN.md` |
| 3 | Inline caching and executable bundling | `PHASE_3_INLINE_CACHING_AND_BUNDLING_DESIGN.md` |
| 4A | Cranelift AOT and `.qnx` native modules | `PHASE_4A_CRANELIFT_AOT_AND_QNX_DESIGN.md` |
| 4B | C FFI and the CPython bridge | `PHASE_4B_C_FFI_AND_CPYTHON_BRIDGE_DESIGN.md` |
| 5 | Self-hosting bootstrap | `PHASE_5_SELF_HOSTING_BOOTSTRAP_DESIGN.md` |
| 6A | GIL-free concurrency | `PHASE_6A_GIL_FREE_CONCURRENCY_DESIGN.md` |
| 6B | Hardware SIMD | `PHASE_6B_HARDWARE_SIMD_DESIGN.md` |

**`BYTECODE_VM_DESIGN.md` §12 is the table to read before starting any phase.** Several
phases assume language features that no milestone above schedules, and it lists which have
since been placed and which are still unowned. A phase may specify how a feature is compiled;
it may not be the only place that feature is specified.

---

## What a milestone document owes the reader

Not house style for its own sake — each of these exists because a document that skipped it
turned out to be unimplementable:

- **What it adds**, numbered, and matching the sections below it. A feature in the body and
  not in the list is a feature nobody sequenced.
- **What earlier milestones leave in place**, so a rule is inherited by citation rather than
  restated slightly differently.
- **A new-token table**, because a keyword reserved without being noticed is how the
  TextMate grammar drifted three words (DESIGN.md, v0.6).
- **Enforcement, split into resolution and run time.** Deciding *where* a rule is checked is
  most of designing it.
- **Work items in tranches, with a stated cut line** — or a stated argument that there is
  none, which is what v0.9 §7 does.
- **Deferred, with reasons.** A deferral whose reason is written down is the thing that
  stops the feature being re-argued from scratch.
- **Decisions taken**, recorded because someone will want to reverse one and the reason is
  the useful half.

**Every example is meant to run.** v0.7 §9 records `op init`, not `fn init` as a decision
found by discovering that the first draft's example did not — which is the argument for the
rule.

**Cross-references are by section number** (`v0.8 §3.5`), and a claim about another milestone
should cite one. Three contradictions between these documents were found by following
citations that turned out not to say what the citing document claimed.

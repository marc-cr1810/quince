//! Everything decided about a program before it runs.
//!
//! Two passes over the same tree, answering two different questions.
//! [`resolve`] answers *where a name lives* — it rewrites every reference into a
//! slot or marks it global, and it fails, because a name that does not resolve is
//! a program that cannot run. [`infer`] answers *what a name holds*, and it does
//! not fail: `Unknown` is an answer, and most of a dynamically typed program is
//! `Unknown`.
//!
//! Beside them sit the two things both produce. [`types`] is the answer's
//! vocabulary — what a `Type` is, and the lookups that read one off a builtin
//! table — and [`symbols`] is the editor's view of a name, which the language
//! server and the REPL both render and neither should have to derive.
//!
//! This is the directory the next three milestones grow into, and the split is
//! for their benefit. v0.7 gives `Type` parameters, which is a change to
//! [`types`] and not to the walk that fills it; its visibility checks and v0.8's
//! modifier rules — `const fn` purity, `override`, overload ambiguity — are arms
//! in `resolve::walk`; v0.9's bounds are one more matching rule in [`types`].
//! Every one of them is a static check, and each belongs beside the two passes
//! that already are one.

pub mod infer;
pub mod resolve;
pub mod symbols;
pub mod types;

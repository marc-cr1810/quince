//! The library, implemented in Rust.
//!
//! Two halves. The [`Native`](crate::runtime::value::Native)s in this directory
//! are what a program reaches without importing anything — the three globals, and
//! the methods seeded onto each builtin type; [`stdlib`] holds the modules that
//! must be imported by name.
//!
//! Split by receiver, one file per type, because that is how a method is found
//! and how one is added. [`types`] is the registry that says which native answers
//! which name on which type, and it is the only file that has to be touched when
//! a method moves.
//!
//! v0.10 adds `set`, `array`, `bytes`, and `range`, and each is a file here beside
//! its representation in [`crate::runtime`].

pub mod convert;
pub mod dict;
pub mod globals;
pub mod list;
pub mod stdlib;
pub mod string;
pub mod types;

pub use globals::BUILTINS;

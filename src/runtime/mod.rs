//! The object model: what a value is, and where it lives.
//!
//! Everything below the evaluator. [`value`] is the tagged union a program
//! computes with, [`heap`] the arena those handles point into, [`class`] the
//! type every value belongs to and the table its behaviour hangs off, [`env`](mod@env)
//! the scopes a name is looked up in, and [`dict`] the one container with rules
//! of its own about what may go in it.
//!
//! Grouped because this is the layer the later milestones widen rather than
//! reshape. v0.7 puts a reified type descriptor on every allocation that carries
//! type arguments — one field, read by `is` and by every container check — and
//! v0.10 adds `set`, `array`, `bytes`, `range`, and the enum instance as new
//! object kinds. Each is a file here and a variant in [`heap::Object`].
//!
//! The line against [`crate::interp`] is that nothing here evaluates anything. A
//! module in this directory may allocate, read, compare, and freeze; asking a
//! class to run its `op eq` is the evaluator's job, because only the evaluator
//! can call back into Quince.

pub mod class;
pub mod dict;
pub mod env;
pub mod heap;
pub mod value;

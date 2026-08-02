//! The modules a program can import without a file to import them from.
//!
//! A stdlib module is an ordinary [`Globals`] holding ordinary [`Native`]s — the
//! same object an imported file produces, built from a table instead of by
//! running statements. Nothing downstream can tell the two apart, which is the
//! property worth keeping: `math.floor` and `util.helper` are one lookup, and
//! `from math import floor` and `from util import helper` are one code path.
//!
//! **A member takes no receiver.** A module hands its names back unbound — see
//! the module arm of `Interp::attr` — so `math.floor(2.5)` calls `FLOOR` with
//! one argument and `arity` is `Some(1)`. This is the opposite of the natives in
//! the sibling files seeded onto a type, where `upper` is `Some(1)` because `args[0]`
//! is the string it upper-cases. The difference is real and it is the reason
//! these live in their own directory rather than beside those.
//!
//! Nothing here is bound until it is imported. That is the whole reason `import`
//! went in before the library did: a name in the global scope is a name a
//! program can never use again, and `floor`, `ceil`, `round`, `abs`, `sqrt`,
//! `pow`, `min` and `max` would have been eight of them for one domain.


pub mod io;
pub mod math;
pub mod random;
pub mod time;

pub use random::{DEFAULT_SEED, next_u64};


use crate::runtime::env::Globals;
use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::{Native, Value};

/// One module the language ships.
pub struct Module {
    pub name: &'static str,
    /// What the module declares. Every entry is immutable — a program may not
    /// reassign `math.pi`, for the same reason it may not reassign `print`.
    pub members: &'static [(&'static str, Member)],
}

/// A name a stdlib module declares.
pub enum Member {
    Fn(&'static Native),
    /// A constant, built each time a module is, because a `Value` cannot be a
    /// `const` when it may hold a handle. None do yet.
    Const(fn() -> Value),
}

/// Every module `import` can find without looking at the filesystem.
///
/// Also the list of names a file may not take: `import math` must not change
/// meaning because someone dropped a `math.qn` beside their program, so this
/// wins and the collision is reported. Small and fixed is what makes that a
/// reasonable rule rather than a trap.
pub static MODULES: &[&Module] =
    &[&math::MATH, &io::IO, &time::TIME, &random::RANDOM];

/// The stdlib module called `name`, if there is one.
pub fn module_named(name: &str) -> Option<&'static Module> {
    MODULES.iter().copied().find(|module| module.name == name)
}

/// Builds `module`'s scope in the heap.
///
/// Called once per module per interpreter; the caller caches the result, which
/// is what makes `import math` in two files the same object both times.
pub fn build(module: &Module, heap: &mut Heap) -> ObjId {
    let mut globals = Globals::module(module.name, None);
    for (name, member) in module.members {
        let value = match member {
            Member::Fn(native) => Value::Native(native),
            Member::Const(build) => build(),
        };
        globals.declare(*name, value, false);
    }
    heap.alloc(Object::Globals(globals))
}

// -- math --------------------------------------------------------------------

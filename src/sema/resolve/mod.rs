//! Static resolution of variable references.
//!
//! Runs between parsing and evaluation, rewriting every name into either a
//! `(hops, index)` pair addressing a slot in an enclosing scope, or `Global`.
//! Two things fall out of that:
//!
//! - Reading a local stops being a hash of a `String` against a chain of maps
//!   and becomes an index into a `Vec`.
//! - Mistakes that used to surface only when a line happened to run — assigning
//!   to a `const`, declaring the same name twice — are reported before the
//!   program starts.
//!
//! Globals stay dynamic. The REPL defines them a line at a time, and a program
//! is allowed to call a function declared further down the file, so neither can
//! be pinned to a slot.

mod walk;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::runtime::class::BUILTINS;
use crate::syntax::ast::{ImportNames, Slot, Stmt, StmtKind};
use crate::syntax::token::Span;

/// An error for a program that parses and still is not one.
///
/// Every error this stage raises is one of these: by the time the resolver runs
/// the grammar is satisfied, and what is left to get wrong is names — declaring
/// one twice, reading one that is not in scope yet, writing `self` where there
/// is no receiver. So the kind is applied here rather than at ten call sites.
pub(super) fn declaration(message: impl Into<String>, span: Span) -> Raised {
    QuinceError::new(message, span).with_kind(ErrorKind::Declaration)
}

/// Resolves a whole program in place.
pub fn resolve(program: &mut [Stmt]) -> Result<()> {
    let mut resolver = Resolver::default();
    // The top level has no scope, so `scoped` never runs over it — which is why
    // nothing used to register the classes declared there or look at the names
    // its bindings take. Slots are unaffected: `declare_slot` returns early
    // without a scope, because a global is bound by name at run time.
    resolver.declare_all(program, &[])?;
    resolver.stmts(program)
}

/// The builtin type called `name`, if there is one.
///
/// Read off the same list the globals are bound from, so a builtin type added
/// later is reserved without this file being touched. Hands back the static name
/// rather than the one passed in, so a caller can keep it past the borrow it
/// searched with.
pub(super) fn builtin_named(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .map(|builtin| builtin.name())
        .find(|builtin| *builtin == name)
}

/// Whether `name` is one of the types the language defines.
fn is_builtin_type(name: &str) -> bool {
    builtin_named(name).is_some()
}

#[derive(Debug)]
pub(super) struct Local {
    pub(super) index: u16,
    pub(super) mutable: bool,
}

#[derive(Default, Debug)]
pub(super) struct Scope {
    pub(super) names: HashMap<String, Local>,
    pub(super) count: u16,
}

/// The lexical scope stack. Empty means "at the top level", where every name is
/// a global.
#[derive(Default)]
pub(super) struct Resolver {
    pub(super) scopes: Vec<Scope>,
    /// Names that belong to a type, and so are not available to anything else.
    ///
    /// Flat rather than per-scope, and never popped: a type name means the type
    /// everywhere, which is what lets `int(5)` and a future `extend int` be read
    /// without asking what is in scope at that point. Over-broad for a class
    /// declared inside a function — its name is reserved for the rest of the
    /// program — and deliberately so, on the same reasoning as `declare`: an
    /// error can be relaxed later, a semantics cannot.
    pub(super) types: HashSet<String>,
    /// Names already declared at the top level, where there is no scope to hold
    /// them.
    ///
    /// A global is bound by name at run time and needs no slot, which is why
    /// `declare_slot` returns early up there — but "needs no slot" is not "may be
    /// declared twice", and for two years it read as though it were. Kept per
    /// resolver rather than per process, so the REPL still redefines a function
    /// freely: each entry is its own `compile`, and this set is empty again.
    pub(super) globals: HashSet<String>,
    /// What each class extends, by name. Flat and never popped, for the reason
    /// [`Resolver::types`] is.
    ///
    /// Enough to answer the one question this file asks of a hierarchy: whether a
    /// class descends from a builtin, and so whether its `op init` owes a
    /// `super.init`. Names rather than classes, because nothing has been
    /// evaluated yet.
    pub(super) parents: HashMap<String, String>,
    /// Whether the body being resolved is an `op init`. Cleared for a `fn` nested
    /// inside one, which could run at any time and so is not construction.
    pub(super) in_init: bool,
    /// How many `super.init(…)` calls the current `op init` body contains.
    ///
    /// A count of what is *written*, not of what runs — a call in each arm of an
    /// `if` is two. That is why the only thing decided from it here is whether
    /// there are none; the evaluator is what enforces that exactly one happens.
    pub(super) super_inits: usize,
}

impl Resolver {
    // -- scopes ------------------------------------------------------------

    /// Resolves `stmts` as a new scope, returning the number of slots it needs.
    ///
    /// Declarations are collected before anything is resolved, so a function
    /// can refer to a sibling declared after it — which is what makes mutually
    /// recursive nested functions work, and matches how the evaluator behaved
    /// when scopes were name-keyed maps.
    pub(super) fn scoped(
        &mut self,
        stmts: &mut [Stmt],
        predeclare: &[(String, bool, Span)],
    ) -> Result<u16> {
        self.scopes.push(Scope::default());
        let result = self
            .declare_all(stmts, predeclare)
            .and_then(|()| self.stmts(stmts));
        let scope = self.scopes.pop().expect("a scope was pushed above");
        result.map(|()| scope.count)
    }

    pub(super) fn declare_all(
        &mut self,
        stmts: &mut [Stmt],
        predeclare: &[(String, bool, Span)],
    ) -> Result<()> {
        // Classes first, so that a `let` stealing a type's name is refused
        // whichever order the two were written in. Without this pass the check
        // below would only catch the half of the mistake that happens to come
        // second in the file.
        for stmt in stmts.iter() {
            if let StmtKind::Class { name, parent, .. } = &stmt.kind {
                self.declare_type(name, stmt.span)?;
                // Recorded in the same pass, so a class can extend one declared
                // further down the file — which the evaluator refuses, but as an
                // undefined variable rather than as a chain this failed to see.
                if let Some(parent) = parent {
                    self.parents.insert(name.clone(), parent.name.clone());
                }
            }
        }

        for (name, mutable, span) in predeclare {
            self.declare(name, *mutable, *span)?;
        }
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Let { name, bind, .. } => {
                    self.declare(name, bind.mutable(), stmt.span)?
                }
                StmtKind::Fn { decl, .. } => self.declare(&decl.name, false, stmt.span)?,
                StmtKind::Class { name, .. } => self.declare_slot(name, false, stmt.span)?,
                // Through the same check as every other binding, so `import math`
                // twice, or beside a `let math`, is the one mistake that already
                // has a sentence for it. Immutable, like a `fn`: rebinding the
                // name would leave a module loaded and unreachable.
                StmtKind::Import { module, names, .. } => match names {
                    ImportNames::Module => self.declare(module, false, stmt.span)?,
                    ImportNames::Names(names) => {
                        for name in names {
                            // `from random import int` collides with the type
                            // called `int`, and the general refusal tells whoever
                            // wrote it to rename their variable — advice that
                            // cannot be taken, because the name belongs to the
                            // module. The qualified form is the way through and
                            // is what this says.
                            if is_builtin_type(&name.name) {
                                return Err(declaration(
                                    format!(
                                        "`{}` is the name of a type built into the language",
                                        name.name
                                    ),
                                    name.span,
                                )
                                .with_help(format!(
                                    "write `import {module}` and reach it as `{module}.{}`, \
                                     which takes no name",
                                    name.name
                                )));
                            }
                            self.declare(&name.name, false, name.span)?;
                        }
                    }
                },
                _ => {}
            }
        }
        Ok(())
    }

    /// Records that `name` belongs to a type, refusing the builtin type names.
    ///
    /// `class int { … }` is the mistake this catches. Shadowing a builtin type is
    /// refused for the same reason shadowing it with a `let` is: every mention of
    /// `int` after that line would mean something the language did not choose.
    pub(super) fn declare_type(&mut self, name: &str, span: Span) -> Result<()> {
        if is_builtin_type(name) {
            return Err(declaration(
                format!("`{name}` is a type built into the language"),
                span,
            )
            .with_help("pick another name for this class"));
        }
        self.types.insert(name.to_string());
        Ok(())
    }

    /// Reserves a slot for a binding, refusing one that would take a type's name.
    ///
    /// The check is here rather than in the evaluator because it is decidable
    /// without running anything, and because at the top level there is no slot to
    /// collide with — a global is bound by name, so `let string = "x"` used to
    /// replace the type silently and every later mention of `string` meant a
    /// string. That is the whole reason this exists.
    pub(super) fn declare(&mut self, name: &str, mutable: bool, span: Span) -> Result<()> {
        if is_builtin_type(name) || self.types.contains(name) {
            let what = match is_builtin_type(name) {
                true => "a type built into the language",
                false => "a class in this program",
            };
            return Err(
                declaration(format!("`{name}` is the name of {what}"), span).with_help(
                    format!(
                        "a type's name cannot also be a variable — rename this one, not `{name}`"
                    ),
                ),
            );
        }
        self.declare_slot(name, mutable, span)
    }

    /// Reserves a slot in the innermost scope, with no opinion about the name.
    ///
    /// Redeclaring a name in the same scope is an error rather than silent
    /// shadowing: with slots the two would be separate storage, so a closure
    /// made between them would quietly keep the older one. Shadowing across
    /// nested scopes is untouched. This is the restrictive choice on purpose —
    /// an error can be relaxed later, a semantics cannot.
    ///
    /// Called directly only for a `class`, whose name *is* the type and so
    /// cannot be refused for being one.
    pub(super) fn declare_slot(&mut self, name: &str, mutable: bool, span: Span) -> Result<()> {
        let Some(scope) = self.scopes.last_mut() else {
            // Top level: a global, bound by name at run time, so there is no slot
            // to reserve — but the same name declared twice is the same mistake
            // it is anywhere else, and the second still wins silently.
            if !self.globals.insert(name.to_string()) {
                return Err(declaration(
                    format!("`{name}` is already declared in this scope"),
                    span,
                ));
            }
            return Ok(());
        };
        if scope.names.contains_key(name) {
            return Err(declaration(
                format!("`{name}` is already declared in this scope"),
                span,
            ));
        }
        let index = scope.count;
        scope.count = index.checked_add(1).ok_or_else(|| {
            declaration("a scope may not declare more than 65535 names", span)
        })?;
        scope
            .names
            .insert(name.to_string(), Local { index, mutable });
        Ok(())
    }

    /// Walks outwards counting scopes, so the hop count matches the runtime
    /// parent chain.
    pub(super) fn find(&self, name: &str) -> Option<(Slot, bool)> {
        for (hops, scope) in self.scopes.iter().rev().enumerate() {
            if let Some(local) = scope.names.get(name) {
                let slot = Slot::Local {
                    hops: hops as u16,
                    index: local.index,
                };
                return Some((slot, local.mutable));
            }
        }
        None
    }

    pub(super) fn slot_of(&self, name: &str) -> Slot {
        self.find(name).map_or(Slot::Global, |(slot, _)| slot)
    }
}

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
use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::runtime::class::BUILTINS;
use crate::sema::overload;
use crate::syntax::ast::{FnDecl, ImportNames, Op, Slot, Stmt, StmtKind};
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
mod alias;

pub fn resolve(program: &mut [Stmt]) -> Result<()> {
    resolve_within(program, &Prior::default())
}

/// The same, told what an earlier compilation left bound at the top level.
///
/// One caller: the REPL, which compiles a line at a time into an interpreter
/// that is still holding everything the lines before it declared. Without this
/// each entry is resolved against an empty world, so a class declared on line
/// one is a class this pass cannot see on line two — and every rule that reads
/// a hierarchy quietly declines to answer.
///
/// The prior world is read off what is *bound*, not off accumulated text. A REPL
/// is not a file being appended to: a name may be redeclared, and a line that
/// raised half way through still bound what it got to. What is in the globals is
/// what the next line can actually reach, and it is the only honest answer.
pub fn resolve_within(program: &mut [Stmt], prior: &Prior) -> Result<()> {
    // Before anything reads a type, so no later pass ever sees an alias.
    alias::expand(program)?;
    let mut resolver = Resolver::default();
    resolver.seed(prior);
    // The top level has no scope, so `scoped` never runs over it — which is why
    // nothing used to register the classes declared there or look at the names
    // its bindings take. Slots are unaffected: `declare_slot` returns early
    // without a scope, because a global is bound by name at run time.
    resolver.declare_all(program, &[])?;
    resolver.stmts(program)
}

/// What an earlier compilation left bound at the top level.
///
/// Declarations rather than names, because every rule that wants this wants to
/// read one: `override` needs a superclass's members, overloading needs the
/// signatures already under a name, and default construction needs what a
/// constructor requires.
#[derive(Default)]
pub struct Prior {
    /// The declarations bound under each top-level name, which is more than one
    /// where the name is overloaded.
    pub functions: HashMap<String, Vec<Rc<FnDecl>>>,
    pub classes: HashMap<String, PriorClass>,
}

/// A class an earlier compilation bound.
pub struct PriorClass {
    pub parent: Option<String>,
    /// The methods the class's own body declared — not the ones it inherited,
    /// which belong to whichever class wrote them and are found by walking.
    pub methods: Vec<Rc<FnDecl>>,
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

/// Whether the builtin type `name` seeds a method called `member`.
///
/// The other half of what a class can inherit, and readable at resolution
/// because a seed table is a `static`: `class Email extends string` inherits
/// `upper` from a list written in Rust rather than from a class body this pass
/// walked, and `override` has to be required there for the same reason it is
/// required anywhere.
fn builtin_declares(name: &str, member: &str) -> bool {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name() == name)
        .is_some_and(|builtin| {
            builtin
                .seed()
                .methods
                .iter()
                .any(|(seeded, _)| *seeded == member)
        })
}

/// The fewest arguments any of these declarations' constructors needs.
///
/// `None` when none of them is a constructor, which is a class that inherits
/// whatever is above it — see [`Resolver::default_constructible`].
fn required_arguments(methods: &[Rc<FnDecl>]) -> Option<usize> {
    methods
        .iter()
        .filter(|decl| decl.op == Some(Op::Init))
        .map(|init| {
            init.params
                .iter()
                .filter(|param| !param.receiver && param.default.is_none())
                .count()
        })
        .min()
}

/// Refuses two declarations sharing a name that cannot be told apart.
///
/// `whose` names what they share — a class, an `extend` block, a scope — and is
/// what the report says they collided in. Every pair is compared rather than
/// each against the one before it, because a defaulted parameter makes a
/// declaration several signatures and the collision may be with any of them.
pub(super) fn refuse_clashes(decls: &[&Rc<FnDecl>], name: &str, whose: &str) -> Result<()> {
    for (index, later) in decls.iter().enumerate() {
        for earlier in &decls[..index] {
            let Some(clash) = overload::clash(earlier, later) else {
                continue;
            };
            return Err(
                declaration(clash.describe(name, whose), later.name_span)
                    .with_help(clash.help()),
            );
        }
    }
    Ok(())
}

/// What the superclass chain has to say about a member a subclass declared.
///
/// Three answers and not two, because "nothing declares it" and "nothing here
/// can tell" have to be acted on differently: the first refuses a stray
/// `override`, and the second must not. A parent imported from another module is
/// the case — `extends` names a binding, and this pass has evaluated nothing —
/// so the check is allowed to miss an override that should have been written and
/// is never allowed to accuse one that was.
pub(super) enum Inherited {
    /// A superclass declares it, and what its declaration said.
    Found { owner: String, member: Member },
    /// The chain is known all the way up and nothing in it declares this.
    Absent,
    /// Some class in the chain is not one this pass can see.
    Unknown,
}

/// What a class's declaration of one method said about it.
///
/// The two words a *later* declaration has to read: `final`, which forbids a
/// subclass replacing it, and `const`, which is what a `const` method is allowed
/// to call. Both are properties of the declaration rather than of the body, so
/// they are recorded when the class is collected and never revisited.
#[derive(Clone, Copy, Debug)]
pub(super) struct Member {
    pub(super) guarded: bool,
    pub(super) constant: bool,
}

/// The `const fn` currently being resolved, if the walk is inside one.
///
/// Held rather than passed because the check is asked at two unrelated
/// expression arms — an assignment and a call — several layers below the
/// declaration that set it.
#[derive(Clone, Debug)]
pub(super) struct Constness {
    /// How deep the scope stack was when the body was entered.
    ///
    /// What separates "a local this function made" from "something that was
    /// already there". A name bound at or beyond this depth is the function's
    /// own and may be reassigned; anything shallower is state the caller can
    /// see, which is exactly what `const` promises not to touch.
    pub(super) base: usize,
    /// How the declaration reads in a report — `const fn distance_to`.
    pub(super) named: String,
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
    /// The methods each class declares, and whether the declaration wrote
    /// `final` in front of one. Flat and never popped, for the reason
    /// [`Resolver::parents`] is.
    ///
    /// Methods only. A field is not a member a subclass *overrides* — the
    /// evaluator lets one shadow a parent's field and reads the nearer
    /// declaration, which is a different rule with a different reason — so
    /// requiring `override` on a redeclared field would be inventing one.
    pub(super) members: HashMap<String, HashMap<String, Member>>,
    /// The class whose body is being resolved, for the one question a method's
    /// own class is the answer to: whether `self.m(…)` reaches a `const` method.
    pub(super) in_class: Option<String>,
    /// The type parameters of the class whose body is being resolved — v0.9
    /// §3.1's "`T` in scope in the body".
    ///
    /// Saved and restored around a body rather than pushed onto a stack, the
    /// way [`Resolver::in_class`] is, and for the same reason: a class declared
    /// inside a method of another must leave the outer one's parameters in
    /// place when it is done.
    ///
    /// Not folded into [`Resolver::types`], which is flat and never popped. A
    /// type parameter is the one type name in the language with a *scope*, and
    /// reserving `T` for the rest of the program because one class used it is
    /// exactly the over-broadness that field admits to and this one cannot
    /// afford.
    pub(super) type_params: Vec<String>,
    /// How many parameters each class's own `op init` requires, for the classes
    /// that declare one. Flat and never popped, as [`Resolver::parents`] is.
    ///
    /// Absent means the class declares no constructor and inherits whatever is
    /// above it, which is what makes this a chain walk rather than a lookup —
    /// see [`Resolver::default_constructible`].
    pub(super) constructors: HashMap<String, usize>,
    /// The enclosing `const fn`, if there is one. See [`Constness`].
    pub(super) constness: Option<Constness>,
    /// Declarations an earlier compilation bound under each top-level name.
    /// Empty for a file, which is compiled all at once. See [`Prior`].
    pub(super) prior: HashMap<String, Vec<Rc<FnDecl>>>,
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
            if let StmtKind::Class {
                name,
                parent,
                methods,
                ..
            } = &stmt.kind
            {
                self.declare_type(name, stmt.span)?;
                // Recorded in the same pass, so a class can extend one declared
                // further down the file — which the evaluator refuses, but as an
                // undefined variable rather than as a chain this failed to see.
                if let Some(parent) = parent {
                    self.parents.insert(name.clone(), parent.name.clone());
                }
                // And its members, for the same reason: `override` is a claim
                // about a class that may be written below this one.
                self.members.insert(
                    name.clone(),
                    methods
                        .iter()
                        .map(|decl| {
                            (
                                decl.name.clone(),
                                Member {
                                    guarded: decl.guarded,
                                    constant: decl.constant,
                                },
                            )
                        })
                        .collect(),
                );
                // The fewest arguments any of the class's constructors needs.
                // "What a call has to supply": the receiver is not the program's
                // to write, a defaulted parameter is one the call may leave out,
                // and a class declaring `op init(n: int)` beside `op init()` is
                // default-constructible on the strength of the second.
                if let Some(required) = required_arguments(methods) {
                    self.constructors.insert(name.clone(), required);
                }
            }
        }

        for (name, mutable, span) in predeclare {
            self.declare(name, *mutable, *span)?;
        }
        // Which `fn` names this scope declares more than once, and whether the
        // several declarations may stand together. Done before the loop below,
        // because the answer is about the *set* and the loop sees one at a time.
        let overloaded = self.overloaded(stmts)?;
        let mut bound: HashSet<String> = HashSet::new();
        for stmt in stmts {
            match &mut stmt.kind {
                StmtKind::Let { name, bind, .. } => {
                    self.declare(name, bind.mutable(), stmt.span)?
                }
                StmtKind::Fn { decl, overload, .. } => {
                    // The first declaration of a name binds it; the rest join
                    // what it bound. Only one slot is reserved, which is what
                    // makes them one name holding several declarations rather
                    // than several names.
                    *overload =
                        overloaded.contains(&decl.name) && !bound.insert(decl.name.clone());
                    if !*overload {
                        self.refuse_prior_ambiguity(decl)?;
                        self.declare(&decl.name, false, stmt.span)?;
                    }
                }
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

    /// Records what an earlier compilation bound, so this one can read it.
    ///
    /// Deliberately *not* `globals`: that set is what refuses a name declared
    /// twice, and a REPL entry redeclaring one is the ordinary thing to do. What
    /// is seeded is only what the declaration rules read.
    fn seed(&mut self, prior: &Prior) {
        for (name, decls) in &prior.functions {
            self.prior.insert(name.clone(), decls.clone());
        }
        for (name, class) in &prior.classes {
            self.types.insert(name.clone());
            if let Some(parent) = &class.parent {
                self.parents.insert(name.clone(), parent.clone());
            }
            self.members.insert(
                name.clone(),
                class
                    .methods
                    .iter()
                    .map(|decl| {
                        (
                            decl.name.clone(),
                            Member {
                                guarded: decl.guarded,
                                constant: decl.constant,
                            },
                        )
                    })
                    .collect(),
            );
            if let Some(required) = required_arguments(&class.methods) {
                self.constructors.insert(name.clone(), required);
            }
        }
    }

    /// Refuses a declaration an earlier compilation's cannot be told apart from.
    ///
    /// A *duplicate* signature is allowed and is the reason this is not simply
    /// `refuse_clashes`: retyping a declaration to change it is what a REPL is
    /// for, and the evaluator replaces the signature it matches. An ambiguity is
    /// not a redefinition of anything — it is a second declaration that some
    /// call would reach equally well — so it is refused here exactly as it is
    /// inside one compilation.
    fn refuse_prior_ambiguity(&self, decl: &Rc<FnDecl>) -> Result<()> {
        let Some(earlier) = self.prior.get(&decl.name) else {
            return Ok(());
        };
        for bound in earlier {
            if let Some(clash @ overload::Clash::Ambiguous { .. }) = overload::clash(bound, decl) {
                return Err(declaration(
                    clash.describe(&decl.name, "an earlier entry"),
                    decl.name_span,
                )
                .with_help(clash.help()));
            }
        }
        Ok(())
    }

    /// The `fn` names this statement list declares more than once, having
    /// checked that the declarations can stand together.
    ///
    /// Grouped first and checked as a group, because §3.5's rules are about a
    /// *set*: an identical signature is a duplicate, and two that some argument
    /// would reach equally well are an ambiguity. Neither question can be asked
    /// of one declaration at a time.
    fn overloaded(&self, stmts: &[Stmt]) -> Result<HashSet<String>> {
        let mut seen: HashMap<&str, Vec<&Rc<FnDecl>>> = HashMap::new();
        for stmt in stmts {
            if let StmtKind::Fn { decl, .. } = &stmt.kind {
                seen.entry(decl.name.as_str()).or_default().push(decl);
            }
        }
        let mut overloaded = HashSet::new();
        for (name, decls) in seen {
            if decls.len() == 1 {
                continue;
            }
            refuse_clashes(&decls, name, "this scope")?;
            overloaded.insert(name.to_string());
        }
        Ok(overloaded)
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
                )
                .with_help(
                    "the second would shadow the first silently — rename it, or assign to the \
                     name that is already there",
                ));
            }
            return Ok(());
        };
        if scope.names.contains_key(name) {
            return Err(declaration(
                format!("`{name}` is already declared in this scope"),
                span,
            )
            .with_help(
                "the second would shadow the first silently — rename it, or assign to the name \
                 that is already there",
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

    /// Whether `let x: T` with no initializer has something to put there.
    ///
    /// A class is default-constructible when the first `op init` up its chain
    /// takes nothing — or when there is no `op init` anywhere, which is the
    /// synthesized `op init() {}` of §3.4 stated as the absence of a reason to
    /// refuse. Declaring `op init(val: int)` suppresses that: a class that
    /// requires an argument means it.
    ///
    /// **Every builtin with a representation answers**, with the zero of it:
    /// `0`, `0.0`, `""`, `false`, `[]`, `{}`. v0.7 §3.3 admitted only the two
    /// containers, on the grounds that zero is a value somebody chose — and
    /// v0.9 moved the line, because a field annotated with a type *parameter*
    /// cannot be written with an initializer that suits every argument, so the
    /// old rule made `class Pair[A, B]` unwritable. See
    /// [`Parser::default_for`](crate::syntax::parser::Parser::default_for).
    ///
    /// The two that still refuse are the two that cannot answer rather than the
    /// two nobody liked: `function` and `class` have no zero, which is the same
    /// reason they are the two builtins that refuse to be constructed at all.
    ///
    /// Answers `true` for a class this pass cannot see, for the reason
    /// [`Inherited::Unknown`] exists: a check that has evaluated nothing must be
    /// allowed to miss, and never to accuse.
    pub(super) fn default_constructible(&self, ty: &str) -> bool {
        let mut seen = HashSet::new();
        let mut name = ty;
        loop {
            if !seen.insert(name) {
                return true;
            }
            if let Some(builtin) = builtin_named(name) {
                return !matches!(builtin, "function" | "class" | "module" | "nil");
            }
            if !self.members.contains_key(name) {
                return true;
            }
            if let Some(&required) = self.constructors.get(name) {
                return required == 0;
            }
            match self.parents.get(name) {
                Some(up) => name = up,
                None => return true,
            }
        }
    }

    /// What the chain starting at `parent` declares under `member`.
    ///
    /// Walks names rather than classes, because nothing has been evaluated. A
    /// builtin ends the walk: its seed table is the last word, and it descends
    /// from nothing.
    ///
    /// `excluding` is the class doing the asking, when the question is about
    /// what it *inherits*: `class A extends B` with `class B extends A` is a
    /// cycle the evaluator refuses, and a walk that came back round to `A` would
    /// find `A`'s own method and report it as overriding itself. `None` asks
    /// about the whole chain including `from`, which is what a `self.m(…)` inside
    /// the class wants.
    pub(super) fn inherited(
        &self,
        excluding: Option<&str>,
        from: &str,
        member: &str,
    ) -> Inherited {
        let mut seen: HashSet<&str> = excluding.into_iter().collect();
        let mut name = from;
        loop {
            // The other half of the same guard: a cycle that does not include
            // the asking class still has to terminate. `builtin_base` carries it
            // for the same reason.
            if !seen.insert(name) {
                return Inherited::Absent;
            }
            if let Some(declared) = self.members.get(name) {
                if let Some(&found) = declared.get(member) {
                    return Inherited::Found {
                        owner: name.to_string(),
                        member: found,
                    };
                }
            } else if let Some(builtin) = builtin_named(name) {
                return match builtin_declares(builtin, member) {
                    // A builtin's method table is written in Rust and carries
                    // neither word. Guarding one, or promising it is pure, would
                    // be a decision taken in a seed table, and none of them
                    // takes it.
                    true => Inherited::Found {
                        owner: builtin.to_string(),
                        member: Member {
                            guarded: false,
                            constant: false,
                        },
                    },
                    false => Inherited::Absent,
                };
            } else {
                return Inherited::Unknown;
            }
            match self.parents.get(name) {
                Some(up) => name = up,
                None => return Inherited::Absent,
            }
        }
    }
}

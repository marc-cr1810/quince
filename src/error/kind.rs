//! What sort of error this is, independent of how it is worded.
//!
//! Split from the error itself because a kind is the stable half — a message is
//! reworded freely, a kind is what a `catch` filters on — and because the list
//! grows with every milestone. v0.7 alone adds a visibility error and a type
//! error that the resolver raises before anything runs.

/// What sort of error this is, independent of how it is worded.
///
/// Two jobs. It picks the class a `catch` reifies the error into, and it is what
/// a typed `catch e: TypeError` will eventually filter on. Message strings are
/// for humans and programs should never match on them, so this is the half that
/// has to stay stable — retrofitting it after programs are written against
/// message text is not a thing that can be done quietly.
///
/// Every variant names a class bound as a global at startup, and every one of
/// those extends `Error`, so `catch e` binds the same shape whatever went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Unclassified, and the default. There are around forty raise sites and
    /// most of them predate this enum; leaving `new` to fill this in is what
    /// kept adding a kind from touching all of them.
    Runtime,
    Type,
    Name,
    /// A field or method that the receiver does not have. Separate from
    /// [`ErrorKind::Type`] because the receiver's type is usually right and the
    /// name usually is not, which is a different mistake to go looking for.
    Attr,
    /// The right type carrying a value it cannot represent — `int("abc")`, where
    /// a string is exactly what `int` accepts and that particular string is not
    /// a number. Separate from [`ErrorKind::Type`] because the call is well
    /// formed and only the data is wrong, so the fix is upstream in the data
    /// rather than at the call.
    Value,
    Index,
    Key,
    Frozen,
    Recursion,
    ZeroDivision,
    Overflow,
    /// A module that could not be loaded: one that is not there, one that cannot
    /// be read, one that imports itself.
    ///
    /// One kind for all three because they are one thing to whoever hit it — the
    /// import did not work — and because the fix is in the same place every
    /// time. Not [`ErrorKind::Name`], which was the first answer and the wrong
    /// one: a cycle reported as a name error says nothing true, and the three
    /// belong together more than any of them belongs with an undefined variable.
    Import,
    /// The filesystem refused: a file that is not there, a directory that cannot
    /// be written to.
    ///
    /// The one kind whose cause is outside the program, and so the one whose
    /// occurrence can differ between two runs of identical source. That is why
    /// it is separate from [`ErrorKind::Import`], which it once stood in for: an
    /// import failing is a fact about the program, and `io.read` failing is a
    /// fact about the machine.
    Io,
    /// Raised by `throw`. The class comes from the instance in
    /// [`QuinceError::payload`](super::QuinceError::payload), so this variant names none of its own.
    Thrown,
    /// Text that does not parse — an unterminated string, a missing operand, a
    /// `{` where a name belongs.
    ///
    /// Raised inside `compile`, so see [`ErrorKind::class_name`] for why it
    /// names no class.
    Syntax,
    /// Text that parses and still is not a program: a name declared twice,
    /// `self` outside a method, an `op` where no `op` may go, a parameter count
    /// that does not match the operation.
    ///
    /// Separate from [`ErrorKind::Syntax`] because the grammar is satisfied and
    /// the rule broken is about names and where they may appear — the same
    /// reason [`ErrorKind::Attr`] is separate from [`ErrorKind::Type`]. Telling
    /// someone their syntax is wrong when it parsed sends them looking in the
    /// wrong place.
    Declaration,
}

impl ErrorKind {
    /// The class an error of this kind reifies into, or `None` if it can never
    /// be caught.
    ///
    /// Kept beside the variants rather than in the interpreter so that adding a
    /// kind and forgetting to bind its class is a compile error in one file
    /// instead of a missing global discovered by a `catch`.
    ///
    /// The `None` arm is the whole compile-time story. `compile` runs to
    /// completion before `Interp::run` is called, so a `catch` cannot be
    /// executing when a syntax or declaration error is raised — there is no
    /// frame to unwind to. Binding classes for them anyway would let someone
    /// write `catch e: SyntaxError` and get a clause the language has no way to
    /// say can never fire. The day `import` compiles a module part-way through a
    /// run, these gain a class and this arm shrinks; until then the invariant is
    /// "every *catchable* kind names a class", and this is where it is enforced.
    pub fn class_name(&self) -> Option<&'static str> {
        match self {
            ErrorKind::Syntax | ErrorKind::Declaration => None,
            // `Thrown` never reaches here — the payload carries its own class —
            // but the base is the honest answer rather than a panic.
            ErrorKind::Runtime | ErrorKind::Thrown => Some("Error"),
            ErrorKind::Type => Some("TypeError"),
            ErrorKind::Name => Some("NameError"),
            ErrorKind::Attr => Some("AttributeError"),
            ErrorKind::Value => Some("ValueError"),
            ErrorKind::Index => Some("IndexError"),
            ErrorKind::Key => Some("KeyError"),
            ErrorKind::Frozen => Some("FrozenError"),
            ErrorKind::Recursion => Some("RecursionError"),
            ErrorKind::ZeroDivision => Some("ZeroDivisionError"),
            ErrorKind::Overflow => Some("OverflowError"),
            ErrorKind::Import => Some("ImportError"),
            ErrorKind::Io => Some("IoError"),
        }
    }

    /// The word this kind reports itself as, inside `error[…]`.
    ///
    /// A different question from [`ErrorKind::class_name`], and the split is
    /// what lets the two compile-time kinds report a name without pretending to
    /// be catchable. Every other kind defers, so a class name is written once
    /// and the two answers cannot drift.
    pub fn code(&self) -> &'static str {
        match self {
            ErrorKind::Syntax => "SyntaxError",
            ErrorKind::Declaration => "DeclarationError",
            // The `None` arms are the two above, both matched already.
            kind => kind.class_name().unwrap_or("Error"),
        }
    }
}

/// Every kind's class, for the startup that has to bind them.
///
/// Maintained by hand for the same reason `class::BUILTINS` is: the mapping runs
/// from kind to name, and Rust cannot be asked to walk it backwards. A stale
/// entry here is a `catch` that cannot find its class, which is why
/// `every_listed_kind_names_a_distinct_class` exists.
///
/// The compile-time kinds are absent because they have no class to bind. That
/// they are exactly the kinds absent — rather than some of them being an
/// oversight — is what `only_the_uncatchable_kinds_are_unlisted` checks.
pub static ERROR_KINDS: &[ErrorKind] = &[
    ErrorKind::Runtime,
    ErrorKind::Type,
    ErrorKind::Name,
    ErrorKind::Attr,
    ErrorKind::Value,
    ErrorKind::Index,
    ErrorKind::Key,
    ErrorKind::Frozen,
    ErrorKind::Recursion,
    ErrorKind::ZeroDivision,
    ErrorKind::Overflow,
    ErrorKind::Import,
    ErrorKind::Io,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_kind_names_a_distinct_class() {
        // What actually stops a new variant going unbound is that `class_name`
        // is an exhaustive match, so it cannot be added without being named.
        // This checks the other half: that the names are usable as globals, one
        // class each, and that `Thrown` stays out — it borrows its class from the
        // instance that was thrown.
        let mut names = Vec::new();
        for kind in ERROR_KINDS {
            assert_ne!(*kind, ErrorKind::Thrown, "`Thrown` names no class");
            names.push(
                kind.class_name()
                    .unwrap_or_else(|| panic!("{kind:?} is listed but names no class")),
            );
        }
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "two kinds share a class: {names:?}"
        );
    }

    /// The other direction of the same invariant.
    ///
    /// `every_listed_kind_names_a_distinct_class` checks that nothing listed is
    /// uncatchable. This checks that nothing unlisted is catchable, which is the
    /// half that would otherwise go wrong quietly: forgetting to add a new kind
    /// to `ERROR_KINDS` costs a `catch` that cannot find its class, and the
    /// symptom is a panic in `error_class` on the day someone first triggers it.
    #[test]
    fn only_the_uncatchable_kinds_are_unlisted() {
        for kind in [ErrorKind::Syntax, ErrorKind::Declaration] {
            assert_eq!(kind.class_name(), None, "{kind:?} should name no class");
            assert!(
                !ERROR_KINDS.contains(&kind),
                "{kind:?} names no class and must not be listed for binding"
            );
        }
        // Every kind reachable from a report. Written out rather than derived,
        // because the point is to fail when a variant is added and its
        // catchability never answered for.
        let all = [
            ErrorKind::Runtime,
            ErrorKind::Type,
            ErrorKind::Name,
            ErrorKind::Attr,
            ErrorKind::Value,
            ErrorKind::Index,
            ErrorKind::Key,
            ErrorKind::Frozen,
            ErrorKind::Recursion,
            ErrorKind::ZeroDivision,
            ErrorKind::Overflow,
            ErrorKind::Thrown,
            ErrorKind::Syntax,
            ErrorKind::Declaration,
        ];
        for kind in all {
            // `Thrown` is the one kind that names a class and is still not bound
            // from the list: it borrows the thrown instance's own.
            if kind == ErrorKind::Thrown {
                continue;
            }
            assert_eq!(
                kind.class_name().is_some(),
                ERROR_KINDS.contains(&kind),
                "{kind:?} disagrees about whether it can be caught"
            );
        }
    }
}

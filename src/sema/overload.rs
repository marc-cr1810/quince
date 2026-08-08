//! When two declarations sharing a name are two declarations, and when they are
//! one mistake.
//!
//! v0.8 §3.5 lets a class, an `extend` block, or a scope declare several `fn`s
//! or `op`s under one name, as long as their parameter type signatures are
//! distinct. Two rules follow from that and both are enforced before anything
//! runs: an identical signature is a duplicate, and two signatures that some
//! argument would reach *equally well* are an ambiguity. A dispatch failure at
//! run time should mean "nothing matched", never "two things did".
//!
//! The run-time half — choosing among the candidates once the argument values
//! are in hand — is [`crate::interp::call`]. The two agree because they read the
//! same idea of how well an argument fits a parameter: an exact type, a widening,
//! or a parameter that takes anything. This file predicts that ranking from the
//! annotations alone; that one measures it.

use crate::syntax::ast::{FnDecl, Param, TypeExpr, TypeName};

/// How well an argument of some type fits a parameter, lower being better.
///
/// Three levels, and the ordering between them is the whole of §3.5's "exact
/// match before widened match".
mod fit {
    /// The value's type is the parameter's, written without a `?`.
    pub const EXACT: u8 = 0;
    /// A conversion the language performs at the boundary: an `int` into a
    /// `float`, a value into the nullable form of its own type, a subclass into
    /// the class it descends from.
    pub const WIDENED: u8 = 1;
    /// The parameter takes anything — unannotated, or `any`. Tried last, which
    /// is what §3.5 means by "a class may have at most one unannotated overload
    /// for a name".
    pub const ANYTHING: u8 = 2;
}

/// A type name no annotation can be written for.
///
/// Stands in for "some value that is neither of these two parameters' types"
/// when [`ties`] asks whether a value exists that both would take equally well.
/// Two catch-all parameters take it identically, which is how `fn f(x)` beside
/// `fn f(y: any)` is caught.
const OTHER: &str = "\u{0}other";

/// Why two declarations sharing a name cannot both stand.
#[derive(Debug, PartialEq, Eq)]
pub enum Clash {
    /// The same parameter types, so nothing could ever tell them apart. The
    /// arity is carried because a defaulted parameter makes one declaration
    /// several signatures, and the report should say which one collided.
    Duplicate { arity: usize },
    /// Different parameter types that some argument would reach equally well —
    /// `f(x: float)` beside `f(x: int?)`, which an `int` widens into either way.
    Ambiguous { arity: usize },
}

impl Clash {
    /// How the collision reads in a report.
    ///
    /// `whose` is what the two declarations share — a class, an `extend` block,
    /// a scope — and comes first, because that is where the reader has to go and
    /// look for the other one.
    pub fn describe(&self, name: &str, whose: &str) -> String {
        match self {
            Clash::Duplicate { .. } => {
                format!("{whose} already declares `{name}` with these parameter types")
            }
            Clash::Ambiguous { .. } => {
                format!("{whose} declares a `{name}` this one cannot be told apart from")
            }
        }
    }

    /// What to do about it.
    pub fn help(&self) -> String {
        match self {
            Clash::Duplicate { arity } => format!(
                "two declarations sharing a name are told apart by their parameter types, and \
                 these two agree at {arity} argument{} — rename one, or give it types the other \
                 does not have",
                plural(*arity)
            ),
            Clash::Ambiguous { arity } => format!(
                "at {arity} argument{} there is a call both would take equally well, and a \
                 dispatch failure has to mean nothing matched rather than that two things did — \
                 make the types disjoint, or rename one",
                plural(*arity)
            ),
        }
    }
}

fn plural(count: usize) -> &'static str {
    match count {
        1 => "",
        _ => "s",
    }
}

/// The parameters a caller writes, which is all of them but the receiver.
pub fn written(decl: &FnDecl) -> &[Param] {
    let receiver = decl.params.first().is_some_and(|param| param.receiver);
    &decl.params[usize::from(receiver)..]
}

/// How many arguments a call has to supply, and how many it may.
///
/// §3.6's rule that a declaration contributes one signature per callable arity:
/// `fn f(a: int, b: int = 0)` is both `(int)` and `(int, int)`, and both are
/// checked against everything else declared under the name.
pub fn arities(decl: &FnDecl) -> std::ops::RangeInclusive<usize> {
    let params = written(decl);
    let required = params.iter().filter(|param| param.default.is_none()).count();
    required..=params.len()
}

/// Why `later` cannot be declared beside `earlier`, if it cannot.
pub fn clash(earlier: &FnDecl, later: &FnDecl) -> Option<Clash> {
    let (a, b) = (written(earlier), written(later));
    let (first, second) = (arities(earlier), arities(later));
    let from = *first.start().max(second.start());
    let to = *first.end().min(second.end());
    for arity in from..=to {
        let pairs = || a[..arity].iter().zip(&b[..arity]);
        if pairs().all(|(x, y)| same(x, y)) {
            return Some(Clash::Duplicate { arity });
        }
        // Every position indistinguishable is what makes the *signature*
        // indistinguishable. One position that separates them is enough, which
        // is why `f(a: int, b: int)` and `f(a: int, b: string)` are fine.
        if pairs().all(|(x, y)| ties(x, y)) {
            return Some(Clash::Ambiguous { arity });
        }
    }
    None
}

/// Whether two parameters are annotated with the same type.
///
/// Unannotated counts as a type here — the one that takes anything — so two
/// unannotated parameters are the same and an unannotated one beside `int` is
/// not.
fn same(a: &Param, b: &Param) -> bool {
    match (&a.ty, &b.ty) {
        (None, None) => true,
        (Some(x), Some(y)) => x.same_as(y),
        _ => false,
    }
}

/// Whether some value would fit both parameters equally well.
///
/// Asked of a handful of representative types rather than of every type there
/// is: each parameter's own base type, a type that is neither, and `nil`. That
/// is enough, because a parameter only ever fits at [`fit::EXACT`] a value of
/// its own type, and everything else it fits it fits at the same level.
///
/// Two containers with the same base and different arguments — `list[int]` and
/// `list[string]` — are *not* a tie. A container that crossed an annotated
/// boundary carries what it was built to hold, and `holds` reads that header, so
/// a `list[int]` reaches the first and a `list[string]` the second. What is left
/// over is the container nothing described — `total([])`, where the literal is
/// every element type at once — and that is refused at the call, by
/// `Interp::selected`, because it is a property of the argument rather than of
/// the declarations.
///
/// **`list[int]` beside a bare `list` is the same case**, and used to be refused
/// here because only one of the two had arguments to compare. An argument nobody
/// wrote is `any?` (§3.10), so that pair is `list[int]` beside `list[any?]` —
/// overlapping, like `int` beside `float`, and told apart at the call the same
/// way, because `Interp::quality` ranks an exact header above a widened one.
/// Requiring *both* to be written out was this file deciding elision for itself,
/// which is the drift `sema::types::arguments_admit` exists to end.
fn ties(a: &Param, b: &Param) -> bool {
    if let (Some(x), Some(y)) = (&a.ty, &b.ty)
        && x.name == y.name
        && (!x.args.is_empty() || !y.args.is_empty())
        && !x.same_args_as(y)
    {
        return false;
    }
    let mut reps = vec![OTHER];
    for param in [a, b] {
        if let Some(TypeExpr {
            name: TypeName::Named(named),
            ..
        }) = &param.ty
        {
            reps.push(named);
        }
    }
    if reps
        .into_iter()
        .any(|rep| matched(a, rep).is_some() && matched(a, rep) == matched(b, rep))
    {
        return true;
    }
    matched_by_nil(a).is_some() && matched_by_nil(a) == matched_by_nil(b)
}

/// How well a value of type `rep` fits `param`, or `None` if it does not fit.
fn matched(param: &Param, rep: &str) -> Option<u8> {
    let Some(ty) = &param.ty else {
        return Some(fit::ANYTHING);
    };
    match &ty.name {
        TypeName::Any => Some(fit::ANYTHING),
        // A value where a type goes, which no argument fits. See
        // `Interp::fits`, which scores the same question at run time.
        TypeName::Const(_) => None,
        TypeName::Named(named) if named == rep => Some(match ty.nullable {
            // Reaching a `T?` with a `T` is a widening, not an exact match:
            // the annotation admits more than the value is. That difference is
            // what lets `f(x: int)` and `f(x: int?)` coexist while `f(x: float)`
            // and `f(x: int?)` cannot — an `int` is exact for the first pair's
            // `int` and widened for both of the second's.
            true => fit::WIDENED,
            false => fit::EXACT,
        }),
        TypeName::Named(named) if named == "float" && rep == "int" => Some(fit::WIDENED),
        // A subclass of `rep` would fit too, and this cannot know whether one
        // exists. Left out on purpose: the check is allowed to miss a tie only
        // a hierarchy creates, and must not invent one between unrelated types.
        TypeName::Named(_) => None,
    }
}

/// The same for `nil`, which no name stands for.
fn matched_by_nil(param: &Param) -> Option<u8> {
    let Some(ty) = &param.ty else {
        return Some(fit::ANYTHING);
    };
    match (&ty.name, ty.nullable) {
        (TypeName::Any, true) => Some(fit::ANYTHING),
        (TypeName::Named(_), true) => Some(fit::WIDENED),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::ast::StmtKind;

    /// The declarations in `src`, which is a class body's worth of methods.
    fn decls(src: &str) -> Vec<std::rc::Rc<FnDecl>> {
        let tokens = crate::syntax::lexer::Lexer::new(src)
            .tokenize()
            .expect("the source lexes");
        let program = crate::syntax::parser::Parser::new(tokens)
            .parse()
            .expect("the source parses");
        program
            .iter()
            .filter_map(|stmt| match &stmt.kind {
                StmtKind::Fn { decl, .. } => Some(std::rc::Rc::clone(decl)),
                _ => None,
            })
            .collect()
    }

    /// The clash between the two functions in `src`, if there is one.
    fn between(src: &str) -> Option<Clash> {
        let decls = decls(src);
        assert_eq!(decls.len(), 2, "the test declares two functions");
        clash(&decls[0], &decls[1])
    }

    #[test]
    fn one_signature_declared_twice_is_a_duplicate() {
        assert_eq!(
            between("fn f(a: int) {}\nfn f(b: int) {}"),
            Some(Clash::Duplicate { arity: 1 }),
            "the parameter names differ and the types do not"
        );
        assert_eq!(
            between("fn f(a) {}\nfn f(b) {}"),
            Some(Clash::Duplicate { arity: 1 }),
            "unannotated is a type too — the one that takes anything"
        );
        assert_eq!(between("fn f() {}\nfn f() {}"), Some(Clash::Duplicate { arity: 0 }));
    }

    #[test]
    fn distinct_types_are_distinct_declarations() {
        assert_eq!(between("fn f(a: int) {}\nfn f(a: string) {}"), None);
        assert_eq!(between("fn f(a: int) {}\nfn f(a: int, b: int) {}"), None);
        // An unannotated parameter takes anything and is tried last, so it sits
        // beside an annotated one rather than colliding with it.
        assert_eq!(between("fn f(a) {}\nfn f(a: int) {}"), None);
        // Exact beats widened, so an `int` argument tells these two apart.
        assert_eq!(between("fn f(a: int) {}\nfn f(a: float) {}"), None);
        assert_eq!(between("fn f(a: int) {}\nfn f(a: int?) {}"), None);
    }

    #[test]
    fn containers_are_told_apart_by_what_they_hold() {
        // Not a tie: a container that crossed an annotated boundary carries what
        // it was built to hold, so `holds` reads the header and a `list[int]`
        // reaches the first of these. What is left over — the container nothing
        // described — is the *call*'s problem and is refused there.
        assert_eq!(between("fn f(a: list[int]) {}\nfn f(a: list[string]) {}"), None);
        assert_eq!(
            between("fn f(a: dict[string, int]) {}\nfn f(a: dict[string, bool]) {}"),
            None
        );
        // The same types written twice are still a duplicate, arguments and all.
        assert_eq!(
            between("fn f(a: list[int]) {}\nfn f(b: list[int]) {}"),
            Some(Clash::Duplicate { arity: 1 })
        );
        // A bare `list` beside a parameterized one is not a tie either. It reads
        // as `list[any?]` — an argument nobody wrote is `any?`, §3.10 — so the
        // pair overlaps the way `int` beside `float` does rather than colliding,
        // and `Interp::quality` ranks the exact header above the widened one.
        //
        // This used to be `Ambiguous`, on the claim that "every container the
        // second takes, the first takes just as exactly". The "just as exactly"
        // was the false half.
        assert_eq!(between("fn f(a: list) {}\nfn f(a: list[int]) {}"), None);
        // `list[any]` is the same shape of overlap, written out.
        assert_eq!(between("fn f(a: list[any]) {}\nfn f(a: list[int]) {}"), None);
        // A bare `list` twice over is still a duplicate: elision is a type, and
        // it is the same type both times.
        assert_eq!(
            between("fn f(a: list) {}\nfn f(b: list) {}"),
            Some(Clash::Duplicate { arity: 1 })
        );
    }

    #[test]
    fn two_widenings_of_one_type_are_ambiguous() {
        // §3.5's own example. An `int` reaches `float` by widening and `int?` by
        // widening, so neither is preferred and the call has no answer.
        assert_eq!(
            between("fn f(a: float) {}\nfn f(a: int?) {}"),
            Some(Clash::Ambiguous { arity: 1 })
        );
        // And `nil` reaches either of two nullables the same way.
        assert_eq!(
            between("fn f(a: int?) {}\nfn f(a: string?) {}"),
            Some(Clash::Ambiguous { arity: 1 })
        );
    }

    #[test]
    fn one_position_that_separates_them_is_enough() {
        assert_eq!(
            between("fn f(a: float, b: int) {}\nfn f(a: int?, b: string) {}"),
            None,
            "the second parameter tells them apart even though the first does not"
        );
    }

    #[test]
    fn a_default_makes_a_declaration_several_signatures() {
        // §3.6's rule, which is the reason defaults could not be deferred past
        // overloading: `fn f(a: int, b: int = 0)` *is* an `f(int)`.
        assert_eq!(
            between("fn f(a: int, b: int = 0) {}\nfn f(a: int) {}"),
            Some(Clash::Duplicate { arity: 1 })
        );
        assert_eq!(
            between("fn f(a: int, b: int = 0) {}\nfn f(a: int, b: string) {}"),
            None,
            "they overlap only at two arguments, where the types differ"
        );
    }

    #[test]
    fn at_most_one_unannotated_overload() {
        // Falls out of unannotated being a type: two of them are a duplicate,
        // and one beside `any` is a tie because nothing distinguishes what they
        // accept.
        assert_eq!(
            between("fn f(a) {}\nfn f(a: any) {}"),
            Some(Clash::Ambiguous { arity: 1 })
        );
    }

    #[test]
    fn a_receiver_is_not_a_parameter_a_call_writes() {
        let methods = decls("fn f(a: int) {}");
        assert_eq!(written(&methods[0]).len(), 1);
        assert_eq!(arities(&methods[0]), 1..=1);
    }
}

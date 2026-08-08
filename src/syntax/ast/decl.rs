//! The parts of a declaration that are not the code inside it.
//!
//! A parameter, a function's header, an import list, and the modifier words a
//! binding or a class carries. These are what the milestones after v0.6 grow:
//! v0.7 puts a type annotation and a visibility word on [`Param`] and
//! [`FnDecl`], v0.8 adds `const`, `override`, and `explicit`, and v0.9 adds a
//! type-parameter list.


use crate::syntax::ast::{Block, Expr, Op};
use crate::syntax::doc::Doc;
use crate::syntax::token::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub span: Span,
    /// What the caller may pass, if the declaration said. `None` is the
    /// unannotated parameter, which is whatever it is handed.
    pub ty: Option<TypeExpr>,
    /// The word the parameter was declared with, meaning here exactly what it
    /// means on a binding: `let` reassignable, `final` bound once, `const`
    /// bound once and the value frozen.
    ///
    /// A parameter is a binding the caller fills in, so it takes the binding
    /// forms — `fn f(const xs: list[int])` is how §3.3's `const` parameter is
    /// written now that the two spellings agree. [`BindKind::Let`] is the
    /// default and is what every parameter written before v0.7 is.
    pub bind: BindKind,
    /// What the parameter holds when the call does not say — v0.8 §3.6.
    ///
    /// Evaluated **at the call**, in the callee's declaration scope, every time.
    /// That is the one place Python's answer is refused outright: a default
    /// evaluated once at the declaration makes `fn f(xs: list = [])` share one
    /// list between every call that omits it, which is the single most reported
    /// footgun in that language.
    ///
    /// `?` on the type does not imply one. `fn f(x: int?)` requires an argument;
    /// only `= nil` makes it optional. An annotation says what a parameter may
    /// hold, not whether it must be written.
    pub default: Option<Expr>,
    /// Whether this is the `self` the parser inserted, rather than a parameter
    /// someone wrote.
    ///
    /// A flag rather than a comparison against [`SELF`], because the name is
    /// only unambiguous while `self` is a keyword — an invariant that lives in
    /// the lexer and would be silently assumed here. The parser knows which
    /// parameter it invented, so it says so.
    pub receiver: bool,
}

/// The receiver's name inside a method body.
///
/// `self` is bound as an ordinary parameter that the parser inserts, rather
/// than as something the evaluator injects. Everything downstream then treats
/// it as a local: the resolver gives it a slot, a closure nested in a method
/// captures it through the scope chain like any other name, and `read` needs no
/// special case. What the keyword buys is the error when it is used outside a
/// method, which would otherwise read `undefined variable`.
pub const SELF: &str = "self";

/// The parent class's name inside a method body.
///
/// Bound the same way, but as a slot in a scope wrapped around the methods of a
/// class that extends another, rather than as a parameter — its value is fixed
/// when the class is declared, not per call. A closure nested in a method
/// reaches it through the same chain that carries [`SELF`].
pub const SUPER: &str = "super";

/// A method the language calls on the program's behalf.
///
/// These are the methods nobody writes a call to: `Point(1, 2)` reaches `init`,
/// `len(x)` reaches `len`, `if x` reaches `bool`, `a + b` reaches `add`.
/// Declared with `op` rather than `fn` so that being one is stated rather than
/// inferred from the name, which is what makes the misspelling an error instead
/// of a method nothing ever calls.
///
/// A closed set on purpose. `Op::from_name` is the only way in, so `op lenght`
/// cannot compile, and every member has to be listed in [`OPS`](super::OPS) to be reachable
/// — see `every_listed_op_round_trips_through_its_name`.
///
/// The declaration order is load-bearing. [`Op::index`] is the discriminant, and
/// it indexes `Class::slots`, so [`OPS`](super::OPS) has to list the members in the same
/// order — two ops sharing an index would not fail loudly the way a missing
#[derive(Clone, Debug, PartialEq)]
pub enum ImportNames {
    /// `import math` — the module itself, under the name it was imported by.
    Module,
    /// `from math import floor, ceil` — each name, bound to what the module
    /// declared under it.
    Names(Vec<ImportName>),
}

/// One name in a `from … import` list.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportName {
    pub name: String,
    /// Its own span, so a module that declares three of the four names asked for
    /// can have the caret put under the fourth.
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    pub name: String,
    /// Where the name was written, which the body's span cannot stand in for: a
    /// report about a *declaration* should underline the word being declared,
    /// not the twenty lines under it.
    pub name_span: Span,
    /// For a method, `self` is `params[0]`; see [`SELF`].
    pub params: Vec<Param>,
    pub body: Block,
    /// Set when the declaration used `op`, which the parser allows only inside a
    /// class body — so a plain function always leaves this `None`.
    pub op: Option<Op>,
    /// What calling it hands back, if the declaration said.
    pub returns: Option<TypeExpr>,
    /// How far it reaches: who may call the method, or whether an importing
    /// module sees the function.
    ///
    /// An `op` is always [`Visibility::Public`] and the parser refuses anything
    /// else — the language calls these on the program's behalf, from outside, so
    /// a private one would be a method `print` is entitled to call and forbidden
    /// from calling.
    pub visibility: Visibility,
    /// Whether `const` was written in front of it, marking the body pure and
    /// read-only — v0.8 §3.1.
    ///
    /// The same word as the binding and annotation forms, and v0.7 §3.3 argues
    /// they are one idea: a `const` binding may not be rebound, a `const T`
    /// parameter may not be mutated through, and a `const fn` may not mutate
    /// anything at all. What it restricts is *state*, not effects — `print`,
    /// `throw`, and an early `return` are all fine. The resolver is what holds
    /// a declaration to it; see `sema::resolve::purity`.
    pub constant: bool,
    /// Whether `override` was written, saying that this member replaces one a
    /// superclass declared.
    ///
    /// Required where it is true and refused where it is not, which is the pair
    /// that makes it worth writing: a keyword that could be written where it is
    /// false would be documentation nobody could trust, and a typo'd method name
    /// is exactly the mistake the other half catches.
    pub overrides: bool,
    /// Whether `final` was written, forbidding a subclass from replacing it.
    ///
    /// The same word as [`BindKind::Final`] and for the same idea — this name is
    /// bound once and cannot be rebound. On a field it is the value; here it is
    /// the implementation.
    pub guarded: bool,
    /// Whether `explicit` was written, refusing the implicit constructor
    /// coercion of v0.8 §3.3.
    ///
    /// Only ever set on an `op init` taking one parameter; the parser refuses it
    /// anywhere else, since there is nothing elsewhere for it to turn off.
    pub explicit: bool,
    /// The `##` block written above it, already checked against `params`.
    ///
    /// Checked at the parser rather than kept raw, so that a `@param` naming
    /// something this function does not take is refused where the parameter
    /// list is in hand — which is the only place the report can say what it
    /// *does* take.
    pub doc: Option<Doc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindKind {
    /// `let`: the name may be reassigned.
    Let,
    /// `final`: the name is bound once. The object it names is untouched, so a
    /// `final` list still grows — see [`BindKind::Const`] for the other one.
    Final,
    /// `const`: the name is bound once *and* the value is frozen, deeply, and
    /// through every other name that already reaches it.
    Const,
}

impl BindKind {
    pub fn mutable(&self) -> bool {
        matches!(self, BindKind::Let)
    }

    pub fn freezes(&self) -> bool {
        matches!(self, BindKind::Const)
    }

    /// How the keyword is written, for error messages that quote it back.
    pub fn word(&self) -> &'static str {
        match self {
            BindKind::Let => "let",
            BindKind::Final => "final",
            BindKind::Const => "const",
        }
    }
}

/// A type parameter a declaration introduces — `class Stack[T]`'s `T`.
///
/// A declaration's side of what [`TypeExpr`] is the use of. The name is in
/// scope as a *type* through the body, and as nothing else: v0.9 §3.1 refuses
/// `T()` because a parameter names a type and not a value, and there is no
/// binding here for one to be found in.
///
/// A `bound` is a field here rather than a second parameter form, because it
/// qualifies a parameter the way `?` qualifies a type — the thing is the same
/// thing either way. v0.9 §3.2.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub span: Span,
    /// What an argument for this parameter must satisfy — `[T: float]`'s
    /// `float`. `None` is the unbounded `[T]`, which §3.2 says means `any?`:
    /// the top type, constraining nothing.
    ///
    /// An ordinary [`TypeExpr`], because a bound is an ordinary type and
    /// satisfying it is ordinary matching. There is no second subtyping
    /// relation in the language and §3.2 exists partly to say so.
    pub bound: Option<TypeExpr>,
}

/// A type as the program wrote it.
///
/// The *source* form, kept beside the declaration it annotates: a name, its
/// arguments, and the two suffixes/prefixes that qualify it. [`crate::sema::types::Type`]
/// is the answer the pass works in, and this turns into one — but the two are
/// deliberately different things. A report about an annotation has to underline
/// the words the program typed, which needs spans this carries and that carries
/// nothing of.
///
/// v0.9 gives [`TypeExpr::args`] something more to hold: a user class declares
/// what fills it, so `Stack[int]` is written in the same shape `list[int]` is.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeExpr {
    pub name: TypeName,
    /// `list[int]`'s `int`, in the order written.
    pub args: Vec<TypeExpr>,
    /// Whether a `?` followed it, admitting `nil`.
    pub nullable: bool,
    /// Whether `const` preceded it, freezing the value deeply as it crosses the
    /// boundary — see v0.7 §3.3. The same word and the same meaning as the
    /// binding form, at a place a binding cannot reach.
    pub frozen: bool,
    pub span: Span,
}

/// What an annotation names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeName {
    /// A class: `int`, `string`, `list`, or one the program declared.
    Named(String),
    /// `any`, or `_`, which are two spellings of one type.
    ///
    /// One variant and not two, because the difference is only how it was
    /// typed. `_` is preferred in a type-argument position, where it reads as
    /// "this parameter, unconstrained", and `any` as a whole annotation — a
    /// house style rather than a rule, so nothing here enforces it.
    Any,
}

impl TypeExpr {
    /// Whether `nil` holds as this type.
    pub fn admits_nil(&self) -> bool {
        self.nullable
    }

    /// Whether this and `other` are the same type.
    ///
    /// Not `==`, which this deliberately does not use: [`TypeExpr`] carries a
    /// [`Span`], so two identical annotations written in two places are unequal
    /// as values while naming one type. Anything comparing *types* wants this —
    /// `is` reading a reified descriptor most of all, since the descriptor was
    /// written at the declaration and the question is asked somewhere else.
    ///
    /// `frozen` is not part of it. `const list[int]` and `list[int]` are one
    /// type; `const` says what happens at a boundary the value crosses, not
    /// what the value is.
    pub fn same_as(&self, other: &TypeExpr) -> bool {
        self.name == other.name && self.nullable == other.nullable && self.same_args_as(other)
    }

    /// Whether this and `other` were written with the same type arguments.
    ///
    /// The arguments *alone*, which is what `is` compares against a reified
    /// descriptor. Deliberately not [`TypeExpr::same_as`] over the whole type:
    /// the descriptor is the annotation the value crossed, and that annotation's
    /// own nullability belongs to the binding rather than to the value in it.
    /// `let xs: list[int]? = [1]` leaves a descriptor reading `list[int]?`, and
    /// `xs is list[int]` is still true — the list is a list of ints whatever the
    /// name that held it was allowed to be.
    pub fn same_args_as(&self, other: &TypeExpr) -> bool {
        self.args.len() == other.args.len()
            && self
                .args
                .iter()
                .zip(&other.args)
                .all(|(ours, theirs)| ours.same_as(theirs))
    }

    /// How the annotation reads back, for a report that quotes it.
    ///
    /// Rebuilt from the parse rather than sliced out of the source, so a
    /// message says the same thing whatever whitespace was written.
    pub fn written(&self) -> String {
        let mut text = String::new();
        if self.frozen {
            text.push_str("const ");
        }
        match &self.name {
            TypeName::Named(name) => text.push_str(name),
            TypeName::Any => text.push_str("any"),
        }
        if !self.args.is_empty() {
            text.push('[');
            for (index, arg) in self.args.iter().enumerate() {
                if index > 0 {
                    text.push_str(", ");
                }
                text.push_str(&arg.written());
            }
            text.push(']');
        }
        if self.nullable {
            text.push('?');
        }
        text
    }
}

/// How far a declaration reaches.
///
/// One word per reach, and [`Visibility::Public`] is what a declaration without
/// a word means — so the common case is written by writing nothing, and the
/// words that appear are the ones that restrict.
///
/// The same three words answer two different questions, which is deliberate:
/// on a class member it is who may reach through the dot, and on a top-level
/// declaration it is whether an importing module sees the name at all. Both are
/// "how far does this reach", and a reader who learns the words once has learned
/// both — the same argument [`Openness`] makes for its four.
///
/// | | outside | subclass | declaring class |
/// |---|---|---|---|
/// | [`Visibility::Public`] | yes | yes | yes |
/// | [`Visibility::Protected`] | no | yes | yes |
/// | [`Visibility::Private`] | no | no | yes |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visibility {
    /// No word, or `public`: reachable from anywhere.
    #[default]
    Public,
    /// `protected`: the declaring class and the classes that extend it.
    Protected,
    /// `private`: the declaring class, and nothing else.
    Private,
}

impl Visibility {
    /// Whether code outside the class hierarchy may reach this.
    pub fn closes_outside(self) -> bool {
        !matches!(self, Visibility::Public)
    }

    /// Whether a subclass's methods may reach this.
    ///
    /// The one row that separates the two restricting words, and the reason
    /// `protected` earns a word of its own rather than being spelled as a
    /// weaker `private`.
    pub fn closes_subclass(self) -> bool {
        matches!(self, Visibility::Private)
    }

    /// Whether an importing module sees a top-level declaration under this.
    ///
    /// `protected` has no meaning at the top level — a module has no subclass —
    /// so it reads as `private` there rather than as a third answer. The parser
    /// refuses it outright (§3.6), and this is the fallback that keeps the
    /// predicate total if it ever stops doing so.
    pub fn exported(self) -> bool {
        matches!(self, Visibility::Public)
    }

    /// The keyword as written, for a report that quotes it back. `None` is the
    /// declaration that used no word and so has nothing to quote.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Visibility::Public => None,
            Visibility::Protected => Some("protected"),
            Visibility::Private => Some("private"),
        }
    }
}

/// A field declared in a class body.
///
/// New in v0.7. Before it, a field existed because an `op init` assigned one,
/// which left [`Visibility`] nothing to attach to — the whole reason this node
/// arrives with the visibility words rather than after them.
///
/// A declared field is still initialized by evaluating its `value` when an
/// instance is built, before `op init` runs, so `init` sees the declared value
/// and may overwrite it. That order is what makes `private let balance = 0`
/// followed by `self.balance = initial` read the way it looks.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    /// Where the name was written, for a report about the *field* rather than
    /// about the expression initializing it.
    pub name_span: Span,
    /// `let`, `final`, or `const`, meaning on a field exactly what it means on
    /// a binding.
    pub bind: BindKind,
    pub visibility: Visibility,
    /// What the field holds, if the declaration said.
    pub ty: Option<TypeExpr>,
    /// What it holds when an instance is built.
    pub value: Expr,
    /// Whether the declaration wrote no `= value`, leaving [`FieldDecl::value`]
    /// the one the language supplied. The same flag a binding carries, for the
    /// same reason — see `StmtKind::Let`.
    pub defaulted: bool,
    /// The `##` block written above it, as for a binding.
    pub doc: Option<Doc>,
}

/// What a class declaration leaves open.
///
/// There are exactly two ways to attach behaviour to a type from outside — a
/// subclass, and an `extend` block — so there are four states, and each has its
/// own word rather than being spelled by stacking modifiers. See DESIGN.md.
///
/// | | inherit | `extend` |
/// |---|---|---|
/// | [`Openness::Open`] | yes | yes |
/// | [`Openness::Final`] | no | yes |
/// | [`Openness::Complete`] | yes | no |
/// | [`Openness::Sealed`] | no | no |
///
/// The two predicates below are exhaustive matches on purpose: a fifth variant
/// cannot be added without answering for both doors, which is the only way the
/// table above and the code can be made to stay in step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Openness {
    /// `class Point { … }`.
    Open,
    /// `final class Point { … }` — no subclass, but its vocabulary may grow.
    Final,
    /// `complete class Point { … }` — the method table is done; subclasses are
    /// still welcome, since a subclass adds nothing to the class it descends
    /// from.
    Complete,
    /// `sealed class Point { … }` — neither door. A composite rather than a
    /// third door: `sealed` is `final` and `complete` at once, given its own
    /// word so the common case reads as one.
    Sealed,
}

impl Openness {
    /// Whether a class may name this one after `extends`.
    pub fn closes_inheritance(self) -> bool {
        matches!(self, Openness::Final | Openness::Sealed)
    }

    /// Whether an `extend` block may add a method to this one.
    pub fn closes_extension(self) -> bool {
        matches!(self, Openness::Complete | Openness::Sealed)
    }

    /// The keyword as written, for a report that quotes it back. `None` is the
    /// declaration that used no modifier and so has nothing to quote.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Openness::Open => None,
            Openness::Final => Some("final"),
            Openness::Complete => Some("complete"),
            Openness::Sealed => Some("sealed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// Parses a bare annotation, for the tests below.
    fn annotation(src: &str) -> TypeExpr {
        let tokens = crate::syntax::lexer::Lexer::new(src)
            .tokenize()
            .expect("the annotation lexes");
        crate::syntax::parser::Parser::new(tokens)
            .parse_type_for_test()
            .expect("the annotation parses")
    }

    #[test]
    fn one_type_written_twice_is_one_type() {
        // The trap this exists for. `TypeExpr` carries a `Span`, so `==` on two
        // identical annotations written in two places is `false` — and `is`
        // compares a descriptor written at a declaration against a type written
        // somewhere else, so `==` had it answering `false` for `xs is list[int]`.
        //
        // The derive cannot simply be removed: `Expr` holds a `TypeExpr` and
        // derives `PartialEq` for the parser's own tests. So the trap stays and
        // this is what keeps anything from walking into it again.
        let here = annotation("list[int]");
        let there = annotation("   list[int]");
        assert_ne!(here, there, "the spans differ, so the values differ");
        assert!(here.same_as(&there), "and yet it is one type");

        assert!(annotation("int").same_as(&annotation("int")));
        assert!(!annotation("int").same_as(&annotation("string")));
        assert!(!annotation("int").same_as(&annotation("int?")));
        assert!(!annotation("list[int]").same_as(&annotation("list[string]")));
        assert!(!annotation("list[int]").same_as(&annotation("list")));
        assert!(
            annotation("dict[string, list[int]]").same_as(&annotation("dict[string, list[int]]")),
            "nesting compares all the way down"
        );
    }

    #[test]
    fn const_says_nothing_about_what_a_value_is() {
        // `const` is what happens at a boundary the value crosses, not what the
        // value is — so it is not part of type identity.
        assert!(annotation("const list[int]").same_as(&annotation("list[int]")));
    }

    #[test]
    fn arguments_compare_apart_from_the_nullability_around_them() {
        // What `is` needs. A descriptor reading `list[int]?` was left by a
        // binding that was allowed to hold `nil`; the list in it is still a list
        // of ints, so `xs is list[int]` has to be true.
        let held = annotation("list[int]?");
        let asked = annotation("list[int]");
        assert!(!held.same_as(&asked), "the types differ");
        assert!(held.same_args_as(&asked), "the arguments do not");
    }

    #[test]
    fn the_four_states_are_the_four_combinations() {
        // The table in `Openness`'s docs, written once more where a change to
        // either predicate has to disagree with it. `Sealed` being exactly the
        // other two at once is the claim worth pinning: it is a spelling of the
        // pair, not a third door.
        let table = [
            (Openness::Open, false, false, None),
            (Openness::Final, true, false, Some("final")),
            (Openness::Complete, false, true, Some("complete")),
            (Openness::Sealed, true, true, Some("sealed")),
        ];
        for (openness, inheritance, extension, word) in table {
            assert_eq!(openness.closes_inheritance(), inheritance, "{openness:?}");
            assert_eq!(openness.closes_extension(), extension, "{openness:?}");
            assert_eq!(openness.word(), word, "{openness:?}");
        }

        // Every state reached, so the four rows above are the whole table and not
        // four of five.
        let states: Vec<_> = table
            .iter()
            .map(|(o, ..)| (o.closes_inheritance(), o.closes_extension()))
            .collect();
        for combination in [(false, false), (true, false), (false, true), (true, true)] {
            assert!(states.contains(&combination), "{combination:?} unreachable");
        }
    }

    #[test]
    fn a_modifier_is_spelled_the_way_it_is_written() {
        // The words the parser matches and the words a report quotes back are the
        // same list, so a rename cannot land in one and not the other.
        for openness in [Openness::Final, Openness::Complete, Openness::Sealed] {
            let word = openness.word().expect("a modifier has a word");
            assert!(
                crate::syntax::token::KEYWORDS.contains(&word),
                "`{word}` is not a reserved word, so it cannot be a modifier"
            );
        }
        for visibility in [Visibility::Private, Visibility::Protected] {
            let word = visibility.word().expect("a restricting word is written");
            assert!(
                crate::syntax::token::KEYWORDS.contains(&word),
                "`{word}` is not a reserved word, so it cannot be a modifier"
            );
        }
        // `public` has no `word()` because it is the absence of one, and it is
        // still reserved — a program may write it, and every other spelling of
        // the default would then be a second way to say nothing.
        assert!(crate::syntax::token::KEYWORDS.contains(&"public"));
    }

    #[test]
    fn the_three_reaches_are_nested() {
        // The table in `Visibility`'s docs. Each word closes everything the one
        // before it closed, which is what makes them three points on one axis
        // rather than three unrelated flags — and is why `exported` can read off
        // the same order.
        let table = [
            (Visibility::Public, false, false, true, None),
            (Visibility::Protected, true, false, false, Some("protected")),
            (Visibility::Private, true, true, false, Some("private")),
        ];
        for (visibility, outside, subclass, exported, word) in table {
            assert_eq!(visibility.closes_outside(), outside, "{visibility:?}");
            assert_eq!(visibility.closes_subclass(), subclass, "{visibility:?}");
            assert_eq!(visibility.exported(), exported, "{visibility:?}");
            assert_eq!(visibility.word(), word, "{visibility:?}");
        }

        // Nesting, stated as the implication it is: anything a subclass cannot
        // reach, the outside cannot reach either.
        for (visibility, ..) in table {
            assert!(!visibility.closes_subclass() || visibility.closes_outside());
        }
    }

    #[test]
    fn writing_no_visibility_is_writing_public() {
        // The default is load-bearing: every declaration that existed before
        // v0.7 parses to `Public`, so adding the field changed no program.
        assert_eq!(Visibility::default(), Visibility::Public);
        assert_eq!(Visibility::default().word(), None);
    }
}

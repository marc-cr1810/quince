//! Supplying a generic class with its arguments, and what a type parameter
//! means inside a body.
//!
//! Two halves of one idea, v0.9 §3.1. `Stack[int]()` builds an instance whose
//! `T` is settled; a `push(item: T)` on it has to be checked against `int` when
//! it runs. The first half is [`Interp::built_generic`], the second is
//! [`substituted`], and the thing that joins them is the header — the arguments
//! are recorded on the instance and read back off it, so there is exactly one
//! place that knows.
//!
//! **`Stack[int]` is a type, not a value.** There is no class object for it and
//! there must not be: [`Interp::extensions`] is keyed by class handle, so a
//! second object for the same declaration would be a class whose `extend` block
//! half its instances could not see. DESIGN.md's *one class representation* is
//! the rule, and the arguments live on the instance instead — which is where
//! `list[int]` has kept them since v0.7 §3.9, so this adds no mechanism at all.
//!
//! What that costs is `let make = Stack[int]`, which is refused: brackets after
//! a class name are part of a construction and not an expression on their own.
//! §3.1 writes it that way everywhere, and nothing in the milestone wants the
//! partial application.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Result};
use crate::interp::Interp;
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{CallArg, Expr, TypeExpr, TypeName};
use crate::syntax::token::Span;

/// A type parameter and what it was bound to, in the order the class declared
/// them.
///
/// A slice of pairs rather than a map: a parameter list is one, two, or three
/// long in every program anyone will write, and a linear scan over three
/// entries beats hashing a string.
pub type Bindings = [(String, TypeExpr)];

impl Interp {
    /// `Stack[int](…)` — a generic class supplied with its arguments and built.
    ///
    /// Fused, the way a method call and `super.init` are fused, and for a
    /// stronger reason than either: those two avoid an allocation, and this one
    /// avoids a *value* that could not exist. `Stack[int]` denotes a type, and
    /// the language has no value that is a type-with-arguments — see the head of
    /// this file for why it must not grow one.
    ///
    /// The arguments arrive as expressions and are evaluated here, because a
    /// class is an ordinary binding and `int` is the class object it has always
    /// been. What that costs is `?` and `_`, which are annotation syntax rather
    /// than expressions: `Stack[int?]()` cannot be written. The annotation form
    /// reaches it — `let s: Stack[int?] = Stack()`, tranche 2 — and the two ways
    /// in were never going to be one, since v0.9 §3.3's `const N` makes a type
    /// argument that is genuinely a value ordinary.
    pub(super) fn built_generic(
        &mut self,
        class: ObjId,
        type_args: &[Expr],
        args: &[CallArg],
        env: ObjId,
        span: Span,
    ) -> Result<Value> {
        let name = self.heap.class(class).name.clone();
        let params = self.heap.class(class).params.clone();

        // A class that declares none is a different mistake from one given the
        // wrong number, and reporting the first as the second would suggest
        // adding an argument to a list that cannot hold any.
        if params.is_empty() {
            return Err(QuinceError::new(
                format!("`{name}` takes no type arguments"),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "`{name}` declares no type parameters, so there is nothing for `[…]` to bind — \
                 write `{name}(…)`"
            )));
        }
        if type_args.len() != params.len() {
            let (wanted, got) = (params.len(), type_args.len());
            return Err(QuinceError::new(
                format!(
                    "`{name}` takes {wanted} type {}, got {got}",
                    plural(wanted, "argument")
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!("`{name}` declares `[{}]`", params.join(", "))));
        }

        let values = self.eval_seq(type_args, env)?;
        let mut bound = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            bound.push(self.as_type_argument(value, &name, &params[index], type_args[index].span)?);
        }

        // Construction is the ordinary one. What the brackets bought is the
        // header, and `pending` is how it reaches an instance that does not
        // exist yet — the alternative was a second construction path, which is
        // how the two would drift.
        let reified = self.heap.class(class).reified(bound);
        let restore = self.pending.replace(Rc::new(reified));
        let built = self.call_evaluated(Value::Class(class), args, env, span);
        self.pending = restore;
        built
    }

    /// The value a type answers with when a declaration gives none.
    ///
    /// The run-time half of
    /// [`Parser::default_for`](crate::syntax::parser::Parser::default_for), and
    /// it exists for one case: a field annotated with a type parameter, whose
    /// default cannot be baked at parse time because what `T` stands for is not
    /// known until an instance is built. Every other declaration's default was
    /// settled by the parser and never reaches here.
    ///
    /// **This is where `Test[NoDefault]()` is refused** — the deferral the
    /// resolver makes when it meets a `T`. The report names the argument rather
    /// than the parameter, because `NoDefault` is what the caller wrote and `T`
    /// is a name inside somebody else's declaration.
    pub(super) fn default_of(
        &mut self,
        ty: &TypeExpr,
        field: &str,
        span: Span,
    ) -> Result<Value> {
        // The absent case is real and `0` is not the absent case — the same
        // rule the parser applies, spelled once more because this arrives with
        // a type rather than with an annotation.
        if ty.nullable {
            return Ok(Value::Nil);
        }
        let TypeName::Named(name) = &ty.name else {
            return Ok(Value::Nil);
        };
        let zero = match name.as_str() {
            "int" => Some(Value::Int(0)),
            "float" => Some(Value::Float(0.0)),
            "string" => Some(Value::from("")),
            "list" => Some(Value::List(self.heap.alloc(Object::List(Vec::new())))),
            "dict" => Some(Value::Dict(
                self.heap.alloc(Object::Dict(crate::runtime::dict::Dict::new())),
            )),
            "bool" => Some(Value::Bool(false)),
            _ => None,
        };
        if let Some(zero) = zero {
            return Ok(zero);
        }
        // A class, which answers with itself when it can be built from nothing.
        let Some(Value::Class(id)) = self.heap.globals(self.globals).get(name).cloned() else {
            return Err(self.no_such_type(name, span));
        };
        // The arity error a bare `NoDefault()` would raise is true and is the
        // wrong thing to lead with: the program never wrote that call, the
        // evaluator did. The message is replaced rather than annotated, so the
        // first line names the declaration the reader can actually change.
        self.call(Value::Class(id), Vec::new(), span).map_err(|_| {
            QuinceError::new(
                format!("`{name}` has no default, so `{field}` has nothing to hold"),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "`{field}` is annotated with a type parameter and this instance bound it to \
                 `{name}`, which declares an `op init` that takes arguments — give `{field}` an \
                 `= …`, or annotate it `?` so it starts absent"
            ))
        })
    }

    /// `Stack[int]` written where a value goes.
    ///
    /// The refusal the head of this file describes, in one place because both
    /// nodes that can spell it reach the same dead end — the bare `Index` form
    /// through the subscript path, and `TypeArgs` directly.
    ///
    /// Worth a report of its own rather than "a class is not indexable": the
    /// program wrote something meaningful and put it somewhere it cannot go,
    /// and the fix is a pair of parentheses.
    pub(super) fn not_a_value(&self, target: &Value, span: Span) -> crate::error::Raised {
        let Value::Class(id) = target else {
            return QuinceError::new(
                format!("{} is not indexable", target.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type);
        };
        let name = self.heap.class(*id).name.clone();
        let generic = !self.heap.class(*id).params.is_empty();
        QuinceError::new(format!("`{name}` with type arguments is a type, not a value"), span)
            .with_kind(ErrorKind::Type)
            .with_help(match generic {
                true => format!(
                    "write `{name}[…](…)` to build one, or use it as an annotation — a class \
                     with its parameters bound names a type, and there is no value for it to be"
                ),
                // The likelier mistake by far, and it is not about generics at
                // all: a subscript whose target turned out to be a class.
                false => format!(
                    "`{name}` declares no type parameters, so `[…]` after it is a subscript — \
                     and a class is not a container"
                ),
            })
    }

    /// A value written between the brackets, read as the type it names.
    ///
    /// Only a class names one. Everything else is the mistake this reports, and
    /// v0.9 §3.3's `const N: int` is the one that will stop being a mistake —
    /// which is why the report says what was found rather than only what was
    /// wanted.
    fn as_type_argument(
        &self,
        value: &Value,
        whose: &str,
        param: &str,
        span: Span,
    ) -> Result<TypeExpr> {
        match value {
            // No arguments of its own: `Stack[list[int]]` would need `list[int]`
            // to be an expression, and it is not one. The annotation form is
            // what reaches a nested argument, as it is for `?`.
            Value::Class(id) => Ok(self.heap.class(*id).reified(Vec::new())),
            other => Err(QuinceError::new(
                format!(
                    "`{whose}`\u{2019}s `{param}` is a type parameter, and {} is not a type",
                    an(other.type_name(&self.heap))
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "a type argument names a type — `Stack[int]`, `Stack[Point]` — and a value \
                 cannot stand in for one",
            )),
        }
    }

    /// What the type parameters of the method now running are bound to.
    ///
    /// Empty outside a generic method, which is where every program spends
    /// almost all of its time — and an empty binding list is what makes
    /// [`substituted`] a clone.
    pub(super) fn bindings(&self) -> &Bindings {
        self.type_bindings.last().map_or(&[], Vec::as_slice)
    }

    /// What each of a receiver's type parameters is bound to.
    ///
    /// The names come from the class and the arguments from the instance's
    /// header, which is the split the whole design rests on: a class knows what
    /// it takes and an instance knows what it got.
    ///
    /// Empty for an instance of any class written before v0.9, which is the case
    /// every caller pays for and so the one that allocates nothing.
    ///
    /// A parameter the instance carries no argument for binds to `any?`, the top
    /// type — v0.9 §3.1's unconstrained defaulting, and the same answer
    /// [`crate::sema::types::arguments_admit`] gives an elided container
    /// argument. A bare `Stack()` is dynamic in a language where an unannotated
    /// anything is.
    pub(super) fn bindings_of(&self, receiver: &Value) -> Vec<(String, TypeExpr)> {
        let Value::Instance(id) = receiver else {
            return Vec::new();
        };
        self.bindings_for(*id)
    }

    /// The same, from a handle — for the paths that have the instance before it
    /// is a [`Value`], which is every one inside construction.
    pub(super) fn bindings_for(&self, instance: ObjId) -> Vec<(String, TypeExpr)> {
        let class = self.heap.class(self.heap.instance(instance).class);
        if class.params.is_empty() {
            return Vec::new();
        }
        let params = class.params.clone();
        let held = self.heap.descriptor(instance);
        params
            .iter()
            .enumerate()
            .map(|(index, param)| {
                let arg = held
                    .as_ref()
                    .and_then(|ty| ty.args.get(index))
                    .cloned()
                    .unwrap_or_else(unconstrained);
                (param.clone(), arg)
            })
            .collect()
    }
}

/// The type an unbound parameter stands for: `any?`, the top type.
///
/// The same value [`crate::sema::types`] gives an elided container argument,
/// spelled again here rather than shared because that one is private to the
/// matching table and this one is about a *binding*. Two callers, one answer,
/// and the day they diverge the reason will be a real one.
fn unconstrained() -> TypeExpr {
    TypeExpr {
        name: TypeName::Any,
        args: Vec::new(),
        nullable: true,
        frozen: false,
        span: Span::new(0, 0),
    }
}

/// An annotation with every type parameter replaced by what it is bound to.
///
/// `list[T]` becomes `list[int]` on a `Stack[int]`, and the result is an
/// ordinary annotation that the checks already in the evaluator can be pointed
/// at without knowing generics exist. That is the whole trick: a `T` is not a
/// new kind of type, it is a type not yet written down.
///
/// Spans survive substitution — the replacement carries the *use*'s span and
/// not the argument's, so a refusal underlines the parameter the program wrote
/// rather than a bracket in another declaration.
///
/// Answers with a clone when there is nothing to replace, which is every
/// annotation in a non-generic class. A `Cow` was the obvious alternative and
/// is not worth it: the caller needs an owned `TypeExpr` either way, since the
/// checks it feeds take one by reference and outlive nothing.
pub(super) fn substituted(ty: &TypeExpr, bindings: &Bindings) -> TypeExpr {
    if bindings.is_empty() {
        return ty.clone();
    }
    let mut out = ty.clone();
    out.args = ty
        .args
        .iter()
        .map(|arg| substituted(arg, bindings))
        .collect();
    let TypeName::Named(name) = &ty.name else {
        return out;
    };
    let Some((_, bound)) = bindings.iter().find(|(param, _)| param == name) else {
        return out;
    };
    // A parameter takes no arguments of its own — `T[int]` is not a form — so
    // whatever was bound replaces the name *and* the empty argument list.
    let mut replaced = bound.clone();
    replaced.span = ty.span;
    // The two qualifiers belong to the use and not to the binding. `T?` admits
    // `nil` whatever `T` is, and a `const T` freezes what crosses it — both are
    // words the body wrote about this position, and the argument has no say in
    // either.
    replaced.nullable = replaced.nullable || ty.nullable;
    replaced.frozen = replaced.frozen || ty.frozen;
    replaced
}

fn plural(count: usize, word: &str) -> String {
    match count {
        1 => word.to_string(),
        _ => format!("{word}s"),
    }
}

/// `a` or `an`, for a type name being quoted into a sentence.
fn an(name: &str) -> String {
    match name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        true => format!("an {name}"),
        false => format!("a {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> TypeExpr {
        TypeExpr {
            name: TypeName::Named(name.to_string()),
            args: Vec::new(),
            nullable: false,
            frozen: false,
            span: Span::new(3, 4),
        }
    }

    fn generic(name: &str, args: Vec<TypeExpr>) -> TypeExpr {
        TypeExpr {
            args,
            ..named(name)
        }
    }

    #[test]
    fn a_bare_parameter_becomes_its_argument() {
        let bindings = vec![("T".to_string(), named("int"))];
        assert_eq!(substituted(&named("T"), &bindings).written(), "int");
    }

    #[test]
    fn a_parameter_nested_in_a_container_is_reached() {
        let bindings = vec![("T".to_string(), named("int"))];
        let ty = generic("list", vec![named("T")]);
        assert_eq!(substituted(&ty, &bindings).written(), "list[int]");
    }

    #[test]
    fn substitution_keeps_the_span_of_the_use() {
        let bindings = vec![(
            "T".to_string(),
            TypeExpr {
                span: Span::new(99, 100),
                ..named("int")
            },
        )];
        // The report is about the `T` the body wrote, which is at 3..4 — the
        // argument's own span is in another declaration entirely.
        assert_eq!(substituted(&named("T"), &bindings).span, Span::new(3, 4));
    }

    #[test]
    fn the_question_mark_belongs_to_the_use() {
        let bindings = vec![("T".to_string(), named("int"))];
        let optional = TypeExpr {
            nullable: true,
            ..named("T")
        };
        assert!(substituted(&optional, &bindings).admits_nil());
        // And does not leak back the other way: a `T` written plain is not
        // nullable because some other position wrote `T?`.
        assert!(!substituted(&named("T"), &bindings).admits_nil());
    }

    #[test]
    fn an_argument_that_is_itself_generic_substitutes_whole() {
        let bindings = vec![("T".to_string(), generic("list", vec![named("int")]))];
        let ty = generic("dict", vec![named("string"), named("T")]);
        assert_eq!(
            substituted(&ty, &bindings).written(),
            "dict[string, list[int]]"
        );
    }

    #[test]
    fn a_name_no_parameter_claims_is_left_alone() {
        let bindings = vec![("T".to_string(), named("int"))];
        assert_eq!(substituted(&named("Point"), &bindings).written(), "Point");
        // Including one that merely starts the same way: matching is on the
        // whole name and not on a prefix.
        assert_eq!(substituted(&named("Total"), &bindings).written(), "Total");
    }
}

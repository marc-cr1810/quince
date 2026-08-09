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
use crate::sema::types::{bound_help, satisfies};
use crate::syntax::ast::{
    CallArg, ConstArg, Expr, ExprKind, ParamKind, Slot, TypeExpr, TypeName, TypeParam, Var,
    written_params,
};
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
        // A pack takes its own position and every one after it, so the list it
        // ends is a minimum rather than a count. §3.4, and `fixed` is the
        // number of parameters before it — which is `params.len()` when there
        // is no pack, so the two cases are one comparison.
        let pack = pack_at(&params);
        let fixed = pack.unwrap_or(params.len());
        let short = type_args.len() < fixed;
        if short || (pack.is_none() && type_args.len() != fixed) {
            let (wanted, got) = (fixed, type_args.len());
            let at_least = if pack.is_some() { "at least " } else { "" };
            return Err(QuinceError::new(
                format!(
                    "`{name}` takes {at_least}{wanted} type {}, got {got}",
                    plural(wanted, "argument")
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!("`{name}` declares `[{}]`", written_params(&params))));
        }

        let values = self.eval_seq(type_args, env)?;
        let mut bound = Vec::with_capacity(values.len());
        for (index, value) in values.iter().enumerate() {
            let at = type_args[index].span;
            // Every argument at or past the pack's position belongs to it, so
            // they are all read as that one parameter — which is what makes the
            // reified list flat: `CustomTuple[int, string, bool]` records three
            // arguments, and `is` compares them one by one with no idea a pack
            // was involved.
            let param = &params[index.min(fixed)];
            let arg = self.as_argument(value, &type_args[index], &name, param, env, at)?;
            // §3.2's bound, checked here as well as at resolution — not instead
            // of it. An explicit argument list is an *expression*, so the
            // resolver never sees it as a type and cannot check it; an
            // annotation is a type, and checking it here would mean waiting for
            // a construction to report a mistake the source already showed.
            // Two places, because there are genuinely two ways in.
            if let Some(bound) = param.bound()
                && !satisfies(bound, &arg, &|name, ancestor| {
                    self.descends_by_name(name, ancestor)
                })
            {
                return Err(QuinceError::new(
                    format!(
                        "`{}` does not satisfy bound `{}`",
                        arg.written(),
                        bound.written()
                    ),
                    at,
                )
                .with_kind(ErrorKind::Type)
                .with_help(bound_help(&name, &param.name, bound)));
            }
            bound.push(arg);
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

    /// Evaluates an initializer with the annotation's arguments in hand.
    ///
    /// v0.9 §3.1's inference: `let s: Stack[int] = Stack()` binds `T` to `int`
    /// from the left-hand side, because writing it twice is noise and the
    /// annotation is the more reliable of the two places to put it.
    ///
    /// Why it has to happen *here* rather than when the value is checked
    /// against the annotation a moment later: the fields are initialized inside
    /// the construction, and a field annotated `list[T]` is stamped as it
    /// crosses. By the time `coerced_from` sees the instance the list inside it
    /// already exists and is already described. The annotation has to arrive
    /// before the constructor runs or it arrives too late to mean anything.
    ///
    /// Restores whatever was pending, so a construction nested inside the
    /// initializer of another cannot see the outer one's header — the same
    /// reason [`Interp::built_generic`] restores rather than clears.
    pub(super) fn evaluated_as(
        &mut self,
        ty: Option<&TypeExpr>,
        value: &Expr,
        env: ObjId,
    ) -> Result<Value> {
        let Some(header) = inferred_header(ty, value) else {
            return self.eval(value, env);
        };
        let restore = self.pending.replace(Rc::new(header));
        let built = self.eval(value, env);
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
        // A tuple, whose arity is settled by the time this runs even though it
        // was not where the field was written — which is the whole reason the
        // resolver defers a pack rather than refusing it. §3.5 refuses
        // `let t: tuple[int, string]` because there is no *empty* value of that
        // type to synthesize, and this does not contradict it: there is no
        // empty tuple to reach for here either, so each element answers with
        // its own zero and a type with none refuses below, exactly as `T` does.
        if name == "tuple" {
            let mut items = Vec::with_capacity(ty.args.len());
            for (index, arg) in ty.args.clone().iter().enumerate() {
                // A pack nothing expanded stands for an unknown number of
                // elements, and zero is the only count it can be given: there
                // is no position here to name a type for, so there is no value
                // to invent. `CustomTuple()` starts its `data` as `()`.
                if matches!(arg.name, TypeName::Pack(_)) {
                    continue;
                }
                items.push(self.default_of(arg, &format!("{field}[{index}]"), span)?);
            }
            return Ok(Value::Tuple(self.heap.alloc(Object::Tuple(items))));
        }
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

    /// Whether the class called `name` is `ancestor` or descends from it.
    ///
    /// By name, because a bound is written as a name and the argument arrives
    /// as one. The hierarchy is real class objects here, unlike the resolver's
    /// by-name approximation of it — but the question and the answer are the
    /// same, which is what lets [`satisfies`] be one function.
    fn descends_by_name(&self, name: &str, ancestor: &str) -> bool {
        let Some(Value::Class(id)) = self.heap.globals(self.globals).get(name) else {
            return false;
        };
        let mut current = Some(*id);
        while let Some(id) = current {
            if self.heap.class(id).name == ancestor {
                return true;
            }
            current = self.heap.class(id).parent;
        }
        false
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

    /// One thing written between the brackets, read as what its parameter wants.
    ///
    /// Two parameter forms and so two rules, and the point of doing both here is
    /// that a program writes them in one list: `Buffer[float, 16]` has a type in
    /// the first position and a value in the second, and which is wanted is a
    /// fact about the *declaration*, not about what was written. So a mismatch
    /// either way reports what the parameter is, which is the thing the reader
    /// has to look up otherwise.
    fn as_argument(
        &self,
        value: &Value,
        expr: &Expr,
        whose: &str,
        param: &TypeParam,
        env: ObjId,
        span: Span,
    ) -> Result<TypeExpr> {
        match &param.kind {
            ParamKind::Const { ty } => {
                self.as_const_argument(value, expr, whose, param, ty, env, span)
            }
            // A pack's arguments are ordinary types, one per position — what a
            // pack changes is how many of them there are, and that is settled
            // by the caller before this is asked about any one of them.
            ParamKind::Pack | ParamKind::Type { .. } => match value {
                // No arguments of its own: `Stack[list[int]]` would need
                // `list[int]` to be an expression, and it is not one. The
                // annotation form is what reaches a nested argument, as it is
                // for `?`.
                Value::Class(id) => Ok(self.heap.class(*id).reified(Vec::new())),
                other => Err(QuinceError::new(
                    format!(
                        "`{whose}`\u{2019}s `{}` is a type parameter, and {} is not a type",
                        param.name,
                        an(other.type_name(&self.heap))
                    ),
                    span,
                )
                .with_kind(ErrorKind::Type)
                .with_help(format!(
                    "a type argument names a type — `Stack[int]`, `Stack[Point]`. Write \
                     `const {}: …` in the declaration if it was meant to take a value",
                    param.name
                ))),
            },
        }
    }

    /// The `16` in `Buffer[float, 16]` — v0.9 §3.3.
    ///
    /// Two things have to be true, and they are different questions. The value
    /// has to have the type the parameter declared, which is ordinary checking.
    /// And it has to be *constant*, which is not about the value at all: it is
    /// about how the expression was written, because a type argument becomes
    /// part of a type identity and a type that could differ between two
    /// evaluations of the same source is not a type.
    ///
    /// Constant means a literal, or a name bound once — `final` or `const`.
    /// §3.3 says "a `const` binding"; a `final` one is no less fixed, and the
    /// difference between the two words is about what they freeze, which a
    /// const argument does not care about since it reads the value out and
    /// keeps a copy.
    fn as_const_argument(
        &self,
        value: &Value,
        expr: &Expr,
        whose: &str,
        param: &TypeParam,
        ty: &TypeExpr,
        env: ObjId,
        span: Span,
    ) -> Result<TypeExpr> {
        let constant = match &expr.kind {
            ExprKind::Int(_) | ExprKind::Str(_) | ExprKind::Bool(_) => true,
            ExprKind::Var(var) => self.is_fixed(var, env),
            _ => false,
        };
        if !constant {
            return Err(QuinceError::new(
                format!(
                    "`{whose}`\u{2019}s `{}` needs a constant, and this is worked out when it runs",
                    param.name
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "`{}` is part of the type, so it has to be the same every time the line is \
                 read — write a literal, or a name declared `const`",
                param.name
            )));
        }

        let held = match value {
            Value::Int(n) => Some(ConstArg::Int(*n)),
            Value::Bool(b) => Some(ConstArg::Bool(*b)),
            Value::Str(text) => Some(ConstArg::Str(text.to_string())),
            _ => None,
        };
        // The declared type, checked against what the constant actually is. The
        // three that can be constants are exactly the three a `const` parameter
        // may be declared as, so a value outside them fails this and needs no
        // separate report.
        let wanted = match &ty.name {
            TypeName::Named(name) => name.as_str(),
            _ => "",
        };
        match held {
            Some(held) if held.type_name() == wanted => Ok(TypeExpr {
                name: TypeName::Const(held),
                args: Vec::new(),
                applied: false,
                nullable: false,
                frozen: false,
                span,
            }),
            _ => Err(QuinceError::new(
                format!(
                    "`{whose}`\u{2019}s `{}` is `{wanted}`, but this is {}",
                    param.name,
                    an(value.type_name(&self.heap))
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "the declaration writes `{}`, so the argument in that position is {} value",
                param.written(),
                an(wanted)
            ))),
        }
    }

    /// The value a const generic parameter holds in the body now running.
    ///
    /// `None` for every name in every frame of a program that declares no const
    /// parameter, which is what makes this safe to ask on the failing path of a
    /// global read — see [`Interp::read`]. v0.9 §3.3's "`N` is in scope in the
    /// body as a value, read-only".
    ///
    /// Read-only falls out rather than being enforced: there is no slot to
    /// assign to, so `N = 5` is the resolver's "cannot assign to an undeclared
    /// name" and never reaches here.
    pub(super) fn const_binding(&self, name: &str) -> Option<Value> {
        let (_, bound) = self.bindings().iter().find(|(param, _)| param == name)?;
        let TypeName::Const(value) = &bound.name else {
            // A *type* parameter of that name. `T` names a type and not a
            // value, which is §3.1's rule and the reason `T()` is refused —
            // answering with anything here would be inventing one.
            return None;
        };
        Some(match value {
            ConstArg::Int(n) => Value::Int(*n),
            ConstArg::Bool(b) => Value::Bool(*b),
            ConstArg::Str(text) => Value::from(text.as_str()),
        })
    }

    /// Whether a name is bound once and never reassigned.
    ///
    /// Both scopes, because a `const` at the top level and one inside a
    /// function are the same promise reached two different ways.
    fn is_fixed(&self, var: &Var, env: ObjId) -> bool {
        match crate::interp::resolved(&var.slot) {
            Slot::Local { hops, index } => {
                let scope = crate::runtime::env::ancestor(&self.heap, env, hops);
                !self.heap.env(scope).bind_kind(index).mutable()
            }
            Slot::Global => self
                .heap
                .globals(crate::runtime::env::module_of(&self.heap, env))
                .is_fixed(&var.name),
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
        let args = held.as_ref().map_or(&[][..], |ty| &ty.args);
        params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| {
                // A pack binds to *all* the arguments from its position on,
                // gathered into a `tuple` — which is what makes `tuple[Ts...]`
                // and `args: Ts...` both work by ordinary substitution: the
                // first splices the arguments back out, and the second is the
                // collected value's type as written. §3.4.
                if param.is_pack() {
                    // Unless nothing described the instance at all, in which
                    // case the pack is left unbound rather than bound to
                    // nothing: `CustomTuple()` with no annotation is dynamic,
                    // and a pack bound to the empty sequence would say the
                    // opposite — that it takes no arguments. §3.1's defaulting
                    // is "unconstrained", and for a pack that is spelled by
                    // leaving `Ts...` where it was. `CustomTuple[]` *is*
                    // described, with an empty argument list, and does bind.
                    let rest = held.as_ref().and_then(|_| args.get(index..))?;
                    return Some((param.name.clone(), packed(rest.to_vec())));
                }
                let arg = args.get(index).cloned().unwrap_or_else(unconstrained);
                Some((param.name.clone(), arg))
            })
            .collect()
    }
}

/// The header an annotation lends to the initializer beside it, if any.
///
/// Deliberately *syntactic*, and narrow: the annotation must carry arguments,
/// and the initializer must be a bare construction of the very class it names.
/// `let s: Stack[int] = Stack()` qualifies and almost nothing else does.
///
/// The alternative considered was to pend the header for any initializer and
/// let construction take it if the class name matched. That reaches further —
/// `let s: Stack[int] = wrap(Stack())` would bind the inner one — and it
/// reaches further than anyone can predict, which is the objection: an
/// annotation on the left silently reconfiguring a construction three calls
/// down is not inference, it is spooky action. §3.1 writes the rule as one
/// sentence about one shape, and this is that shape.
///
/// A written argument list wins outright — `let s: Stack[int] = Stack[string]()`
/// is a disagreement to report and not a gap to fill, and
/// [`Interp::built_generic`] is what reports it, having pended its own header
/// over this one.
fn inferred_header(ty: Option<&TypeExpr>, value: &Expr) -> Option<TypeExpr> {
    let ty = ty?;
    // Written brackets and not merely arguments, because `CustomTuple[]` binds
    // a pack to the empty sequence and that is a header worth lending — it is
    // the difference between "takes nothing" and "takes whatever". §3.4.
    if !ty.applied {
        return None;
    }
    let TypeName::Named(named) = &ty.name else {
        return None;
    };
    let ExprKind::Call { callee, .. } = &value.kind else {
        return None;
    };
    let ExprKind::Var(var) = &callee.kind else {
        return None;
    };
    if &var.name != named {
        return None;
    }
    // Stripped of the qualifiers, which describe the *binding* and not what the
    // instance holds: `const Stack[int]` freezes what crosses the annotation,
    // and a header saying `const` would freeze every later reader of it.
    Some(TypeExpr {
        nullable: false,
        frozen: false,
        ..ty.clone()
    })
}

/// Where a pack sits in a parameter list, if there is one.
///
/// Always the last position — the parser refuses anything after it — so this
/// doubles as "how many parameters take exactly one argument".
fn pack_at(params: &[TypeParam]) -> Option<usize> {
    params.iter().position(TypeParam::is_pack)
}

/// What a pack is bound to: the arguments it took, as a `tuple`.
///
/// A tuple and not a bare list of types, because it has to be a [`TypeExpr`] —
/// `Bindings` maps a name to one type, and a pack is several. `tuple` is the
/// honest head for it: `op init(args: Ts...)` collects its arguments into
/// exactly this value at run time, so the annotation a substituted `Ts...`
/// becomes is the type of the thing the body holds. §3.4.
fn packed(args: Vec<TypeExpr>) -> TypeExpr {
    TypeExpr {
        name: TypeName::Named("tuple".to_string()),
        applied: true,
        args,
        nullable: false,
        frozen: false,
        span: Span::new(0, 0),
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
        applied: false,
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
    // A pack in an argument position *splices*: `tuple[Ts...]` with `Ts` bound
    // to three types becomes a `tuple` with three arguments, not one argument
    // that is itself a tuple. This is the whole of what makes a pack different
    // from a parameter, and it is why the argument walk is a `flat_map`.
    out.args = ty
        .args
        .iter()
        .flat_map(|arg| expanded(arg, bindings))
        .collect();
    if let TypeName::Pack(name) = &ty.name {
        // A pack standing where a whole type goes — `args: Ts...` — is the
        // collected tuple itself. Nothing to splice into, so the binding is
        // used as it is.
        let Some((_, bound)) = bindings.iter().find(|(param, _)| param == name) else {
            return out;
        };
        let mut replaced = bound.clone();
        replaced.span = ty.span;
        replaced.nullable = replaced.nullable || ty.nullable;
        replaced.frozen = replaced.frozen || ty.frozen;
        return replaced;
    }
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

/// One argument, substituted — as however many types it turns into.
///
/// One for everything but a bound pack, which is the reason this hands back a
/// list at all. See [`substituted`].
fn expanded(arg: &TypeExpr, bindings: &Bindings) -> Vec<TypeExpr> {
    let TypeName::Pack(name) = &arg.name else {
        return vec![substituted(arg, bindings)];
    };
    match bindings.iter().find(|(param, _)| param == name) {
        // The arguments the pack took, spliced in where it was written. Each is
        // already a finished type — a pack binds to what a construction was
        // given, and a type argument cannot itself be a pack.
        Some((_, bound)) => bound.args.clone(),
        // Nothing bound it, so it stands for itself. A bare `CustomTuple()`
        // leaves this, and `holds` reads an unsubstituted pack as constraining
        // nothing.
        None => vec![arg.clone()],
    }
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
            applied: false,
            name: TypeName::Named(name.to_string()),
            args: Vec::new(),
            nullable: false,
            frozen: false,
            span: Span::new(3, 4),
        }
    }

    fn generic(name: &str, args: Vec<TypeExpr>) -> TypeExpr {
        TypeExpr {
            applied: !args.is_empty(),
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

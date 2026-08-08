//! Calling things, and constructing them.
//!
//! A function, a native, a bound method, a class: four callees and one entry
//! point. Arity checking and the receiver-prepending that makes a method call
//! work sit here too.
//!
//! v0.8's overload dispatch is a change to this file and to almost nothing else —
//! choosing among several declarations sharing a name happens where the argument
//! values are already in hand.


use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::interp::error::{an, check_arity, does_not_hold, frozen};
use crate::interp::{Attr, Flow, Interp, MAX_DEPTH, out_of_stack};
use crate::runtime::class::{BUILTINS as BUILTIN_TYPES, Builtin, Instance};
use crate::runtime::dict::{Dict, Key};
use crate::runtime::env::Env;
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::{Native, Value};
use crate::interp::generic::substituted;
use crate::sema::types::{arguments_admit, holds};
use crate::syntax::ast::{CallArg, Expr, ExprKind, FnDecl, Op, Param, TypeExpr, TypeName};
use crate::syntax::token::Span;

/// What a refusal knows about where the value it refused was written.
///
/// Two independent facts, and the reason they are one type is that they were
/// once one flag and that was wrong. A caller may have the expression that
/// produced the value and still be checking it against an annotation written in
/// text this error will not be drawn against — which is exactly a write to a
/// name declared on an earlier line of another file, or at a prompt, an earlier
/// entry.
#[derive(Clone, Copy, Default)]
pub(super) struct Written<'a> {
    /// Where the value was written, when that is narrower than the statement
    /// the caller would otherwise underline.
    at: Option<Span>,
    /// The expression that produced the value, when the caller has one.
    ///
    /// It buys precision a span cannot: by the time a list is checked it is a
    /// heap object whose elements have no spans, so `[1, 2, 3, "hi"]` failing
    /// on its fourth element could only underline the whole literal. With the
    /// literal in hand the caret lands on `"hi"`.
    expr: Option<&'a Expr>,
    /// Whether [`TypeExpr::span`] indexes the same text `at` does, so that a
    /// second label may point at the annotation that did the refusing.
    ///
    /// A [`Span`] is a byte range and nothing else — it does not know which
    /// source it came from — so a label placed at one is only meaningful while
    /// both belong to the same string.
    annotation_is_here: bool,
}

impl<'a> Written<'a> {
    /// Nothing: the value arrived from somewhere with no span of its own — an
    /// argument settled from a default, a container assembled elsewhere — and
    /// the caller's span is the whole of what a report can say.
    pub(super) fn nowhere() -> Self {
        Self::default()
    }

    /// The initializer of the declaration being executed, so the annotation is
    /// the one a few characters to its left.
    pub(super) fn declared(expr: &'a Expr) -> Self {
        Self { at: Some(expr.span), expr: Some(expr), annotation_is_here: true }
    }

    /// The right-hand side of a write to a name declared earlier. The value is
    /// here and the caret belongs on it; the annotation is on a declaration
    /// that may be in another file, or at a prompt in an entry now off screen.
    pub(super) fn rebound(expr: &'a Expr) -> Self {
        Self { at: Some(expr.span), expr: Some(expr), annotation_is_here: false }
    }

    /// A value the program computed rather than wrote: what `a += b` produced,
    /// which is neither `a` nor `b`. The span is the whole expression, because
    /// that is the thing that made the value.
    pub(super) fn produced(span: Span) -> Self {
        Self { at: Some(span), ..Self::default() }
    }

    /// The span to point at, falling back to the caller's.
    pub(super) fn caret(&self, span: Span) -> Span {
        self.at.unwrap_or(span)
    }

    /// Where a container literal's `index`th element was written.
    ///
    /// `None` for every value that did not come from one — returned from a
    /// function, or read out of another name — where the best the caret can do
    /// is the expression as a whole.
    fn element(&self, index: usize) -> Option<&'a Expr> {
        match &self.expr?.kind {
            ExprKind::List(items) => items.get(index),
            _ => None,
        }
    }

    /// The same knowledge, one level down: a sub-expression of the value, for a
    /// nested annotation refused inside a nested literal.
    ///
    /// `annotation_is_here` carries over unchanged. `list[list[int]]` is one
    /// annotation however deep the report reaches into it, so if its outermost
    /// span is in this text then the `int` inside it is too.
    fn within(&self, expr: Option<&'a Expr>) -> Self {
        Self {
            at: expr.map(|expr| expr.span),
            expr,
            annotation_is_here: self.annotation_is_here,
        }
    }

    /// The pairs of a dict literal, when there are exactly as many of them as
    /// the dict holds.
    ///
    /// Position is what lets the caret find the entry in the literal that
    /// produced a refused key or value, and it is sound only while the two line
    /// up. A repeated key breaks that — `{"a": 1, "a": 2}` is two entries
    /// written and one stored — so the length is checked before any index is
    /// trusted, and the whole literal is underlined when it is not.
    fn pairs(&self, held: usize) -> Option<&'a [(Expr, Expr)]> {
        match &self.expr?.kind {
            ExprKind::Dict(pairs) if pairs.len() == held => Some(pairs.as_slice()),
            _ => None,
        }
    }
}

/// A call with more positional arguments than there are parameters.
///
/// Separate from [`check_arity`] because a declaration with defaults has a
/// *range* rather than a count, and "takes 3 arguments" is false for one that
/// also accepts 1.
fn too_many_arguments(name: &str, params: &[Param], given: usize, span: Span) -> Raised {
    let required = params.iter().filter(|param| param.default.is_none()).count();
    let takes = match required == params.len() {
        true => format!("{required}"),
        false => format!("{required} to {}", params.len()),
    };
    QuinceError::new(
        format!("`{name}` takes {takes} arguments, but {given} were given"),
        span,
    )
    .with_kind(ErrorKind::Type)
}

/// A named argument that no parameter answers to.
fn no_such_parameter(name: &str, params: &[Param], written: &str, span: Span) -> Raised {
    let declared: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
    let err = QuinceError::new(
        format!("`{name}` has no parameter called `{written}`"),
        span,
    )
    .with_kind(ErrorKind::Type);
    match crate::error::did_you_mean(written, declared.clone()) {
        Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
        None if declared.is_empty() => {
            err.with_help(format!("`{name}` takes no arguments at all"))
        }
        None => err.with_help(format!("it takes: {}", declared.join(", "))),
    }
}

/// The declaration a call is about to reach.
///
/// What [`Interp::arranged`] needs to turn what a caller *wrote* into the
/// positional vector a call takes: which parameters exist, what they default to,
/// and the scope those defaults are evaluated in.
pub(super) struct Shape {
    decl: Rc<FnDecl>,
    /// The callee's declaration scope, which is where a default expression is
    /// evaluated — §3.6. Not the caller's: a default is part of the declaration
    /// and reads the names the declaration could see.
    env: ObjId,
    /// Whether a receiver goes in front of what the caller wrote, so the
    /// parameters a name can reach start one along.
    receiver: bool,
}

impl Interp {
    /// The declaration a call reaches, once the overload set has been narrowed
    /// to one candidate.
    ///
    /// `None` for a native and for anything not callable. A builtin declares its
    /// parameters in a static table with no defaults and a receiver offset of
    /// its own, so it takes what it is given positionally and reports for
    /// itself — see [`Interp::arranged`].
    pub(super) fn shape_for(
        &self,
        target: &Value,
        args: &[CallArg],
        values: &[Value],
        span: Span,
    ) -> Result<Option<Shape>> {
        // Two of the four callees put a receiver in front of what the caller
        // wrote, so the parameters a name can reach start one along.
        let (callee, receiver) = match target {
            // Calling a class runs its `op init`, and the instance it is handed
            // is the receiver nobody writes.
            Value::Class(id) => (self.heap.class(*id).slot(Op::Init).cloned(), true),
            Value::BoundMethod(id) => (Some(self.heap.bound_method(*id).method.clone()), true),
            other => (Some(other.clone()), false),
        };
        let Some(callee) = callee else {
            return Ok(None);
        };
        self.shape_of_chosen(&self.selected(&callee, args, values, receiver, span)?, receiver)
    }

    /// The same for a value already known to be called with a receiver in front.
    pub(super) fn shape_for_method(
        &self,
        method: &Value,
        args: &[CallArg],
        values: &[Value],
        span: Span,
    ) -> Result<Option<Shape>> {
        self.shape_of_chosen(&self.selected(method, args, values, true, span)?, true)
    }

    fn shape_of_chosen(&self, chosen: &Value, receiver: bool) -> Result<Option<Shape>> {
        let Value::Function(id) = chosen else {
            return Ok(None);
        };
        Ok(Some(Shape {
            decl: Rc::clone(&self.heap.function(*id).decl),
            env: self.heap.function(*id).env,
            receiver,
        }))
    }

    /// What a report about this call quotes back.
    ///
    /// Not [`Value::callable_name`] for the two indirect callees: calling a
    /// class runs its `op init`, and a bound method is the method it bound, so
    /// those are the names an arity mismatch has always named and the corpus
    /// pins. A class with no constructor is the one case where the class's own
    /// name is what there is to say.
    pub(super) fn called_name(&self, target: &Value) -> String {
        match target {
            Value::Class(id) => match self.heap.class(*id).slot(Op::Init) {
                Some(init) => init.callable_name(&self.heap).to_string(),
                None => self.heap.class(*id).name.clone(),
            },
            Value::BoundMethod(id) => self.heap.bound_method(*id).method
                .callable_name(&self.heap)
                .to_string(),
            other => other.callable_name(&self.heap).to_string(),
        }
    }

    /// Chooses the declaration an overloaded name reaches — v0.8 §3.5.
    ///
    /// Anything that is not a set answers with itself, which is what makes this
    /// safe to call on every callee: an ordinary function is a set of one, and
    /// paying for the check would be paying for a feature the call does not use.
    ///
    /// Dispatch is on the argument *values*, exact match before widened, which
    /// is v0.7 §4.1's rule and not a second one. A tie cannot arise for a
    /// program the resolver accepted — two candidates some call reaches equally
    /// well are refused where they are declared — so the first declared wins and
    /// the choice is deterministic either way.
    pub(super) fn selected(
        &self,
        callee: &Value,
        args: &[CallArg],
        values: &[Value],
        receiver: bool,
        span: Span,
    ) -> Result<Value> {
        let Value::Overload(id) = callee else {
            return Ok(callee.clone());
        };
        let candidates = self.heap.overload(*id);
        let mut best: Option<(u32, &Value)> = None;
        // Whether the best score is shared. The resolver refuses the pairs it
        // can see from the annotations alone, but two it cleared can still meet
        // on a value the annotations do not describe — an empty list is a
        // `list[int]` and a `list[string]` at once until something says which.
        // Answering with whichever was written first would be the "two things
        // matched" §3.5 exists to rule out, so it is a refusal.
        let mut tied = false;
        for candidate in candidates {
            let Value::Function(func) = candidate else {
                continue;
            };
            let decl = &self.heap.function(*func).decl;
            let params = &decl.params[usize::from(receiver)..];
            let Some(score) = self.fits(params, args, values) else {
                continue;
            };
            match best {
                Some((found, _)) if score == found => tied = true,
                // Lower is better: an exact type beats a widening.
                Some((found, _)) if score < found => {
                    best = Some((score, candidate));
                    tied = false;
                }
                None => best = Some((score, candidate)),
                _ => {}
            }
        }
        match best {
            Some(_) if tied => Err(self.two_matched(candidates, values, receiver, span)),
            Some((_, chosen)) => Ok(chosen.clone()),
            None => Err(self.nothing_matched(candidates, values, receiver, span)),
        }
    }

    /// How well a call fits one declaration, or `None` if it does not.
    ///
    /// The same three steps [`Interp::arranged`] takes, asked rather than done:
    /// positional arguments fill left to right, named ones fill by name, and
    /// what is left has to have a default. The score is the sum of how well each
    /// argument fits, so a lower one is a better match and an exact type beats a
    /// widening.
    fn fits(&self, params: &[Param], args: &[CallArg], values: &[Value]) -> Option<u32> {
        // An internal call passes values with no `CallArg`s beside them, which
        // is every argument positional.
        let positional = match args.is_empty() {
            true => values.len(),
            false => args.iter().take_while(|arg| arg.name.is_none()).count(),
        };
        if positional > params.len() {
            return None;
        }
        let mut filled: Vec<Option<&Value>> = vec![None; params.len()];
        for (slot, value) in filled.iter_mut().zip(values.iter().take(positional)) {
            *slot = Some(value);
        }
        // A keyword call selects among overloads by name as well as by type: a
        // candidate with no parameter of that name is not one this call could
        // have meant.
        for (arg, value) in args.iter().zip(values).skip(positional) {
            let (written, _) = arg.name.as_ref()?;
            let index = params.iter().position(|param| &param.name == written)?;
            if filled[index].is_some() {
                return None;
            }
            filled[index] = Some(value);
        }

        let mut score = 0;
        for (param, held) in params.iter().zip(&filled) {
            match held {
                Some(value) => score += u32::from(self.quality(param, value)?),
                None if param.default.is_some() => {}
                None => return None,
            }
        }
        Some(score)
    }

    /// How well `value` fits `param`, or `None` if it does not fit at all.
    ///
    /// Three levels, matching `sema::overload`'s: the parameter's own type
    /// exactly, a widening the language performs at the boundary, and a
    /// parameter that takes anything. The two files have to agree, because one
    /// predicts from the annotations what the other measures from the values.
    fn quality(&self, param: &Param, value: &Value) -> Option<u8> {
        let Some(ty) = &param.ty else {
            return Some(2);
        };
        if !holds(ty, value, &self.heap) {
            return None;
        }
        match &ty.name {
            TypeName::Any => Some(2),
            // A `nil` reaching a nullable is a widening: the annotation admits
            // more than the value is.
            TypeName::Named(_) if matches!(value, Value::Nil) => Some(1),
            TypeName::Named(named) => {
                if value.type_name(&self.heap) != named || ty.nullable {
                    return Some(1);
                }
                // The same three levels one argument deeper. Once `list[any]`
                // admits a `list[int]` — which is the point of `any` being the
                // top type — the class name alone stops telling the two apart,
                // and a program declaring both could call neither.
                Some(u8::from(!self.describes_exactly(ty, value)))
            }
        }
    }

    /// Whether `ty`'s arguments are the ones the value's header actually
    /// carries, rather than merely ones that admit them.
    ///
    /// Identity here and admission in [`arguments_admit`], deliberately: this is
    /// not asking whether the value fits, which the caller has already settled.
    /// It is ranking two declarations that both fit.
    ///
    /// A container nothing described matches only a parameter that asked for
    /// nothing. It is not an exact `list[int]` — nothing ever said it was one —
    /// so `total([])` between a `list[int]` and a `list[string]` stays the tie
    /// §3.5 refuses rather than silently picking the first.
    fn describes_exactly(&self, ty: &TypeExpr, value: &Value) -> bool {
        match value
            .base(&self.heap)
            .handle()
            .and_then(|id| self.heap.descriptor(id))
        {
            Some(held) => ty.same_args_as(&held),
            None => ty.args.is_empty(),
        }
    }

    /// The same, checking the fit even where there is only one candidate.
    ///
    /// [`Interp::selected`] answers with a lone declaration without asking,
    /// because an ordinary call reports a wrong argument against the parameter
    /// that refused it — and "`host` is `string`, but this is an int" says more
    /// than a list of signatures would. An operator has no such report to fall
    /// back on: the parameter is one the program never wrote and the caret would
    /// land on a declaration somewhere else. So the operators use this, and
    /// `None` is the answer they turn into a report of their own.
    pub(super) fn fitting(&self, callee: &Value, args: &[Value], receiver: bool) -> Option<Value> {
        let candidates = match callee {
            Value::Overload(id) => self.heap.overload(*id).to_vec(),
            other => vec![other.clone()],
        };
        let mut best: Option<(u32, Value)> = None;
        for candidate in candidates {
            let Value::Function(func) = &candidate else {
                continue;
            };
            let decl = Rc::clone(&self.heap.function(*func).decl);
            let params = &decl.params[usize::from(receiver)..];
            let Some(score) = self.fits(params, &[], args) else {
                continue;
            };
            if best.as_ref().is_none_or(|(found, _)| score < *found) {
                best = Some((score, candidate));
            }
        }
        best.map(|(_, chosen)| chosen)
    }

    /// How each declaration under a name reads, as a parameter list.
    ///
    /// `(int), (string)` — what a report says the name *does* take, which is the
    /// question a reader has the moment they are told their call does not match.
    pub(super) fn signatures(&self, callee: &Value, receiver: bool) -> Vec<String> {
        let candidates = match callee {
            Value::Overload(id) => self.heap.overload(*id).to_vec(),
            other => vec![other.clone()],
        };
        candidates
            .iter()
            .filter_map(|candidate| match candidate {
                Value::Function(id) => Some(Rc::clone(&self.heap.function(*id).decl)),
                _ => None,
            })
            .map(|decl| {
                let params: Vec<String> = decl.params[usize::from(receiver)..]
                    .iter()
                    .map(|param| match &param.ty {
                        Some(ty) => ty.written(),
                        None => "any".to_string(),
                    })
                    .collect();
                format!("({})", params.join(", "))
            })
            .collect()
    }

    /// The report for a call two declarations take equally well.
    ///
    /// Rare by construction — the resolver refuses every pair it can see from
    /// the annotations — and reached by the ones it cannot: a container the
    /// program never described, which is `[]` and `{}` written straight into
    /// the call. The fix is to say which, so the report says so.
    fn two_matched(
        &self,
        candidates: &[Value],
        values: &[Value],
        receiver: bool,
        span: Span,
    ) -> Raised {
        let name = candidates
            .first()
            .map_or("this", |first| first.callable_name(&self.heap));
        let given: Vec<&str> = values.iter().map(|v| v.type_name(&self.heap)).collect();
        let written: Vec<String> = candidates
            .iter()
            .flat_map(|candidate| self.signatures(candidate, receiver))
            .collect();
        QuinceError::new(
            format!("more than one `{name}` takes ({})", given.join(", ")),
            span,
        )
        .with_kind(ErrorKind::Type)
        .with_help(format!(
            "the declarations under that name take: {} — an empty container is every element \
             type at once, so say which with an annotated binding, or name the one you meant",
            written.join(", ")
        ))
    }

    /// The report for a call no declaration under the name could take.
    fn nothing_matched(
        &self,
        candidates: &[Value],
        values: &[Value],
        receiver: bool,
        span: Span,
    ) -> Raised {
        let name = candidates
            .first()
            .map_or("this", |first| first.callable_name(&self.heap));
        let given: Vec<&str> = values.iter().map(|v| v.type_name(&self.heap)).collect();
        let written: Vec<String> = candidates
            .iter()
            .flat_map(|candidate| self.signatures(candidate, receiver))
            .collect();
        QuinceError::new(
            format!("no `{name}` takes ({})", given.join(", ")),
            span,
        )
        .with_kind(ErrorKind::Type)
        .with_help(format!(
            "the declarations under that name take: {}",
            written.join(", ")
        ))
    }

    /// Turns the arguments a call wrote into the positional vector it takes.
    ///
    /// Three things happen here and nowhere else, in this order — which is the
    /// order §3.6 requires:
    ///
    /// 1. Positional arguments fill parameters left to right.
    /// 2. Each named argument fills the parameter it names, refusing a name no
    ///    parameter answers to and a parameter already filled.
    /// 3. Every parameter still empty takes its default, **evaluated now**, in
    ///    the callee's declaration scope. `fn f(xs: list = [])` therefore builds
    ///    a fresh list per call and carries no mutation between them.
    ///
    /// A call whose callee has no readable declaration — a builtin — passes
    /// through untouched, and a named argument to one is refused: a native's
    /// parameters are a static table it reports against itself, and there is
    /// nothing there to default.
    pub(super) fn arranged(
        &mut self,
        shape: Option<Shape>,
        args: &[CallArg],
        values: Vec<Value>,
        name: &str,
        span: Span,
    ) -> Result<Vec<Value>> {
        let named = args.iter().any(|arg| arg.name.is_some());
        let Some(shape) = shape else {
            if let Some((written, at)) = args.iter().find_map(|arg| arg.name.clone()) {
                return Err(QuinceError::new(
                    format!("`{name}` does not take arguments by name"),
                    at,
                )
                .with_kind(ErrorKind::Type)
                .with_help(format!(
                    "it is built into the language, and its parameters are positional — pass \
                     the value for `{written}` in order"
                )));
            }
            return Ok(values);
        };

        let params = &shape.decl.params[usize::from(shape.receiver)..];
        // The familiar report, for the shape that has always produced it: no
        // defaults and no names, where "takes N arguments" is the whole truth.
        // Worth keeping rather than folding into the general case below, because
        // it is what every existing program and every corpus case reads back.
        if !named && params.iter().all(|param| param.default.is_none()) {
            check_arity(name, params.len(), values.len(), span)?;
            return Ok(values);
        }

        let positional = args.iter().take_while(|arg| arg.name.is_none()).count();
        if positional > params.len() {
            return Err(too_many_arguments(name, params, args.len(), span));
        }

        let mut filled: Vec<Option<Value>> = vec![None; params.len()];
        for (slot, value) in filled.iter_mut().zip(values.iter().take(positional)) {
            *slot = Some(value.clone());
        }

        for (arg, value) in args.iter().zip(&values).skip(positional) {
            let (written, at) = arg.name.clone().expect("named arguments follow positional ones");
            let Some(index) = params.iter().position(|param| param.name == written) else {
                return Err(no_such_parameter(name, params, &written, at));
            };
            if filled[index].is_some() {
                return Err(QuinceError::new(
                    format!("`{written}` is given twice"),
                    at,
                )
                .with_kind(ErrorKind::Type)
                .with_help(match index < positional {
                    true => format!(
                        "argument {} already filled it — a parameter is filled once, and \
                         last-wins would make which value arrives depend on a reading order \
                         nobody agrees on",
                        index + 1
                    ),
                    false => "a parameter is filled once — delete one of them".to_string(),
                }));
            }
            filled[index] = Some(value.clone());
        }

        // The values so far are held in a Rust local while the defaults run, and
        // a default is an arbitrary expression that can reach a safe point.
        let mark = self.temps.len();
        self.temps
            .extend(filled.iter().flatten().filter(|v| v.handle().is_some()).cloned());
        let arranged = self.fill_defaults(&shape, params, filled, name, span);
        self.temps.truncate(mark);
        arranged
    }

    /// Evaluates the default of every parameter a call left empty.
    fn fill_defaults(
        &mut self,
        shape: &Shape,
        params: &[Param],
        filled: Vec<Option<Value>>,
        name: &str,
        span: Span,
    ) -> Result<Vec<Value>> {
        let mut arranged = Vec::with_capacity(filled.len());
        for (param, held) in params.iter().zip(filled) {
            let value = match (held, &param.default) {
                (Some(value), _) => value,
                (None, Some(default)) => self.eval(default, shape.env)?,
                (None, None) => {
                    return Err(QuinceError::new(
                        format!("`{name}` needs an argument for `{}`", param.name),
                        span,
                    )
                    .with_kind(ErrorKind::Type)
                    .with_help(format!(
                        "it has no default, so every call has to say what it is — pass it in \
                         order, or write `{}: …`",
                        param.name
                    )));
                }
            };
            if value.handle().is_some() {
                self.temps.push(value.clone());
            }
            arranged.push(value);
        }
        Ok(arranged)
    }

    pub(crate) fn call(&mut self, target: Value, args: Vec<Value>, span: Span) -> Result<Value> {
        match target {
            // Narrowed here as well as at the expression that wrote the call,
            // because a set can be reached through a name a program stored it
            // under — and because the values a call ends up making are the ones
            // dispatch is about. §3.6's rule that a declaration contributes one
            // signature per arity is what makes the two agree: at the arity the
            // arranged values have, only one candidate can accept them.
            Value::Overload(_) => {
                let chosen = self.selected(&target, &[], &args, false, span)?;
                self.call(chosen, args, span)
            }
            Value::Native(native) => {
                if let Some(arity) = native.arity {
                    check_arity(native.name, arity, args.len(), span)?;
                }
                self.check_native_args(native, &args, span)?;
                // `args` lives in this frame and nothing roots it. That is safe
                // only while no builtin reaches a safe point; the first one to
                // call back into Quince has to root them here first.
                (native.func)(self, &args, span)
            }

            Value::Function(id) => {
                let func = self.heap.function(id).clone();
                check_arity(&func.decl.name, func.decl.params.len(), args.len(), span)?;

                if self.depth >= MAX_DEPTH {
                    return Err(QuinceError::new(
                        format!("recursion limit of {MAX_DEPTH} calls exceeded"),
                        span,
                    )
                    .with_kind(ErrorKind::Recursion)
                    .with_help(
                        "the recursion needs a base case that stops sooner, or rewriting as a \
                         loop — the limit is a fixed count and not a memory the program ran out \
                         of",
                    ));
                }

                // The same refusal for the same reason, arrived at differently:
                // `MAX_DEPTH` counts calls, and this measures what a call was
                // standing in for. Both are here because each catches what the
                // other cannot — a count is the same everywhere and so is worth
                // being the documented limit, and a measurement is the only thing
                // that sees an expensive shape run out early.
                if out_of_stack() {
                    return Err(QuinceError::new("recursion is too deep to continue", span)
                        .with_kind(ErrorKind::Recursion)
                        .with_help(
                            "this nests further than the interpreter can follow — the recursion \
                             needs a base case that stops sooner",
                        ));
                }

                // Parameters are the body scope's first slots, in order, so
                // binding them needs no names at all.
                let scope = self.heap.alloc(Object::Env(Env::new(
                    Some(func.env),
                    func.decl.body.slot_count,
                )));
                // What this body's type parameters are bound to, if it is a
                // method of a generic class. Read off the receiver, which is
                // `args[0]` for a method and is why this can be asked once
                // rather than per parameter: every `T` in one signature means
                // the same `T`, and the receiver is what says which.
                //
                // Pushed before the parameters are checked and popped after the
                // return is, because both mention `T` — and so does every `let`
                // in between, which is why it is a frame on the interpreter
                // rather than a local. Empty for every function in a program
                // that declares no generic class. v0.9 §3.1.
                self.type_bindings.push(match func.decl.params.first() {
                    Some(first) if first.receiver => self.bindings_of(&args[0]),
                    _ => Vec::new(),
                });

                // Every path from here to the matching `pop` has to reach it,
                // so the parameter loop hands back a `Result` rather than using
                // `?` — a refusal against an annotation is the ordinary case,
                // not the exceptional one.
                let bound = (|interp: &mut Interp| -> Result<()> {
                    for (index, mut arg) in args.into_iter().enumerate() {
                        let param = &func.decl.params[index];
                        // Against the parameter's annotation, at the boundary
                        // the value actually crosses. The span is the call's,
                        // because that is where the wrong value was written —
                        // the declaration is right and is somewhere else.
                        //
                        // `coerced` substitutes, so `push(item: T)` on a
                        // `Stack[int]` is checked as `push(item: int)` by the
                        // rules that were already here. What is substituted
                        // again below is the annotation the scope *keeps*, for
                        // the assignment checks that read it later.
                        let ty = param.ty.clone();
                        if let Some(ty) = &ty {
                            let named = format!("`{}`", param.name);
                            arg = interp.coerced(ty, arg, &named, span)?;
                        }
                        // `const p` freezes what the caller passed, which is the
                        // guarantee §3.3 wants from a `const` parameter: the
                        // callee cannot mutate caller data through it. `final p`
                        // binds the name once and leaves the object alone — the
                        // other axis, and the same pair `let`/`final`/`const`
                        // mean anywhere.
                        if param.bind.freezes() {
                            interp.heap.freeze(&arg);
                        }
                        let ty = ty.map(|ty| Rc::new(substituted(&ty, interp.bindings())));
                        interp.heap.env_mut(scope).declare(index as u16, arg, ty, param.bind);
                    }
                    Ok(())
                })(self);
                if let Err(err) = bound {
                    self.type_bindings.pop();
                    return Err(err);
                }

                self.depth += 1;
                self.reaching.push(func.owner);
                let result = self.exec_scoped(&func.decl.body.stmts, scope);
                self.reaching.pop();
                self.depth -= 1;

                // The one place a call into another module's code comes back
                // out, and so the one place that can say whose text the spans in
                // an error belong to. A function carries the scope it was
                // defined in, so this is right however far the value travelled
                // before it was called.
                let result = result.map_err(|err| match self.module_source(func.env) {
                    Some(source) => err.in_module(source),
                    None => err,
                });

                let produced = match result {
                    Ok(Flow::Return(value)) => value,
                    Ok(Flow::Normal) => Value::Nil,
                    Err(err) => {
                        self.type_bindings.pop();
                        return Err(err);
                    }
                };
                // A declared return is checked on the way out, which catches the
                // implicit `nil` a function that falls off its end produces —
                // the case an annotation most often exists to rule out.
                //
                // Still inside the frame, because `pop(): T?` on a `Stack[int]`
                // returns an `int?` and `coerced` is what turns the one into the
                // other.
                let checked = match func.decl.returns.clone() {
                    Some(ty) => {
                        let named = format!("`{}`\u{2019}s return", func.decl.name);
                        self.coerced(&ty, produced, &named, span)
                    }
                    None => Ok(produced),
                };
                self.type_bindings.pop();
                checked
            }

            Value::BoundMethod(id) => {
                let bound = self.heap.bound_method(id).clone();
                self.call_method(bound.receiver, bound.method, args, span)
            }

            // Calling a class builds an instance and hands it to `init`, which
            // is why `init` returns nothing useful: the object already exists
            // by the time it runs.
            Value::Class(id) => {
                // Except for a builtin, where there is no object to build. An
                // int has nowhere to keep a field, so its `init` returns the
                // value instead of filling one in, and the call yields that. The
                // difference is only in what construction *produces* — the class
                // is a class like any other, and its `init` is found the same way.
                if let Some(builtin) = self.heap.class(id).builtin {
                    return self.construct_builtin(id, builtin, args, span);
                }

                let instance_id = self.heap.alloc(Object::Instance(Instance {
                    class: id,
                    fields: Dict::new(),
                    // Filled below, either by an `op init` calling `super.init` or
                    // by the conversion this class inherited. Not here, because
                    // only the constructor knows what value to convert.
                    payload: None,
                }));
                let instance = Value::Instance(instance_id);
                // Before the fields, because a field annotated `list[T]` is
                // checked on the way in and the answer to what `T` is comes
                // from here. v0.9 §3.1's "binding is reified".
                //
                // Taken, not read: a `Stack[int](Point())` evaluates its
                // argument inside this call, and the `Point` it builds must not
                // inherit the header meant for the `Stack`.
                if let Some(header) = self.pending.take() {
                    self.heap.describe(instance_id, header);
                }

                // Declared fields, before `op init` — so an `init` assigning one
                // overwrites a value that is already there, which is what makes
                // `let balance = 0` followed by `self.balance = opening` read the
                // way it looks. Rooted through `temps`, because an initializer is
                // an arbitrary expression and may reach a safe point — the same
                // reason the `op init` case below pushes the instance.
                //
                // The arguments are rooted for the same reason and are easy to
                // miss: they were evaluated before this call and are held only
                // in a Rust `Vec` until `op init` binds them into its scope, so
                // an initializer that reaches a safe point in between —
                // `let left: Expr = ConstExpr(0.0)` is a call and does — would
                // collect an argument the constructor has not looked at yet.
                let mark = self.temps.len();
                self.temps.push(instance.clone());
                self.temps.extend(args.iter().filter(|arg| arg.handle().is_some()).cloned());
                let initialized = self.init_fields(id, instance_id, span);
                self.temps.truncate(mark);
                initialized?;

                // The `op init` the class resolved when it was built, not a
                // lookup of the name `init` — a method merely *called* `init` is
                // an ordinary method, and construction must not reach it.
                match self.heap.class(id).slot(Op::Init).cloned() {
                    // A native here is a conversion, inherited by a class that
                    // declares no `op init` of its own: nothing was written to run,
                    // so construction does what an `op init` forwarding to
                    // `super.init` would have done. `Username("marc")` needs no
                    // constructor to be a string, and writing the one that only
                    // forwards is boilerplate the language can supply.
                    Some(init @ Value::Native(_)) => {
                        let builtin = self
                            .builtin_base(id)
                            .expect("only a builtin's seed puts a native in `init`");
                        self.set_payload(instance_id, init, builtin, args, span)
                            .map_err(|err| {
                                // The arity comes from the conversion, so it says
                                // `string` where the call site said `Username`.
                                // True, and worth saying rather than rewriting:
                                // this class is built from what its base is.
                                let name = self.heap.class(id).name.clone();
                                err.with_help(format!(
                                    "`{name}` extends `{}`, so it is built from whatever a {} is",
                                    builtin.name(),
                                    builtin.name()
                                ))
                            })?;
                    }
                    // `init` runs Quince code, so it reaches safe points, and
                    // the instance needs no root here: it sits in slot 0 of the
                    // constructor's scope, which `exec_scoped` roots for the
                    // whole body.
                    //
                    // That holds only because `self` cannot be reassigned — the
                    // resolver refuses it, see `Param::receiver`. This used to
                    // push the instance onto `temps` for exactly that reason,
                    // and the root came out when the language rule went in.
                    Some(init) => {
                        self.call_method(instance.clone(), init, args, span)?;
                    }
                    // No constructor, so the only correct call passes nothing.
                    None => {
                        let name = self.heap.class(id).name.clone();
                        // Marking is what makes a constructor a constructor, so
                        // a plain `fn init` is the likeliest reason to be here
                        // with arguments in hand. Saying so turns the one mistake
                        // the marking rule allows into an instruction.
                        let unmarked = self
                            .heap
                            .class(id)
                            .method(Op::Init.name(), &self.heap)
                            .is_some();
                        check_arity(&name, 0, args.len(), span).map_err(|err| {
                            if unmarked {
                                err.with_help(format!(
                                    "`{name}` has a method `init`, but only `op init` runs when a class is constructed"
                                ))
                            } else {
                                err
                            }
                        })?;
                    }
                }
                Ok(instance)
            }

            other => Err(QuinceError::new(
                format!("{} is not callable", other.type_name(&self.heap)),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("only functions and classes can be called")),
        }
    }

    /// Calling a builtin type: `int("42")`, `list()`.
    ///
    /// No instance is allocated. A builtin's `init` is a conversion, so it takes
    /// the call's arguments and nothing else, and the value it returns is the
    /// result of the call. That is why this reaches `call` rather than
    /// `call_method` — there is no receiver to insert.
    pub(super) fn construct_builtin(
        &mut self,
        id: ObjId,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // One argument is a conversion of that argument, and its class is what
        // gets to say what it converts to. Asked here rather than in each of the
        // six natives, because it is one question — and asked before them, so an
        // `op int` beats the payload underneath it, which is the whole point of
        // declaring one.
        //
        // `string` and `bool` would have reached their op anyway, through
        // `display` and `is_truthy`. This just gets there first, with the same
        // answer.
        if let [arg] = args.as_slice()
            && let Some(op) = builtin.conversion()
            && let Some(method) = self.slot(arg, op)
        {
            let arg = arg.clone();
            let produced = self.call_op(method, &arg, Vec::new())?;
            // An `op int` answering with a string would make `int(x)` return
            // something that is not an int, which nothing downstream is written
            // to survive.
            if produced.base(&self.heap).class(&self.heap) != self.heap.builtin_class(builtin) {
                let expected = an(builtin.name());
                return Err(self.op_returned(op, &arg, &expected, &produced));
            }
            return Ok(produced);
        }

        let Some(init) = self.heap.class(id).slot(Op::Init).cloned() else {
            // Only `function` reaches this: `nil` and `class` are keywords, so
            // neither is bound as a global and nothing can name them to call.
            return Err(
                QuinceError::new(format!("cannot make {}", an(builtin.name())), span)
                    .with_kind(ErrorKind::Type)
                    .with_help(format!(
                        "there is no value a {} could be made from — `fn` is how one is written",
                        builtin.name()
                    )),
            );
        };
        self.call(init, args, span)
    }

    /// `super.init(…)` where the superclass is a builtin: the conversion runs,
    /// and what it produces becomes the receiver's payload.
    ///
    /// The conversion is the same one `string(x)` reaches, so the arities and the
    /// errors are the ones already written — `super.init("abc")` on an `int`
    /// ancestor raises the same `ValueError` as `int("abc")` does, at the
    /// `super.init` rather than at the constructor call.
    pub(super) fn super_init(
        &mut self,
        class: ObjId,
        builtin: Builtin,
        receiver: &Value,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        let Value::Instance(id) = receiver else {
            unreachable!("`super` binds the enclosing method's receiver, always an instance");
        };
        let init = self
            .heap
            .class(class)
            .slot(Op::Init)
            .cloned()
            .expect("a builtin reached as a superclass is one that converts");
        self.set_payload(*id, init, builtin, args, span)
    }

    /// Runs a conversion and keeps what it produced as `id`'s payload.
    ///
    /// The value as the annotation `ty` says it should be, or a refusal.
    ///
    /// Three things in the order §3.3 and §4.1 need them: the check, so a
    /// refusal reports the value the program wrote; the widening, because
    /// `let x: float = 0` has to *store* a float or `type(x)` would disagree
    /// with the annotation next to it; and the freeze, at the boundary the value
    /// crosses.
    ///
    /// Hands the value back rather than checking in place, because the widening
    /// makes this a conversion and not only a test — and a caller that bound the
    /// original would have an `int` under an annotation reading `float`.
    pub(super) fn coerced(
        &mut self,
        ty: &TypeExpr,
        value: Value,
        what: &str,
        span: Span,
    ) -> Result<Value> {
        self.coerced_from(ty, value, what, span, Written::nowhere())
    }

    /// The same, told where the value was written.
    ///
    /// See [`Written`] for what that buys. The caller's `span` stays the
    /// fallback for everything `written` cannot be more precise about.
    pub(super) fn coerced_from(
        &mut self,
        ty: &TypeExpr,
        value: Value,
        what: &str,
        span: Span,
        written: Written<'_>,
    ) -> Result<Value> {
        // Every report below is about the value, so it points at the value
        // wherever the caller knew where that was. A statement-wide underline is
        // what is left when nobody did.
        let span = written.caret(span);
        // Every annotation checked at run time passes through here, which is
        // why substitution is here and not at each of the callers: a `T` is a
        // type not yet written down, and this is the one place that has both the
        // annotation and the frame that says what it stands for. A caller that
        // forgot would silently accept anything. v0.9 §3.1.
        //
        // A clone and nothing else outside a generic method — see
        // [`crate::interp::generic::substituted`].
        let substituted = substituted(ty, self.bindings());
        let ty = &substituted;
        // A name that is not a type at all is a different mistake from a value
        // that does not hold, and reporting it as the second blames the value
        // for the annotation being wrong. Checked here rather than at
        // resolution because that is where the answer is: a class is an
        // ordinary binding, and which ones exist is not known until they run.
        if let TypeName::Named(name) = &ty.name
            && !self.names_a_type(name)
        {
            return Err(self.no_such_type(name, ty.span));
        }
        let mut value = value;
        if !holds(ty, &value, &self.heap) {
            // The one thing that can rescue a value that does not hold: a class
            // that says how to make one of itself from it. §3.3.
            match self.constructed_from(ty, &value, span)? {
                Some(built) => value = built,
                None => {
                    // A container that failed only on its contents gets a report
                    // about the element rather than about itself — "this is a
                    // list" when a list was asked for says nothing. Rewalked
                    // rather than threaded out of `holds`, because this runs
                    // once, on the way to an error.
                    if let Some(precise) = self.offending_element(ty, &value, span, written) {
                        return Err(precise);
                    }
                    return Err(does_not_hold(
                        &self.heap,
                        ty,
                        &value,
                        what,
                        span,
                        written.annotation_is_here,
                    ));
                }
            }
        }
        // Elements widen too, or `let xs: list[float] = [1, 2]` would hold ints
        // under an annotation reading `float` — the same contradiction §4.1
        // rules out for a plain binding, one level down.
        let value = self.widen_elements(ty, value, span)?;
        // The one widening §4.1 admits, made real. Narrowing is not symmetric
        // and is not here: `let n: int = 3.7` would have to choose a rounding,
        // and `int(x)` is how a program says which.
        let value = match (&ty.name, &value) {
            (TypeName::Named(name), Value::Int(n)) if name == "float" => Value::Float(*n as f64),
            _ => value,
        };
        // The container remembers what it was checked as, so a later `push` or
        // index-set can be refused without re-walking it. §3.9's reified header.
        if !ty.args.is_empty()
            && let Some(id) = value.base(&self.heap).handle()
        {
            self.heap.describe(id, Rc::new(ty.clone()));
        }
        // `const T` freezes deeply, exactly as a `const` binding does — the same
        // word at a place a binding cannot reach.
        if ty.frozen {
            self.heap.freeze(&value);
        }
        Ok(value)
    }

    /// Builds a `T` from a value that is not one, where `T` says how.
    ///
    /// The implicit constructor coercion of §3.3: an annotation naming a class
    /// that declares a single-parameter `op init` is a standing offer to convert,
    /// so `let i: CustomInt = 10` runs `CustomInt(10)`. `Ok(None)` means no such
    /// offer exists and the caller should report the value as not holding.
    ///
    /// Four things narrow it, and each is load-bearing:
    ///
    /// - **One parameter only.** There is no rule that could pick among several
    ///   arguments from one value.
    /// - **A constructor written in Quince.** A builtin's `init` slot holds a
    ///   *conversion*, so admitting those would make `let s: string = 5` legal
    ///   and quietly stringify every wrong argument in the language.
    /// - **The payload is checked first**, against the constructor's own
    ///   parameter type. That is also what stops coercion chaining: if `A` is
    ///   built from a `B` and `B` from an `int`, `let a: A = 1` fails this check
    ///   and is reported as an `int` that is not an `A`, rather than searching.
    /// - **`explicit` opts out**, and is the one case that reports rather than
    ///   declining — the class does convert, and has said not to do it silently.
    fn constructed_from(
        &mut self,
        ty: &TypeExpr,
        value: &Value,
        span: Span,
    ) -> Result<Option<Value>> {
        let TypeName::Named(name) = &ty.name else {
            return Ok(None);
        };
        // `nil` is not a value to build from. An annotation that admits it says
        // `?`, and one that does not is a refusal rather than a conversion.
        if matches!(value, Value::Nil) {
            return Ok(None);
        }
        let Some(Value::Class(id)) = self.heap.globals(self.globals).get(name).cloned() else {
            return Ok(None);
        };
        let Some(init) = self.heap.class(id).slot(Op::Init).cloned() else {
            return Ok(None);
        };
        // A class may declare several constructors, and the offer to convert is
        // made by whichever of them takes one parameter this value fits. Asked
        // here rather than left to `call` because the *payload* decides whether
        // there is an offer at all — a class with an `op init(text: string)`
        // does not convert an int, and reporting that as "no `init` takes (int)"
        // would blame the constructor for an annotation that is the mistake.
        let candidates = match &init {
            Value::Overload(set) => self.heap.overload(*set).to_vec(),
            other => vec![other.clone()],
        };
        let found = candidates.iter().find_map(|candidate| {
            let Value::Function(func) = candidate else {
                return None;
            };
            let decl = Rc::clone(&self.heap.function(*func).decl);
            // `self` and one more. A constructor's receiver is in `params` and
            // is not the program's to count.
            let [_, param] = decl.params.as_slice() else {
                return None;
            };
            // An unannotated parameter takes whatever it is handed, which is
            // what it means everywhere else.
            match &param.ty {
                Some(source) if !holds(source, value, &self.heap) => None,
                _ => Some(decl),
            }
        });
        let Some(decl) = found else {
            return Ok(None);
        };
        if decl.explicit {
            return Err(QuinceError::new(
                format!(
                    "`{name}`'s constructor is `explicit`, so {} does not become one on its own",
                    an(value.type_name(&self.heap))
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "write `{name}(…)` — the class marked its constructor `explicit` because the \
                 argument is not a conversion, and naming the class is what makes the call read"
            )));
        }
        self.call(Value::Class(id), vec![value.clone()], span).map(Some)
    }

    /// The report for the element that made a container fail, if one did.
    ///
    /// `None` when the container itself was the mistake — a dict where a list
    /// was asked for — which is the case the caller already words well.
    ///
    /// Recursive, because `list[list[int]]` refused by `[[1], ["a"]]` has two
    /// answers and only one of them is useful: the outer element is a list that
    /// is not a `list[int]`, which reads as "this is a list" and tells nobody
    /// anything, while one level down there is a `"a"` that is not an `int`.
    /// The static check in `sema::check` already descends this way, and the two
    /// are meant to be the same sentence about the same program.
    fn offending_element(
        &mut self,
        ty: &TypeExpr,
        value: &Value,
        span: Span,
        written: Written<'_>,
    ) -> Option<Raised> {
        let name = match &ty.name {
            TypeName::Named(name) => name.as_str(),
            TypeName::Any => return None,
        };
        match (name, value.base(&self.heap).clone()) {
            ("list", Value::List(id)) => {
                let arg = ty.args.first()?;
                let items = self.heap.list(id).clone();
                let (index, item) = items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| !holds(arg, item, &self.heap))?;
                let item = item.clone();
                let inside = written.within(written.element(index));
                let at = inside.caret(span);
                self.offending_element(arg, &item, at, inside)
                    .or_else(|| {
                        Some(does_not_hold(
                            &self.heap,
                            arg,
                            &item,
                            &format!("item {index}"),
                            at,
                            written.annotation_is_here,
                        ))
                    })
            }
            ("dict", Value::Dict(id)) => {
                let dict = self.heap.dict(id).clone();
                let pairs = written.pairs(dict.len());
                if let Some(arg) = ty.args.first()
                    && let Some(index) = dict.keys().position(|key| !holds(arg, &key, &self.heap))
                {
                    let key = dict.keys().nth(index)?;
                    // No recursion here: a key is hashable and so is never one
                    // of the containers this would descend into.
                    let at = pairs.and_then(|pairs| Some(pairs.get(index)?.0.span));
                    return Some(does_not_hold(
                        &self.heap,
                        arg,
                        &key,
                        "the key",
                        at.unwrap_or(span),
                        written.annotation_is_here,
                    ));
                }
                let arg = ty.args.get(1)?;
                let index = dict
                    .values()
                    .position(|held| !holds(arg, held, &self.heap))?;
                let held = dict.values().nth(index)?.clone();
                let inside = written.within(pairs.and_then(|pairs| Some(&pairs.get(index)?.1)));
                let at = inside.caret(span);
                self.offending_element(arg, &held, at, inside)
                    .or_else(|| {
                        Some(does_not_hold(
                            &self.heap,
                            arg,
                            &held,
                            "the value",
                            at,
                            written.annotation_is_here,
                        ))
                    })
            }
            _ => None,
        }
    }

    /// Rewrites a container's elements to the types its annotation states.
    ///
    /// Only the §4.1 widening has anything to do — every other element already
    /// holds by the time this runs. In place, because the value the program
    /// holds is the one that has to change: a copy would leave the original
    /// reachable through every other name for it.
    fn widen_elements(&mut self, ty: &TypeExpr, value: Value, span: Span) -> Result<Value> {
        let name = match &ty.name {
            TypeName::Named(name) => name.as_str(),
            TypeName::Any => return Ok(value),
        };
        match (name, value.base(&self.heap).clone()) {
            ("list", Value::List(id)) => {
                let Some(arg) = ty.args.first().cloned() else {
                    return Ok(value);
                };
                let items = self.heap.list(id).clone();
                let mut widened = Vec::with_capacity(items.len());
                for item in items {
                    widened.push(self.coerced(&arg, item, "the item", span)?);
                }
                if let Ok(held) = self.heap.list_mut(id) {
                    *held = widened;
                }
            }
            ("dict", Value::Dict(id)) => {
                let Some(arg) = ty.args.get(1).cloned() else {
                    return Ok(value);
                };
                let dict = self.heap.dict(id).clone();
                let mut widened = Vec::new();
                for (key, held) in dict.iter() {
                    widened.push((key.clone(), self.coerced(&arg, held.clone(), "the value", span)?));
                }
                if let Ok(held) = self.heap.dict_mut(id) {
                    for (key, value) in widened {
                        held.insert(key, value);
                    }
                }
            }
            _ => {}
        }
        Ok(value)
    }

    /// Refuses a value about to be put into a described container.
    ///
    /// The other half of the reified header: [`Self::coerced`] checks a
    /// container's contents once when it crosses an annotated boundary, and this
    /// is what keeps them true afterwards. `slot` picks which argument applies —
    /// a list's element, a dict's key, or a dict's value.
    ///
    /// Answers the value back, because the same §4.1 widening applies: pushing
    /// an int onto a `list[float]` stores the float.
    pub(crate) fn admitted(
        &mut self,
        container: ObjId,
        slot: usize,
        value: Value,
        what: &str,
        span: Span,
    ) -> Result<Value> {
        let Some(descriptor) = self.heap.descriptor(container) else {
            return Ok(value);
        };
        // An argument the declaration elided is `any?`, which admits everything,
        // so there is nothing to check and no allocation worth making to say so.
        //
        // This is *not* where a `dict[K]` is kept honest about its values. What
        // stops a `dict[string]` being written through as a `dict[string, int]`
        // is that it never gets to be one: `arguments_admit` refuses the
        // boundary, because `int` does not admit the `any?` the shorthand
        // elided. Before that rule the pass in and the write both waved it
        // through, and a string landed in the int slot.
        let Some(ty) = descriptor.args.get(slot) else {
            return Ok(value);
        };
        if !holds(ty, &value, &self.heap) {
            // Never `true`. This annotation was stamped on the allocation when
            // it crossed a boundary that may have been a different file, or in
            // the REPL a different entry — so its span means nothing here.
            return Err(does_not_hold(&self.heap, ty, &value, what, span, false));
        }
        Ok(match (&ty.name, &value) {
            (TypeName::Named(name), Value::Int(n)) if name == "float" => Value::Float(*n as f64),
            _ => value,
        })
    }

    /// Refuses an argument a builtin's declaration does not admit.
    ///
    /// The library's half of what an annotation does for a function someone
    /// wrote, and worded by the same rule so the two read alike: a wrong
    /// argument to `split` now says what a wrong argument to `fn f(s: string)`
    /// says. Before this the tables recorded parameter *names* and not types, so
    /// each builtin refused in its own words — three sentences for one mistake —
    /// and the inference pass could not see the rule at all.
    ///
    /// A parameter that names no types admits anything, which is most of them:
    /// `print` takes any value, `push` takes any item, and a conversion refuses
    /// in its own words because "cannot convert a list to an int" says more than
    /// a list of what `int` accepts would.
    fn check_native_args(&mut self, native: &Native, args: &[Value], span: Span) -> Result<()> {
        // A type's method is called on a receiver that `arity` counts and the
        // caller does not write, so the declared parameters line up with the
        // *end* of `args`. `every_native_names_the_parameters_it_takes` is what
        // keeps that offset from being a guess.
        let offset = args.len().saturating_sub(native.params.len());
        for (index, param) in native.params.iter().enumerate() {
            let Some(value) = args.get(offset + index) else {
                continue;
            };
            if param.admits(value, &self.heap) {
                continue;
            }
            return Err(QuinceError::new(
                format!(
                    "`{}` is {}, but this is {}",
                    param.name,
                    param.written(),
                    an(value.type_name(&self.heap))
                ),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "`{}` takes {} there",
                native.name,
                param.written()
            )));
        }
        Ok(())
    }

    /// Whether `value` has the type `ty`, as `is` asks it.
    ///
    /// Shares [`arguments_admit`] with [`holds`], which is the whole of what
    /// makes the two agree. They used to compare arguments by *identity* here
    /// and by admission there, so `xs is list[any]` was `false` for a value a
    /// `list[any]` parameter accepted, and `list` and `list[any?]` — one type
    /// under §3.10's elision rule — gave opposite answers.
    ///
    /// Two differences from [`holds`] survive, and both are the same
    /// distinction: **`holds` asks whether a value may become a `T`, and this
    /// asks what it already is.**
    ///
    /// - **No widening.** `1 is float` is `false`, where an annotation widens.
    ///   Widening is a conversion performed at a boundary; a question about a
    ///   value in hand should answer about the value in hand.
    /// - **The descriptor, never the elements.** A container nothing described
    ///   reads as `list[any?]` — every argument elided, which is what an
    ///   unannotated literal is. [`holds`] walks such a container's elements
    ///   instead, because at a boundary it is raw material about to be stamped:
    ///   `let xs: list[int] = [1, 2]` is the annotation *deciding* the type, not
    ///   agreeing with one. This keeps the check O(1) in the container's size,
    ///   which is §3.9's promise and the reason this cannot simply delegate.
    ///
    /// So an undescribed `[1, 2]` answers `true` to `list[any?]` and `false` to
    /// `list[int]`, while `let xs: list[int] = [1, 2]` is still accepted. Those
    /// agree: unstamped is a real state, and it is not the same state as stamped
    /// `list[any?]`.
    ///
    /// `nil` still needs a `?`, exactly as an annotation does — `nil is int` is
    /// `false` and `nil is int?` is `true`.
    pub(super) fn has_type(&mut self, ty: &TypeExpr, value: &Value) -> bool {
        if matches!(value, Value::Nil) {
            return ty.admits_nil();
        }
        let name = match &ty.name {
            TypeName::Any => return true,
            TypeName::Named(name) => name.as_str(),
        };

        let actual = value.type_name(&self.heap);
        if actual != name && !self.descends_from_named(value, name) {
            return false;
        }
        // Deliberately no `float` arm: `holds` widens an int because `let x:
        // float = 0` has to *store* a float, and that is a conversion. `is` asks
        // what the value already is, and `1` is not one.
        //
        // The reified header, read through the same argument rule an annotation
        // uses. A container nothing described has no arguments to offer, so it
        // reads as every argument elided — which is to say a `list[any?]`, the
        // top container type, and exactly what an unannotated literal is.
        //
        // Still O(1): the header is a lookup and the arguments are a fixed-width
        // comparison. Nothing walks the elements, which is §3.9's promise and
        // the reason `is` cannot simply call `holds`.
        let held = value
            .base(&self.heap)
            .handle()
            .and_then(|id| self.heap.descriptor(id));
        arguments_admit(ty, held.as_ref().map_or(&[][..], |held| &held.args))
    }

    /// Whether `value`'s class is `name` or descends from it.
    fn descends_from_named(&self, value: &Value, name: &str) -> bool {
        let mut current = Some(value.class(&self.heap));
        while let Some(id) = current {
            if self.heap.class(id).name == name {
                return true;
            }
            current = self.heap.class(id).parent;
        }
        false
    }

    /// Whether `name` names a class that exists.
    ///
    /// The builtins, and whatever the program bound at the top level. A class
    /// declared inside a function and used as an annotation in the same scope
    /// is not found — which is a gap, and the honest one: this has no scope in
    /// hand, and the alternative is threading one through every check for a
    /// shape nothing in the corpus writes.
    fn names_a_type(&self, name: &str) -> bool {
        if BUILTIN_TYPES.iter().any(|builtin| builtin.name() == name) {
            return true;
        }
        matches!(
            self.heap.globals(self.globals).get(name),
            Some(Value::Class(_))
        )
    }

    /// Reports an annotation naming something that is not a type.
    fn no_such_type(&self, name: &str, span: Span) -> crate::error::Raised {
        let mut known: Vec<&str> = BUILTIN_TYPES.iter().map(|builtin| builtin.name()).collect();
        let declared: Vec<String> = self
            .heap
            .globals(self.globals)
            .iter()
            .filter(|(_, value)| matches!(value, Value::Class(_)))
            .map(|(key, _)| key.to_string())
            .collect();
        known.extend(declared.iter().map(String::as_str));

        let err = QuinceError::new(format!("there is no type called `{name}`"), span)
            .with_kind(ErrorKind::Name);
        match crate::error::did_you_mean(name, known) {
            Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
            None => err,
        }
    }

    /// Runs the initializer of every field the class and its ancestors declared.
    ///
    /// Ancestors first, so a subclass redeclaring a name writes last and wins —
    /// the same order [`Class::field`] reads them back in, which is what keeps
    /// the value an instance holds and the declaration a report names in step.
    ///
    /// The chain is collected before anything runs, because an initializer is
    /// arbitrary Quince code: it may allocate, collect, or construct another
    /// instance of the same class, and none of that may happen while a borrow of
    /// the class is being held.
    fn init_fields(&mut self, class: ObjId, instance: ObjId, span: Span) -> Result<()> {
        let mut chain = Vec::new();
        let mut current = Some(class);
        while let Some(id) = current {
            chain.push(id);
            current = self.heap.class(id).parent;
        }

        for id in chain.into_iter().rev() {
            let declared = self.heap.class(id).fields.clone();
            let Some(env) = self.heap.class(id).field_env else {
                continue;
            };
            // The instance's own class only. A generic *parent* would need
            // `class B[T] extends A[T]` to say what its argument is, and there
            // is no syntax for that yet — so an ancestor's `T` binds to nothing
            // here rather than to whatever the subclass happens to call its
            // first, which is the answer that would be wrong rather than
            // merely absent.
            let bindings = match id == class {
                true => self.bindings_for(instance),
                false => Vec::new(),
            };
            for field in declared {
                let mut value = self.eval(&field.value, env)?;
                if let Some(ty) = field.ty.clone() {
                    let ty = substituted(&ty, &bindings);
                    let named = format!("`{}`", field.name);
                    value = self.coerced_from(
                        &ty,
                        value,
                        &named,
                        field.value.span,
                        Written::declared(&field.value),
                    )?;
                }
                if field.bind.freezes() {
                    self.heap.freeze(&value);
                }
                let key = Key::Str(Rc::from(field.name.as_str()));
                let written = self
                    .heap
                    .instance_mut(instance)
                    .map(|held| held.fields.insert(key, value));
                written.map_err(|_| frozen(&self.heap, &Value::Instance(instance), span))?;
            }
        }
        Ok(())
    }

    /// Both ways a payload comes to exist end here: an explicit `super.init(…)`,
    /// and the implicit construction a class declaring no `op init` gets. They are
    /// the same operation on purpose — an implicit `op init` is not a second rule,
    /// only the observation that a class inheriting a conversion as its
    /// constructor should run it as one.
    pub(super) fn set_payload(
        &mut self,
        id: ObjId,
        init: Value,
        builtin: Builtin,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // Checked before the conversion runs, so a second write is refused on the
        // strength of the first rather than after quietly replacing it. The
        // resolver requires that *a* `super.init` is written but cannot say how
        // many run — a call in each arm of an `if` is one call — so this is where
        // "once" is actually enforced.
        if self.heap.instance(id).payload.is_some() {
            let name = self.heap.class(self.heap.instance(id).class).name.clone();
            return Err(QuinceError::new(
                format!("`super.init` was already called for this {name}"),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "{} is given its {} once, and this would replace it",
                an(&name),
                builtin.name()
            )));
        }

        // A conversion is a native and natives reach no safe point, so the
        // instance behind `id` cannot move or be collected across this call.
        let value = self.call(init, args, span)?;
        match self.heap.instance_mut(id) {
            Ok(instance) => {
                instance.payload = Some(value);
                Ok(Value::Nil)
            }
            // Freezing happens to a value a constructor already returned, so
            // reaching this means `init` was called a second time by hand, on an
            // object that never got a payload the first time.
            Err(_) => Err(frozen(&self.heap, &Value::Instance(id), span)),
        }
    }

    /// The builtin a class descends from, if it descends from one at all.
    ///
    /// Walks the same chain `Class::method` walks, and terminates for the same
    /// reason: a parent is read before its subclass's name is bound, so the chain
    /// cannot contain a cycle.
    pub(super) fn builtin_base(&self, class: ObjId) -> Option<Builtin> {
        let mut class = self.heap.class(class);
        loop {
            if let Some(builtin) = class.builtin {
                return Some(builtin);
            }
            class = self.heap.class(class.parent?);
        }
    }

    /// Evaluates `receiver.name(args)`.
    ///
    /// Kept out of `eval` to stop the arm's locals widening a frame that
    /// recurses once per node in the tree. Measured against this program's
    /// recursion limit it made no difference — debug frames are dominated by
    /// things other than one arm — but it keeps `eval` readable.
    /// `x.m(…)`, fused rather than built as a bound method and then called.
    ///
    /// Takes the callee whole rather than its pieces: the receiver, the name,
    /// and whether the dot was optional all come off the same node, and passing
    /// them apart was five arguments describing one thing.
    /// A call whose callee is already a value: evaluate the arguments, arrange
    /// them, and go.
    ///
    /// The tail of every call form that is not fused. Named arguments and
    /// defaults are settled here, where the callee's declaration is in hand and
    /// the values already are, so [`Interp::call`] stays a function of a
    /// positional vector.
    pub(super) fn call_evaluated(
        &mut self,
        target: Value,
        args: &[CallArg],
        env: ObjId,
        span: Span,
    ) -> Result<Value> {
        // The callee is held across every argument, any of which can reach a
        // safe point, and a closure built by an expression is reachable from
        // nowhere else. Kept out of `eval_seq` so the argument vector stays
        // exactly as long as the call needs.
        let mark = self.temps.len();
        if target.handle().is_some() {
            self.temps.push(target.clone());
        }
        let values = self.eval_seq(args.iter().map(|arg| &arg.value), env);
        self.temps.truncate(mark);
        let values = values?;
        let shape = self.shape_for(&target, args, &values, span)?;
        let named = self.called_name(&target);
        let values = self.arranged(shape, args, values, &named, span)?;
        self.call(target, values, span)
    }

    pub(super) fn eval_method_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        env: ObjId,
        span: Span,
    ) -> Result<Value> {
        let ExprKind::Field {
            target,
            name,
            optional,
        } = &callee.kind
        else {
            unreachable!("only a field access is fused into a method call");
        };
        let callee_span = callee.span;
        let receiver = self.eval(target, env)?;
        // Before the arguments, which is the point: `a?.b(expensive())` must not
        // evaluate `expensive()` when `a` is `nil`.
        if self.skips(*optional, &receiver) {
            return Ok(Value::Nil);
        }
        let name_span = Span::new(
            (target.span.end as usize + 1).min(callee_span.end as usize),
            callee_span.end as usize,
        );
        let attr = self.attr(&receiver, name, target.span, name_span, callee_span)?;

        // The receiver is held across every argument, any of which can reach a
        // safe point — the same hazard as an ordinary callee, and the same fix.
        // The attribute needs it too: a field holding a function is reachable
        // only through that field, which an argument is free to overwrite.
        let mark = self.temps.len();
        for value in [&receiver, attr.value()] {
            if value.handle().is_some() {
                self.temps.push(value.clone());
            }
        }
        let values = self.eval_seq(args.iter().map(|arg| &arg.value), env);
        self.temps.truncate(mark);
        let values = values?;

        match attr {
            Attr::Method(method) => {
                let shape = self.shape_for_method(&method, args, &values, span)?;
                let named = method.callable_name(&self.heap).to_string();
                let values = self.arranged(shape, args, values, &named, span)?;
                self.call_method(receiver, method, values, span)
            }
            // A field that happens to hold a function; it never took a receiver.
            Attr::Field(value) => {
                let shape = self.shape_for(&value, args, &values, span)?;
                let named = self.called_name(&value);
                let values = self.arranged(shape, args, values, &named, span)?;
                self.call(value, values, span)
            }
        }
    }

    /// Calls `method` with `receiver` in front of `args`.
    ///
    /// The receiver is passed as argument zero for both kinds of method, which
    /// is what lets one `Native` serve as a free function and a method at once,
    /// and what lets a user method be an ordinary function whose first
    /// parameter the parser wrote.
    pub(super) fn call_method(
        &mut self,
        receiver: Value,
        method: Value,
        mut args: Vec<Value>,
        span: Span,
    ) -> Result<Value> {
        // Arity is reported against what the call site can actually write. The
        // receiver is one of the declared arguments but has no syntax as one,
        // so quoting the declared count would ask for an argument the user has
        // no way to supply. Every method has a receiver, making the subtraction
        // safe.
        // Before the arity check, because which declaration's arity applies is
        // exactly what an overload set has not decided yet. The receiver is not
        // among the values dispatch reads: nobody writes it.
        let method = self.selected(&method, &[], &args, true, span)?;
        let declared = match &method {
            Value::Native(native) => native.arity,
            Value::Function(id) => Some(self.heap.function(*id).decl.params.len()),
            // `attr` only ever produces the two above, and `selected` has
            // narrowed a set to one of them.
            other => panic!("expected a method, found {other:?}"),
        };
        if let Some(arity) = declared {
            check_arity(
                method.callable_name(&self.heap),
                arity.saturating_sub(1),
                args.len(),
                span,
            )?;
        }

        // A native was written against a `Value` and matches on its variant, so
        // an instance of a class extending a builtin has to arrive as the value
        // it *is* rather than as the object holding it — `e.upper()` reaches
        // `string`'s `upper`, which knows nothing about classes. A method written
        // in Quince gets the instance, because `self.domain` is the whole reason
        // it was written. Those two cases are exactly `Native` and `Function`:
        // every user method is a function, and a native only ever comes from a
        // builtin's seed.
        //
        // The one substitution in the interpreter, and the reason it is only one:
        // every path that gives a method a receiver comes through here.
        let receiver = match (&method, &receiver) {
            (Value::Native(_), Value::Instance(id)) => match &self.heap.instance(*id).payload {
                Some(payload) => payload.clone(),
                None => return Err(self.no_payload(*id, span)),
            },
            _ => receiver,
        };

        args.insert(0, receiver);
        self.call(method, args, span)
    }
}

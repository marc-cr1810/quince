//! Reaching a member of a value.
//!
//! Field or method, declared or inherited or added by an `extend` block: the walk
//! that answers `x.name`, and the reports for when it cannot.
//!
//! v0.7's visibility rules are enforced here for members reached dynamically,
//! which is the half the resolver cannot see.

use std::rc::Rc;

use crate::error::{ErrorKind, QuinceError, Raised, Result};
use crate::interp::error::an;
use crate::interp::{Attr, Interp};
use crate::runtime::dict::Key;
use crate::runtime::heap::ObjId;
use crate::runtime::value::Value;
use crate::syntax::ast::{FnDecl, Var};
use crate::syntax::ast::Visibility;
use crate::syntax::token::Span;

impl Interp {
    /// Looks up `name` on `receiver`, without calling anything.
    ///
    /// Fields shadow methods, following Python: a field is per-object and a
    /// method is per-class, so the more specific one wins. It also means a
    /// field holding a function is called as an ordinary function rather than
    /// silently acquiring a receiver it was never written to take.
    pub(super) fn attr(
        &self,
        receiver: &Value,
        name: &str,
        target_span: Span,
        name_span: Span,
        expr_span: Span,
    ) -> Result<Attr> {
        if let Value::Instance(id) = receiver
            && let Some(value) = self
                .heap
                .instance(*id)
                .fields
                .get(&Key::Str(Rc::from(name)))
        {
            self.may_reach(self.heap.instance(*id).class, name, name_span)?;
            return Ok(Attr::Field(value.clone()));
        }

        // A module hands back what it declared, and hands it back *unbound*: the
        // names in a module are ordinary top-level names, so `math.floor` is the
        // same function `from math import floor` would have bound directly, and
        // calling it inserts no receiver. A module is a scope, not an object with
        // methods, and this is the line that says so.
        if let Value::Module(id) = receiver {
            return match self.heap.globals(*id).get(name) {
                Some(value) => {
                    self.may_import(*id, name, name_span)?;
                    Ok(Attr::Field(value.clone()))
                }
                None => Err(self.not_in_module(
                    self.heap.globals(*id).name().unwrap_or("module"),
                    name,
                    name_span,
                    *id,
                )),
            };
        }

        // A class hands back its methods unbound, so `Point.dist(p)` works and
        // a method really is a function with the receiver written out.
        if let Value::Class(id) = receiver {
            return match self.find_method(*id, name) {
                Some(method) => Ok(Attr::Field(method)),
                None => Err(self.no_attr(receiver, name, target_span, name_span, expr_span)),
            };
        }

        let class = receiver.class(&self.heap);
        match self.find_method(class, name) {
            Some(method) => {
                self.may_reach(class, name, name_span)?;
                Ok(Attr::Method(method))
            }
            None => Err(self.no_attr(receiver, name, target_span, name_span, expr_span)),
        }
    }

    /// Refuses a member reached from outside the visibility it was declared with.
    ///
    /// Three questions, in the order that makes the common case free: what did
    /// the declaration say, who is reaching, and does the one reach the other.
    /// A member nothing declared — one an `op init` invented by assigning it —
    /// has no visibility and is reachable, which is what keeps every program
    /// written before v0.7 running unchanged.
    pub(super) fn may_reach(&self, class: ObjId, name: &str, span: Span) -> Result<()> {
        let Some((visibility, owner)) = self.declared_reach(class, name) else {
            return Ok(());
        };
        if !visibility.closes_outside() {
            return Ok(());
        }

        // Inside a method of the declaring class, everything is reachable —
        // including on an instance that is not `self`, which is what makes
        // `other.balance` work inside `op eq`.
        let reaching = self.reaching.last().copied().flatten();
        let allowed = match reaching {
            Some(from) if from == owner => true,
            // `protected` also admits a subclass's methods. Walking up from the
            // reaching class rather than down from the owner, because the chain
            // only runs that way.
            Some(from) if !visibility.closes_subclass() => self.descends_from(from, owner),
            _ => false,
        };
        if allowed {
            return Ok(());
        }

        let word = visibility.word().expect("a closed member was written with one");
        let whose = self.heap.class(owner).name.clone();
        Err(QuinceError::new(
            format!("`{name}` is {word} to `{whose}`"),
            span,
        )
        .with_kind(ErrorKind::Visibility)
        .with_help(match visibility {
            Visibility::Private => format!(
                "only methods declared inside `{whose}` may reach it — a method on a subclass \
                 is outside, and so is an `extend` block"
            ),
            _ => format!(
                "only methods of `{whose}` and of the classes extending it may reach it"
            ),
        }))
    }

    /// What `name` was declared as on `class` or an ancestor, and by which class.
    ///
    /// `None` when nothing declared it: a field an `op init` assigned into
    /// existence, or a method an `extend` block added. Neither carries a
    /// visibility, so neither has one to enforce.
    fn declared_reach(&self, class: ObjId, name: &str) -> Option<(Visibility, ObjId)> {
        let mut current = Some(class);
        while let Some(id) = current {
            let class = self.heap.class(id);
            if let Some(field) = class.fields.iter().find(|field| field.name == name) {
                return Some((field.visibility, id));
            }
            if let Some(Value::Function(func)) = class.methods.get(name) {
                return Some((self.heap.function(*func).decl.visibility, id));
            }
            current = class.parent;
        }
        None
    }

    /// Whether `class` is `ancestor`, or descends from it.
    fn descends_from(&self, class: ObjId, ancestor: ObjId) -> bool {
        let mut current = Some(class);
        while let Some(id) = current {
            if id == ancestor {
                return true;
            }
            current = self.heap.class(id).parent;
        }
        false
    }

    /// The method `name` on class `id`, declared, inherited, or added by
    /// `extend`.
    ///
    /// Both walks are over the whole chain, and the *methods* walk finishes
    /// first: a declared method always beats an extension, including one declared
    /// on an ancestor. That is what "consulted after the class's own methods"
    /// has to mean once inheritance is in the picture — `extend int` adding
    /// `double` must not take precedence over a `Wrap extends int` that declares
    /// its own.
    ///
    /// The second walk happens only on a miss, which is the whole cost of keeping
    /// extensions out of `Class::methods`.
    pub(super) fn find_method(&self, id: ObjId, name: &str) -> Option<Value> {
        if let Some(method) = self.heap.class(id).method(name, &self.heap) {
            return Some(method);
        }
        if self.extensions.is_empty() {
            return None;
        }
        let mut class = Some(id);
        while let Some(id) = class {
            if let Some(method) = self.extensions.get(&(id, name.to_string())) {
                return Some(method.clone());
            }
            class = self.heap.class(id).parent;
        }
        None
    }

    /// Every method callable on a value of class `id`, with what it resolves to.
    ///
    /// The same two walks [`Interp::find_method`] makes and in the same order,
    /// so what this lists is exactly what a call would reach: the class and its
    /// ancestors first, then the extensions beside them. A name declared twice
    /// appears once, as the one that would run.
    ///
    /// For the REPL, which has values rather than source and so can answer
    /// about a receiver exactly instead of inferring. Before this existed it
    /// rebuilt half of it by hand into a `HashMap<String, Vec<String>>` and
    /// missed extensions entirely — `extend list { fn second() … }` was
    /// callable and never offered.
    pub fn methods_of(&self, id: ObjId) -> Vec<(String, Value)> {
        let mut found: Vec<(String, Value)> = Vec::new();
        let push = |name: &String, value: Value, found: &mut Vec<(String, Value)>| {
            if !found.iter().any(|(seen, _)| seen == name) {
                found.push((name.clone(), value));
            }
        };

        let mut class = Some(id);
        while let Some(current) = class {
            let object = self.heap.class(current);
            let mut names: Vec<&String> = object.methods.keys().collect();
            names.sort();
            for name in names {
                push(name, object.methods[name].clone(), &mut found);
            }
            class = object.parent;
        }

        let mut class = Some(id);
        while let Some(current) = class {
            let mut extensions: Vec<(&String, &Value)> = self
                .extensions
                .iter()
                .filter(|((owner, _), _)| *owner == current)
                .map(|((_, name), value)| (name, value))
                .collect();
            extensions.sort_by_key(|(name, _)| (*name).clone());
            for (name, value) in extensions {
                push(name, value.clone(), &mut found);
            }
            class = self.heap.class(current).parent;
        }

        found
    }

    /// Whether `extend` may add `name` to the class `id`.
    ///
    /// Two refusals, and the same reason under both: an extension that replaced
    /// something would make every existing caller silently wrong, with no line
    /// in the program to point at. A rename is a small price and an obvious fix.
    ///
    /// C# prefers the real member instead, silently — a choice driven by a class
    /// library that has to keep growing without breaking callers. Quince has nine
    /// builtin types with single-digit method counts, so the loud answer is
    /// affordable here and would not be there.
    pub(super) fn may_extend(&self, id: ObjId, decl: &FnDecl, span: Span) -> Result<()> {
        let name = &decl.name;
        let type_name = || self.heap.class(id).name.clone();
        // First, because it is the only one of the three that is about the type
        // rather than about the name being added: a closed type refuses the block,
        // and which method it happens to start with is beside the point.
        if self.heap.class(id).openness.closes_extension() {
            let word = self.heap.class(id).openness.word().unwrap_or_default();
            return Err(QuinceError::new(
                format!("{} cannot be extended", type_name()),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(format!(
                "`{}` is declared `{word}`, and a method added from outside is exactly what \
                 that closes — declare this one in the class body",
                type_name()
            )));
        }

        if let Some(op) = decl.op {
            let class = self.heap.class(id);
            let natively_supported = if let Some(builtin) = class.builtin {
                builtin.natively_supports_op(op)
            } else {
                false
            };
            if natively_supported || class.slot(op).is_some() {
                return Err(QuinceError::new(
                    format!("`{}` natively supports `op {}` and cannot be overridden by an extension", type_name(), op.name()),
                    span,
                )
                .with_kind(ErrorKind::Type)
                .with_help(
                    "an extension may only add ops that the type does not already natively support",
                ));
            }
        } else if self.heap.class(id).method(name, &self.heap).is_some() {
            return Err(QuinceError::new(
                format!("{} already has a method `{name}`", type_name()),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help(
                "an extension adds to a type and never replaces part of it, because every \
                 existing call would quietly start meaning something else — give this one \
                 another name",
            ));
        }

        if self.extensions.contains_key(&(id, name.to_string())) {
            return Err(QuinceError::new(
                format!("{} has already been extended with `{name}`", type_name()),
                span,
            )
            .with_kind(ErrorKind::Type)
            .with_help("the first `extend` won, and this one would replace it just as silently"));
        }
        Ok(())
    }

    /// The class `super` searches from, read through the slot the resolver
    /// assigned it in the scope wrapped around the methods.
    ///
    /// Separate from the receiver, which comes from the enclosing method's
    /// parameters — the two halves of `super` live in different scopes, and
    /// neither is searched for by name.
    pub(super) fn super_class(&mut self, parent: &Var, env: ObjId, span: Span) -> Result<ObjId> {
        match self.read(parent, env, span)? {
            Value::Class(id) => Ok(id),
            _ => unreachable!("`super` is only ever bound to a class"),
        }
    }

    /// Looks `name` up starting *at* the superclass, which is what stops an
    /// override from calling itself: `Dog.speak` reaching for `super.speak`
    /// must not find `Dog.speak` again.
    pub(super) fn super_method(&mut self, id: ObjId, name: &str, span: Span) -> Result<Value> {
        match self.find_method(id, name) {
            Some(method) => Ok(method),
            None => {
                let parent = self.heap.class(id).name.clone();
                let mut names = self.heap.class(id).method_names(&self.heap);
                names.sort();
                let err = QuinceError::new(format!("{parent} has no method `{name}`"), span)
                    .with_kind(ErrorKind::Attr);
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                Err(match crate::error::did_you_mean(name, refs) {
                    Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
                    None if names.is_empty() => err.with_help(format!(
                        "`{parent}` declares no methods, so `super` reaches nothing here"
                    )),
                    None => err.with_help(format!("`{parent}` has: {}", names.join(", "))),
                })
            }
        }
    }

    /// A builtin's method reached through an instance that has no payload yet.
    ///
    /// The resolver refuses the ordinary way to arrive here — an `op init` with no
    /// `super.init` — but it works on names in one pass, so `final S = string`
    /// followed by `class X extends S` gets past it. This is what that costs, and
    /// it is a report rather than the panic a native would otherwise hit.
    pub(super) fn no_payload(&self, id: ObjId, span: Span) -> Raised {
        let class = self.heap.instance(id).class;
        let name = self.heap.class(class).name.clone();
        // A native is only ever found on an instance by walking the class chain to
        // a builtin, since every method written in Quince is a `Function`. So
        // there is always a builtin to name, and the span says which use of it
        // asked — a method call, or the inherited conversion a class with no
        // `op init` of its own ends up with as its constructor.
        let builtin = self
            .builtin_base(class)
            .expect("a native method is only reachable through a builtin in the chain");
        QuinceError::new(
            format!("`{name}` was never given {}", an(builtin.name())),
            span,
        )
        .with_kind(ErrorKind::Type)
        .with_help(format!(
            "`{name}` extends `{}`, so its `op init` must call `super.init` before it is used",
            builtin.name()
        ))
    }

    pub(super) fn no_attr(
        &self,
        receiver: &Value,
        name: &str,
        target_span: Span,
        name_span: Span,
        expr_span: Span,
    ) -> Raised {
        // An instance can grow fields at run time, so a missing name there is a
        // different mistake from a missing method on a builtin type.
        let what = match receiver {
            Value::Instance(_) => "no field or method",
            _ => "no method",
        };
        let mut err = QuinceError::new(
            format!("{} has {what} `{name}`", receiver.type_name(&self.heap)),
            expr_span,
        )
        .with_kind(ErrorKind::Attr);

        if target_span.start < target_span.end {
            err = err.with_label(target_span, receiver.type_name(&self.heap));
        }
        if name_span.start < name_span.end {
            err = err.with_label(name_span, format!("no field or method `{name}`"));
        }

        let mut candidates: Vec<String> = Vec::new();
        if let Value::Instance(id) = receiver {
            for (key, _) in self.heap.instance(*id).fields.iter() {
                if let crate::runtime::dict::Key::Str(s) = key {
                    candidates.push(s.to_string());
                }
            }
            let class = self.heap.instance(*id).class;
            candidates.extend(self.heap.class(class).method_names(&self.heap));
        } else if let Value::Class(id) = receiver {
            candidates.extend(self.heap.class(*id).method_names(&self.heap));
        } else {
            let class = receiver.class(&self.heap);
            candidates.extend(self.heap.class(class).method_names(&self.heap));
        }

        // A near-miss gets the name it probably meant. Everything else gets the
        // list, which is short for a builtin and is the answer to the question
        // the reader is about to ask anyway — and a bare "no such method" that
        // does not say what there *is* leaves them reading the source instead.
        let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
        err = match crate::error::did_you_mean(name, refs) {
            Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
            None if candidates.is_empty() => err.with_help(format!(
                "{} has no members to reach",
                receiver.type_name(&self.heap)
            )),
            None => {
                candidates.sort();
                err.with_help(format!("it has: {}", candidates.join(", ")))
            }
        };

        err
    }
}

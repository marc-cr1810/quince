use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::runtime::heap::{Heap, ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::ast::{BindKind, TypeExpr};

#[derive(Clone, Debug)]
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
    /// What the declaration said the name holds, if it said.
    ///
    /// Kept past the declaration because an annotation is about the *name* and
    /// not about the one value that happened to be bound first: `let x: int = 0`
    /// followed by `x = "s"` is the same mistake as writing the string in the
    /// first place, and without this the second line has nothing to check
    /// against. See [`SlotData::ty`] for the local form.
    pub ty: Option<Rc<TypeExpr>>,
}

/// What a local slot holds, and what it was declared to hold.
///
/// A struct rather than a bare `Value` for the reason [`Binding::ty`] gives.
/// Locals need no `mutable` flag: the resolver refuses a write to a `final` or
/// `const` local statically, which is why that check does not appear here and
/// does appear on [`Binding`].
#[derive(Clone, Debug)]
pub struct SlotData {
    pub value: Value,
    /// The annotation the declaration carried. `None` is the unannotated
    /// binding, which is every binding a v0.6 program wrote.
    ///
    /// Shared, because the same annotation is read on every write to the name
    /// and cloning a nested `list[dict[string, int]]` per assignment would be
    /// paying for the check twice over.
    pub ty: Option<Rc<TypeExpr>>,
    /// The word the slot was bound with.
    ///
    /// Separate from [`SlotData::ty`] and not folded into its `frozen` flag,
    /// because they are two different claims: `final` fixes the *name* and
    /// `const T` freezes the *value*, and `final xs: list[int]` makes both at
    /// once about different things. Overloading one field for both would make
    /// `final` freeze a list it was never meant to touch.
    ///
    /// A local declared `final` is already refused a rebinding by the resolver.
    /// This is here for the one binding the resolver cannot see a keyword on: a
    /// parameter, whose `const` or `final` is written at the declaration and
    /// read at the call.
    pub bind: BindKind,
}

/// A local scope: a flat run of slots, addressed by index.
///
/// The resolver assigns the indices, so a variable read is a bounds-checked
/// `Vec` index rather than hashing a `String` against a chain of maps.
///
/// Scopes live in the heap rather than behind an `Rc` because closures capture
/// the scope they were defined in, and that scope usually holds the closure —
/// every recursive function is a cycle. Handles make that a non-event.
#[derive(Clone, Debug)]
pub struct Env {
    /// `None` is a slot whose declaration has not run yet. The resolver hoists
    /// declarations to the top of their scope so a nested function can see a
    /// sibling declared below it, which means a slot can legitimately be
    /// reached before it holds anything.
    slots: Vec<Option<SlotData>>,
    parent: Option<ObjId>,
}

impl Env {
    pub fn new(parent: Option<ObjId>, slot_count: u16) -> Self {
        Env {
            slots: vec![None; slot_count as usize],
            parent,
        }
    }

    pub fn parent(&self) -> Option<ObjId> {
        self.parent
    }

    /// Binds a slot, keeping whatever annotation it already carried.
    ///
    /// The annotation belongs to the declaration and the declaration runs once;
    /// every write after it is an assignment, and an assignment must not be able
    /// to drop the constraint it was just checked against.
    pub fn set(&mut self, index: u16, value: Value) {
        let (ty, bind) = match &self.slots[index as usize] {
            Some(slot) => (slot.ty.clone(), slot.bind),
            None => (None, BindKind::Let),
        };
        self.slots[index as usize] = Some(SlotData { value, ty, bind });
    }

    /// Binds a slot and records what its declaration said about it.
    pub fn declare(
        &mut self,
        index: u16,
        value: Value,
        ty: Option<Rc<TypeExpr>>,
        bind: BindKind,
    ) {
        self.slots[index as usize] = Some(SlotData { value, ty, bind });
    }

    /// The word slot `index` was bound with.
    pub fn bind_kind(&self, index: u16) -> BindKind {
        self.slots[index as usize]
            .as_ref()
            .map_or(BindKind::Let, |slot| slot.bind)
    }

    pub fn get(&self, index: u16) -> Option<&Value> {
        self.slots[index as usize]
            .as_ref()
            .map(|slot| &slot.value)
    }

    /// What slot `index` was declared to hold, if its declaration said.
    pub fn ty(&self, index: u16) -> Option<Rc<TypeExpr>> {
        self.slots[index as usize]
            .as_ref()
            .and_then(|slot| slot.ty.clone())
    }

    /// Pushes every handle this scope keeps alive, for the collector's mark
    /// phase. The parent link counts: an inner scope keeps its enclosing ones
    /// reachable.
    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(
            self.slots
                .iter()
                .flatten()
                .filter_map(|slot| slot.value.handle()),
        );
        worklist.extend(self.parent);
    }
}

/// The top-level scope of one module, which stays keyed by name.
///
/// Globals cannot be slotted: the REPL introduces them a line at a time, and a
/// program may call a function declared further down the file. Both need a
/// lookup that can fail at run time.
///
/// This is also what a module *is*. A module has a name table, and so does the
/// top-level scope; the only thing an imported file adds is knowing what it is
/// called and where it came from, which is the two fields below. Nothing else
/// about a module needed inventing.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    vars: HashMap<String, Binding>,
    /// What the module is called where it is imported. `None` for the module a
    /// program starts in, which nothing imports and so nothing needs to name.
    name: Option<String>,
    /// The file it was read from, which is what a relative import resolves
    /// against. `None` for a stdlib module, which came from no file, and for the
    /// REPL, which resolves against the working directory instead.
    path: Option<PathBuf>,
}

impl Globals {
    pub fn new() -> Self {
        Globals::default()
    }

    /// The scope of a module something imported, which knows both.
    pub fn module(name: impl Into<String>, path: Option<PathBuf>) -> Self {
        Globals {
            vars: HashMap::new(),
            name: Some(name.into()),
            path,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Tells the starting module where it was read from, which it learns from
    /// the CLI rather than from an import.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    /// Redeclaring a global replaces it, so a REPL session can redefine a
    /// function without restarting.
    pub fn declare(&mut self, name: impl Into<String>, value: Value, mutable: bool) {
        self.declare_typed(name, value, mutable, None);
    }

    /// Declares a global, recording what it was annotated as.
    pub fn declare_typed(
        &mut self,
        name: impl Into<String>,
        value: Value,
        mutable: bool,
        ty: Option<Rc<TypeExpr>>,
    ) {
        self.vars.insert(
            name.into(),
            Binding {
                value,
                mutable,
                ty,
            },
        );
    }

    /// What `name` was declared to hold, if its declaration said.
    pub fn ty(&self, name: &str) -> Option<Rc<TypeExpr>> {
        self.vars.get(name).and_then(|binding| binding.ty.clone())
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name).map(|binding| &binding.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.vars
            .iter()
            .map(|(name, binding)| (name.as_str(), &binding.value))
    }

    pub fn assign(&mut self, name: &str, value: Value) -> Result<(), AssignError> {
        match self.vars.get_mut(name) {
            None => Err(AssignError::Undefined),
            Some(binding) if !binding.mutable => Err(AssignError::Immutable),
            Some(binding) => {
                binding.value = value;
                Ok(())
            }
        }
    }

    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.vars.values().filter_map(|b| b.value.handle()));
    }
}

pub enum AssignError {
    Undefined,
    Immutable,
}

/// Walks out `hops` scopes. The resolver counted these against the same chain
/// the evaluator builds, so overrunning it is a compiler bug, not a user error.
pub fn ancestor(heap: &Heap, env: ObjId, hops: u16) -> ObjId {
    let mut current = env;
    for _ in 0..hops {
        current = heap
            .env(current)
            .parent()
            .expect("the resolver counted more scopes than exist");
    }
    current
}

/// Walks out to the module `env` was written in — the [`Globals`] every scope
/// chain ends at.
///
/// This is what makes a name resolve in the file that spelled it. [`Slot::Global`]
/// is looked up by name at run time, and until modules existed that name was
/// looked up in the one global scope there was; now it is looked up in the scope
/// at the root of the chain the code was compiled into. A `Function` captures the
/// scope it was defined in, so a function written in one module and *called* from
/// another still reads its own module's names, which is what lexical scoping
/// already meant everywhere else in the language. Nothing had to be recorded on
/// the function to make that true.
///
/// The walk is as long as the code is nested, and only global reads pay it. That
/// is nothing beside what a global read already does — hash a string. If it ever
/// stops being nothing, the fix is a module handle on `Function` and a current
/// module on the interpreter, pushed and restored like every other frame state.
///
/// [`Slot::Global`]: crate::syntax::ast::Slot::Global
pub fn module_of(heap: &Heap, env: ObjId) -> ObjId {
    let mut current = env;
    loop {
        match heap.get(current) {
            Object::Globals(_) => return current,
            Object::Env(scope) => {
                current = scope
                    .parent()
                    .expect("a scope chain ends at a module, not at nothing");
            }
            other => panic!("expected a scope, found {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::heap::Object;

    fn scope(heap: &mut Heap, parent: Option<ObjId>, slots: u16) -> ObjId {
        heap.alloc(Object::Env(Env::new(parent, slots)))
    }

    #[test]
    fn slots_start_empty_and_hold_what_is_put_in_them() {
        let mut heap = Heap::new();
        let env = scope(&mut heap, None, 2);
        assert_eq!(heap.env(env).get(0), None);

        heap.env_mut(env).set(0, Value::Int(1));
        assert_eq!(heap.env(env).get(0), Some(&Value::Int(1)));
        assert_eq!(heap.env(env).get(1), None);
    }

    #[test]
    fn hops_walk_outwards_through_the_parent_chain() {
        let mut heap = Heap::new();
        let outer = scope(&mut heap, None, 1);
        heap.env_mut(outer).set(0, Value::Int(1));
        let middle = scope(&mut heap, Some(outer), 1);
        let inner = scope(&mut heap, Some(middle), 1);

        assert_eq!(ancestor(&heap, inner, 0), inner);
        assert_eq!(ancestor(&heap, inner, 1), middle);
        assert_eq!(ancestor(&heap, inner, 2), outer);
    }

    #[test]
    fn a_scope_chain_leads_back_to_the_module_it_was_written_in() {
        // Two modules, and a scope nested three deep inside the second. The
        // walk has to find the module the chain hangs off rather than the first
        // one allocated, which is what the interpreter did — read a single field
        // — for as long as there was only one module to find.
        let mut heap = Heap::new();
        let first = heap.alloc(Object::Globals(Globals::new()));
        let second = heap.alloc(Object::Globals(Globals::module("other", None)));

        let outer = scope(&mut heap, Some(second), 1);
        let inner = scope(&mut heap, Some(outer), 1);

        assert_eq!(module_of(&heap, inner), second);
        assert_eq!(module_of(&heap, outer), second);
        // A module is its own module, which is the case top-level code takes:
        // `run` executes statements with the module itself as the scope.
        assert_eq!(module_of(&heap, second), second);
        assert_eq!(module_of(&heap, first), first);
    }

    #[test]
    fn globals_refuse_assignment_to_a_bound_name() {
        let mut globals = Globals::new();
        globals.declare("k", Value::Int(1), false);

        assert!(matches!(
            globals.assign("k", Value::Int(2)),
            Err(AssignError::Immutable)
        ));
        assert_eq!(globals.get("k"), Some(&Value::Int(1)));
    }

    #[test]
    fn assigning_an_unknown_global_is_an_error() {
        let mut globals = Globals::new();
        assert!(matches!(
            globals.assign("ghost", Value::Nil),
            Err(AssignError::Undefined)
        ));
    }

    #[test]
    fn redeclaring_a_global_replaces_it() {
        let mut globals = Globals::new();
        globals.declare("x", Value::Int(1), true);
        globals.declare("x", Value::Int(2), true);
        assert_eq!(globals.get("x"), Some(&Value::Int(2)));
    }
}

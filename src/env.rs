use std::collections::HashMap;

use crate::heap::{Heap, ObjId};
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
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
    slots: Vec<Option<Value>>,
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

    pub fn set(&mut self, index: u16, value: Value) {
        self.slots[index as usize] = Some(value);
    }

    pub fn get(&self, index: u16) -> Option<&Value> {
        self.slots[index as usize].as_ref()
    }

    /// Pushes every handle this scope keeps alive, for the collector's mark
    /// phase. The parent link counts: an inner scope keeps its enclosing ones
    /// reachable.
    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.slots.iter().flatten().filter_map(Value::handle));
        worklist.extend(self.parent);
    }
}

/// The top-level scope, which stays keyed by name.
///
/// Globals cannot be slotted: the REPL introduces them a line at a time, and a
/// program may call a function declared further down the file. Both need a
/// lookup that can fail at run time.
#[derive(Clone, Debug, Default)]
pub struct Globals {
    vars: HashMap<String, Binding>,
}

impl Globals {
    pub fn new() -> Self {
        Globals::default()
    }

    /// Redeclaring a global replaces it, so a REPL session can redefine a
    /// function without restarting.
    pub fn declare(&mut self, name: impl Into<String>, value: Value, mutable: bool) {
        self.vars.insert(name.into(), Binding { value, mutable });
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name).map(|binding| &binding.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.vars.iter().map(|(name, binding)| (name.as_str(), &binding.value))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::Object;

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

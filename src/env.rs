use std::collections::HashMap;

use crate::heap::{Heap, ObjId};
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
}

/// A lexical scope.
///
/// Scopes live in the heap rather than behind an `Rc` because closures capture
/// the scope they were defined in, and that scope usually holds the closure —
/// every recursive function is a cycle. Handles make that a non-event.
#[derive(Clone, Debug)]
pub struct Env {
    vars: HashMap<String, Binding>,
    parent: Option<ObjId>,
}

impl Env {
    pub fn new(parent: Option<ObjId>) -> Self {
        Env {
            vars: HashMap::new(),
            parent,
        }
    }

    /// Declaring an existing name shadows it, rather than being an error.
    pub fn declare(&mut self, name: impl Into<String>, value: Value, mutable: bool) {
        self.vars.insert(name.into(), Binding { value, mutable });
    }
}

pub enum AssignError {
    Undefined,
    Immutable,
}

/// Walks the scope chain outwards looking for `name`.
pub fn lookup(heap: &Heap, env: ObjId, name: &str) -> Option<Value> {
    let mut current = env;
    loop {
        let scope = heap.env(current);
        if let Some(binding) = scope.vars.get(name) {
            return Some(binding.value.clone());
        }
        current = scope.parent?;
    }
}

/// Assigns to an existing binding, refusing to touch a `const`.
pub fn assign(heap: &mut Heap, env: ObjId, name: &str, value: Value) -> Result<(), AssignError> {
    let mut current = env;
    loop {
        let scope = heap.env_mut(current);
        if let Some(binding) = scope.vars.get_mut(name) {
            if !binding.mutable {
                return Err(AssignError::Immutable);
            }
            binding.value = value;
            return Ok(());
        }
        match scope.parent {
            Some(parent) => current = parent,
            None => return Err(AssignError::Undefined),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::Object;

    fn scope(heap: &mut Heap, parent: Option<ObjId>) -> ObjId {
        heap.alloc(Object::Env(Env::new(parent)))
    }

    #[test]
    fn lookup_finds_bindings_in_enclosing_scopes() {
        let mut heap = Heap::new();
        let outer = scope(&mut heap, None);
        heap.env_mut(outer).declare("x", Value::Int(1), true);
        let inner = scope(&mut heap, Some(outer));

        assert_eq!(lookup(&heap, inner, "x"), Some(Value::Int(1)));
        assert_eq!(lookup(&heap, inner, "nope"), None);
    }

    #[test]
    fn inner_declarations_shadow_outer_ones() {
        let mut heap = Heap::new();
        let outer = scope(&mut heap, None);
        heap.env_mut(outer).declare("x", Value::Int(1), true);
        let inner = scope(&mut heap, Some(outer));
        heap.env_mut(inner).declare("x", Value::Int(2), true);

        assert_eq!(lookup(&heap, inner, "x"), Some(Value::Int(2)));
        assert_eq!(lookup(&heap, outer, "x"), Some(Value::Int(1)));
    }

    #[test]
    fn assignment_reaches_the_enclosing_scope() {
        let mut heap = Heap::new();
        let outer = scope(&mut heap, None);
        heap.env_mut(outer).declare("x", Value::Int(1), true);
        let inner = scope(&mut heap, Some(outer));

        assert!(assign(&mut heap, inner, "x", Value::Int(9)).is_ok());
        assert_eq!(lookup(&heap, outer, "x"), Some(Value::Int(9)));
    }

    #[test]
    fn constants_refuse_assignment() {
        let mut heap = Heap::new();
        let env = scope(&mut heap, None);
        heap.env_mut(env).declare("k", Value::Int(1), false);

        assert!(matches!(
            assign(&mut heap, env, "k", Value::Int(2)),
            Err(AssignError::Immutable)
        ));
        assert_eq!(lookup(&heap, env, "k"), Some(Value::Int(1)));
    }

    #[test]
    fn assigning_an_unknown_name_is_an_error() {
        let mut heap = Heap::new();
        let env = scope(&mut heap, None);
        assert!(matches!(
            assign(&mut heap, env, "ghost", Value::Nil),
            Err(AssignError::Undefined)
        ));
    }
}

use crate::env::Env;
use crate::value::{Function, Value};

/// A handle into the [`Heap`].
///
/// Objects are addressed by index rather than by pointer so that cyclic
/// references are just integers pointing at each other, which the borrow checker
/// has no opinion about. It also leaves a tracing collector as a plain mark pass
/// over a contiguous `Vec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjId(u32);

#[derive(Clone, Debug)]
pub enum Object {
    List(Vec<Value>),
    Env(Env),
    Function(Function),
}

/// The arena holding every object that can participate in a reference cycle.
///
/// Strings deliberately live outside it — see [`Value::Str`].
#[derive(Default)]
pub struct Heap {
    objects: Vec<Object>,
}

impl Heap {
    pub fn new() -> Self {
        Heap::default()
    }

    /// Nothing is ever freed yet. Collection needs the interpreter's roots, so
    /// it arrives with the evaluator rather than being guessed at now.
    pub fn alloc(&mut self, object: Object) -> ObjId {
        self.objects.push(object);
        ObjId((self.objects.len() - 1) as u32)
    }

    pub fn get(&self, id: ObjId) -> &Object {
        &self.objects[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: ObjId) -> &mut Object {
        &mut self.objects[id.0 as usize]
    }

    /// Handles only ever come from `alloc`, so a mismatched variant is a bug in
    /// the evaluator rather than a user error.
    pub fn list(&self, id: ObjId) -> &Vec<Value> {
        match self.get(id) {
            Object::List(items) => items,
            other => panic!("expected a list, found {other:?}"),
        }
    }

    pub fn list_mut(&mut self, id: ObjId) -> &mut Vec<Value> {
        match self.get_mut(id) {
            Object::List(items) => items,
            other => panic!("expected a list, found {other:?}"),
        }
    }

    pub fn env(&self, id: ObjId) -> &Env {
        match self.get(id) {
            Object::Env(env) => env,
            other => panic!("expected a scope, found {other:?}"),
        }
    }

    pub fn env_mut(&mut self, id: ObjId) -> &mut Env {
        match self.get_mut(id) {
            Object::Env(env) => env,
            other => panic!("expected a scope, found {other:?}"),
        }
    }

    pub fn function(&self, id: ObjId) -> &Function {
        match self.get(id) {
            Object::Function(func) => func,
            other => panic!("expected a function, found {other:?}"),
        }
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_returns_distinct_handles() {
        let mut heap = Heap::new();
        let a = heap.alloc(Object::List(vec![]));
        let b = heap.alloc(Object::List(vec![]));
        assert_ne!(a, b);
        assert_eq!(heap.len(), 2);
    }

    #[test]
    fn objects_round_trip_through_their_handle() {
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(heap.list(id).len(), 2);
    }

    #[test]
    fn objects_are_mutable_through_their_handle() {
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id).push(Value::Int(7));
        assert_eq!(heap.list(id)[0], Value::Int(7));
    }

    #[test]
    fn a_list_may_contain_itself() {
        // The reason for handles: this is a cycle, and it costs nothing here.
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id).push(Value::List(id));
        let Value::List(inner) = heap.list(id)[0] else {
            panic!("expected a nested list");
        };
        assert_eq!(inner, id);
    }
}

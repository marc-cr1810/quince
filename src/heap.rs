use crate::dict::Dict;
use crate::env::{Env, Globals};
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
    Dict(Dict),
    Env(Env),
    /// The top-level scope. Exactly one exists, and it is the collector's
    /// permanent root.
    Globals(Globals),
    Function(Function),
}

/// Live objects at which a collection is worth doing. Below this the mark phase
/// costs more than the memory it reclaims.
const MIN_THRESHOLD: usize = 256;

/// The arena holding every object that can participate in a reference cycle.
///
/// Strings deliberately live outside it — see [`Value::Str`].
pub struct Heap {
    /// A freed slot becomes `None` rather than being removed, so that live
    /// handles never shift. Reclaimed indices are reused via `free`.
    objects: Vec<Option<Object>>,
    free: Vec<u32>,
    live: usize,
    threshold: usize,
    /// How many collections have run. Exposed for tests and `--stats`.
    pub collections: usize,
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            objects: Vec::new(),
            free: Vec::new(),
            live: 0,
            threshold: MIN_THRESHOLD,
            collections: 0,
        }
    }

    pub fn alloc(&mut self, object: Object) -> ObjId {
        self.live += 1;
        match self.free.pop() {
            Some(index) => {
                self.objects[index as usize] = Some(object);
                ObjId(index)
            }
            None => {
                self.objects.push(Some(object));
                ObjId((self.objects.len() - 1) as u32)
            }
        }
    }

    /// Whether the heap has grown enough since the last collection to be worth
    /// another. Deciding this here keeps the policy out of the evaluator, which
    /// only knows *when* collecting is safe.
    pub fn should_collect(&self) -> bool {
        self.live >= self.threshold
    }

    /// Mark and sweep, returning the number of objects reclaimed.
    ///
    /// `roots` are the handles reachable from outside the heap. Anything not
    /// reachable from one of them is freed, so an incomplete root set is a
    /// use-after-free — see the safe-point rules in `interp.rs`.
    pub fn collect(&mut self, roots: &[ObjId]) -> usize {
        let mut marked = vec![false; self.objects.len()];
        let mut worklist: Vec<ObjId> = roots.to_vec();

        // Marking on pop rather than on push is what makes cycles terminate: a
        // handle may be queued many times, but its children are traced once.
        while let Some(id) = worklist.pop() {
            let index = id.0 as usize;
            if marked[index] {
                continue;
            }
            marked[index] = true;
            trace(self.get(id), &mut worklist);
        }

        let mut freed = 0;
        for (index, slot) in self.objects.iter_mut().enumerate() {
            if !marked[index] && slot.take().is_some() {
                self.free.push(index as u32);
                freed += 1;
            }
        }

        self.live -= freed;
        // Grow the threshold with the surviving set, so a program with a large
        // live heap does not collect on every statement.
        self.threshold = (self.live * 2).max(MIN_THRESHOLD);
        self.collections += 1;
        freed
    }

    /// A handle that outlives its object is a collector bug — a missing root —
    /// not something a Quince program can cause.
    pub fn get(&self, id: ObjId) -> &Object {
        self.objects[id.0 as usize]
            .as_ref()
            .expect("handle points at a collected object")
    }

    pub fn get_mut(&mut self, id: ObjId) -> &mut Object {
        self.objects[id.0 as usize]
            .as_mut()
            .expect("handle points at a collected object")
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

    pub fn dict(&self, id: ObjId) -> &Dict {
        match self.get(id) {
            Object::Dict(entries) => entries,
            other => panic!("expected a dict, found {other:?}"),
        }
    }

    pub fn dict_mut(&mut self, id: ObjId) -> &mut Dict {
        match self.get_mut(id) {
            Object::Dict(entries) => entries,
            other => panic!("expected a dict, found {other:?}"),
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

    pub fn globals(&self, id: ObjId) -> &Globals {
        match self.get(id) {
            Object::Globals(globals) => globals,
            other => panic!("expected the global scope, found {other:?}"),
        }
    }

    pub fn globals_mut(&mut self, id: ObjId) -> &mut Globals {
        match self.get_mut(id) {
            Object::Globals(globals) => globals,
            other => panic!("expected the global scope, found {other:?}"),
        }
    }

    pub fn function(&self, id: ObjId) -> &Function {
        match self.get(id) {
            Object::Function(func) => func,
            other => panic!("expected a function, found {other:?}"),
        }
    }

    /// Objects currently allocated. Not the arena's size — freed slots are
    /// still there, waiting to be reused.
    pub fn live(&self) -> usize {
        self.live
    }
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

/// Pushes every handle an object keeps alive.
///
/// Adding a variant to [`Object`] makes this fail to compile, which is the
/// point: a collector that silently forgets to trace a new object type produces
/// bugs that are almost impossible to find.
fn trace(object: &Object, worklist: &mut Vec<ObjId>) {
    match object {
        Object::List(items) => worklist.extend(items.iter().filter_map(Value::handle)),
        Object::Dict(entries) => entries.trace(worklist),
        Object::Env(env) => env.trace(worklist),
        Object::Globals(globals) => globals.trace(worklist),
        Object::Function(func) => worklist.push(func.env),
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
        assert_eq!(heap.live(), 2);
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

    #[test]
    fn collection_frees_unreachable_objects() {
        let mut heap = Heap::new();
        let kept = heap.alloc(Object::List(vec![]));
        heap.alloc(Object::List(vec![]));

        assert_eq!(heap.collect(&[kept]), 1);
        assert_eq!(heap.live(), 1);
        assert!(heap.list(kept).is_empty());
    }

    #[test]
    fn collection_follows_handles_held_inside_objects() {
        let mut heap = Heap::new();
        let inner = heap.alloc(Object::List(vec![]));
        let outer = heap.alloc(Object::List(vec![Value::List(inner)]));

        assert_eq!(heap.collect(&[outer]), 0);
        assert_eq!(heap.live(), 2);
    }

    #[test]
    fn an_unreachable_cycle_is_collected() {
        // The case `Rc` cannot handle, and the reason the arena exists.
        let mut heap = Heap::new();
        let a = heap.alloc(Object::List(vec![]));
        let b = heap.alloc(Object::List(vec![Value::List(a)]));
        heap.list_mut(a).push(Value::List(b));

        assert_eq!(heap.collect(&[]), 2);
        assert_eq!(heap.live(), 0);
    }

    #[test]
    fn a_reachable_cycle_survives_and_terminates() {
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id).push(Value::List(id));

        assert_eq!(heap.collect(&[id]), 0);
        assert_eq!(heap.live(), 1);
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut heap = Heap::new();
        let kept = heap.alloc(Object::List(vec![]));
        let stale = heap.alloc(Object::List(vec![]));
        heap.collect(&[kept]);

        let fresh = heap.alloc(Object::List(vec![Value::Int(1)]));
        assert_eq!(fresh, stale, "the free slot should have been reused");
        assert_eq!(heap.live(), 2);
    }

    #[test]
    fn the_threshold_grows_with_the_surviving_set() {
        let mut heap = Heap::new();
        let mut roots = Vec::new();
        for _ in 0..MIN_THRESHOLD {
            roots.push(heap.alloc(Object::List(vec![])));
        }
        assert!(heap.should_collect());

        heap.collect(&roots);
        // Everything survived, so collecting again immediately would be futile.
        assert!(!heap.should_collect());
    }
}

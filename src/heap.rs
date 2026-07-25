use crate::class::{BUILTINS, Builtin, Class, Instance};
use crate::dict::Dict;
use crate::env::{Env, Globals};
use crate::value::{BoundMethod, Function, Value};

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
    BoundMethod(BoundMethod),
    Class(Class),
    Instance(Instance),
}

/// A mutation refused because `const` froze the object.
///
/// Carries nothing: the caller already knows which operation it was attempting
/// and where, which is everything a useful message needs and more than this
/// could supply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frozen;

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
    /// Parallel to `objects`: whether the object at that index has been frozen
    /// by `const`. Kept alongside rather than inside [`Object`] so that the
    /// three mutable variants do not each grow a field, and so that the flag is
    /// indexed by exactly the same number as the object it describes.
    ///
    /// It must be cleared when a slot is reused, or a fresh list inherits the
    /// frozenness of whatever died there — see `alloc`.
    frozen: Vec<bool>,
    free: Vec<u32>,
    live: usize,
    threshold: usize,
    /// How many collections have run. Exposed for tests and `--stats`.
    pub collections: usize,
    /// The class object for each builtin type, indexed by [`Builtin::index`].
    ///
    /// Here rather than on the interpreter because
    /// [`Value::class`](crate::value::Value::class) takes only a `&Heap`, and it
    /// is what every type error in the language goes through — putting the table
    /// anywhere else would mean threading an `&Interp` into all of them.
    ///
    /// Nothing outside the heap holds these handles, so the heap roots them
    /// itself; see [`Heap::collect`].
    builtin_classes: Vec<ObjId>,
}

impl Heap {
    pub fn new() -> Self {
        let mut heap = Heap {
            objects: Vec::new(),
            frozen: Vec::new(),
            free: Vec::new(),
            live: 0,
            threshold: MIN_THRESHOLD,
            collections: 0,
            builtin_classes: Vec::new(),
        };
        // Before anything else, so the order matches `Builtin::index` and needs
        // no interpreter: a name and a table of natives is all a builtin class
        // is, and both come from a `static`.
        let classes = BUILTINS
            .iter()
            .map(|builtin| heap.alloc(Object::Class(Class::builtin(*builtin))))
            .collect();
        heap.builtin_classes = classes;
        heap
    }

    /// The class object for a builtin type.
    pub fn builtin_class(&self, builtin: Builtin) -> ObjId {
        self.builtin_classes[builtin.index()]
    }

    pub fn alloc(&mut self, object: Object) -> ObjId {
        self.live += 1;
        match self.free.pop() {
            Some(index) => {
                self.objects[index as usize] = Some(object);
                // A reused slot starts thawed. Frozenness belongs to the object
                // that was there, not to the index.
                self.frozen[index as usize] = false;
                ObjId(index)
            }
            None => {
                self.objects.push(Some(object));
                self.frozen.push(false);
                ObjId((self.objects.len() - 1) as u32)
            }
        }
    }

    /// Whether `id` has been frozen by `const`.
    pub fn is_frozen(&self, id: ObjId) -> bool {
        self.frozen[id.0 as usize]
    }

    /// Freezes everything reachable from `value` through its *data*.
    ///
    /// Deep, because an immutable list of mutable lists is not an immutable
    /// value, and the whole point of `const` is to mean what it says. Frozen on
    /// pop rather than on push for the collector's reason: the graph may contain
    /// cycles, and this is what makes them terminate.
    pub fn freeze(&mut self, value: &Value) {
        let Some(root) = value.handle() else {
            return;
        };
        let mut worklist = vec![root];
        while let Some(id) = worklist.pop() {
            if self.is_frozen(id) {
                continue;
            }
            self.frozen[id.0 as usize] = true;
            reachable_data(self.get(id), &mut worklist);
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
    ///
    /// The builtin classes are added to whatever the caller passes. They are
    /// permanently live and reachable from nowhere the caller can see: a program
    /// that never mentions `int` still needs `int`'s class the moment it adds two
    /// numbers wrong and the error asks for a type name. This is the only root
    /// the heap contributes on its own behalf.
    pub fn collect(&mut self, roots: &[ObjId]) -> usize {
        let mut marked = vec![false; self.objects.len()];
        let mut worklist: Vec<ObjId> = roots
            .iter()
            .chain(self.builtin_classes.iter())
            .copied()
            .collect();

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

    /// Private, so that the only ways to obtain a `&mut` to a freezable object
    /// are the accessors below, which check. A `pub` escape hatch here would
    /// make `const` advisory.
    fn get_mut(&mut self, id: ObjId) -> &mut Object {
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

    /// The `Result` is the enforcement: `const` is only as good as the number of
    /// places that can forget to ask, and this makes that number zero.
    pub fn list_mut(&mut self, id: ObjId) -> Result<&mut Vec<Value>, Frozen> {
        self.thawed(id)?;
        match self.get_mut(id) {
            Object::List(items) => Ok(items),
            other => panic!("expected a list, found {other:?}"),
        }
    }

    pub fn dict(&self, id: ObjId) -> &Dict {
        match self.get(id) {
            Object::Dict(entries) => entries,
            other => panic!("expected a dict, found {other:?}"),
        }
    }

    pub fn dict_mut(&mut self, id: ObjId) -> Result<&mut Dict, Frozen> {
        self.thawed(id)?;
        match self.get_mut(id) {
            Object::Dict(entries) => Ok(entries),
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

    pub fn bound_method(&self, id: ObjId) -> &BoundMethod {
        match self.get(id) {
            Object::BoundMethod(bound) => bound,
            other => panic!("expected a bound method, found {other:?}"),
        }
    }

    pub fn class(&self, id: ObjId) -> &Class {
        match self.get(id) {
            Object::Class(class) => class,
            other => panic!("expected a class, found {other:?}"),
        }
    }

    pub fn instance(&self, id: ObjId) -> &Instance {
        match self.get(id) {
            Object::Instance(instance) => instance,
            other => panic!("expected an instance, found {other:?}"),
        }
    }

    pub fn instance_mut(&mut self, id: ObjId) -> Result<&mut Instance, Frozen> {
        self.thawed(id)?;
        match self.get_mut(id) {
            Object::Instance(instance) => Ok(instance),
            other => panic!("expected an instance, found {other:?}"),
        }
    }

    fn thawed(&self, id: ObjId) -> Result<(), Frozen> {
        match self.is_frozen(id) {
            true => Err(Frozen),
            false => Ok(()),
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
        // A bound method is often the only thing holding its receiver alive:
        // in `[1, 2].push`, the list is reachable from nowhere else.
        //
        // The method itself is a different matter. Today it is always reachable
        // anyway — it was found on the receiver's class, and the receiver above
        // reaches that class — so no test can be written that fails without the
        // second line. It stays because `trace`'s contract is "every handle
        // this object holds", not "every handle nothing else happens to reach":
        // the latter is an invariant living in two files at once, and the whole
        // point of the arena is that forgetting to trace something produces
        // bugs that cannot be found.
        Object::BoundMethod(bound) => {
            worklist.extend(bound.receiver.handle());
            worklist.extend(bound.method.handle());
        }
        Object::Class(class) => class.trace(worklist),
        // An instance keeps its class alive, which is what lets a class be
        // reachable only through the objects it made.
        Object::Instance(instance) => instance.trace(worklist),
    }
}

/// Pushes the handles [`Heap::freeze`] should follow: the ones an object holds
/// *as data*.
///
/// Deliberately not [`trace`], and the difference is the whole design of `const`.
/// A closure's captured scope is shared with the code that created it — at the
/// top level it *is* the globals — so following `Function::env` would let one
/// `const` freeze an unrelated function's locals, or the entire program's. A
/// function reached from a frozen list keeps working; what is frozen is the
/// list's inability to stop pointing at it.
///
/// Adding an [`Object`] variant makes this fail to compile alongside `trace`,
/// which is the point: the two questions are different, and a new object type
/// has to answer both.
fn reachable_data(object: &Object, worklist: &mut Vec<ObjId>) {
    match object {
        Object::List(items) => worklist.extend(items.iter().filter_map(Value::handle)),
        // Keys are hashable, so they are never heap objects — see `dict.rs`.
        Object::Dict(entries) => entries.trace(worklist),
        // Fields are data; the class holding the methods is not.
        Object::Instance(instance) => instance.fields.trace(worklist),
        // Nothing below here is mutable through a Quince expression, so nothing
        // below here can be frozen. Freezing them would be at best a no-op and
        // at worst — see above — a scope frozen at a distance.
        Object::Env(_)
        | Object::Globals(_)
        | Object::Function(_)
        | Object::BoundMethod(_)
        | Object::Class(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a fresh heap already holds: one class object per builtin type,
    /// permanently live. Counted rather than written down so that adding a
    /// builtin does not have to edit every assertion below.
    fn base() -> usize {
        Heap::new().live()
    }

    #[test]
    fn alloc_returns_distinct_handles() {
        let mut heap = Heap::new();
        let a = heap.alloc(Object::List(vec![]));
        let b = heap.alloc(Object::List(vec![]));
        assert_ne!(a, b);
        assert_eq!(heap.live(), base() + 2);
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
        heap.list_mut(id)
            .expect("never frozen here")
            .push(Value::Int(7));
        assert_eq!(heap.list(id)[0], Value::Int(7));
    }

    #[test]
    fn a_list_may_contain_itself() {
        // The reason for handles: this is a cycle, and it costs nothing here.
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id)
            .expect("never frozen here")
            .push(Value::List(id));
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
        assert_eq!(heap.live(), base() + 1);
        assert!(heap.list(kept).is_empty());
    }

    #[test]
    fn collection_follows_handles_held_inside_objects() {
        let mut heap = Heap::new();
        let inner = heap.alloc(Object::List(vec![]));
        let outer = heap.alloc(Object::List(vec![Value::List(inner)]));

        assert_eq!(heap.collect(&[outer]), 0);
        assert_eq!(heap.live(), base() + 2);
    }

    #[test]
    fn an_unreachable_cycle_is_collected() {
        // The case `Rc` cannot handle, and the reason the arena exists.
        let mut heap = Heap::new();
        let a = heap.alloc(Object::List(vec![]));
        let b = heap.alloc(Object::List(vec![Value::List(a)]));
        heap.list_mut(a)
            .expect("never frozen here")
            .push(Value::List(b));

        assert_eq!(heap.collect(&[]), 2);
        assert_eq!(heap.live(), base(), "everything the test allocated is gone");
    }

    #[test]
    fn a_reachable_cycle_survives_and_terminates() {
        let mut heap = Heap::new();
        let id = heap.alloc(Object::List(vec![]));
        heap.list_mut(id)
            .expect("never frozen here")
            .push(Value::List(id));

        assert_eq!(heap.collect(&[id]), 0);
        assert_eq!(heap.live(), base() + 1);
    }

    #[test]
    fn freed_slots_are_reused() {
        let mut heap = Heap::new();
        let kept = heap.alloc(Object::List(vec![]));
        let stale = heap.alloc(Object::List(vec![]));
        heap.collect(&[kept]);

        let fresh = heap.alloc(Object::List(vec![Value::Int(1)]));
        assert_eq!(fresh, stale, "the free slot should have been reused");
        assert_eq!(heap.live(), base() + 2);
    }

    #[test]
    fn the_builtin_classes_survive_a_collection_with_no_roots() {
        // Nothing outside the heap holds these handles, so a collector that took
        // `roots` at its word would free every type in the language — and the
        // next type error to ask for a name would read a collected object.
        let mut heap = Heap::new();
        assert_eq!(heap.collect(&[]), 0, "no builtin class was reclaimed");
        assert_eq!(heap.live(), base());
        assert_eq!(Value::Int(1).type_name(&heap), "int");
    }

    #[test]
    fn freezing_reaches_through_nested_data() {
        let mut heap = Heap::new();
        let inner = heap.alloc(Object::List(vec![]));
        let outer = heap.alloc(Object::List(vec![Value::List(inner)]));

        heap.freeze(&Value::List(outer));
        assert!(heap.is_frozen(inner), "a frozen list of mutable lists");
    }

    #[test]
    fn freezing_a_cycle_terminates() {
        let mut heap = Heap::new();
        let a = heap.alloc(Object::List(vec![]));
        let b = heap.alloc(Object::List(vec![Value::List(a)]));
        heap.list_mut(a)
            .expect("never frozen here")
            .push(Value::List(b));

        heap.freeze(&Value::List(a));
        assert!(heap.is_frozen(a) && heap.is_frozen(b));
    }

    #[test]
    fn freezing_stops_at_a_function() {
        // The decision `reachable_data` exists to make: a scope is shared with
        // whoever created it, so following a function into one would let a
        // `const` freeze code it has nothing to do with.
        let mut heap = Heap::new();
        let scope = heap.alloc(Object::Env(Env::new(None, 1)));
        let func = heap.alloc(Object::Function(Function {
            decl: std::rc::Rc::new(crate::ast::FnDecl {
                name: "f".to_string(),
                params: Vec::new(),
                body: crate::ast::Block {
                    stmts: Vec::new(),
                    slot_count: 0,
                    span: crate::token::Span::new(0, 0),
                },
                op: None,
            }),
            env: scope,
        }));
        let held = heap.alloc(Object::List(vec![Value::Function(func)]));

        heap.freeze(&Value::List(held));
        assert!(heap.is_frozen(held), "the list itself freezes");
        assert!(!heap.is_frozen(scope), "the function's scope must not");
    }

    #[test]
    fn a_reused_slot_starts_thawed() {
        // Frozenness belongs to the object, not to the index it happened to
        // occupy. Without the reset in `alloc`, a fresh list would inherit the
        // immutability of whatever died in its slot.
        let mut heap = Heap::new();
        let stale = heap.alloc(Object::List(vec![]));
        heap.freeze(&Value::List(stale));
        heap.collect(&[]);

        let fresh = heap.alloc(Object::List(vec![]));
        assert_eq!(fresh, stale, "the slot should have been reused");
        assert!(!heap.is_frozen(fresh));
        assert!(heap.list_mut(fresh).is_ok());
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

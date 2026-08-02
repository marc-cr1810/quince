//! Dictionaries: insertion-ordered maps over a restricted set of key values.

use std::collections::HashMap;
use std::rc::Rc;

use crate::runtime::heap::ObjId;
use crate::runtime::value::Value;

/// A value admitted as a dict key.
///
/// The set is deliberately narrower than [`Value`]:
///
/// - Lists, dicts, and functions are excluded. They are either mutable or
///   compared by identity, and a key that can change out from under the map it
///   is filed in is a bug with no good failure mode. Excluding them also means a
///   key can never hold an [`ObjId`], which is why [`Dict::trace`] only has to
///   walk the values.
/// - `nan` is excluded, because it is not equal to itself: a `nan` key could be
///   inserted and then never found again.
///
/// A float with an integral value is stored as an `Int`, so `d[1]` and `d[1.0]`
/// are one entry. The language already says `1 == 1.0`, and a lookup that
/// disagreed with `==` would be indefensible.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Nil,
    Bool(bool),
    Int(i64),
    /// Held as a bit pattern, since `f64` is neither `Eq` nor `Hash`. Only
    /// non-integral, non-`nan` floats reach this variant, and no two distinct
    /// bit patterns there compare equal — the one pair that would, `0.0` and
    /// `-0.0`, has already been folded into `Int`.
    Float(u64),
    Str(Rc<str>),
}

/// Why a value cannot be a key. Rendered by the caller, which has the span —
/// and, for `Unhashable`, the heap needed to name the offending type.
pub enum NotAKey {
    Unhashable,
    Nan,
}

/// `i64::MIN` is a power of two, so both of these convert exactly and the range
/// test below is off by nothing.
const I64_MIN_AS_F64: f64 = i64::MIN as f64;
const I64_MAX_EXCLUSIVE: f64 = -(i64::MIN as f64);

impl Key {
    pub fn from_value(value: &Value) -> Result<Key, NotAKey> {
        let key = match value {
            Value::Nil => Key::Nil,
            Value::Bool(b) => Key::Bool(*b),
            Value::Int(n) => Key::Int(*n),
            Value::Str(s) => Key::Str(Rc::clone(s)),
            Value::Float(n) if n.is_nan() => return Err(NotAKey::Nan),
            // `fract` is `nan` for an infinity, so this also rejects those from
            // the integral path and leaves them as ordinary float keys.
            Value::Float(n)
                if n.fract() == 0.0 && *n >= I64_MIN_AS_F64 && *n < I64_MAX_EXCLUSIVE =>
            {
                Key::Int(*n as i64)
            }
            Value::Float(n) => Key::Float(n.to_bits()),
            _ => return Err(NotAKey::Unhashable),
        };
        Ok(key)
    }

    /// The value this key came from, for iteration and error messages.
    pub fn to_value(&self) -> Value {
        match self {
            Key::Nil => Value::Nil,
            Key::Bool(b) => Value::Bool(*b),
            Key::Int(n) => Value::Int(*n),
            Key::Float(bits) => Value::Float(f64::from_bits(*bits)),
            Key::Str(s) => Value::Str(Rc::clone(s)),
        }
    }
}

/// An insertion-ordered map.
///
/// Order is preserved rather than left to the hasher because it is what a
/// program sees: printing a dict, iterating one, or asking for its keys all
/// expose it, and a test corpus comparing exact output needs it to be
/// deterministic. Python made the same call for the same reason.
///
/// `entries` holds the order and the data; `index` maps a key to its position.
/// Keys are therefore stored twice, which costs little — the only key that owns
/// anything is a string, and those are `Rc`.
#[derive(Clone, Debug, Default)]
pub struct Dict {
    entries: Vec<(Key, Value)>,
    index: HashMap<Key, usize>,
}

impl Dict {
    pub fn new() -> Self {
        Dict::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.index.get(key).map(|&at| &self.entries[at].1)
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.index.contains_key(key)
    }

    /// Inserts or overwrites. An existing key keeps its position, so updating a
    /// value does not reorder the dict.
    pub fn insert(&mut self, key: Key, value: Value) {
        match self.index.get(&key) {
            Some(&at) => self.entries[at].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    /// Removes a key, returning its value.
    ///
    /// Linear in the size of the dict, because holding the order means closing
    /// the gap and renumbering what follows. Removal is rare next to lookup, so
    /// this is the right side of that trade for now.
    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        let at = self.index.remove(key)?;
        let (_, value) = self.entries.remove(at);
        for position in self.index.values_mut() {
            if *position > at {
                *position -= 1;
            }
        }
        Some(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = &(Key, Value)> {
        self.entries.iter()
    }

    pub fn keys(&self) -> impl Iterator<Item = Value> + '_ {
        self.entries.iter().map(|(key, _)| key.to_value())
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.iter().map(|(_, value)| value)
    }

    /// Only the values need tracing — a [`Key`] cannot hold a handle.
    pub fn trace(&self, worklist: &mut Vec<ObjId>) {
        worklist.extend(self.entries.iter().filter_map(|(_, value)| value.handle()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: Value) -> Key {
        Key::from_value(&value).unwrap_or_else(|_| panic!("value should be a valid key"))
    }

    #[test]
    fn integral_floats_and_ints_are_the_same_key() {
        // Follows from `1 == 1.0` being true in the language.
        assert_eq!(key(Value::Int(1)), key(Value::Float(1.0)));
        assert_eq!(key(Value::Float(-0.0)), key(Value::Int(0)));
        assert_ne!(key(Value::Float(1.5)), key(Value::Int(1)));
    }

    #[test]
    fn bools_and_ints_stay_distinct() {
        // `1 == true` is false, so they must not collide either.
        assert_ne!(key(Value::Bool(true)), key(Value::Int(1)));
    }

    #[test]
    fn huge_floats_do_not_saturate_into_an_int_key() {
        // `as i64` saturates rather than wrapping, so 1e300 and 1e301 would both
        // land on `i64::MAX` and become the same key.
        assert_ne!(key(Value::Float(1e300)), key(Value::Float(1e301)));
        assert_ne!(key(Value::Float(1e300)), key(Value::Int(i64::MAX)));
    }

    #[test]
    fn infinities_are_keys_but_nan_is_not() {
        assert_ne!(key(Value::Float(f64::INFINITY)), key(Value::Float(1.0)));
        assert!(matches!(
            Key::from_value(&Value::Float(f64::NAN)),
            Err(NotAKey::Nan)
        ));
    }

    #[test]
    fn mutable_and_identity_values_are_rejected() {
        let list = Value::List(crate::runtime::heap::Heap::new().alloc(crate::runtime::heap::Object::List(vec![])));
        assert!(matches!(Key::from_value(&list), Err(NotAKey::Unhashable)));
    }

    #[test]
    fn keys_round_trip_back_to_values() {
        for value in [
            Value::Nil,
            Value::Bool(true),
            Value::Int(7),
            Value::Float(1.5),
            Value::from("hi"),
        ] {
            assert_eq!(key(value.clone()).to_value(), value);
        }
    }

    #[test]
    fn insertion_order_survives_updates() {
        let mut dict = Dict::new();
        dict.insert(key(Value::from("a")), Value::Int(1));
        dict.insert(key(Value::from("b")), Value::Int(2));
        // Overwriting must not move `a` to the back.
        dict.insert(key(Value::from("a")), Value::Int(3));

        let order: Vec<_> = dict.keys().collect();
        assert_eq!(order, vec![Value::from("a"), Value::from("b")]);
        assert_eq!(dict.get(&key(Value::from("a"))), Some(&Value::Int(3)));
        assert_eq!(dict.len(), 2);
    }

    #[test]
    fn removal_closes_the_gap_and_keeps_later_keys_findable() {
        let mut dict = Dict::new();
        for (name, n) in [("a", 1), ("b", 2), ("c", 3)] {
            dict.insert(key(Value::from(name)), Value::Int(n));
        }

        assert_eq!(dict.remove(&key(Value::from("a"))), Some(Value::Int(1)));
        // `c` was at index 2 and is now at 1; a stale index would find `b`.
        assert_eq!(dict.get(&key(Value::from("c"))), Some(&Value::Int(3)));
        assert_eq!(dict.get(&key(Value::from("b"))), Some(&Value::Int(2)));
        assert_eq!(dict.keys().collect::<Vec<_>>().len(), 2);
        assert_eq!(dict.remove(&key(Value::from("a"))), None);
    }

    #[test]
    fn tracing_reaches_values_held_in_the_dict() {
        let mut heap = crate::runtime::heap::Heap::new();
        let id = heap.alloc(crate::runtime::heap::Object::List(vec![]));
        let mut dict = Dict::new();
        dict.insert(key(Value::from("k")), Value::List(id));

        let mut worklist = Vec::new();
        dict.trace(&mut worklist);
        assert_eq!(worklist, vec![id]);
    }
}

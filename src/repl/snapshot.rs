//! What the REPL knows, taken from the values it is holding.
//!
//! The editor infers; the REPL does not have to. A bound value has an exact type,
//! and its class has an exact method table, so a snapshot of the live interpreter
//! answers completion and hover without a pass over the source.

use std::collections::HashMap;

use quince::interp::Interp;
use quince::runtime::class;
use quince::runtime::value::Value;
use quince::sema::symbols::{Kind, Symbol};
use quince::sema::types::{Type};
use crate::cursor;

/// What the interpreter knows right now, as symbols.
///
/// Rebuilt after every entry, from the live objects. That is the REPL's whole
/// advantage over the editor and it was being thrown away: a bound name has a
/// value, and a value has a class, so there is nothing here to infer. What the
/// receiver is, is a fact.
///
/// This replaces three hand-maintained maps — globals as `(String, String)`,
/// methods as `HashMap<String, Vec<String>>`, fields as another — which between
/// them could not say what a member returned, missed every `extend`ed method,
/// and fell back to offering every method of every type when the receiver was
/// not a plain global.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Every global, with the class of the value actually bound to it.
    pub(crate) globals: Vec<Symbol>,
    /// What a dot reaches on a value of each class.
    members: HashMap<String, Vec<Symbol>>,
}

impl Snapshot {
    /// Reads the interpreter's globals and the classes they reach.
    pub(crate) fn of(interp: &Interp) -> Snapshot {
        let mut snapshot = Snapshot::default();
        for (name, value) in interp.get_globals() {
            let class = value.type_name(&interp.heap).to_string();
            // A native keeps what its table records, so `from math import floor`
            // leaves `floor` with the parameters and documentation `math.floor`
            // has. Anything else is named by the class of the value bound.
            let mut symbol = match &value {
                Value::Native(native) => {
                    let mut symbol = quince::sema::symbols::symbol_of_native(native, Kind::Function);
                    symbol.name = name.clone();
                    symbol
                }
                _ => Symbol::new(&name, kind_of(&value), Type::class(&class)),
            };
            // Calling a class makes one of it, which is what `Point(` needs to
            // know to offer the parameters of `Point`'s `init`.
            match &value {
                // Calling a class makes one of it, which is what `Point(`
                // needs in order to offer `Point`'s `init` parameters.
                Value::Class(id) => {
                    symbol.returns = Type::class(interp.heap.class(*id).name.clone())
                }
                // A module is not a class, so it is keyed apart from one — a
                // program may perfectly well declare `class math`.
                Value::Module(id) => {
                    symbol.ty =
                        Type::class(format!("module {}", interp.heap.globals(*id).name().unwrap_or_default()))
                }
                _ => {}
            }
            snapshot.globals.push(symbol);
            snapshot.learn(interp, &value);
        }
        // The builtin types, so `"abc".` and `[1, 2].` are answerable before a
        // program has bound anything of that class.
        for builtin in class::BUILTINS {
            let id = interp.heap.builtin_class(*builtin);
            snapshot.learn_class(interp, builtin.name(), id);
        }
        snapshot
    }

    /// Records what a dot reaches on `value`, and on the class it names.
    pub(crate) fn learn(&mut self, interp: &Interp, value: &Value) {
        let class = value.type_name(&interp.heap).to_string();
        match value {
            // A class object: its instances are what anyone asks about.
            Value::Class(id) => {
                let named = interp.heap.class(*id).name.clone();
                self.learn_class(interp, &named, *id);
            }
            // A module's names come out of the scope object it is, which is the
            // same object `import` produced — there is no second list.
            Value::Module(id) => {
                let named = format!("module {}", interp.heap.globals(*id).name().unwrap_or_default());
                // Lazily, because the walk is only worth doing the first time a
                // module is seen — the same module reached twice has the same
                // members both times.
                self.members.entry(named).or_insert_with(|| {
                    interp
                        .heap
                        .globals(*id)
                        .iter()
                        .map(|(member, held)| match held {
                            Value::Native(native) => {
                                let mut symbol =
                                    quince::sema::symbols::symbol_of_native(native, Kind::Function);
                                symbol.name = member.to_string();
                                symbol
                            }
                            _ => Symbol::new(
                                member,
                                Kind::Variable,
                                Type::class(held.type_name(&interp.heap)),
                            ),
                        })
                        .collect()
                });
            }
            Value::Instance(id) => {
                let instance = interp.heap.instance(*id);
                self.learn_class(interp, &class, instance.class);
                // Fields exist because something assigned them, so they are read
                // off the instance rather than guessed from the class body.
                let fields: Vec<Symbol> = instance
                    .fields
                    .iter()
                    .filter_map(|(key, held)| match key.to_value() {
                        Value::Str(name) => Some(Symbol::new(
                            name.to_string(),
                            Kind::Field,
                            Type::class(held.type_name(&interp.heap)),
                        )),
                        _ => None,
                    })
                    .collect();
                let known = self.members.entry(class).or_default();
                for field in fields {
                    if !known.iter().any(|seen| seen.name == field.name) {
                        known.push(field);
                    }
                }
            }
            _ => {
                let id = value.class(&interp.heap);
                self.learn_class(interp, &class, id);
            }
        }
    }

    /// Records the methods callable on a value of the class `id`.
    ///
    /// Through `Interp::methods_of`, which makes the same two walks dispatch
    /// makes — so an `extend` block's methods are offered, which they never
    /// were before.
    pub(crate) fn learn_class(&mut self, interp: &Interp, name: &str, id: quince::runtime::heap::ObjId) {
        if self.members.contains_key(name) {
            return;
        }
        let members = interp
            .methods_of(id)
            .into_iter()
            .map(|(method, value)| match &value {
                Value::Native(native) => {
                    let mut symbol = quince::sema::symbols::symbol_of_native(native, Kind::Method);
                    symbol.name = method;
                    symbol
                }
                Value::Function(handle) => {
                    let decl = &interp.heap.function(*handle).decl;
                    let mut symbol = Symbol::new(&method, Kind::Method, Type::class("function"));
                    symbol.doc = decl.doc.clone();
                    symbol.params = decl
                        .params
                        .iter()
                        .filter(|param| !param.receiver)
                        .map(|param| param.name.clone())
                        .collect();
                    symbol
                }
                _ => Symbol::new(&method, Kind::Method, Type::class("function")),
            })
            .collect();
        self.members.insert(name.to_string(), members);
    }

    /// What the text before a dot evaluates to.
    ///
    /// A dotted path resolved a segment at a time against what is bound, and
    /// failing that a literal read by the lexer. The same two questions the
    /// editor asks, answered from values instead of from a tree.
    pub(crate) fn type_of(&self, before: &str) -> Type {
        let Some(path) = cursor::path_ending_at(before) else {
            return cursor::trailing_literal_type(before);
        };
        let mut segments = path.split('.');
        let Some(first) = segments.next() else {
            return Type::Unknown;
        };
        let call = first.strip_suffix("()");
        let mut ty = match self
            .globals
            .iter()
            .find(|symbol| symbol.name == call.unwrap_or(first))
        {
            // Calling a name makes what it returns; naming it holds it. A class
            // named and not called is the exception: `Dog.bark` reaches the
            // method, so a dot on the class object finds what its instances
            // have — which the language allows and this has to follow.
            Some(symbol) if call.is_some() || symbol.kind == Kind::Class => {
                symbol.returns.clone()
            }
            Some(symbol) => symbol.ty.clone(),
            None => return cursor::trailing_literal_type(before),
        };
        for segment in segments {
            let Some(class) = ty.class_name() else {
                return Type::Unknown;
            };
            let call = segment.strip_suffix("()");
            let name = call.unwrap_or(segment);
            let found = self
                .members
                .get(class)
                .and_then(|members| members.iter().find(|symbol| symbol.name == name));
            ty = match (found, call) {
                (Some(symbol), Some(_)) => symbol.returns.clone(),
                (Some(symbol), None) => symbol.ty.clone(),
                (None, _) => return Type::Unknown,
            };
        }
        ty
    }

    /// Everything a dot after `before` reaches.
    ///
    /// A class object gets methods and no fields. `Dog.bark` finds the method
    /// and `Dog.breed` finds nothing — a field exists because an instance
    /// assigned it, and the class never did.
    pub(crate) fn members_after(&self, before: &str) -> Vec<Symbol> {
        let on_class_object = cursor::path_ending_at(before)
            .filter(|path| !path.contains('.') && !path.ends_with("()"))
            .and_then(|name| self.globals.iter().find(|symbol| symbol.name == name))
            .is_some_and(|symbol| symbol.kind == Kind::Class);

        match self.type_of(before).class_name() {
            Some(class) => self
                .members
                .get(class)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|symbol| !(on_class_object && symbol.kind == Kind::Field))
                .collect(),
            None => Vec::new(),
        }
    }
}

/// What a value is, for a completion list that has to draw it.
pub(crate) fn kind_of(value: &Value) -> Kind {
    match value {
        Value::Class(_) => Kind::Class,
        Value::Function(_) | Value::Overload(_) | Value::Native(_) | Value::BoundMethod(_) => {
            Kind::Function
        }
        Value::Module(_) => Kind::Module,
        _ => Kind::Variable,
    }
}

/// What may be written at an `import` position.
///
/// Off `stdlib::MODULES` both times, which is the list `import` itself reads —
/// so a module or a member added to the library is offered without this being
/// touched.
pub(crate) fn import_candidates(site: &cursor::ImportSite) -> Vec<String> {
    match site {
        cursor::ImportSite::Module => quince::builtins::stdlib::MODULES
            .iter()
            .map(|module| module.name.to_string())
            .collect(),
        cursor::ImportSite::Member(module) => quince::sema::symbols::module_symbols(module)
            .into_iter()
            .map(|symbol| symbol.name)
            .collect(),
    }
}

/// The text before the dot the cursor sits after, if it does.
pub(crate) fn before_dot(line: &str, start: usize) -> Option<&str> {
    if start == 0 || line.as_bytes().get(start - 1) != Some(&b'.') {
        return None;
    }
    Some(line[..start - 1].trim_end())
}

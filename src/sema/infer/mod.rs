//! What class an expression's value belongs to, where that is decidable.
//!
//! A pass beside the resolver rather than inside it. The resolver answers
//! *where a name lives*; this answers *what it holds*, and the two questions
//! have different failure modes: a name that does not resolve is a program that
//! cannot run, while a type that cannot be worked out is the ordinary condition
//! of a dynamically typed program. So nothing here returns a `Result` and
//! nothing here is an error. [`Type::Unknown`] is an answer.
//!
//! That is the whole design constraint. Most of a Quince program is unknown —
//! a parameter is whatever the caller passed, a list holds whatever was put in
//! it, a field is whatever the last assignment left there — and a pass that
//! guesses to avoid saying so is the thing it exists to replace. The heuristics
//! in the language server are that guess: a receiver's class decided by whether
//! its name starts with a capital letter, a function found by looking for `fn `
//! at the start of a line. They stay, because a document mid-keystroke usually
//! does not parse and an editor that goes blank between two valid states is
//! worse than one that guesses. They are a floor under this, not a rival to it.
//!
//! What is decidable, and is decided here:
//!
//! - a literal, and a collection literal
//! - `self`, and `super`, inside a method
//! - a constructor call, `Point(1, 2)`, and a conversion, `int("42")`
//! - a call to a function or method whose returns all agree
//! - a field, from the assignments the declaring class makes to it
//! - an operator whose operands are builtins, which is where the language
//!   itself decides the answer — `1 / 2` is a float, `a == b` is a bool
//! - an imported stdlib module, and what its members hold
//! - a call into the library, from what the native says it returns —
//!   `"a,b".split(",")` is a list, and `math.floor(2.5)` is an int
//!
//! Everything else is [`Type::Unknown`], including every case where two
//! answers disagree: a variable assigned an int here and a string there, a
//! function returning one class down one branch and another down the other.
//! Disagreement is not a tie to be broken, it is the absence of an answer.
mod walk;

#[cfg(test)]
mod tests;

use walk::Infer;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::runtime::class::BUILTINS;
use crate::sema::symbols::{Kind, Symbol, module_native, push_once, symbol_for, symbol_of_native};
use crate::sema::types::{Type, builtin_ancestor, builtin_constructor, module_member, returned_by};
use crate::syntax::ast::{FnDecl, Stmt, Visibility};
use crate::syntax::token::Span;


/// What one class declaration said, before anything was worked out from it.
#[derive(Clone, Debug)]
pub struct ClassInfo {
    /// The name after `extends`, if there was one. Kept as a name rather than a
    /// resolved handle because a parent may be declared further down the file,
    /// or not at all.
    pub parent: Option<String>,
    pub methods: HashMap<String, Rc<FnDecl>>,
    pub all_methods: HashMap<String, Vec<Rc<FnDecl>>>,
    /// The names [`overloading`] left out of `methods` because the class
    /// declared them more than once.
    ///
    /// `methods` answers "which declaration does this name mean", and for an
    /// overloaded name there is no answer. This answers the weaker question —
    /// "is this name declared at all" — which is the one an operator check
    /// wants, and which an absent `methods` entry otherwise reads as "no".
    pub overloaded: HashSet<String>,
    /// The fields the body declared, by name, and how far each reaches.
    ///
    /// Only the declared ones. A field an `op init` assigned into existence is
    /// found by [`Pass::fields_of`] walking the methods, and carries no
    /// visibility because nothing wrote one.
    pub fields: HashMap<String, Visibility>,
    pub openness: crate::syntax::ast::Openness,
    /// The whole declaration, so an editor can ask which class an offset is
    /// inside of — which is what a visibility-aware completion needs and no
    /// other question here does.
    pub span: Span,
    /// Whether this class was imported from another module file rather than declared locally.
    pub is_imported: bool,
}

/// One name in scope, and what is known about it.
pub(crate) struct Binding {
    pub(crate) symbol: Symbol,
    /// The block the name lives in. Bindings are found by asking which of them
    /// covers an offset, which is how a local in one function is kept from
    /// answering for the same name in another.
    pub(crate) scope: Span,
    /// The offset from which the name means this. A `let` starts at its own
    /// statement; a `fn` or a `class` covers its whole scope, because the
    /// resolver lets a function call one declared below it.
    pub(crate) from: u32,
}

/// The scope every top-level name lives in.
///
/// A real span would have to be the whole file's, and the parser hands out no
/// such node — the top level is a `Vec<Stmt>`, not a `Block`. Using the widest
/// span there is makes it the outermost scope by construction, which is the one
/// property the lookup needs of it.
pub(crate) const FILE: Span = Span {
    start: 0,
    end: u32::MAX,
};

/// The innermost binding of `name` that covers `offset`.
///
/// Innermost is the narrowest scope, and within one scope the latest binding
/// wins — which is shadowing, spelled as a search rather than as a stack,
/// because the walk that filled the list is over by the time anyone asks.
///
/// A binding whose scope does not cover the offset is not consulted at all,
/// even when nothing else answers. An editor asking about a stale AST is the
/// reason that matters: the honest answer there is `Unknown`, and the caller
/// has a text heuristic to fall back to. Reaching for a local belonging to some
/// other function would look like knowledge.
pub(crate) fn lookup(bindings: &[Binding], name: &str, offset: u32) -> Option<usize> {
    bindings
        .iter()
        .enumerate()
        .filter(|(_, binding)| {
            binding.symbol.name == name
                && binding.from <= offset
                && binding.scope.start <= offset
                && offset <= binding.scope.end
        })
        .min_by_key(|(_, binding)| {
            let width = binding.scope.end - binding.scope.start;
            (width, u32::MAX - binding.from)
        })
        .map(|(index, _)| index)
}

/// What a custom module exports to importers.
#[derive(Clone, Debug)]
pub struct ModuleInfo {
    pub name: String,
    pub symbols: Vec<Symbol>,
    pub classes: HashMap<String, ClassInfo>,
    pub functions: HashMap<String, Type>,
    pub fn_decls: HashMap<String, Rc<FnDecl>>,
    pub all_fn_decls: HashMap<String, Vec<Rc<FnDecl>>>,
    pub methods: HashMap<(String, String), Type>,
    pub fields: HashMap<String, HashMap<String, Type>>,
}

/// A hook allowing module imports to be resolved against workspace documents or the filesystem.
pub trait ModuleResolver {
    fn resolve_module(&self, name: &str) -> Option<Vec<Stmt>>;
}

pub struct NoResolver;
impl ModuleResolver for NoResolver {
    fn resolve_module(&self, _name: &str) -> Option<Vec<Stmt>> {
        None
    }
}

impl<F> ModuleResolver for F
where
    F: Fn(&str) -> Option<Vec<Stmt>>,
{
    fn resolve_module(&self, name: &str) -> Option<Vec<Stmt>> {
        self(name)
    }
}

impl ModuleResolver for &dyn ModuleResolver {
    fn resolve_module(&self, name: &str) -> Option<Vec<Stmt>> {
        (**self).resolve_module(name)
    }
}

/// Everything the pass worked out, ready to be asked questions.
pub struct Types {
    /// Keyed by exact AST expression Span.
    pub(crate) exprs: HashMap<Span, Type>,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) classes: HashMap<String, ClassInfo>,
    pub(crate) fields: HashMap<String, HashMap<String, Type>>,
    /// What each top-level or nested `fn` returns, by name. A name shadowed by
    /// a second declaration holds the last one, which is what the evaluator
    /// would find too.
    pub(crate) functions: HashMap<String, Type>,
    /// The declaration of each `fn`, by name.
    ///
    /// Kept beside the return type rather than instead of it: the return is
    /// what an expression's type comes from and is answered constantly, while
    /// this is read only when something wants the *parameters* — which nothing
    /// did until the static check started looking at calls.
    pub(crate) fn_decls: HashMap<String, Rc<FnDecl>>,
    pub(crate) all_fn_decls: HashMap<String, Vec<Rc<FnDecl>>>,
    /// Which builtin each imported name stands for — `from math import floor`
    /// makes `floor` a name for one, and a call through it should be checked
    /// exactly as `math.floor` is.
    pub(crate) natives: HashMap<String, &'static crate::runtime::value::Native>,
    /// What each method returns, by the class that declares it and its name.
    pub(crate) methods: HashMap<(String, String), Type>,
    /// Custom non-stdlib modules imported by this program.
    pub(crate) modules: HashMap<String, ModuleInfo>,
}

impl Types {
    /// What the expression at `span` evaluates to.
    pub fn of_expr(&self, span: Span) -> Type {
        self.exprs.get(&span).cloned().unwrap_or_default()
    }

    /// What the smallest expression containing `offset` evaluates to.
    pub fn of_offset(&self, offset: u32) -> Type {
        self.exprs
            .iter()
            .filter(|(span, _)| span.start <= offset && offset <= span.end)
            .min_by_key(|(span, _)| span.end - span.start)
            .map(|(_, ty)| ty.clone())
            .unwrap_or_default()
    }

    /// What `name` holds, as seen from `offset`. See [`lookup`].
    pub fn of_name(&self, name: &str, offset: u32) -> Type {
        lookup(&self.bindings, name, offset)
            .map(|index| self.bindings[index].symbol.ty.clone())
            .unwrap_or_default()
    }

    /// What `class` stores under `field`, searching its parents.
    pub fn of_field(&self, class: &str, field: &str) -> Type {
        self.walk_up(class, |types, current| {
            types.fields.get(current)?.get(field).cloned()
        })
        .unwrap_or_default()
    }

    /// What calling `class`'s method called `name` produces.
    ///
    /// The program's own methods first and the builtin tables after, which is
    /// the order dispatch uses: a class extending `list` that writes its own
    /// `sort` is answered by the one it wrote.
    pub fn of_method(&self, class: &str, name: &str) -> Type {
        self.walk_up(class, |types, current| {
            types
                .methods
                .get(&(current.to_string(), name.to_string()))
                .cloned()
        })
        .or_else(|| builtin_ancestor(&self.classes, class, name).map(returned_by))
        .unwrap_or_default()
    }

    /// What calling the function called `name` produces.
    pub fn of_function(&self, name: &str) -> Type {
        self.functions.get(name).cloned().unwrap_or_default()
    }

    /// Whether `class` declares — or inherits — a method called `name`.
    pub fn has_method(&self, class: &str, name: &str) -> bool {
        self.walk_up(class, |types, current| {
            types.classes.get(current)?.methods.contains_key(name).then_some(())
        })
        .is_some()
    }

    /// Whether `path` names a class object rather than an instance of one.
    ///
    /// `Dog` does and `Dog()` does not, and the difference decides whether a
    /// dot reaches fields: a field exists because an instance assigned it, and
    /// the class never did.
    pub fn names_a_class(&self, path: &str, offset: u32) -> bool {
        !path.contains('.')
            && !path.ends_with("()")
            && self
                .symbol(path, offset)
                .is_some_and(|symbol| symbol.kind == Kind::Class)
    }

    /// The builtin `name` stands for, if an import made it one.
    pub fn native(&self, name: &str) -> Option<&'static crate::runtime::value::Native> {
        self.natives.get(name).copied()
    }

    /// The `fn` declared under `name`, if the program declared one.
    pub fn function(&self, name: &str) -> Option<&Rc<FnDecl>> {
        self.fn_decls.get(name)
    }

    /// All `fn` declarations under `name`, including overloads.
    pub fn functions_named(&self, name: &str) -> Vec<Rc<FnDecl>> {
        if let Some(decls) = self.all_fn_decls.get(name) {
            if !decls.is_empty() {
                return decls.clone();
            }
        }
        if let Some(decl) = self.fn_decls.get(name) {
            return vec![Rc::clone(decl)];
        }
        for module in self.modules.values() {
            if let Some(decls) = module.all_fn_decls.get(name) {
                if !decls.is_empty() {
                    return decls.clone();
                }
            }
            if let Some(decl) = module.fn_decls.get(name) {
                return vec![Rc::clone(decl)];
            }
        }
        Vec::new()
    }

    /// Looks up class info across current document classes and imported modules.
    pub fn class_info(&self, class: &str) -> Option<&ClassInfo> {
        if let Some(info) = self.classes.get(class) {
            return Some(info);
        }
        for module in self.modules.values() {
            if let Some(info) = module.classes.get(class) {
                return Some(info);
            }
        }
        None
    }

    /// The method `name` on `class`, searching its ancestors.
    pub fn method_of(&self, class: &str, name: &str) -> Option<&Rc<FnDecl>> {
        let mut current = class;
        let mut seen = 0;
        loop {
            let info = self.class_info(current)?;
            if let Some(decl) = info.methods.get(name) {
                return Some(decl);
            }
            // The hierarchy may be cyclic — `class A extends B` and the reverse
            // is refused at run time, not here — so the walk is bounded.
            seen += 1;
            if seen > 64 {
                return None;
            }
            current = info.parent.as_deref()?;
        }
    }

    /// All method declarations under `name` on `class`, searching its ancestors.
    pub fn all_methods_of(&self, class: &str, name: &str) -> Vec<Rc<FnDecl>> {
        let mut results = Vec::new();
        let mut current = class;
        let mut seen = 0;
        loop {
            let Some(info) = self.class_info(current) else {
                break;
            };
            if let Some(decls) = info.all_methods.get(name) {
                for d in decls {
                    if !results.iter().any(|existing: &Rc<FnDecl>| Rc::ptr_eq(existing, d)) {
                        results.push(Rc::clone(d));
                    }
                }
            } else if let Some(decl) = info.methods.get(name) {
                if !results.iter().any(|existing: &Rc<FnDecl>| Rc::ptr_eq(existing, decl)) {
                    results.push(Rc::clone(decl));
                }
            }
            seen += 1;
            if seen > 64 {
                break;
            }
            match info.parent.as_deref() {
                Some(parent) => current = parent,
                None => break,
            }
        }
        results
    }

    /// Whether `class` or an ancestor declares `name` at all, overloads
    /// included.
    ///
    /// [`Self::method_of`] cannot answer this: it returns the one declaration a
    /// name means, and deliberately has none for an overloaded name. A caller
    /// asking "is this implemented" rather than "what does it do" — an operator
    /// check, say — must ask here, or it reads two `op sub` declarations as
    /// zero.
    pub fn declares_method(&self, class: &str, name: &str) -> bool {
        let mut current = class;
        let mut seen = 0;
        loop {
            let Some(info) = self.class_info(current) else {
                return false;
            };
            if info.methods.contains_key(name) || info.overloaded.contains(name) {
                return true;
            }
            seen += 1;
            if seen > 64 {
                return false;
            }
            match info.parent.as_deref() {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    /// How far `name` reaches on `class`, if the program declared it.
    pub fn reach_of(&self, class: &str, name: &str) -> Option<(Visibility, String)> {
        let mut current = class.to_string();
        let mut seen = 0;
        loop {
            let info = self.class_info(&current)?;
            if let Some(visibility) = info.fields.get(name) {
                return Some((*visibility, current));
            }
            if let Some(decl) = info.methods.get(name) {
                return Some((decl.visibility, current));
            }
            seen += 1;
            if seen > 64 {
                return None;
            }
            current = info.parent.clone()?;
        }
    }

    /// Every class the program declared, for a list an editor offers.
    ///
    /// Includes the entries an `extend` block made, which is right: `extend
    /// list` does not declare `list`, but a program that mentions it means the
    /// name, and the builtins are offered from their own table anyway.
    pub fn class_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.classes.keys().cloned().collect();
        for module in self.modules.values() {
            names.extend(module.classes.keys().cloned());
        }
        names.sort();
        names.dedup();
        names
    }

    /// Whether the program declared a class by this name.
    pub fn declares(&self, class: &str) -> bool {
        self.class_info(class).is_some()
    }

    /// Every name visible at `offset`, innermost first.
    ///
    /// What a completion list outside a dot is made of, together with the
    /// keywords and the globals. Shadowed names appear once: the one that
    /// would win is the one offered, because the other cannot be reached from
    /// here by writing its name.
    pub fn in_scope(&self, offset: u32) -> Vec<Symbol> {
        let mut found: Vec<Symbol> = Vec::new();
        for binding in &self.bindings {
            if binding.from > offset
                || binding.scope.start > offset
                || offset > binding.scope.end
                || found.iter().any(|seen| seen.name == binding.symbol.name)
            {
                continue;
            }
            // The one the lookup would answer with, rather than this one — two
            // bindings of a name in one scope are a rebinding, and the live one
            // is the later.
            let winner = lookup(&self.bindings, &binding.symbol.name, offset)
                .expect("this binding covers the offset");
            found.push(self.bindings[winner].symbol.clone());
        }
        found
    }

    /// One name, as seen from `offset`.
    pub fn symbol(&self, name: &str, offset: u32) -> Option<Symbol> {
        lookup(&self.bindings, name, offset).map(|index| self.bindings[index].symbol.clone())
    }

    /// Everything reachable through a dot on an instance of `class`.
    ///
    /// Its own methods and fields, then its ancestors', then the builtin table
    /// underneath — the order dispatch uses, so a name declared twice is
    /// offered as whichever one would actually run.
    pub fn members_of(&self, class: &str) -> Vec<Symbol> {
        let mut found: Vec<Symbol> = Vec::new();
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                break;
            }
            if let Some(info) = self.class_info(&current) {
                for (name, decl) in &info.methods {
                    let ty = self
                        .methods
                        .get(&(current.clone(), name.clone()))
                        .or_else(|| {
                            for module in self.modules.values() {
                                if let Some(t) = module.methods.get(&(current.clone(), name.clone())) {
                                    return Some(t);
                                }
                            }
                            None
                        })
                        .cloned()
                        .unwrap_or_default();
                    push_once(&mut found, symbol_for(decl, Kind::Method, ty));
                }
            }
            let fields_opt = self.fields.get(&current).or_else(|| {
                for module in self.modules.values() {
                    if let Some(f) = module.fields.get(&current) {
                        return Some(f);
                    }
                }
                None
            });
            if let Some(fields) = fields_opt {
                for (name, ty) in fields {
                    // A declared field carries the word it was written with; one
                    // an `init` invented carries none, and is public.
                    let visibility = self
                        .class_info(&current)
                        .and_then(|info| info.fields.get(name))
                        .copied()
                        .unwrap_or_default();
                    push_once(
                        &mut found,
                        Symbol::new(name, Kind::Field, ty.clone()).reaching(visibility),
                    );
                }
            }
            // The builtin this link *is*, if it is one: `extend list` puts a
            // class entry under `list`, and the list's own methods sit under it
            // rather than in that entry.
            if let Some(builtin) = BUILTINS.iter().find(|builtin| builtin.name() == current) {
                for (name, native) in builtin.seed().methods {
                    let mut symbol = symbol_of_native(native, Kind::Method);
                    symbol.name = (*name).to_string();
                    push_once(&mut found, symbol);
                }
            }
            match self.class_info(&current).and_then(|info| info.parent.clone()) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        found
    }

    /// The class whose declaration encloses `offset`, if any.
    ///
    /// The innermost, so a class nested in nothing is still the answer and a
    /// tie cannot happen — class declarations do not overlap. `None` at the top
    /// level, which is what makes top-level code an outsider to every class.
    pub fn class_at(&self, offset: u32) -> Option<&str> {
        self.classes
            .iter()
            .filter(|(_, info)| !info.is_imported && info.span.start <= offset && offset <= info.span.end)
            .min_by_key(|(_, info)| info.span.end - info.span.start)
            .map(|(name, _)| name.as_str())
    }

    /// Whether code inside `from` may reach a member of `of` declared with
    /// `visibility`.
    ///
    /// The editor's half of the rule the evaluator enforces, and deliberately
    /// the *less* precise half: `members_of` flattens a chain, so a `private`
    /// member of an ancestor is offered to the subclass that cannot actually
    /// reach it. Erring towards offering is the right way round for a
    /// completion list — the language still refuses it, with a message that
    /// says why, and an editor that hides a name the reader can see in the
    /// source is the more confusing failure.
    pub fn may_offer(&self, visibility: Visibility, of: &str, from: Option<&str>) -> bool {
        if !visibility.closes_outside() {
            return true;
        }
        let Some(from) = from else {
            return false;
        };
        if from == of {
            return true;
        }
        if visibility.closes_subclass() {
            return false;
        }
        // `protected`: any class on `from`'s chain reaching `of` may see it.
        let mut current = Some(from);
        let mut seen = HashSet::new();
        while let Some(name) = current {
            if !seen.insert(name.to_string()) {
                return false;
            }
            if name == of {
                return true;
            }
            current = self.parent_of(name);
        }
        false
    }

    /// Whether the program declared a class by this name.
    ///
    /// Told apart from "has no parent", which `parent_of` cannot distinguish it
    /// from — and the difference decides whether a name the pass does not
    /// recognise is a mistake or simply something it was not told about.
    pub fn declares_class(&self, name: &str) -> bool {
        self.class_info(name).is_some()
    }

    /// The class `class` extends, if the program said so.
    pub fn parent_of(&self, class: &str) -> Option<&str> {
        self.class_info(class)?.parent.as_deref()
    }

    /// Walks a class and then its ancestors, stopping at the first answer.
    ///
    /// The visited set is not paranoia: `class A extends B` and `class B
    /// extends A` is refused at run time, not here, so a cycle is a shape this
    /// pass can be handed and must not hang on.
    fn walk_up<T>(&self, class: &str, mut ask: impl FnMut(&Self, &str) -> Option<T>) -> Option<T> {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            if let Some(answer) = ask(self, &current) {
                return Some(answer);
            }
            current = self.class_info(&current)?.parent.clone()?;
        }
    }

    /// Every symbol a module (custom or stdlib) offers after its dot.
    pub fn module_symbols(&self, module: &str) -> Vec<Symbol> {
        if let Some(info) = self.modules.get(module) {
            info.symbols.clone()
        } else {
            crate::sema::symbols::module_symbols(module)
        }
    }

    /// Access the resolved custom modules.
    pub fn modules(&self) -> &HashMap<String, ModuleInfo> {
        &self.modules
    }

    /// What a dotted path evaluates to, as seen from `offset`.
    ///
    /// `p.origin.x`, `math.pi`, `b.twin().n` — the form an editor has in hand
    /// when someone types a `.`, which is a string rather than an expression
    /// because the thing being completed has not been written yet and so cannot
    /// have parsed.
    ///
    /// A segment written with parentheses is a call and is answered by what the
    /// call produces; one without is a field or a member. That distinction is
    /// the whole reason the caller has to keep the parentheses rather than
    /// trimming them off: `Point` is a class and `Point()` is a `Point`, and a
    /// path cannot tell them apart once the parentheses are gone.
    pub fn of_path(&self, path: &str, offset: u32) -> Type {
        let mut segments = path.split('.');
        let Some(first) = segments.next() else {
            return Type::Unknown;
        };

        let mut ty = match called(first) {
            Some(name) => {
                if self.declares(name) {
                    Type::class(name)
                } else if let Some(builtin) = builtin_constructor(name) {
                    Type::class(builtin)
                } else {
                    self.of_function(name)
                }
            }
            // A class named and not called is a class object — and `Dog.bark`
            // reaches the method, so a dot on one finds what its instances
            // have. That is the language's answer, checked rather than assumed:
            // `print(Dog.bark)` writes `<fn bark>`.
            None => match self.symbol(first, offset) {
                Some(symbol) if symbol.kind == Kind::Class => symbol.returns,
                _ => self.of_name(first, offset),
            },
        };

        for segment in segments {
            let Type::Class(class) = &ty else {
                ty = match (&ty, called(segment)) {
                    (Type::Module(module), None) => {
                        if let Some(info) = self.modules.get(module.as_ref()) {
                            info.symbols
                                .iter()
                                .find(|s| s.name == segment)
                                .map_or(Type::Unknown, |s| s.ty.clone())
                        } else {
                            module_member(module, segment)
                        }
                    }
                    (Type::Module(module), Some(name)) => {
                        if let Some(info) = self.modules.get(module.as_ref()) {
                            if info.classes.contains_key(name) {
                                Type::class(name)
                            } else {
                                info.functions.get(name).cloned().unwrap_or(Type::Unknown)
                            }
                        } else {
                            module_native(module, name).map_or(Type::Unknown, returned_by)
                        }
                    }
                    _ => Type::Unknown,
                };
                if !ty.is_known() {
                    return Type::Unknown;
                }
                continue;
            };
            let class = class.name.clone();
            ty = match called(segment) {
                Some(name) => self.of_method(&class, name),
                None => self.member(&class, segment),
            };
        }
        ty
    }

    /// A name read off an instance without calling it.
    ///
    /// A method named but not called is the method — a `function` — rather than
    /// what calling it would produce. That is not a nicety: `xs.sort` and
    /// `xs.sort()` are different values, and an editor completing the first as
    /// though it were the second would offer a list's methods on a function.
    fn member(&self, class: &str, name: &str) -> Type {
        if self.has_method(class, name) || builtin_ancestor(&self.classes, class, name).is_some() {
            return Type::class("function");
        }
        self.of_field(class, name)
    }
}

/// The name inside a path segment written as a call, or `None` when it was not.
pub(crate) fn called(segment: &str) -> Option<&str> {
    segment.strip_suffix("()")
}

/// Works out what it can about a program.
pub fn infer(program: &[Stmt]) -> Types {
    infer_with_resolver(program, &NoResolver)
}

/// Works out what it can about a program, resolving custom imported modules.
pub fn infer_with_resolver(program: &[Stmt], resolver: &dyn ModuleResolver) -> Types {
    let mut pass = Infer::new(resolver);
    pass.declare(program);
    pass.fields();
    pass.scopes.push(FILE);
    pass.stmts(program);
    Types {
        exprs: pass.exprs,
        bindings: pass.bindings,
        classes: pass.classes,
        fields: pass.fields,
        functions: pass.function_returns,
        fn_decls: pass.fn_decls,
        all_fn_decls: pass.all_fn_decls,
        natives: pass.natives,
        methods: pass.method_returns,
        modules: pass.modules,
    }
}

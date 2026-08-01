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

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::{
    BinaryOp, Block, Expr, ExprKind, FnDecl, ImportNames, SELF, Stmt, StmtKind, UnaryOp,
};
use crate::class::BUILTINS;
use crate::doc::Doc;
use crate::stdlib;
use crate::token::Span;
use crate::value::{Native, Value};

/// What the pass worked out about a value.
///
/// Three states, not two. A module is not a class — `math` has members and no
/// methods, and answering `Class("module")` for it would be true and useless,
/// since what a caller wants to know is *which* module so it can list what is
/// in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Type {
    /// Nothing was decidable, or two decidable answers disagreed.
    #[default]
    Unknown,
    /// An instance of the named class — a builtin type such as `int`, or one
    /// the program declared.
    Class(String),
    /// A stdlib module, by name. A module loaded from a file is `Unknown`:
    /// nothing here reads the filesystem, which is what makes cross-file
    /// inference a later piece of work rather than a flag on this one.
    Module(String),
}

impl Type {
    /// An instance of the named class.
    pub fn class(name: impl Into<String>) -> Type {
        Type::Class(name.into())
    }

    /// The class this is an instance of, or `None` for anything else.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            Type::Class(name) => Some(name),
            _ => None,
        }
    }

    /// The module this names, or `None` for anything else.
    pub fn module_name(&self) -> Option<&str> {
        match self {
            Type::Module(name) => Some(name),
            _ => None,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Type::Unknown)
    }

    /// The one answer both of these give, or `Unknown` when they give two.
    ///
    /// Deliberately not a lattice with a `nil`-shaped bottom or a union in the
    /// middle. A variable holding an int on one path and a string on the other
    /// has no type this pass could report that would be worth reporting, and
    /// the v0.7 annotations are where a program gets to *say* it meant both.
    fn join(self, other: Type) -> Type {
        if self == other { self } else { Type::Unknown }
    }
}

/// What one class declaration said, before anything was worked out from it.
#[derive(Default)]
struct ClassInfo {
    /// The name after `extends`, if there was one. Kept as a name rather than a
    /// resolved handle because a parent may be declared further down the file,
    /// or not at all.
    parent: Option<String>,
    methods: HashMap<String, Rc<FnDecl>>,
}

/// What a name is, for an editor that has to say so before it can draw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Class,
    Function,
    Method,
    Field,
    Variable,
    /// A name the caller filled in, which carries no type until v0.7.
    Parameter,
    Module,
}

/// A name, and everything an editor needs in order to offer it.
///
/// The one thing `lsp.rs` and the REPL both ask for. Before this existed each
/// of them walked the source for itself and reached its own conclusions, which
/// is how the editor came to complete a list's methods on a string.
#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: Kind,
    /// What the name holds. A function's name holds a `function`, whatever
    /// calling it would produce — the two are different questions and were one
    /// field until the corpus check caught it.
    pub ty: Type,
    /// What calling it produces, for a function or a method. `Unknown` for
    /// everything else, which is not callable and so has no answer.
    pub returns: Type,
    pub doc: Option<Doc>,
    /// The names a caller writes, for a function or a method.
    pub params: Vec<String>,
    /// The word that introduced the declaration, where one did.
    ///
    /// `let`, `final`, `const` — a real distinction and one an editor should
    /// show, since `final` and `const` are the difference between a name that
    /// can be rebound and one that cannot.
    pub keyword: Option<&'static str>,
}

impl Symbol {
    /// A symbol with nothing known about it beyond its name and what it holds.
    pub fn new(name: impl Into<String>, kind: Kind, ty: Type) -> Symbol {
        Symbol {
            name: name.into(),
            kind,
            ty,
            returns: Type::Unknown,
            doc: None,
            params: Vec::new(),
            keyword: None,
        }
    }

    fn declared_with(mut self, keyword: &'static str) -> Symbol {
        self.keyword = Some(keyword);
        self
    }

    fn returning(mut self, returns: Type) -> Symbol {
        self.returns = returns;
        self
    }

    fn with_doc(mut self, doc: Option<Doc>) -> Symbol {
        self.doc = doc;
        self
    }

    /// How the declaration reads back, for a hover or a completion's detail.
    pub fn signature(&self) -> String {
        let named = |ty: &Type| match ty.class_name() {
            // `nil` is not written. A function that hands nothing back says so
            // by having no return type, which is how every language that has
            // them spells it — and `fn init(name): nil` reads as a claim rather
            // than as the absence of one.
            Some("nil") | None => String::new(),
            Some(class) => format!(": {class}"),
        };
        match self.kind {
            Kind::Function | Kind::Method => format!(
                "fn {}({}){}",
                self.name,
                self.params.join(", "),
                named(&self.returns)
            ),
            Kind::Class => format!("class {}", self.name),
            Kind::Module => format!("module {}", self.name),
            _ => match self.keyword {
                Some(keyword) => format!("{keyword} {}{}", self.name, named(&self.ty)),
                None => format!("{}{}", self.name, named(&self.ty)),
            },
        }
    }
}

/// One name in scope, and what is known about it.
struct Binding {
    symbol: Symbol,
    /// The block the name lives in. Bindings are found by asking which of them
    /// covers an offset, which is how a local in one function is kept from
    /// answering for the same name in another.
    scope: Span,
    /// The offset from which the name means this. A `let` starts at its own
    /// statement; a `fn` or a `class` covers its whole scope, because the
    /// resolver lets a function call one declared below it.
    from: u32,
}

/// The scope every top-level name lives in.
///
/// A real span would have to be the whole file's, and the parser hands out no
/// such node — the top level is a `Vec<Stmt>`, not a `Block`. Using the widest
/// span there is makes it the outermost scope by construction, which is the one
/// property the lookup needs of it.
const FILE: Span = Span {
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
fn lookup(bindings: &[Binding], name: &str, offset: u32) -> Option<usize> {
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

/// Everything the pass worked out, ready to be asked questions.
pub struct Types {
    /// Keyed by where the expression starts, which is unique: no two
    /// expressions in one file begin at the same byte.
    exprs: HashMap<u32, Type>,
    bindings: Vec<Binding>,
    classes: HashMap<String, ClassInfo>,
    fields: HashMap<String, HashMap<String, Type>>,
    /// What each top-level or nested `fn` returns, by name. A name shadowed by
    /// a second declaration holds the last one, which is what the evaluator
    /// would find too.
    functions: HashMap<String, Type>,
    /// What each method returns, by the class that declares it and its name.
    methods: HashMap<(String, String), Type>,
}

impl Types {
    /// What the expression starting at `offset` evaluates to.
    ///
    /// Where two expressions start at the same byte — a call and the name being
    /// called — the outer one answers, since it is the value the line produces.
    pub fn of_expr(&self, offset: u32) -> Type {
        self.exprs.get(&offset).cloned().unwrap_or_default()
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

    /// Whether the program declared a class by this name.
    pub fn declares(&self, class: &str) -> bool {
        self.classes.contains_key(class)
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
            if let Some(info) = self.classes.get(&current) {
                for (name, decl) in &info.methods {
                    let ty = self
                        .methods
                        .get(&(current.clone(), name.clone()))
                        .cloned()
                        .unwrap_or_default();
                    push_once(&mut found, symbol_for(decl, Kind::Method, ty));
                }
            }
            if let Some(fields) = self.fields.get(&current) {
                for (name, ty) in fields {
                    push_once(&mut found, Symbol::new(name, Kind::Field, ty.clone()));
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
            match self.classes.get(&current).and_then(|info| info.parent.clone()) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        found
    }

    /// The class `class` extends, if the program said so.
    pub fn parent_of(&self, class: &str) -> Option<&str> {
        self.classes.get(class)?.parent.as_deref()
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
            current = self.classes.get(&current)?.parent.clone()?;
        }
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
                    (Type::Module(module), None) => module_member(module, segment),
                    (Type::Module(module), Some(name)) => {
                        module_native(module, name).map_or(Type::Unknown, returned_by)
                    }
                    _ => Type::Unknown,
                };
                if !ty.is_known() {
                    return Type::Unknown;
                }
                continue;
            };
            let class = class.clone();
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
fn called(segment: &str) -> Option<&str> {
    segment.strip_suffix("()")
}

/// What a stdlib module's member holds.
///
/// Read off `stdlib::MODULES` rather than from a list kept here, for the reason
/// the completion tables were made to: a second copy of the library's contents
/// is a copy that will be wrong. A constant is *built* to find out what it is,
/// which is exact rather than a guess — `math.pi` is a float because building
/// it produces one.
fn module_member(module: &str, name: &str) -> Type {
    let Some(module) = stdlib::module_named(module) else {
        return Type::Unknown;
    };
    match module.members.iter().find(|(member, _)| *member == name) {
        Some((_, stdlib::Member::Fn(_))) => Type::class("function"),
        Some((_, stdlib::Member::Const(build))) => of_value(&build()),
        None => Type::Unknown,
    }
}

/// Adds a symbol unless a name has already answered for it.
///
/// First wins, and the walk goes in dispatch order — so a method a subclass
/// overrode is offered once, as the one that would run.
fn push_once(found: &mut Vec<Symbol>, symbol: Symbol) {
    if !found.iter().any(|seen| seen.name == symbol.name) {
        found.push(symbol);
    }
}

/// A symbol for a function or method the program declared.
///
/// Its parameters are the ones someone wrote, so the receiver a method carries
/// is left out — `self` is not a name a caller types.
fn symbol_for(decl: &Rc<FnDecl>, kind: Kind, returns: Type) -> Symbol {
    Symbol {
        name: decl.name.clone(),
        kind,
        ty: Type::class("function"),
        returns,
        doc: decl.doc.clone(),
        keyword: None,
        params: decl
            .params
            .iter()
            .filter(|param| !param.receiver)
            .map(|param| param.name.clone())
            .collect(),
    }
}

/// What a function's documentation said about one of its parameters.
///
/// A parameter carries no type until v0.7, so an `@param` is the only thing
/// anyone can be told about one — which is most of what makes writing them
/// worth the trouble.
fn described_by(decl: &Rc<FnDecl>, param: &str) -> Option<Doc> {
    let doc = decl.doc.as_ref()?;
    let described = doc.params.iter().find(|named| named.name == param)?;
    Some(Doc {
        summary: described.text.clone(),
        params: Vec::new(),
        returns: None,
        throws: Vec::new(),
        span: described.span,
    })
}

/// A symbol for a native, read off the tables it is declared in.
///
/// Its documentation goes through the same parser a `##` block does, so `print`
/// and a function someone wrote are rendered by one code path. A native's doc
/// is a Rust string literal and cannot carry a span, so a malformed one is
/// dropped rather than reported — there is no source line to underline, and
/// `every_native_documents_only_parameters_it_takes` is what catches it instead.
pub fn symbol_of_native(native: &'static Native, kind: Kind) -> Symbol {
    Symbol {
        name: native.name.to_string(),
        kind,
        ty: Type::class("function"),
        returns: returned_by(native),
        doc: Doc::parse_text(native.doc).ok().filter(|doc| !doc.is_empty()),
        keyword: None,
        params: native.params.iter().map(|name| name.to_string()).collect(),
    }
}

/// Every name a program starts with: the globals, and the types it can call.
///
/// Read off the same tables the interpreter binds them from, so a builtin added
/// later is offered without this function being touched — the rule the
/// completion lists were put under when `bool` turned out to be missing from a
/// hand-written copy of them.
pub fn globals() -> Vec<Symbol> {
    let mut found: Vec<Symbol> = crate::interp::BUILTINS
        .iter()
        .map(|native| symbol_of_native(native, Kind::Function))
        .collect();
    for builtin in BUILTINS {
        let seed = builtin.seed();
        // A type with no constructor is a type no program can name: `nil` and
        // `class` are keywords, so completing to one would be a lie.
        if let Some(init) = seed.init {
            let mut symbol = symbol_of_native(init, Kind::Class);
            symbol.name = seed.name.to_string();
            // The name holds the class object; calling it makes one of the type.
            symbol.ty = Type::class("class");
            found.push(symbol);
        }
    }
    found.extend(error_classes().iter().cloned());
    found
}

/// The error classes, inferred from the prelude that declares them.
///
/// The prelude is Quince — `Error` and its `op init(message)` are written in
/// the language, and the kinds extend it — so this reads it with the pass
/// rather than restating it. `TypeError(message)` is what the editor shows
/// because that is what the source says, and a change to `BASE_ERROR` reaches
/// the editor without anything here being touched.
///
/// Built once. It is the same handful of classes on every keystroke.
fn error_classes() -> &'static [Symbol] {
    static CLASSES: std::sync::OnceLock<Vec<Symbol>> = std::sync::OnceLock::new();
    CLASSES.get_or_init(|| {
        let mut source = String::from(crate::interp::BASE_ERROR);
        for kind in crate::error::ERROR_KINDS {
            let Some(name) = kind.class_name() else {
                continue;
            };
            if name != "Error" {
                source.push_str(&format!("class {name} extends Error {{}}\n"));
            }
        }
        let Ok(program) = crate::compile(&source) else {
            return Vec::new();
        };
        let types = infer(&program);
        types
            .in_scope(source.len() as u32)
            .into_iter()
            .filter(|symbol| symbol.kind == Kind::Class)
            .map(|mut symbol| {
                // Calling a class runs its `init`, so those are the parameters
                // worth showing — inherited, for every kind but `Error` itself.
                symbol.params = types
                    .members_of(&symbol.name)
                    .into_iter()
                    .find(|member| member.name == "init")
                    .map(|init| init.params)
                    .unwrap_or_default();
                symbol
            })
            .collect()
    })
}

/// What a stdlib module offers after its dot.
pub fn module_symbols(module: &str) -> Vec<Symbol> {
    let Some(module) = stdlib::module_named(module) else {
        return Vec::new();
    };
    module
        .members
        .iter()
        .map(|(name, member)| match member {
            stdlib::Member::Fn(native) => symbol_of_native(native, Kind::Function),
            stdlib::Member::Const(build) => {
                Symbol::new(*name, Kind::Variable, of_value(&build()))
            }
        })
        .collect()
}

/// The native a stdlib module declares under `name`.
fn module_native(module: &str, name: &str) -> Option<&'static Native> {
    let module = stdlib::module_named(module)?;
    module.members.iter().find_map(|(member, kind)| match kind {
        stdlib::Member::Fn(native) if *member == name => Some(*native),
        _ => None,
    })
}

/// The native a builtin type declares as its method called `name`.
///
/// Only the builtins: a class the program wrote has its methods in the AST, and
/// those are answered by walking it. Reached through `BUILTINS` and the seed
/// tables the classes are built from, so a method added to a type is found here
/// without this file knowing it exists.
fn builtin_method(class: &str, name: &str) -> Option<&'static Native> {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name() == class)?
        .seed()
        .methods
        .iter()
        .find_map(|(method, native)| (*method == name).then_some(*native))
}

/// The native answering for `class`'s method called `name`, searching the
/// classes it descends from.
///
/// A class extending `list` inherits the list's methods, so `Stack().push` has
/// to find `push` on the builtin two links up. Free rather than a method,
/// because both the walk and the finished table need it and they hold their
/// classes in the same shape.
fn builtin_ancestor(
    classes: &HashMap<String, ClassInfo>,
    class: &str,
    name: &str,
) -> Option<&'static Native> {
    let mut current = class.to_string();
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        if let Some(native) = builtin_method(&current, name) {
            return Some(native);
        }
        current = classes.get(&current)?.parent.clone()?;
    }
}

/// What a native hands back, which is `Unknown` for one that does not say.
///
/// The `None` case is not a shortfall to be filled in later. `dict.get` answers
/// with whatever was stored and `io.line` answers with a string until input runs
/// out — declaring a type for either would be a guess wearing the authority of
/// a table.
fn returned_by(native: &Native) -> Type {
    match native.returns {
        Some(builtin) => Type::class(builtin.name()),
        None => Type::Unknown,
    }
}

/// The class of a value that names its own class.
///
/// Only the variants that carry no handle, because naming the rest needs a heap
/// and there is none here. Every stdlib constant is one of these, and one that
/// is not answers `Unknown` rather than being reached for.
fn of_value(value: &Value) -> Type {
    match value {
        Value::Nil => Type::class("nil"),
        Value::Bool(_) => Type::class("bool"),
        Value::Int(_) => Type::class("int"),
        Value::Float(_) => Type::class("float"),
        Value::Str(_) => Type::class("string"),
        _ => Type::Unknown,
    }
}

/// Whether `name` is a builtin type that can be called to make one.
///
/// `int(x)`, `string(x)`, `bool(x)` and the three collections. Asked of
/// `Builtin::conversion` rather than of a list written here, so the day a
/// builtin type gains or loses a constructor this follows it. `nil` and `class`
/// answer no, and would anyway: both are keywords, so neither can be called.
fn builtin_constructor(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|builtin| builtin.name() == name && builtin.conversion().is_some())
        .map(|builtin| builtin.name())
}

/// Works out what it can about a program.
///
/// Takes the AST the resolver accepted, though it does not read the slots —
/// what it needs is that names mean what the scope rules say they mean. The
/// language server hands it unresolved trees too, because a document mid-edit
/// is often all there is, and the answers are the same wherever a name is
/// unambiguous.
pub fn infer(program: &[Stmt]) -> Types {
    let mut pass = Infer::default();
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
        methods: pass.method_returns,
    }
}

#[derive(Default)]
struct Infer {
    classes: HashMap<String, ClassInfo>,
    functions: HashMap<String, Rc<FnDecl>>,
    /// Return types worked out so far, keyed by the identity of the
    /// declaration rather than by its name.
    ///
    /// Two methods in two classes are both called `at`, and a function may be
    /// shadowed by a local — a name is not a key. The `Rc` a declaration is
    /// held behind is.
    returns: HashMap<usize, Type>,
    /// Declarations whose return type is being worked out right now.
    ///
    /// `fn down(n) { return down(n - 1) }` is a program, and a pass that
    /// followed it would not stop. Meeting a declaration already in here
    /// answers `Unknown`, which is also the true answer: the recursive arm
    /// carries no information about the type.
    computing: HashSet<usize>,
    /// Classes whose fields are being worked out right now, for the same reason
    /// and against the same shape: `self.next = Node()` inside `Node`.
    computing_fields: HashSet<String>,
    fields: HashMap<String, HashMap<String, Type>>,
    function_returns: HashMap<String, Type>,
    method_returns: HashMap<(String, String), Type>,
    exprs: HashMap<u32, Type>,
    bindings: Vec<Binding>,
    /// Enclosing block spans, innermost last.
    scopes: Vec<Span>,
    /// The classes whose bodies are being walked, innermost last, so `self` has
    /// something to mean.
    receivers: Vec<String>,
}

impl Infer {
    /// Finds every class and every function first, so a call to one declared
    /// further down is not a call to nothing.
    ///
    /// The same reason the resolver has a `declare_all`: a Quince file is not
    /// read top to bottom by the program that runs it, and a pass that pretended
    /// otherwise would answer `Unknown` for exactly the forward references the
    /// language went out of its way to allow.
    fn declare(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Fn { decl, .. } => {
                    self.functions.insert(decl.name.clone(), Rc::clone(decl));
                    self.declare(&decl.body.stmts);
                }
                StmtKind::Class {
                    name,
                    parent,
                    methods,
                    ..
                } => {
                    let info = ClassInfo {
                        parent: parent.as_ref().map(|var| var.name.clone()),
                        methods: methods
                            .iter()
                            .map(|decl| (decl.name.clone(), Rc::clone(decl)))
                            .collect(),
                    };
                    self.classes.insert(name.clone(), info);
                }
                // An extension adds to a class that already exists, so its
                // methods join that entry. `extend list` makes an entry with no
                // parent, which is not a claim that the program declared
                // `list` — the builtin's own methods are still found through
                // `builtin_ancestor`, and this only records what was added
                // beside them.
                StmtKind::Extend { target, methods, .. } => {
                    let info = self.classes.entry(target.name.clone()).or_default();
                    for decl in methods {
                        info.methods.insert(decl.name.clone(), Rc::clone(decl));
                    }
                }
                StmtKind::If { then, otherwise, .. } => {
                    self.declare(&then.stmts);
                    if let Some(other) = otherwise {
                        self.declare(std::slice::from_ref(other.as_ref()));
                    }
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                    self.declare(&body.stmts)
                }
                StmtKind::Try { body, handler, .. } => {
                    self.declare(&body.stmts);
                    self.declare(&handler.stmts);
                }
                StmtKind::Block(block) => self.declare(&block.stmts),
                _ => {}
            }
        }
    }

    /// Works out what every class stores in its fields.
    ///
    /// Before the main walk rather than during it, because `p.x` may be written
    /// above the class that gives `x` a type — and a field whose type depended
    /// on where in the file it was asked about would be a worse answer than no
    /// answer at all. Sorted so that two classes whose fields refer to each
    /// other resolve the same way on every run rather than by hash order.
    fn fields(&mut self) {
        let mut names: Vec<String> = self.classes.keys().cloned().collect();
        names.sort();
        for name in names {
            self.fields_of(&name);
        }
    }

    /// Everything the class's own methods assign to `self`.
    ///
    /// Joined across every assignment, so a field set to an int in `init` and
    /// to a string in a setter is `Unknown` — which is what it is. Only the
    /// class's own methods are read; an inherited field is found by walking up
    /// afterwards, so a subclass does not silently claim its parent's.
    fn fields_of(&mut self, class: &str) {
        if self.fields.contains_key(class) {
            return;
        }
        if !self.computing_fields.insert(class.to_string()) {
            // Reached from inside its own computation. Leaving no entry behind
            // is what makes that `Unknown` rather than a loop.
            return;
        }
        let methods: Vec<Rc<FnDecl>> = match self.classes.get(class) {
            Some(info) => info.methods.values().cloned().collect(),
            None => Vec::new(),
        };

        let mut found: HashMap<String, Type> = HashMap::new();
        self.receivers.push(class.to_string());
        for decl in methods {
            self.scopes.push(decl.body.span);
            for param in &decl.params {
                let ty = if param.receiver {
                    Type::class(class)
                } else {
                    Type::Unknown
                };
                self.bind(&param.name, Kind::Parameter, ty, decl.body.span.start);
            }
            self.assignments_to_self(&decl.body.stmts, &mut found);
            self.scopes.pop();
        }
        self.receivers.pop();

        self.computing_fields.remove(class);
        self.fields.insert(class.to_string(), found);
    }

    /// Collects `self.name = value` out of a statement list, into `found`.
    fn assignments_to_self(&mut self, stmts: &[Stmt], found: &mut HashMap<String, Type>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Expr(expr) | StmtKind::Throw(expr) | StmtKind::Return(Some(expr)) => {
                    self.assignments_in(expr, found);
                }
                StmtKind::Let { value, .. } => self.assignments_in(value, found),
                StmtKind::If { cond, then, otherwise } => {
                    self.assignments_in(cond, found);
                    self.assignments_to_self(&then.stmts, found);
                    if let Some(other) = otherwise {
                        self.assignments_to_self(std::slice::from_ref(other.as_ref()), found);
                    }
                }
                StmtKind::While { cond, body } => {
                    self.assignments_in(cond, found);
                    self.assignments_to_self(&body.stmts, found);
                }
                StmtKind::For { iter, body, .. } => {
                    self.assignments_in(iter, found);
                    self.assignments_to_self(&body.stmts, found);
                }
                StmtKind::Try { body, handler, .. } => {
                    self.assignments_to_self(&body.stmts, found);
                    self.assignments_to_self(&handler.stmts, found);
                }
                StmtKind::Block(block) => self.assignments_to_self(&block.stmts, found),
                _ => {}
            }
        }
    }

    fn assignments_in(&mut self, expr: &Expr, found: &mut HashMap<String, Type>) {
        if let ExprKind::Assign { target, value } = &expr.kind
            && let ExprKind::Field { target: receiver, name } = &target.kind
            && matches!(&receiver.kind, ExprKind::Var(var) if var.name == SELF)
        {
            let ty = self.expr(value);
            let ty = match found.remove(name) {
                Some(previous) => previous.join(ty),
                None => ty,
            };
            found.insert(name.clone(), ty);
        }
        for child in children(expr) {
            self.assignments_in(child, found);
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Expr(expr) | StmtKind::Throw(expr) => {
                self.expr(expr);
            }
            StmtKind::Let { name, value, bind, doc, .. } => {
                let ty = self.expr(value);
                let symbol = Symbol::new(name, Kind::Variable, ty)
                    .declared_with(bind.word())
                    .with_doc(doc.clone());
                self.bind_symbol(symbol, stmt.span.start);
            }
            StmtKind::Fn { decl, .. } => {
                let scope = self.scope().start;
                let returns = self.function(decl, None);
                self.function_returns
                    .insert(decl.name.clone(), returns.clone());
                self.bind_symbol(symbol_for(decl, Kind::Function, returns), scope);
            }
            StmtKind::Class { name, methods, doc, .. } => {
                // The name holds the class object, not an instance of it —
                // `Point` is a `class` and `Point()` is a `Point`. Saying
                // otherwise would make every mention of a class name look like
                // a value of it, which is the mistake the capital-letter
                // heuristic makes.
                let scope = self.scope().start;
                let symbol = Symbol::new(name, Kind::Class, Type::class("class"))
                    .returning(Type::class(name.clone()))
                    .with_doc(doc.clone());
                self.bind_symbol(symbol, scope);
                self.methods(name, methods);
            }
            StmtKind::Extend { target, methods, .. } => self.methods(&target.name, methods),
            StmtKind::Import { module, names, .. } => {
                let known = stdlib::module_named(module).is_some();
                match names {
                    ImportNames::Module => {
                        let ty = if known {
                            Type::Module(module.clone())
                        } else {
                            Type::Unknown
                        };
                        self.bind(module, Kind::Module, ty, stmt.span.start);
                    }
                    ImportNames::Names(names) => {
                        // The member's own symbol, so `from math import floor`
                        // leaves `floor` with the documentation and parameters
                        // `math.floor` has. Binding only its type would have
                        // made the shorter spelling the worse one.
                        let members = module_symbols(module);
                        for name in names {
                            match members.iter().find(|symbol| symbol.name == name.name) {
                                Some(symbol) if known => {
                                    self.bind_symbol(symbol.clone(), stmt.span.start)
                                }
                                _ => self.bind(
                                    &name.name,
                                    Kind::Variable,
                                    Type::Unknown,
                                    stmt.span.start,
                                ),
                            }
                        }
                    }
                }
            }
            StmtKind::If { cond, then, otherwise } => {
                self.expr(cond);
                self.block(then);
                if let Some(other) = otherwise {
                    self.stmt(other);
                }
            }
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For { var, iter, body, .. } => {
                let element = self.element_of(iter);
                self.scopes.push(body.span);
                self.bind(var, Kind::Variable, element, body.span.start);
                self.stmts(&body.stmts);
                self.scopes.pop();
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            StmtKind::Try { body, binding, handler, .. } => {
                self.block(body);
                self.scopes.push(handler.span);
                // The caught value is an instance of some error class, and
                // which one is the throw's business rather than the catch's.
                // `Error` would be a guess that reads as knowledge.
                self.bind(binding, Kind::Variable, Type::Unknown, handler.span.start);
                self.stmts(&handler.stmts);
                self.scopes.pop();
            }
            StmtKind::Block(block) => self.block(block),
        }
    }

    /// Walks the methods a `class` or an `extend` block declares.
    fn methods(&mut self, class: &str, methods: &[Rc<FnDecl>]) {
        self.receivers.push(class.to_string());
        for decl in methods {
            let ty = self.function(decl, Some(class.to_string()));
            self.method_returns
                .insert((class.to_string(), decl.name.clone()), ty);
        }
        self.receivers.pop();
    }

    fn block(&mut self, block: &Block) {
        self.scopes.push(block.span);
        self.stmts(&block.stmts);
        self.scopes.pop();
    }

    /// Walks a function body, binding its parameters, and answers with what it
    /// returns.
    fn function(&mut self, decl: &Rc<FnDecl>, class: Option<String>) -> Type {
        self.scopes.push(decl.body.span);
        for param in &decl.params {
            // A parameter carries no information at all until v0.7 lets one be
            // annotated — except the receiver, which is the one parameter the
            // language itself filled in.
            let ty = match (param.receiver, &class) {
                (true, Some(class)) => Type::class(class.clone()),
                _ => Type::Unknown,
            };
            let symbol = Symbol::new(&param.name, Kind::Parameter, ty)
                .with_doc(described_by(decl, &param.name));
            self.bind_symbol(symbol, decl.body.span.start);
        }
        // `super` is a name in a method of a class that extends one, bound to
        // the parent — the resolver puts it in a scope wrapped around the
        // methods, and this is the same fact said where a lookup can find it.
        if let Some(parent) = class.as_ref().and_then(|class| self.classes.get(class))
            .and_then(|info| info.parent.clone())
        {
            self.bind(
                crate::ast::SUPER,
                Kind::Variable,
                Type::Class(parent),
                decl.body.span.start,
            );
        }
        self.stmts(&decl.body.stmts);
        self.scopes.pop();
        self.return_of(decl)
    }

    /// What calling this declaration produces.
    ///
    /// The join of every `return` in its body, with a body that returns nothing
    /// anywhere answering `nil`. A body with *some* returns and a path that
    /// falls off the end is taken at its word rather than joined with the `nil`
    /// that path produces — the alternative is a flow analysis, and this pass
    /// is the floor such a thing would stand on rather than a first draft of it.
    fn return_of(&mut self, decl: &Rc<FnDecl>) -> Type {
        let key = Rc::as_ptr(decl) as usize;
        if let Some(ty) = self.returns.get(&key) {
            return ty.clone();
        }
        if !self.computing.insert(key) {
            return Type::Unknown;
        }
        let mut returns: Option<Type> = None;
        self.returned(&decl.body.stmts, &mut returns);
        self.computing.remove(&key);
        let ty = returns.unwrap_or_else(|| Type::class("nil"));
        self.returns.insert(key, ty.clone());
        ty
    }

    /// Joins every `return` in a body, without descending into functions
    /// declared inside it — those return for themselves.
    fn returned(&mut self, stmts: &[Stmt], found: &mut Option<Type>) {
        for stmt in stmts {
            match &stmt.kind {
                StmtKind::Return(value) => {
                    let ty = match value {
                        Some(value) => self.expr(value),
                        // `return` with nothing after it hands back `nil`, so
                        // it joins like any other answer rather than being
                        // skipped: a function returning a `Point` on one path
                        // and bare-`return`ing on another does not return a
                        // `Point`.
                        None => Type::class("nil"),
                    };
                    *found = Some(match found.take() {
                        Some(previous) => previous.join(ty),
                        None => ty,
                    });
                }
                StmtKind::If { then, otherwise, .. } => {
                    self.returned(&then.stmts, found);
                    if let Some(other) = otherwise {
                        self.returned(std::slice::from_ref(other.as_ref()), found);
                    }
                }
                StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                    self.returned(&body.stmts, found)
                }
                StmtKind::Try { body, handler, .. } => {
                    self.returned(&body.stmts, found);
                    self.returned(&handler.stmts, found);
                }
                StmtKind::Block(block) => self.returned(&block.stmts, found),
                _ => {}
            }
        }
    }

    /// What a `for` loop's variable holds.
    ///
    /// A list literal is read for its items, which is the one case where the
    /// element type is written down in the file. Iterating a string yields
    /// strings. Anything else — a list held in a variable, a class with `op
    /// iter` — is `Unknown`, because a list is not a `list[T]` until v0.7 says
    /// so.
    fn element_of(&mut self, iter: &Expr) -> Type {
        let ty = self.expr(iter);
        if let ExprKind::List(items) = &iter.kind {
            let mut joined: Option<Type> = None;
            for item in items {
                let item = self.of(item);
                joined = Some(match joined.take() {
                    Some(previous) => previous.join(item),
                    None => item,
                });
            }
            return joined.unwrap_or_default();
        }
        match ty.class_name() {
            Some("string") => Type::class("string"),
            _ => Type::Unknown,
        }
    }

    /// Records `name` in the innermost scope, visible from `from` onward.
    fn bind(&mut self, name: &str, kind: Kind, ty: Type, from: u32) {
        self.bind_symbol(Symbol::new(name, kind, ty), from);
    }

    fn bind_symbol(&mut self, symbol: Symbol, from: u32) {
        let scope = self.scope();
        self.bindings.push(Binding {
            symbol,
            scope,
            from,
        });
    }

    fn scope(&self) -> Span {
        self.scopes.last().copied().unwrap_or(FILE)
    }

    /// What an expression already walked evaluates to.
    fn of(&self, expr: &Expr) -> Type {
        self.exprs.get(&expr.span.start).cloned().unwrap_or_default()
    }

    /// Walks an expression, records what it evaluates to, and answers with it.
    fn expr(&mut self, expr: &Expr) -> Type {
        let ty = self.decide(expr);
        self.exprs.insert(expr.span.start, ty.clone());
        ty
    }

    fn decide(&mut self, expr: &Expr) -> Type {
        match &expr.kind {
            ExprKind::Int(_) => Type::class("int"),
            ExprKind::Float(_) => Type::class("float"),
            ExprKind::Str(_) => Type::class("string"),
            ExprKind::Bool(_) => Type::class("bool"),
            ExprKind::Nil => Type::class("nil"),

            ExprKind::List(items) => {
                for item in items {
                    self.expr(item);
                }
                Type::class("list")
            }
            ExprKind::Dict(pairs) => {
                for (key, value) in pairs {
                    self.expr(key);
                    self.expr(value);
                }
                Type::class("dict")
            }

            ExprKind::Var(var) => lookup(&self.bindings, &var.name, expr.span.start)
                .map(|index| self.bindings[index].symbol.ty.clone())
                .unwrap_or_default(),

            ExprKind::Unary { op, rhs } => {
                let rhs = self.expr(rhs);
                match op {
                    // `!x` asks for truthiness, and truthiness is a bool
                    // whatever the class of `x` had to say about it.
                    UnaryOp::Not => Type::class("bool"),
                    // `-x` reaches `op neg` first, and whatever that answers is
                    // the answer — so only the numbers, where the language
                    // decides, are decidable here.
                    UnaryOp::Neg => match rhs.class_name() {
                        Some("int") => Type::class("int"),
                        Some("float") => Type::class("float"),
                        _ => Type::Unknown,
                    },
                }
            }

            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                binary(*op, &lhs, &rhs)
            }

            // Both operands are answers: `a || b` hands back the operand rather
            // than a bool, so that it reads as a default. Two operands of one
            // class make one; anything else is two answers and so is neither.
            ExprKind::Logical { lhs, rhs, .. } => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                lhs.join(rhs)
            }

            ExprKind::Call { callee, args } => {
                for arg in args {
                    self.expr(arg);
                }
                self.call(callee)
            }

            // Indexing a string gives a string. A list holds whatever was put
            // in it and a dict holds whatever was mapped to, so neither says.
            ExprKind::Index { target, index } => {
                let target = self.expr(target);
                self.expr(index);
                match target.class_name() {
                    Some("string") => Type::class("string"),
                    _ => Type::Unknown,
                }
            }

            // A slice of a sequence is a sequence of the same kind, which is
            // more than indexing gives: `xs[1:]` is a list even when nothing is
            // known about what is in it.
            ExprKind::Slice { target, start, end } => {
                let target = self.expr(target);
                for bound in [start, end].into_iter().flatten() {
                    self.expr(bound);
                }
                match target.class_name() {
                    Some("string") => Type::class("string"),
                    Some("list") => Type::class("list"),
                    _ => Type::Unknown,
                }
            }

            ExprKind::Field { target, name } => {
                let target = self.expr(target);
                self.read(&target, name)
            }

            ExprKind::Super { name, .. } => match self.parent() {
                Some(parent) => self.read(&Type::Class(parent), name),
                None => Type::Unknown,
            },

            ExprKind::Assign { target, value } => {
                let ty = self.expr(value);
                self.expr(target);
                // A name assigned a second type holds neither from here on.
                // Recorded against the binding rather than against this
                // expression, because it is the *name* the answer changes for.
                if let ExprKind::Var(var) = &target.kind {
                    self.reassign(&var.name, target.span.start, &ty);
                }
                ty
            }
        }
    }

    /// Narrows a binding to what it holds once it has been assigned again.
    fn reassign(&mut self, name: &str, offset: u32, ty: &Type) {
        if let Some(index) = lookup(&self.bindings, name, offset) {
            let previous = std::mem::take(&mut self.bindings[index].symbol.ty);
            self.bindings[index].symbol.ty = previous.join(ty.clone());
        }
    }

    /// The class whose body is being walked.
    fn receiver(&self) -> Option<String> {
        self.receivers.last().cloned()
    }

    /// The class that one extends.
    fn parent(&self) -> Option<String> {
        let class = self.receiver()?;
        self.classes.get(&class)?.parent.clone()
    }

    /// A field or method read off a value without calling it: `p.x`, `math.pi`.
    ///
    /// A method named but not called is a `function` — the value is the method,
    /// not what it would produce — which is why this and [`Self::call`] answer
    /// differently for the same two names.
    fn read(&mut self, target: &Type, name: &str) -> Type {
        match target {
            Type::Class(class) => {
                if self.method(class, name).is_some()
                    || builtin_ancestor(&self.classes, class, name).is_some()
                {
                    return Type::class("function");
                }
                self.field(class, name)
            }
            Type::Module(module) => module_member(module, name),
            Type::Unknown => Type::Unknown,
        }
    }

    /// What `class` stores under `name`, working its fields out on demand and
    /// searching its parents.
    fn field(&mut self, class: &str, name: &str) -> Type {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return Type::Unknown;
            }
            self.fields_of(&current);
            if let Some(ty) = self.fields.get(&current).and_then(|fields| fields.get(name)) {
                return ty.clone();
            }
            match self.classes.get(&current).and_then(|info| info.parent.clone()) {
                Some(parent) => current = parent,
                None => return Type::Unknown,
            }
        }
    }

    /// The declaration of `class`'s method called `name`, searching parents.
    fn method(&self, class: &str, name: &str) -> Option<Rc<FnDecl>> {
        let mut current = class.to_string();
        let mut seen = HashSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return None;
            }
            let info = self.classes.get(&current)?;
            if let Some(decl) = info.methods.get(name) {
                return Some(Rc::clone(decl));
            }
            current = info.parent.clone()?;
        }
    }

    /// What calling `callee` produces.
    fn call(&mut self, callee: &Expr) -> Type {
        match &callee.kind {
            ExprKind::Var(var) => {
                self.expr(callee);
                // A class name in call position makes one of that class. This
                // is the one place a bare name means an instance, and it is
                // decided by the declaration rather than by the spelling —
                // `point()` builds a `point` if that is what was declared.
                if self.classes.contains_key(&var.name) {
                    return Type::class(var.name.clone());
                }
                if let Some(builtin) = builtin_constructor(&var.name) {
                    return Type::class(builtin);
                }
                match self.functions.get(&var.name).cloned() {
                    Some(decl) => self.return_of(&decl),
                    // A builtin global — `len`, `print`, `type` — which says
                    // what it hands back on the native itself.
                    None => match crate::interp::BUILTINS
                        .iter()
                        .find(|native| native.name == var.name)
                    {
                        Some(native) => returned_by(native),
                        None => Type::Unknown,
                    },
                }
            }

            ExprKind::Field { target, name } => {
                let target = self.expr(target);
                // The callee is the method itself, and the call is what it
                // produces. Both get recorded, at their own spans.
                let member = self.read(&target, name);
                self.exprs.insert(callee.span.start, member);
                match &target {
                    // The program's own method first, so a class extending a
                    // builtin that overrides `sort` is answered by what it
                    // wrote rather than by the table it inherited from.
                    Type::Class(class) => match self.method(class, name) {
                        Some(decl) => self.return_of(&decl),
                        None => builtin_ancestor(&self.classes, class, name)
                            .map_or(Type::Unknown, returned_by),
                    },
                    Type::Module(module) => {
                        module_native(module, name).map_or(Type::Unknown, returned_by)
                    }
                    Type::Unknown => Type::Unknown,
                }
            }

            ExprKind::Super { name, .. } => match self.parent() {
                Some(parent) => match self.method(&parent, name) {
                    Some(decl) => self.return_of(&decl),
                    None => Type::Unknown,
                },
                None => Type::Unknown,
            },

            _ => {
                self.expr(callee);
                Type::Unknown
            }
        }
    }
}

/// What an operator produces, when the language rather than a class decides.
///
/// A class declaring `op add` may answer with anything, so only operands that
/// are builtins are decidable — with the comparisons excepted, which the
/// evaluator forces to a bool whatever an `op cmp` returned.
fn binary(op: BinaryOp, lhs: &Type, rhs: &Type) -> Type {
    use BinaryOp::*;

    // Every comparison ends in a bool. `==` is defined for every pair of values,
    // `<` and its family read whatever `op cmp` answered for its sign, and `in`
    // does the same to `op contains`.
    if matches!(op, Eq | Ne | Lt | Le | Gt | Ge | In) {
        return Type::class("bool");
    }

    let (Some(lhs), Some(rhs)) = (lhs.class_name(), rhs.class_name()) else {
        return Type::Unknown;
    };

    match (op, lhs, rhs) {
        // True division leaves the integers behind: `1 / 2` is `0.5`.
        (Div, "int" | "float", "int" | "float") => Type::class("float"),
        (_, "int", "int") => Type::class("int"),
        (_, "int" | "float", "int" | "float") => Type::class("float"),
        (Add, "string", "string") => Type::class("string"),
        (Add, "list", "list") => Type::class("list"),
        _ => Type::Unknown,
    }
}

/// Every expression written inside this one.
///
/// One level, so a caller that wants the whole tree recurses. A `Vec` of borrows
/// rather than an iterator because the shapes differ enough that an iterator
/// would be a hand-written enum of six cases.
fn children(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Nil
        | ExprKind::Var(_)
        | ExprKind::Super { .. } => Vec::new(),
        ExprKind::List(items) => items.iter().collect(),
        ExprKind::Dict(pairs) => pairs.iter().flat_map(|(k, v)| [k, v]).collect(),
        ExprKind::Unary { rhs, .. } => vec![rhs],
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logical { lhs, rhs, .. } => vec![lhs, rhs],
        ExprKind::Call { callee, args } => std::iter::once(callee.as_ref()).chain(args).collect(),
        ExprKind::Index { target, index } => vec![target, index],
        ExprKind::Slice { target, start, end } => std::iter::once(target.as_ref())
            .chain([start, end].into_iter().flatten().map(|bound| bound.as_ref()))
            .collect(),
        ExprKind::Field { target, .. } => vec![target],
        ExprKind::Assign { target, value } => vec![target, value],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Infers over a program, which must compile — the pass is allowed to be
    /// handed a broken tree by the language server, but a test asserting an
    /// answer should not be quietly asserting it about a parse error.
    fn types(src: &str) -> Types {
        let program = crate::compile(src).expect("the test program compiles");
        infer(&program)
    }

    /// What `name` holds at the end of the file, which is where an editor asks
    /// from — so a test that passes here is a test about the thing that ships.
    fn of(src: &str, name: &str) -> Type {
        types(src).of_name(name, src.len() as u32)
    }

    fn class_of(src: &str, name: &str) -> Option<String> {
        of(src, name).class_name().map(str::to_string)
    }

    /// The offset of `needle` in `src`, for asking a question from inside a
    /// scope rather than at the end of the file.
    fn at(src: &str, needle: &str) -> u32 {
        src.find(needle).expect("the marker is in the program") as u32
    }

    #[test]
    fn a_literal_is_its_own_type() {
        assert_eq!(class_of("let a = 1", "a").as_deref(), Some("int"));
        assert_eq!(class_of("let a = 1.5", "a").as_deref(), Some("float"));
        assert_eq!(class_of("let a = \"hi\"", "a").as_deref(), Some("string"));
        assert_eq!(class_of("let a = true", "a").as_deref(), Some("bool"));
        assert_eq!(class_of("let a = nil", "a").as_deref(), Some("nil"));
        assert_eq!(class_of("let a = [1, 2]", "a").as_deref(), Some("list"));
        assert_eq!(class_of("let a = {\"k\": 1}", "a").as_deref(), Some("dict"));
    }

    #[test]
    fn a_constructor_call_makes_one_of_the_class_it_names() {
        let src = "class Point {\n  op init(x) { self.x = x }\n}\nlet p = Point(1)\n";
        assert_eq!(class_of(src, "p").as_deref(), Some("Point"));
    }

    #[test]
    fn a_class_name_holds_a_class_and_not_an_instance() {
        // The capital-letter heuristic answers `Point` here and is wrong. The
        // distinction is a reason the pass exists: `Point` is a value of type
        // `class`, and only `Point()` is a `Point`.
        let src = "class Point {\n  op init() { self.x = 1 }\n}\n";
        assert_eq!(class_of(src, "Point").as_deref(), Some("class"));
    }

    #[test]
    fn a_lowercase_class_is_still_a_class() {
        // The other half of the same point: the heuristic decides by spelling,
        // and this decides by declaration.
        let src = "class point {\n  op init() { self.x = 1 }\n}\nlet p = point()\n";
        assert_eq!(class_of(src, "p").as_deref(), Some("point"));
    }

    #[test]
    fn a_conversion_produces_the_type_it_names() {
        assert_eq!(class_of("let a = int(\"4\")", "a").as_deref(), Some("int"));
        assert_eq!(class_of("let a = string(4)", "a").as_deref(), Some("string"));
        assert_eq!(class_of("let a = list(\"ab\")", "a").as_deref(), Some("list"));
    }

    #[test]
    fn a_function_is_what_its_returns_agree_on() {
        let src = "fn pick(c) {\n  if c { return 1 }\n  return 2\n}\nlet a = pick(true)\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("int"));
    }

    #[test]
    fn a_function_whose_returns_disagree_is_unknown() {
        let src = "fn pick(c) {\n  if c { return 1 }\n  return \"two\"\n}\nlet a = pick(true)\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_function_that_returns_nothing_returns_nil() {
        let src = "fn shout(x) { print(x) }\nlet a = shout(1)\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("nil"));
    }

    #[test]
    fn a_bare_return_is_a_nil_that_joins() {
        // What makes `return` with no value worth handling rather than skipping:
        // skipping it would call this function an int.
        let src = "fn maybe(c) {\n  if c { return }\n  return 1\n}\nlet a = maybe(true)\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_recursive_function_does_not_hang_the_pass() {
        let src =
            "fn down(n) {\n  if n <= 0 { return 0 }\n  return down(n - 1)\n}\nlet a = down(3)\n";
        // The recursive arm carries no information, so the two returns
        // disagree. Answering `Unknown` is the point; not answering at all
        // would be the bug.
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_method_return_is_read_through_the_receiver() {
        let src = "class Box {\n  op init() { self.n = 1 }\n  fn size() { return 2 }\n}\nlet b = Box()\nlet a = b.size()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("int"));
    }

    #[test]
    fn a_method_named_but_not_called_is_a_function() {
        let src = "class Box {\n  op init() { self.n = 1 }\n  fn size() { return 2 }\n}\nlet b = Box()\nlet a = b.size\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("function"));
    }

    #[test]
    fn a_field_is_what_the_class_assigns_to_it() {
        let src = "class Point {\n  op init() { self.x = 1 }\n}\nlet p = Point()\nlet a = p.x\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("int"));
    }

    #[test]
    fn a_field_assigned_two_types_is_unknown() {
        let src = "class Wobble {\n  op init() { self.v = 1 }\n  fn reset() { self.v = \"\" }\n}\nlet w = Wobble()\nlet a = w.v\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_field_is_found_on_the_parent() {
        let src = "class Base {\n  op init() { self.tag = \"b\" }\n}\nclass Kid extends Base {\n  op init() { super.init() }\n}\nlet k = Kid()\nlet a = k.tag\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("string"));
    }

    #[test]
    fn a_self_referential_field_does_not_hang_the_pass() {
        let src = "class Node {\n  op init() { self.next = Node() }\n}\nlet n = Node()\n";
        assert_eq!(class_of(src, "n").as_deref(), Some("Node"));
    }

    #[test]
    fn self_is_the_class_whose_body_it_is_in() {
        let src = "class Point {\n  op init() { self.x = 1 }\n  fn me() { return self }\n}\nlet p = Point()\nlet a = p.me()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("Point"));
    }

    #[test]
    fn super_reaches_the_parent_class() {
        let src = "class Base {\n  op init() { self.n = 1 }\n  fn tag() { return \"b\" }\n}\nclass Kid extends Base {\n  op init() { super.init() }\n  fn mine() { return super.tag() }\n}\nlet k = Kid()\nlet a = k.mine()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("string"));
    }

    #[test]
    fn a_parameter_carries_no_information() {
        let src = "fn f(x) { return x }\nlet a = f(1)\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn the_operators_the_language_decides_are_decided() {
        assert_eq!(class_of("let a = 1 + 2", "a").as_deref(), Some("int"));
        assert_eq!(class_of("let a = 1 / 2", "a").as_deref(), Some("float"));
        assert_eq!(class_of("let a = 7 // 2", "a").as_deref(), Some("int"));
        assert_eq!(class_of("let a = 1 + 2.0", "a").as_deref(), Some("float"));
        assert_eq!(class_of("let a = \"x\" + \"y\"", "a").as_deref(), Some("string"));
        assert_eq!(class_of("let a = [1] + [2]", "a").as_deref(), Some("list"));
        assert_eq!(class_of("let a = 1 < 2", "a").as_deref(), Some("bool"));
        assert_eq!(class_of("let a = !1", "a").as_deref(), Some("bool"));
        assert_eq!(class_of("let a = -1", "a").as_deref(), Some("int"));
    }

    #[test]
    fn an_operator_a_class_answers_for_is_unknown() {
        // `op add` may return anything at all, so `m + m` is not a `Money`
        // because the operands were. Assuming otherwise is the kind of guess
        // that is right often enough to be trusted and wrong without warning.
        let src = "class Money {\n  op init(c) { self.c = c }\n  op add(o) { return self.c }\n}\nlet m = Money(1)\nlet a = m + m\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_comparison_on_a_class_is_still_a_bool() {
        // Because the evaluator makes it one: whatever `op cmp` returns is read
        // for its sign and turned into a bool before anyone sees it.
        let src = "class Money {\n  op init(c) { self.c = c }\n  op cmp(o) { return self.c - o.c }\n}\nlet m = Money(1)\nlet a = m < m\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("bool"));
    }

    #[test]
    fn indexing_says_only_what_it_can() {
        assert_eq!(class_of("let a = \"abc\"[0]", "a").as_deref(), Some("string"));
        assert_eq!(of("let a = [1, 2][0]", "a"), Type::Unknown);
        assert_eq!(class_of("let a = [1, 2][0:1]", "a").as_deref(), Some("list"));
        assert_eq!(class_of("let a = \"abc\"[1:]", "a").as_deref(), Some("string"));
    }

    #[test]
    fn a_loop_variable_takes_the_elements_it_can_see() {
        let src = "for n in [1, 2] { print(n) }\n";
        assert_eq!(types(src).of_name("n", at(src, "print")).class_name(), Some("int"));

        let src = "for n in [1, \"two\"] { print(n) }\n";
        assert_eq!(types(src).of_name("n", at(src, "print")), Type::Unknown);

        let src = "let xs = [1, 2]\nfor n in xs { print(n) }\n";
        // A list is not a `list[T]`, so what is in one held by a name is not
        // written down anywhere for this to read.
        assert_eq!(types(src).of_name("n", at(src, "print")), Type::Unknown);
    }

    #[test]
    fn a_name_assigned_a_second_type_holds_neither() {
        let src = "let a = 1\na = \"one\"\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_name_assigned_the_same_type_keeps_it() {
        let src = "let a = 1\na = 2\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("int"));
    }

    #[test]
    fn a_local_does_not_answer_for_a_name_outside_it() {
        let src = "fn f() {\n  let x = 1\n  return x\n}\nlet y = 2\n";
        // `x` is gone by the end of the file, and nothing is offered in its
        // place.
        assert_eq!(of(src, "x"), Type::Unknown);
    }

    #[test]
    fn the_innermost_binding_wins() {
        let src = "let x = 1\nfn f() {\n  let x = \"inner\"\n  print(x)\n}\n";
        let types = types(src);
        assert_eq!(types.of_name("x", at(src, "print")).class_name(), Some("string"));
        assert_eq!(types.of_name("x", src.len() as u32).class_name(), Some("int"));
    }

    #[test]
    fn a_name_is_not_known_before_it_is_bound() {
        let src = "let a = 1\nlet b = 2\n";
        assert_eq!(types(src).of_name("b", 0), Type::Unknown);
    }

    #[test]
    fn a_function_declared_below_is_still_callable_above() {
        // The forward reference the resolver goes out of its way to allow. A
        // pass reading the file top to bottom would answer `Unknown` here.
        let src = "let a = two()\nfn two() { return 2 }\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("int"));
    }

    #[test]
    fn an_imported_module_is_a_module() {
        let src = "import math\n";
        assert_eq!(of(src, "math"), Type::Module("math".to_string()));
    }

    #[test]
    fn a_module_constant_is_what_building_it_produces() {
        let src = "import math\nlet a = math.pi\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("float"));
    }

    #[test]
    fn a_module_function_is_a_function() {
        let src = "from math import floor\n";
        assert_eq!(class_of(src, "floor").as_deref(), Some("function"));
    }

    #[test]
    fn a_native_is_what_it_says_it_returns() {
        // The case the whole `returns` field exists for. `split` crosses from a
        // string to a list, and nothing about the line it is written on says so.
        assert_eq!(
            class_of("let a = \"x,y\".split(\",\")", "a").as_deref(),
            Some("list")
        );
        assert_eq!(class_of("let a = len(\"ab\")", "a").as_deref(), Some("int"));
        assert_eq!(class_of("let a = type(1)", "a").as_deref(), Some("string"));
        assert_eq!(
            class_of("import math\nlet a = math.floor(2.5)", "a").as_deref(),
            Some("int")
        );
        assert_eq!(
            class_of("let s = \", \"\nlet a = s.join([\"x\"])", "a").as_deref(),
            Some("string")
        );
    }

    #[test]
    fn a_native_that_does_not_say_is_still_unknown() {
        // `abs` keeps the type it was handed and `dict.get` answers with
        // whatever was stored. A table cannot say what those are, and the field
        // being allowed to decline is what keeps the rest of it trustworthy.
        assert_eq!(of("import math\nlet a = math.abs(-1)", "a"), Type::Unknown);
        assert_eq!(of("let a = {\"k\": 1}.get(\"k\", 0)", "a"), Type::Unknown);
        assert_eq!(of("import io\nlet a = io.line()", "a"), Type::Unknown);
    }

    #[test]
    fn a_class_extending_a_builtin_inherits_what_its_methods_return() {
        let src = "class Stack extends list {\n  op init() { super.init() }\n}\nlet s = Stack()\nlet a = s.sort()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("list"));
    }

    #[test]
    fn a_method_a_program_wrote_beats_the_table_it_inherited() {
        // Dispatch asks the class first, so inference has to as well — a
        // `sort` written here is not the builtin's `sort`.
        let src = "class Odd extends list {\n  op init() { super.init() }\n  fn sort() { return \"nope\" }\n}\nlet o = Odd()\nlet a = o.sort()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("string"));
    }

    #[test]
    fn an_extension_on_a_builtin_is_found_beside_its_own_methods() {
        let src = "extend list {\n  fn second() { return self[1] }\n}\nlet xs = [1, 2]\n";
        let types = types(src);
        let end = src.len() as u32;
        // The one the extension added, and the one the builtin already had.
        assert_eq!(types.of_path("xs.second", end).class_name(), Some("function"));
        assert_eq!(types.of_path("xs.sort()", end).class_name(), Some("list"));
    }

    #[test]
    fn a_name_a_module_does_not_declare_is_not_guessed_at() {
        let src = "import math\nlet a = math.nosuch\n";
        assert_eq!(of(src, "a"), Type::Unknown);
    }

    #[test]
    fn a_dotted_path_is_followed_a_segment_at_a_time() {
        let src = "class Inner {\n  op init() { self.n = 1 }\n}\nclass Outer {\n  op init() { self.inner = Inner() }\n}\nlet o = Outer()\n";
        let types = types(src);
        let end = src.len() as u32;
        assert_eq!(types.of_path("o", end).class_name(), Some("Outer"));
        assert_eq!(types.of_path("o.inner", end).class_name(), Some("Inner"));
        assert_eq!(types.of_path("o.inner.n", end).class_name(), Some("int"));
        assert_eq!(types.of_path("o.inner.nope", end), Type::Unknown);
    }

    #[test]
    fn a_path_tells_a_call_from_a_name() {
        // The parentheses are the whole of the difference, which is why the
        // caller has to keep them: `Box` is a class, `Box()` is a `Box`,
        // `b.twin` is the method, and `b.twin()` is another `Box`.
        let src = "class Box {\n  op init() { self.n = 1 }\n  fn twin() { return Box() }\n}\nlet b = Box()\n";
        let types = types(src);
        let end = src.len() as u32;
        // A class object reaches what its instances have, because the
        // language lets it: `print(Box.twin)` writes `<fn twin>`. What it does
        // not reach is a field, which only an instance ever assigned.
        assert_eq!(types.of_path("Box", end).class_name(), Some("Box"));
        assert!(types.names_a_class("Box", end));
        assert!(!types.names_a_class("Box()", end));
        assert!(!types.names_a_class("b", end));
        assert_eq!(types.of_path("Box()", end).class_name(), Some("Box"));
        assert_eq!(types.of_path("b.twin", end).class_name(), Some("function"));
        assert_eq!(types.of_path("b.twin()", end).class_name(), Some("Box"));
        assert_eq!(types.of_path("b.twin().n", end).class_name(), Some("int"));
    }

    #[test]
    fn a_path_through_a_function_call_is_followed_too() {
        let src = "class Box {\n  op init() { self.n = 1 }\n}\nfn make() { return Box() }\n";
        let types = types(src);
        let end = src.len() as u32;
        assert_eq!(types.of_path("make()", end).class_name(), Some("Box"));
        assert_eq!(types.of_path("make().n", end).class_name(), Some("int"));
        assert_eq!(types.of_path("string()", end).class_name(), Some("string"));
    }

    #[test]
    fn an_extension_adds_methods_the_pass_can_see() {
        let src = "class Box {\n  op init() { self.n = 1 }\n}\nextend Box {\n  fn tag() { return \"box\" }\n}\nlet b = Box()\nlet a = b.tag()\n";
        assert_eq!(class_of(src, "a").as_deref(), Some("string"));
    }

    #[test]
    fn a_cycle_in_what_was_written_does_not_hang_the_walk() {
        // `extends` cycles are refused at run time rather than by the resolver,
        // so this pass can be handed one. The same guard the resolver's own
        // walk needed, for the same reason.
        let src = "class A extends B {\n  fn a() { return 1 }\n}\nclass B extends A {\n  fn b() { return 2 }\n}\n";
        let types = types(src);
        assert_eq!(types.of_field("A", "nothing"), Type::Unknown);
        assert!(types.has_method("A", "b"));
        assert!(!types.has_method("A", "nothing"));
    }

    #[test]
    fn every_builtin_that_can_be_called_names_a_type() {
        // The constructors are read off `BUILTINS` rather than listed here, so
        // this pins that the reading agrees with the list: a type that can be
        // called is a type a call produces, and `nil` and `class` — keywords,
        // and so uncallable — are neither.
        for builtin in BUILTINS {
            let expected = builtin.conversion().is_some().then_some(builtin.name());
            assert_eq!(
                builtin_constructor(builtin.name()),
                expected,
                "{}",
                builtin.name()
            );
        }
        assert_eq!(builtin_constructor("nil"), None);
        assert_eq!(builtin_constructor("Point"), None);
    }
}

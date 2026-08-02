//! A name, and everything an editor needs in order to draw it.
//!
//! [`Symbol`] is the one thing the language server and the REPL both ask for. Before it
//! existed each of them walked the source for itself and reached its own
//! conclusions, which is how the editor came to complete a list's methods on a
//! string.
//!
//! The tables here are read off the interpreter's own — [`globals`] off
//! `BUILTINS`, [`module_symbols`] off `stdlib::MODULES` — rather than kept as a
//! second copy, which is the rule these were put under when `bool` turned out to
//! be missing from a hand-written list of them.

use std::rc::Rc;

use crate::builtins::stdlib;
use crate::runtime::class::BUILTINS;
use crate::runtime::value::Native;
use crate::sema::infer::infer;
use crate::sema::types::{Type, of_value, returned_by};
use crate::syntax::ast::{FnDecl, Visibility};
use crate::syntax::doc::Doc;


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
/// The one thing the language server and the REPL both ask for. Before this existed each
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
    /// How far the declaration reaches.
    ///
    /// Here rather than only in the runtime class, because this is what both
    /// editing surfaces render and a completion list that offers what the
    /// language will refuse is worse than one that offers nothing. Every symbol
    /// that was not declared with a word is [`Visibility::Public`], which is
    /// what a builtin's method and an inferred field both are.
    pub visibility: Visibility,
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
            visibility: Visibility::Public,
        }
    }

    pub(crate) fn declared_with(mut self, keyword: &'static str) -> Symbol {
        self.keyword = Some(keyword);
        self
    }

    /// Records how far the declaration reaches.
    pub(crate) fn reaching(mut self, visibility: Visibility) -> Symbol {
        self.visibility = visibility;
        self
    }

    pub(crate) fn returning(mut self, returns: Type) -> Symbol {
        self.returns = returns;
        self
    }

    pub(crate) fn with_doc(mut self, doc: Option<Doc>) -> Symbol {
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

/// Adds a symbol unless a name has already answered for it.
///
/// First wins, and the walk goes in dispatch order — so a method a subclass
/// overrode is offered once, as the one that would run.
pub(crate) fn push_once(found: &mut Vec<Symbol>, symbol: Symbol) {
    if !found.iter().any(|seen| seen.name == symbol.name) {
        found.push(symbol);
    }
}

/// A symbol for a function or method the program declared.
///
/// Its parameters are the ones someone wrote, so the receiver a method carries
/// is left out — `self` is not a name a caller types.
pub(crate) fn symbol_for(decl: &Rc<FnDecl>, kind: Kind, returns: Type) -> Symbol {
    Symbol {
        name: decl.name.clone(),
        kind,
        ty: Type::class("function"),
        returns,
        doc: decl.doc.clone(),
        keyword: None,
        visibility: decl.visibility,
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
pub(crate) fn described_by(decl: &Rc<FnDecl>, param: &str) -> Option<Doc> {
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
        // A native carries no visibility word; the tables have none to write.
        visibility: Visibility::Public,
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
    let mut found: Vec<Symbol> = crate::builtins::BUILTINS
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
pub(crate) fn error_classes() -> &'static [Symbol] {
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
pub(crate) fn module_native(module: &str, name: &str) -> Option<&'static Native> {
    let module = stdlib::module_named(module)?;
    module.members.iter().find_map(|(member, kind)| match kind {
        stdlib::Member::Fn(native) if *member == name => Some(*native),
        _ => None,
    })
}

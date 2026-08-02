//! Loading a module, and reporting when it cannot be loaded.
//!
//! Both kinds: the static tables the language ships, and a `.qn` file beside the
//! importer. The cycle detection, and the per-module source registry that makes an
//! error raised inside an import point at the right file, live here.
//!
//! v0.7's module visibility is a run-time check and this is where it goes — a file
//! module's exports are not known until the file has run.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::builtins::{BUILTINS, stdlib};
use crate::error::{ErrorKind, ModuleSource, QuinceError};
use crate::interp::{Interp, ModuleState, file_name};
use crate::runtime::class::BUILTINS as BUILTIN_TYPES;
use crate::runtime::env::{self, Globals};
use crate::runtime::heap::{ObjId, Object};
use crate::runtime::value::Value;
use crate::syntax::token::{Span, TokenKind};

impl Interp {
    /// The scope of the module called `name`, loading it if this is the first
    /// time it has been asked for.
    ///
    /// The stdlib is searched first and a file second, which is the rule that
    /// keeps `import math` meaning one thing: a file appearing in a directory
    /// must not quietly take over a name the language ships. The reserved set is
    /// small, fixed, and listed in `stdlib::MODULES`, which is what makes that a
    /// rule someone can hold in their head rather than a trap.
    pub(super) fn load_module(&mut self, name: &str, env: ObjId, span: Span) -> Result<ObjId, QuinceError> {
        if let Some(id) = self.stdlib_modules.get(name) {
            return Ok(*id);
        }
        if let Some(module) = stdlib::module_named(name) {
            let id = stdlib::build(module, &mut self.heap);
            self.stdlib_modules.insert(module.name, id);
            return Ok(id);
        }

        // A name reaching for anywhere but the importer's own directory is
        // refused by the parser, which sees the `/` or the `.` that this never
        // gets handed. Subdirectories, a search path, and packages are each a
        // decision that wants a language with modules already in use.
        let path = self.resolve_import(name, env, span)?;
        match self.files.get(&path) {
            Some(ModuleState::Loaded(id)) => return Ok(*id),
            Some(ModuleState::Loading(_)) => return Err(self.import_cycle(&path, span)),
            None => {}
        }

        let text = std::fs::read_to_string(&path).map_err(|err| {
            QuinceError::new(
                format!("could not read `{}`: {err}", path.display()),
                span,
            )
            .with_kind(ErrorKind::Import)
        })?;
        self.run_module(name, path, text)
    }

    /// Where `import name` should look, which is beside the file doing the
    /// importing.
    ///
    /// Relative to the importer and not to the working directory, so a program
    /// runs the same from anywhere. The REPL has no file, so it resolves against
    /// the working directory — the only sensible answer for input that came from
    /// a terminal.
    pub(super) fn resolve_import(&self, name: &str, env: ObjId, span: Span) -> Result<PathBuf, QuinceError> {
        let importer = env::module_of(&self.heap, env);
        let base = match self.heap.globals(importer).path() {
            Some(path) => path.parent().map(Path::to_path_buf).unwrap_or_default(),
            None => PathBuf::new(),
        };
        let candidate = base.join(format!("{name}.qn"));
        if !candidate.is_file() {
            let mut err = QuinceError::new(format!("there is no module called `{name}`"), span)
                .with_kind(ErrorKind::Import);
            let names: Vec<&str> = stdlib::MODULES.iter().map(|m| m.name).collect();
            err = match crate::error::did_you_mean(name, names) {
                Some(suggestion) => err.with_help(format!("did you mean `{suggestion}`?")),
                None => err.with_help(format!(
                    "no module the language ships is called `{name}`, and there is no \
                     `{name}.qn` beside this file"
                )),
            };
            return Err(err);
        }
        // Canonicalised so two spellings of one file are one module. Without it
        // `import util` from two directories that resolve to the same file would
        // run it twice and produce two of everything it declares.
        candidate.canonicalize().map_err(|err| {
            QuinceError::new(
                format!("could not read `{}`: {err}", candidate.display()),
                span,
            )
            .with_kind(ErrorKind::Import)
        })
    }

    /// Compiles and runs a file as a module, and hands back its scope.
    pub(super) fn run_module(&mut self, name: &str, path: PathBuf, text: String) -> Result<ObjId, QuinceError> {
        let source = Rc::new(ModuleSource {
            // The file's name and not the path it was found at, so a report is
            // the same on every machine. The starting file already has this
            // property and the corpus depends on it.
            path: file_name(&path),
            text: Rc::from(text.as_str()),
        });

        // Compiled before anything is registered, so a module that does not
        // parse leaves no half-built entry for a later import to find.
        let program = crate::compile(&text).map_err(|err| {
            // Named as imported when the diagnostic has nothing else to say,
            // because a syntax error in a file the reader did not know was being
            // loaded is otherwise a report about a file they did not open. Never
            // over advice the diagnostic already carries — what to write instead
            // is worth more than where it came from.
            let err = match err.help.is_none() {
                true => err.with_help(format!("this file was reached by `import {name}`")),
                false => err,
            };
            err.in_module(Rc::clone(&source))
        })?;

        let globals = self.new_module_globals(name, Some(path.clone()));
        self.module_sources.insert(globals, Rc::clone(&source));
        self.files
            .insert(path.clone(), ModuleState::Loading(globals));
        self.loading.push(path.clone());

        // Its statements run in its own scope, which is what makes every name it
        // declares its own — and what makes `module_of` answer with this scope
        // for every function it declares, for as long as those functions live.
        let mut result = Ok(());
        for stmt in &program {
            if let Err(err) = self.exec(stmt, globals) {
                result = Err(err.in_module(Rc::clone(&source)));
                break;
            }
        }

        self.loading.pop();
        match result {
            Ok(()) => {
                self.files.insert(path, ModuleState::Loaded(globals));
                Ok(globals)
            }
            Err(err) => {
                // Removed rather than left `Loading`: the import failed, and a
                // second attempt should fail the same way rather than be told it
                // is a cycle.
                self.files.remove(&path);
                Err(err)
            }
        }
    }

    /// A scope holding everything a module starts with.
    ///
    /// The error classes are the *same* objects the starting module holds, not
    /// fresh ones. A `catch TypeError` in one file has to catch a `TypeError`
    /// raised in another, and `catch` compares the class it was given against
    /// the one the error reified into — so two modules with two `TypeError`
    /// classes would give a handler that silently never fires. Re-running the
    /// prelude per module would have done exactly that, and cost a compile each
    /// time to do it.
    pub(super) fn new_module_globals(&mut self, name: &str, path: Option<PathBuf>) -> ObjId {
        let mut globals = Globals::module(name, path);
        for native in BUILTINS {
            globals.declare(native.name, Value::Native(native), false);
        }
        for builtin in BUILTIN_TYPES {
            let type_name = builtin.name();
            if TokenKind::keyword(type_name).is_some() {
                continue;
            }
            globals.declare(
                type_name,
                Value::Class(self.heap.builtin_class(*builtin)),
                false,
            );
        }
        // Taken from the starting module rather than from `error_classes`, so
        // that a name and the class under it cannot disagree.
        for (class_name, value) in self.error_class_bindings() {
            globals.declare(class_name, value, false);
        }
        self.heap.alloc(Object::Globals(globals))
    }

    /// The `Error` classes as name/value pairs, read off the starting module.
    pub(super) fn error_class_bindings(&self) -> Vec<(String, Value)> {
        let mut bindings = vec![(
            "Error".to_string(),
            self.heap
                .globals(self.globals)
                .get("Error")
                .expect("the prelude declares `Error`")
                .clone(),
        )];
        for (kind, id) in &self.error_classes {
            if let Some(class_name) = kind.class_name() {
                bindings.push((class_name.to_string(), Value::Class(*id)));
            }
        }
        bindings
    }

    /// The text of the module `env` bottoms out in, if it is one that was loaded
    /// from a file.
    pub(super) fn module_source(&self, env: ObjId) -> Option<Rc<ModuleSource>> {
        self.module_sources
            .get(&env::module_of(&self.heap, env))
            .cloned()
    }

    /// `a.qn` imports `b.qn` imports `a.qn`, reported as the path it took.
    pub(super) fn import_cycle(&self, path: &Path, span: Span) -> QuinceError {
        let start = self
            .loading
            .iter()
            .position(|loading| loading == path)
            .unwrap_or(0);
        let mut chain: Vec<String> = self.loading[start..]
            .iter()
            .map(|path| file_name(path))
            .collect();
        chain.push(file_name(path));

        // True of a file that imports itself and of one reached the long way
        // round, which "imports itself" is not — the chain below is what says
        // which of the two happened.
        QuinceError::new(
            format!(
                "`{}` is imported before it has finished loading: {}",
                file_name(path),
                chain.join(" → ")
            ),
            span,
        )
        .with_kind(ErrorKind::Import)
        .with_help(
            "a module is loaded once, and a cycle has no order that could do that — move what \
             both files need into a third",
        )
    }

    /// A name a module does not declare, reached either way it can be asked for.
    ///
    /// `math.florr` and `from math import florr` are the same mistake and get the
    /// same sentence, which is worth arranging deliberately: the two spellings
    /// reach the same lookup, and `no_attr` would otherwise report the first as
    /// "module has no method `florr`" — naming the type rather than the module,
    /// and calling a name a method when a module has none.
    ///
    /// [`ErrorKind::Attr`] because that is what this is: a scope that exists,
    /// asked for something it does not have.
    pub(super) fn not_in_module(&self, module: &str, name: &str, span: Span, loaded: ObjId) -> QuinceError {
        let mut err = QuinceError::new(format!("`{module}` declares nothing called `{name}`"), span)
            .with_kind(ErrorKind::Attr);
        let declared: Vec<String> = self
            .heap
            .globals(loaded)
            .iter()
            .map(|(key, _)| key.to_string())
            .collect();
        let refs: Vec<&str> = declared.iter().map(|s| s.as_str()).collect();
        if let Some(suggestion) = crate::error::did_you_mean(name, refs) {
            err = err.with_help(format!("did you mean `{suggestion}`?"));
        }
        err
    }
}

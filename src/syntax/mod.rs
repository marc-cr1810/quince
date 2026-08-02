//! Source text to AST.
//!
//! The front of the pipeline, and the only part of the compiler that ever sees
//! characters. [`lexer`] turns text into [`token`]s, [`parser`] turns those into
//! [`ast`], and [`doc`] reads the `##` blocks the lexer attached along the way.
//!
//! Grouped because they change together: a new keyword is a row in `token`, an
//! arm in `lexer`, a production in `parser`, and a node in `ast`, and every
//! milestone from v0.7 on adds several. Nothing downstream of here — the
//! resolver, the inference pass, the evaluator — should ever need to reach past
//! `ast` into the two files that built it.

pub mod ast;
pub mod doc;
pub mod lexer;
pub mod parser;
pub mod token;

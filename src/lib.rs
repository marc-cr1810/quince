//! Quince: a dynamically typed scripting language.
//!
//! The pipeline runs left to right and each stage is a directory:
//!
//! ```text
//! source (.qn)
//!   → syntax      tokens, then an AST
//!   → sema        names resolved to slots, and what the pass can work out about types
//!   → interp      the tree-walking evaluator, over runtime's object model
//! ```
//!
//! [`runtime`] is what the evaluator computes with, [`builtins`] is the library it
//! computes with, [`error`] is what any of them raises, and the two editing
//! surfaces — the REPL and the language server — live in the binary beside
//! `main.rs` because they are how the language is *used* rather than part of it.

pub mod builtins;
pub mod color;
pub mod error;
pub mod interp;
pub mod runtime;
pub mod sema;
pub mod syntax;

use crate::error::Result;
use crate::syntax::ast::Stmt;
use crate::syntax::lexer::Lexer;
use crate::syntax::parser::Parser;

/// Lexes, parses, and resolves a whole source file.
pub fn compile(source: &str) -> Result<Vec<Stmt>> {
    compile_tokens(Lexer::new(source).tokenize()?)
}

/// Everything after lexing.
///
/// Split out so `--dump tokens` can print the token stream without the caller
/// having to reproduce the rest of the pipeline — which is how the resolver
/// came to be missing from the binary while every test still passed.
pub fn compile_tokens(
    tokens: Vec<crate::syntax::token::Token>,
) -> Result<Vec<Stmt>> {
    let mut program = Parser::new(tokens).parse()?;
    sema::resolve::resolve(&mut program)?;
    Ok(program)
}

pub mod ast;
pub mod class;
pub mod dict;
pub mod env;
pub mod error;
pub mod heap;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod resolver;
pub mod token;
pub mod value;

use crate::ast::Stmt;
use crate::error::QuinceError;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Lexes, parses, and resolves a whole source file.
pub fn compile(source: &str) -> Result<Vec<Stmt>, QuinceError> {
    compile_tokens(Lexer::new(source).tokenize()?)
}

/// Everything after lexing.
///
/// Split out so `--dump tokens` can print the token stream without the caller
/// having to reproduce the rest of the pipeline — which is how the resolver
/// came to be missing from the binary while every test still passed.
pub fn compile_tokens(tokens: Vec<crate::token::Token>) -> Result<Vec<Stmt>, QuinceError> {
    let mut program = Parser::new(tokens).parse()?;
    resolver::resolve(&mut program)?;
    Ok(program)
}

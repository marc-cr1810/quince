pub mod ast;
pub mod env;
pub mod error;
pub mod heap;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod token;
pub mod value;

use crate::ast::Stmt;
use crate::error::QuinceError;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// Lexes and parses a whole source file.
pub fn compile(source: &str) -> Result<Vec<Stmt>, QuinceError> {
    let tokens = Lexer::new(source).tokenize()?;
    Parser::new(tokens).parse()
}

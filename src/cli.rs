use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "quince",
    version,
    about = "A dynamically-typed scripting language"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a Quince source file
    Run {
        /// Path to a .qn file
        file: PathBuf,
        /// Stop after a stage and print what it produced
        #[arg(long, value_name = "STAGE")]
        dump: Option<Dump>,
    },
    /// Start an interactive session
    Repl,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Dump {
    Tokens,
    Ast,
}

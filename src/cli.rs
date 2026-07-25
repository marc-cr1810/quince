use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use quince::color::ColorChoice;

#[derive(Parser)]
#[command(
    name = "quince",
    version,
    about = "A dynamically-typed scripting language"
)]
pub struct Cli {
    /// When to use colored output [auto, always, never]
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

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

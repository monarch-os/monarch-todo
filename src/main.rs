//! monarch-todo — fast todo manager for Monarch.
//!
//! No subcommand → the TUI. A subcommand → the scriptable CLI. Both operate on
//! the same JSON store as the rest of Monarch.

mod cli;
mod store;
mod tui;

use clap::Parser;

fn main() {
    let args = cli::Cli::parse();
    let result = match args.command {
        Some(command) => cli::run(command),
        None => tui::run(),
    };
    if let Err(e) = result {
        eprintln!("monarch-todo: {e:#}");
        std::process::exit(1);
    }
}

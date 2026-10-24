//! monarch-todo — fast todo manager for Monarch.
//!
//! Noctalia and shell scripts share this backend and the same locked store.

mod cli;
mod desktop;
mod input;
mod reminder;
mod store;

use clap::Parser;

fn main() {
    let args = cli::Cli::parse();
    let result = match args.command {
        Some(command) => cli::run(command),
        None => {
            use clap::CommandFactory;
            let _ = cli::Cli::command().print_help();
            println!();
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("monarch-todo: {e:#}");
        std::process::exit(1);
    }
}

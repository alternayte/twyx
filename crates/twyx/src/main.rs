//! The `twyx` binary: CLI, TUI client, and the hidden daemon and holder
//! processes.

mod cli;
mod integrations;
mod projects;
mod recipes;
mod tasks;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("twyx: {error:#}");
        std::process::exit(1);
    }
}

//! The `twyx` binary: CLI, TUI client, and the hidden daemon and holder
//! processes.

mod cli;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("twyx: {error:#}");
        std::process::exit(1);
    }
}

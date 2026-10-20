//! The `twyx` binary: CLI, TUI client, and the hidden daemon and holder
//! processes.

/// Prints a line with every control character but tabs written as an
/// escape: repository and pane text must not drive the terminal.
macro_rules! say {
    ($($arg:tt)*) => {
        println!("{}", twyx_core::text::printable(&format!($($arg)*)))
    };
}

mod checks;
mod cli;
mod integrations;
mod projects;
mod recipes;
mod review;
mod tasks;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!(
            "twyx: {}",
            twyx_core::text::printable(&format!("{error:#}"))
        );
        std::process::exit(1);
    }
}

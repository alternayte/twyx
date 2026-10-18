//! Command line parsing and dispatch.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use twyx_core::pane::PaneId;
use twyx_holder::engine::{Engine, Geometry};
use twyx_holder::pty::Spawn;

#[derive(Debug, Parser)]
#[command(name = "twyx", version, about = "A developer workspace")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one pane holder. The daemon starts these; running one by hand has
    /// no daemon to adopt it.
    #[command(hide = true)]
    Holder {
        /// The pane this holder owns.
        #[arg(long)]
        pane: String,
        /// The socket the daemon connects to.
        #[arg(long)]
        socket: PathBuf,
        /// Columns of the pane.
        #[arg(long, default_value_t = 80)]
        cols: u16,
        /// Rows of the pane.
        #[arg(long, default_value_t = 24)]
        rows: u16,
        /// Directory the command starts in.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Lines of scrollback to keep.
        #[arg(long)]
        scrollback_lines: Option<u32>,
        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },
}

pub fn run() -> Result<()> {
    match Cli::parse().command {
        None => {
            println!("twyx {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(Command::Holder {
            pane,
            socket,
            cols,
            rows,
            cwd,
            scrollback_lines,
            argv,
        }) => {
            let pane: PaneId = pane.parse().context("pane id")?;
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().context("current directory")?,
            };
            let mut env = vec![
                ("TWYX_PANE_ID".to_owned(), pane.to_string()),
                (
                    "TWYX_SOCKET".to_owned(),
                    socket.to_string_lossy().into_owned(),
                ),
            ];
            env.push(("TERM".to_owned(), "xterm-256color".to_owned()));
            env.push(("COLORTERM".to_owned(), "truecolor".to_owned()));

            let spawn = Spawn {
                argv,
                cwd,
                env,
                cols,
                rows,
            };
            let scrollback_lines = scrollback_lines.unwrap_or_else(|| {
                twyx_holder::session::lines(twyx_core::tuning::PANE_SCROLLBACK_BYTES, cols)
            });

            // A holder outlives the daemon that started it, so it must not
            // share the daemon's session.
            twyx_holder::session::detach();

            let engine: Box<dyn Engine> =
                Box::new(twyx_holder::engine::alacritty::Alacritty::new(Geometry {
                    cols,
                    rows,
                    scrollback_lines,
                }));

            twyx_holder::runtime::run(
                twyx_holder::runtime::Config {
                    socket,
                    pane,
                    build: twyx_holder::runtime::build_id(),
                    spawn,
                    scrollback_lines,
                    exit_grace: twyx_core::tuning::EXIT_GRACE,
                },
                engine,
            )
        }
    }
}

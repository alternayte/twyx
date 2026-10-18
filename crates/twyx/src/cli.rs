//! Command line parsing and dispatch.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use twyx_core::pane::PaneId;
use twyx_daemon::paths::Paths;
use twyx_holder::engine::{Engine, Geometry};
use twyx_holder::pty::Spawn;
use twyx_protocol::client::ToDaemon;

use crate::client;

#[derive(Debug, Parser)]
#[command(name = "twyx", version, about = "A developer workspace")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start, stop and inspect the daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
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

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the daemon in the foreground.
    Run,
    /// Start the daemon in the background.
    Start,
    /// Stop the daemon. Panes keep running.
    Stop,
    /// Report whether the daemon runs.
    Status,
}

pub fn run() -> Result<()> {
    let Some(command) = Cli::parse().command else {
        println!("twyx {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };
    match command {
        Command::Daemon { command } => daemon(command),
        Command::Holder {
            pane,
            socket,
            cols,
            rows,
            cwd,
            scrollback_lines,
            argv,
        } => holder(pane, socket, cols, rows, cwd, scrollback_lines, argv),
    }
}

fn daemon(command: DaemonCommand) -> Result<()> {
    let paths = Paths::discover()?;
    match command {
        DaemonCommand::Run => {
            // The daemon outlives the shell that started it.
            let _ = rustix::process::setsid();
            twyx_daemon::daemon::run(paths)
        }
        DaemonCommand::Start => {
            client::ensure_daemon(&paths)?;
            let connection = client::connect(&paths, 80, 24)?;
            println!("daemon running, pid {}", connection.hello.pid);
            Ok(())
        }
        DaemonCommand::Stop => {
            if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
                println!("daemon stopped");
                return Ok(());
            }
            let mut connection = client::connect(&paths, 80, 24)?;
            connection.send(&ToDaemon::Shutdown)?;
            Ok(())
        }
        DaemonCommand::Status => {
            match client::connect(&paths, 80, 24) {
                Ok(connection) => println!(
                    "running pid {} with {} pane{}",
                    connection.hello.pid,
                    connection.hello.panes.len(),
                    if connection.hello.panes.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
                Err(_) => println!("stopped"),
            }
            Ok(())
        }
    }
}

fn holder(
    pane: String,
    socket: PathBuf,
    cols: u16,
    rows: u16,
    cwd: Option<PathBuf>,
    scrollback_lines: Option<u32>,
    argv: Vec<String>,
) -> Result<()> {
    let pane: PaneId = pane.parse().context("pane id")?;
    if argv.is_empty() {
        bail!("a pane needs a command");
    }
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().context("current directory")?,
    };
    let env = vec![
        ("TWYX_PANE_ID".to_owned(), pane.to_string()),
        (
            "TWYX_SOCKET".to_owned(),
            socket.to_string_lossy().into_owned(),
        ),
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ];

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

    // A holder outlives the daemon that started it, so it must not share the
    // daemon's session.
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

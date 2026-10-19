//! Command line parsing and dispatch.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use twyx_core::layout::Axis;
use twyx_core::pane::PaneId;
use twyx_core::paths::Paths;
use twyx_core::theme::Variant;
use twyx_holder::engine::Geometry;
use twyx_holder::pty::Spawn;
use twyx_protocol::client::{FromDaemon, Launch, PaneState, Place, ToDaemon};
use twyx_protocol::connection as client;

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
    /// Work with panes.
    Pane {
        #[command(subcommand)]
        command: PaneCommand,
    },
    /// Print what the outer terminal supports.
    Doctor,
    /// Show the theme specimen: the chrome over sample panes.
    Specimen {
        /// Paint a reference sheet instead of drawing on the terminal's own
        /// paper.
        #[arg(long, value_enum)]
        paper: Option<Paper>,
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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum PanePlace {
    /// Split the focused pane, the new one to the right.
    Right,
    /// Split the focused pane, the new one below.
    Down,
    /// Stack it with the focused pane.
    Stack,
    /// Float it over the tab.
    Float,
    /// Open it in a new tab.
    Tab,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Paper {
    Light,
    Dark,
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

#[derive(Debug, Subcommand)]
enum PaneCommand {
    /// Start a pane.
    New {
        /// Directory the command starts in.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Where the pane goes, next to the tab's focused pane.
        #[arg(long, value_enum, default_value_t = PanePlace::Right)]
        place: PanePlace,
        /// The command and its arguments. The login shell when empty.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// List the panes.
    List,
    /// Kill a pane's process.
    Kill {
        /// The pane to kill.
        pane: u64,
    },
}

pub fn run() -> Result<()> {
    let Some(command) = Cli::parse().command else {
        let paths = Paths::discover()?;
        return twyx_tui::app::attach(&paths);
    };
    match command {
        Command::Daemon { command } => daemon(command),
        Command::Pane { command } => pane(command),
        Command::Doctor => twyx_tui::app::doctor(),
        Command::Specimen { paper } => twyx_tui::specimen::run(paper.map(|paper| match paper {
            Paper::Light => Variant::Light,
            Paper::Dark => Variant::Dark,
        })),
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
            let connection = client::connect(&paths.socket(), 80, 24, false)?;
            println!("daemon running, pid {}", connection.hello.pid);
            Ok(())
        }
        DaemonCommand::Stop => {
            if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
                println!("daemon stopped");
                return Ok(());
            }
            let mut connection = client::connect(&paths.socket(), 80, 24, false)?;
            connection.send(&ToDaemon::Shutdown)?;
            Ok(())
        }
        DaemonCommand::Status => {
            match client::connect(&paths.socket(), 80, 24, false) {
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

fn pane(command: PaneCommand) -> Result<()> {
    let paths = Paths::discover()?;
    client::ensure_daemon(&paths)?;
    match command {
        PaneCommand::New { cwd, place, argv } => {
            let mut connection = client::connect(&paths.socket(), 80, 24, false)?;
            let known: Vec<u64> = connection
                .hello
                .panes
                .iter()
                .map(|pane| pane.pane)
                .collect();
            let launch = if argv.is_empty() {
                Launch::Shell
            } else {
                Launch::Command(argv)
            };
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().context("current directory")?,
            };
            connection.send(&ToDaemon::NewPane {
                launch,
                cwd: cwd.to_string_lossy().into_owned(),
                place: match place {
                    PanePlace::Right => Place::Split(Axis::Row),
                    PanePlace::Down => Place::Split(Axis::Column),
                    PanePlace::Stack => Place::Stack,
                    PanePlace::Float => Place::Floating,
                    PanePlace::Tab => Place::NewTab,
                },
            })?;
            loop {
                match connection.receive()? {
                    FromDaemon::Pane(info) if !known.contains(&info.pane) => {
                        println!("{}", info.pane);
                        return Ok(());
                    }
                    FromDaemon::Refused { reason } => bail!("{reason}"),
                    _ => {}
                }
            }
        }
        PaneCommand::List => {
            let connection = client::connect(&paths.socket(), 80, 24, false)?;
            if connection.hello.panes.is_empty() {
                println!("no panes");
                return Ok(());
            }
            for pane in connection.hello.panes {
                println!(
                    "{} {}x{} {}",
                    pane.pane,
                    pane.cols,
                    pane.rows,
                    state(&pane.state)
                );
            }
            Ok(())
        }
        PaneCommand::Kill { pane } => {
            let mut connection = client::connect(&paths.socket(), 80, 24, false)?;
            connection.send(&ToDaemon::KillPane { pane })?;
            Ok(())
        }
    }
}

fn state(state: &PaneState) -> String {
    match state {
        PaneState::Running => "running".to_owned(),
        PaneState::Exited { code } => format!("exited {code}"),
        PaneState::Killed { signal } => format!("killed by {signal}"),
        PaneState::Lost => "lost".to_owned(),
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

    let geometry = Geometry {
        cols,
        rows,
        scrollback_lines,
    };
    let config = twyx_holder::runtime::Config {
        socket,
        pane,
        build: twyx_holder::runtime::build_id(),
        spawn,
        scrollback_lines,
        exit_grace: twyx_core::tuning::EXIT_GRACE,
    };

    twyx_holder::runtime::run(config, |answerback| {
        twyx_holder::engine::create(geometry, answerback)
    })
}

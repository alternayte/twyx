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
use twyx_protocol::client::{
    AgentEvent, AgentInfo, FromDaemon, Launch, PaneState, Place, ToDaemon,
};
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
    /// Open the project a path is in and show its workspace.
    Open {
        /// A path in a repository, or a plain directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Only record it; switch no client and attach nothing.
        #[arg(long)]
        no_switch: bool,
        #[arg(long)]
        json: bool,
    },
    /// List projects, worktrees and workspaces, with branch and dirty state.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Work with workspaces.
    Workspace {
        #[command(subcommand)]
        command: crate::projects::WorkspaceCommand,
    },
    /// Work with the worktrees of a project.
    Worktree {
        #[command(subcommand)]
        command: crate::projects::WorktreeCommand,
    },
    /// List, run and trust recipes.
    Recipe {
        #[command(subcommand)]
        command: crate::recipes::RecipeCommand,
    },
    /// Work with pane history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Report an agent's state for the pane this runs in. Agent
    /// integrations run this from their hooks.
    Notify {
        #[arg(value_enum)]
        event: NotifyEvent,
        /// The agent kind; needed when the pane has no agent session yet.
        #[arg(long)]
        agent: Option<String>,
        /// The agent's own reference to its session, for resume.
        #[arg(long)]
        session: Option<String>,
        /// Read a JSON object from stdin and take the session reference from
        /// this field, as a hook gets it.
        #[arg(long, value_name = "FIELD", conflicts_with = "session")]
        session_from_stdin: Option<String>,
    },
    /// List the inbox: the agent sessions and panes that need you, newest
    /// first.
    Inbox,
    /// Start tasks: a worktree per agent, with the prompt.
    Task {
        #[command(subcommand)]
        command: crate::tasks::TaskCommand,
    },
    /// Install, show and remove agent integrations.
    Integration {
        #[command(subcommand)]
        command: crate::integrations::IntegrationCommand,
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
        /// Where to write the pane's history.
        #[arg(long)]
        history: Option<PathBuf>,
        /// A history to show above the new output, for a pane that runs again.
        #[arg(long)]
        after: Option<PathBuf>,
        /// A variable for the command, as NAME=VALUE.
        #[arg(long = "env")]
        env: Vec<String>,
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
enum NotifyEvent {
    Working,
    AwaitingApproval,
    AwaitingPrompt,
    TurnFinished,
    Error,
    /// Only the session reference.
    Session,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Paper {
    Light,
    Dark,
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    /// Delete every pane's history. Running panes start a new one.
    Purge,
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the daemon in the foreground.
    Run,
    /// Start the daemon in the background.
    Start,
    /// Stop the daemon. Panes keep running.
    Stop,
    /// Report whether the daemon runs, and any holders too old to adopt.
    Status,
    /// End the holders too old for this daemon to adopt, with their panes.
    KillLegacy,
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
        /// Keep no history of this pane on disk.
        #[arg(long)]
        no_history: bool,
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
        return twyx_tui::app::attach(&paths, None);
    };
    match command {
        Command::Daemon { command } => daemon(command),
        Command::Pane { command } => pane(command),
        Command::Open {
            path,
            no_switch,
            json,
        } => crate::projects::open(&path, no_switch, json),
        Command::Ls { json } => crate::projects::ls(json),
        Command::Workspace { command } => crate::projects::workspace(command),
        Command::Worktree { command } => crate::projects::worktree(command),
        Command::Recipe { command } => crate::recipes::recipe(command),
        Command::History {
            command: HistoryCommand::Purge,
        } => {
            let paths = Paths::discover()?;
            let mut connection = command_client(&paths)?;
            connection.send(&ToDaemon::PurgeHistory)?;
            println!("deleted all pane history");
            Ok(())
        }
        Command::Notify {
            event,
            agent,
            session,
            session_from_stdin,
        } => {
            let session = match session_from_stdin {
                Some(field) => from_stdin(&field)?,
                None => session,
            };
            notify(event, agent, session)
        }
        Command::Integration { command } => crate::integrations::integration(command),
        Command::Inbox => inbox(),
        Command::Task { command } => crate::tasks::task(command),
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
            history,
            after,
            env,
            argv,
        } => holder(HolderArgs {
            pane,
            socket,
            cols,
            rows,
            cwd,
            scrollback_lines,
            history,
            after,
            env,
            argv,
        }),
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
            let connection = client::attach(&paths, 80, 24, false, None, |_| false)?;
            println!("daemon running, pid {}", connection.hello.pid);
            Ok(())
        }
        DaemonCommand::Stop => {
            if std::os::unix::net::UnixStream::connect(paths.socket()).is_err() {
                println!("daemon stopped");
                return Ok(());
            }
            match client::connect(&paths.socket(), 80, 24, false, None) {
                Ok(mut connection) => connection.send(&ToDaemon::Shutdown)?,
                // A daemon of another major still says who it is.
                Err(error) => match error.downcast_ref::<client::Unsupported>() {
                    Some(unsupported) => {
                        if let Some(pid) = rustix::process::Pid::from_raw(unsupported.pid as i32) {
                            rustix::process::kill_process(pid, rustix::process::Signal::TERM)
                                .context("stop the daemon")?;
                        }
                    }
                    None => return Err(error),
                },
            }
            Ok(())
        }
        DaemonCommand::KillLegacy => {
            let mut connection = command_client(&paths)?;
            let count = connection.hello.legacy.len();
            connection.send(&ToDaemon::KillLegacy)?;
            println!(
                "ended {count} legacy holder{}",
                if count == 1 { "" } else { "s" }
            );
            Ok(())
        }
        DaemonCommand::Status => {
            match client::connect(&paths.socket(), 80, 24, false, None) {
                Ok(connection) => {
                    let hello = &connection.hello;
                    let plural = if hello.panes.len() == 1 { "" } else { "s" };
                    println!(
                        "running pid {} with {} pane{plural}",
                        hello.pid,
                        hello.panes.len()
                    );
                    for holder in &hello.legacy {
                        let pid = holder
                            .pid
                            .map_or_else(|| "unknown".to_owned(), |pid| pid.to_string());
                        println!(
                            "legacy holder {} speaks protocol {}, pid {pid}; twyx daemon kill-legacy ends it",
                            holder.socket, holder.major
                        );
                    }
                    if hello.build != twyx_protocol::BUILD_ID {
                        println!(
                            "build {}, not this build {}",
                            hello.build,
                            twyx_protocol::BUILD_ID
                        );
                    }
                }
                Err(error) => match error.downcast_ref::<client::Unsupported>() {
                    Some(unsupported) => println!("running, {unsupported}"),
                    None => println!("stopped"),
                },
            }
            Ok(())
        }
    }
}

fn pane(command: PaneCommand) -> Result<()> {
    let paths = Paths::discover()?;
    match command {
        PaneCommand::New {
            cwd,
            place,
            no_history,
            argv,
        } => {
            let mut connection = command_client(&paths)?;
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
                history: !no_history,
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
            let connection = command_client(&paths)?;
            if connection.hello.panes.is_empty() {
                println!("no panes");
                return Ok(());
            }
            for pane in connection.hello.panes {
                println!(
                    "{} {}x{} {}{}",
                    pane.pane,
                    pane.cols,
                    pane.rows,
                    state(&pane.state),
                    agent(pane.agent.as_ref())
                );
            }
            Ok(())
        }
        PaneCommand::Kill { pane } => {
            let mut connection = command_client(&paths)?;
            connection.send(&ToDaemon::KillPane { pane })?;
            Ok(())
        }
    }
}

/// A CLI command's connection: this build's daemon, replacing one from
/// another build only when no interactive client uses it.
fn command_client(paths: &Paths) -> Result<client::Connection> {
    client::attach(paths, 80, 24, false, None, |_| false)
}

/// Sends an integration's event to the daemon of the pane this runs in. It
/// never starts or replaces a daemon: a hook must not.
fn notify(event: NotifyEvent, agent: Option<String>, session: Option<String>) -> Result<()> {
    let pane =
        std::env::var("TWYX_PANE_ID").context("not in a twyx pane: TWYX_PANE_ID is not set")?;
    let pane: PaneId = pane.parse().context("TWYX_PANE_ID")?;
    let socket =
        std::env::var_os("TWYX_SOCKET").context("not in a twyx pane: TWYX_SOCKET is not set")?;
    if let Some(kind) = &agent {
        let known = twyx_core::agent::names_in(&Paths::discover()?.config.join("agents"));
        if !known.contains(kind) {
            bail!(
                "{kind:?} is not an agent kind; the kinds are {}",
                known.join(", ")
            );
        }
    }
    let event = match event {
        NotifyEvent::Working => AgentEvent::Working,
        NotifyEvent::AwaitingApproval => AgentEvent::AwaitingApproval,
        NotifyEvent::AwaitingPrompt => AgentEvent::AwaitingPrompt,
        NotifyEvent::TurnFinished => AgentEvent::TurnFinished,
        NotifyEvent::Error => AgentEvent::Error,
        NotifyEvent::Session => AgentEvent::Session,
    };
    let mut connection = client::connect(std::path::Path::new(&socket), 80, 24, false, None)?;
    connection.send(&ToDaemon::Notify {
        pane: pane.raw(),
        agent,
        event,
        session,
    })
}

fn inbox() -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = crate::projects::connect(&paths)?;
    connection.send(&ToDaemon::Inbox)?;
    let items = crate::projects::wait(&mut connection, |message| match message {
        FromDaemon::Inbox(items) => Some(items),
        _ => None,
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    for item in items {
        let place = match (&item.project, &item.worktree) {
            (Some(project), Some(worktree)) => format!("{project}/{worktree}"),
            (Some(project), None) => project.clone(),
            (None, _) => "-".to_owned(),
        };
        let state = match item.agent {
            Some((state, source)) => format!("{} ({})", state.name(), source.name()),
            None => "-".to_owned(),
        };
        println!(
            "{}  {}  {}  {place}  pane {}  {}  {}s ago{}",
            item.item,
            item.mark.map_or(item.reason.name(), |mark| mark.word()),
            state,
            item.pane,
            item.title,
            now.saturating_sub(item.created_ms / 1000),
            if item.read { "" } else { "  unread" },
        );
    }
    Ok(())
}

/// A string field of the JSON object on stdin.
fn from_stdin(field: &str) -> Result<Option<String>> {
    let value: serde_json::Value =
        serde_json::from_reader(std::io::stdin().lock()).context("read JSON from stdin")?;
    Ok(value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|reference| !reference.is_empty())
        .map(str::to_owned))
}

fn agent(agent: Option<&AgentInfo>) -> String {
    agent.map_or_else(String::new, |agent| {
        format!(
            " {} {} ({})",
            agent.kind,
            agent.state.name(),
            agent.source.name()
        )
    })
}

fn state(state: &PaneState) -> String {
    match state {
        PaneState::Running => "running".to_owned(),
        PaneState::Exited { code } => format!("exited {code}"),
        PaneState::Killed { signal } => format!("killed by {signal}"),
        PaneState::Lost => "lost".to_owned(),
    }
}

struct HolderArgs {
    pane: String,
    socket: PathBuf,
    cols: u16,
    rows: u16,
    cwd: Option<PathBuf>,
    scrollback_lines: Option<u32>,
    history: Option<PathBuf>,
    after: Option<PathBuf>,
    env: Vec<String>,
    argv: Vec<String>,
}

fn holder(args: HolderArgs) -> Result<()> {
    let HolderArgs {
        pane,
        socket,
        cols,
        rows,
        cwd,
        scrollback_lines,
        history,
        after,
        env: overrides,
        argv,
    } = args;
    let pane: PaneId = pane.parse().context("pane id")?;
    if argv.is_empty() {
        bail!("a pane needs a command");
    }
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().context("current directory")?,
    };
    let mut env: Vec<(String, String)> = overrides
        .iter()
        .filter_map(|setting| {
            let (name, value) = setting.split_once('=')?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect();
    // twyx's own variables come last, so a recipe cannot hide the pane.
    env.extend([
        ("TWYX_PANE_ID".to_owned(), pane.to_string()),
        (
            "TWYX_SOCKET".to_owned(),
            Paths::discover()?.socket().to_string_lossy().into_owned(),
        ),
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ]);

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
        history,
    };

    twyx_holder::runtime::run(config, |answerback| match &after {
        Some(earlier) => twyx_holder::engine::create_after(geometry, answerback, earlier),
        None => twyx_holder::engine::create(geometry, answerback),
    })
}

//! Command line parsing and dispatch.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use serde::Deserialize;
use serde_json::{Value, json};
use twyx_core::pane::PaneId;
use twyx_core::paths::Paths;
use twyx_core::theme::Variant;
use twyx_holder::engine::Geometry;
use twyx_holder::pty::Spawn;
use twyx_protocol::api;
use twyx_protocol::client::{
    AgentEvent, AgentInfo, InboxItem, PaneInfo, PaneState, TabInfo, ToDaemon,
};
use twyx_protocol::connection as client;

use crate::projects::{Connection, print_json};

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
    /// Work with the tabs of the current directory's workspace.
    Tab {
        #[command(subcommand)]
        command: TabCommand,
    },
    /// Print the daemon's events as JSON lines until it stops.
    Events {
        /// An event kind to print: agent, pane, workspace, overview, inbox,
        /// check or ship. Every kind when left out.
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// The output is JSON lines already; this is accepted for
        /// uniformity.
        #[arg(long)]
        json: bool,
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
    Inbox {
        #[arg(long)]
        json: bool,
    },
    /// Review the changes of the current worktree since its base.
    Review {
        #[command(subcommand)]
        command: crate::review::ReviewCommand,
    },
    /// Run the checks of the current worktree and read their results.
    Check {
        #[command(subcommand)]
        command: crate::checks::CheckCommand,
    },
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
    /// Print a skill file that teaches an agent to use twyx.
    Skill,
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
    Purge {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum TabCommand {
    /// List the tabs.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show a tab in the client that last had input.
    Select {
        tab: u64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    /// Run the daemon in the foreground.
    Run,
    /// Start the daemon in the background.
    Start {
        #[arg(long)]
        json: bool,
    },
    /// Stop the daemon. Panes keep running.
    Stop {
        #[arg(long)]
        json: bool,
    },
    /// Report whether the daemon runs, and any holders too old to adopt.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// End the holders too old for this daemon to adopt, with their panes.
    KillLegacy {
        #[arg(long)]
        json: bool,
    },
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
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },
    /// List the panes.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Kill a pane's process.
    Kill {
        /// The pane to kill.
        pane: u64,
        #[arg(long)]
        json: bool,
    },
    /// Show a pane in the client that last had input.
    Focus {
        pane: u64,
        #[arg(long)]
        json: bool,
    },
    /// Type text and keys into a pane, through the same queue as a client's
    /// keys.
    Send {
        pane: u64,
        /// Text to type.
        #[arg(long)]
        text: Option<String>,
        /// Send the text as one paste instead of typing it.
        #[arg(long, requires = "text")]
        paste: bool,
        /// A key to press after the text, such as enter or ctrl-c. Repeat
        /// it for more keys.
        #[arg(long = "key")]
        keys: Vec<String>,
        #[arg(long)]
        json: bool,
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
            command: HistoryCommand::Purge { json },
        } => {
            let paths = Paths::discover()?;
            let () = crate::projects::connect(&paths)?.call("history.purge", &Value::Null)?;
            done(json, "deleted all pane history")
        }
        Command::Tab { command } => tab(command),
        Command::Events { kinds, json: _ } => events(kinds),
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
        Command::Inbox { json } => inbox(json),
        Command::Task { command } => crate::tasks::task(command),
        Command::Review { command } => crate::review::review(command),
        Command::Check { command } => crate::checks::check(command),
        Command::Doctor => twyx_tui::app::doctor(),
        Command::Skill => {
            print!("{}", include_str!("../../../docs/skill.md"));
            Ok(())
        }
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
        DaemonCommand::Start { json } => {
            let connection = client::attach_api(&paths)?;
            let pid = connection.welcome.pid;
            if json {
                return print_json(&json!({ "pid": pid }));
            }
            println!("daemon running, pid {pid}");
            Ok(())
        }
        DaemonCommand::Stop { json } => {
            if std::os::unix::net::UnixStream::connect(paths.socket()).is_ok() {
                match client::connect(&paths.socket(), 80, 24, false, None) {
                    Ok(mut connection) => connection.send(&ToDaemon::Shutdown)?,
                    // A daemon of another major still says who it is.
                    Err(error) => match error.downcast_ref::<client::Unsupported>() {
                        Some(unsupported) => {
                            if let Some(pid) =
                                rustix::process::Pid::from_raw(unsupported.pid as i32)
                            {
                                rustix::process::kill_process(pid, rustix::process::Signal::TERM)
                                    .context("stop the daemon")?;
                            }
                        }
                        None => return Err(error),
                    },
                }
            }
            done(json, "daemon stopped")
        }
        DaemonCommand::KillLegacy { json } => {
            let mut connection = crate::projects::connect(&paths)?;
            let count = status(&mut connection)?.legacy.len();
            let () = connection.call("daemon.kill_legacy", &Value::Null)?;
            if json {
                return print_json(&json!({ "ended": count }));
            }
            println!(
                "ended {count} legacy holder{}",
                if count == 1 { "" } else { "s" }
            );
            Ok(())
        }
        DaemonCommand::Status { json } => {
            match client::connect(&paths.socket(), 80, 24, false, None) {
                Ok(connection) => {
                    let hello = &connection.hello;
                    if json {
                        return print_json(&json!({
                            "running": true,
                            "pid": hello.pid,
                            "build": hello.build,
                            "panes": hello.panes.len(),
                            "legacy": hello.legacy,
                        }));
                    }
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
                Err(error) => {
                    let unsupported = error.downcast_ref::<client::Unsupported>();
                    if json {
                        return print_json(&json!({
                            "running": unsupported.is_some(),
                            "pid": unsupported.map(|unsupported| unsupported.pid),
                        }));
                    }
                    match unsupported {
                        Some(unsupported) => println!("running, {unsupported}"),
                        None => println!("stopped"),
                    }
                }
            }
            Ok(())
        }
    }
}

/// What `daemon.status` answers.
#[derive(Debug, Deserialize)]
struct DaemonStatus {
    legacy: Vec<twyx_protocol::client::LegacyHolder>,
}

fn status(connection: &mut Connection) -> Result<DaemonStatus> {
    connection.call("daemon.status", &Value::Null)
}

/// The end of a command with nothing to report but that it ran.
fn done(json: bool, text: &str) -> Result<()> {
    if json {
        return print_json(&Value::Null);
    }
    println!("{text}");
    Ok(())
}

fn tab(command: TabCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = crate::projects::connect(&paths)?;
    match command {
        TabCommand::List { json } => {
            let workspace = crate::recipes::here(&mut connection)?;
            let tabs: Vec<TabInfo> =
                connection.call("tab.list", &api::WorkspaceRef { workspace })?;
            if json {
                return print_json(&tabs);
            }
            for tab in &tabs {
                let panes = tab.layout.panes().len();
                let name = if tab.name.is_empty() { "-" } else { &tab.name };
                println!(
                    "{}  {name}  {panes} pane{}",
                    tab.tab,
                    if panes == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }
        TabCommand::Select { tab, json } => {
            let () = connection.call("tab.select", &api::TabRef { tab })?;
            done(json, &format!("tab {tab} shown"))
        }
    }
}

fn events(kinds: Vec<String>) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = crate::projects::connect(&paths)?;
    let () = connection.call("events.subscribe", &api::Subscribe { kinds })?;
    loop {
        let event = connection.event()?;
        println!(
            "{}",
            serde_json::to_string(&event).context("write an event")?
        );
    }
}

fn pane(command: PaneCommand) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = crate::projects::connect(&paths)?;
    match command {
        PaneCommand::New {
            cwd,
            place,
            no_history,
            json,
            argv,
        } => {
            let cwd = match cwd {
                Some(cwd) => cwd,
                None => std::env::current_dir().context("current directory")?,
            };
            let info: PaneInfo = connection.call(
                "pane.new",
                &api::NewPane {
                    argv,
                    cwd: cwd.to_string_lossy().into_owned(),
                    place: match place {
                        PanePlace::Right => api::PanePlace::Right,
                        PanePlace::Down => api::PanePlace::Down,
                        PanePlace::Stack => api::PanePlace::Stack,
                        PanePlace::Float => api::PanePlace::Float,
                        PanePlace::Tab => api::PanePlace::Tab,
                    },
                    history: !no_history,
                },
            )?;
            if json {
                return print_json(&info);
            }
            println!("{}", info.pane);
            Ok(())
        }
        PaneCommand::List { json } => {
            let panes: Vec<PaneInfo> = connection.call("pane.list", &Value::Null)?;
            if json {
                return print_json(&panes);
            }
            if panes.is_empty() {
                println!("no panes");
                return Ok(());
            }
            for pane in panes {
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
        PaneCommand::Kill { pane, json } => {
            let () = connection.call("pane.kill", &api::PaneRef { pane })?;
            done(json, &format!("killed pane {pane}"))
        }
        PaneCommand::Focus { pane, json } => {
            let () = connection.call("pane.focus", &api::PaneRef { pane })?;
            done(json, &format!("pane {pane} shown"))
        }
        PaneCommand::Send {
            pane,
            text,
            paste,
            keys,
            json,
        } => {
            let () = connection.call(
                "pane.send",
                &api::SendToPane {
                    pane,
                    text,
                    paste,
                    keys,
                },
            )?;
            done(json, &format!("sent to pane {pane}"))
        }
    }
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
    let mut connection = api::Client::connect(std::path::Path::new(&socket))?;
    connection.call(
        "notify",
        &api::Notify {
            pane: pane.raw(),
            agent,
            event,
            session,
        },
    )
}

fn inbox(json: bool) -> Result<()> {
    let paths = Paths::discover()?;
    let mut connection = crate::projects::connect(&paths)?;
    let items: Vec<InboxItem> = connection.call("inbox.list", &Value::Null)?;
    if json {
        return print_json(&items);
    }
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
    let value: Value =
        serde_json::from_reader(std::io::stdin().lock()).context("read JSON from stdin")?;
    Ok(value
        .get(field)
        .and_then(Value::as_str)
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

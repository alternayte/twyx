//! The terminal client: one pane, the outer terminal, and the daemon between
//! them.

use std::io::Write;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};

use twyx_core::layout::LeafId;
use twyx_core::theme::Rgb;
use twyx_protocol::client::{FromDaemon, Launch, Place, ToDaemon, WorkspaceInfo};
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::wire;

use crate::emitter::{Capabilities, Emitter};
use crate::input;
use crate::probe;
use twyx_protocol::connection as client;
use twyx_protocol::view::View;

/// The key twyx takes from the pane. S2 makes it configurable.
const PREFIX: char = 'g';

/// Attaches the terminal client to the daemon.
pub fn attach(paths: &twyx_core::paths::Paths) -> Result<()> {
    client::ensure_daemon(paths)?;
    let (cols, rows) = clamp_size(terminal::size().context("read the terminal size")?);

    let mut screen = Screen::start()?;
    let result = run(paths, cols, rows, &mut screen);
    screen.stop();
    result
}

/// Prints what the outer terminal says it can do.
pub fn doctor() -> Result<()> {
    let answers = probe()?;
    println!(
        "terminal:  {}",
        answers.name.as_deref().unwrap_or("unknown")
    );
    println!(
        "version:   {}",
        answers.version.as_deref().unwrap_or("unknown")
    );
    println!(
        "kitty keyboard: {}",
        match answers.kitty_keyboard {
            Some(flags) => format!("yes, flags {flags}"),
            None => "no".to_owned(),
        }
    );
    println!(
        "synchronized output: {}",
        yes_no(answers.synchronized_output)
    );
    println!("truecolor: {}", yes_no(answers.truecolor));
    let colour = |rgb: Option<Rgb>| match rgb {
        Some(Rgb { r, g, b }) => format!("#{r:02x}{g:02x}{b:02x}"),
        None => "unknown".to_owned(),
    };
    println!("paper: {}", colour(answers.paper));
    println!("ink: {}", colour(answers.ink));
    Ok(())
}

/// Waits for a daemon after the last one went away, starting one if none
/// answers. The new daemon adopts the holders, so the panes are still there.
fn reconnect(paths: &twyx_core::paths::Paths, size: (u16, u16)) -> Result<client::Parts> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let attempt = client::ensure_daemon(paths)
            .and_then(|()| client::connect(&paths.socket(), size.0, size.1, true));
        match attempt {
            Ok(connection) => return Ok(connection.split()),
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("lost the daemon and could not start a new one"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Asks the outer terminal what it can do, in raw mode for the moment of the
/// question.
pub fn probe() -> Result<probe::Answers> {
    terminal::enable_raw_mode().context("enter raw mode")?;
    let answers = probe::interrogate(&mut tty()?);
    terminal::disable_raw_mode().context("leave raw mode")?;
    answers
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn tty() -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("open the terminal")
}

/// What the client follows: the workspace, its own focus, and the pane on
/// screen.
#[derive(Debug, Default)]
struct Follow {
    workspace: Option<WorkspaceInfo>,
    tab: Option<u64>,
    leaf: Option<LeafId>,
    /// The pane drawn now.
    shown: Option<u64>,
    /// Whether the workspace held a pane since this client attached.
    had_panes: bool,
    /// Whether this client asked for a first pane.
    asked: bool,
}

impl Follow {
    fn focused_pane(&self) -> Option<u64> {
        let tab = self
            .workspace
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)?;
        Some(tab.layout.leaf(self.leaf?)?.pane.raw())
    }

    fn is_empty(&self) -> bool {
        self.workspace
            .as_ref()
            .is_some_and(|workspace| workspace.tabs.is_empty())
    }
}

fn run(paths: &twyx_core::paths::Paths, cols: u16, rows: u16, screen: &mut Screen) -> Result<()> {
    let cols = cols.max(2);
    let rows = rows.max(1);
    let connection = client::connect(&paths.socket(), cols, rows, true)?;
    let mut daemon = connection.split();
    let mut size = (cols, rows);

    let mut view = View::new(cols, rows);
    screen.emitter.resize(cols, rows);
    let mut prefix_pending = false;
    let mut follow = Follow::default();
    let mut title = String::new();

    loop {
        let mut drew = false;
        let mut lost = false;
        loop {
            let message = match daemon.incoming.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    lost = true;
                    break;
                }
            };
            match message {
                FromDaemon::Workspace(workspace) => follow.workspace = Some(workspace),
                FromDaemon::Focus { tab, leaf } => {
                    follow.tab = tab;
                    follow.leaf = leaf;
                }
                FromDaemon::Snapshot { pane, body, .. } if Some(pane) == follow.shown => {
                    view.apply_snapshot(wire::decode_body::<Snapshot>(&body)?);
                    drew = true;
                }
                FromDaemon::Frame { pane, body, .. } if Some(pane) == follow.shown => {
                    view.apply_frame(wire::decode_body::<Frame>(&body)?);
                    drew = true;
                }
                FromDaemon::Clipboard { text, .. } => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                    screen.write(&format!("\x1b]52;c;{encoded}\x1b\\"))?;
                }
                FromDaemon::Unsupported { major, minor } => {
                    anyhow::bail!("the daemon speaks client protocol {major}.{minor}");
                }
                // Only a first pane that could not start leaves nothing to show.
                FromDaemon::Refused { reason } if follow.is_empty() => anyhow::bail!("{reason}"),
                FromDaemon::Refused { .. }
                | FromDaemon::Welcome(_)
                | FromDaemon::Pane(_)
                | FromDaemon::PaneClosed { .. }
                | FromDaemon::Snapshot { .. }
                | FromDaemon::Frame { .. } => {}
            }
        }

        if lost {
            daemon = reconnect(paths, size)?;
            // The new daemon adopts the panes and keeps the leaves; this
            // client's own focus goes back to it.
            if let Some(leaf) = follow.leaf {
                let _ = daemon.outgoing.send(ToDaemon::FocusLeaf { leaf });
            }
            follow = Follow {
                had_panes: follow.had_panes,
                asked: true,
                ..Follow::default()
            };
            continue;
        }

        if follow.is_empty() {
            // The last pane closed: the client has nothing left to show.
            if follow.had_panes {
                return Ok(());
            }
            if !follow.asked {
                follow.asked = true;
                let _ = daemon.outgoing.send(ToDaemon::NewPane {
                    launch: Launch::Shell,
                    cwd: working_directory(),
                    place: Place::NewTab,
                });
            }
        } else if follow.workspace.is_some() {
            follow.had_panes = true;
        }

        let focused = follow.focused_pane();
        if focused != follow.shown {
            follow.shown = focused;
            if let Some(pane) = focused {
                let _ = daemon.outgoing.send(ToDaemon::Snapshot { pane });
            }
        }

        if drew {
            screen.emitter.draw(&mut view)?;
            if view.title != title {
                title.clone_from(&view.title);
                screen.set_title(&title)?;
            }
        }

        // Frames are only drained on each turn, so this timeout is a latency
        // floor for every keystroke-to-screen path. It matches the holder's
        // frame interval.
        if !event::poll(Duration::from_millis(8)).context("poll the terminal")? {
            continue;
        }
        match event::read().context("read a terminal event")? {
            Event::Resize(new_cols, new_rows) => {
                let new_cols = new_cols.max(2);
                let new_rows = new_rows.max(1);
                size = (new_cols, new_rows);
                // The view keeps the pane's screen; only the crop changes until
                // the pane itself is resized.
                screen.emitter.resize(new_cols, new_rows);
                screen.emitter.draw(&mut view)?;
                let _ = daemon.outgoing.send(ToDaemon::Resize {
                    cols: new_cols,
                    rows: new_rows,
                });
            }
            Event::Key(key) => {
                if key.code == KeyCode::Char(PREFIX)
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    if prefix_pending {
                        prefix_pending = false;
                        if let Some(event) = input::to_input(Event::Key(key)) {
                            let _ = daemon.outgoing.send(ToDaemon::Input(event));
                        }
                    } else {
                        prefix_pending = true;
                    }
                    continue;
                }
                if prefix_pending {
                    prefix_pending = false;
                    if let KeyCode::Char('q') = key.code {
                        return Ok(());
                    }
                    if let Some(event) = input::to_input(Event::Key(key)) {
                        let _ = daemon.outgoing.send(ToDaemon::Input(event));
                    }
                    continue;
                }
                if let Some(event) = input::to_input(Event::Key(key)) {
                    let _ = daemon.outgoing.send(ToDaemon::Input(event));
                }
            }
            other => {
                if let Some(event) = input::to_input(other) {
                    let _ = daemon.outgoing.send(ToDaemon::Input(event));
                }
            }
        }
    }
}

/// A terminal that reports no size is not one twyx can lay out; fall back to
/// the usual 80 by 24 rather than creating a one cell pane.
fn clamp_size((cols, rows): (u16, u16)) -> (u16, u16) {
    if cols == 0 || rows == 0 {
        (80, 24)
    } else {
        (cols, rows)
    }
}

fn working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_owned())
}

/// The outer terminal, from raw mode to the restored screen.
pub struct Screen {
    pub emitter: Emitter<std::io::Stdout>,
    capabilities: Capabilities,
    active: bool,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("capabilities", &self.capabilities)
            .field("active", &self.active)
            .finish()
    }
}

impl Screen {
    fn start() -> Result<Self> {
        terminal::enable_raw_mode().context("enter raw mode")?;
        let answers = probe::interrogate(&mut tty()?);
        let capabilities = answers
            .map(|answers| answers.capabilities())
            .unwrap_or_default();

        let mut stdout = std::io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            EnableFocusChange
        )
        .context("set up the screen")?;
        if capabilities.kitty_keyboard {
            // Disambiguation is what makes Ctrl-I and Tab different keys.
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )
            .context("ask for the kitty keyboard protocol")?;
        }

        Ok(Self {
            emitter: Emitter::new(std::io::stdout(), capabilities),
            capabilities,
            active: true,
        })
    }

    fn write(&mut self, bytes: &str) -> Result<()> {
        let mut stdout = std::io::stdout();
        stdout.write_all(bytes.as_bytes()).context("write")?;
        stdout.flush().context("flush")
    }

    fn set_title(&mut self, title: &str) -> Result<()> {
        let title: String = title.chars().filter(|c| !c.is_control()).collect();
        let mut stdout = std::io::stdout();
        write!(stdout, "\x1b]2;{title}\x07").context("set the title")?;
        stdout.flush().context("flush the title")
    }

    fn stop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let _ = self.emitter.leave();
        let mut stdout = std::io::stdout();
        if self.capabilities.kitty_keyboard {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            DisableFocusChange,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_that_reports_no_size_falls_back_to_eighty_by_twenty_four() {
        assert_eq!(clamp_size((0, 0)), (80, 24));
        assert_eq!(clamp_size((0, 40)), (80, 24));
        assert_eq!(clamp_size((120, 0)), (80, 24));
    }

    #[test]
    fn a_real_size_is_kept() {
        assert_eq!(clamp_size((120, 40)), (120, 40));
        assert_eq!(clamp_size((1, 1)), (1, 1));
    }
}

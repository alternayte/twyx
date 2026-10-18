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

use twyx_protocol::client::{FromDaemon, PaneState, ToDaemon};
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
    terminal::enable_raw_mode().context("enter raw mode")?;
    let answers = probe::interrogate(&mut tty()?);
    terminal::disable_raw_mode().context("leave raw mode")?;

    let answers = answers?;
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
    Ok(())
}

/// Waits for a daemon after the last one went away, starting one if none
/// answers. The new daemon adopts the holders, so the panes are still there.
fn reconnect(paths: &twyx_core::paths::Paths, size: (u16, u16)) -> Result<client::Parts> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let attempt = client::ensure_daemon(paths)
            .and_then(|()| client::connect(&paths.socket(), size.0, size.1));
        match attempt {
            Ok(connection) => return Ok(connection.split()),
            Err(error) if Instant::now() >= deadline => {
                return Err(error.context("lost the daemon and could not start a new one"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
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

fn run(paths: &twyx_core::paths::Paths, cols: u16, rows: u16, screen: &mut Screen) -> Result<()> {
    let cols = cols.max(2);
    let rows = rows.max(1);
    let connection = client::connect(&paths.socket(), cols, rows)?;
    let mut daemon = connection.split();
    let mut size = (cols, rows);

    let mut view = View::new(cols, rows);
    screen.emitter.resize(cols, rows);
    let mut prefix_pending = false;
    // A pane that has ended keeps its screen but takes no input, so a client
    // that attached to one would look dead. Take the first live pane, and ask
    // for one when there is none.
    let mut pane = daemon
        .hello
        .panes
        .iter()
        .find(|info| info.state == PaneState::Running)
        .map(|info| info.pane);
    let mut title = String::new();

    if let Some(info) = daemon
        .hello
        .panes
        .iter()
        .find(|info| info.pane == pane.unwrap_or(u64::MAX))
    {
        view.resize_grid(info.cols, info.rows);
    } else {
        daemon.outgoing.send(ToDaemon::NewPane {
            argv: vec![shell()],
            cwd: working_directory(),
            cols,
            rows,
        })?;
    }

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
                FromDaemon::Welcome(hello) => {
                    if pane.is_none() {
                        pane = hello.panes.first().map(|info| info.pane);
                    }
                }
                FromDaemon::Snapshot { pane: id, body, .. } if Some(id) == pane => {
                    view.apply_snapshot(wire::decode_body::<Snapshot>(&body)?);
                    drew = true;
                }
                FromDaemon::Frame { pane: id, body, .. } if Some(id) == pane => {
                    view.apply_frame(wire::decode_body::<Frame>(&body)?);
                    drew = true;
                }
                FromDaemon::Pane(info) => {
                    if pane.is_none() && info.state == PaneState::Running {
                        pane = Some(info.pane);
                    }
                }
                FromDaemon::Clipboard { text, .. } => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                    screen.write(&format!("\x1b]52;c;{encoded}\x1b\\"))?;
                }
                FromDaemon::Unsupported { major, minor } => {
                    anyhow::bail!("the daemon speaks client protocol {major}.{minor}");
                }
                FromDaemon::Lease { .. }
                | FromDaemon::Snapshot { .. }
                | FromDaemon::Frame { .. } => {}
            }
        }

        if lost {
            daemon = reconnect(paths, size)?;
            // The pane keeps its id across daemons; take another live one only
            // if it is gone.
            let kept = daemon
                .hello
                .panes
                .iter()
                .any(|info| Some(info.pane) == pane && info.state == PaneState::Running);
            if !kept {
                pane = daemon
                    .hello
                    .panes
                    .iter()
                    .find(|info| info.state == PaneState::Running)
                    .map(|info| info.pane);
            }
            continue;
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

fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
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

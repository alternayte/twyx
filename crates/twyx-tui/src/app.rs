//! The terminal client: one pane, the outer terminal, and the daemon between
//! them.

use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};

use twyx_core::keys::{Command, Keymap, Mode, Outcome};
use twyx_core::layout::LeafId;
use twyx_core::theme::{Rgb, Theme, Variant};
use twyx_protocol::client::{
    FromDaemon, Launch, PaneInfo, Place, ToDaemon, WorkspaceInfo, request,
};
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::input::InputEvent;
use twyx_protocol::wire;

use crate::chrome::{Depth, Pen};
use crate::compositor::{CHROME_ROWS, Scene, tab_area};
use crate::emitter::{Capabilities, Emitter};
use crate::input;
use crate::mouse::{Action, Mouse};
use crate::overlay::{Choice, Outcome as OverlayOutcome, Overlay};
use crate::probe;
use twyx_protocol::connection as client;
use twyx_protocol::view::View;

/// Attaches the terminal client to the daemon.
pub fn attach(paths: &twyx_core::paths::Paths) -> Result<()> {
    let config = twyx_core::config::load(&paths.config.join("config.toml"))?;
    client::ensure_daemon(paths)?;
    let (cols, rows) = clamp_size(terminal::size().context("read the terminal size")?);

    let mut screen = Screen::start()?;
    let result = run(paths, cols, rows, &mut screen, config.keymap);
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
    /// The focused pane.
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

fn run(
    paths: &twyx_core::paths::Paths,
    cols: u16,
    rows: u16,
    screen: &mut Screen,
    mut keymap: Keymap,
) -> Result<()> {
    let mut size = (cols.max(2), rows.max(1 + CHROME_ROWS));
    let area = tab_area(size.0, size.1);
    let mut daemon = client::connect(&paths.socket(), area.0, area.1, true)?.split();
    let mut follow = Follow::default();
    let mut panes: HashMap<u64, PaneInfo> = HashMap::new();
    let mut views: HashMap<u64, View> = HashMap::new();
    let mut title = String::new();
    let mut client_id = daemon.hello.client;
    let known = |panes: &mut HashMap<u64, PaneInfo>, hello: &[PaneInfo]| {
        *panes = hello.iter().map(|info| (info.pane, info.clone())).collect();
    };
    known(&mut panes, &daemon.hello.panes);
    let mut dirty = true;
    let mut mouse = Mouse::default();
    let mut overlay: Option<Overlay> = None;
    // Everything the compositor and the mouse need to know, now.
    macro_rules! scene {
        () => {
            Scene {
                pen: &screen.pen,
                cols: size.0,
                rows: size.1,
                client: client_id,
                workspace: follow.workspace.as_ref(),
                tab: follow.tab,
                focus: follow.leaf,
                panes: &panes,
                views: &views,
                keymap: &keymap,
                selection: mouse.selection(),
                overlay: overlay.as_ref(),
            }
        };
    }

    loop {
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
            dirty = true;
            match message {
                FromDaemon::Workspace(workspace) => follow.workspace = Some(workspace),
                FromDaemon::Focus { tab, leaf } => {
                    follow.tab = tab;
                    follow.leaf = leaf;
                }
                FromDaemon::Snapshot { pane, body, .. } => {
                    let snapshot = wire::decode_body::<Snapshot>(&body)?;
                    views
                        .entry(pane)
                        .or_insert_with(|| View::new(snapshot.screen.cols, snapshot.screen.rows))
                        .apply_snapshot(snapshot);
                }
                FromDaemon::Frame { pane, body, .. } => {
                    // A frame for a pane with no snapshot yet waits for one.
                    if let Some(view) = views.get_mut(&pane) {
                        view.apply_frame(wire::decode_body::<Frame>(&body)?);
                    }
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
                FromDaemon::Pane(info) => {
                    panes.insert(info.pane, info);
                }
                FromDaemon::PaneClosed { pane } => {
                    panes.remove(&pane);
                    views.remove(&pane);
                }
                FromDaemon::Refused { .. } | FromDaemon::Welcome(_) => {}
            }
        }

        if lost {
            let area = tab_area(size.0, size.1);
            daemon = reconnect(paths, area)?;
            client_id = daemon.hello.client;
            keymap.reset();
            known(&mut panes, &daemon.hello.panes);
            views.clear();
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
        follow.shown = follow.focused_pane();

        if dirty {
            dirty = false;
            let grid = scene!().compose();
            screen.emitter.draw(&grid)?;
            let focused_title = follow
                .shown
                .and_then(|pane| views.get(&pane))
                .map(|view| view.title.clone())
                .unwrap_or_default();
            if focused_title != title {
                title = focused_title;
                screen.set_title(&title)?;
            }
        }

        // Frames are only drained on each turn, so this timeout is a latency
        // floor for every keystroke-to-screen path. It matches the holder's
        // frame interval.
        if !event::poll(Duration::from_millis(8)).context("poll the terminal")? {
            continue;
        }
        dirty = true;
        match event::read().context("read a terminal event")? {
            Event::Resize(new_cols, new_rows) => {
                size = (new_cols.max(2), new_rows.max(1 + CHROME_ROWS));
                screen.emitter.invalidate();
                let (cols, rows) = tab_area(size.0, size.1);
                let _ = daemon.outgoing.send(ToDaemon::Resize { cols, rows });
            }
            Event::Key(key) => {
                let Some(event) = input::to_input(Event::Key(key)) else {
                    continue;
                };
                let chord = match &event {
                    InputEvent::Key(key) => key.chord(),
                    _ => None,
                };
                let command = if let Some(open) = &mut overlay {
                    let Some(chord) = chord else {
                        continue;
                    };
                    let mut open = open.clone();
                    let outcome = open.key(&scene!(), chord);
                    overlay = Some(open);
                    match outcome {
                        OverlayOutcome::Stay => None,
                        OverlayOutcome::Close => {
                            overlay = None;
                            None
                        }
                        OverlayOutcome::Rename { tab, name } => {
                            overlay = None;
                            let _ = daemon.outgoing.send(ToDaemon::RenameTab { tab, name });
                            None
                        }
                        OverlayOutcome::Choose(choice) => {
                            overlay = None;
                            match choice {
                                Choice::Command(command) => Some(command),
                                Choice::Tab(tab) => {
                                    let _ = daemon.outgoing.send(ToDaemon::SelectTab { tab });
                                    None
                                }
                                Choice::Leaf(leaf) => {
                                    let _ = daemon.outgoing.send(ToDaemon::FocusLeaf { leaf });
                                    None
                                }
                            }
                        }
                    }
                } else {
                    let outcome = match chord {
                        Some(chord) => keymap.press(chord),
                        // A release goes to the pane only outside the modes.
                        None if keymap.mode() == Mode::Normal => Outcome::ToPane,
                        None => Outcome::Consumed,
                    };
                    match outcome {
                        Outcome::ToPane => {
                            let _ = daemon.outgoing.send(ToDaemon::Input(event));
                            None
                        }
                        Outcome::Consumed => None,
                        Outcome::Run(command) => Some(command),
                    }
                };
                match command {
                    None => {}
                    Some(Command::Detach) => return Ok(()),
                    Some(Command::Palette) => {
                        keymap.reset();
                        overlay = Some(Overlay::palette());
                    }
                    Some(Command::RenameTab) => {
                        keymap.reset();
                        if let Some(tab) = follow.tab {
                            let name = follow
                                .workspace
                                .as_ref()
                                .and_then(|workspace| workspace.tabs.iter().find(|t| t.tab == tab))
                                .map(|tab| tab.name.clone())
                                .unwrap_or_default();
                            overlay = Some(Overlay::Rename { tab, name });
                        }
                    }
                    Some(command) => {
                        let focused = follow.shown.and_then(|pane| panes.get(&pane));
                        if let Some(request) = request(command, focused) {
                            let _ = daemon.outgoing.send(request);
                        }
                    }
                }
            }
            Event::Mouse(event) => {
                if overlay.is_some() {
                    continue;
                }
                let actions = mouse.handle(&scene!(), event);
                for action in actions {
                    match action {
                        Action::Send(request) => {
                            let _ = daemon.outgoing.send(request);
                        }
                        Action::Copy(text) => {
                            let encoded =
                                base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                            screen.write(&format!("\x1b]52;c;{encoded}\x1b\\"))?;
                        }
                    }
                }
            }
            Event::Paste(text) if overlay.is_some() => {
                if let Some(open) = &mut overlay {
                    open.paste(&text);
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
    /// The theme on this terminal's paper.
    pen: Pen,
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
        let answers = probe::interrogate(&mut tty()?).unwrap_or_default();
        let capabilities = answers.capabilities();
        let depth = if answers.truecolor {
            Depth::Truecolor
        } else {
            Depth::Palette256
        };
        let pen = Pen::on_paper(Theme::on(answers.paper, answers.ink, Variant::Dark), depth);

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
            pen,
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

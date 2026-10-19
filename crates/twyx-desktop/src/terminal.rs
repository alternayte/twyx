//! The window: the focused pane, drawn with GPUI, talking to the daemon.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, MouseButton, Pixels, Render,
    ScrollWheelEvent, Window, canvas, div, px,
};

use twyx_core::keys::{Command, Keymap, Mode, Outcome};
use twyx_core::layout::LeafId;
use twyx_core::paths::Paths;
use twyx_protocol::client::{
    FromDaemon, Launch, PaneInfo, Place, ToDaemon, WorkspaceInfo, request,
};
use twyx_protocol::connection;
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::holder::CopyAction;
use twyx_protocol::input::{InputEvent, Modifiers, MouseAction, MouseEvent};
use twyx_protocol::view::View;
use twyx_protocol::wire;

use crate::keys;
use crate::paint::{self, Metrics};

/// State the daemon task and the window both touch.
struct Shared {
    view: View,
    /// The pane drawn now: the focused leaf's.
    pane: Option<u64>,
    workspace: Option<WorkspaceInfo>,
    panes: HashMap<u64, PaneInfo>,
    tab: Option<u64>,
    leaf: Option<LeafId>,
    /// Whether the workspace held a pane since the window opened.
    had_panes: bool,
    asked: bool,
    clipboard: Option<String>,
}

pub struct Terminal {
    shared: Arc<Mutex<Shared>>,
    outgoing: Option<Sender<ToDaemon>>,
    metrics: Metrics,
    focus: FocusHandle,
    grid: (u16, u16),
    bounds: Bounds<Pixels>,
    keymap: Keymap,
    focused_once: bool,
    connected: bool,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("grid", &self.grid)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    /// The shared state, or the state a panicking reader left behind.
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn new(window: &mut Window, cx: &mut Context<Self>, keymap: Keymap) -> Self {
        let metrics = Metrics::measure(window.text_system(), px(13.0));
        let shared = Arc::new(Mutex::new(Shared {
            view: View::new(80, 24),
            pane: None,
            workspace: None,
            panes: HashMap::new(),
            tab: None,
            leaf: None,
            had_panes: false,
            asked: false,
            clipboard: None,
        }));

        let mut terminal = Self {
            shared: Arc::clone(&shared),
            outgoing: None,
            metrics,
            focus: cx.focus_handle(),
            grid: (80, 24),
            bounds: Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(0.0), px(0.0))),
            keymap,
            focused_once: false,
            connected: false,
        };
        // The task that reads the daemon needs the entity to be registered, so
        // the connection starts on the first render.
        let _ = &mut terminal;
        terminal
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if std::env::var_os("TWYX_DEBUG").is_some() {
            eprintln!("twyx-desktop: connecting");
        }
        let paths = match Paths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("twyx-desktop: {error:#}");
                return;
            }
        };
        // The window has no prompt yet, so a daemon from another build that
        // other clients use is kept rather than replaced.
        let attached = connection::attach(&paths, self.grid.0, self.grid.1, true, |others| {
            eprintln!("twyx-desktop: the daemon runs another build; {others} other clients use it");
            false
        });
        let connection = match attached {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("twyx-desktop: {error:#}");
                self.retry_connect(cx);
                return;
            }
        };

        let parts = connection.split();
        let outgoing = parts.outgoing.clone();
        self.outgoing = Some(outgoing.clone());

        // The daemon says what to show. A reconnect keeps this window's own
        // focus: the new daemon keeps the leaves.
        {
            let mut shared = self.shared();
            if let Some(leaf) = shared.leaf {
                let _ = outgoing.send(ToDaemon::FocusLeaf { leaf });
            }
            shared.workspace = None;
            shared.panes = parts
                .hello
                .panes
                .iter()
                .map(|info| (info.pane, info.clone()))
                .collect();
            shared.tab = None;
            shared.leaf = None;
        }

        let shared = Arc::clone(&self.shared);
        let incoming = parts.incoming;
        cx.spawn(async move |this, cx| {
            if std::env::var_os("TWYX_DEBUG").is_some() {
                eprintln!("twyx-desktop: reader task started");
            }
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let mut dirty = false;
                let debug = std::env::var_os("TWYX_DEBUG").is_some();
                loop {
                    let message = match incoming.try_recv() {
                        Ok(message) => message,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            // The daemon went away. Reconnect now rather than on
                            // the next paint: a covered window does not paint.
                            let _ = this.update(cx, |terminal, cx| {
                                terminal.outgoing = None;
                                terminal.connect(cx);
                            });
                            return;
                        }
                    };
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    match message {
                        FromDaemon::Workspace(workspace) => state.workspace = Some(workspace),
                        FromDaemon::Focus { tab, leaf } => {
                            state.tab = tab;
                            state.leaf = leaf;
                        }
                        FromDaemon::Snapshot { pane, body, .. } if Some(pane) == state.pane => {
                            if let Ok(snapshot) = wire::decode_body::<Snapshot>(&body) {
                                if debug {
                                    eprintln!(
                                        "snapshot {}x{}, first row {:?}",
                                        snapshot.screen.cols,
                                        snapshot.screen.rows,
                                        snapshot.rows.first().map(|row| row
                                            .cells
                                            .iter()
                                            .map(|cell| cell.text.as_str())
                                            .collect::<String>())
                                    );
                                }
                                state.view.apply_snapshot(snapshot);
                                dirty = true;
                            }
                        }
                        FromDaemon::Frame { pane, body, .. } if Some(pane) == state.pane => {
                            if let Ok(frame) = wire::decode_body::<Frame>(&body) {
                                if debug {
                                    let text: String = frame
                                        .changed
                                        .iter()
                                        .flat_map(|update| update.cells.iter())
                                        .map(|cell| cell.text.as_str())
                                        .collect();
                                    if text.trim().len() > 3 {
                                        eprintln!(
                                            "frame {}x{} changed {} rows, text {:?}",
                                            frame.screen.cols,
                                            frame.screen.rows,
                                            frame.changed.len(),
                                            text.trim()
                                        );
                                    }
                                }
                                state.view.apply_frame(frame);
                                dirty = true;
                            }
                        }
                        FromDaemon::Clipboard { text, .. } => {
                            state.clipboard = Some(text);
                            dirty = true;
                        }
                        FromDaemon::Refused { reason } => eprintln!("twyx-desktop: {reason}"),
                        FromDaemon::Pane(info) => {
                            state.panes.insert(info.pane, info);
                        }
                        FromDaemon::PaneClosed { pane } => {
                            state.panes.remove(&pane);
                        }
                        FromDaemon::Welcome(_)
                        | FromDaemon::Unsupported { .. }
                        | FromDaemon::Snapshot { .. }
                        | FromDaemon::Frame { .. } => {}
                    }
                }
                let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                let empty = state
                    .workspace
                    .as_ref()
                    .is_some_and(|workspace| workspace.tabs.is_empty());
                if empty && state.had_panes {
                    // The last pane closed: the window has nothing to show.
                    drop(state);
                    let _ = this.update(cx, |_, cx| cx.quit());
                    return;
                }
                if empty && !state.asked {
                    state.asked = true;
                    let _ = outgoing.send(ToDaemon::NewPane {
                        launch: Launch::Shell,
                        cwd: working_directory(),
                        place: Place::NewTab,
                    });
                } else if state.workspace.is_some() && !empty {
                    state.had_panes = true;
                }
                let focused = focused_pane(&state);
                if focused != state.pane {
                    state.pane = focused;
                    if let Some(pane) = focused {
                        let _ = outgoing.send(ToDaemon::Snapshot { pane });
                    }
                }
                drop(state);
                if dirty && this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Tries the connection again shortly.
    fn retry_connect(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |terminal, cx| terminal.connect(cx));
        })
        .detach();
    }

    fn send(&mut self, event: InputEvent) {
        if let Some(outgoing) = &self.outgoing {
            let _ = outgoing.send(ToDaemon::Input(event));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if keys::is_paste(&event.keystroke) {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.send(InputEvent::Paste(text));
            }
            return;
        }

        let Some(key) = keys::key_event(event) else {
            return;
        };

        let before = self.keymap.mode();
        let outcome = match key.chord() {
            Some(chord) => self.keymap.press(chord),
            None if self.keymap.mode() == Mode::Normal => Outcome::ToPane,
            None => Outcome::Consumed,
        };
        // Copy mode lives in the pane's holder; it opens and closes with the
        // keymap's copy mode.
        let after = self.keymap.mode();
        if before != after
            && (before == Mode::Copy || after == Mode::Copy)
            && let Some(outgoing) = &self.outgoing
        {
            let action = if after == Mode::Copy {
                CopyAction::Enter
            } else {
                CopyAction::Exit
            };
            let _ = outgoing.send(ToDaemon::Copy(action));
        }
        if outcome == Outcome::Run(Command::CopyYank) {
            // The holder leaves copy mode once it has copied.
            self.keymap.reset();
        }
        match outcome {
            Outcome::ToPane => self.send(InputEvent::Key(key)),
            Outcome::Consumed => {}
            Outcome::Run(Command::Detach) => cx.quit(),
            Outcome::Run(command) => {
                let request = {
                    let shared = self.shared();
                    let focused = shared.pane.and_then(|pane| shared.panes.get(&pane));
                    request(command, focused)
                };
                if let (Some(request), Some(outgoing)) = (request, &self.outgoing) {
                    let _ = outgoing.send(request);
                }
            }
        }
    }

    fn on_mouse(
        &mut self,
        button: MouseButton,
        action: MouseAction,
        position: gpui::Point<Pixels>,
    ) {
        let (col, row) = self.metrics.cell_at(self.bounds, position);
        self.send(InputEvent::Mouse(MouseEvent {
            col,
            row,
            button: match button {
                MouseButton::Left => twyx_protocol::input::MouseButton::Left,
                MouseButton::Right => twyx_protocol::input::MouseButton::Right,
                MouseButton::Middle => twyx_protocol::input::MouseButton::Middle,
                _ => twyx_protocol::input::MouseButton::None,
            },
            action,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        }));
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let lines = f32::from(event.delta.pixel_delta(px(1.0)).y);
        let button = if lines < 0.0 {
            twyx_protocol::input::MouseButton::WheelDown
        } else {
            twyx_protocol::input::MouseButton::WheelUp
        };
        let (col, row) = self.metrics.cell_at(self.bounds, event.position);
        self.send(InputEvent::Mouse(MouseEvent {
            col,
            row,
            button,
            action: MouseAction::Press,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        }));
    }

    /// Takes a clipboard write the pane asked for, and sends a resize when the
    /// window changed shape.
    fn sync(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected {
            self.connected = true;
            self.connect(cx);
        }
        self.bounds = bounds;
        let size = window.viewport_size();
        let grid = (
            self.metrics.columns(size.width),
            self.metrics.rows(size.height),
        );
        if grid != self.grid {
            // The view keeps the pane's screen at the pane's size; the pane
            // follows only if this client holds the lease.
            self.grid = grid;
            if let Some(outgoing) = &self.outgoing {
                let _ = outgoing.send(ToDaemon::Resize {
                    cols: grid.0,
                    rows: grid.1,
                });
            }
        }

        let clipboard = self.shared().clipboard.take();
        if let Some(text) = clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }

        if !self.focused_once {
            self.focused_once = true;
            self.focus.focus(window, cx);
        }
    }
}

fn focused_pane(state: &Shared) -> Option<u64> {
    let tab = state
        .workspace
        .as_ref()?
        .tabs
        .iter()
        .find(|tab| Some(tab.tab) == state.tab)?;
    Some(tab.layout.leaf(state.leaf?)?.pane.raw())
}

fn working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_owned())
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The canvas knows the pane's bounds only when it paints, so the first
        // pass uses the window's own size and the paint corrects it.
        let size = window.viewport_size();
        let bounds = Bounds::new(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(size.width, size.height),
        );
        self.sync(bounds, window, cx);

        let shared = Arc::clone(&self.shared);
        let metrics = self.metrics.clone();
        let focused = self.focus.clone();

        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    this.on_mouse(MouseButton::Left, MouseAction::Press, event.position);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, _, cx| {
                    this.on_mouse(MouseButton::Left, MouseAction::Release, event.position);
                    cx.stop_propagation();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                this.on_scroll(event);
                cx.stop_propagation();
            }))
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |bounds, bounds_state, window, cx| {
                        let view = shared
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .view
                            .clone();
                        paint::paint(&view, bounds_state, &metrics, window, cx);
                        let _ = bounds;
                        let _ = &focused;
                    },
                )
                .size_full(),
            )
    }
}

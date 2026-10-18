//! The window: one pane, drawn with GPUI, talking to the daemon.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, MouseButton, Pixels, Render,
    ScrollWheelEvent, Window, canvas, div, px,
};

use twyx_core::paths::Paths;
use twyx_protocol::client::{FromDaemon, PaneState, ToDaemon};
use twyx_protocol::connection;
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::input::{InputEvent, KeyCode, Modifiers, MouseAction, MouseEvent};
use twyx_protocol::view::View;
use twyx_protocol::wire;

use crate::keys;
use crate::paint::{self, Metrics};

/// The key twyx takes from the pane. S2 makes it configurable.
const PREFIX: char = 'g';

/// State the daemon task and the window both touch.
struct Shared {
    view: View,
    pane: Option<u64>,
    clipboard: Option<String>,
}

pub struct Terminal {
    shared: Arc<Mutex<Shared>>,
    outgoing: Option<Sender<ToDaemon>>,
    metrics: Metrics,
    focus: FocusHandle,
    grid: (u16, u16),
    bounds: Bounds<Pixels>,
    prefix_pending: bool,
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

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let metrics = Metrics::measure(window, px(13.0));
        let shared = Arc::new(Mutex::new(Shared {
            view: View::new(80, 24),
            pane: None,
            clipboard: None,
        }));

        let mut terminal = Self {
            shared: Arc::clone(&shared),
            outgoing: None,
            metrics,
            focus: cx.focus_handle(),
            grid: (80, 24),
            bounds: Bounds::new(gpui::point(px(0.0), px(0.0)), gpui::size(px(0.0), px(0.0))),
            prefix_pending: false,
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
        if let Err(error) = connection::ensure_daemon(&paths) {
            eprintln!("twyx-desktop: {error:#}");
            return;
        }
        let connection = match connection::connect(&paths.socket(), self.grid.0, self.grid.1) {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("twyx-desktop: {error:#}");
                return;
            }
        };

        let parts = connection.split();
        let outgoing = parts.outgoing.clone();
        self.outgoing = Some(outgoing.clone());

        // A client with no live pane looks dead, so ask for one.
        let live = parts
            .hello
            .panes
            .iter()
            .find(|info| info.state == PaneState::Running)
            .map(|info| info.pane);
        {
            let mut shared = self.shared();
            shared.pane = live;
            if let Some(info) = parts
                .hello
                .panes
                .iter()
                .find(|info| Some(info.pane) == live)
            {
                shared.view.resize_grid(info.cols, info.rows);
            }
        }
        if live.is_none() {
            let _ = outgoing.send(ToDaemon::NewPane {
                argv: vec![shell()],
                cwd: working_directory(),
                cols: self.grid.0,
                rows: self.grid.1,
            });
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
                while let Ok(message) = incoming.try_recv() {
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    match message {
                        FromDaemon::Welcome(hello) => {
                            if state.pane.is_none() {
                                state.pane = hello
                                    .panes
                                    .iter()
                                    .find(|info| info.state == PaneState::Running)
                                    .map(|info| info.pane);
                            }
                        }
                        // The daemon sends a snapshot before the pane message
                        // that names it, so a client with no pane adopts both.
                        FromDaemon::Snapshot { pane, body, .. }
                            if state.pane.is_none() || Some(pane) == state.pane =>
                        {
                            state.pane.get_or_insert(pane);
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
                        FromDaemon::Frame { pane, body, .. }
                            if state.pane.is_none() || Some(pane) == state.pane =>
                        {
                            state.pane.get_or_insert(pane);
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
                        FromDaemon::Pane(info) => {
                            if state.pane.is_none() && info.state == PaneState::Running {
                                state.pane = Some(info.pane);
                            }
                        }
                        FromDaemon::Clipboard { text, .. } => {
                            state.clipboard = Some(text);
                            dirty = true;
                        }
                        FromDaemon::Lease { .. }
                        | FromDaemon::Unsupported { .. }
                        | FromDaemon::Snapshot { .. }
                        | FromDaemon::Frame { .. } => {}
                    }
                }
                if dirty && this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
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

        if key.code == KeyCode::Char(PREFIX) && key.mods.contains(Modifiers::CTRL) {
            if self.prefix_pending {
                self.prefix_pending = false;
                self.send(InputEvent::Key(key));
            } else {
                self.prefix_pending = true;
            }
            return;
        }

        if self.prefix_pending {
            self.prefix_pending = false;
            if key.code == KeyCode::Char('q') && key.mods.is_empty() {
                cx.quit();
                return;
            }
            self.send(InputEvent::Key(key));
            return;
        }

        self.send(InputEvent::Key(key));
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
            self.grid = grid;
            self.shared().view.resize_grid(grid.0, grid.1);
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

fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
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

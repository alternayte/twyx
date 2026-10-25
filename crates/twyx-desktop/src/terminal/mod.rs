//! The window: the client's tab with every shown pane, the tab bar and the
//! status bar, drawn with GPUI, talking to the daemon.

mod chrome;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollWheelEvent, Window, canvas, div,
    point, px, size,
};

use twyx_core::keys::{Command, Keymap, Mode, Outcome};
use twyx_core::layout::{
    Cells, Drop, Fractions, Leaf, LeafId, Placed, Side, drop_at, frame_title, rule_at, under,
};
use twyx_core::paths::Paths;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::{
    FromDaemon, Launch, PaneInfo, Place, TabInfo, ToDaemon, WorkspaceInfo, request,
};
use twyx_protocol::connection;
use twyx_protocol::grid::{Frame, Snapshot};
use twyx_protocol::holder::CopyAction;
use twyx_protocol::input::{InputEvent, Modifiers, MouseAction, MouseEvent};
use twyx_protocol::view::View;
use twyx_protocol::wire;

use crate::keys;
use crate::paint::{self, Metrics, Tones};
use crate::panes::{self, FocusMark, Grid, Scene, Shown};

/// State the daemon task and the window both touch.
struct Shared {
    views: HashMap<u64, View>,
    /// Panes whose screen this client asked for and has not got yet.
    asked_screens: HashSet<u64>,
    workspace: Option<WorkspaceInfo>,
    panes: HashMap<u64, PaneInfo>,
    tab: Option<u64>,
    leaf: Option<LeafId>,
    /// This client's identity, for the size lease.
    client: u64,
    /// Whether the workspace held a pane since the window opened.
    had_panes: bool,
    asked: bool,
    /// The workspace the window first showed, whose first pane starts in
    /// the app's own directory.
    first: Option<u64>,
    clipboard: Option<String>,
}

impl Shared {
    fn tab_info(&self) -> Option<&TabInfo> {
        self.workspace
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)
    }

    fn title(&self, leaf: &Leaf) -> String {
        match leaf.terminal() {
            Some(pane) => self
                .panes
                .get(&pane.raw())
                .map(PaneInfo::label)
                .unwrap_or_default(),
            None => "review".to_owned(),
        }
    }
}

/// A press that the following moves continue.
#[derive(Debug, Clone, Copy)]
enum Drag {
    Rule {
        leaf: LeafId,
        side: Side,
        last: (u16, u16),
    },
    Floating {
        leaf: LeafId,
        start: (u16, u16),
        geometry: Fractions,
        resize: bool,
    },
    /// A tiled leaf dragged by its title, to drop beside another, into a
    /// stack, or into the floating layer.
    Move { leaf: LeafId },
    /// The pane took the press; its moves and release go to it too.
    Pane,
}

/// The tab as the last render placed it, for the mouse.
#[derive(Debug, Default)]
struct Placement {
    grid: Option<Grid>,
    area: Cells,
    placed: Vec<Placed>,
    focus: Option<LeafId>,
    floating: Vec<(LeafId, Fractions)>,
}

pub struct Terminal {
    shared: Arc<Mutex<Shared>>,
    outgoing: Option<Sender<ToDaemon>>,
    metrics: Metrics,
    family: &'static str,
    focus: FocusHandle,
    /// The tab area, in cells.
    grid: (u16, u16),
    keymap: Keymap,
    /// Whether the key sheet shows over the panes.
    keys: bool,
    focused_once: bool,
    connected: bool,
    placement: Placement,
    drag: Option<Drag>,
    /// Where the leaf being moved would land now.
    drop: Option<Drop>,
    mark: Option<FocusMark>,
    tab_mark: Option<panes::Travel<u64>>,
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
        let family = paint::family(window.text_system());
        let metrics = Metrics::measure(window.text_system(), family, px(DESKTOP.size.body));
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        let shared = Arc::new(Mutex::new(Shared {
            views: HashMap::new(),
            asked_screens: HashSet::new(),
            workspace: None,
            panes: HashMap::new(),
            tab: None,
            leaf: None,
            client: 0,
            had_panes: false,
            asked: false,
            first: None,
            clipboard: None,
        }));
        // The task that reads the daemon needs the entity to be registered,
        // so the connection starts on the first render.
        Self {
            shared,
            outgoing: None,
            metrics,
            family,
            focus: cx.focus_handle(),
            grid: (80, 24),
            keymap,
            keys: false,
            focused_once: false,
            connected: false,
            placement: Placement::default(),
            drag: None,
            drop: None,
            mark: None,
            tab_mark: None,
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let paths = match Paths::discover() {
            Ok(paths) => paths,
            Err(error) => {
                eprintln!("twyx-desktop: {error:#}");
                return;
            }
        };
        // The window has no prompt yet, so a daemon from another build that
        // other clients use is kept rather than replaced.
        let shown = self
            .shared()
            .workspace
            .as_ref()
            .map(|shown| shown.workspace);
        let attached =
            connection::attach(&paths, self.grid.0, self.grid.1, true, shown, |others| {
                eprintln!(
                    "twyx-desktop: the daemon runs another build; {others} other clients use it"
                );
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
        self.keymap.reset();
        self.drag = None;

        // The daemon says what to show. A reconnect keeps this window's own
        // focus: the new daemon keeps the leaves.
        {
            let mut shared = self.shared();
            if let Some(leaf) = shared.leaf {
                let _ = outgoing.send(ToDaemon::FocusLeaf { leaf });
            }
            shared.client = parts.hello.client;
            shared.workspace = None;
            shared.panes = parts
                .hello
                .panes
                .iter()
                .map(|info| (info.pane, info.clone()))
                .collect();
            shared.views.clear();
            shared.asked_screens.clear();
            shared.tab = None;
            shared.leaf = None;
        }

        let shared = Arc::clone(&self.shared);
        let incoming = parts.incoming;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(8))
                    .await;
                let mut dirty = false;
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
                    dirty = true;
                    let mut state = shared.lock().unwrap_or_else(|poison| poison.into_inner());
                    receive(&mut state, message);
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
                    let first =
                        state.workspace.as_ref().map(|shown| shown.workspace) == state.first;
                    let _ = outgoing.send(ToDaemon::NewPane {
                        launch: Launch::Shell,
                        // Elsewhere the daemon starts it in the workspace's
                        // directory.
                        cwd: if first {
                            working_directory()
                        } else {
                            String::new()
                        },
                        place: Place::NewTab,
                        history: true,
                    });
                } else if state.workspace.is_some() && !empty {
                    state.had_panes = true;
                }
                // A shown pane with no screen yet asks for one.
                let missing: Vec<u64> = state
                    .tab_info()
                    .map(|tab| tab.layout.shown())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|pane| pane.raw())
                    .filter(|pane| !state.views.contains_key(pane))
                    .collect();
                for pane in missing {
                    if state.asked_screens.insert(pane) {
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

    fn request(&self, message: ToDaemon) {
        if let Some(outgoing) = &self.outgoing {
            let _ = outgoing.send(message);
        }
    }

    fn send(&self, event: InputEvent) {
        self.request(ToDaemon::Input(event));
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
        // The key sheet takes every key while it shows; these close it.
        if self.keys {
            if matches!(
                event.keystroke.key.as_str(),
                "escape" | "enter" | "return" | "?" | "q"
            ) || event.keystroke.key_char.as_deref() == Some("?")
            {
                self.keys = false;
                cx.notify();
            }
            return;
        }

        let before = self.keymap.mode();
        let outcome = match key.chord() {
            Some(chord) => self.keymap.press(chord),
            None if self.keymap.mode() == Mode::Normal => Outcome::ToPane,
            None => Outcome::Consumed,
        };
        // Copy mode lives in the pane's holder; it opens and closes with the
        // keymap's copy mode.
        let after = self.keymap.mode();
        if before != after && (before == Mode::Copy || after == Mode::Copy) {
            self.request(ToDaemon::Copy(if after == Mode::Copy {
                CopyAction::Enter
            } else {
                CopyAction::Exit
            }));
        }
        if outcome == Outcome::Run(Command::CopyYank) {
            // The holder leaves copy mode once it has copied.
            self.keymap.reset();
        }
        if before != after {
            cx.notify();
        }
        match outcome {
            Outcome::ToPane => self.send(InputEvent::Key(key)),
            Outcome::Consumed => {}
            Outcome::Run(Command::Detach) => cx.quit(),
            Outcome::Run(Command::Help) => {
                self.keys = true;
                cx.notify();
            }
            Outcome::Run(command) => {
                let request = {
                    let shared = self.shared();
                    let focused = focused_pane(&shared).and_then(|pane| shared.panes.get(&pane));
                    request(command, focused)
                };
                if let Some(request) = request {
                    self.request(request);
                }
            }
        }
    }

    /// A mouse event in a pane's own cells, cut to its edges.
    fn to_pane(
        &self,
        placed: &Placed,
        col: u16,
        row: u16,
        button: MouseButton,
        action: MouseAction,
    ) {
        let inner = placed.inner;
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        self.send(InputEvent::Mouse(MouseEvent {
            col: col.clamp(inner.x, inner.right() - 1) - inner.x,
            row: row.clamp(inner.y, inner.bottom() - 1) - inner.y,
            button: mouse_button(button),
            action,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        }));
    }

    fn focused_placed(&self) -> Option<Placed> {
        let placement = &self.placement;
        placement
            .placed
            .iter()
            .find(|p| Some(p.leaf.id) == placement.focus && p.content)
            .copied()
    }

    /// A press on another pane focuses it. A press on a tiled pane's title
    /// row drags the pane to a new place; on a rule between panes it drags
    /// the rule; on a floating pane's top rule it moves the pane, and on its
    /// bottom right corner it resizes it. A press in the focused pane goes
    /// to it.
    fn on_press(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some((col, row)) = self.cell(event.position) else {
            return;
        };
        let placement = &self.placement;
        if let Some(floating) = placement
            .placed
            .iter()
            .rev()
            .find(|p| p.floating && p.holds(col, row))
            .copied()
        {
            let leaf = floating.leaf.id;
            if Some(leaf) != placement.focus {
                self.request(ToDaemon::FocusLeaf { leaf });
            }
            let outer = floating.outer;
            let corner = col + 1 == outer.right() && row + 1 == outer.bottom();
            let geometry = placement
                .floating
                .iter()
                .find(|(id, _)| *id == leaf)
                .map(|(_, geometry)| *geometry);
            if (row == outer.y || corner)
                && let Some(geometry) = geometry
            {
                self.drag = Some(Drag::Floating {
                    leaf,
                    start: (col, row),
                    geometry,
                    resize: corner,
                });
                return;
            }
            if Some(leaf) != placement.focus {
                return;
            }
        } else if let Some(titled) = under(&placement.placed, col, row)
            .filter(|p| !p.floating && row == p.outer.y)
            .copied()
        {
            // A title row moves its pane, even where it borders a rule.
            if Some(titled.leaf.id) != placement.focus {
                self.request(ToDaemon::FocusLeaf {
                    leaf: titled.leaf.id,
                });
            }
            self.drag = Some(Drag::Move {
                leaf: titled.leaf.id,
            });
            cx.notify();
            return;
        } else if let Some((leaf, side)) = rule_at(&placement.placed, placement.area, col, row) {
            self.drag = Some(Drag::Rule {
                leaf,
                side,
                last: (col, row),
            });
            return;
        }

        let Some(target) = under(&placement.placed, col, row).copied() else {
            return;
        };
        if Some(target.leaf.id) != placement.focus {
            self.request(ToDaemon::FocusLeaf {
                leaf: target.leaf.id,
            });
            cx.notify();
            return;
        }
        if target.local(col, row).is_some() {
            self.drag = Some(Drag::Pane);
            self.to_pane(&target, col, row, event.button, MouseAction::Press);
        }
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        if event.pressed_button.is_none() {
            self.drag = None;
            self.drop = None;
            cx.notify();
            return;
        }
        let Some((col, row)) = self.cell(event.position) else {
            return;
        };
        match drag {
            Drag::Rule { leaf, side, last } => {
                let cells = match side {
                    Side::Right => i32::from(col) - i32::from(last.0),
                    _ => i32::from(row) - i32::from(last.1),
                };
                if cells != 0 {
                    self.drag = Some(Drag::Rule {
                        leaf,
                        side,
                        last: (col, row),
                    });
                    self.request(ToDaemon::DragRule { leaf, side, cells });
                }
            }
            Drag::Floating {
                leaf,
                start,
                geometry,
                resize,
            } => {
                let area = self.placement.area;
                let dx = (f32::from(col) - f32::from(start.0)) / f32::from(area.width.max(1));
                let dy = (f32::from(row) - f32::from(start.1)) / f32::from(area.height.max(1));
                let geometry = if resize {
                    Fractions {
                        width: geometry.width + dx,
                        height: geometry.height + dy,
                        ..geometry
                    }
                } else {
                    Fractions {
                        x: geometry.x + dx,
                        y: geometry.y + dy,
                        ..geometry
                    }
                };
                self.request(ToDaemon::MoveFloating { leaf, geometry });
            }
            Drag::Move { leaf } => {
                let drop = self.drop_at(leaf, col, row, event.modifiers.alt);
                if drop != self.drop {
                    self.drop = drop;
                    cx.notify();
                }
            }
            Drag::Pane => {
                if let Some(focused) = self.focused_placed() {
                    let button = event.pressed_button.unwrap_or(MouseButton::Left);
                    self.to_pane(&focused, col, row, button, MouseAction::Motion);
                }
            }
        }
    }

    fn on_release(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some((col, row)) = self.cell(event.position) else {
            self.drag = None;
            self.drop = None;
            return;
        };
        match self.drag.take() {
            Some(Drag::Pane) => {
                if let Some(focused) = self.focused_placed() {
                    self.to_pane(&focused, col, row, event.button, MouseAction::Release);
                }
            }
            Some(Drag::Move { leaf }) => {
                if let Some(drop) = self.drop_at(leaf, col, row, event.modifiers.alt) {
                    self.request(ToDaemon::DropLeaf { leaf, drop });
                }
                self.drop = None;
                cx.notify();
            }
            _ => {}
        }
    }

    fn drop_at(&self, leaf: LeafId, col: u16, row: u16, float: bool) -> Option<Drop> {
        let placement = &self.placement;
        drop_at(&placement.placed, placement.area, leaf, col, row, float)
    }

    /// The cells a drop would give the moved leaf.
    fn preview(&self) -> Option<Cells> {
        let placement = &self.placement;
        let target = |id: LeafId| {
            placement
                .placed
                .iter()
                .find(|p| p.leaf.id == id)
                .map(|p| p.outer)
        };
        match self.drop? {
            Drop::Stack(id) => target(id),
            Drop::Beside(id, side) => {
                let outer = target(id)?;
                let (half_width, half_height) = (outer.width / 2, outer.height / 2);
                Some(match side {
                    Side::Left => Cells {
                        width: half_width,
                        ..outer
                    },
                    Side::Right => Cells {
                        x: outer.x + outer.width - half_width,
                        width: half_width,
                        ..outer
                    },
                    Side::Up => Cells {
                        height: half_height,
                        ..outer
                    },
                    Side::Down => Cells {
                        y: outer.y + outer.height - half_height,
                        height: half_height,
                        ..outer
                    },
                })
            }
            Drop::Float(geometry) => {
                let area = placement.area;
                let cells = |fraction: f32, length: u16| (fraction * f32::from(length)) as u16;
                Some(Cells {
                    x: area.x + cells(geometry.x, area.width),
                    y: area.y + cells(geometry.y, area.height),
                    width: cells(geometry.width, area.width),
                    height: cells(geometry.height, area.height),
                })
            }
        }
    }

    /// The wheel goes to the focused pane under the pointer.
    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let Some((col, row)) = self.cell(event.position) else {
            return;
        };
        let Some(focused) = self.focused_placed() else {
            return;
        };
        if focused.local(col, row).is_none() {
            return;
        }
        let up = f32::from(event.delta.pixel_delta(px(1.0)).y) >= 0.0;
        let inner = focused.inner;
        self.send(InputEvent::Mouse(MouseEvent {
            col: col - inner.x,
            row: row - inner.y,
            button: if up {
                twyx_protocol::input::MouseButton::WheelUp
            } else {
                twyx_protocol::input::MouseButton::WheelDown
            },
            action: MouseAction::Press,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        }));
    }

    /// The tab-area cell under a window point.
    fn cell(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        self.placement.grid.as_ref()?.cell(position)
    }

    /// Connects on the first render, takes a clipboard write the pane asked
    /// for, and sends a resize when the tab area changed shape.
    fn sync(&mut self, area: gpui::Size<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let grid = (
            self.metrics.columns(area.width),
            self.metrics.rows(area.height),
        );
        if grid != self.grid {
            // The panes follow only if this client holds the lease.
            self.grid = grid;
            self.request(ToDaemon::Resize {
                cols: grid.0,
                rows: grid.1,
            });
        }
        if !self.connected {
            self.connected = true;
            self.connect(cx);
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

    /// The tab area for this frame: each shown leaf, and where the mark is.
    fn scene(&mut self, grid: Grid, bounds: Bounds<Pixels>, tones: Tones, reduced: bool) -> Scene {
        let shared = Arc::clone(&self.shared);
        let shared = shared.lock().unwrap_or_else(|poison| poison.into_inner());
        let palette = paint::Palette::of(tones.theme.variant);
        let mut scene = Scene {
            grid: grid.clone(),
            bounds,
            leaves: Vec::new(),
            mark: None,
            preview: None,
            tones,
            palette,
            family: self.family,
        };
        // The preview reads the last placement, which the mouse used too.
        scene.preview = self.preview().map(|cells| grid.rect(cells));
        self.placement = Placement {
            grid: Some(grid.clone()),
            ..Placement::default()
        };
        let Some(tab) = shared.tab_info() else {
            self.mark = None;
            return scene;
        };
        // The leaves are placed at the leaseholder's tab area: another
        // client's smaller area pads them, a larger one crops them.
        let area = Cells {
            x: 0,
            y: 0,
            width: tab.cols,
            height: tab.rows,
        };
        let placed = tab.layout.place(area);
        for each in &placed {
            if !each.frame {
                continue;
            }
            let focused = Some(each.leaf.id) == shared.leaf;
            let title = shared.title(&each.leaf);
            let title = if each.floating {
                title
            } else {
                frame_title(&placed, title)
            };
            let view = each
                .leaf
                .terminal()
                .and_then(|pane| shared.views.get(&pane.raw()))
                .map(|view| {
                    let mut view = view.clone();
                    view.cursor.visible &= focused;
                    view
                });
            scene.leaves.push(Shown {
                placed: *each,
                title,
                view,
                focused,
            });
        }
        if let Some(focused) = placed
            .iter()
            .find(|p| p.frame && Some(p.leaf.id) == shared.leaf)
        {
            let to = grid.frame(focused.outer);
            let mark = self
                .mark
                .get_or_insert_with(|| FocusMark::at(focused.leaf.id, to));
            mark.go(focused.leaf.id, to, reduced);
            scene.mark = Some(mark.now(reduced));
        } else {
            self.mark = None;
        }
        self.placement = Placement {
            grid: Some(grid),
            area,
            focus: shared.leaf,
            floating: tab
                .layout
                .floating
                .iter()
                .map(|floating| (floating.leaf.id, floating.geometry))
                .collect(),
            placed,
        };
        scene
    }
}

/// Takes one message from the daemon into the shared state.
fn receive(state: &mut Shared, message: FromDaemon) {
    match message {
        FromDaemon::Workspace(workspace) => {
            let id = workspace.workspace;
            if state
                .workspace
                .as_ref()
                .is_some_and(|shown| shown.workspace != id)
            {
                // Another workspace: its emptiness is not the end of the
                // window.
                state.had_panes = false;
                state.asked = false;
            }
            state.first.get_or_insert(id);
            state.workspace = Some(workspace);
        }
        FromDaemon::Focus { tab, leaf } => {
            state.tab = tab;
            state.leaf = leaf;
        }
        FromDaemon::Snapshot { pane, body, .. } => {
            if let Ok(snapshot) = wire::decode_body::<Snapshot>(&body) {
                state.asked_screens.remove(&pane);
                state
                    .views
                    .entry(pane)
                    .or_insert_with(|| View::new(snapshot.screen.cols, snapshot.screen.rows))
                    .apply_snapshot(snapshot);
            }
        }
        FromDaemon::Frame { pane, body, .. } => {
            // A frame for a pane with no screen yet waits for its snapshot.
            if let (Some(view), Ok(frame)) = (
                state.views.get_mut(&pane),
                wire::decode_body::<Frame>(&body),
            ) {
                view.apply_frame(frame);
            }
        }
        FromDaemon::Clipboard { text, .. } => state.clipboard = Some(text),
        FromDaemon::Refused { reason } => eprintln!("twyx-desktop: {reason}"),
        FromDaemon::Pane(info) => {
            state.panes.insert(info.pane, info);
        }
        FromDaemon::PaneClosed { pane } => {
            state.panes.remove(&pane);
            state.views.remove(&pane);
            state.asked_screens.remove(&pane);
        }
        FromDaemon::Welcome(_)
        | FromDaemon::Opened { .. }
        | FromDaemon::Switched { .. }
        | FromDaemon::Overview(_)
        | FromDaemon::Inbox(_)
        | FromDaemon::Notify { .. }
        | FromDaemon::TaskStarted { .. }
        | FromDaemon::Review(_)
        | FromDaemon::Delivered { .. }
        | FromDaemon::Notes { .. }
        | FromDaemon::Checks(_)
        | FromDaemon::CheckStarted { .. }
        | FromDaemon::CheckEnded(_)
        | FromDaemon::Shipped(_)
        | FromDaemon::Synced { .. }
        | FromDaemon::WorktreeAdded { .. }
        | FromDaemon::RemovalPlan(_)
        | FromDaemon::WorktreeRemoved { .. }
        | FromDaemon::Recipes(_)
        | FromDaemon::RecipeOpened { .. }
        | FromDaemon::RecipeTrusted { .. }
        | FromDaemon::SetupFile(_)
        | FromDaemon::SetupTrusted { .. }
        | FromDaemon::SetupDone { .. }
        | FromDaemon::Unsupported { .. } => {}
    }
}

fn focused_pane(state: &Shared) -> Option<u64> {
    Some(
        state
            .tab_info()?
            .layout
            .leaf(state.leaf?)?
            .terminal()?
            .raw(),
    )
}

fn mouse_button(button: MouseButton) -> twyx_protocol::input::MouseButton {
    match button {
        MouseButton::Left => twyx_protocol::input::MouseButton::Left,
        MouseButton::Right => twyx_protocol::input::MouseButton::Right,
        MouseButton::Middle => twyx_protocol::input::MouseButton::Middle,
        _ => twyx_protocol::input::MouseButton::None,
    }
}

fn working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_owned())
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let area = size(
            viewport.width,
            (viewport.height - px(DESKTOP.tab_bar + DESKTOP.status_bar)).max(px(0.0)),
        );
        self.sync(area, window, cx);

        let tones = Tones::of(paint::variant(window.appearance()));
        let reduced = cx.reduce_motion();
        let origin = point(px(0.0), px(DESKTOP.tab_bar));
        let grid = Grid {
            origin,
            metrics: self.metrics.clone(),
        };
        let scene = self.scene(grid, Bounds::new(origin, area), tones, reduced);
        let tab_bar = self.tab_bar(tones, reduced, cx);
        let status_bar = self.status_bar(tones, cx);
        if self.mark.is_some_and(|mark| mark.moving(reduced))
            || self.tab_mark.is_some_and(|mark| mark.moving(reduced))
        {
            window.request_animation_frame();
        }
        let sheet = self.keys.then(|| self.key_sheet(tones, window));

        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(tones.paper)
            .text_color(tones.ink)
            .font_family(self.family)
            .text_size(px(DESKTOP.size.body))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .child(tab_bar)
            .child(
                div()
                    .flex_1()
                    .relative()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.on_press(event, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.on_move(event, cx);
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, _, cx| {
                            this.on_release(event, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        if !this.keys {
                            this.on_scroll(event);
                        }
                        cx.stop_propagation();
                    }))
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, cx| panes::paint(&scene, window, cx),
                        )
                        .size_full(),
                    ),
            )
            .child(status_bar)
            .children(sheet)
    }
}

//! The window: the sidebar, the client's tab with every shown pane, the tab
//! bar and the status bar, drawn with GPUI, talking to the daemon.

mod chrome;
mod dialog;
mod document;
mod files;
mod lists;
mod palette;
mod review;
mod sidebar;

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Bounds, ClipboardItem, Context, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    SystemNotification, Window, canvas, div, point, px, size,
};

use twyx_core::config::{Config, ThemeConfig, Watch};
use twyx_core::keys::{Command, Keymap, Mode, Outcome};
use twyx_core::layout::{
    Cells, Content, Drop, Fractions, Leaf, LeafId, Placed, Side, drop_at, frame_title, rule_at,
    under,
};
use twyx_core::paths::Paths;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::{
    FromDaemon, Launch, PaneInfo, Place, TabInfo, ToDaemon, WorkspaceInfo, request,
};
use twyx_protocol::connection;
use twyx_protocol::dialog::{Answer, Dialog, Purpose};
use twyx_protocol::files::Finder;
use twyx_protocol::grid::{Frame, MouseMode, Snapshot};
use twyx_protocol::holder::CopyAction;
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers, MouseAction, MouseEvent};
use twyx_protocol::palette::{Choice, Entry, Pick, Picker, Scope, entries};
use twyx_protocol::review::{Action as ReviewAction, Reviews};
use twyx_protocol::view::{Selection, View};
use twyx_protocol::wire;
use twyx_protocol::workspaces::{Effect, Workspaces};

use crate::highlight::Highlighter;
use crate::keys::{self, Shortcut};
use crate::paint::{self, Metrics, Tones};
use crate::panes::{self, FocusMark, Grid, Layer, Scene, Shown};
use document::{DocumentLeaf, Picture};
use lists::List;
use review::ReviewLeaf;

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
    /// The sidebar's rows and the worktree actions.
    workspaces: Workspaces,
    /// The question open over the panes, if any.
    dialog: Option<Dialog>,
    reviews: Reviews,
    documents: twyx_protocol::documents::Documents,
    /// Notifications the daemon asked this client to show: title, body,
    /// sound, and the pane.
    notices: Vec<(String, String, bool, u64)>,
    /// Text found in the files, for the find sheet.
    found: Vec<twyx_protocol::client::FoundText>,
}

impl Shared {
    fn tab_info(&self) -> Option<&TabInfo> {
        self.workspace
            .as_ref()?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)
    }

    /// The focused leaf and its worktree, when it is a review leaf.
    fn focused_review(&self) -> Option<(LeafId, u64)> {
        let leaf = self.leaf?;
        match self.tab_info()?.layout.leaf(leaf)?.content {
            Content::Review(worktree) => Some((leaf, worktree)),
            Content::Pane(_) | Content::Document(_) => None,
        }
    }

    /// The focused leaf, when it is a document leaf.
    fn focused_document(&self) -> Option<LeafId> {
        let leaf = self.tab_info()?.layout.leaf(self.leaf?)?;
        matches!(leaf.content, Content::Document(_)).then_some(leaf.id)
    }

    /// The document leaves in the shown tab.
    fn shown_documents(&self) -> Vec<LeafId> {
        self.tab_info()
            .into_iter()
            .flat_map(|tab| tab.layout.leaves())
            .filter(|leaf| matches!(leaf.content, Content::Document(_)))
            .map(|leaf| leaf.id)
            .collect()
    }

    /// The worktrees of the review leaves in the shown tab.
    fn shown_reviews(&self) -> Vec<u64> {
        self.tab_info()
            .into_iter()
            .flat_map(|tab| tab.layout.leaves())
            .filter_map(|leaf| match leaf.content {
                Content::Review(worktree) => Some(worktree),
                Content::Pane(_) | Content::Document(_) => None,
            })
            .collect()
    }

    fn title(&self, leaf: &Leaf) -> String {
        match &leaf.content {
            Content::Pane(pane) => self
                .panes
                .get(&pane.raw())
                .map(PaneInfo::label)
                .unwrap_or_default(),
            Content::Review(_) => "review".to_owned(),
            Content::Document(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
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
    /// Text being selected in a pane whose program did not ask for the
    /// mouse, or with shift held.
    Select,
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
    /// Panes whose program asked for the mouse.
    mouse: HashSet<u64>,
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
    theme: ThemeConfig,
    /// Reloads the keys and the theme when the config file changes.
    watch: Option<Watch>,
    /// Why the config file could not be used; the last good config stays.
    config_error: Option<String>,
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
    /// Each sidebar row's last mark change.
    fades: sidebar::Fades,
    /// Whether a sidebar mark is still fading.
    fading: bool,
    /// The selection on the open palette or list.
    sheet_mark: Option<panes::Travel<usize>>,
    /// The agent a new task starts, from config.
    default_agent: Option<String>,
    /// The palette or the switcher, while it is open.
    picker: Option<Picker>,
    /// The inbox or the checks, while one is open.
    list: Option<List>,
    /// The search of the workspace's files, while it is open.
    finder: Option<Finder>,
    /// Whether the sidebar shows the files instead of the spaces.
    files_view: bool,
    /// Whether the file tree takes the keys.
    files_focus: bool,
    /// The workspace whose files this window last asked for, and when.
    files_asked: Option<(u64, std::time::Instant)>,
    /// Text selected with the mouse, for copy.
    selection: Option<Selection>,
    /// The link from pane output under the mouse: its pane and link id.
    hover_link: Option<(u64, u32)>,
    /// The repository the app started in has had its workspace opened.
    opened_here: bool,
    /// The review leaves the last render placed.
    review_leaves: Vec<ReviewLeaf>,
    /// The document leaves the last render placed, and where each one is
    /// scrolled.
    document_leaves: Vec<DocumentLeaf>,
    document_scroll: HashMap<LeafId, gpui::ScrollHandle>,
    /// The remote images readers allowed, by address.
    pictures: HashMap<String, Picture>,
    /// The wheel's turn over a review leaf not yet taken as a hunk step.
    review_wheel: f32,
    /// Wheel travel over a pane not yet a whole line.
    pane_wheel: f32,
    highlighter: Highlighter,
    /// The unread count the Dock badge shows.
    badge: usize,
    /// The paper and ink the daemon last heard from this window.
    colours_sent: Option<twyx_protocol::holder::PaneColours>,
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

    /// A window on `config`. `watch` reloads it when its file changes;
    /// `error` says why the file at start could not be used.
    pub fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        config: Config,
        watch: Option<Watch>,
        error: Option<String>,
    ) -> Self {
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
            workspaces: Workspaces::new(true, None),
            // The first window on a machine offers the agent integrations.
            dialog: Paths::discover()
                .ok()
                .and_then(|paths| twyx_protocol::offers::first_run(&paths)),
            reviews: Reviews::default(),
            documents: Default::default(),
            notices: Vec::new(),
            found: Vec::new(),
        }));
        let this = cx.weak_entity();
        cx.on_system_notification_response(move |response, cx| {
            if let Ok(pane) = response.tag.parse::<u64>() {
                let _ = this.update(cx, |terminal, cx| {
                    terminal.jump(pane);
                    cx.activate(true);
                    cx.notify();
                });
            }
        });
        // The task that reads the daemon needs the entity to be registered,
        // so the connection starts on the first render.
        Self {
            shared,
            outgoing: None,
            metrics,
            family,
            focus: cx.focus_handle(),
            grid: (80, 24),
            keymap: config.keymap,
            theme: config.theme,
            watch,
            config_error: error,
            keys: false,
            focused_once: false,
            connected: false,
            placement: Placement::default(),
            drag: None,
            drop: None,
            mark: None,
            tab_mark: None,
            sheet_mark: None,
            fades: HashMap::new(),
            fading: false,
            default_agent: config.default_agent,
            picker: None,
            list: None,
            finder: None,
            files_view: false,
            files_focus: false,
            files_asked: None,
            selection: None,
            hover_link: None,
            opened_here: false,
            review_leaves: Vec::new(),
            document_leaves: Vec::new(),
            document_scroll: HashMap::new(),
            pictures: HashMap::new(),
            review_wheel: 0.0,
            pane_wheel: 0.0,
            highlighter: Highlighter::default(),
            badge: 0,
            colours_sent: None,
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
        let mut shown = self
            .shared()
            .workspace
            .as_ref()
            .map(|shown| shown.workspace);
        // Started in a repository, the first window shows its workspace,
        // as `twyx` does.
        if !self.opened_here {
            match connection::open_here(&paths) {
                Ok(here) => {
                    self.opened_here = true;
                    shown = shown.or(here);
                }
                Err(error) => eprintln!("twyx-desktop: {error:#}"),
            }
        }
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
        self.colours_sent = None;
        self.keymap.reset();
        self.drag = None;
        let _ = outgoing.send(ToDaemon::WatchOverview);

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
                let mut effects = Vec::new();
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
                    effects.extend(receive(&mut state, message));
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
                let shown = state.shown_reviews();
                let Shared {
                    reviews,
                    workspaces,
                    ..
                } = &mut *state;
                for request in reviews.wanted(&shown, workspaces.overview.as_ref()) {
                    let _ = outgoing.send(request);
                }
                let shown = state.shown_documents();
                for request in state.documents.wanted(&shown) {
                    let _ = outgoing.send(request);
                }
                drop(state);
                let notices = std::mem::take(
                    &mut shared
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .notices,
                );
                let updated = this.update(cx, |terminal, cx| {
                    for effect in effects {
                        terminal.apply(effect);
                    }
                    if terminal.reload() {
                        cx.notify();
                    }
                    let found = std::mem::take(&mut terminal.shared().found);
                    if let Some(finder) = terminal.finder.as_mut() {
                        for answer in found {
                            finder.answer(answer);
                        }
                    }
                    let wanted = terminal.files_view
                        || terminal
                            .picker
                            .as_ref()
                            .is_some_and(|picker| picker.scope != Scope::Switcher);
                    terminal.refresh_files(wanted);
                    let unread = terminal
                        .shared()
                        .workspaces
                        .inbox
                        .iter()
                        .filter(|item| !item.read)
                        .count();
                    if unread != terminal.badge {
                        terminal.badge = unread;
                        crate::dock::badge(unread);
                    }
                    for (title, body, sound, pane) in notices {
                        cx.show_system_notification(SystemNotification {
                            tag: pane.to_string().into(),
                            title: title.into(),
                            body: body.into(),
                            actions: Vec::new(),
                        });
                        if sound {
                            notice_sound();
                        }
                    }
                    if dirty {
                        cx.notify();
                    }
                });
                if updated.is_err() {
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

    /// Takes the config again when its file changed; a bad file keeps the
    /// last good config. Whether anything changed.
    fn reload(&mut self) -> bool {
        let Some(reloaded) = self.watch.as_mut().and_then(Watch::poll) else {
            return false;
        };
        match reloaded {
            Ok(config) => {
                self.keymap = config.keymap;
                self.theme = config.theme;
                self.default_agent = config.default_agent;
                self.config_error = None;
            }
            Err(error) => self.config_error = Some(error.brief()),
        }
        true
    }

    /// The tones for this window: the config's variant, else the system's,
    /// with the config's accent.
    fn tones(&self, window: &Window) -> Tones {
        let variant = self
            .theme
            .variant
            .unwrap_or_else(|| paint::variant(window.appearance()));
        Tones::of(variant).with_accent(self.theme.accent)
    }

    /// Sends a workspace action's requests and opens its dialog.
    fn apply(&mut self, effect: Effect) {
        for request in effect.requests {
            self.request(request);
        }
        if let Some(dialog) = effect.dialog {
            self.keymap.reset();
            self.shared().dialog = Some(dialog);
        }
        if effect.switcher {
            self.keymap.reset();
            self.picker = Some(Picker::switcher());
        }
        if effect.leave {
            self.keymap.reset();
        }
    }

    /// Takes one key for the open dialog.
    fn dialog_key(&mut self, chord: twyx_core::keys::Chord, cx: &mut Context<Self>) {
        let answer = {
            let mut shared = self.shared();
            let Some(dialog) = shared.dialog.as_mut() else {
                return;
            };
            let answer = dialog.key(chord);
            if answer != Answer::Stay {
                shared.dialog = None;
            }
            answer
        };
        let effect = {
            let mut shared = self.shared();
            let workspaces = &mut shared.workspaces;
            match answer {
                Answer::Stay | Answer::Close => return,
                Answer::Prompted { purpose, text } => workspaces.prompted(purpose, text),
                Answer::Submitted { purpose, values } => workspaces.submitted(purpose, values),
                Answer::Confirmed {
                    purpose: Purpose::Integrations,
                    ..
                } => {
                    if let Ok(paths) = Paths::discover() {
                        workspaces.message = Some(twyx_protocol::offers::install(&paths));
                    }
                    return;
                }
                Answer::Confirmed {
                    purpose: Purpose::OpenLink { target },
                    ..
                } => {
                    cx.open_url(&target);
                    return;
                }
                Answer::Confirmed { purpose, key } => workspaces.confirmed(purpose, key),
            }
        };
        self.apply(effect);
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
        match keys::shortcut(&event.keystroke) {
            Some(Shortcut::Paste) => self.paste(cx),
            Some(Shortcut::Copy) => {
                let text = self.selection.and_then(|selection| {
                    let shared = self.shared();
                    let text = shared.views.get(&selection.pane)?.selected_text(&selection);
                    (!text.is_empty()).then_some(text)
                });
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            // A shortcut runs only where a key would reach the panes.
            Some(Shortcut::Run(command))
                if self.picker.is_none()
                    && self.list.is_none()
                    && self.finder.is_none()
                    && self.shared().dialog.is_none()
                    && !self.keys =>
            {
                self.keymap.reset();
                self.run(command, cx);
            }
            Some(Shortcut::Run(_)) => {}
            None => self.key(event, cx),
        }
    }

    /// Pastes the clipboard where typing goes: the palette, a dialog, or
    /// the pane.
    fn paste(&mut self, cx: &mut Context<Self>) {
        {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                if let Some(picker) = self.picker.as_mut() {
                    picker.paste(&text.replace(['\n', '\r'], ""));
                    cx.notify();
                    return;
                }
                if let Some(finder) = self.finder.as_mut() {
                    if let twyx_protocol::files::FindKey::Send(request) =
                        finder.paste(&text.replace(['\n', '\r'], ""))
                    {
                        self.request(request);
                    }
                    cx.notify();
                    return;
                }
                let mut shared = self.shared();
                match shared.dialog.as_mut() {
                    Some(dialog) => {
                        let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
                        dialog.paste(&text);
                        cx.notify();
                    }
                    None => {
                        drop(shared);
                        self.send(InputEvent::Paste(text));
                    }
                }
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(key) = keys::key_event(event) else {
            return;
        };
        self.shared().workspaces.message = None;
        if self.shared().dialog.is_some() {
            if let Some(chord) = key.chord() {
                self.dialog_key(chord, cx);
            }
            cx.notify();
            return;
        }
        if let Some(mut picker) = self.picker.take() {
            let pick = match key.chord() {
                Some(chord) => picker.key(chord, &self.entries(picker.scope)),
                None => Pick::Stay,
            };
            match pick {
                Pick::Stay => self.picker = Some(picker),
                Pick::Close => {}
                Pick::Choose(choice) => self.choose(choice, cx),
            }
            cx.notify();
            return;
        }
        if let Some(list) = self.list.take() {
            if let Some(chord) = key.chord() {
                self.list_key(list, chord);
            } else {
                self.list = Some(list);
            }
            cx.notify();
            return;
        }
        if let Some(finder) = self.finder.take() {
            match key.chord() {
                Some(chord) => self.find_key(finder, chord),
                None => self.finder = Some(finder),
            }
            cx.notify();
            return;
        }
        // The file tree takes every key but the prefix while it has them.
        if self.files_focus
            && self.files_view
            && self.keymap.mode() == Mode::Normal
            && let Some(chord) = key.chord().filter(|chord| *chord != self.keymap.prefix)
        {
            self.tree_key(chord);
            cx.notify();
            return;
        }
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
        if before != after && after == Mode::Workspace {
            self.shared().workspaces.focus_current();
        }
        if before != after {
            cx.notify();
        }
        let document = self.shared().focused_document();
        if let (Outcome::ToPane, Some(leaf), Some(chord)) = (&outcome, document, key.chord()) {
            self.document_key(leaf, chord, cx);
            return;
        }
        let review = self.shared().focused_review();
        match (outcome, review) {
            // A review leaf takes its keys itself.
            (Outcome::ToPane, Some((leaf, worktree))) => {
                if let Some(chord) = key.chord() {
                    self.review_key(leaf, worktree, chord);
                    cx.notify();
                }
            }
            (Outcome::ToPane, None) => self.send(InputEvent::Key(key)),
            (Outcome::Consumed, _) => {}
            (Outcome::Run(command), _) => self.run(command, cx),
        }
    }

    /// Takes a key in a focused review leaf.
    fn review_key(&mut self, leaf: LeafId, worktree: u64, chord: twyx_core::keys::Chord) {
        let action = self.shared().reviews.key(leaf, worktree, chord);
        let mut shared = self.shared();
        match action {
            None => {}
            Some(ReviewAction::Send(request)) => {
                drop(shared);
                self.request(request);
            }
            Some(ReviewAction::SendNotes) => {
                let agents: Vec<PaneInfo> = shared
                    .workspace
                    .iter()
                    .flat_map(|workspace| &workspace.tabs)
                    .flat_map(|tab| tab.layout.panes())
                    .filter_map(|pane| shared.panes.get(&pane.raw()).cloned())
                    .collect();
                shared.dialog = twyx_protocol::review::notes_target(worktree, &agents);
                if shared.dialog.is_none() {
                    shared.workspaces.message = Some("no agent runs in this workspace".to_owned());
                }
            }
            Some(action) => shared.dialog = action.dialog(worktree),
        }
    }

    /// What the palette, the switcher or quick open offers now. The
    /// desktop palette also offers the shown workspace's files.
    fn entries(&self, scope: Scope) -> Vec<Entry> {
        let shared = self.shared();
        let shown = shared.workspace.as_ref().map(|shown| shown.workspace);
        let files = shared
            .workspaces
            .files
            .list
            .as_ref()
            .filter(|list| Some(list.workspace) == shown)
            .map_or(&[][..], |list| list.paths.as_slice());
        entries(
            &self.keymap,
            shared.workspace.as_ref(),
            shared.workspaces.overview.as_ref(),
            |leaf| shared.title(leaf),
            |pane| {
                shared
                    .panes
                    .get(&pane)
                    .map(PaneInfo::label)
                    .unwrap_or_default()
            },
            files,
            scope,
        )
    }

    /// Does what a palette entry stands for.
    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.picker = None;
        match choice {
            Choice::Command(command) => self.run(command, cx),
            choice => {
                if let Some(request) = choice.request() {
                    self.request(request);
                }
            }
        }
        cx.notify();
    }

    /// Runs a command from a key or the palette.
    fn run(&mut self, command: Command, cx: &mut Context<Self>) {
        match command {
            Command::Detach => cx.quit(),
            Command::Palette => {
                self.keymap.reset();
                self.picker = Some(Picker::palette());
                cx.notify();
            }
            Command::Help => {
                self.keys = true;
                cx.notify();
            }
            command @ (Command::ToggleSidebar
            | Command::SidebarUp
            | Command::SidebarDown
            | Command::SidebarOpen
            | Command::NextWorkspace
            | Command::PreviousWorkspace
            | Command::NewWorktree
            | Command::RemoveWorktree
            | Command::SetupWorktree
            | Command::Switcher) => {
                let in_mode = self.keymap.mode() == Mode::Workspace;
                let effect = {
                    let mut shared = self.shared();
                    let shown = shared.workspace.clone();
                    shared.workspaces.command(command, in_mode, shown.as_ref())
                };
                self.apply(effect);
                cx.notify();
            }
            Command::RenameTab | Command::CopySearch | Command::Resume | Command::Integrations => {
                // A search keeps copy mode: the holder finds the text there.
                if command != Command::CopySearch {
                    self.keymap.reset();
                }
                let offer = {
                    let shared = self.shared();
                    match command {
                        Command::RenameTab => {
                            Workspaces::rename_tab(shared.workspace.as_ref(), shared.tab)
                                .ok_or_else(|| "no tab to rename".to_owned())
                        }
                        Command::CopySearch => Ok(Workspaces::copy_search()),
                        Command::Resume => twyx_protocol::offers::resume_offer(
                            focused_pane(&shared).and_then(|pane| shared.panes.get(&pane)),
                            shared.workspace.as_ref(),
                            &shared.panes,
                        ),
                        _ => Paths::discover()
                            .map_err(|error| format!("{error:#}"))
                            .and_then(|paths| twyx_protocol::offers::integrations_offer(&paths)),
                    }
                };
                let mut shared = self.shared();
                match offer {
                    Ok(dialog) => shared.dialog = Some(dialog),
                    Err(why) => shared.workspaces.message = Some(why),
                }
                cx.notify();
            }
            Command::OpenFile => {
                self.keymap.reset();
                // A file made a moment ago is in the list.
                self.files_asked = None;
                self.picker = Some(Picker::files());
                cx.notify();
            }
            Command::FindInFiles => {
                self.keymap.reset();
                let workspace = self
                    .shared()
                    .workspace
                    .as_ref()
                    .map(|shown| shown.workspace);
                if let Some(workspace) = workspace {
                    self.finder = Some(Finder::new(workspace));
                }
                cx.notify();
            }
            Command::Files => {
                self.keymap.reset();
                self.toggle_files();
                cx.notify();
            }
            Command::Inbox => {
                self.keymap.reset();
                self.list = Some(List::Inbox { selected: 0 });
                cx.notify();
            }
            Command::Checks => {
                self.keymap.reset();
                let worktree = self
                    .shared()
                    .workspace
                    .as_ref()
                    .and_then(|shown| shown.worktree.as_ref())
                    .map(|worktree| worktree.worktree);
                match worktree {
                    Some(worktree) => {
                        self.request(ToDaemon::Checks { worktree });
                        self.list = Some(List::Checks {
                            worktree,
                            selected: 0,
                        });
                    }
                    None => {
                        self.shared().workspaces.message = Some("not in a Git worktree".to_owned());
                    }
                }
                cx.notify();
            }
            Command::NewTask => {
                self.keymap.reset();
                let effect = {
                    let mut shared = self.shared();
                    let shown = shared.workspace.clone();
                    shared
                        .workspaces
                        .new_task(shown.as_ref(), self.default_agent.as_deref())
                };
                self.apply(effect);
                cx.notify();
            }
            command => {
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
            .cloned()
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
            .cloned()
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
            .cloned()
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

        let Some(target) = under(&placement.placed, col, row).cloned() else {
            return;
        };
        if Some(target.leaf.id) != placement.focus {
            self.request(ToDaemon::FocusLeaf {
                leaf: target.leaf.id,
            });
            cx.notify();
            return;
        }
        let Some(local) = target.local(col, row) else {
            return;
        };
        let Some(pane) = target.leaf.terminal().map(|pane| pane.raw()) else {
            return;
        };
        self.selection = None;
        // Cmd-click (Ctrl-click on Linux) asks to open a link, also in a
        // pane that takes the mouse.
        let link_click = if cfg!(target_os = "macos") {
            event.modifiers.platform
        } else {
            event.modifiers.control
        };
        if link_click {
            let target = {
                let shared = self.shared();
                shared.views.get(&pane).and_then(|view| {
                    let cell = view.cell(local.0, local.1)?;
                    view.link(cell)
                        .filter(|target| twyx_core::text::link_allowed(target))
                        .map(str::to_owned)
                })
            };
            if let Some(target) = target {
                self.shared().dialog = Some(Dialog::open_link(&target));
                cx.notify();
                return;
            }
        }
        if self.placement.mouse.contains(&pane) && !event.modifiers.shift {
            self.drag = Some(Drag::Pane);
            self.to_pane(&target, col, row, event.button, MouseAction::Press);
        } else {
            self.drag = Some(Drag::Select);
            self.selection = Some(Selection {
                pane,
                anchor: local,
                head: local,
            });
        }
        cx.notify();
    }

    /// The pane and link id of the link from pane output at a point.
    fn link_at(&self, position: Point<Pixels>) -> Option<(u64, u32)> {
        let (col, row) = self.cell(position)?;
        let target = under(&self.placement.placed, col, row)?;
        let (local_col, local_row) = target.local(col, row)?;
        let pane = target.leaf.terminal()?.raw();
        let link = self
            .shared()
            .views
            .get(&pane)?
            .cell(local_col, local_row)?
            .link?;
        Some((pane, link))
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let hover = self.link_at(event.position);
        if hover != self.hover_link {
            self.hover_link = hover;
            cx.notify();
        }
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
            Drag::Select => {
                let Some(selection) = self.selection else {
                    return;
                };
                let Some(target) = self
                    .placement
                    .placed
                    .iter()
                    .find(|p| p.leaf.terminal().map(|pane| pane.raw()) == Some(selection.pane))
                else {
                    return;
                };
                let inner = target.inner;
                let head = (
                    col.clamp(inner.x, inner.right().saturating_sub(1)) - inner.x,
                    row.clamp(inner.y, inner.bottom().saturating_sub(1)) - inner.y,
                );
                if head != selection.head {
                    self.selection = Some(Selection { head, ..selection });
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

    /// The wheel over the focused pane scrolls a program that asked for
    /// the mouse. A full-screen program that did not gets arrow keys, and
    /// a shell's history scrolls in copy mode, as in the TUI. A wheel
    /// notch is three lines; a trackpad moves a line per line height.
    fn on_scroll(&mut self, event: &ScrollWheelEvent) {
        let Some((col, row)) = self.cell(event.position) else {
            return;
        };
        let Some(focused) = self.focused_placed() else {
            return;
        };
        let Some(pane) = focused.leaf.terminal().map(|pane| pane.raw()) else {
            return;
        };
        if focused.local(col, row).is_none() {
            return;
        }
        self.pane_wheel += match event.delta {
            ScrollDelta::Lines(lines) => lines.y * 3.0,
            ScrollDelta::Pixels(pixels) => pixels.y / self.metrics.line_height,
        };
        let lines = self.pane_wheel.trunc();
        self.pane_wheel -= lines;
        if lines == 0.0 {
            return;
        }
        let (up, count) = (lines > 0.0, lines.abs() as u32);
        let modes = self
            .shared()
            .views
            .get(&pane)
            .map(|view| (view.modes.mouse != MouseMode::Off, view.modes.alt_screen));
        let inner = focused.inner;
        match modes {
            Some((true, _)) => {
                for _ in 0..count {
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
            }
            Some((false, true)) => {
                let code = if up { KeyCode::Up } else { KeyCode::Down };
                for _ in 0..count {
                    self.send(InputEvent::Key(KeyEvent::press(code, Modifiers::empty())));
                }
            }
            Some((false, false)) => {
                self.keymap.open(Mode::Copy);
                let lines = if up { -(count as i32) } else { count as i32 };
                self.request(ToDaemon::Copy(CopyAction::Scroll(lines)));
            }
            None => {}
        }
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
        let palette = paint::Palette {
            cursor: tones.accent,
            ..paint::Palette::of(tones.theme.variant)
        };
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
            self.review_leaves.clear();
            self.document_leaves.clear();
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
        self.review_leaves = placed
            .iter()
            .filter(|p| p.frame && p.content)
            .filter_map(|p| match p.leaf.content {
                Content::Review(worktree) => {
                    let mut bounds = grid.rect(p.inner);
                    bounds.origin -= grid.origin;
                    Some(ReviewLeaf {
                        leaf: p.leaf.id,
                        worktree,
                        bounds,
                        cols: p.inner.width,
                        focused: Some(p.leaf.id) == shared.leaf,
                    })
                }
                Content::Pane(_) | Content::Document(_) => None,
            })
            .collect();
        self.document_leaves = placed
            .iter()
            .filter(|p| p.frame && p.content && matches!(p.leaf.content, Content::Document(_)))
            .map(|p| {
                let mut bounds = grid.rect(p.inner);
                bounds.origin -= grid.origin;
                DocumentLeaf {
                    leaf: p.leaf.id,
                    bounds,
                    focused: Some(p.leaf.id) == shared.leaf,
                }
            })
            .collect();
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
                    if let Some((pane, link)) = self.hover_link
                        && Some(pane) == each.leaf.terminal().map(|pane| pane.raw())
                    {
                        // The link under the mouse is underlined, all of it.
                        for cell in view.cells.iter_mut().flatten() {
                            if cell.link == Some(link) {
                                cell.underline = twyx_protocol::grid::Underline::Single;
                            }
                        }
                    }
                    view
                });
            let selection = self.selection.filter(|selection| {
                each.leaf.terminal().map(|pane| pane.raw()) == Some(selection.pane)
            });
            scene.leaves.push(Shown {
                placed: each.clone(),
                title,
                view,
                focused,
                selection,
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
            mouse: placed
                .iter()
                .filter_map(|p| p.leaf.terminal())
                .map(|pane| pane.raw())
                .filter(|pane| {
                    shared
                        .views
                        .get(pane)
                        .is_some_and(|view| view.modes.mouse != MouseMode::Off)
                })
                .collect(),
            placed,
        };
        scene
    }
}

/// Takes one message from the daemon into the shared state; an answer to a
/// workspace action can ask for more.
fn receive(state: &mut Shared, message: FromDaemon) -> Option<Effect> {
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
        answer @ (FromDaemon::Refused { .. }
        | FromDaemon::Overview(_)
        | FromDaemon::Inbox(_)
        | FromDaemon::TaskStarted { .. }
        | FromDaemon::Delivered { .. }
        | FromDaemon::WorktreeAdded { .. }
        | FromDaemon::RemovalPlan(_)
        | FromDaemon::WorktreeRemoved { .. }
        | FromDaemon::SetupFile(_)
        | FromDaemon::SetupTrusted { .. }
        | FromDaemon::SetupDone { .. }
        | FromDaemon::Checks(_)
        | FromDaemon::CheckStarted { .. }
        | FromDaemon::CheckEnded(_)
        | FromDaemon::Files(_)
        | FromDaemon::Shipped(_)) => return Some(state.workspaces.answer(&answer)),
        FromDaemon::Found(found) => state.found.push(found),
        FromDaemon::Notify {
            title,
            body,
            sound,
            pane,
        } => state.notices.push((title, body, sound, pane)),
        FromDaemon::Review(info) => state.reviews.answer(info),
        FromDaemon::Document(text) => state.documents.answer(text),
        FromDaemon::Notes { worktree, notes } => {
            state.reviews.notes.insert(worktree, notes);
        }
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
        | FromDaemon::Synced { .. }
        | FromDaemon::Recipes(_)
        | FromDaemon::RecipeOpened { .. }
        | FromDaemon::RecipeTrusted { .. }
        | FromDaemon::Unsupported { .. } => {}
    }
    None
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

/// The system's notification sound, played beside the notification.
// workaround: upstream draft gpui-notification-sound
fn notice_sound() {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("afplay", &["/System/Library/Sounds/Glass.aiff"])
    } else {
        ("canberra-gtk-play", &["--id", "message-new-instant"])
    };
    let played = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = played {
        std::thread::spawn(move || child.wait());
    }
}

fn working_directory() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".to_owned())
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (docked, overlaid) = {
            let mut shared = self.shared();
            let (workspace, leaf, panes) =
                (shared.workspace.clone(), shared.leaf, shared.panes.clone());
            shared.workspaces.refresh(workspace.as_ref(), leaf, &panes);
            let docked = shared.workspaces.docked;
            (docked, !docked && self.keymap.mode() == Mode::Workspace)
        };
        let viewport = window.viewport_size();
        let left = if docked { px(DESKTOP.sidebar) } else { px(0.0) };
        let area = size(
            (viewport.width - left).max(px(0.0)),
            (viewport.height - px(DESKTOP.tab_bar + DESKTOP.status_bar)).max(px(0.0)),
        );
        self.sync(area, window, cx);

        let tones = self.tones(window);
        // The panes' programs read the paper they are shown on.
        let sheet = twyx_core::theme::Sheet::of(tones.theme.variant);
        let colours = twyx_protocol::holder::PaneColours {
            paper: [sheet.paper.r, sheet.paper.g, sheet.paper.b],
            ink: [sheet.ink.r, sheet.ink.g, sheet.ink.b],
        };
        if self.outgoing.is_some() && self.colours_sent != Some(colours) {
            self.colours_sent = Some(colours);
            self.request(ToDaemon::Colours(colours));
        }
        let reduced = cx.reduce_motion();
        let origin = point(left, px(DESKTOP.tab_bar));
        let grid = Grid {
            origin,
            metrics: self.metrics.clone(),
        };
        let scene = Arc::new(self.scene(grid, Bounds::new(origin, area), tones, reduced));
        let review_leaves = self.review_leaves.clone();
        let floating_reviews: HashSet<LeafId> = scene
            .leaves
            .iter()
            .filter(|shown| shown.placed.floating)
            .map(|shown| shown.placed.leaf.id)
            .collect();
        let (tiled, floating): (Vec<ReviewLeaf>, Vec<ReviewLeaf>) = review_leaves
            .into_iter()
            .partition(|shown| !floating_reviews.contains(&shown.leaf));
        let tiled: Vec<_> = tiled
            .into_iter()
            .map(|shown| self.review_leaf(shown, tones, cx))
            .collect();
        let mut floating: Vec<_> = floating
            .into_iter()
            .map(|shown| self.review_leaf(shown, tones, cx))
            .collect();
        let (tiled_documents, floating_documents): (Vec<DocumentLeaf>, Vec<DocumentLeaf>) = self
            .document_leaves
            .clone()
            .into_iter()
            .partition(|shown| !floating_reviews.contains(&shown.leaf));
        let tiled: Vec<_> = tiled
            .into_iter()
            .chain(
                tiled_documents
                    .into_iter()
                    .map(|shown| self.document_leaf(shown, tones, cx)),
            )
            .collect();
        floating.extend(
            floating_documents
                .into_iter()
                .map(|shown| self.document_leaf(shown, tones, cx)),
        );
        let over = Arc::clone(&scene);
        let tab_bar = self.tab_bar(tones, reduced, cx);
        let status_bar = self.status_bar(tones, cx);
        let sheet = self.keys.then(|| self.key_sheet(tones, window));
        if self.list.is_none() && self.picker.is_none() && self.finder.is_none() {
            self.sheet_mark = None;
        }
        let list = self
            .list
            .map(|list| self.list_sheet(list, tones, reduced, window, cx));
        let dialog = self
            .shared()
            .dialog
            .clone()
            .map(|dialog| self.dialog_sheet(&dialog, tones, window));
        self.fading = false;
        let sidebar = (docked || overlaid).then(|| self.sidebar(tones, reduced, cx));
        let picker = self
            .picker
            .clone()
            .map(|picker| self.palette_sheet(&picker, tones, reduced, window, cx));
        let finder = self
            .finder
            .clone()
            .map(|finder| self.find_sheet(&finder, tones, reduced, window, cx));
        if self.mark.is_some_and(|mark| mark.moving(reduced))
            || self.tab_mark.is_some_and(|mark| mark.moving(reduced))
            || self.sheet_mark.is_some_and(|mark| mark.moving(reduced))
            || self.fading
        {
            window.request_animation_frame();
        }

        let main = div()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
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
                    // A drag let go outside the panes lands nowhere.
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if this.drag.take().is_some() {
                                this.drop = None;
                                cx.notify();
                            }
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
                            move |_, _, window, cx| {
                                panes::paint(&scene, Layer::Tiled, window, cx);
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    // The review leaves lie between the tiled panes and the
                    // floating layer.
                    .children(tiled)
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, cx| {
                                panes::paint(&over, Layer::Floating, window, cx);
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(floating),
            )
            .child(status_bar);

        let mut root = div()
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(tones.paper)
            .text_color(tones.ink)
            .font_family(self.family)
            .text_size(px(DESKTOP.size.body))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key));
        match sidebar {
            Some(sidebar) if docked => root = root.child(sidebar).child(main),
            Some(sidebar) => {
                // In workspace mode an undocked sidebar shows over the panes.
                root = root.child(main).child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(DESKTOP.tab_bar))
                        .bottom(px(DESKTOP.status_bar))
                        .child(sidebar.h_full()),
                );
            }
            None => root = root.child(main),
        }
        root.children(sheet)
            .children(picker)
            .children(finder)
            .children(list)
            .children(dialog)
    }
}

//! Builds one screen from the client's tab: the panes where the layout puts
//! them, their frames, the tab bar, the status bar and the hint bar.
//!
//! The layout is placed at the leaseholder's tab area. A client with another
//! size shows it from the top left corner, cropped or padded.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::Widget;
use twyx_core::inbox::most_urgent;
use twyx_core::keys::{Command, Keymap, Mode};
use twyx_core::layout::{Cells, Content, Fractions, Leaf, LeafId, Placed, frame_title};
use twyx_core::pane::PaneId;
use twyx_core::theme::{AgentState, SPACE};
use twyx_protocol::client::{
    InboxItem, Launch, Overview, PaneInfo, PaneState, TabInfo, WorkspaceInfo,
};
use twyx_protocol::grid::{Attrs, Cell, Colour, Cursor, Underline};
use twyx_protocol::sidebar::Row;
use twyx_protocol::view::View;

use crate::chrome::Pen;
use crate::chrome::bars::{HintBar, StatusBar, Tab, TabBar};
use crate::chrome::frames::{Frame, Frames};
use crate::chrome::sidebar::Sidebar;
use crate::document::{DocumentView, Documents, Tops};
use crate::emitter::Grid;
use crate::mouse::Selection;
use crate::overlay::Overlay;
use crate::review::{ReviewView, Reviews};

/// A buffer cell no widget wrote: the cell below shows through.
const UNTOUCHED: &str = "\u{0}";

/// Rows the chrome takes: the tab bar above the panes, the status bar below.
pub const CHROME_ROWS: u16 = 2;

/// What the client knows, for one screen.
#[derive(Debug)]
pub struct Scene<'a> {
    pub pen: &'a Pen,
    pub cols: u16,
    pub rows: u16,
    pub client: u64,
    pub workspace: Option<&'a WorkspaceInfo>,
    pub tab: Option<u64>,
    pub focus: Option<LeafId>,
    pub panes: &'a HashMap<u64, PaneInfo>,
    pub views: &'a HashMap<u64, View>,
    pub keymap: &'a Keymap,
    /// Text the mouse is selecting, shown in reverse.
    pub selection: Option<Selection>,
    /// The palette or a prompt, over everything else.
    pub overlay: Option<&'a Overlay>,
    /// A message for the status bar, such as a config error.
    pub notice: Option<&'a str>,
    /// Every project, worktree and workspace, once the daemon sent them.
    pub overview: Option<&'a Overview>,
    /// The inbox, newest first.
    pub inbox: &'a [InboxItem],
    /// The reviews that review leaves show.
    pub reviews: &'a Reviews,
    /// The files that document leaves show, and where each is scrolled.
    pub documents: &'a Documents,
    pub tops: &'a Tops,
    /// The checks of the worktree the checks overlay last asked for.
    pub checks: Option<&'a twyx_protocol::client::ChecksInfo>,
    /// The shown workspace's files, for quick open.
    pub files: &'a [String],
    pub sidebar: Option<SidebarScene<'a>>,
}

/// The sidebar on one screen.
#[derive(Debug, Clone, Copy)]
pub struct SidebarScene<'a> {
    pub rows: &'a [Row],
    pub cursor: Option<usize>,
    /// Beside the panes, which make room for it; else over them, for as
    /// long as workspace mode is open.
    pub docked: bool,
}

/// A pane's name: its title, else its program.
pub fn pane_title(panes: &HashMap<u64, PaneInfo>, pane: u64) -> String {
    panes.get(&pane).map(PaneInfo::label).unwrap_or_default()
}

/// The sidebar's width on a screen `cols` wide.
pub fn sidebar_width(cols: u16) -> u16 {
    SPACE.sidebar.min(cols / 2)
}

/// The tab area: the screen between the tab bar and the status bar, beside
/// a docked sidebar.
pub fn tab_area(cols: u16, rows: u16, docked: bool) -> (u16, u16) {
    let sidebar = if docked { sidebar_width(cols) } else { 0 };
    (
        cols.saturating_sub(sidebar).max(2),
        rows.saturating_sub(CHROME_ROWS).max(1),
    )
}

impl<'a> Scene<'a> {
    fn tab_info(&self) -> Option<&TabInfo> {
        self.workspace?
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == self.tab)
    }

    /// The leaseholder's tab area, which the layout is placed in.
    pub fn tab_size(&self) -> Option<(u16, u16)> {
        self.tab_info().map(|tab| (tab.cols, tab.rows))
    }

    pub fn floating_geometry(&self, leaf: LeafId) -> Option<Fractions> {
        self.tab_info()?
            .layout
            .floating
            .iter()
            .find(|floating| floating.leaf.id == leaf)
            .map(|floating| floating.geometry)
    }

    /// The text a selection covers: rows joined by newlines, each without
    /// its trailing blanks.
    pub fn selected_text(&self, selection: &Selection) -> String {
        self.views
            .get(&selection.pane)
            .map(|view| view.selected_text(selection))
            .unwrap_or_default()
    }

    /// The column the tab area starts at: right of a docked sidebar.
    pub fn origin(&self) -> u16 {
        match self.sidebar {
            Some(sidebar) if sidebar.docked => sidebar_width(self.cols),
            _ => 0,
        }
    }

    /// Where the sidebar is on this screen, when it shows.
    /// A docked sidebar runs from the top row, beside the tab bar; one over
    /// the panes starts below it.
    pub fn sidebar_area(&self) -> Option<Rect> {
        let top = u16::from(!self.sidebar?.docked);
        Some(Rect::new(
            0,
            top,
            sidebar_width(self.cols),
            self.rows.saturating_sub(CHROME_ROWS) + 1 - top,
        ))
    }

    /// Where each leaf of the client's tab lands on this screen.
    pub fn placed(&self) -> Vec<Placed> {
        let Some(tab) = self.tab_info() else {
            return Vec::new();
        };
        let area = Cells {
            x: self.origin(),
            y: 1,
            width: tab.cols,
            height: tab.rows,
        };
        tab.layout.place(area)
    }

    pub fn title(&self, pane: u64) -> String {
        pane_title(self.panes, pane)
    }

    /// A leaf's title: its pane's, or what the view shows.
    pub fn leaf_title(&self, leaf: &Leaf) -> String {
        match &leaf.content {
            Content::Pane(pane) => self.title(pane.raw()),
            Content::Document(path) => std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Content::Review(worktree) => {
                let name = self
                    .overview
                    .iter()
                    .flat_map(|overview| &overview.projects)
                    .flat_map(|project| &project.worktrees)
                    .find(|each| each.worktree == *worktree)
                    .map_or("worktree", |each| each.name.as_str());
                format!("review · {name}")
            }
        }
    }

    /// The checks of `worktree`, once the daemon sent them.
    pub fn checks_of(&self, worktree: u64) -> &'a [twyx_protocol::client::CheckInfo] {
        self.checks
            .filter(|info| info.worktree == worktree)
            .map_or(&[], |info| info.checks.as_slice())
    }

    /// A pane's status mark, from the overview.
    fn mark(&self, pane: u64) -> Option<AgentState> {
        self.overview?
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .find(|each| each.pane == pane)?
            .mark
    }

    /// How many panes of every workspace show each mark.
    fn roll_up(&self) -> Vec<(AgentState, usize)> {
        let mut marks = std::collections::BTreeMap::new();
        for pane in self
            .overview
            .iter()
            .flat_map(|overview| &overview.workspaces)
            .flat_map(|workspace| &workspace.panes)
        {
            if let Some(mark) = pane.mark {
                marks.insert(pane.pane, mark);
            }
        }
        AgentState::ALL
            .iter()
            .map(|state| (*state, marks.values().filter(|mark| *mark == state).count()))
            .collect()
    }

    /// The keys that run a command, from the prefix: `ctrl-g p a`.
    pub fn keys_for(&self, command: Command) -> Option<String> {
        self.keymap.keys_for(command)
    }

    /// The screen between the tab bar and the status bar, beside a docked
    /// sidebar: where the panes and the overlays go.
    pub fn panes_area(&self) -> Rect {
        let origin = self.origin();
        Rect::new(
            origin,
            1,
            self.cols.saturating_sub(origin),
            self.rows.saturating_sub(CHROME_ROWS),
        )
    }

    pub fn compose(&self) -> Grid {
        let mut grid = Grid::blank(self.cols, self.rows);
        let screen = Rect::new(0, 0, self.cols, self.rows);
        if self.rows == 0 || self.cols == 0 {
            return grid;
        }
        let pen = self.pen;
        let origin = self.origin();
        let panes_area = self.panes_area();
        let mut links = Links::default();

        let mut chrome = sentinel(screen);
        self.tab_bar(&mut chrome);
        self.status_bar(&mut chrome);
        if let (Some(sidebar), Some(area)) = (self.sidebar, self.sidebar_area())
            && sidebar.docked
        {
            self.render_sidebar(sidebar, area, &mut chrome);
        }

        let placed = self.placed();
        let tiled: Vec<&Placed> = placed.iter().filter(|p| !p.floating && p.frame).collect();
        let titles: Vec<String> = tiled
            .iter()
            .map(|p| frame_title(&placed, self.leaf_title(&p.leaf)))
            .collect();
        let frames: Vec<Frame<'_>> = tiled
            .iter()
            .zip(&titles)
            .map(|(placed, title)| Frame {
                outer: rect(placed.outer),
                title,
            })
            .collect();
        Frames {
            pen,
            frames: &frames,
            focused: tiled.iter().position(|p| Some(p.leaf.id) == self.focus),
        }
        .render(panes_area, &mut chrome);
        overlay_cells(&mut grid, &chrome);

        for placed in tiled.iter().filter(|p| p.content) {
            self.pane(&mut grid, &mut links, placed, panes_area);
        }
        for placed in placed.iter().filter(|p| p.floating) {
            clear(&mut grid, rect(placed.outer).intersection(panes_area));
            let mut layer = sentinel(screen);
            let title = self.leaf_title(&placed.leaf);
            Frames {
                pen,
                frames: &[Frame {
                    outer: rect(placed.outer),
                    title: &title,
                }],
                focused: (Some(placed.leaf.id) == self.focus).then_some(0),
            }
            .render(panes_area, &mut layer);
            overlay_cells(&mut grid, &layer);
            self.pane(&mut grid, &mut links, placed, panes_area);
        }

        if let (Some(sidebar), Some(area)) = (self.sidebar, self.sidebar_area())
            && !sidebar.docked
        {
            let mut layer = sentinel(screen);
            self.render_sidebar(sidebar, area, &mut layer);
            overlay_cells(&mut grid, &layer);
        }

        if self.keymap.mode() != Mode::Normal && panes_area.height > 1 {
            let mut layer = sentinel(screen);
            let hints = self.keymap.hints();
            let hints: Vec<(&str, &str)> = hints
                .iter()
                .map(|(key, word)| (key.as_str(), word.as_str()))
                .collect();
            HintBar { pen, hints: &hints }.render(
                Rect::new(origin, panes_area.bottom() - 1, self.cols - origin, 1),
                &mut layer,
            );
            overlay_cells(&mut grid, &layer);
        }

        let mut cursor = self.cursor(&placed, panes_area);
        if let Some(overlay) = self.overlay {
            let mut layer = sentinel(screen);
            overlay.render(self, panes_area, &mut layer);
            overlay_cells(&mut grid, &layer);
            cursor = None;
        }

        grid.links = links.targets;
        grid.cursor = cursor;
        grid
    }

    fn render_sidebar(&self, sidebar: SidebarScene<'_>, area: Rect, buf: &mut Buffer) {
        Sidebar {
            pen: self.pen,
            rows: sidebar.rows,
            cursor: sidebar.cursor,
            rule: !sidebar.docked,
        }
        .render(area, buf);
    }

    fn tab_bar(&self, buf: &mut Buffer) {
        let Some(workspace) = self.workspace else {
            return;
        };
        let names: Vec<String> = workspace
            .tabs
            .iter()
            .map(|tab| tab.label(|leaf| self.leaf_title(leaf)))
            .collect();
        let tabs: Vec<Tab<'_>> = names
            .iter()
            .zip(&workspace.tabs)
            .map(|(name, tab)| Tab {
                name,
                agent: most_urgent(
                    tab.layout
                        .leaves()
                        .iter()
                        .filter_map(|leaf| self.mark(leaf.terminal()?.raw())),
                ),
            })
            .collect();
        TabBar {
            pen: self.pen,
            tabs: &tabs,
            active: workspace
                .tabs
                .iter()
                .position(|tab| Some(tab.tab) == self.tab)
                .unwrap_or(0),
            workspace: &workspace.name,
        }
        .render(
            Rect::new(self.origin(), 0, self.cols - self.origin(), 1),
            buf,
        );
    }

    fn status_bar(&self, buf: &mut Buffer) {
        let location = self
            .workspace
            .map(WorkspaceInfo::location)
            .unwrap_or_default();
        let lease = self.tab_info().and_then(|tab| tab.lease);
        let note = match self.notice {
            Some(notice) => notice,
            None if lease.is_some() && lease != Some(self.client) => "sized by another client",
            None => "",
        };
        StatusBar {
            pen: self.pen,
            mode: self.keymap.mode().name(),
            worktree: &location.name,
            branch: &location.branch,
            dirty: location.dirty,
            note,
            roll_up: &self.roll_up(),
        }
        .render(Rect::new(0, self.rows - 1, self.cols, 1), buf);
    }

    /// Copies a pane's screen into its place, cut to the panes area. A pane
    /// that ended says so on its bottom rule.
    fn pane(&self, grid: &mut Grid, links: &mut Links, placed: &Placed, area: Rect) {
        let Some(pane) = placed.leaf.terminal().map(PaneId::raw) else {
            if let Content::Document(_) = placed.leaf.content {
                let mut layer = sentinel(Rect::new(0, 0, self.cols, self.rows));
                DocumentView {
                    pen: self.pen,
                    shown: self.documents.shown.get(&placed.leaf.id),
                    top: self
                        .tops
                        .0
                        .get(&placed.leaf.id)
                        .copied()
                        .unwrap_or_default(),
                    focused: Some(placed.leaf.id) == self.focus,
                }
                .render(rect(placed.inner).intersection(area), &mut layer);
                overlay_cells(grid, &layer);
            }
            if let Content::Review(worktree) = placed.leaf.content {
                let mut layer = sentinel(Rect::new(0, 0, self.cols, self.rows));
                ReviewView {
                    pen: self.pen,
                    info: self.reviews.infos.get(&worktree),
                    notes: self.reviews.notes.get(&worktree).map_or(&[], Vec::as_slice),
                    cursor: self
                        .reviews
                        .cursors
                        .get(&placed.leaf.id)
                        .copied()
                        .unwrap_or_default(),
                    focused: Some(placed.leaf.id) == self.focus,
                }
                .render(rect(placed.inner).intersection(area), &mut layer);
                overlay_cells(grid, &layer);
            }
            return;
        };
        let inner = rect(placed.inner).intersection(area);
        if let Some(view) = self.views.get(&pane) {
            for row in 0..inner.height {
                let y = usize::from(inner.y + row);
                let Some(source) = view.cells.get(usize::from(inner.y - placed.inner.y + row))
                else {
                    break;
                };
                let skip = usize::from(inner.x - placed.inner.x);
                for col in 0..inner.width {
                    let x = usize::from(inner.x + col);
                    let Some(cell) = source.get(skip + usize::from(col)) else {
                        break;
                    };
                    let mut cell = cell.clone();
                    // A wide cell cut at the edge cannot show its second half.
                    let last = col + 1 == inner.width;
                    if last
                        && source
                            .get(skip + usize::from(col) + 1)
                            .is_some_and(|c| c.text.is_empty())
                    {
                        cell = Cell::blank();
                    }
                    if col == 0 && cell.text.is_empty() {
                        cell = Cell::blank();
                    }
                    cell.link = cell.link.and_then(|id| links.intern(pane, id, view));
                    let local = (skip as u16 + col, inner.y - placed.inner.y + row);
                    if self.selection.is_some_and(|selection| {
                        selection.pane == pane && selection.contains(local.0, local.1)
                    }) {
                        cell.attrs ^= Attrs::REVERSE;
                    }
                    grid.cells[y][x] = cell;
                }
            }
        }
        if let Some(info) = self.panes.get(&pane)
            && info.state != PaneState::Running
        {
            self.ended(grid, placed, info, area);
        }
    }

    fn ended(&self, grid: &mut Grid, placed: &Placed, info: &PaneInfo, area: Rect) {
        let state = match &info.state {
            PaneState::Exited { code } => format!("exited {code}"),
            PaneState::Killed { signal } => format!("killed by {signal}"),
            PaneState::Lost => "ended".to_owned(),
            PaneState::Running => return,
        };
        let again = (info.launch != Launch::Unknown)
            .then(|| self.keys_for(Command::RunAgain))
            .flatten();
        let resume = info
            .resume
            .as_ref()
            .and_then(|kind| Some((self.keys_for(Command::Resume)?, format!("resume {kind}"))));
        let mut words: Vec<(&str, &str)> = vec![("", state.as_str())];
        if let Some(keys) = &again {
            words.push((keys.as_str(), "run again"));
        }
        if let Some((keys, word)) = &resume {
            words.push((keys.as_str(), word.as_str()));
        }
        let outer = rect(placed.outer);
        let row = Rect::new(outer.x, outer.bottom().saturating_sub(1), outer.width, 1);
        if outer.height < 2 || !area.contains(row.as_position()) {
            return;
        }
        let mut layer = sentinel(Rect::new(0, 0, self.cols, self.rows));
        HintBar {
            pen: self.pen,
            hints: &words,
        }
        .render(row, &mut layer);
        overlay_cells(grid, &layer);
    }

    fn cursor(&self, placed: &[Placed], area: Rect) -> Option<Cursor> {
        let focused = placed
            .iter()
            .find(|p| Some(p.leaf.id) == self.focus && p.content)?;
        let pane = focused.leaf.terminal()?.raw();
        let view = self.views.get(&pane)?;
        let state = self.panes.get(&pane)?.state.clone();
        // The copy cursor shows in copy mode; any other mode hides the cursor.
        let shown = matches!(self.keymap.mode(), Mode::Normal | Mode::Copy);
        if state != PaneState::Running || !view.cursor.visible || !shown || self.overlay.is_some() {
            return None;
        }
        let col = focused.inner.x.checked_add(view.cursor.col)?;
        let row = focused.inner.y.checked_add(view.cursor.row)?;
        let inside = rect(focused.inner).intersection(area);
        if !inside.contains(ratatui::layout::Position::new(col, row)) {
            return None;
        }
        // A floating pane above the cursor hides it.
        let covered = placed
            .iter()
            .filter(|p| p.floating && p.leaf.id != focused.leaf.id)
            .any(|p| rect(p.outer).contains(ratatui::layout::Position::new(col, row)));
        (!covered || focused.floating).then_some(Cursor {
            col,
            row,
            ..view.cursor
        })
    }
}

/// Link ids are per pane; the grid gives each target one id of its own.
#[derive(Debug, Default)]
struct Links {
    ids: HashMap<(u64, u32), u32>,
    targets: HashMap<u32, String>,
}

impl Links {
    fn intern(&mut self, pane: u64, id: u32, view: &View) -> Option<u32> {
        if let Some(found) = self.ids.get(&(pane, id)) {
            return Some(*found);
        }
        let probe = Cell {
            link: Some(id),
            ..Cell::blank()
        };
        let target = view.link(&probe)?.to_owned();
        let next = self.targets.len() as u32 + 1;
        self.ids.insert((pane, id), next);
        self.targets.insert(next, target);
        Some(next)
    }
}

fn rect(cells: Cells) -> Rect {
    Rect::new(cells.x, cells.y, cells.width, cells.height)
}

fn sentinel(area: Rect) -> Buffer {
    let mut cell = ratatui::buffer::Cell::default();
    cell.set_symbol(UNTOUCHED);
    Buffer::filled(area, cell)
}

fn clear(grid: &mut Grid, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            grid.cells[usize::from(y)][usize::from(x)] = Cell::blank();
        }
    }
}

/// Copies every cell a widget wrote into the grid.
fn overlay_cells(grid: &mut Grid, buf: &Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom().min(grid.rows) {
        for x in area.left()..area.right().min(grid.cols) {
            let cell = &buf[(x, y)];
            if cell.symbol() == UNTOUCHED {
                continue;
            }
            grid.cells[usize::from(y)][usize::from(x)] = convert(cell);
        }
    }
}

fn convert(cell: &ratatui::buffer::Cell) -> Cell {
    let colour = |color: Color| match color {
        Color::Reset => Colour::Default,
        Color::Indexed(index) => Colour::Palette(index),
        Color::Rgb(r, g, b) => Colour::Rgb { r, g, b },
        // The chrome draws only theme colours; a named one never reaches here.
        _ => Colour::Default,
    };
    let mut attrs = Attrs::empty();
    for (modifier, attr) in [
        (Modifier::BOLD, Attrs::BOLD),
        (Modifier::ITALIC, Attrs::ITALIC),
        (Modifier::CROSSED_OUT, Attrs::STRIKE),
    ] {
        if cell.modifier.contains(modifier) {
            attrs |= attr;
        }
    }
    Cell {
        text: cell.symbol().to_owned(),
        fg: colour(cell.fg),
        bg: colour(cell.bg),
        attrs,
        underline: if cell.modifier.contains(Modifier::UNDERLINED) {
            Underline::Single
        } else {
            Underline::None
        },
        ..Cell::blank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::Depth;
    use twyx_core::layout::{Axis, Layout, Leaf};
    use twyx_core::pane::PaneId;
    use twyx_core::theme::{Theme, Variant};
    use twyx_protocol::grid::{CursorShape, Modes, Row, Screen, Snapshot};

    fn view(text: &[&str], cols: u16, rows: u16) -> View {
        let mut view = View::new(cols, rows);
        let cells = (0..rows)
            .map(|row| {
                let line = text.get(usize::from(row)).copied().unwrap_or("");
                let mut cells: Vec<Cell> = line
                    .chars()
                    .map(|ch| Cell {
                        text: ch.to_string(),
                        ..Cell::blank()
                    })
                    .collect();
                cells.resize(usize::from(cols), Cell::blank());
                Row { cells }
            })
            .collect();
        view.apply_snapshot(Snapshot {
            seq: 1,
            screen: Screen {
                cols,
                rows,
                cursor: Cursor {
                    col: 1,
                    row: 0,
                    shape: CursorShape::Block,
                    visible: true,
                },
                modes: Modes::default(),
                title: String::new(),
            },
            rows: cells,
            links: Vec::new(),
            scrollback: 0,
        });
        view
    }

    fn info(pane: u64, state: PaneState) -> PaneInfo {
        PaneInfo {
            pane,
            cols: 0,
            rows: 0,
            state,
            launch: Launch::Command(vec![format!("/bin/p{pane}")]),
            cwd: "/home/n/project".to_owned(),
            title: String::new(),
            agent: None,
            resume: None,
        }
    }

    /// Two panes side by side in a `tab`x4 tab area, on a client of
    /// `cols`x`rows`.
    fn screen(cols: u16, rows: u16, tab: u16, lease: u64, states: [PaneState; 2]) -> Vec<String> {
        let leaf = |n: u64| Leaf::pane(LeafId(n), PaneId::from_raw(n));
        let mut layout = Layout::default();
        layout.split(None, leaf(1), Axis::Row);
        layout.split(Some(LeafId(1)), leaf(2), Axis::Row);
        let workspace = WorkspaceInfo {
            workspace: 1,
            name: "default".to_owned(),
            directory: "/home/n/project".to_owned(),
            worktree: None,
            tabs: vec![TabInfo {
                tab: 7,
                name: "main".to_owned(),
                layout,
                lease: Some(lease),
                cols: tab,
                rows: 4,
            }],
        };
        let [first, second] = states;
        let panes = HashMap::from([(1, info(1, first)), (2, info(2, second))]);
        let views = HashMap::from([
            (1, view(&["left", "wide"], 8, 2)),
            (2, view(&["right"], 8, 2)),
        ]);
        let pen = Pen::on_paper(Theme::sheet(Variant::Dark), Depth::Truecolor);
        let keymap = Keymap::default();
        let grid = Scene {
            pen: &pen,
            cols,
            rows,
            client: 1,
            workspace: Some(&workspace),
            tab: Some(7),
            focus: Some(LeafId(2)),
            panes: &panes,
            views: &views,
            keymap: &keymap,
            selection: None,
            overlay: None,
            notice: None,
            overview: None,
            inbox: &[],
            reviews: &Reviews::default(),
            documents: &Documents::default(),
            tops: &Tops::default(),
            checks: None,
            files: &[],
            sidebar: None,
        }
        .compose();
        grid.cells
            .iter()
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn panes_sit_inside_their_frames_between_the_bars() {
        let rows = screen(20, 6, 20, 1, [PaneState::Running, PaneState::Running]);
        // 20 columns: two boxes a blank apart and 15 inner, the spare one
        // on the left.
        assert_eq!(
            rows[1..5],
            [
                "╭─ p1 ───╮ ╭─ p2 ──╮",
                "│left    │ │right  │",
                "│wide    │ │       │",
                "╰────────╯ ╰───────╯",
            ]
        );
        assert!(rows[0].contains("1 main"), "{:?}", rows[0]);
        assert!(rows[5].contains("NORMAL"), "{:?}", rows[5]);
    }

    #[test]
    fn a_client_without_the_lease_shows_the_tab_cropped_and_says_so() {
        let rows = screen(12, 5, 20, 99, [PaneState::Running, PaneState::Running]);
        assert_eq!(rows[1], "╭─ p1 ───╮ ╭");
        assert_eq!(rows[3], "│wide    │ │");
        assert!(rows[4].contains("NORMAL"), "{:?}", rows[4]);
        let wide = screen(60, 6, 20, 99, [PaneState::Running, PaneState::Running]);
        assert!(wide[5].contains("sized by another client"), "{:?}", wide[5]);
    }

    #[test]
    fn an_ended_pane_shows_its_exit_and_the_keys_that_run_it_again() {
        let rows = screen(
            80,
            6,
            80,
            1,
            [PaneState::Exited { code: 3 }, PaneState::Running],
        );
        assert!(
            rows[4].starts_with("╰─ exited 3  ctrl-g p a run again"),
            "{:?}",
            rows[4]
        );
    }
}

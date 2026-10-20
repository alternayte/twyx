//! The keymap both clients share: the prefix, command mode, the sticky
//! modes, and what each key does in each.
//!
//! Every key goes to the focused pane except the prefix. The prefix opens
//! command mode for one key; a mode key there opens a sticky mode, which
//! stays until Esc. The prefix twice sends the prefix to the pane.

use std::fmt;
use std::str::FromStr;

use crate::layout::Side;

/// A key as the keymap sees it: shifted characters are their own keys, so
/// `H` and `|` need no shift modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

impl Chord {
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub const fn ctrl(key: Key) -> Self {
        Self {
            ctrl: true,
            ..Self::plain(key)
        }
    }
}

const NAMED: &[(&str, Key)] = &[
    ("enter", Key::Enter),
    ("tab", Key::Tab),
    ("backspace", Key::Backspace),
    ("esc", Key::Escape),
    ("delete", Key::Delete),
    ("insert", Key::Insert),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("up", Key::Up),
    ("down", Key::Down),
    ("left", Key::Left),
    ("right", Key::Right),
    ("space", Key::Char(' ')),
];

impl FromStr for Chord {
    type Err = String;

    /// `ctrl-g`, `alt-shift-left`, `H`, `|`, `f5`, `space`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut chord = Chord::plain(Key::Escape);
        let mut rest = text;
        loop {
            let lower = rest.to_ascii_lowercase();
            let modifier = ["ctrl-", "alt-", "shift-"]
                .into_iter()
                .find(|prefix| lower.starts_with(prefix) && rest.len() > prefix.len());
            match modifier {
                Some("ctrl-") => chord.ctrl = true,
                Some("alt-") => chord.alt = true,
                Some(_) => chord.shift = true,
                None => break,
            }
            rest = &rest[modifier.map_or(0, str::len)..];
        }
        let lower = rest.to_ascii_lowercase();
        chord.key = if let Some((_, key)) = NAMED.iter().find(|(name, _)| *name == lower) {
            *key
        } else if let Some(number) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
            && (1..=35).contains(&number)
        {
            Key::F(number)
        } else {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => Key::Char(ch),
                _ => return Err(format!("{text:?} is not a key")),
            }
        };
        Ok(chord)
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.alt {
            f.write_str("alt-")?;
        }
        if self.shift {
            f.write_str("shift-")?;
        }
        match self.key {
            Key::Char(' ') => f.write_str("space"),
            Key::Char(ch) => write!(f, "{ch}"),
            Key::F(number) => write!(f, "f{number}"),
            key => {
                let name = NAMED
                    .iter()
                    .find(|(_, named)| *named == key)
                    .map_or("?", |(name, _)| name);
                f.write_str(name)
            }
        }
    }
}

/// Where the keys go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Keys go to the focused pane.
    Normal,
    /// One key after the prefix.
    Command,
    Pane,
    Tab,
    Resize,
    Move,
    Floating,
    /// Moves a cursor over the pane's screen and scrollback, selects and
    /// copies.
    Copy,
    /// Moves through the sidebar's projects, worktrees, workspaces and
    /// panes, and runs worktree actions.
    Workspace,
}

impl Mode {
    /// The modes a user can bind keys in, with their config names.
    pub const BOUND: [(Mode, &'static str); 8] = [
        (Mode::Command, "command"),
        (Mode::Pane, "pane"),
        (Mode::Tab, "tab"),
        (Mode::Resize, "resize"),
        (Mode::Move, "move"),
        (Mode::Floating, "floating"),
        (Mode::Copy, "copy"),
        (Mode::Workspace, "workspace"),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Command => "command",
            Mode::Pane => "pane",
            Mode::Tab => "tab",
            Mode::Resize => "resize",
            Mode::Move => "move",
            Mode::Floating => "floating",
            Mode::Copy => "copy",
            Mode::Workspace => "workspace",
        }
    }
}

/// Where a copy mode key moves the copy cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CopyMotion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    LineStart,
    LineEnd,
    Top,
    Bottom,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
}

const MOTIONS: [(CopyMotion, &str); 14] = [
    (CopyMotion::Left, "left"),
    (CopyMotion::Right, "right"),
    (CopyMotion::Up, "up"),
    (CopyMotion::Down, "down"),
    (CopyMotion::WordForward, "word-forward"),
    (CopyMotion::WordBack, "word-back"),
    (CopyMotion::LineStart, "line-start"),
    (CopyMotion::LineEnd, "line-end"),
    (CopyMotion::Top, "top"),
    (CopyMotion::Bottom, "bottom"),
    (CopyMotion::PageUp, "page-up"),
    (CopyMotion::PageDown, "page-down"),
    (CopyMotion::HalfPageUp, "half-page-up"),
    (CopyMotion::HalfPageDown, "half-page-down"),
];

/// What a key can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    SplitRight,
    SplitDown,
    NewStacked,
    NewFloating,
    NewTab,
    ClosePane,
    RunAgain,
    /// Resume an ended pane's agent session, after a confirmation.
    Resume,
    /// Offer the agent integrations again.
    Integrations,
    Focus(Side),
    Resize(Side),
    Swap(Side),
    MoveFloating(Side),
    ToggleZoom,
    ToggleFloating,
    NextTab,
    PreviousTab,
    GoToTab(u8),
    CloseTab,
    RenameTab,
    Palette,
    Detach,
    Enter(Mode),
    CopyMove(CopyMotion),
    CopySelect,
    CopySelectLine,
    CopyYank,
    CopySearch,
    CopyNext,
    CopyPrevious,
    ToggleSidebar,
    Switcher,
    SidebarUp,
    SidebarDown,
    SidebarOpen,
    NextWorkspace,
    PreviousWorkspace,
    NewWorktree,
    RemoveWorktree,
    SetupWorktree,
}

const SIDES: [(Side, &str); 4] = [
    (Side::Left, "left"),
    (Side::Down, "down"),
    (Side::Up, "up"),
    (Side::Right, "right"),
];

impl Command {
    /// The config name, as `focus-left` or `mode-pane`.
    pub fn name(self) -> String {
        let side = |side: Side| {
            SIDES
                .iter()
                .find(|(named, _)| *named == side)
                .map_or("", |(_, name)| name)
        };
        match self {
            Command::SplitRight => "split-right".to_owned(),
            Command::SplitDown => "split-down".to_owned(),
            Command::NewStacked => "new-stacked".to_owned(),
            Command::NewFloating => "new-floating".to_owned(),
            Command::NewTab => "new-tab".to_owned(),
            Command::ClosePane => "close-pane".to_owned(),
            Command::RunAgain => "run-again".to_owned(),
            Command::Resume => "resume".to_owned(),
            Command::Integrations => "install-integrations".to_owned(),
            Command::Focus(s) => format!("focus-{}", side(s)),
            Command::Resize(s) => format!("resize-{}", side(s)),
            Command::Swap(s) => format!("swap-{}", side(s)),
            Command::MoveFloating(s) => format!("move-floating-{}", side(s)),
            Command::ToggleZoom => "toggle-zoom".to_owned(),
            Command::ToggleFloating => "toggle-floating".to_owned(),
            Command::NextTab => "next-tab".to_owned(),
            Command::PreviousTab => "previous-tab".to_owned(),
            Command::GoToTab(n) => format!("go-to-tab-{n}"),
            Command::CloseTab => "close-tab".to_owned(),
            Command::RenameTab => "rename-tab".to_owned(),
            Command::Palette => "palette".to_owned(),
            Command::Detach => "detach".to_owned(),
            Command::Enter(mode) => format!("mode-{}", mode.name()),
            Command::CopyMove(motion) => format!(
                "copy-{}",
                MOTIONS
                    .iter()
                    .find(|(named, _)| *named == motion)
                    .map_or("", |(_, name)| name)
            ),
            Command::CopySelect => "copy-select".to_owned(),
            Command::CopySelectLine => "copy-select-line".to_owned(),
            Command::CopyYank => "copy-yank".to_owned(),
            Command::CopySearch => "copy-search".to_owned(),
            Command::CopyNext => "copy-next".to_owned(),
            Command::CopyPrevious => "copy-previous".to_owned(),
            Command::ToggleSidebar => "toggle-sidebar".to_owned(),
            Command::Switcher => "switcher".to_owned(),
            Command::SidebarUp => "sidebar-up".to_owned(),
            Command::SidebarDown => "sidebar-down".to_owned(),
            Command::SidebarOpen => "sidebar-open".to_owned(),
            Command::NextWorkspace => "next-workspace".to_owned(),
            Command::PreviousWorkspace => "previous-workspace".to_owned(),
            Command::NewWorktree => "new-worktree".to_owned(),
            Command::RemoveWorktree => "remove-worktree".to_owned(),
            Command::SetupWorktree => "setup-worktree".to_owned(),
        }
    }

    /// The word the hint bar shows.
    pub fn hint(self) -> String {
        match self {
            Command::SplitRight => "right".to_owned(),
            Command::SplitDown => "down".to_owned(),
            Command::NewStacked => "stack".to_owned(),
            Command::NewFloating => "new".to_owned(),
            Command::NewTab => "new".to_owned(),
            Command::ClosePane | Command::CloseTab => "close".to_owned(),
            Command::RunAgain => "run again".to_owned(),
            Command::Resume => "resume".to_owned(),
            Command::Integrations => "integrations".to_owned(),
            Command::Focus(_) => "focus".to_owned(),
            Command::Resize(_) => "resize".to_owned(),
            Command::Swap(_) => "move".to_owned(),
            Command::MoveFloating(_) => "move".to_owned(),
            Command::ToggleZoom => "zoom".to_owned(),
            Command::ToggleFloating => "float".to_owned(),
            Command::NextTab => "next".to_owned(),
            Command::PreviousTab => "previous".to_owned(),
            Command::GoToTab(_) => "tab".to_owned(),
            Command::RenameTab => "rename".to_owned(),
            Command::Palette => "palette".to_owned(),
            Command::Detach => "detach".to_owned(),
            Command::Enter(mode) => mode.name().to_owned(),
            Command::CopyMove(CopyMotion::WordForward | CopyMotion::WordBack) => "word".to_owned(),
            Command::CopyMove(CopyMotion::LineStart | CopyMotion::LineEnd) => "line".to_owned(),
            Command::CopyMove(CopyMotion::Top | CopyMotion::Bottom) => "top/end".to_owned(),
            Command::CopyMove(_) => "move".to_owned(),
            Command::CopySelect => "select".to_owned(),
            Command::CopySelectLine => "line".to_owned(),
            Command::CopyYank => "copy".to_owned(),
            Command::CopySearch => "search".to_owned(),
            Command::CopyNext => "next".to_owned(),
            Command::CopyPrevious => "previous".to_owned(),
            Command::ToggleSidebar => "sidebar".to_owned(),
            Command::Switcher => "switch".to_owned(),
            Command::SidebarUp | Command::SidebarDown => "move".to_owned(),
            Command::SidebarOpen => "open".to_owned(),
            Command::NextWorkspace => "next".to_owned(),
            Command::PreviousWorkspace => "previous".to_owned(),
            Command::NewWorktree => "new worktree".to_owned(),
            Command::RemoveWorktree => "remove".to_owned(),
            Command::SetupWorktree => "set up".to_owned(),
        }
    }

    /// Every command, in the order the palette lists them.
    pub fn every() -> Vec<Command> {
        let mut all = vec![
            Command::SplitRight,
            Command::SplitDown,
            Command::NewStacked,
            Command::NewFloating,
            Command::NewTab,
            Command::ClosePane,
            Command::RunAgain,
            Command::Resume,
            Command::Integrations,
            Command::ToggleZoom,
            Command::ToggleFloating,
            Command::NextTab,
            Command::PreviousTab,
            Command::CloseTab,
            Command::RenameTab,
            Command::Palette,
            Command::Detach,
            Command::ToggleSidebar,
            Command::Switcher,
            Command::NextWorkspace,
            Command::PreviousWorkspace,
            Command::NewWorktree,
            Command::RemoveWorktree,
            Command::SetupWorktree,
            Command::SidebarUp,
            Command::SidebarDown,
            Command::SidebarOpen,
        ];
        for (side, _) in SIDES {
            all.extend([
                Command::Focus(side),
                Command::Resize(side),
                Command::Swap(side),
                Command::MoveFloating(side),
            ]);
        }
        all.extend((1..=9).map(Command::GoToTab));
        all.extend(MOTIONS.iter().map(|(motion, _)| Command::CopyMove(*motion)));
        all.extend([
            Command::CopySelect,
            Command::CopySelectLine,
            Command::CopyYank,
            Command::CopySearch,
            Command::CopyNext,
            Command::CopyPrevious,
        ]);
        all.extend(
            Mode::BOUND
                .iter()
                .filter(|(mode, _)| *mode != Mode::Command)
                .map(|(mode, _)| Command::Enter(*mode)),
        );
        all
    }
}

impl FromStr for Command {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Command::every()
            .into_iter()
            .find(|command| command.name() == name)
            .ok_or_else(|| format!("{name:?} is not a command"))
    }
}

/// What the client does with one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Send the key to the focused pane.
    ToPane,
    /// The key changed the mode; nothing else happens.
    Consumed,
    Run(Command),
}

/// The bindings, and the mode one client is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub prefix: Chord,
    /// Per mode, the bindings in the order the hint bar shows them.
    bindings: Vec<(Mode, Vec<(Chord, Command)>)>,
    mode: Mode,
}

impl Default for Keymap {
    fn default() -> Self {
        use Command as C;
        use Key::{Char, Down, Left, Right, Up};
        let arrows = |make: fn(Side) -> Command| {
            let mut keys = Vec::new();
            for ((ch, arrow), side) in [('h', Left), ('j', Down), ('k', Up), ('l', Right)]
                .into_iter()
                .zip([Side::Left, Side::Down, Side::Up, Side::Right])
            {
                keys.push((Chord::plain(Char(ch)), make(side)));
                keys.push((Chord::plain(arrow), make(side)));
            }
            keys
        };
        let key = |ch: char, command: Command| (Chord::plain(Char(ch)), command);

        let mut command = vec![
            key('p', C::Enter(Mode::Pane)),
            key('t', C::Enter(Mode::Tab)),
            key('r', C::Enter(Mode::Resize)),
            key('m', C::Enter(Mode::Move)),
            key('f', C::Enter(Mode::Floating)),
            key('c', C::Enter(Mode::Copy)),
            key('n', C::SplitRight),
            key('x', C::ClosePane),
            key('z', C::ToggleZoom),
            key(':', C::Palette),
            key('d', C::Detach),
            key('w', C::Enter(Mode::Workspace)),
            key('s', C::Switcher),
            key('b', C::ToggleSidebar),
        ];
        command.extend(arrows(C::Focus));
        command.extend((1..=9).map(|n| key(char::from(b'0' + n), C::GoToTab(n))));

        let mut pane = vec![
            key('r', C::SplitRight),
            key('d', C::SplitDown),
            key('s', C::NewStacked),
            key('f', C::ToggleFloating),
            key('z', C::ToggleZoom),
            key('x', C::ClosePane),
            key('a', C::RunAgain),
            key('e', C::Resume),
        ];
        pane.extend(arrows(C::Focus));

        let mut tab = vec![
            key('n', C::NewTab),
            key('r', C::RenameTab),
            key('x', C::CloseTab),
            key('h', C::PreviousTab),
            (Chord::plain(Left), C::PreviousTab),
            key('l', C::NextTab),
            (Chord::plain(Right), C::NextTab),
        ];
        tab.extend((1..=9).map(|n| key(char::from(b'0' + n), C::GoToTab(n))));

        let mut floating = vec![key('n', C::NewFloating), key('f', C::ToggleFloating)];
        floating.extend(arrows(C::MoveFloating));

        // The actions come first, so a narrow hint bar keeps them.
        let mut copy = vec![
            key('v', C::CopySelect),
            key('V', C::CopySelectLine),
            key('y', C::CopyYank),
            (Chord::plain(Key::Enter), C::CopyYank),
            key('/', C::CopySearch),
            key('n', C::CopyNext),
            key('N', C::CopyPrevious),
        ];
        copy.extend(arrows(|side| {
            C::CopyMove(match side {
                Side::Left => CopyMotion::Left,
                Side::Right => CopyMotion::Right,
                Side::Up => CopyMotion::Up,
                Side::Down => CopyMotion::Down,
            })
        }));
        copy.extend([
            key('w', C::CopyMove(CopyMotion::WordForward)),
            key('b', C::CopyMove(CopyMotion::WordBack)),
            key('0', C::CopyMove(CopyMotion::LineStart)),
            key('$', C::CopyMove(CopyMotion::LineEnd)),
            key('g', C::CopyMove(CopyMotion::Top)),
            key('G', C::CopyMove(CopyMotion::Bottom)),
            (Chord::ctrl(Char('u')), C::CopyMove(CopyMotion::HalfPageUp)),
            (
                Chord::ctrl(Char('d')),
                C::CopyMove(CopyMotion::HalfPageDown),
            ),
            (Chord::plain(Key::PageUp), C::CopyMove(CopyMotion::PageUp)),
            (
                Chord::plain(Key::PageDown),
                C::CopyMove(CopyMotion::PageDown),
            ),
        ]);

        // The actions come first, so a narrow hint bar keeps them.
        let workspace = vec![
            (Chord::plain(Key::Enter), C::SidebarOpen),
            key('n', C::NewWorktree),
            key('x', C::RemoveWorktree),
            key('u', C::SetupWorktree),
            key('s', C::Switcher),
            key('b', C::ToggleSidebar),
            key('j', C::SidebarDown),
            (Chord::plain(Down), C::SidebarDown),
            key('k', C::SidebarUp),
            (Chord::plain(Up), C::SidebarUp),
            key('l', C::NextWorkspace),
            (Chord::plain(Right), C::NextWorkspace),
            key('h', C::PreviousWorkspace),
            (Chord::plain(Left), C::PreviousWorkspace),
        ];

        Self {
            prefix: Chord::ctrl(Char('g')),
            bindings: vec![
                (Mode::Command, command),
                (Mode::Pane, pane),
                (Mode::Tab, tab),
                (Mode::Resize, arrows(C::Resize)),
                (Mode::Move, arrows(C::Swap)),
                (Mode::Floating, floating),
                (Mode::Copy, copy),
                (Mode::Workspace, workspace),
            ],
            mode: Mode::Normal,
        }
    }
}

impl Keymap {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Leaves any mode, for a client that lost its daemon or its focus.
    pub fn reset(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Opens a mode without its keys, as the mouse wheel opens copy mode.
    pub fn open(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn bindings(&self, mode: Mode) -> &[(Chord, Command)] {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == mode)
            .map_or(&[], |(_, keys)| keys.as_slice())
    }

    /// Binds `chord` in `mode`, replacing what it did there. `None` unbinds
    /// it.
    pub fn bind(&mut self, mode: Mode, chord: Chord, command: Option<Command>) {
        let Some((_, keys)) = self.bindings.iter_mut().find(|(bound, _)| *bound == mode) else {
            return;
        };
        let at = keys.iter().position(|(bound, _)| *bound == chord);
        match (at, command) {
            (Some(at), Some(command)) => keys[at].1 = command,
            (Some(at), None) => {
                keys.remove(at);
            }
            (None, Some(command)) => keys.push((chord, command)),
            (None, None) => {}
        }
    }

    /// The hint bar for the current mode: one entry per command, with its
    /// first key. Arrows and the vim keys for one command show once.
    pub fn hints(&self) -> Vec<(String, String)> {
        let mut seen: Vec<String> = Vec::new();
        let mut hints = Vec::new();
        for (chord, command) in self.bindings(self.mode) {
            let word = match command {
                Command::Focus(_)
                | Command::Resize(_)
                | Command::Swap(_)
                | Command::MoveFloating(_)
                | Command::CopyMove(
                    CopyMotion::Left | CopyMotion::Right | CopyMotion::Up | CopyMotion::Down,
                ) => {
                    let word = command.hint();
                    if seen.contains(&word) {
                        continue;
                    }
                    hints.push(("hjkl".to_owned(), word.clone()));
                    seen.push(word);
                    continue;
                }
                Command::SidebarUp | Command::SidebarDown => {
                    if !seen.iter().any(|word| word == "sidebar jk") {
                        seen.push("sidebar jk".to_owned());
                        hints.push(("jk".to_owned(), "move".to_owned()));
                    }
                    continue;
                }
                Command::NextWorkspace | Command::PreviousWorkspace => {
                    if !seen.iter().any(|word| word == "workspace hl") {
                        seen.push("workspace hl".to_owned());
                        hints.push(("hl".to_owned(), "workspace".to_owned()));
                    }
                    continue;
                }
                Command::GoToTab(_) => {
                    if seen.iter().any(|word| word == "tab 1-9") {
                        continue;
                    }
                    seen.push("tab 1-9".to_owned());
                    hints.push(("1-9".to_owned(), "tab".to_owned()));
                    continue;
                }
                command => command.hint(),
            };
            // A second key for a command already shown adds nothing.
            if hints.iter().any(|(_, shown)| *shown == word) && matches!(command, Command::CopyYank)
            {
                continue;
            }
            hints.push((chord.to_string(), word));
        }
        if self.mode != Mode::Normal {
            hints.push(("esc".to_owned(), "back".to_owned()));
        }
        hints
    }

    /// Takes one key and says what to do with it.
    pub fn press(&mut self, chord: Chord) -> Outcome {
        match self.mode {
            Mode::Normal => {
                if chord == self.prefix {
                    self.mode = Mode::Command;
                    Outcome::Consumed
                } else {
                    Outcome::ToPane
                }
            }
            Mode::Command if chord == self.prefix => {
                self.mode = Mode::Normal;
                Outcome::ToPane
            }
            mode => {
                if chord == Chord::plain(Key::Escape) || chord == self.prefix {
                    self.mode = if chord == self.prefix {
                        Mode::Command
                    } else {
                        Mode::Normal
                    };
                    return Outcome::Consumed;
                }
                let command = self
                    .bindings(mode)
                    .iter()
                    .find(|(bound, _)| *bound == chord)
                    .map(|(_, command)| *command);
                match command {
                    Some(Command::Enter(next)) => {
                        self.mode = next;
                        Outcome::Consumed
                    }
                    Some(command) => {
                        // Command mode lasts one key; the sticky modes stay.
                        if mode == Mode::Command {
                            self.mode = Mode::Normal;
                        }
                        Outcome::Run(command)
                    }
                    None => {
                        if mode == Mode::Command {
                            self.mode = Mode::Normal;
                        }
                        Outcome::Consumed
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(keymap: &mut Keymap, keys: &str) -> Vec<Outcome> {
        keys.split(' ')
            .map(|key| keymap.press(key.parse().expect("a key")))
            .collect()
    }

    #[test]
    fn chords_parse_and_print_the_same_way() {
        for text in [
            "ctrl-g",
            "alt-shift-left",
            "H",
            "|",
            "f5",
            "space",
            "esc",
            "-",
        ] {
            let chord: Chord = text.parse().expect("a chord");
            assert_eq!(chord.to_string(), text);
        }
        assert_eq!("ctrl--".parse::<Chord>(), Ok(Chord::ctrl(Key::Char('-'))));
        assert!("ctrl-".parse::<Chord>().is_err());
        assert!("hello".parse::<Chord>().is_err());
        assert!("f36".parse::<Chord>().is_err());
    }

    #[test]
    fn command_mode_takes_one_key_and_sticky_modes_stay() {
        let mut keymap = Keymap::default();
        assert_eq!(
            press(&mut keymap, "a ctrl-g n a"),
            [
                Outcome::ToPane,
                Outcome::Consumed,
                Outcome::Run(Command::SplitRight),
                Outcome::ToPane
            ]
        );
        assert_eq!(
            press(&mut keymap, "ctrl-g p h l esc a"),
            [
                Outcome::Consumed,
                Outcome::Consumed,
                Outcome::Run(Command::Focus(Side::Left)),
                Outcome::Run(Command::Focus(Side::Right)),
                Outcome::Consumed,
                Outcome::ToPane
            ]
        );
    }

    #[test]
    fn the_prefix_twice_reaches_the_pane_and_an_unbound_key_is_dropped() {
        let mut keymap = Keymap::default();
        assert_eq!(
            press(&mut keymap, "ctrl-g ctrl-g"),
            [Outcome::Consumed, Outcome::ToPane]
        );
        assert_eq!(keymap.mode(), Mode::Normal);
        assert_eq!(
            press(&mut keymap, "ctrl-g q a"),
            [Outcome::Consumed, Outcome::Consumed, Outcome::ToPane]
        );
    }

    #[test]
    fn the_prefix_in_a_sticky_mode_returns_to_command_mode() {
        let mut keymap = Keymap::default();
        press(&mut keymap, "ctrl-g r");
        assert_eq!(keymap.mode(), Mode::Resize);
        assert_eq!(
            press(&mut keymap, "ctrl-g t"),
            [Outcome::Consumed, Outcome::Consumed]
        );
        assert_eq!(keymap.mode(), Mode::Tab);
    }

    #[test]
    fn every_command_name_reads_back() {
        for command in Command::every() {
            assert_eq!(command.name().parse::<Command>(), Ok(command));
        }
    }

    #[test]
    fn hints_show_each_command_once_and_end_with_esc() {
        let mut keymap = Keymap::default();
        press(&mut keymap, "ctrl-g r");
        assert_eq!(
            keymap.hints(),
            [
                ("hjkl".to_owned(), "resize".to_owned()),
                ("esc".to_owned(), "back".to_owned())
            ]
        );
    }

    /// The binding tables of `docs/keys.md`, from the defaults.
    fn tables(keymap: &Keymap) -> String {
        let mut out = String::new();
        for (mode, name) in Mode::BOUND {
            out.push_str(&format!("### {name}\n\n| Key | Command |\n|---|---|\n"));
            for (chord, command) in keymap.bindings(mode) {
                let key = match chord.to_string().as_str() {
                    "|" => "\\|".to_owned(),
                    key => key.to_owned(),
                };
                out.push_str(&format!("| `{key}` | `{}` |\n", command.name()));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_key_docs_list_every_default_binding() {
        let docs = include_str!("../../../docs/keys.md");
        let tables = tables(&Keymap::default());
        assert!(
            docs.contains(&tables),
            "docs/keys.md is out of date; its tables should read:\n\n{tables}"
        );
    }
}

//! Direction: a draughtsman's drawing sheet: graphite hairlines and a ruled
//! title block drawn on the user's own paper, greys mixed from the user's own
//! ink, and one blue pencil mark for where the hand is; nothing is filled and
//! nothing moves, except on the desktop, where the pencil mark travels to
//! its new place.
//!
//! The theme tokens. Every colour, glyph and spacing value the chrome uses is
//! read from here.

/// One colour in sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(value: u32) -> Self {
        Self {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }

    /// `self` moved toward `other` by `amount`, from 0.0 (self) to 1.0 (other).
    pub fn mix(self, other: Self, amount: f32) -> Self {
        let channel =
            |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
        Self {
            r: channel(self.r, other.r),
            g: channel(self.g, other.g),
            b: channel(self.b, other.b),
        }
    }

    /// Perceived lightness, 0.0 black to 1.0 white.
    pub fn lightness(self) -> f32 {
        (0.2126 * f32::from(self.r) + 0.7152 * f32::from(self.g) + 0.0722 * f32::from(self.b))
            / 255.0
    }

    /// The nearest colour in the xterm 256-colour cube, or in its grey ramp
    /// for a colour with almost no hue. The first
    /// sixteen entries are left out, because every user sets them differently.
    pub fn to_palette(self) -> u8 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let nearest_level = |value: u8| {
            (0..6)
                .min_by_key(|&index| LEVELS[index].abs_diff(value))
                .unwrap_or(0)
        };
        let (r, g, b) = (
            nearest_level(self.r),
            nearest_level(self.g),
            nearest_level(self.b),
        );
        let cube = Self {
            r: LEVELS[r],
            g: LEVELS[g],
            b: LEVELS[b],
        };
        let grey_step = (((u16::from(self.r) + u16::from(self.g) + u16::from(self.b)) / 3)
            .saturating_sub(3)
            / 10)
            .min(23) as u8;
        let grey_value = 8 + 10 * grey_step;
        let grey = Self {
            r: grey_value,
            g: grey_value,
            b: grey_value,
        };
        // A colour with a visible hue keeps its hue, even where a grey is
        // nearer by distance.
        let chroma = self.r.max(self.g).max(self.b) - self.r.min(self.g).min(self.b);
        if chroma < 24 && self.distance(grey) < self.distance(cube) {
            232 + grey_step
        } else {
            16 + 36 * r as u8 + 6 * g as u8 + b as u8
        }
    }

    fn distance(self, other: Self) -> u32 {
        let d = |a: u8, b: u8| u32::from(a.abs_diff(b)).pow(2);
        d(self.r, other.r) + d(self.g, other.g) + d(self.b, other.b)
    }
}

impl std::str::FromStr for Rgb {
    type Err = String;

    /// `#rrggbb`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text
            .strip_prefix('#')
            .filter(|digits| digits.len() == 6)
            .ok_or_else(|| format!("{text:?} is not a colour; write it as #rrggbb"))?;
        u32::from_str_radix(digits, 16)
            .map(Rgb::hex)
            .map_err(|_| format!("{text:?} is not a colour; write it as #rrggbb"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Light,
    Dark,
}

/// A colour the chrome draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The terminal's own foreground. Exact on any terminal.
    Ink,
    Rgb(Rgb),
}

/// A text style: the type of a cell grid is weight and tone, never a font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub tone: Tone,
    pub bold: bool,
}

impl Face {
    const fn plain(tone: Tone) -> Self {
        Self { tone, bold: false }
    }

    const fn bold(tone: Tone) -> Self {
        Self { tone, bold: true }
    }
}

/// The text roles. Mode names are set in capitals, like a title block's
/// field labels; every other text keeps the case the user gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Type {
    /// The name of the focused tab or pane.
    pub title: Face,
    /// Any other text the user reads.
    pub label: Face,
    /// Secondary text: unfocused tab names, status fields, descriptions.
    pub caption: Face,
    /// Keys in the hint bar and the palette.
    pub key: Face,
    /// The mode name in the status bar.
    pub mode: Face,
    /// The matched characters in a palette entry.
    pub matched: Face,
}

/// Hairlines, rounded corners. Each tiled pane is its own box; the panes of
/// a stack share their rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub tee_down: char,
    pub tee_up: char,
    pub tee_right: char,
    pub tee_left: char,
    pub cross: char,
}

pub const RULES: Rules = Rules {
    horizontal: '─',
    vertical: '│',
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    tee_down: '┬',
    tee_up: '┴',
    tee_right: '├',
    tee_left: '┤',
    cross: '┼',
};

/// Spacing, in cells. One cell is the unit; a field is padded by one cell
/// and fields are kept apart by one rule with a cell on each side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub pad: u16,
    pub gap: u16,
    /// The sidebar's width, its rule included.
    pub sidebar: u16,
    /// The blank cells between two tiled pane boxes.
    pub panes: u16,
}

pub const SPACE: Space = Space {
    pad: 1,
    gap: 1,
    sidebar: 30,
    panes: 1,
};

/// The desktop client's measures, in logical pixels, and its one motion.
///
/// The sheet is lettered in one technical face: monospaced, one stroke
/// weight plus a semibold, like stencil lettering on a drawing. Sizes step by
/// a ratio of 1.125 from the pane text, so the hierarchy comes from weight and
/// case more than size. Every measure is a multiple of a 4 px unit; rules are
/// 1 px hairlines and corners are square.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Desktop {
    /// Font families in order of preference; the first one installed wins.
    pub fonts: &'static [&'static str],
    pub size: Sizes,
    pub unit: f32,
    /// A hairline: borders, separators, the title block's rules.
    pub rule: f32,
    /// The blue pencil mark: the active tab's underline and the focused
    /// pane's border.
    pub mark: f32,
    pub pad: f32,
    pub gap: f32,
    pub sidebar: f32,
    pub tab_bar: f32,
    pub status_bar: f32,
    pub palette_width: f32,
    pub motion: Motion,
}

/// Type sizes, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sizes {
    pub caption: f32,
    /// Pane text, chrome labels and titles.
    pub body: f32,
    /// Document headings, smallest to largest.
    pub heading: [f32; 3],
}

/// How the pencil mark moves. Reduced motion makes every duration zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    /// The mark travelling to a new tab, pane or palette entry, with an
    /// ease-out curve.
    pub travel: std::time::Duration,
    /// An agent state mark changing colour.
    pub fade: std::time::Duration,
}

impl Motion {
    /// Where a move is after `elapsed`, from 0.0 to 1.0: a cubic ease-out.
    pub fn ease(duration: std::time::Duration, elapsed: std::time::Duration) -> f32 {
        if duration.is_zero() || elapsed >= duration {
            return 1.0;
        }
        let t = elapsed.as_secs_f32() / duration.as_secs_f32();
        1.0 - (1.0 - t).powi(3)
    }

    /// The same motion with every duration zero.
    pub fn reduced(self) -> Self {
        Self {
            travel: std::time::Duration::ZERO,
            fade: std::time::Duration::ZERO,
        }
    }
}

pub const DESKTOP: Desktop = Desktop {
    fonts: &[
        "JetBrains Mono",
        "SF Mono",
        "Menlo",
        "DejaVu Sans Mono",
        "Liberation Mono",
    ],
    size: Sizes {
        caption: 11.5,
        body: 13.0,
        heading: [14.5, 16.5, 18.5],
    },
    unit: 4.0,
    rule: 1.0,
    mark: 2.0,
    pad: 8.0,
    gap: 8.0,
    sidebar: 240.0,
    tab_bar: 28.0,
    status_bar: 24.0,
    palette_width: 560.0,
    motion: Motion {
        travel: std::time::Duration::from_millis(120),
        fade: std::time::Duration::from_millis(120),
    },
};

/// The state of an agent session, as the sidebar, inbox and status bar
/// show it. `ALL` lists them from the most urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum AgentState {
    NeedsApproval,
    Failed,
    Done,
    Working,
    Idle,
    Unknown,
}

impl AgentState {
    pub const ALL: [Self; 6] = [
        Self::NeedsApproval,
        Self::Failed,
        Self::Done,
        Self::Working,
        Self::Idle,
        Self::Unknown,
    ];

    /// The glyph that carries the state without colour.
    pub fn glyph(self) -> char {
        match self {
            Self::NeedsApproval => '!',
            Self::Failed => '✗',
            Self::Done => '✓',
            Self::Working => '●',
            Self::Idle => '○',
            Self::Unknown => '?',
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::NeedsApproval => "needs approval",
            Self::Failed => "failed",
            Self::Done => "done",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }
}

/// A sheet twyx paints itself: the paper, the ink and the sixteen terminal
/// colours. The TUI paints one only in the specimen; the desktop client
/// paints one always.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sheet {
    pub paper: Rgb,
    pub ink: Rgb,
    pub ansi: [Rgb; 16],
}

/// Drafting vellum and graphite.
pub const LIGHT: Sheet = Sheet {
    paper: Rgb::hex(0xF4F1EA),
    ink: Rgb::hex(0x2B2B2B),
    ansi: [
        Rgb::hex(0x2B2B2B),
        Rgb::hex(0xB3382B),
        Rgb::hex(0x3F7A2E),
        Rgb::hex(0x8A6400),
        Rgb::hex(0x1F5FD1),
        Rgb::hex(0x8A3FA8),
        Rgb::hex(0x1A7373),
        Rgb::hex(0xD8D3C8),
        Rgb::hex(0x6B6B6B),
        Rgb::hex(0xD2503F),
        Rgb::hex(0x52933D),
        Rgb::hex(0xA87C10),
        Rgb::hex(0x3A78E6),
        Rgb::hex(0xA457C2),
        Rgb::hex(0x2A8C8C),
        Rgb::hex(0xFFFDF8),
    ],
};

/// Graphite board and chalk-white ink.
pub const DARK: Sheet = Sheet {
    paper: Rgb::hex(0x1A1B1E),
    ink: Rgb::hex(0xD9D6CF),
    ansi: [
        Rgb::hex(0x1A1B1E),
        Rgb::hex(0xE0675A),
        Rgb::hex(0x8DBF76),
        Rgb::hex(0xD9B35B),
        Rgb::hex(0x6FA3EF),
        Rgb::hex(0xC38FD6),
        Rgb::hex(0x6CBFBF),
        Rgb::hex(0xD9D6CF),
        Rgb::hex(0x5C5F66),
        Rgb::hex(0xF08578),
        Rgb::hex(0xA6D68F),
        Rgb::hex(0xEBC877),
        Rgb::hex(0x92BBFA),
        Rgb::hex(0xD6A9E6),
        Rgb::hex(0x8ED6D6),
        Rgb::hex(0xF4F1EA),
    ],
};

impl Sheet {
    pub fn of(variant: Variant) -> &'static Self {
        match variant {
            Variant::Light => &LIGHT,
            Variant::Dark => &DARK,
        }
    }
}

/// How far each grey of the ramp sits from the paper toward the ink.
const GRAPHITE: f32 = 0.68;
const RULE: f32 = 0.40;
/// How far the selection bar sits from the paper toward the accent.
const SELECTION: f32 = 0.16;
/// How far a diff line's hue is moved toward the graphite grey, to mute it.
const DIFF: f32 = 0.25;

/// The resolved tokens for one paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub variant: Variant,
    /// The paper: lettering on the active tab chip.
    pub paper: Rgb,
    /// The tint under the selected sidebar row.
    pub selection: Rgb,
    /// Secondary text.
    pub graphite: Rgb,
    /// Unfocused borders and separators.
    pub rule: Rgb,
    /// The blue pencil: the focused pane's border, the focused tab, the mode.
    pub accent: Rgb,
    pub needs_approval: Rgb,
    pub failed: Rgb,
    pub done: Rgb,
    /// Added and removed lines of a diff.
    pub added: Rgb,
    pub removed: Rgb,
    pub type_: Type,
}

impl Theme {
    /// The tokens for the user's own paper and ink, as the terminal reported
    /// them. A terminal that does not answer gets the reference sheet of
    /// `fallback`.
    pub fn on(paper: Option<Rgb>, ink: Option<Rgb>, fallback: Variant) -> Self {
        let variant = match paper {
            Some(paper) if paper.lightness() >= 0.5 => Variant::Light,
            Some(_) => Variant::Dark,
            None => fallback,
        };
        let sheet = Sheet::of(variant);
        Self::build(
            variant,
            paper.unwrap_or(sheet.paper),
            ink.unwrap_or(sheet.ink),
        )
    }

    /// The tokens for the user's paper and ink, with the variant chosen in
    /// config rather than read from the paper.
    pub fn forced(variant: Variant, paper: Option<Rgb>, ink: Option<Rgb>) -> Self {
        let sheet = Sheet::of(variant);
        Self::build(
            variant,
            paper.unwrap_or(sheet.paper),
            ink.unwrap_or(sheet.ink),
        )
    }

    /// The same tokens with another accent.
    pub fn with_accent(mut self, accent: Rgb) -> Self {
        self.accent = accent;
        self.type_.mode.tone = Tone::Rgb(accent);
        self.type_.matched.tone = Tone::Rgb(accent);
        self
    }

    /// The tokens for a sheet twyx paints itself.
    pub fn sheet(variant: Variant) -> Self {
        let sheet = Sheet::of(variant);
        Self::build(variant, sheet.paper, sheet.ink)
    }

    fn build(variant: Variant, paper: Rgb, ink: Rgb) -> Self {
        let ansi = Sheet::of(variant).ansi;
        let graphite = paper.mix(ink, GRAPHITE);
        let accent = ansi[4];
        Self {
            variant,
            paper,
            selection: paper.mix(accent, SELECTION),
            graphite,
            rule: paper.mix(ink, RULE),
            accent,
            needs_approval: ansi[3],
            failed: ansi[1],
            done: ansi[2],
            added: ansi[2].mix(graphite, DIFF),
            removed: ansi[1].mix(graphite, DIFF),
            type_: Type {
                title: Face::bold(Tone::Ink),
                label: Face::plain(Tone::Ink),
                caption: Face::plain(Tone::Rgb(graphite)),
                key: Face::bold(Tone::Ink),
                mode: Face::bold(Tone::Rgb(accent)),
                matched: Face::plain(Tone::Rgb(accent)),
            },
        }
    }

    /// The tone of an agent state. Only these four states have a hue; the
    /// others are drawn in the grey ramp.
    pub fn agent(&self, state: AgentState) -> Tone {
        match state {
            AgentState::NeedsApproval => Tone::Rgb(self.needs_approval),
            AgentState::Failed => Tone::Rgb(self.failed),
            AgentState::Done => Tone::Rgb(self.done),
            AgentState::Working => Tone::Ink,
            AgentState::Idle => Tone::Rgb(self.graphite),
            AgentState::Unknown => Tone::Rgb(self.rule),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast ratio.
    fn contrast(a: Rgb, b: Rgb) -> f32 {
        fn luminance(colour: Rgb) -> f32 {
            let linear = |channel: u8| {
                let value = f32::from(channel) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(colour.r) + 0.7152 * linear(colour.g) + 0.0722 * linear(colour.b)
        }
        let (high, low) = {
            let (x, y) = (luminance(a), luminance(b));
            if x > y { (x, y) } else { (y, x) }
        };
        (high + 0.05) / (low + 0.05)
    }

    #[test]
    fn every_text_tone_is_legible_on_both_reference_papers() {
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::sheet(variant);
            let paper = Sheet::of(variant).paper;
            for (name, colour, minimum) in [
                ("graphite", theme.graphite, 4.5),
                ("accent", theme.accent, 4.5),
                ("needs approval", theme.needs_approval, 4.5),
                ("failed", theme.failed, 4.5),
                ("done", theme.done, 4.5),
                ("added", theme.added, 4.5),
                ("removed", theme.removed, 4.5),
                ("rule", theme.rule, 2.0),
            ] {
                let ratio = contrast(colour, paper);
                assert!(
                    ratio >= minimum,
                    "{name} on {variant:?} paper: {ratio:.2} < {minimum}"
                );
            }
            for (name, colour, ground, minimum) in [
                (
                    "ink on the selection",
                    Sheet::of(variant).ink,
                    theme.selection,
                    4.5,
                ),
                (
                    "accent on the selection",
                    theme.accent,
                    theme.selection,
                    3.0,
                ),
                ("paper on the accent chip", paper, theme.accent, 4.5),
            ] {
                let ratio = contrast(colour, ground);
                assert!(
                    ratio >= minimum,
                    "{name} on {variant:?}: {ratio:.2} < {minimum}"
                );
            }
        }
    }

    #[test]
    fn desktop_measures_sit_on_the_unit_and_sizes_step_by_the_ratio() {
        for measure in [
            DESKTOP.pad,
            DESKTOP.gap,
            DESKTOP.sidebar,
            DESKTOP.tab_bar,
            DESKTOP.status_bar,
            DESKTOP.palette_width,
        ] {
            assert_eq!(measure % DESKTOP.unit, 0.0, "{measure}");
        }
        let mut size = DESKTOP.size.body;
        for heading in DESKTOP.size.heading {
            size *= 1.125;
            assert!((heading - size).abs() < 0.3, "{heading} against {size}");
        }
        assert!((DESKTOP.size.caption - DESKTOP.size.body / 1.125).abs() < 0.3);
    }

    #[test]
    fn the_mark_eases_out_and_reduced_motion_arrives_at_once() {
        let motion = DESKTOP.motion;
        let half = Motion::ease(motion.travel, motion.travel / 2);
        assert!(half > 0.8 && half < 1.0, "{half}");
        let reduced = motion.reduced();
        assert_eq!(Motion::ease(reduced.travel, std::time::Duration::ZERO), 1.0);
    }

    #[test]
    fn the_variant_follows_the_reported_paper() {
        let light = Theme::on(Some(Rgb::hex(0xFFFFFF)), None, Variant::Dark);
        let dark = Theme::on(Some(Rgb::hex(0x000000)), None, Variant::Light);
        let unknown = Theme::on(None, None, Variant::Light);
        assert_eq!(
            (light.variant, dark.variant, unknown.variant),
            (Variant::Light, Variant::Dark, Variant::Light)
        );
    }

    #[test]
    fn palette_downgrade_picks_the_cube_or_the_grey_ramp() {
        assert_eq!(Rgb::hex(0x000000).to_palette(), 16);
        assert_eq!(Rgb::hex(0xFFFFFF).to_palette(), 231);
        assert_eq!(Rgb::hex(0x808080).to_palette(), 244);
        assert_eq!(Rgb::hex(0x5F87FF).to_palette(), 69);
        // A muted green stays green.
        assert_eq!(Rgb::hex(0x3F7A2E).to_palette(), 64);
    }
}

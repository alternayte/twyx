//! The desktop theme specimen: the chrome drawn from the tokens over sample
//! panes, a document and a diff hunk, with the travelling pencil mark.
//!
//! `tab` and `1` to `3` change the scene, `h` `j` `k` `l` move the pane
//! focus, `:` opens the palette, `m` turns motion off and on, `v` changes the
//! paper, and `q` quits.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, Div, FocusHandle, FontWeight, Hsla, KeyDownEvent, Pixels, Render,
    SharedString, TextAlign, TextRun, Window, canvas, div, fill, font, point, px, size,
};
use twyx_core::palette;
use twyx_core::specimen::{AGENT, EDITOR, Run, SHELL};
use twyx_core::theme::{AgentState, DESKTOP, Sheet, Theme, Variant};
use twyx_protocol::grid::{Attrs, Colour};
use twyx_protocol::view::View;

use crate::paint::{self, Metrics, Palette, hsla};

const SCENES: [&str; 3] = ["workspace", "document", "review"];
const PANES: [&str; 3] = ["zsh", "nvim theme.rs", "claude"];
const QUERY: &str = "them";
/// Label, what it is, and its key.
const ENTRIES: [(&str, &str, &str); 4] = [
    ("crates/twyx-core/src/theme.rs", "file", ""),
    ("nvim theme.rs", "pane · twyx/main", ""),
    ("theme specimen", "command", "ctrl-g :"),
    (
        "claude: write the theme tokens",
        "agent · needs approval",
        "",
    ),
];

/// The colours of one paper.
#[derive(Debug, Clone, Copy)]
struct Tones {
    paper: Hsla,
    ink: Hsla,
    graphite: Hsla,
    rule: Hsla,
    accent: Hsla,
    theme: Theme,
}

impl Tones {
    fn of(variant: Variant) -> Self {
        let sheet = Sheet::of(variant);
        let theme = Theme::sheet(variant);
        Self {
            paper: hsla(sheet.paper),
            ink: hsla(sheet.ink),
            graphite: hsla(theme.graphite),
            rule: hsla(theme.rule),
            accent: hsla(theme.accent),
            theme,
        }
    }

    fn agent(&self, state: AgentState) -> Hsla {
        match self.theme.agent(state) {
            twyx_core::theme::Tone::Ink => self.ink,
            twyx_core::theme::Tone::Rgb(colour) => hsla(colour),
        }
    }
}

/// The pencil mark on its way from one place to another. A new move starts
/// from where the last one was going.
#[derive(Debug, Clone, Copy)]
struct Mark {
    from: usize,
    to: usize,
    start: Instant,
}

impl Mark {
    fn at(place: usize) -> Self {
        Self {
            from: place,
            to: place,
            start: Instant::now(),
        }
    }

    fn go(&mut self, place: usize) {
        if place != self.to {
            *self = Self {
                from: self.to,
                to: place,
                start: Instant::now(),
            };
        }
    }

    /// How far along the move is, from 0.0 to 1.0.
    fn progress(&self, reduced: bool) -> f32 {
        let motion = if reduced {
            DESKTOP.motion.reduced()
        } else {
            DESKTOP.motion
        };
        twyx_core::theme::Motion::ease(motion.travel, self.start.elapsed())
    }
}

pub struct Specimen {
    focus: FocusHandle,
    metrics: Metrics,
    family: &'static str,
    /// The paper chosen with `v` or on the command line; otherwise the
    /// system's appearance picks it.
    paper: Option<Variant>,
    scene: Mark,
    pane: Mark,
    palette: Option<Mark>,
}

impl Specimen {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, paper: Option<Variant>) -> Self {
        let family = paint::family(window.text_system());
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        Self {
            focus,
            metrics: Metrics::measure(window.text_system(), family, px(DESKTOP.size.body)),
            family,
            paper,
            scene: Mark::at(0),
            pane: Mark::at(2),
            palette: None,
        }
    }

    fn variant(&self, window: &Window) -> Variant {
        self.paper
            .unwrap_or_else(|| paint::variant(window.appearance()))
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if let Some(palette) = &mut self.palette {
            match key {
                "escape" | "enter" => self.palette = None,
                "down" | "j" => palette.go((palette.to + 1) % ENTRIES.len()),
                "up" | "k" => palette.go((palette.to + ENTRIES.len() - 1) % ENTRIES.len()),
                _ => return,
            }
            cx.notify();
            return;
        }
        let pane = self.pane.to;
        match key {
            "q" => cx.quit(),
            "tab" => self.scene.go((self.scene.to + 1) % SCENES.len()),
            "1" | "2" | "3" => {
                self.scene
                    .go(usize::from(key.as_bytes()[0].saturating_sub(b'1')));
            }
            ":" | ";" => self.palette = Some(Mark::at(0)),
            "h" | "left" => self.pane.go(0),
            "l" | "right" if pane == 0 => self.pane.go(1),
            "j" | "down" if pane == 1 => self.pane.go(2),
            "k" | "up" if pane == 2 => self.pane.go(1),
            "m" => {
                let reduced = cx.reduce_motion();
                cx.set_reduce_motion(!reduced);
            }
            "v" => {
                self.paper = Some(match self.variant(window) {
                    Variant::Light => Variant::Dark,
                    Variant::Dark => Variant::Light,
                });
            }
            _ => return,
        }
        cx.notify();
    }

    /// A tab's width: its name in the body face and a pad each side.
    fn tab_width(&self, name: &str) -> f32 {
        name.chars().count() as f32 * f32::from(self.metrics.cell_width) + 4.0 * DESKTOP.pad
    }
}

impl Render for Specimen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tones = Tones::of(self.variant(window));
        let reduced = cx.reduce_motion();
        let moving = [Some(self.scene), Some(self.pane), self.palette]
            .into_iter()
            .flatten()
            .any(|mark| mark.progress(reduced) < 1.0);
        if moving {
            window.request_animation_frame();
        }

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
            .on_key_down(cx.listener(Self::on_key))
            .child(self.sidebar(tones))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .h_full()
                    .child(self.tab_bar(tones, reduced))
                    .child(self.scene_view(tones, reduced))
                    .child(self.status_bar(tones, reduced)),
            );
        if let Some(mark) = self.palette {
            root = root.child(self.palette_view(tones, mark, reduced, window));
        }
        root
    }
}

impl Specimen {
    fn sidebar(&self, tones: Tones) -> Div {
        let focused = self.pane.to;
        let label = |text: &str| {
            div()
                .px(px(DESKTOP.pad * 2.0))
                .pt(px(DESKTOP.gap * 2.0))
                .pb(px(DESKTOP.gap))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text.to_uppercase())
        };
        let row =
            |depth: f32, mark: Option<AgentState>, name: &str, detail: &str, current: bool| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(DESKTOP.unit * 6.0))
                    .pl(px(DESKTOP.pad * 2.0 + depth * DESKTOP.pad * 2.0))
                    .pr(px(DESKTOP.pad * 2.0))
                    .child(
                        div()
                            .w(px(DESKTOP.pad * 2.0))
                            .flex_shrink_0()
                            .text_color(mark.map_or(tones.graphite, |state| tones.agent(state)))
                            .child(mark.map_or(String::new(), |state| state.glyph().to_string())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(if current { tones.accent } else { tones.ink })
                            .when(current, |name| name.font_weight(FontWeight::SEMIBOLD))
                            .child(name.to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(detail.to_owned()),
                    )
            };
        let marks = [None, None, Some(AgentState::NeedsApproval)];
        let mut sidebar = div()
            .w(px(DESKTOP.sidebar))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r(px(DESKTOP.rule))
            .border_color(tones.rule)
            .child(label("Projects"))
            .child(row(0.0, None, "twyx", "", false))
            .child(row(
                1.0,
                Some(AgentState::NeedsApproval),
                "main",
                "main*",
                false,
            ));
        for (index, name) in PANES.iter().enumerate() {
            sidebar = sidebar.child(row(2.0, marks[index], name, "", index == focused));
        }
        sidebar
            .child(row(
                1.0,
                Some(AgentState::Working),
                "specimen",
                "feat/specimen",
                false,
            ))
            .child(row(2.0, Some(AgentState::Working), "claude", "", false))
            .child(row(0.0, None, "notes", "", false))
            .child(label("Inbox · 1"))
            .child(
                div()
                    .mx(px(DESKTOP.pad))
                    .px(px(DESKTOP.pad))
                    .py(px(DESKTOP.gap))
                    .border(px(DESKTOP.rule))
                    .border_color(tones.rule)
                    .flex()
                    .flex_col()
                    .gap(px(DESKTOP.unit))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(DESKTOP.gap))
                            .child(
                                div()
                                    .text_color(tones.agent(AgentState::NeedsApproval))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(AgentState::NeedsApproval.glyph().to_string()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("claude"),
                            )
                            .child(
                                div()
                                    .text_size(px(DESKTOP.size.caption))
                                    .text_color(tones.graphite)
                                    .child("2m"),
                            ),
                    )
                    .child(div().child("Do you want to make this edit?"))
                    .child(
                        div()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child("twyx/main · needs approval"),
                    ),
            )
    }

    fn tab_bar(&self, tones: Tones, reduced: bool) -> Div {
        let lefts: Vec<f32> = SCENES
            .iter()
            .scan(0.0, |left, name| {
                let here = *left;
                *left += self.tab_width(name);
                Some(here)
            })
            .collect();
        let t = self.scene.progress(reduced);
        let place = |index: usize| (lefts[index], self.tab_width(SCENES[index]));
        let (from_left, from_width) = place(self.scene.from);
        let (to_left, to_width) = place(self.scene.to);
        let mut bar = div()
            .relative()
            .h(px(DESKTOP.tab_bar))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .border_b(px(DESKTOP.rule))
            .border_color(tones.rule);
        for (index, name) in SCENES.iter().enumerate() {
            let active = index == self.scene.to;
            bar = bar.child(
                div()
                    .w(px(self.tab_width(name)))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if active { tones.ink } else { tones.graphite })
                    .when(active, |tab| tab.font_weight(FontWeight::SEMIBOLD))
                    .child(*name),
            );
        }
        bar.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_end()
                .pr(px(DESKTOP.pad * 2.0))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child("twyx · main"),
        )
        .child(
            div()
                .absolute()
                .bottom(px(-DESKTOP.rule))
                .left(px(lerp(from_left, to_left, t)))
                .w(px(lerp(from_width, to_width, t)))
                .h(px(DESKTOP.mark))
                .bg(tones.accent),
        )
    }

    fn scene_view(&self, tones: Tones, reduced: bool) -> Div {
        let body = div().flex_1().relative().overflow_hidden();
        match self.scene.to {
            0 => body.child(self.panes(tones, reduced)),
            1 => body.child(document(tones)),
            _ => body.child(review(tones)),
        }
    }

    /// Three panes: a shell on the left, an editor and an agent on the
    /// right. Painted by hand, so the focus mark can travel between them.
    fn panes(&self, tones: Tones, reduced: bool) -> impl IntoElement {
        let metrics = self.metrics.clone();
        let family = self.family;
        let mark = self.pane;
        let t = mark.progress(reduced);
        let palette = Palette::of(tones.theme.variant);
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let rects = pane_rects(bounds);
                let rule = px(DESKTOP.rule);
                // The shared hairlines between the panes.
                let middle = rects[1].origin.x;
                window.paint_quad(fill(
                    Bounds::new(
                        point(middle, bounds.origin.y),
                        size(rule, bounds.size.height),
                    ),
                    tones.rule,
                ));
                window.paint_quad(fill(
                    Bounds::new(
                        point(middle, rects[2].origin.y),
                        size(rects[2].size.width, rule),
                    ),
                    tones.rule,
                ));
                let title = px(DESKTOP.unit * 6.0);
                for (index, (rect, rows)) in rects.iter().zip([SHELL, EDITOR, AGENT]).enumerate() {
                    let focused = index == mark.to;
                    let inset = px(DESKTOP.pad);
                    text(
                        window,
                        cx,
                        family,
                        PANES[index],
                        point(rect.origin.x + inset, rect.origin.y + px(DESKTOP.unit)),
                        if focused {
                            DESKTOP.size.body
                        } else {
                            DESKTOP.size.caption
                        },
                        if focused { tones.ink } else { tones.graphite },
                        focused,
                    );
                    let grid = Bounds::new(
                        point(rect.origin.x + inset, rect.origin.y + title),
                        size(
                            rect.size.width - inset * 2.0,
                            rect.size.height - title - inset,
                        ),
                    );
                    let view = screen(
                        rows,
                        metrics.columns(grid.size.width),
                        metrics.rows(grid.size.height),
                        focused,
                    );
                    paint::paint(&view, grid, &metrics, palette, window, cx);
                }
                let from = rects[mark.from];
                let to = rects[mark.to];
                let at = Bounds::new(
                    point(
                        px(lerp(from.origin.x.into(), to.origin.x.into(), t)),
                        px(lerp(from.origin.y.into(), to.origin.y.into(), t)),
                    ),
                    size(
                        px(lerp(from.size.width.into(), to.size.width.into(), t)),
                        px(lerp(from.size.height.into(), to.size.height.into(), t)),
                    ),
                );
                outline(window, at, px(DESKTOP.mark), tones.accent);
            },
        )
        .size_full()
    }

    fn status_bar(&self, tones: Tones, reduced: bool) -> Div {
        let field = |child: Div| {
            child
                .h_full()
                .flex()
                .items_center()
                .px(px(DESKTOP.pad * 2.0))
                .border_r(px(DESKTOP.rule))
                .border_color(tones.rule)
        };
        let hint = |key: &'static str, word: &'static str| {
            div()
                .flex()
                .flex_row()
                .gap(px(DESKTOP.unit))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(key))
                .child(div().text_color(tones.graphite).child(word))
        };
        let mode = if self.palette.is_some() {
            "PALETTE"
        } else {
            "PANE"
        };
        div()
            .h(px(DESKTOP.status_bar))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .border_t(px(DESKTOP.rule))
            .border_color(tones.rule)
            .text_size(px(DESKTOP.size.caption))
            .child(field(
                div()
                    .text_color(tones.accent)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(mode),
            ))
            .child(field(div().child("twyx/main")))
            .child(field(
                div()
                    .flex_row()
                    .child(div().text_color(tones.graphite).child("main"))
                    .child("*"),
            ))
            .child(field(div().text_color(tones.graphite).child(if reduced {
                "motion off"
            } else {
                "motion on"
            })))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap(px(DESKTOP.gap * 2.0))
                    .px(px(DESKTOP.pad * 2.0))
                    .child(hint("tab", "scene"))
                    .child(hint("hjkl", "focus"))
                    .child(hint(":", "palette"))
                    .child(hint("m", "motion"))
                    .child(hint("v", "paper"))
                    .child(hint("q", "quit")),
            )
    }

    fn palette_view(&self, tones: Tones, mark: Mark, reduced: bool, window: &Window) -> Div {
        let row = DESKTOP.unit * 7.0;
        let t = mark.progress(reduced);
        let width = DESKTOP.palette_width;
        let left = (f32::from(window.viewport_size().width) - width) / 2.0;
        let mut entries = div().relative().flex().flex_col();
        let found = palette::search(ENTRIES.iter().map(|entry| entry.0), QUERY);
        for (index, hit) in found.iter().enumerate() {
            let (label, kind, key) = ENTRIES[hit.index];
            let selected = index == mark.to;
            entries = entries.child(
                div()
                    .h(px(row))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .gap(px(DESKTOP.gap))
                    .child(matched(label, &hit.positions, tones, selected))
                    .child(
                        div()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(kind),
                    )
                    .child(
                        div()
                            .w(px(DESKTOP.unit * 20.0))
                            .flex()
                            .justify_end()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(key),
                    ),
            );
        }
        let entries = entries.child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(lerp(mark.from as f32 * row, mark.to as f32 * row, t)))
                .w(px(DESKTOP.mark))
                .h(px(row))
                .bg(tones.accent),
        );
        div()
            .absolute()
            .top(px(DESKTOP.tab_bar + DESKTOP.unit * 12.0))
            .left(px(left))
            .w(px(width))
            .bg(tones.paper)
            .border(px(DESKTOP.rule))
            .border_color(tones.graphite)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(row + DESKTOP.unit))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .gap(px(DESKTOP.gap))
                    .border_b(px(DESKTOP.rule))
                    .border_color(tones.rule)
                    .child(div().text_color(tones.accent).child("›"))
                    .child(
                        div().flex().flex_row().items_center().child(QUERY).child(
                            div()
                                .w(px(DESKTOP.rule))
                                .h(px(DESKTOP.size.body))
                                .bg(tones.ink),
                        ),
                    ),
            )
            .child(entries)
    }
}

/// A label with the characters the query matched in the accent.
fn matched(label: &str, positions: &[usize], tones: Tones, selected: bool) -> Div {
    let mut line = div()
        .flex_1()
        .flex()
        .flex_row()
        .when(selected, |line| line.font_weight(FontWeight::SEMIBOLD));
    for (index, character) in label.chars().enumerate() {
        let hit = positions.contains(&index);
        line = line.child(
            div()
                .text_color(if hit { tones.accent } else { tones.ink })
                .child(character.to_string()),
        );
    }
    line
}

fn document(tones: Tones) -> Div {
    let heading = |level: usize, text: &str| {
        div()
            .pt(px(DESKTOP.gap * 2.0))
            .pb(px(DESKTOP.gap))
            .text_size(px(DESKTOP.size.heading[level]))
            .font_weight(FontWeight::SEMIBOLD)
            .child(text.to_owned())
    };
    let task = |done: bool, text: &str| {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(DESKTOP.gap))
            .child(
                div()
                    .w(px(DESKTOP.unit * 3.0))
                    .h(px(DESKTOP.unit * 3.0))
                    .border(px(DESKTOP.rule))
                    .border_color(if done { tones.accent } else { tones.graphite })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(DESKTOP.size.caption))
                    .text_color(tones.accent)
                    .child(if done { "✓" } else { "" }),
            )
            .child(
                div()
                    .text_color(if done { tones.graphite } else { tones.ink })
                    .child(text.to_owned()),
            )
    };
    let cell = |text: &str, head: bool| {
        div()
            .w(px(DESKTOP.unit * 40.0))
            .px(px(DESKTOP.pad))
            .py(px(DESKTOP.unit))
            .when(head, |cell| cell.font_weight(FontWeight::SEMIBOLD))
            .child(text.to_owned())
    };
    let table_row = |a: &str, b: &str, head: bool| {
        div()
            .flex()
            .flex_row()
            .border_b(px(DESKTOP.rule))
            .border_color(if head { tones.graphite } else { tones.rule })
            .child(cell(a, head))
            .child(cell(b, head))
    };
    let code = |rows: &[&[Run]]| {
        let palette = Palette::of(tones.theme.variant);
        let mut block = div()
            .my(px(DESKTOP.gap))
            .pl(px(DESKTOP.pad * 2.0))
            .border_l(px(DESKTOP.mark))
            .border_color(tones.rule)
            .flex()
            .flex_col();
        for row in rows.iter().take(5) {
            let mut line = div().flex().flex_row().h(px(DESKTOP.unit * 5.0));
            for (text, colour, bold) in row.iter() {
                line =
                    line.child(
                        div()
                            .text_color(colour.map_or(tones.ink, |index| {
                                palette.ansi[usize::from(index).min(15)]
                            }))
                            .when(*bold, |run| run.font_weight(FontWeight::SEMIBOLD))
                            .child(text.replace(' ', "\u{a0}")),
                    );
            }
            block = block.child(line);
        }
        block
    };
    div()
        .size_full()
        .px(px(DESKTOP.pad * 6.0))
        .py(px(DESKTOP.pad * 2.0))
        .flex()
        .flex_col()
        .max_w(px(DESKTOP.unit * 200.0))
        .child(
            div()
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child("docs/theme.md"),
        )
        .child(heading(2, "Theme tokens"))
        .child(div().child(
            "The sheet is drawn on the paper of the system appearance. Every colour, size and measure comes from the tokens.",
        ))
        .child(heading(1, "Checklist"))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(DESKTOP.unit))
                .child(task(true, "Commit the tokens"))
                .child(task(true, "Approve the TUI specimen"))
                .child(task(false, "Approve the desktop specimen")),
        )
        .child(heading(0, "Measures"))
        .child(
            div()
                .flex()
                .flex_col()
                .child(table_row("Measure", "Value", true))
                .child(table_row("pad", "8 px", false))
                .child(table_row("sidebar", "240 px", false))
                .child(table_row("tab bar", "28 px", false)),
        )
        .child(code(EDITOR))
        .child(
            div()
                .flex()
                .flex_row()
                .child("See ")
                .child(
                    div()
                        .text_color(tones.accent)
                        .underline()
                        .child("docs/keys.md"),
                )
                .child(" for every key."),
        )
}

fn review(tones: Tones) -> Div {
    let removed = tones.agent(AgentState::Failed);
    let added = tones.agent(AgentState::Done);
    let line = |old: &str, new: &str, sign: &str, text: &str| {
        let colour = match sign {
            "-" => Some(removed),
            "+" => Some(added),
            _ => None,
        };
        div()
            .flex()
            .flex_row()
            .h(px(DESKTOP.unit * 5.0))
            .border_l(px(DESKTOP.mark))
            .border_color(colour.unwrap_or(tones.paper))
            .child(
                div()
                    .w(px(DESKTOP.unit * 10.0))
                    .text_color(tones.graphite)
                    .flex()
                    .justify_end()
                    .child(old.to_owned()),
            )
            .child(
                div()
                    .w(px(DESKTOP.unit * 10.0))
                    .text_color(tones.graphite)
                    .flex()
                    .justify_end()
                    .child(new.to_owned()),
            )
            .child(
                div()
                    .w(px(DESKTOP.unit * 6.0))
                    .flex()
                    .justify_center()
                    .text_color(colour.unwrap_or(tones.graphite))
                    .child(sign.replace('-', "−")),
            )
            .child(
                div()
                    .text_color(colour.unwrap_or(tones.ink))
                    .child(text.replace(' ', "\u{a0}")),
            )
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(DESKTOP.unit * 9.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(DESKTOP.gap * 2.0))
                .px(px(DESKTOP.pad * 2.0))
                .border_b(px(DESKTOP.rule))
                .border_color(tones.rule)
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("crates/twyx-core/src/theme.rs"),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.unit))
                        .child(div().text_color(added).child("+2"))
                        .child(div().text_color(removed).child("−1")),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(DESKTOP.size.caption))
                        .text_color(tones.graphite)
                        .child("1 of 3 files reviewed"),
                ),
        )
        .child(
            div()
                .h(px(DESKTOP.unit * 7.0))
                .flex()
                .items_center()
                .px(px(DESKTOP.pad * 2.0))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child("@@ -31,6 +31,7 @@ pub fn mix(self, other: Self) -> Self"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(line(
                    "31",
                    "31",
                    " ",
                    "pub fn mix(self, other: Self, amount: f32) -> Self {",
                ))
                .child(line("32", "32", " ", "    let channel = |a: u8, b: u8| {"))
                .child(line(
                    "33",
                    "",
                    "-",
                    "        (a as f32 * 0.62).round() as u8",
                ))
                .child(line("", "33", "+", "        // gamma-space mix, rounded"))
                .child(line(
                    "",
                    "34",
                    "+",
                    "        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8",
                ))
                .child(line("34", "35", " ", "    };"))
                .child(line("35", "36", " ", "    Self {"))
                .child(line(
                    "36",
                    "37",
                    " ",
                    "        r: channel(self.r, other.r),",
                )),
        )
}

/// The sample screen of a pane at its size, with the cursor in the focused
/// one.
fn screen(rows: &[&[Run]], cols: u16, lines: u16, focused: bool) -> View {
    let mut view = View::new(cols, lines);
    view.cursor.visible = false;
    for (y, row) in rows.iter().enumerate().take(usize::from(lines)) {
        let mut x = 0;
        for (text, colour, bold) in row.iter() {
            for character in text.chars() {
                let Some(cell) = view.cells[y].get_mut(x) else {
                    break;
                };
                cell.text = character.to_string();
                cell.fg = colour.map_or(Colour::Default, Colour::Palette);
                if *bold {
                    cell.attrs |= Attrs::BOLD;
                }
                x += 1;
            }
        }
        if focused && y + 1 == rows.len() {
            view.cursor.visible = true;
            view.cursor.row = y as u16;
            view.cursor.col = x as u16;
        }
    }
    view
}

fn pane_rects(bounds: Bounds<Pixels>) -> [Bounds<Pixels>; 3] {
    let half = (bounds.size.width / 2.0).round();
    let top = (bounds.size.height / 2.0).round();
    let right = bounds.size.width - half;
    [
        Bounds::new(bounds.origin, size(half, bounds.size.height)),
        Bounds::new(
            point(bounds.origin.x + half, bounds.origin.y),
            size(right, top),
        ),
        Bounds::new(
            point(bounds.origin.x + half, bounds.origin.y + top),
            size(right, bounds.size.height - top),
        ),
    ]
}

/// A rectangle drawn in four strokes of `width`, inside `bounds`.
fn outline(window: &mut Window, bounds: Bounds<Pixels>, width: Pixels, colour: Hsla) {
    let Bounds { origin, size: area } = bounds;
    for side in [
        Bounds::new(origin, size(area.width, width)),
        Bounds::new(
            point(origin.x, origin.y + area.height - width),
            size(area.width, width),
        ),
        Bounds::new(origin, size(width, area.height)),
        Bounds::new(
            point(origin.x + area.width - width, origin.y),
            size(width, area.height),
        ),
    ] {
        window.paint_quad(fill(side, colour));
    }
}

#[allow(clippy::too_many_arguments)]
fn text(
    window: &mut Window,
    cx: &mut App,
    family: &'static str,
    content: &str,
    origin: gpui::Point<Pixels>,
    text_size: f32,
    colour: Hsla,
    semibold: bool,
) {
    let mut face = font(family);
    if semibold {
        face.weight = FontWeight::SEMIBOLD;
    }
    let run = TextRun {
        len: content.len(),
        font: face,
        color: colour,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(content.to_owned()),
        px(text_size),
        &[run],
        None,
    );
    let _ = line.paint(
        origin,
        px(DESKTOP.unit * 4.0),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

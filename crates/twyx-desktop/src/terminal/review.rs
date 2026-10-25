//! A review leaf: the worktree's changed files beside the unreviewed hunks
//! of the chosen file, unified or on two sides, laid over the leaf's cells.
//! A click chooses a file or a hunk; the wheel steps through the hunks.

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Div, FontWeight, HighlightStyle, Hsla, MouseButton,
    MouseDownEvent, Pixels, ScrollWheelEvent, StyledText, div, px,
};
use twyx_core::diff::{FileDiff, LineKind};
use twyx_core::keys::{Chord, Key};
use twyx_core::layout::LeafId;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::{IndexState, NoteInfo, NoteState, ReviewInfo, Reviewed, ToDaemon};
use twyx_protocol::review::{Cursor, KEYS, SideLine, SideRow, sides};

use super::Terminal;
use crate::highlight::Tokens;
use crate::paint::{Palette, Tones, hsla};

/// The narrowest diff, in cells, that shows two sides.
const SPLIT_WIDTH: u16 = 120;
/// The narrowest leaf, in cells, that shows the file list beside the diff.
const LIST_WIDTH: u16 = 60;
/// How far the wheel turns, in pixels, to step one hunk.
const WHEEL_STEP: f32 = 48.0;

/// One review leaf to lay over the tab area.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReviewLeaf {
    pub leaf: LeafId,
    pub worktree: u64,
    /// The leaf's inner cells, from the tab area's corner.
    pub bounds: Bounds<Pixels>,
    pub cols: u16,
    pub focused: bool,
}

/// A diff line's colour, when it has one, and its sign.
fn marked(kind: LineKind, tones: Tones) -> (Option<Hsla>, &'static str) {
    match kind {
        LineKind::Added => (Some(hsla(tones.theme.added)), "+"),
        LineKind::Removed => (Some(hsla(tones.theme.removed)), "−"),
        LineKind::Context => (None, " "),
        LineKind::NoNewline => (None, "\\"),
    }
}

impl Terminal {
    pub(super) fn review_leaf(
        &self,
        shown: ReviewLeaf,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (info, notes, cursor) = {
            let shared = self.shared();
            let reviews = &shared.reviews;
            (
                reviews.infos.get(&shown.worktree).cloned(),
                reviews
                    .notes
                    .get(&shown.worktree)
                    .cloned()
                    .unwrap_or_default(),
                reviews.cursor(shown.leaf, shown.worktree),
            )
        };
        let ReviewLeaf {
            leaf,
            worktree,
            bounds,
            ..
        } = shown;
        let root = div()
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height)
            .overflow_hidden()
            .bg(tones.paper)
            .text_color(tones.ink)
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.focus_review(leaf);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                this.review_wheel += f32::from(event.delta.pixel_delta(px(DESKTOP.unit * 5.0)).y);
                while this.review_wheel.abs() >= WHEEL_STEP {
                    let back = this.review_wheel > 0.0;
                    this.review_wheel -= WHEEL_STEP.copysign(this.review_wheel);
                    let step = Chord::plain(Key::Char(if back { 'p' } else { 'n' }));
                    this.shared().reviews.key(leaf, worktree, step);
                }
                cx.notify();
                cx.stop_propagation();
            }));
        let caption = |text: String| {
            div()
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        let Some(info) = info else {
            return root
                .p(px(DESKTOP.pad))
                .child(caption("reading the review".to_owned()))
                .into_any_element();
        };
        let open = info
            .files
            .iter()
            .filter(|file| file.reviewed != Reviewed::Yes)
            .count();
        let base: String = info.base.chars().take(8).collect();
        let branch = info.base_ref.rsplit('/').next().unwrap_or_default();
        let mut head = div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .px(px(DESKTOP.pad))
            .py(px(DESKTOP.unit))
            .border_b(px(DESKTOP.rule))
            .border_color(tones.rule);
        if let Some(error) = &info.error {
            head = head.child(div().child(error.clone()));
        }
        let root = root.child(head.child(caption(format!(
            "base {base} · {branch} · {open} to review · {} reviewed",
            info.files.len() - open
        ))));
        if info.files.is_empty() {
            return root
                .child(
                    div()
                        .p(px(DESKTOP.pad))
                        .child(caption("nothing changed since the base".to_owned())),
                )
                .into_any_element();
        }
        let selected = cursor.file.min(info.files.len() - 1);
        let mut body = div().flex_1().min_h(px(0.0)).flex().flex_row();
        if shown.cols >= LIST_WIDTH {
            let width = self.metrics.cell_width * f32::from((shown.cols / 3).clamp(20, 40));
            body = body.child(self.review_files(&info, selected, shown, width, tones, cx));
        }
        let diff_cols = if shown.cols >= LIST_WIDTH {
            shown.cols - (shown.cols / 3).clamp(20, 40)
        } else {
            shown.cols
        };
        let split = (diff_cols >= SPLIT_WIDTH) != cursor.flip;
        body = body.child(self.review_diff(&info, &notes, cursor, split, shown, tones, cx));
        let mut root = root.child(body);
        if shown.focused {
            root = root.child(review_keys(tones));
        }
        root.into_any_element()
    }

    /// The changed files: review state, staged state and path, the chosen
    /// one on a tinted bar.
    fn review_files(
        &self,
        info: &ReviewInfo,
        selected: usize,
        shown: ReviewLeaf,
        width: Pixels,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> Div {
        let row = px(DESKTOP.unit * 6.0);
        let room = (f32::from(shown.bounds.size.height) / f32::from(row)).max(1.0) as usize;
        let first = (selected + 1).saturating_sub(room);
        let mut list = div()
            .w(width)
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .py(px(DESKTOP.unit))
            .border_r(px(DESKTOP.rule))
            .border_color(tones.rule);
        for (index, file) in info.files.iter().enumerate().skip(first).take(room) {
            let chosen = index == selected;
            let (glyph, colour) = match file.reviewed {
                Reviewed::No => (" ", tones.ink),
                Reviewed::Partly => ("~", tones.ink),
                Reviewed::Yes => ("✓", tones.graphite),
                Reviewed::TooLarge => ("!", tones.graphite),
            };
            let staged = match file.index {
                IndexState::Staged => "+",
                IndexState::Partly => "±",
                IndexState::Committed | IndexState::Unstaged => " ",
            };
            let leaf = shown.leaf;
            list = list.child(
                div()
                    .relative()
                    .h(row)
                    .flex_shrink_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(DESKTOP.pad))
                    .gap(px(DESKTOP.unit))
                    .cursor_pointer()
                    .when(chosen, |line| {
                        line.bg(tones.selection).child(
                            div()
                                .absolute()
                                .left(px(0.0))
                                .top(px(0.0))
                                .bottom(px(0.0))
                                .w(px(DESKTOP.mark))
                                .bg(if shown.focused {
                                    tones.accent
                                } else {
                                    tones.graphite
                                }),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.focus_review(leaf);
                            this.shared().reviews.choose_file(leaf, index);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .w(self.metrics.cell_width)
                            .text_color(colour)
                            .child(glyph),
                    )
                    .child(
                        div()
                            .w(self.metrics.cell_width)
                            .text_color(tones.graphite)
                            .child(staged),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_color(colour)
                            .when(chosen, |path| path.font_weight(FontWeight::SEMIBOLD))
                            .child(file.path.clone()),
                    ),
            );
        }
        list
    }

    /// The chosen file's header, its notes, and its hunks from the chosen
    /// one on.
    #[allow(clippy::too_many_arguments)]
    fn review_diff(
        &self,
        info: &ReviewInfo,
        notes: &[NoteInfo],
        cursor: Cursor,
        split: bool,
        shown: ReviewLeaf,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = cursor.file.min(info.files.len().saturating_sub(1));
        let file = &info.files[selected];
        let reviewed = info
            .files
            .iter()
            .filter(|file| file.reviewed == Reviewed::Yes)
            .count();
        let (added, removed) = file.diff.as_ref().map_or((0, 0), |diff| {
            let count = |kind| {
                diff.hunks
                    .iter()
                    .flat_map(|hunk| &hunk.lines)
                    .filter(|line| line.kind == kind)
                    .count()
            };
            (count(LineKind::Added), count(LineKind::Removed))
        });
        let mut column = div()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col();
        column = column.child(
            div()
                .flex_shrink_0()
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
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(file.path.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.unit))
                        .child(
                            div()
                                .text_color(hsla(tones.theme.added))
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(hsla(tones.theme.removed))
                                .child(format!("−{removed}")),
                        ),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(DESKTOP.size.caption))
                        .text_color(tones.graphite)
                        .child(format!("{reviewed} of {} files reviewed", info.files.len())),
                ),
        );
        for note in notes.iter().filter(|note| note.path == file.path) {
            let state = match note.state {
                NoteState::Open => "",
                NoteState::Sent => " (sent)",
                NoteState::Outdated => " (outdated)",
            };
            column = column.child(
                div()
                    .flex_shrink_0()
                    .px(px(DESKTOP.pad * 2.0))
                    .py(px(DESKTOP.unit))
                    .border_b(px(DESKTOP.rule))
                    .border_color(tones.rule)
                    .text_size(px(DESKTOP.size.caption))
                    .child(format!(
                        "✎ lines {}-{}{state}: {}",
                        note.lines.0, note.lines.1, note.body
                    )),
            );
        }
        let message = |text: &str| {
            div()
                .px(px(DESKTOP.pad * 2.0))
                .py(px(DESKTOP.pad))
                .text_color(tones.graphite)
                .child(text.to_owned())
        };
        match (&file.diff, file.reviewed) {
            (Some(diff), _) if diff.binary => {
                column.child(message("a binary file: f marks it reviewed"))
            }
            (Some(diff), _) => {
                column.child(self.review_hunks(diff, cursor, split, shown, tones, cx))
            }
            (None, Reviewed::TooLarge) => column.child(message("too large to review")),
            (None, _) => column.child(message("reviewed")),
        }
    }

    /// The hunks from the chosen one on, as many as fit.
    fn review_hunks(
        &self,
        diff: &FileDiff,
        cursor: Cursor,
        split: bool,
        shown: ReviewLeaf,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> Div {
        let row = px(DESKTOP.unit * 5.0);
        let palette = Palette::of(tones.theme.variant);
        let room = (f32::from(shown.bounds.size.height) / f32::from(row)) as usize + 1;
        let chosen = cursor.hunk.min(diff.hunks.len().saturating_sub(1));
        let number = |value: Option<u32>| {
            div()
                .w(px(DESKTOP.unit * 10.0))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .pr(px(DESKTOP.unit))
                .text_color(tones.graphite)
                .child(value.map(|value| value.to_string()).unwrap_or_default())
        };
        let sign = |sign: &'static str, colour: Option<Hsla>| {
            div()
                .w(px(DESKTOP.unit * 6.0))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .text_color(colour.unwrap_or(tones.graphite))
                .child(sign)
        };
        // A line in its syntax colours on a tint of its change, or else in
        // the colour of its change.
        let code = |text: &str, colour: Option<Hsla>, tokens: Option<&Tokens>| {
            let line = div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap();
            match tokens {
                Some(tokens) => line.child(coloured(text, tokens, tones, palette)),
                None => line
                    .text_color(colour.unwrap_or(tones.ink))
                    .child(shown_text(text)),
            }
        };
        let line_row = |colour: Option<Hsla>, tinted: bool| {
            div()
                .h(row)
                .flex_shrink_0()
                .flex()
                .flex_row()
                .items_center()
                .border_l(px(DESKTOP.mark))
                .border_color(colour.unwrap_or(tones.paper))
                .when_some(colour.filter(|_| tinted), |line, colour| {
                    line.bg(colour.opacity(0.08))
                })
        };
        let mut rows = div().flex().flex_col().pb(px(DESKTOP.pad));
        let mut count = 0;
        for (index, hunk) in diff.hunks.iter().enumerate().skip(chosen) {
            if count >= room {
                break;
            }
            let current = index == chosen;
            let leaf = shown.leaf;
            let tokens = self.highlighter.hunk(&diff.path, hunk);
            let tokens_of = |line: usize| tokens.as_ref().and_then(|tokens| tokens.get(line));
            let tinted = tokens.is_some();
            rows = rows.child(
                div()
                    .h(px(DESKTOP.unit * 7.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .border_l(px(DESKTOP.mark))
                    .border_color(match (current, shown.focused) {
                        (true, true) => tones.accent,
                        (true, false) => tones.graphite,
                        (false, _) => tones.paper,
                    })
                    .text_size(px(DESKTOP.size.caption))
                    .text_color(if current { tones.ink } else { tones.graphite })
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.focus_review(leaf);
                            this.shared().reviews.choose_hunk(leaf, index);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(hunk.header.clone()),
            );
            count += 1;
            if split {
                for side_row in sides(hunk) {
                    if count >= room {
                        break;
                    }
                    let (left, right) = match side_row {
                        SideRow::Same {
                            index,
                            old,
                            new,
                            text,
                        } => (
                            Some((
                                SideLine {
                                    index,
                                    number: old,
                                    text,
                                },
                                LineKind::Context,
                            )),
                            Some((
                                SideLine {
                                    index,
                                    number: new,
                                    text,
                                },
                                LineKind::Context,
                            )),
                        ),
                        SideRow::Pair(old, new) => (
                            old.map(|side| (side, LineKind::Removed)),
                            new.map(|side| (side, LineKind::Added)),
                        ),
                    };
                    let half = |side: Option<(SideLine<'_>, LineKind)>| match side {
                        Some((side, kind)) => {
                            let (colour, mark) = marked(kind, tones);
                            line_row(colour, tinted)
                                .flex_1()
                                .min_w(px(0.0))
                                .child(number(Some(side.number)))
                                .child(sign(mark, colour))
                                .child(code(side.text, colour, tokens_of(side.index)))
                        }
                        None => line_row(None, false).flex_1().min_w(px(0.0)),
                    };
                    rows = rows.child(
                        div()
                            .flex()
                            .flex_row()
                            .child(half(left))
                            .child(div().w(px(DESKTOP.rule)).h(row).bg(tones.rule))
                            .child(half(right)),
                    );
                    count += 1;
                }
            } else {
                for (at, (line, (old, new))) in hunk.lines.iter().zip(hunk.numbers()).enumerate() {
                    if count >= room {
                        break;
                    }
                    let (colour, mark) = marked(line.kind, tones);
                    rows = rows.child(
                        line_row(colour, tinted)
                            .child(number(old))
                            .child(number(new))
                            .child(sign(mark, colour))
                            .child(code(&line.text, colour, tokens_of(at))),
                    );
                    count += 1;
                }
            }
        }
        rows
    }

    /// Focuses a review leaf a click landed in.
    fn focus_review(&self, leaf: LeafId) {
        if self.shared().leaf != Some(leaf) {
            self.request(ToDaemon::FocusLeaf { leaf });
        }
    }
}

/// Text as a terminal would lay it out: tabs to four spaces, and spaces
/// that the layout keeps.
fn shown_text(text: &str) -> String {
    text.replace('\t', "    ").replace(' ', "\u{a0}")
}

/// A line in its tokens' colours, laid out as `shown_text` lays it out.
fn coloured(text: &str, tokens: &Tokens, tones: Tones, palette: Palette) -> StyledText {
    let mut shown = String::new();
    let mut runs = Vec::new();
    let mut end = 0;
    for (range, slot) in tokens {
        let Some(piece) = text.get(range.clone()) else {
            continue;
        };
        let start = shown.len();
        shown.push_str(&shown_text(piece));
        runs.push((
            start..shown.len(),
            HighlightStyle {
                color: Some(slot.colour(tones, palette)),
                ..HighlightStyle::default()
            },
        ));
        end = range.end;
    }
    shown.push_str(&shown_text(text.get(end..).unwrap_or_default()));
    StyledText::new(shown).with_highlights(runs)
}

/// The keys of a focused review leaf, on two rows at its foot.
fn review_keys(tones: Tones) -> Div {
    let mut keys = div()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(DESKTOP.unit))
        .px(px(DESKTOP.pad))
        .py(px(DESKTOP.unit * 2.0))
        .border_t(px(DESKTOP.rule))
        .border_color(tones.rule)
        .text_size(px(DESKTOP.size.caption));
    for row in KEYS {
        keys = keys.child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(DESKTOP.gap * 2.0))
                .children(row.iter().map(|(key, word)| {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.unit))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(*key))
                        .child(div().text_color(tones.graphite).child(*word))
                })),
        );
    }
    keys
}

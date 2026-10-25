//! The inbox and a worktree's checks on a sheet over the panes, with the
//! keys both clients share. A click takes a row: it shows an inbox item's
//! pane, or runs a check.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, MouseButton, MouseDownEvent, Window, div, px};
use twyx_core::check::Outcome;
use twyx_core::keys::Chord;
use twyx_core::theme::{AgentState, DESKTOP};
use twyx_protocol::client::ToDaemon;
use twyx_protocol::lists::{self, CheckKey, InboxKey};

use super::Terminal;
use crate::paint::Tones;

/// The most rows the sheet shows at once.
const ROWS: usize = 12;

/// A list open over the panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum List {
    Inbox { selected: usize },
    Checks { worktree: u64, selected: usize },
}

/// One row of a list: a glyph and its colour, the text, and a caption.
struct Line {
    glyph: String,
    colour: Hsla,
    text: String,
    detail: String,
    /// Not read yet, or the check's result.
    strong: bool,
}

impl Terminal {
    /// Takes a key in the open list.
    pub(super) fn list_key(&mut self, mut list: List, chord: Chord) {
        match &mut list {
            List::Inbox { selected } => {
                let key = {
                    let shared = self.shared();
                    lists::inbox_key(selected, &shared.workspaces.inbox, chord)
                };
                self.list = Some(list);
                self.inbox(key);
            }
            List::Checks { worktree, selected } => {
                let worktree = *worktree;
                let key = {
                    let shared = self.shared();
                    let checks = shared
                        .workspaces
                        .checks
                        .as_ref()
                        .filter(|info| info.worktree == worktree)
                        .map_or(&[][..], |info| info.checks.as_slice());
                    lists::checks_key(selected, checks, chord)
                };
                self.list = Some(list);
                self.check(worktree, key);
            }
        }
    }

    fn inbox(&mut self, key: InboxKey) {
        match key {
            InboxKey::Stay => {}
            InboxKey::Close => self.list = None,
            InboxKey::Jump(pane) => {
                self.list = None;
                self.jump(pane);
            }
            InboxKey::Unread(item) => self.request(ToDaemon::MarkUnread { item }),
            InboxKey::Dismiss(item) => self.request(ToDaemon::Dismiss { item }),
        }
    }

    fn check(&mut self, worktree: u64, key: CheckKey) {
        let effect = self.shared().workspaces.check(worktree, key);
        if effect.close || effect.dialog.is_some() {
            self.list = None;
        }
        self.apply(effect);
    }

    pub(super) fn list_sheet(
        &mut self,
        list: List,
        tones: Tones,
        reduced: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = lists::now_ms();
        let (title, hint, lines, selected, note) = {
            let shared = self.shared();
            match list {
                List::Inbox { selected } => {
                    let lines: Vec<Line> = shared
                        .workspaces
                        .inbox
                        .iter()
                        .map(|item| Line {
                            glyph: item
                                .mark
                                .map_or(" ".to_owned(), |mark| mark.glyph().to_string()),
                            colour: item.mark.map_or(tones.graphite, |mark| tones.agent(mark)),
                            text: format!("{} · {}", lists::state(item), item.title),
                            detail: format!(
                                "{}{}",
                                lists::age(now.saturating_sub(item.created_ms)),
                                lists::place(item)
                            ),
                            strong: !item.read,
                        })
                        .collect();
                    let note = lines.is_empty().then(|| "the inbox is empty".to_owned());
                    ("inbox", lists::INBOX_KEYS, lines, selected, note)
                }
                List::Checks { worktree, selected } => {
                    let info = shared
                        .workspaces
                        .checks
                        .as_ref()
                        .filter(|info| info.worktree == worktree);
                    let lines: Vec<Line> = info
                        .map_or(&[][..], |info| info.checks.as_slice())
                        .iter()
                        .map(|entry| {
                            let (result, when) = lists::result(entry, now);
                            let outcome = entry.run.as_ref().and_then(|run| run.outcome);
                            let stale = entry.run.as_ref().is_some_and(|run| run.stale);
                            let state = match outcome {
                                None if entry.run.is_some() => AgentState::Working,
                                None => AgentState::Unknown,
                                Some(_) if stale => AgentState::Unknown,
                                Some(Outcome::Pass) => AgentState::Done,
                                Some(_) => AgentState::Failed,
                            };
                            Line {
                                glyph: state.glyph().to_string(),
                                colour: tones.agent(state),
                                text: format!("{}  {result}", entry.check.name),
                                detail: [entry.check.command.as_str(), when.as_str()]
                                    .iter()
                                    .filter(|part| !part.is_empty())
                                    .copied()
                                    .collect::<Vec<_>>()
                                    .join(" · "),
                                strong: !stale && outcome.is_some(),
                            }
                        })
                        .collect();
                    let untrusted = info
                        .and_then(|info| info.file.as_ref())
                        .is_some_and(|file| !file.trusted);
                    let note = match info {
                        None => Some("reading the checks".to_owned()),
                        Some(info) if info.checks.is_empty() => Some(
                            "no checks: add [[check]] to config.toml or .twyx/checks.toml"
                                .to_owned(),
                        ),
                        Some(info) => info.error.clone().or_else(|| {
                            info.checks
                                .iter()
                                .any(|entry| {
                                    entry.run.as_ref().and_then(|run| run.outcome)
                                        == Some(Outcome::Pass)
                                })
                                .then(|| twyx_core::check::EVIDENCE.to_owned())
                        }),
                    };
                    (
                        "checks",
                        lists::checks_keys(untrusted),
                        lines,
                        selected,
                        note,
                    )
                }
            }
        };
        let row = DESKTOP.unit * 9.0;
        let width = DESKTOP.palette_width;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(0.0);
        let selected = selected.min(lines.len().saturating_sub(1));
        let first = selected.saturating_sub(ROWS - 1);
        let mut rows = div().relative().flex().flex_col();
        if !lines.is_empty() {
            rows = rows.child(self.selection_bar(selected, selected - first, row, tones, reduced));
        }
        for (index, line) in lines.into_iter().enumerate().skip(first).take(ROWS) {
            rows = rows.child(
                div()
                    .relative()
                    .h(px(row))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .gap(px(DESKTOP.gap))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.list_click(list, index);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .w(self.metrics.cell_width)
                            .flex_shrink_0()
                            .text_color(line.colour)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(line.glyph),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .when(line.strong, |text| {
                                        text.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(line.text),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_size(px(DESKTOP.size.caption))
                                    .text_color(tones.graphite)
                                    .child(line.detail),
                            ),
                    ),
            );
        }
        let caption = |text: String| {
            div()
                .px(px(DESKTOP.pad * 2.0))
                .py(px(DESKTOP.gap))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        div()
            .absolute()
            .top(px(DESKTOP.tab_bar + DESKTOP.unit * 12.0))
            .left(px(left))
            .w(px(width))
            .bg(tones.paper)
            .text_color(tones.ink)
            .border(px(DESKTOP.rule))
            .border_color(tones.graphite)
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(DESKTOP.unit * 8.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(DESKTOP.gap * 2.0))
                    .px(px(DESKTOP.pad * 2.0))
                    .border_b(px(DESKTOP.rule))
                    .border_color(tones.rule)
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(hint),
                    ),
            )
            .child(rows)
            .children(note.map(caption))
            .into_any_element()
    }

    /// A click on a row: the inbox shows the item's pane, the checks run
    /// the check.
    fn list_click(&mut self, list: List, index: usize) {
        let list = match list {
            List::Inbox { .. } => List::Inbox { selected: index },
            List::Checks { worktree, .. } => List::Checks {
                worktree,
                selected: index,
            },
        };
        self.list_key(list, Chord::plain(twyx_core::keys::Key::Enter));
    }
}

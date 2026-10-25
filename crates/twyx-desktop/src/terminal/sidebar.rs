//! The sidebar down the left edge: the spaces section (projects, their
//! worktrees and workspaces, and the plain panes of each) and the agents
//! section, from the rows both clients share, then the newest inbox items.
//! A click shows what a row or an item stands for.

use gpui::prelude::*;
use gpui::{Context, Div, FontWeight, MouseButton, MouseDownEvent, div, px};
use twyx_core::keys::Mode;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::InboxItem;
use twyx_protocol::lists;
use twyx_protocol::sidebar::{Kind, Row};

use super::Terminal;
use crate::paint::Tones;

/// The most inbox items the sidebar shows; the inbox list has them all.
const INBOX_ITEMS: usize = 5;

impl Terminal {
    /// The sidebar's rows. The selected row sits on a tinted bar: the
    /// workspace mode cursor while the mode is open, else the client's own
    /// workspace. The focused pane is lettered in the accent.
    pub(super) fn sidebar(&self, tones: Tones, cx: &mut Context<Self>) -> Div {
        let (rows, selected, inbox) = {
            let shared = self.shared();
            let workspaces = &shared.workspaces;
            let cursor = (self.keymap.mode() == Mode::Workspace).then_some(workspaces.cursor);
            let selected = cursor.or_else(|| {
                workspaces
                    .rows
                    .iter()
                    .position(|row| row.current && row.kind != Kind::Pane)
            });
            (workspaces.rows.clone(), selected, workspaces.inbox.clone())
        };
        let mut list = div()
            .id("sidebar-rows")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(px(DESKTOP.pad));
        for (index, row) in rows.into_iter().enumerate() {
            list = list.child(self.sidebar_row(index, row, selected == Some(index), tones, cx));
        }
        if !inbox.is_empty() {
            let unread = inbox.iter().filter(|item| !item.read).count();
            list = list.child(
                div()
                    .px(px(DESKTOP.pad * 2.0))
                    .pt(px(DESKTOP.gap * 2.0))
                    .pb(px(DESKTOP.gap))
                    .text_size(px(DESKTOP.size.caption))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(tones.graphite)
                    .child(if unread == 0 {
                        "INBOX".to_owned()
                    } else {
                        format!("INBOX · {unread}")
                    }),
            );
            let now = lists::now_ms();
            for item in inbox.into_iter().take(INBOX_ITEMS) {
                list = list.child(self.inbox_card(item, now, tones, cx));
            }
        }
        div()
            .w(px(DESKTOP.sidebar))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .bg(tones.paper)
            .border_r(px(DESKTOP.rule))
            .border_color(tones.rule)
            .child(list)
    }

    fn sidebar_row(
        &self,
        index: usize,
        row: Row,
        selected: bool,
        tones: Tones,
        cx: &mut Context<Self>,
    ) -> Div {
        if row.kind == Kind::Heading {
            return div()
                .px(px(DESKTOP.pad * 2.0))
                .pt(px(if row.label.is_empty() {
                    0.0
                } else {
                    DESKTOP.gap * 2.0
                }))
                .pb(px(DESKTOP.gap))
                .text_size(px(DESKTOP.size.caption))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tones.graphite)
                .child(row.label.to_uppercase());
        }
        let focused_pane = row.current && row.kind == Kind::Pane;
        // A branch named like its worktree says nothing new.
        let detail = if row.detail == row.label {
            String::new()
        } else {
            row.detail.clone()
        };
        let label_colour = if focused_pane {
            tones.accent
        } else if row.kind == Kind::Pane {
            tones.graphite
        } else {
            tones.ink
        };
        div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .h(px(DESKTOP.unit * 6.0))
            .pl(px(
                DESKTOP.pad * 2.0 + f32::from(row.depth) * DESKTOP.pad * 2.0
            ))
            .pr(px(DESKTOP.pad * 2.0))
            .when(selected, |line| {
                line.bg(tones.selection).child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .w(px(DESKTOP.mark))
                        .bg(tones.accent),
                )
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.open_row(index, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(DESKTOP.pad * 2.0))
                    .flex_shrink_0()
                    .text_color(row.mark.map_or(tones.graphite, |state| tones.agent(state)))
                    .child(
                        row.mark
                            .map_or(String::new(), |state| state.glyph().to_string()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_color(label_colour)
                    .when(
                        focused_pane || row.kind == Kind::Project || selected,
                        |label| label.font_weight(FontWeight::SEMIBOLD),
                    )
                    .when(row.kind == Kind::Pane, |label| {
                        label.text_size(px(DESKTOP.size.caption))
                    })
                    .child(row.label),
            )
            .when(!detail.is_empty() || row.dirty, |line| {
                line.child(
                    div()
                        .flex_shrink_0()
                        .pl(px(DESKTOP.gap))
                        .text_size(px(DESKTOP.size.caption))
                        .text_color(tones.graphite)
                        .child(format!("{detail}{}", if row.dirty { "*" } else { "" })),
                )
            })
    }

    /// One inbox item: its mark, title and age, then its state and place.
    fn inbox_card(&self, item: InboxItem, now: u64, tones: Tones, cx: &mut Context<Self>) -> Div {
        let caption = |text: String| {
            div()
                .truncate()
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        let pane = item.pane;
        div()
            .mx(px(DESKTOP.pad))
            .mb(px(DESKTOP.unit))
            .px(px(DESKTOP.pad))
            .py(px(DESKTOP.unit * 1.5))
            .border(px(DESKTOP.rule))
            .border_color(tones.rule)
            .flex()
            .flex_col()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.jump(pane);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(DESKTOP.gap))
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(item.mark.map_or(tones.graphite, |mark| tones.agent(mark)))
                            .child(
                                item.mark
                                    .map_or("·".to_owned(), |mark| mark.glyph().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .when(!item.read, |title| title.font_weight(FontWeight::SEMIBOLD))
                            .child(item.title.clone()),
                    )
                    .child(caption(lists::age(now.saturating_sub(item.created_ms)))),
            )
            .child(caption(format!(
                "{}{}",
                lists::state(&item),
                lists::place(&item)
            )))
    }

    /// Shows a pane wherever it is.
    pub(super) fn jump(&mut self, pane: u64) {
        let choice = lists::jump(pane, self.shared().workspaces.overview.as_ref());
        if let Some(request) = choice.request() {
            self.request(request);
        }
    }

    /// Shows what a sidebar row stands for, and puts the cursor on it.
    pub(super) fn open_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let request = {
            let mut shared = self.shared();
            let request = shared.workspaces.open(index);
            if request.is_some() {
                shared.workspaces.cursor = index;
            }
            request
        };
        if let Some(request) = request {
            self.request(request);
        }
        self.keymap.reset();
        cx.notify();
    }
}

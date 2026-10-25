//! The tab bar, the status bar and the key sheet around the tab area.

use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Div, FontWeight, MouseButton, MouseDownEvent, Window, div, point,
    px, size,
};
use twyx_core::keys::Mode;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::{ToDaemon, WorkspaceInfo};

use super::Terminal;
use crate::paint::Tones;
use crate::panes::Travel;

/// The longest tab label, in characters.
const TAB_LABEL: usize = 24;

/// `text` cut to `limit` characters, with an ellipsis when it was cut.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

impl Terminal {
    /// A tab's width: its label in the body face and a pad each side.
    fn tab_width(&self, label: &str) -> f32 {
        label.chars().count() as f32 * f32::from(self.metrics.cell_width) + 4.0 * DESKTOP.pad
    }

    /// The tabs of the workspace, numbered; a click shows one. The pencil
    /// mark under the active tab travels when the tab changes.
    pub(super) fn tab_bar(&mut self, tones: Tones, reduced: bool, cx: &mut Context<Self>) -> Div {
        let (tabs, active, workspace) = {
            let shared = self.shared();
            let tabs: Vec<(u64, String)> = shared
                .workspace
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .enumerate()
                .map(|(number, tab)| {
                    let label = tab.label(|leaf| shared.title(leaf));
                    (
                        tab.tab,
                        format!("{} {}", number + 1, clip(&label, TAB_LABEL)),
                    )
                })
                .collect();
            let workspace = shared
                .workspace
                .as_ref()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default();
            (tabs, shared.tab, workspace)
        };

        let mut bar = div()
            .relative()
            .h(px(DESKTOP.tab_bar))
            .flex_shrink_0()
            .flex()
            .flex_row()
            .border_b(px(DESKTOP.rule))
            .border_color(tones.rule);
        let mut left = 0.0;
        let mut mark = None;
        for (tab, label) in tabs {
            let width = self.tab_width(&label);
            let current = Some(tab) == active;
            if current {
                mark = Some((
                    tab,
                    Bounds::new(point(px(left), px(0.0)), size(px(width), px(DESKTOP.mark))),
                ));
            }
            left += width;
            bar = bar.child(
                div()
                    .w(px(width))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(if current { tones.ink } else { tones.graphite })
                    .when(current, |tab| tab.font_weight(FontWeight::SEMIBOLD))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.request(ToDaemon::SelectTab { tab });
                            cx.stop_propagation();
                        }),
                    ),
            );
        }
        bar = bar.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_end()
                .pr(px(DESKTOP.pad * 2.0))
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(workspace),
        );
        let Some((tab, to)) = mark else {
            self.tab_mark = None;
            return bar;
        };
        let travel = self.tab_mark.get_or_insert_with(|| Travel::at(tab, to));
        travel.go(tab, to, reduced);
        let at = travel.now(reduced);
        bar.child(
            div()
                .absolute()
                .bottom(px(-DESKTOP.rule))
                .left(at.origin.x)
                .w(at.size.width)
                .h(px(DESKTOP.mark))
                .bg(tones.accent),
        )
    }

    /// The mode, where the client is, who sizes the panes, and the keys the
    /// mode takes.
    pub(super) fn status_bar(&self, tones: Tones, cx: &mut Context<Self>) -> Div {
        let field = |child: Div| {
            child
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(DESKTOP.unit))
                .px(px(DESKTOP.pad * 2.0))
                .border_r(px(DESKTOP.rule))
                .border_color(tones.rule)
        };
        let hint = |key: String, word: String| {
            div()
                .flex()
                .flex_row()
                .gap(px(DESKTOP.unit))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(key))
                .child(div().text_color(tones.graphite).child(word))
        };

        let (location, sized_elsewhere, message) = {
            let shared = self.shared();
            let location = shared.workspace.as_ref().map(WorkspaceInfo::location);
            let lease = shared.tab_info().and_then(|tab| tab.lease);
            (
                location,
                lease.is_some_and(|holder| holder != shared.client),
                shared.workspaces.message.clone(),
            )
        };
        let mode = self.keymap.mode();
        let moving = matches!(self.drag, Some(super::Drag::Move { .. }));
        let hints = match mode {
            _ if moving => vec![
                ("edge".to_owned(), "split".to_owned()),
                ("middle".to_owned(), "stack".to_owned()),
                (
                    if cfg!(target_os = "macos") {
                        "option"
                    } else {
                        "alt"
                    }
                    .to_owned(),
                    "float".to_owned(),
                ),
            ],
            Mode::Normal => {
                let prefix = self.keymap.prefix.to_string();
                vec![
                    (prefix.clone(), "commands".to_owned()),
                    (format!("{prefix} ?"), "keys".to_owned()),
                ]
            }
            _ => self.keymap.hints(),
        };

        let mut bar = div()
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
                    .child(mode.name().to_uppercase()),
            ));
        match location {
            Some(location) => {
                bar = bar.child(field(div().child(location.name)));
                if !location.branch.is_empty() {
                    bar = bar.child(field(
                        div()
                            .child(div().text_color(tones.graphite).child(location.branch))
                            .when(location.dirty, |field| field.child("*")),
                    ));
                }
            }
            None => {
                bar = bar.child(field(div().text_color(tones.graphite).child("connecting")));
            }
        }
        if let Some(message) = message {
            bar =
                bar.child(field(div().min_w(px(0.0)).child(
                    div().truncate().text_color(tones.graphite).child(message),
                )));
        }
        if sized_elsewhere {
            bar = bar.child(field(
                div()
                    .child(
                        div()
                            .text_color(tones.graphite)
                            .child("sized by another client"),
                    )
                    .child(
                        div()
                            .id("take-control")
                            .text_color(tones.accent)
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .child("take control")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                    this.request(ToDaemon::TakeLease);
                                    cx.stop_propagation();
                                }),
                            ),
                    ),
            ));
        }
        bar.child(
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(DESKTOP.gap * 2.0))
                .px(px(DESKTOP.pad * 2.0))
                .children(hints.into_iter().map(|(key, word)| hint(key, word))),
        )
    }

    /// Every bound key by mode, in columns on a sheet over the panes. The
    /// wheel scrolls it when it is taller than the window.
    pub(super) fn key_sheet(&self, tones: Tones, window: &Window) -> AnyElement {
        let size = window.viewport_size();
        let margin = DESKTOP.unit * 8.0;
        let column = DESKTOP.unit * 64.0;
        let mut columns = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(DESKTOP.gap * 4.0));
        for section in self.keymap.sheet() {
            let mut title = div()
                .flex()
                .flex_row()
                .gap(px(DESKTOP.gap))
                .pb(px(DESKTOP.unit))
                .child(
                    div()
                        .text_size(px(DESKTOP.size.caption))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tones.graphite)
                        .child(section.mode.to_uppercase()),
                );
            if let Some(opens) = section.opens {
                title = title.child(
                    div()
                        .text_size(px(DESKTOP.size.caption))
                        .text_color(tones.graphite)
                        .child(opens),
                );
            }
            let mut rows = div().w(px(column)).flex().flex_col().child(title);
            for (keys, what) in section.rows {
                rows = rows.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.gap))
                        .child(
                            div()
                                .w(px(column * 0.35))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(keys),
                        )
                        .child(div().child(what)),
                );
            }
            columns = columns.child(rows);
        }
        let width = (f32::from(size.width) - 2.0 * margin).max(column);
        div()
            .id("key-sheet")
            .absolute()
            .top(px(margin))
            .left(px(margin))
            .w(px(width))
            .max_h(px((f32::from(size.height) - 2.0 * margin).max(margin)))
            .overflow_y_scroll()
            .p(px(DESKTOP.pad * 2.0))
            .bg(tones.paper)
            .text_color(tones.ink)
            .border(px(DESKTOP.rule))
            .border_color(tones.graphite)
            .flex()
            .flex_col()
            .gap(px(DESKTOP.gap * 2.0))
            .child(
                div()
                    .text_color(tones.graphite)
                    .text_size(px(DESKTOP.size.caption))
                    .child("Keys · esc closes"),
            )
            .child(columns)
            .into_any_element()
    }
}

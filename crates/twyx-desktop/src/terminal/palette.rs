//! The command palette and the workspace switcher on a sheet over the
//! panes: the query, then the entries it keeps with the matched letters in
//! the accent and the selection on a tinted bar. A click takes an entry.

use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, MouseButton, MouseDownEvent, Window, div, px};
use twyx_core::theme::DESKTOP;
use twyx_protocol::palette::Picker;

use super::Terminal;
use crate::paint::Tones;

/// The most entries the sheet shows at once.
const ROWS: usize = 12;

impl Terminal {
    pub(super) fn palette_sheet(
        &self,
        picker: &Picker,
        tones: Tones,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = DESKTOP.unit * 7.0;
        let width = DESKTOP.palette_width;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(0.0);
        let entries = self.entries(picker.switcher);
        let found = picker.found(&entries);
        // The list scrolls so the selection stays in view.
        let first = picker.selected.saturating_sub(ROWS - 1);
        let mut list = div().flex().flex_col();
        for (index, hit) in found.iter().enumerate().skip(first).take(ROWS) {
            let entry = &entries[hit.index];
            let selected = index == picker.selected;
            let choice = entry.choice.clone();
            list = list.child(
                div()
                    .relative()
                    .h(px(row))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .gap(px(DESKTOP.gap))
                    .cursor_pointer()
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
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.choose(choice.clone(), cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(matched(&entry.label, &hit.positions, tones, selected))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(entry.kind.clone()),
                    ),
            );
        }
        if found.is_empty() {
            list = list.child(
                div()
                    .h(px(row))
                    .flex()
                    .items_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .text_color(tones.graphite)
                    .child("nothing matches"),
            );
        }
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
                        div()
                            .flex_1()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(picker.query.clone())
                            .child(
                                div()
                                    .w(px(DESKTOP.rule * 2.0))
                                    .h(px(DESKTOP.size.body + 2.0))
                                    .bg(tones.accent),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(if picker.switcher {
                                "workspaces"
                            } else {
                                "palette"
                            }),
                    ),
            )
            .child(list)
            .into_any_element()
    }
}

/// A label with the letters the query matched in the accent.
fn matched(label: &str, positions: &[usize], tones: Tones, selected: bool) -> Div {
    let mut line = div()
        .flex_1()
        .min_w(px(0.0))
        .overflow_hidden()
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

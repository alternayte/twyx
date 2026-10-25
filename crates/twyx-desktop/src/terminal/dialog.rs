//! The shared dialogs drawn on a sheet over the panes: a line of text, a
//! form, or lines to read with the keys that answer them.

use gpui::prelude::*;
use gpui::{AnyElement, Div, FontWeight, Window, div, px};
use twyx_core::theme::DESKTOP;
use twyx_protocol::dialog::Dialog;

use super::Terminal;
use crate::paint::Tones;

impl Terminal {
    pub(super) fn dialog_sheet(
        &self,
        dialog: &Dialog,
        tones: Tones,
        window: &Window,
    ) -> AnyElement {
        let width = DESKTOP.palette_width;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(0.0);
        let caption = |text: String| {
            div()
                .text_size(px(DESKTOP.size.caption))
                .text_color(tones.graphite)
                .child(text)
        };
        let keys = |keys: Vec<(String, String)>| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(DESKTOP.gap * 2.0))
                .pt(px(DESKTOP.gap))
                .text_size(px(DESKTOP.size.caption))
                .children(keys.into_iter().map(|(key, word)| {
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.unit))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(key))
                        .child(div().text_color(tones.graphite).child(word))
                }))
        };
        // The text being typed, with the caret after it.
        let line = |text: &str, active: bool| -> Div {
            div()
                .flex()
                .flex_row()
                .child(text.to_owned())
                .when(active, |line| {
                    line.child(
                        div()
                            .w(px(DESKTOP.rule * 2.0))
                            .h(px(DESKTOP.size.body + 2.0))
                            .bg(tones.accent),
                    )
                })
        };
        let body = match dialog {
            Dialog::Prompt { title, text, .. } => div()
                .flex()
                .flex_col()
                .gap(px(DESKTOP.gap))
                .child(caption(title.clone()))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(DESKTOP.unit * 2.0))
                        .child(div().text_color(tones.accent).child("›"))
                        .child(line(text, true)),
                )
                .child(keys(vec![
                    ("enter".to_owned(), "ok".to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ])),
            Dialog::Form {
                title,
                fields,
                at,
                purpose,
            } => {
                let mut form = div()
                    .flex()
                    .flex_col()
                    .gap(px(DESKTOP.gap))
                    .child(caption(title.clone()));
                if let Some(hint) = purpose.hint() {
                    form = form.child(caption(hint.to_owned()));
                }
                for (number, (label, value)) in fields.iter().enumerate() {
                    let active = number == *at;
                    form = form.child(
                        div()
                            .relative()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(DESKTOP.gap))
                            .pl(px(DESKTOP.pad))
                            .when(active, |row| {
                                row.bg(tones.selection).child(
                                    div()
                                        .absolute()
                                        .left(px(0.0))
                                        .top(px(0.0))
                                        .bottom(px(0.0))
                                        .w(px(DESKTOP.mark))
                                        .bg(tones.accent),
                                )
                            })
                            .child(
                                div()
                                    .w(px(DESKTOP.unit * 18.0))
                                    .flex_shrink_0()
                                    .text_color(tones.graphite)
                                    .child(label.clone()),
                            )
                            .child(line(value, active)),
                    );
                }
                form.child(keys(vec![
                    ("tab".to_owned(), "next".to_owned()),
                    ("enter".to_owned(), purpose.submit().to_owned()),
                    ("esc".to_owned(), "cancel".to_owned()),
                ]))
            }
            Dialog::Confirm {
                title,
                lines,
                keys: offered,
                ..
            } => {
                let mut offered: Vec<(String, String)> = offered
                    .iter()
                    .map(|(key, word)| (key.to_string(), word.clone()))
                    .collect();
                offered.push(("esc".to_owned(), "cancel".to_owned()));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(DESKTOP.gap))
                    .child(caption(title.clone()))
                    .children(lines.iter().map(|text| div().child(text.clone())))
                    .child(keys(offered))
            }
        };
        div()
            .id("dialog")
            .absolute()
            .top(px(DESKTOP.tab_bar + DESKTOP.unit * 12.0))
            .left(px(left))
            .w(px(width))
            .max_h(px((f32::from(window.viewport_size().height)
                - DESKTOP.tab_bar * 2.0
                - DESKTOP.unit * 24.0)
                .max(DESKTOP.unit * 24.0)))
            .overflow_y_scroll()
            .p(px(DESKTOP.pad * 2.0))
            .bg(tones.paper)
            .text_color(tones.ink)
            .border(px(DESKTOP.rule))
            .border_color(tones.graphite)
            .child(body)
            .into_any_element()
    }
}

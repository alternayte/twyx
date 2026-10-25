//! The workspace's files: the tree in the sidebar's files view, and the
//! find sheet over the panes. Both take the keys both clients share; a
//! click opens a directory or opens a file or a found line in Neovim.

use std::time::Instant;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, MouseButton, MouseDownEvent, Window, div, px};
use twyx_core::keys::Chord;
use twyx_core::theme::DESKTOP;
use twyx_protocol::client::ToDaemon;
use twyx_protocol::files::{FindKey, Finder, TreeKey};

use super::Terminal;
use crate::paint::Tones;

/// The most found lines the sheet shows at once.
const ROWS: usize = 12;

impl Terminal {
    /// Shows the files view and gives it the keys, or hides it.
    pub(super) fn toggle_files(&mut self) {
        self.files_view = !self.files_view;
        self.files_focus = self.files_view;
        if self.files_view {
            self.shared().workspaces.docked = true;
        }
    }

    /// Asks for the shown workspace's files when a view needs them: none
    /// yet, another workspace's, or older than the refresh interval.
    pub(super) fn refresh_files(&mut self, wanted: bool) {
        let Some(workspace) = self
            .shared()
            .workspace
            .as_ref()
            .map(|shown| shown.workspace)
        else {
            return;
        };
        let fresh = self.files_asked.is_some_and(|(asked, at)| {
            asked == workspace && at.elapsed() < twyx_core::tuning::REVIEW_REFRESH
        });
        if wanted && !fresh {
            self.files_asked = Some((workspace, Instant::now()));
            self.request(ToDaemon::Files { workspace });
        }
    }

    /// Takes a key while the tree has the keys.
    pub(super) fn tree_key(&mut self, chord: Chord) {
        let key = self.shared().workspaces.files.key(chord);
        match key {
            TreeKey::Stay => {}
            TreeKey::Leave => self.files_focus = false,
            TreeKey::Send(request) => {
                // The file opens in a pane; the keys go there.
                self.files_focus = false;
                self.request(request);
            }
        }
    }

    /// Takes a key in the open find sheet.
    pub(super) fn find_key(&mut self, mut finder: Finder, chord: Chord) {
        match finder.key(chord) {
            FindKey::Stay => self.finder = Some(finder),
            FindKey::Close => {}
            FindKey::Send(request @ ToDaemon::OpenInEditor { .. }) => self.request(request),
            FindKey::Send(request) => {
                self.finder = Some(finder);
                self.request(request);
            }
        }
    }

    /// The files view: the tree of the shown workspace's files.
    pub(super) fn files_view(&self, tones: Tones, cx: &mut Context<Self>) -> Div {
        let (rows, cursor, note) = {
            let shared = self.shared();
            let files = &shared.workspaces.files;
            let note = match &files.list {
                None => Some("reading the files".to_owned()),
                Some(list) => list.error.clone().or_else(|| {
                    if list.paths.is_empty() {
                        Some("no files".to_owned())
                    } else {
                        list.truncated
                            .then(|| format!("the first {} files", list.paths.len()))
                    }
                }),
            };
            (files.rows(), files.cursor, note)
        };
        let cursor = cursor.min(rows.len().saturating_sub(1));
        let mut list = div()
            .id("file-rows")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(px(DESKTOP.pad));
        for (index, row) in rows.into_iter().enumerate() {
            let chosen = index == cursor;
            let path = row.path.clone();
            let directory = row.directory;
            list = list.child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(DESKTOP.unit * 6.0))
                    .pl(px(DESKTOP.pad * 2.0 + row.depth as f32 * DESKTOP.pad * 1.5))
                    .pr(px(DESKTOP.pad * 2.0))
                    .cursor_pointer()
                    .when(chosen, |line| {
                        line.bg(tones.selection).child(
                            div()
                                .absolute()
                                .left(px(0.0))
                                .top(px(0.0))
                                .bottom(px(0.0))
                                .w(px(DESKTOP.mark))
                                .bg(if self.files_focus {
                                    tones.accent
                                } else {
                                    tones.graphite
                                }),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.files_focus = true;
                            {
                                let mut shared = this.shared();
                                shared.workspaces.files.cursor = index;
                                if directory {
                                    shared.workspaces.files.toggle(&path);
                                }
                            }
                            if !directory {
                                this.files_focus = false;
                                this.request(ToDaemon::OpenInEditor {
                                    path: path.clone(),
                                    line: None,
                                });
                            }
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .w(px(DESKTOP.pad * 1.5))
                            .flex_shrink_0()
                            .text_color(tones.graphite)
                            .child(match (row.directory, row.open) {
                                (true, true) => "▾",
                                (true, false) => "▸",
                                (false, _) => "",
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .when(row.directory, |name| name.font_weight(FontWeight::SEMIBOLD))
                            .when(!row.directory, |name| name.text_color(tones.ink))
                            .child(row.name),
                    ),
            );
        }
        if let Some(note) = note {
            list = list.child(
                div()
                    .px(px(DESKTOP.pad * 2.0))
                    .py(px(DESKTOP.gap))
                    .text_size(px(DESKTOP.size.caption))
                    .text_color(tones.graphite)
                    .child(note),
            );
        }
        div().flex_1().min_h(px(0.0)).flex().flex_col().child(list)
    }

    /// The find sheet: the query, then the found lines with their file and
    /// line number.
    pub(super) fn find_sheet(
        &mut self,
        finder: &Finder,
        tones: Tones,
        reduced: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = DESKTOP.unit * 9.0;
        let width = DESKTOP.palette_width * 1.4;
        let left = ((f32::from(window.viewport_size().width) - width) / 2.0).max(0.0);
        let hits = finder.hits().to_vec();
        let selected = finder.selected.min(hits.len().saturating_sub(1));
        let first = selected.saturating_sub(ROWS - 1);
        let mut rows = div().relative().flex().flex_col();
        if !hits.is_empty() {
            rows = rows.child(self.selection_bar(selected, selected - first, row, tones, reduced));
        }
        for (index, hit) in hits.into_iter().enumerate().skip(first).take(ROWS) {
            let request = ToDaemon::OpenInEditor {
                path: hit.path.clone(),
                line: Some(hit.line),
            };
            rows = rows.child(
                div()
                    .h(px(row))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px(px(DESKTOP.pad * 2.0))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.finder = None;
                            this.request(request.clone());
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .when(index == selected, |text| {
                                text.font_weight(FontWeight::SEMIBOLD)
                            })
                            .child(hit.text.trim().replace('\t', "    ")),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(DESKTOP.size.caption))
                            .text_color(tones.graphite)
                            .child(format!("{}:{}", hit.path, hit.line)),
                    ),
            );
        }
        let note = match &finder.found {
            _ if finder.query.is_empty() => Some("type to find text in the files".to_owned()),
            None => Some("finding".to_owned()),
            Some(found) => found.error.clone().or_else(|| {
                if found.hits.is_empty() {
                    (found.query == finder.query).then(|| "nothing found".to_owned())
                } else {
                    found
                        .truncated
                        .then(|| format!("the first {} lines", found.hits.len()))
                }
            }),
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
                            .child(finder.query.clone())
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
                            .child("find in files"),
                    ),
            )
            .child(rows)
            .children(note.map(|note| {
                div()
                    .px(px(DESKTOP.pad * 2.0))
                    .py(px(DESKTOP.gap))
                    .text_size(px(DESKTOP.size.caption))
                    .text_color(tones.graphite)
                    .child(note)
            }))
            .into_any_element()
    }
}

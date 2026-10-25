//! The tab area: the tab's leaves placed in cells, as the TUI places them,
//! each in a hairline frame with its title over the top rule.

use std::time::Instant;

use gpui::{App, Bounds, ContentMask, Pixels, Point, Window, fill, point, px, size};
use twyx_core::layout::{Cells, LeafId, Placed};
use twyx_core::theme::{DESKTOP, Motion};
use twyx_protocol::view::{Selection, View};

use crate::paint::{self, Metrics, Palette, Tones, lerp, outline, text};

/// The cells of the tab area and where they start in the window.
#[derive(Debug, Clone)]
pub struct Grid {
    pub origin: Point<Pixels>,
    pub metrics: Metrics,
}

impl Grid {
    pub fn rect(&self, cells: Cells) -> Bounds<Pixels> {
        let (width, height) = (self.metrics.cell_width, self.metrics.line_height);
        Bounds::new(
            point(
                self.origin.x + width * f32::from(cells.x),
                self.origin.y + height * f32::from(cells.y),
            ),
            size(
                width * f32::from(cells.width),
                height * f32::from(cells.height),
            ),
        )
    }

    /// The cell under a point, when the point is not above or left of the
    /// area.
    pub fn cell(&self, position: Point<Pixels>) -> Option<(u16, u16)> {
        let x = f32::from(position.x - self.origin.x);
        let y = f32::from(position.y - self.origin.y);
        (x >= 0.0 && y >= 0.0).then(|| {
            (
                (x / f32::from(self.metrics.cell_width).max(1.0)) as u16,
                (y / f32::from(self.metrics.line_height).max(1.0)) as u16,
            )
        })
    }

    /// The hairline of a frame: through the middle of its outer cells.
    pub fn frame(&self, cells: Cells) -> Bounds<Pixels> {
        let outer = self.rect(cells);
        let (width, height) = (self.metrics.cell_width, self.metrics.line_height);
        Bounds::new(
            point(outer.origin.x + width / 2.0, outer.origin.y + height / 2.0),
            size(outer.size.width - width, outer.size.height - height),
        )
    }
}

/// One leaf to draw.
#[derive(Debug, Clone)]
pub struct Shown {
    pub placed: Placed,
    pub title: String,
    /// The pane's screen; `None` for a view the client draws itself, or a
    /// pane whose screen has not come yet.
    pub view: Option<View>,
    pub focused: bool,
    /// The text selected in this pane, if any.
    pub selection: Option<Selection>,
}

/// What the tab area draws in one frame.
#[derive(Debug)]
pub struct Scene {
    pub grid: Grid,
    pub bounds: Bounds<Pixels>,
    pub leaves: Vec<Shown>,
    /// Where the focus mark is on its way to the focused frame.
    pub mark: Option<Bounds<Pixels>>,
    /// Where a pane being moved would land.
    pub preview: Option<Bounds<Pixels>>,
    pub tones: Tones,
    pub palette: Palette,
    pub family: &'static str,
}

/// The tab area paints in two layers, so that the client's own views can
/// lie over the tiled panes and under the floating ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The paper and the tiled leaves.
    Tiled,
    /// The floating leaves, the drop preview and the focus mark.
    Floating,
}

pub fn paint(scene: &Scene, layer: Layer, window: &mut Window, cx: &mut App) {
    if layer == Layer::Tiled {
        window.paint_quad(fill(scene.bounds, scene.tones.paper));
    }
    window.with_content_mask(
        Some(ContentMask {
            bounds: scene.bounds,
        }),
        |window| {
            if layer == Layer::Tiled {
                for shown in scene.leaves.iter().filter(|shown| !shown.placed.floating) {
                    leaf(scene, shown, window, cx);
                }
                return;
            }
            for shown in scene.leaves.iter().filter(|shown| shown.placed.floating) {
                window.paint_quad(fill(scene.grid.rect(shown.placed.outer), scene.tones.paper));
                leaf(scene, shown, window, cx);
            }
            if let Some(preview) = scene.preview {
                window.paint_quad(fill(preview, scene.tones.accent.opacity(0.12)));
                outline(window, preview, px(DESKTOP.mark), scene.tones.accent);
            }
            if let Some(mark) = scene.mark {
                outline(window, mark, px(DESKTOP.mark), scene.tones.accent);
                // The mark runs under the focused title, as the rule does.
                if let Some(focused) = scene.leaves.iter().find(|shown| shown.focused) {
                    title(scene, focused, window, cx);
                }
            }
        },
    );
}

fn leaf(scene: &Scene, shown: &Shown, window: &mut Window, cx: &mut App) {
    let (grid, tones) = (&scene.grid, scene.tones);
    let placed = &shown.placed;
    let outer = grid.rect(placed.outer);
    let frame = grid.frame(placed.outer);
    let line = grid.metrics.line_height;
    if placed.content {
        outline(window, frame, px(DESKTOP.rule), tones.rule);
        let inner = grid.rect(placed.inner);
        if let Some(view) = &shown.view {
            let clip = inner.intersect(&scene.bounds);
            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                paint::paint(view, inner, &grid.metrics, scene.palette, window, cx);
                if let Some(selection) = &shown.selection {
                    selected(selection, view, inner, &grid.metrics, tones, window);
                }
            });
        }
    } else {
        // A collapsed stack child: its title row, on a rule.
        window.paint_quad(fill(
            Bounds::new(
                point(frame.origin.x, outer.origin.y + line / 2.0),
                size(frame.size.width, px(DESKTOP.rule)),
            ),
            tones.rule,
        ));
    }
    title(scene, shown, window, cx);
}

/// The selected cells, tinted in the accent, row by row.
fn selected(
    selection: &Selection,
    view: &View,
    inner: Bounds<Pixels>,
    metrics: &Metrics,
    tones: Tones,
    window: &mut Window,
) {
    let ((start_col, start_row), (end_col, end_row)) = selection.span();
    for row in start_row..=end_row.min(view.rows.saturating_sub(1)) {
        let from = if row == start_row { start_col } else { 0 };
        let to = if row == end_row {
            end_col
        } else {
            view.cols.saturating_sub(1)
        };
        let cells = f32::from(to.saturating_sub(from) + 1);
        window.paint_quad(fill(
            Bounds::new(
                point(
                    inner.origin.x + metrics.cell_width * f32::from(from),
                    inner.origin.y + metrics.line_height * f32::from(row),
                ),
                size(metrics.cell_width * cells, metrics.line_height),
            ),
            tones.accent.opacity(0.25),
        ));
    }
}

/// The title over a frame's top rule, on paper so the rule breaks for it.
fn title(scene: &Scene, shown: &Shown, window: &mut Window, cx: &mut App) {
    let (grid, tones) = (&scene.grid, scene.tones);
    let placed = &shown.placed;
    let outer = grid.rect(placed.outer);
    let (cell, line) = (grid.metrics.cell_width, grid.metrics.line_height);
    let room = usize::from(placed.outer.width.saturating_sub(4));
    let title: String = shown.title.chars().take(room).collect();
    if title.is_empty() {
        return;
    }
    let length = title.chars().count() as f32;
    window.paint_quad(fill(
        Bounds::new(
            point(outer.origin.x + cell, outer.origin.y),
            size(cell * (length + 2.0), line),
        ),
        tones.paper,
    ));
    text(
        window,
        cx,
        scene.family,
        &title,
        point(outer.origin.x + cell * 2.0, outer.origin.y),
        DESKTOP.size.body,
        if shown.focused {
            tones.ink
        } else {
            tones.graphite
        },
        shown.focused,
    );
}

/// The pencil mark on its way to a place. It travels when the place is
/// another leaf or tab, and jumps when the same one only moved.
#[derive(Debug, Clone, Copy)]
pub struct Travel<K> {
    key: K,
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    start: Instant,
}

/// The focus mark travels between leaves.
pub type FocusMark = Travel<LeafId>;

impl<K: PartialEq + Copy> Travel<K> {
    pub fn at(key: K, place: Bounds<Pixels>) -> Self {
        Self {
            key,
            from: place,
            to: place,
            start: Instant::now(),
        }
    }

    pub fn go(&mut self, key: K, to: Bounds<Pixels>, reduced: bool) {
        if key != self.key {
            *self = Self {
                key,
                from: self.now(reduced),
                to,
                start: Instant::now(),
            };
        } else if to != self.to {
            *self = Self::at(key, to);
        }
    }

    fn progress(&self, reduced: bool) -> f32 {
        let motion = if reduced {
            DESKTOP.motion.reduced()
        } else {
            DESKTOP.motion
        };
        Motion::ease(motion.travel, self.start.elapsed())
    }

    pub fn now(&self, reduced: bool) -> Bounds<Pixels> {
        let t = self.progress(reduced);
        let (from, to) = (self.from, self.to);
        let mix = |a: Pixels, b: Pixels| px(lerp(a.into(), b.into(), t));
        Bounds::new(
            point(
                mix(from.origin.x, to.origin.x),
                mix(from.origin.y, to.origin.y),
            ),
            size(
                mix(from.size.width, to.size.width),
                mix(from.size.height, to.size.height),
            ),
        )
    }

    pub fn moving(&self, reduced: bool) -> bool {
        self.progress(reduced) < 1.0
    }
}

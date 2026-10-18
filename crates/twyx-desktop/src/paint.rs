//! Paints a pane's grid with GPUI.

use gpui::{
    App, Bounds, Font, FontWeight, Hsla, Pixels, Point, SharedString, TextAlign, TextRun,
    TextSystem, UnderlineStyle, Window, fill, font, point, px, rgb,
};
use twyx_protocol::grid::{Attrs, Cell, Colour, Underline};
use twyx_protocol::view::View;

/// The colours a pane falls back to when a cell names none.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: rgb(0xcdd6f4).into(),
            background: rgb(0x1e1e2e).into(),
            cursor: rgb(0xf5e0dc).into(),
        }
    }
}

/// What a grid needs to know before it can be laid out.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub font_size: Pixels,
    pub font: Font,
}

impl Metrics {
    pub fn measure(text_system: &TextSystem, font_size: Pixels) -> Self {
        let font = font("Menlo");
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .ch_advance(font_id, font_size)
            .unwrap_or(px(8.0));
        // gpui reports the descent below the baseline as a negative value.
        let line_height =
            text_system.ascent(font_id, font_size) - text_system.descent(font_id, font_size);
        Self {
            cell_width,
            line_height,
            font_size,
            font,
        }
    }

    pub fn columns(&self, width: Pixels) -> u16 {
        let cell = f32::from(self.cell_width).max(1.0);
        ((f32::from(width) / cell).floor() as u16).max(2)
    }

    pub fn rows(&self, height: Pixels) -> u16 {
        let line = f32::from(self.line_height).max(1.0);
        ((f32::from(height) / line).floor() as u16).max(1)
    }

    /// The cell a point falls in.
    pub fn cell_at(&self, bounds: Bounds<Pixels>, position: Point<Pixels>) -> (u16, u16) {
        let x = f32::from(position.x - bounds.origin.x) / f32::from(self.cell_width).max(1.0);
        let y = f32::from(position.y - bounds.origin.y) / f32::from(self.line_height).max(1.0);
        (x.max(0.0) as u16, y.max(0.0) as u16)
    }
}

/// Draws every row of the view inside `bounds`.
pub fn paint(
    view: &View,
    bounds: Bounds<Pixels>,
    metrics: &Metrics,
    window: &mut Window,
    cx: &mut App,
) {
    let palette = Palette::default();
    window.paint_quad(fill(bounds, palette.background));

    for (row, cells) in view.cells.iter().enumerate() {
        let origin = point(
            bounds.origin.x,
            bounds.origin.y + metrics.line_height * row as f32,
        );
        if origin.y > bounds.origin.y + bounds.size.height {
            break;
        }
        paint_row(cells, origin, metrics, window, cx, palette);
    }

    if view.cursor.visible {
        let origin = point(
            bounds.origin.x + metrics.cell_width * view.cursor.col as f32,
            bounds.origin.y + metrics.line_height * view.cursor.row as f32,
        );
        window.paint_quad(fill(
            Bounds::new(origin, gpui::size(metrics.cell_width, metrics.line_height)),
            palette.cursor.opacity(0.55),
        ));
    }
}

fn paint_row(
    cells: &[Cell],
    origin: Point<Pixels>,
    metrics: &Metrics,
    window: &mut Window,
    cx: &mut App,
    palette: Palette,
) {
    let (text, runs) = shape_input(cells, metrics, palette);
    if text.is_empty() {
        return;
    }
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text), metrics.font_size, &runs, None);
    let _ = line.paint(
        origin,
        metrics.line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// The text of one row and its styled runs, whose lengths cover the text exactly.
fn shape_input(cells: &[Cell], metrics: &Metrics, palette: Palette) -> (String, Vec<TextRun>) {
    let mut text = String::new();
    let mut runs: Vec<TextRun> = Vec::new();

    for cell in cells {
        // The spacer after a wide cell is covered by the wide cell itself.
        if cell.text.is_empty() {
            continue;
        }
        let run = style_of(cell, metrics, palette);
        let hidden = cell.attrs.contains(Attrs::HIDDEN);
        let len = if hidden { 1 } else { cell.text.len() };
        match runs.last_mut() {
            Some(last)
                if last.color == run.color
                    && last.background_color == run.background_color
                    && last.underline == run.underline
                    && last.strikethrough == run.strikethrough
                    && last.font.weight == run.font.weight
                    && last.font.style == run.font.style =>
            {
                last.len += len;
            }
            _ => runs.push(TextRun { len, ..run }),
        }
        if hidden {
            text.push(' ');
        } else {
            text.push_str(&cell.text);
        }
    }
    (text, runs)
}

fn style_of(cell: &Cell, metrics: &Metrics, palette: Palette) -> TextRun {
    let mut foreground = colour(cell.fg, palette.foreground);
    let mut background = colour(cell.bg, palette.background);
    if cell.attrs.contains(Attrs::REVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    let mut run_font = metrics.font.clone();
    if cell.attrs.contains(Attrs::BOLD) {
        run_font.weight = FontWeight::BOLD;
    }
    if cell.attrs.contains(Attrs::ITALIC) {
        run_font.style = gpui::FontStyle::Italic;
    }
    TextRun {
        len: cell.text.len().max(1),
        font: run_font,
        color: foreground,
        background_color: (background != palette.background).then_some(background),
        underline: match cell.underline {
            Underline::None => None,
            _ => Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(colour(
                    cell.underline_colour.unwrap_or(Colour::Default),
                    foreground,
                )),
                wavy: cell.underline == Underline::Curly,
            }),
        },
        strikethrough: cell
            .attrs
            .contains(Attrs::STRIKE)
            .then(|| gpui::StrikethroughStyle {
                thickness: px(1.0),
                color: Some(foreground),
            }),
    }
}

fn colour(colour: Colour, default: Hsla) -> Hsla {
    match colour {
        Colour::Default => default,
        Colour::Palette(index) => palette_colour(index),
        Colour::Rgb { r, g, b } => {
            rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)).into()
        }
    }
}

/// The 256-colour palette, as a dark theme renders it.
fn palette_colour(index: u8) -> Hsla {
    const ANSI: [u32; 16] = [
        0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70,
        0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
    ];
    match index {
        0..=15 => rgb(ANSI[index as usize]).into(),
        16..=231 => {
            let index = index - 16;
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            rgb(u32::from(level(r)) << 16 | u32::from(level(g)) << 8 | u32::from(level(b))).into()
        }
        _ => {
            let level = 8 + (index - 232) * 10;
            rgb(u32::from(level) << 16 | u32::from(level) << 8 | u32::from(level)).into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_cover_a_row_with_several_styles_exactly() {
        let metrics = Metrics {
            cell_width: px(8.0),
            line_height: px(16.0),
            font_size: px(13.0),
            font: font("Menlo"),
        };
        let cell = |text: &str, fg: Colour| Cell {
            text: text.to_owned(),
            fg,
            ..Cell::blank()
        };
        let cells = vec![
            cell("➜", Colour::Palette(2)),
            cell(" ", Colour::Default),
            cell("字", Colour::Palette(6)),
            cell("", Colour::Palette(6)),
            cell("x", Colour::Default),
        ];

        let (text, runs) = shape_input(&cells, &metrics, Palette::default());

        assert_eq!(text, "➜ 字x");
        assert_eq!(runs.len(), 4);
        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    }
}

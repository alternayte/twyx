//! Paints a pane's grid with GPUI.

use gpui::{
    App, Bounds, Font, FontWeight, Hsla, Pixels, Point, SharedString, TextAlign, TextRun,
    TextSystem, UnderlineStyle, Window, WindowAppearance, fill, font, point, px, rgb, size,
};
use twyx_core::theme::{AgentState, DESKTOP, Rgb, Sheet, Theme, Variant};
use twyx_protocol::grid::{Attrs, Cell, Colour, Underline};
use twyx_protocol::view::View;

/// A pane's colours: the sheet's paper, ink and sixteen terminal colours,
/// with the blue pencil for the cursor.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub ansi: [Hsla; 16],
}

impl Palette {
    pub fn of(variant: Variant) -> Self {
        let sheet = Sheet::of(variant);
        Self {
            foreground: hsla(sheet.ink),
            background: hsla(sheet.paper),
            cursor: hsla(Theme::sheet(variant).accent),
            ansi: sheet.ansi.map(hsla),
        }
    }
}

pub fn hsla(colour: Rgb) -> Hsla {
    rgb(u32::from(colour.r) << 16 | u32::from(colour.g) << 8 | u32::from(colour.b)).into()
}

/// The sheet that matches the system's light or dark appearance.
pub fn variant(appearance: WindowAppearance) -> Variant {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Variant::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Variant::Dark,
    }
}

/// The first of the sheet's fonts that is installed.
pub fn family(text_system: &TextSystem) -> &'static str {
    let installed = text_system.all_font_names();
    DESKTOP
        .fonts
        .iter()
        .copied()
        .find(|family| installed.iter().any(|name| name == family))
        .or_else(|| DESKTOP.fonts.last().copied())
        .unwrap_or("monospace")
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
    pub fn measure(text_system: &TextSystem, family: &'static str, font_size: Pixels) -> Self {
        let font = font(family);
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
}

/// Draws every row of the view inside `bounds`.
pub fn paint(
    view: &View,
    bounds: Bounds<Pixels>,
    metrics: &Metrics,
    palette: Palette,
    window: &mut Window,
    cx: &mut App,
) {
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
            Bounds::new(origin, size(metrics.cell_width, metrics.line_height)),
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
    // GPUI paints a text run's background only on request, and at the
    // glyphs' advances; the cells' own backgrounds go on the grid.
    for (column, width, background) in backgrounds(cells, palette) {
        window.paint_quad(fill(
            Bounds::new(
                point(origin.x + metrics.cell_width * column as f32, origin.y),
                size(metrics.cell_width * width as f32, metrics.line_height),
            ),
            background,
        ));
    }
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

/// Each run of cells whose background is not the paper: its first column,
/// its width in cells, and its colour.
fn backgrounds(cells: &[Cell], palette: Palette) -> Vec<(usize, usize, Hsla)> {
    let mut spans: Vec<(usize, usize, Hsla)> = Vec::new();
    for (column, cell) in cells.iter().enumerate() {
        let (_, background) = colours(cell, palette);
        if background == palette.background {
            continue;
        }
        match spans.last_mut() {
            Some((start, width, colour)) if *start + *width == column && *colour == background => {
                *width += 1;
            }
            _ => spans.push((column, 1, background)),
        }
    }
    spans
}

/// A cell's foreground and background, after reverse video.
fn colours(cell: &Cell, palette: Palette) -> (Hsla, Hsla) {
    let foreground = colour(cell.fg, palette.foreground, &palette);
    let background = colour(cell.bg, palette.background, &palette);
    if cell.attrs.contains(Attrs::REVERSE) {
        (background, foreground)
    } else {
        (foreground, background)
    }
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
    let (foreground, _) = colours(cell, palette);
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
        background_color: None,
        underline: match cell.underline {
            Underline::None => None,
            _ => Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(colour(
                    cell.underline_colour.unwrap_or(Colour::Default),
                    foreground,
                    &palette,
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

fn colour(colour: Colour, default: Hsla, palette: &Palette) -> Hsla {
    match colour {
        Colour::Default => default,
        Colour::Palette(index) if index < 16 => palette.ansi[usize::from(index)],
        Colour::Palette(index) => palette_colour(index),
        Colour::Rgb { r, g, b } => {
            rgb(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)).into()
        }
    }
}

/// The xterm colour cube and grey ramp, above the sheet's sixteen.
fn palette_colour(index: u8) -> Hsla {
    match index {
        232.. => {
            let level = 8 + (index - 232) * 10;
            rgb(u32::from(level) << 16 | u32::from(level) << 8 | u32::from(level)).into()
        }
        _ => {
            let index = index.saturating_sub(16);
            let r = index / 36;
            let g = (index % 36) / 6;
            let b = index % 6;
            let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            rgb(u32::from(level(r)) << 16 | u32::from(level(g)) << 8 | u32::from(level(b))).into()
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

        let (text, runs) = shape_input(&cells, &metrics, Palette::of(Variant::Dark));

        assert_eq!(text, "➜ 字x");
        assert_eq!(runs.len(), 4);
        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
    }

    #[test]
    fn reversed_and_coloured_cells_get_backgrounds_on_the_grid() {
        let palette = Palette::of(Variant::Light);
        let reversed = Cell {
            attrs: Attrs::REVERSE,
            ..Cell::blank()
        };
        let red = Cell {
            bg: Colour::Palette(1),
            ..Cell::blank()
        };
        let cells = vec![
            Cell::blank(),
            reversed.clone(),
            reversed,
            red.clone(),
            Cell::blank(),
            red,
        ];
        assert_eq!(
            backgrounds(&cells, palette),
            vec![
                (1, 2, palette.foreground),
                (3, 1, palette.ansi[1]),
                (5, 1, palette.ansi[1]),
            ],
            "a selection's reverse video shows as the ink behind the cells"
        );
    }
}

/// The colours of one paper.
#[derive(Debug, Clone, Copy)]
pub struct Tones {
    pub paper: Hsla,
    pub ink: Hsla,
    pub graphite: Hsla,
    pub rule: Hsla,
    pub accent: Hsla,
    /// The tinted bar under a selected row.
    pub selection: Hsla,
    pub theme: Theme,
}

impl Tones {
    pub fn of(variant: Variant) -> Self {
        let sheet = Sheet::of(variant);
        let theme = Theme::sheet(variant);
        Self {
            paper: hsla(sheet.paper),
            ink: hsla(sheet.ink),
            graphite: hsla(theme.graphite),
            rule: hsla(theme.rule),
            accent: hsla(theme.accent),
            selection: hsla(theme.selection),
            theme,
        }
    }

    /// These tones with `accent` in place of the sheet's, when there is one.
    pub fn with_accent(self, accent: Option<Rgb>) -> Self {
        match accent {
            Some(accent) => {
                let theme = self.theme.with_accent(accent);
                Self {
                    accent: hsla(theme.accent),
                    theme,
                    ..self
                }
            }
            None => self,
        }
    }

    pub fn agent(&self, state: AgentState) -> Hsla {
        match self.theme.agent(state) {
            twyx_core::theme::Tone::Ink => self.ink,
            twyx_core::theme::Tone::Rgb(colour) => hsla(colour),
        }
    }
}

/// A rectangle drawn in four strokes of `width`, inside `bounds`.
pub fn outline(window: &mut Window, bounds: Bounds<Pixels>, width: Pixels, colour: Hsla) {
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
pub fn text(
    window: &mut Window,
    cx: &mut App,
    family: &'static str,
    content: &str,
    origin: Point<Pixels>,
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

pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

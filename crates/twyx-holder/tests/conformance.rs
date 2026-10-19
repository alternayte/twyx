//! Conformance: bytes in, screen out.
//!
//! The expectations are the engine's screen model, which is the oracle twyx
//! relies on.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use twyx_holder::engine::{self, Geometry};
use twyx_protocol::grid::Underline;

const COLS: u16 = 20;
const ROWS: u16 = 5;

fn screen(input: &[u8]) -> Vec<String> {
    let mut engine = engine::create(
        Geometry {
            cols: COLS,
            rows: ROWS,
            scrollback_lines: 100,
        },
        Box::new(std::io::sink()),
    )
    .expect("the engine starts");
    engine.advance(input);
    engine
        .view()
        .rows
        .iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[test]
fn plain_text_and_carriage_returns() {
    let rows = screen(b"one\r\ntwo");
    assert_eq!(rows[0], "one");
    assert_eq!(rows[1], "two");
    assert_eq!(rows[2], "");
}

#[test]
fn cursor_positioning_puts_text_where_it_is_told() {
    let rows = screen(b"\x1b[3;5Habc");
    assert_eq!(rows[2], "    abc");
}

#[test]
fn erase_in_line_clears_from_the_cursor() {
    let rows = screen(b"abcdef\x1b[1;4H\x1b[K");
    assert_eq!(rows[0], "abc");
}

#[test]
fn erase_in_display_clears_everything() {
    let rows = screen(b"abc\x1b[2J");
    assert_eq!(rows[0], "");
}

#[test]
fn a_wide_character_takes_two_cells() {
    let rows = screen("ab字cd".as_bytes());
    assert_eq!(rows[0], "ab字cd");
}

#[test]
fn a_zwj_emoji_stays_one_cluster() {
    // Family emoji: four people joined by zero width joiners.
    let rows = screen("👨‍👩‍👧‍👦x".as_bytes());
    assert!(rows[0].starts_with("👨‍👩‍👧‍👦"), "{:?}", rows[0]);
    assert!(rows[0].ends_with('x'), "{:?}", rows[0]);
}

#[test]
fn a_combining_mark_attaches_to_its_base() {
    let rows = screen("e\u{0301}x".as_bytes());
    assert!(rows[0].starts_with("e\u{0301}"), "{:?}", rows[0]);
    assert!(rows[0].ends_with('x'), "{:?}", rows[0]);
}

#[test]
fn wrapping_moves_to_the_next_row() {
    let rows = screen(&[b'x'; COLS as usize + 3]);
    assert_eq!(rows[0].len(), COLS as usize);
    assert_eq!(rows[1], "xxx");
}

#[test]
fn output_past_the_last_row_scrolls() {
    // Seven line breaks and an eighth line with no break after it, so the
    // cursor stays on the last line and the viewport is unambiguous.
    let mut input = Vec::new();
    for line in 0..7 {
        input.extend_from_slice(format!("line{line}\r\n").as_bytes());
    }
    input.extend_from_slice(b"line7");
    let rows = screen(&input);
    assert_eq!(rows[0], "line3");
    assert_eq!(rows[ROWS as usize - 1], "line7");
}

#[test]
fn reverse_video_index_reaches_the_cells() {
    let mut engine = engine::create(
        Geometry {
            cols: COLS,
            rows: ROWS,
            scrollback_lines: 100,
        },
        Box::new(std::io::sink()),
    )
    .expect("the engine starts");
    engine.advance(b"\x1b[7mx");
    let cell = engine.view().rows[0].cells[0].clone();
    assert!(cell.attrs.contains(twyx_protocol::grid::Attrs::REVERSE));
}

#[test]
fn underline_styles_reach_the_cells() {
    for (sequence, expected) in [
        (b"\x1b[4m".as_slice(), Underline::Single),
        (b"\x1b[4:2m".as_slice(), Underline::Double),
        (b"\x1b[4:3m".as_slice(), Underline::Curly),
        (b"\x1b[4:4m".as_slice(), Underline::Dotted),
        (b"\x1b[4:5m".as_slice(), Underline::Dashed),
    ] {
        let mut input = sequence.to_vec();
        input.push(b'x');
        let mut engine = engine::create(
            Geometry {
                cols: COLS,
                rows: ROWS,
                scrollback_lines: 100,
            },
            Box::new(std::io::sink()),
        )
        .expect("the engine starts");
        engine.advance(&input);
        let cell = engine.view().rows[0].cells[0].clone();
        assert_eq!(cell.underline, expected, "{sequence:?}");
    }
}

#[test]
fn the_alternate_screen_hides_the_primary_one() {
    let rows = screen(b"primary\x1b[?1049h\x1b[Halternate");
    assert_eq!(rows[0], "alternate");
    let rows = screen(b"primary\x1b[?1049h\x1b[Halternate\x1b[?1049l");
    assert_eq!(rows[0], "primary");
}

#[test]
fn insert_and_delete_lines_move_the_text() {
    let rows = screen(b"a\r\nb\r\nc\x1b[1;1H\x1b[L");
    assert_eq!(rows[0], "");
    assert_eq!(rows[1], "a");
    assert_eq!(rows[2], "b");
    let rows = screen(b"a\r\nb\r\nc\x1b[1;1H\x1b[M");
    assert_eq!(rows[0], "b");
    assert_eq!(rows[1], "c");
}

#[test]
fn a_hyperlink_reaches_the_cell() {
    let mut engine = engine::create(
        Geometry {
            cols: COLS,
            rows: ROWS,
            scrollback_lines: 100,
        },
        Box::new(std::io::sink()),
    )
    .expect("the engine starts");
    engine.advance(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
    let view = engine.view();
    let cell = view.rows[0].cells[0].clone();
    assert_eq!(cell.text, "l");
    let id = cell.link.expect("the cell carries a link");
    let target = view
        .links
        .iter()
        .find(|(link, _)| *link == id)
        .map(|(_, target)| target.as_str());
    assert_eq!(target, Some("https://example.com"));
}

#[test]
fn sgr_reset_returns_to_the_default_attributes() {
    let mut engine = engine::create(
        Geometry {
            cols: COLS,
            rows: ROWS,
            scrollback_lines: 100,
        },
        Box::new(std::io::sink()),
    )
    .expect("the engine starts");
    engine.advance(b"\x1b[1;4ma\x1b[0mb");
    let view = engine.view();
    let plain = view.rows[0].cells[1].clone();
    assert!(plain.attrs.is_empty());
    assert_eq!(plain.underline, Underline::None);
}

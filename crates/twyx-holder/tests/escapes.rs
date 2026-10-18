//! Conformance: escape sequences against expected screens.
//!
//! The categories follow esctest's, but each case is written here from the
//! xterm control sequence documentation. Every case runs against whichever
//! engine this build was compiled with, on a 20x5 screen.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use twyx_holder::engine::{self, Geometry};

const COLS: u16 = 20;
const ROWS: u16 = 5;

struct Screen {
    rows: Vec<String>,
    cursor: (u16, u16),
}

fn run(input: &[u8]) -> Screen {
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
    let view = engine.view();
    Screen {
        rows: view
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
            .collect(),
        cursor: (view.cursor.col, view.cursor.row),
    }
}

fn rows(input: &[u8]) -> Vec<String> {
    run(input).rows
}

fn cursor(input: &[u8]) -> (u16, u16) {
    run(input).cursor
}

// wezterm-term keeps the cursor off the screen after an out-of-range CUP, so
// the next character is never drawn. Recorded for GATE 1.
#[test]
#[cfg_attr(
    feature = "engine-wezterm",
    ignore = "wezterm-term does not clamp CUP; see the gate notes"
)]
fn cup_clamps_to_the_screen() {
    let screen = run(b"\x1b[99;99HX");
    assert_eq!(screen.rows[4], format!("{}X", " ".repeat(19)));
    assert_eq!(screen.cursor, (19, 4));
}

#[test]
fn relative_cursor_moves_stop_at_the_edges() {
    assert_eq!(cursor(b"\x1b[3;3H\x1b[10A"), (2, 0));
    assert_eq!(cursor(b"\x1b[3;3H\x1b[10B"), (2, 4));
    assert_eq!(cursor(b"\x1b[3;3H\x1b[30C"), (19, 2));
    assert_eq!(cursor(b"\x1b[3;3H\x1b[10D"), (0, 2));
}

#[test]
fn cha_and_vpa_set_one_coordinate() {
    assert_eq!(rows(b"\x1b[5G\x1b[3dX")[2], "    X");
}

#[test]
fn ed_0_erases_from_the_cursor_down() {
    assert_eq!(
        rows(b"aaa\r\nbbb\r\nccc\x1b[2;2H\x1b[J"),
        ["aaa", "b", "", "", ""]
    );
}

// alacritty_terminal 0.26.0 clears the lines above the cursor only when the
// cursor is below line 1, so from line 1 the top line survives. Recorded in
// the upstream table as a draft.
#[test]
#[cfg_attr(
    not(any(feature = "engine-ghostty", feature = "engine-wezterm")),
    ignore = "alacritty_terminal keeps line 0 when the cursor is on line 1; see the gate notes"
)]
fn ed_1_erases_from_the_top_to_the_cursor() {
    assert_eq!(
        rows(b"aaa\r\nbbb\r\nccc\x1b[2;2H\x1b[1J"),
        ["", "  b", "ccc", "", ""]
    );
}

#[test]
fn el_1_and_el_2_erase_left_and_the_whole_line() {
    assert_eq!(rows(b"abcdef\x1b[1;3H\x1b[1K")[0], "   def");
    assert_eq!(rows(b"abcdef\x1b[1;3H\x1b[2K")[0], "");
}

#[test]
fn ich_inserts_blanks_and_shifts_right() {
    assert_eq!(rows(b"abcdef\x1b[1;2H\x1b[2@")[0], "a  bcdef");
}

#[test]
fn dch_deletes_and_shifts_left() {
    assert_eq!(rows(b"abcdef\x1b[1;2H\x1b[2P")[0], "adef");
}

#[test]
fn ech_erases_in_place_without_moving_the_cursor() {
    let screen = run(b"abcdef\x1b[1;2H\x1b[2X");
    assert_eq!(screen.rows[0], "a  def");
    assert_eq!(screen.cursor, (1, 0));
}

#[test]
fn rep_repeats_the_last_character() {
    assert_eq!(rows(b"a\x1b[3b")[0], "aaaa");
}

#[test]
fn il_inserts_a_line_and_pushes_the_rest_down() {
    assert_eq!(rows(b"a\r\nb\r\nc\x1b[2;1H\x1b[L"), ["a", "", "b", "c", ""]);
}

#[test]
fn dl_deletes_a_line_and_pulls_the_rest_up() {
    assert_eq!(rows(b"a\r\nb\r\nc\x1b[1;1H\x1b[M"), ["b", "c", "", "", ""]);
}

#[test]
fn a_line_feed_at_the_bottom_margin_scrolls_only_the_region() {
    assert_eq!(
        rows(b"1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[4;1H\n"),
        ["1", "3", "4", "", "5"]
    );
}

#[test]
fn su_and_sd_scroll_the_screen() {
    assert_eq!(rows(b"a\r\nb\r\nc\x1b[S"), ["b", "c", "", "", ""]);
    assert_eq!(rows(b"a\r\nb\x1b[T"), ["", "a", "b", "", ""]);
}

#[test]
fn ri_at_the_top_scrolls_down() {
    assert_eq!(rows(b"a\r\nb\x1b[1;1H\x1bM"), ["", "a", "b", "", ""]);
}

#[test]
fn nel_moves_to_the_start_of_the_next_line() {
    assert_eq!(rows(b"ab\x1bEc")[..2], ["ab", "c"]);
}

#[test]
fn decsc_and_decrc_restore_the_cursor() {
    assert_eq!(rows(b"\x1b[2;3H\x1b7\x1b[5;5H\x1b8X")[1], "  X");
}

#[test]
fn origin_mode_addresses_inside_the_margins() {
    let screen = run(b"\x1b[2;4r\x1b[?6h\x1b[1;1HX");
    assert_eq!(screen.rows[1], "X");
    assert_eq!(screen.cursor, (1, 1));
}

#[test]
fn default_tab_stops_are_every_eight_columns() {
    assert_eq!(rows(b"a\tb")[0], "a       b");
}

#[test]
fn tbc_and_hts_replace_the_tab_stops() {
    assert_eq!(rows(b"\x1b[3g\x1b[1;4H\x1bH\r\tX")[0], "   X");
}

#[test]
fn autowrap_continues_on_the_next_line() {
    let screen = rows(b"abcdefghijklmnopqrstuvwxy");
    assert_eq!(screen[0], "abcdefghijklmnopqrst");
    assert_eq!(screen[1], "uvwxy");
}

#[test]
fn without_autowrap_the_last_column_is_overwritten() {
    let screen = rows(b"\x1b[?7labcdefghijklmnopqrstuvwxyz");
    assert_eq!(screen[0], "abcdefghijklmnopqrsz");
    assert_eq!(screen[1], "");
}

#[test]
fn the_alternate_screen_restores_the_main_screen_and_cursor() {
    let screen = run(b"main\x1b[?1049h\x1b[2JALT\x1b[?1049l");
    assert_eq!(screen.rows[0], "main");
    assert_eq!(screen.cursor, (4, 0));
}

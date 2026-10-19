//! Holder messages recorded from earlier builds, which this build must still
//! read: the daemon adopts holders that outlived it, and they keep speaking
//! the protocol they started with.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::Cursor as Bytes;

use twyx_protocol::grid::{
    Attrs, Cell, Colour, Cursor, CursorShape, Frame, Modes, Row, RowUpdate, Screen, Snapshot,
    Underline,
};
use twyx_protocol::holder::{ExitState, FromHolder, PromptMark, Signal, Welcome};
use twyx_protocol::wire;

/// Every kind of message a holder of protocol 1 sends, with real bodies.
fn messages() -> Vec<FromHolder> {
    let cell = Cell {
        text: "é".to_owned(),
        fg: Colour::Rgb { r: 1, g: 2, b: 3 },
        bg: Colour::Palette(4),
        attrs: Attrs::BOLD | Attrs::ITALIC,
        underline: Underline::Curly,
        underline_colour: Some(Colour::Default),
        link: Some(1),
    };
    let screen = Screen {
        cols: 2,
        rows: 1,
        cursor: Cursor {
            col: 1,
            row: 0,
            shape: CursorShape::Beam,
            visible: true,
        },
        modes: Modes {
            bracketed_paste: true,
            alt_screen: true,
            kitty_keyboard: 3,
            ..Modes::default()
        },
        title: "nvim".to_owned(),
    };
    let snapshot = Snapshot {
        seq: 7,
        screen: screen.clone(),
        rows: vec![Row {
            cells: vec![cell.clone(), Cell::blank()],
        }],
        links: vec![(1, "https://example.com".to_owned())],
        scrollback: 12,
    };
    let frame = Frame {
        seq: 8,
        screen,
        changed: vec![RowUpdate {
            row: 0,
            cells: vec![Cell::blank(), cell],
        }],
        links: Vec::new(),
        scrollback: 13,
    };
    vec![
        FromHolder::Welcome(Welcome {
            major: 1,
            minor: 0,
            pane: 42,
            build: "0.1.0-recorded".to_owned(),
            snapshot: wire::encode_body(&snapshot).expect("encode"),
        }),
        FromHolder::Frame {
            seq: 8,
            body: wire::encode_body(&frame).expect("encode"),
        },
        FromHolder::Snapshot {
            seq: 7,
            body: wire::encode_body(&snapshot).expect("encode"),
        },
        FromHolder::Signal(Signal::Title("nvim".to_owned())),
        FromHolder::Signal(Signal::Bell),
        FromHolder::Signal(Signal::ClipboardWrite("copied".to_owned())),
        FromHolder::Signal(Signal::PromptMark(PromptMark::CommandExecuted(Some(1)))),
        FromHolder::Signal(Signal::Notification {
            title: "done".to_owned(),
            body: "tests passed".to_owned(),
        }),
        FromHolder::Exited(ExitState::Exited { code: 3 }),
        FromHolder::Exited(ExitState::Killed {
            signal: "SIGKILL".to_owned(),
        }),
        FromHolder::Pong,
        FromHolder::Unsupported { major: 2, minor: 0 },
        // Since minor 1.
        FromHolder::Copied("line 18 word".to_owned()),
    ]
}

const RECORDED_1: &[u8] = include_bytes!("fixtures/holder-1.bin");

#[test]
fn invariant_19_the_daemon_reads_recorded_messages_of_this_holder_major() {
    let expected = messages();
    let mut bytes = Bytes::new(RECORDED_1);
    let mut read = Vec::new();
    while let Some(message) = wire::read::<FromHolder>(&mut bytes).expect("a recorded message") {
        read.push(message);
    }
    assert_eq!(read, expected);
    // The bodies, which clients read, decode as well.
    for message in &read {
        match message {
            FromHolder::Welcome(Welcome { snapshot: body, .. })
            | FromHolder::Snapshot { body, .. } => {
                wire::decode_body::<Snapshot>(body).expect("a recorded snapshot");
            }
            FromHolder::Frame { body, .. } => {
                wire::decode_body::<Frame>(body).expect("a recorded frame");
            }
            _ => {}
        }
    }
}

/// Writes the fixture. Run it once when a new major starts, then keep the
/// file; never write over the file of a released major.
#[test]
#[ignore = "writes the fixture"]
fn record_holder_messages() {
    let mut bytes = Vec::new();
    for message in messages() {
        wire::write(&mut bytes, &message).expect("record");
    }
    std::fs::write(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/holder-1.bin"),
        bytes,
    )
    .expect("write the fixture");
}

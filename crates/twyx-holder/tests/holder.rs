//! The holder serves the daemon's socket, publishes frames, and keeps a
//! finished pane's screen until the daemon acknowledges it.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use twyx_core::pane::PaneId;
use twyx_holder::engine::{self, Geometry};
use twyx_holder::pty::Spawn;
use twyx_holder::runtime::{self, Config};
use twyx_protocol::grid::{Row, Snapshot};
use twyx_protocol::holder::{ExitState, FromHolder, Hello, MAJOR, MINOR, ToHolder, Welcome};
use twyx_protocol::wire;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    socket: PathBuf,
    pane: PaneId,
}

impl Fixture {
    fn new(script: &str, exit_grace: Duration) -> (Self, thread::JoinHandle<()>) {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket = std::env::temp_dir()
            .join(format!("twyx-holder-test-{}-{id}", std::process::id()))
            .join("pane.sock");
        let pane = PaneId::from_raw(1);
        let spawn = Spawn {
            argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            cols: 40,
            rows: 6,
        };
        let config = Config {
            socket: socket.clone(),
            pane,
            build: "test".to_owned(),
            spawn,
            scrollback_lines: 100,
            exit_grace,
        };
        let geometry = Geometry {
            cols: 40,
            rows: 6,
            scrollback_lines: 100,
        };
        let handle = thread::spawn(move || {
            // The engine is built inside the holder's thread, because
            // libghostty-vt handles are single-threaded.
            runtime::run(config, move |answerback| {
                engine::create(geometry, answerback)
            })
            .expect("the holder runs");
        });
        (Self { socket, pane }, handle)
    }

    fn connect(&self) -> (BufReader<UnixStream>, UnixStream) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(&self.socket) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .expect("set a read timeout");
                let reader = BufReader::new(stream.try_clone().expect("clone the stream"));
                return (reader, stream);
            }
            assert!(Instant::now() < deadline, "the holder never listened");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn attach(&self) -> (BufReader<UnixStream>, UnixStream, Snapshot) {
        let (mut reader, mut writer) = self.connect();
        wire::write(
            &mut writer,
            &ToHolder::Hello(Hello {
                major: MAJOR,
                minor: MINOR,
                pane: self.pane.raw(),
                build: "test".to_owned(),
            }),
        )
        .expect("send the hello");
        match read(&mut reader) {
            FromHolder::Welcome(Welcome { snapshot, .. }) => {
                let snapshot: Snapshot = wire::decode_body(&snapshot).expect("decode the snapshot");
                (reader, writer, snapshot)
            }
            other => panic!("expected a welcome, got {other:?}"),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.socket.parent().expect("a parent directory"));
    }
}

fn read(reader: &mut BufReader<UnixStream>) -> FromHolder {
    wire::read(reader)
        .expect("read a message")
        .expect("the holder stays connected")
}

fn text(rows: &[Row]) -> String {
    rows.iter()
        .map(|row| {
            row.cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_holder_publishes_frames_and_keeps_the_final_screen_until_the_daemon_acknowledges() {
    let (fixture, handle) = Fixture::new("printf 'hello'; exec sleep 30", Duration::from_secs(5));
    let (mut reader, mut writer, snapshot) = fixture.attach();

    let mut seen = text(&snapshot.rows);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !seen.contains("hello") {
        match read(&mut reader) {
            FromHolder::Frame { body, .. } => {
                let frame: twyx_protocol::grid::Frame =
                    wire::decode_body(&body).expect("decode a frame");
                for update in frame.changed {
                    seen.push_str(&text(&[Row {
                        cells: update.cells,
                    }]));
                }
            }
            FromHolder::Signal(_) => {}
            other => panic!("unexpected message {other:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "the screen never showed the output"
        );
    }

    wire::write(&mut writer, &ToHolder::Kill).expect("send the kill");
    match read_until_exit(&mut reader) {
        ExitState::Killed { .. } => {}
        other => panic!("expected the kill to end the pane, got {other:?}"),
    }

    // The final screen is still there after the process ended.
    wire::write(&mut writer, &ToHolder::Snapshot).expect("ask for a snapshot");
    let snapshot = loop {
        match read(&mut reader) {
            FromHolder::Snapshot { body, .. } => {
                break wire::decode_body::<Snapshot>(&body).expect("decode the snapshot");
            }
            FromHolder::Frame { .. } | FromHolder::Signal(_) => {}
            other => panic!("unexpected message {other:?}"),
        }
    };
    assert!(text(&snapshot.rows).contains("hello"));

    wire::write(&mut writer, &ToHolder::AckExit).expect("send the acknowledgement");
    handle
        .join()
        .expect("the holder exits after the acknowledgement");
}

fn read_until_exit(reader: &mut BufReader<UnixStream>) -> ExitState {
    loop {
        match read(reader) {
            FromHolder::Exited(state) => return state,
            FromHolder::Frame { .. } | FromHolder::Signal(_) => {}
            other => panic!("unexpected message {other:?}"),
        }
    }
}

#[test]
fn a_holder_keeps_the_pane_after_the_daemon_goes_away() {
    let (fixture, handle) = Fixture::new("exec sleep 30", Duration::from_secs(5));
    let (reader, writer, _) = fixture.attach();
    drop(reader);
    drop(writer);

    // A new daemon finds the same pane and the same screen.
    let (mut reader, mut writer, _) = fixture.attach();
    wire::write(&mut writer, &ToHolder::Ping).expect("send a ping");
    loop {
        match read(&mut reader) {
            FromHolder::Pong => break,
            FromHolder::Frame { .. } | FromHolder::Signal(_) => {}
            other => panic!("unexpected message {other:?}"),
        }
    }

    wire::write(&mut writer, &ToHolder::Kill).expect("send the kill");
    read_until_exit(&mut reader);
    wire::write(&mut writer, &ToHolder::AckExit).expect("send the acknowledgement");
    handle
        .join()
        .expect("the holder exits after the acknowledgement");
}

#[test]
fn a_holder_exits_at_the_end_of_the_grace_period_without_an_acknowledgement() {
    let (fixture, handle) = Fixture::new("exit 0", Duration::from_millis(200));
    let (mut reader, writer, _) = fixture.attach();
    read_until_exit(&mut reader);
    drop(writer);
    drop(reader);
    handle.join().expect("the holder exits on its own");
}

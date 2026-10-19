//! A pane in a real holder on a real PTY, driven as a client drives it.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use twyx_core::pane::PaneId;
use twyx_holder::engine::{self, Geometry};
use twyx_holder::pty::Spawn;
use twyx_holder::runtime::{self, Config};
use twyx_protocol::grid::{Cell, Frame, Row, Screen, Snapshot};
use twyx_protocol::holder::{FromHolder, Hello, MAJOR, MINOR, Signal, ToHolder, Welcome};
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};
use twyx_protocol::wire;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct Seen {
    pub screen: Option<Screen>,
    pub rows: Vec<Row>,
    pub clipboard: Vec<String>,
    pub exited: bool,
}

pub struct Pane {
    socket_dir: PathBuf,
    writer: UnixStream,
    pub seen: Arc<Mutex<Seen>>,
}

impl Pane {
    pub fn start(argv: Vec<String>, cwd: &Path, cols: u16, rows: u16) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket_dir =
            std::env::temp_dir().join(format!("twyx-pane-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&socket_dir).expect("create the socket directory");
        let socket = socket_dir.join("pane.sock");

        let spawn = Spawn {
            argv,
            cwd: cwd.to_path_buf(),
            env: vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ],
            cols,
            rows,
        };
        let config = Config {
            socket: socket.clone(),
            pane: PaneId::from_raw(1),
            build: "test".to_owned(),
            spawn,
            scrollback_lines: 1000,
            exit_grace: Duration::from_millis(100),
            history: None,
        };
        let geometry = Geometry {
            cols,
            rows,
            scrollback_lines: 1000,
        };
        thread::spawn(move || {
            runtime::run(config, move |answerback| {
                engine::create(geometry, answerback)
            })
            .expect("the holder runs");
        });

        let (reader, mut writer) = connect(&socket);
        wire::write(
            &mut writer,
            &ToHolder::Hello(Hello {
                major: MAJOR,
                minor: MINOR,
                pane: 1,
                build: "test".to_owned(),
            }),
        )
        .expect("send the hello");

        let seen = Arc::new(Mutex::new(Seen::default()));
        let sink = Arc::clone(&seen);
        thread::spawn(move || follow(reader, &sink));

        Self {
            socket_dir,
            writer,
            seen,
        }
    }

    pub fn send(&mut self, event: InputEvent) {
        wire::write(&mut self.writer, &ToHolder::Input(event)).expect("send an input event");
    }

    pub fn key(&mut self, code: KeyCode, mods: Modifiers) {
        self.send(InputEvent::Key(KeyEvent::press(code, mods)));
    }

    /// Types text one key at a time, as a hand would.
    pub fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.send(InputEvent::Key(KeyEvent {
                text: Some(ch.to_string()),
                ..KeyEvent::press(KeyCode::Char(ch), Modifiers::empty())
            }));
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        wire::write(&mut self.writer, &ToHolder::Resize { cols, rows }).expect("send a resize");
    }

    pub fn screen(&self) -> Option<Screen> {
        self.seen.lock().unwrap().screen.clone()
    }

    pub fn rows(&self) -> Vec<Row> {
        self.seen.lock().unwrap().rows.clone()
    }

    /// The visible text, one trimmed string per row.
    pub fn text(&self) -> Vec<String> {
        self.rows()
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

    pub fn exited(&self) -> bool {
        self.seen.lock().unwrap().exited
    }

    pub fn any_cell(&self, matches: impl Fn(&Cell) -> bool) -> bool {
        self.rows().iter().flat_map(|row| &row.cells).any(matches)
    }

    pub fn wait_for(&self, what: &str, done: impl FnMut() -> bool) {
        self.wait_for_within(what, Duration::from_secs(10), done);
    }

    pub fn wait_for_within(&self, what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting: {what}\nscreen:\n{}",
                self.text().join("\n")
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the screen has not changed for `quiet`.
    pub fn wait_until_still(&self, quiet: Duration, limit: Duration) {
        let deadline = Instant::now() + limit;
        let mut last = self.text();
        let mut since = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(100));
            let now = self.text();
            if now != last {
                last = now;
                since = Instant::now();
            } else if since.elapsed() >= quiet {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the screen never settled\nscreen:\n{}",
                last.join("\n")
            );
        }
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = wire::write(&mut self.writer, &ToHolder::Kill);
        let _ = wire::write(&mut self.writer, &ToHolder::AckExit);
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

fn connect(socket: &Path) -> (BufReader<UnixStream>, UnixStream) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stream) = UnixStream::connect(socket) {
            let reader = BufReader::new(stream.try_clone().expect("clone the stream"));
            return (reader, stream);
        }
        assert!(Instant::now() < deadline, "the holder never listened");
        thread::sleep(Duration::from_millis(10));
    }
}

fn follow(mut reader: BufReader<UnixStream>, seen: &Mutex<Seen>) {
    while let Ok(Some(message)) = wire::read::<FromHolder>(&mut reader) {
        let mut seen = seen.lock().unwrap();
        match message {
            FromHolder::Welcome(Welcome { snapshot, .. })
            | FromHolder::Snapshot { body: snapshot, .. } => {
                let snapshot: Snapshot = wire::decode_body(&snapshot).expect("decode a snapshot");
                seen.screen = Some(snapshot.screen);
                seen.rows = snapshot.rows;
            }
            FromHolder::Frame { body, .. } => {
                let frame: Frame = wire::decode_body(&body).expect("decode a frame");
                seen.rows
                    .resize(usize::from(frame.screen.rows), Row { cells: Vec::new() });
                for update in frame.changed {
                    if let Some(row) = seen.rows.get_mut(usize::from(update.row)) {
                        row.cells = update.cells;
                    }
                }
                seen.screen = Some(frame.screen);
            }
            FromHolder::Signal(Signal::ClipboardWrite(text)) => seen.clipboard.push(text),
            FromHolder::Exited(_) => seen.exited = true,
            _ => {}
        }
    }
}

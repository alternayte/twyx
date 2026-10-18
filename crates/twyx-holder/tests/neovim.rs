//! Conformance: the Neovim items, through a real holder on a real PTY.
//!
//! Each case starts Neovim in a holder built with this build's engine, sends
//! input events as a client would, and checks two oracles: Neovim's own state
//! through its RPC socket, and the engine's screen model in the holder's
//! frames. Neovim must be on PATH.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use twyx_core::pane::PaneId;
use twyx_holder::engine::{self, Geometry};
use twyx_holder::pty::Spawn;
use twyx_holder::runtime::{self, Config};
use twyx_protocol::grid::{Cell, Colour, Frame, MouseMode, Row, Screen, Snapshot, Underline};
use twyx_protocol::holder::{FromHolder, Hello, MAJOR, MINOR, Signal, ToHolder, Welcome};
use twyx_protocol::input::{
    InputEvent, KeyCode, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
};
use twyx_protocol::wire;

const COLS: u16 = 80;
const ROWS: u16 = 24;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Seen {
    screen: Option<Screen>,
    rows: Vec<Row>,
    clipboard: Vec<String>,
}

struct Nvim {
    dir: PathBuf,
    server: PathBuf,
    writer: UnixStream,
    seen: Arc<Mutex<Seen>>,
}

impl Nvim {
    fn start(init_lua: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("twyx-nvim-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the test directory");
        let init = dir.join("init.lua");
        std::fs::write(&init, init_lua).expect("write init.lua");
        let server = dir.join("nvim.sock");
        let socket = dir.join("pane.sock");

        let spawn = Spawn {
            argv: vec![
                "nvim".to_owned(),
                "--clean".to_owned(),
                "-n".to_owned(),
                "-u".to_owned(),
                init.display().to_string(),
                "--listen".to_owned(),
                server.display().to_string(),
            ],
            cwd: dir.clone(),
            env: vec![
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ],
            cols: COLS,
            rows: ROWS,
        };
        let config = Config {
            socket: socket.clone(),
            pane: PaneId::from_raw(1),
            build: "test".to_owned(),
            spawn,
            scrollback_lines: 100,
            exit_grace: Duration::from_millis(100),
        };
        let geometry = Geometry {
            cols: COLS,
            rows: ROWS,
            scrollback_lines: 100,
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

        let nvim = Self {
            dir,
            server,
            writer,
            seen,
        };
        nvim.wait_for("Neovim answers on its socket", || {
            nvim.try_eval("v:vim_did_enter").as_deref() == Some("1")
        });
        nvim
    }

    fn try_eval(&self, expr: &str) -> Option<String> {
        if !self.server.exists() {
            return None;
        }
        let mut child = Command::new("nvim")
            .arg("--server")
            .arg(&self.server)
            .arg("--remote-expr")
            .arg(expr)
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        // A Neovim that waits on the terminal never answers; fail, do not hang.
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().ok()?.is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn eval(&self, expr: &str) -> String {
        self.try_eval(expr)
            .unwrap_or_else(|| panic!("Neovim did not evaluate {expr}"))
    }

    fn send(&mut self, event: InputEvent) {
        wire::write(&mut self.writer, &ToHolder::Input(event)).expect("send an input event");
    }

    fn key(&mut self, code: KeyCode, mods: Modifiers) {
        self.send(InputEvent::Key(KeyEvent::press(code, mods)));
    }

    fn mouse(&mut self, action: MouseAction, col: u16, row: u16) {
        self.send(InputEvent::Mouse(MouseEvent {
            col,
            row,
            button: MouseButton::Left,
            action,
            mods: Modifiers::empty(),
            pixel_col: None,
            pixel_row: None,
        }));
    }

    fn wait_for(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_eval(&self, expr: &str, expected: &str) {
        let mut last = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            last = self.try_eval(expr).or(last);
            if last.as_deref() == Some(expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{expr} is {last:?}, expected {expected:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen(&self) -> Option<Screen> {
        self.seen.lock().unwrap().screen.clone()
    }

    fn rows(&self) -> Vec<Row> {
        self.seen.lock().unwrap().rows.clone()
    }

    fn any_cell(&self, matches: impl Fn(&Cell) -> bool) -> bool {
        self.rows().iter().flat_map(|row| &row.cells).any(matches)
    }
}

impl Drop for Nvim {
    fn drop(&mut self) {
        let _ = wire::write(&mut self.writer, &ToHolder::Kill);
        let _ = wire::write(&mut self.writer, &ToHolder::AckExit);
        let _ = std::fs::remove_dir_all(&self.dir);
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
            _ => {}
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Colour {
    Colour::Rgb { r, g, b }
}

#[test]
fn truecolor_reaches_the_screen_model() {
    let nvim = Nvim::start(
        "vim.o.termguicolors = true\n\
         vim.api.nvim_set_hl(0, 'Normal', { fg = '#123456', bg = '#fedcba' })\n\
         vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'truecolor' })\n",
    );
    nvim.wait_for("a cell in Normal's 24-bit colours", || {
        nvim.any_cell(|cell| {
            cell.text == "t" && cell.fg == rgb(0x12, 0x34, 0x56) && cell.bg == rgb(0xfe, 0xdc, 0xba)
        })
    });
}

// A known gap, not a passing case: with TERM=xterm-256color, which has no
// Smulx, Neovim draws a plain underline with no colour on alacritty_terminal
// and wezterm-term. Recorded for GATE 1.
#[test]
#[ignore = "Neovim falls back to a plain underline; see the gate notes"]
fn undercurl_and_underline_colour_reach_the_screen_model() {
    let nvim = Nvim::start(
        "vim.o.termguicolors = true\n\
         vim.api.nvim_set_hl(0, 'Curly', { undercurl = true, sp = '#ff0000' })\n\
         vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'curly' })\n\
         vim.fn.matchadd('Curly', 'curly')\n",
    );
    nvim.wait_for("a red undercurl under the match", || {
        nvim.any_cell(|cell| {
            cell.text == "c"
                && cell.underline == Underline::Curly
                && cell.underline_colour == Some(rgb(0xff, 0, 0))
        })
    });
}

#[test]
#[cfg_attr(
    feature = "engine-wezterm",
    ignore = "wezterm-term does not report the kitty keyboard flags; see the gate notes"
)]
fn ctrl_i_tab_shift_enter_and_ctrl_shift_x_arrive_as_different_keys() {
    let mut nvim = Nvim::start(
        "vim.g.keys = {}\n\
         for lhs, name in pairs({ ['<C-i>'] = 'ci', ['<Tab>'] = 'tab', ['<S-CR>'] = 'scr', ['<C-S-x>'] = 'csx' }) do\n\
           vim.keymap.set('n', lhs, function() local k = vim.g.keys; table.insert(k, name); vim.g.keys = k end)\n\
         end\n",
    );
    nvim.wait_for("Neovim turns on the kitty keyboard protocol", || {
        nvim.screen()
            .is_some_and(|screen| screen.modes.kitty_keyboard != 0)
    });

    nvim.key(KeyCode::Tab, Modifiers::empty());
    nvim.key(KeyCode::Char('i'), Modifiers::CTRL);
    nvim.key(KeyCode::Enter, Modifiers::SHIFT);
    nvim.send(InputEvent::Key(KeyEvent {
        shifted: Some('X'),
        ..KeyEvent::press(KeyCode::Char('x'), Modifiers::CTRL | Modifiers::SHIFT)
    }));

    nvim.wait_for_eval("join(g:keys, ',')", "tab,ci,scr,csx");
}

#[test]
fn a_one_megabyte_bracketed_paste_arrives_intact() {
    let mut nvim = Nvim::start("");
    nvim.wait_for("Neovim turns on bracketed paste", || {
        nvim.screen()
            .is_some_and(|screen| screen.modes.bracketed_paste)
    });

    let pattern = "0123456789abcdef";
    nvim.send(InputEvent::Paste(pattern.repeat(65_536)));

    nvim.wait_for_eval("strlen(getline(1))", "1048576");
    assert_eq!(
        nvim.eval("getline(1) ==# repeat('0123456789abcdef', 65536) && line('$') == 1"),
        "1"
    );
}

#[test]
fn focus_gained_and_lost_fire() {
    let mut nvim = Nvim::start(
        "vim.g.focus = {}\n\
         local function note(name) return function() local f = vim.g.focus; table.insert(f, name); vim.g.focus = f end end\n\
         vim.api.nvim_create_autocmd('FocusLost', { callback = note('lost') })\n\
         vim.api.nvim_create_autocmd('FocusGained', { callback = note('gained') })\n",
    );
    nvim.wait_for("Neovim turns on focus events", || {
        nvim.screen()
            .is_some_and(|screen| screen.modes.focus_events)
    });
    nvim.eval("execute('let g:focus = []')");

    nvim.send(InputEvent::Focus(false));
    nvim.wait_for_eval("join(g:focus, ',')", "lost");
    nvim.send(InputEvent::Focus(true));
    nvim.wait_for_eval("join(g:focus, ',')", "lost,gained");
}

#[test]
#[cfg_attr(
    feature = "engine-wezterm",
    ignore = "wezterm-term does not report OSC 52 writes without a clipboard handler; see the gate notes"
)]
fn an_osc_52_yank_reaches_the_holder_as_a_clipboard_write() {
    let mut nvim = Nvim::start(
        "vim.g.clipboard = 'osc52'\n\
         vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'yanked by twyx' })\n",
    );
    for code in ['"', '+', 'y', 'y'] {
        nvim.key(KeyCode::Char(code), Modifiers::empty());
    }
    nvim.wait_for("a clipboard write with the yanked line", || {
        nvim.seen
            .lock()
            .unwrap()
            .clipboard
            .iter()
            .any(|text| text.trim_end() == "yanked by twyx")
    });
}

#[test]
#[cfg_attr(
    feature = "engine-wezterm",
    ignore = "wezterm-term does not report the mouse mode Neovim sets; see the gate notes"
)]
fn a_mouse_drag_on_a_split_border_resizes_the_split() {
    let mut nvim = Nvim::start("vim.o.mouse = 'a'\nvim.cmd('vsplit')\n");
    nvim.wait_for("Neovim turns on mouse tracking", || {
        nvim.screen()
            .is_some_and(|screen| screen.modes.mouse != MouseMode::Off)
    });
    let border = nvim.eval("winwidth(1)").parse::<u16>().expect("a width");

    // Paced like a hand: with no gap after the press, Neovim does not start
    // the separator drag.
    nvim.mouse(MouseAction::Press, border, 5);
    for col in (border - 10..border).rev() {
        thread::sleep(Duration::from_millis(20));
        nvim.mouse(MouseAction::Motion, col, 5);
    }
    thread::sleep(Duration::from_millis(20));
    nvim.mouse(MouseAction::Release, border - 10, 5);

    nvim.wait_for_eval("winwidth(1)", &(border - 10).to_string());
}

#[test]
fn cjk_and_zwj_emoji_keep_the_cursor_in_place() {
    let nvim = Nvim::start(
        "vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'a字b👩\u{200d}💻c' })\n\
         vim.cmd('normal! $')\n",
    );
    let expected = nvim
        .eval("virtcol('.') - 1")
        .parse::<u16>()
        .expect("a column");
    assert_eq!(expected, 6, "Neovim's own width for 'a字b👩‍💻'");

    nvim.wait_for("the engine's cursor on the 'c'", || {
        let Some(screen) = nvim.screen() else {
            return false;
        };
        let rows = nvim.rows();
        let on_c = rows
            .first()
            .and_then(|row| row.cells.get(usize::from(screen.cursor.col)))
            .is_some_and(|cell| cell.text == "c");
        screen.cursor.row == 0 && screen.cursor.col == expected && on_c
    });
}

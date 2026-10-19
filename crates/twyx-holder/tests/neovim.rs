//! Conformance: the Neovim items, through a real holder on a real PTY.
//!
//! Each case starts Neovim in a holder, sends input events as a client would,
//! and checks two oracles: Neovim's own state through its RPC socket, and the
//! engine's screen model in the holder's frames. Neovim must be on PATH.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use twyx_protocol::grid::{Colour, MouseMode, Underline};
use twyx_protocol::input::{
    InputEvent, KeyCode, KeyEvent, Modifiers, MouseAction, MouseButton, MouseEvent,
};

use support::Pane;

const COLS: u16 = 80;
const ROWS: u16 = 24;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Nvim {
    dir: PathBuf,
    server: PathBuf,
    pane: Pane,
}

impl Deref for Nvim {
    type Target = Pane;

    fn deref(&self) -> &Pane {
        &self.pane
    }
}

impl DerefMut for Nvim {
    fn deref_mut(&mut self) -> &mut Pane {
        &mut self.pane
    }
}

impl Nvim {
    fn start(init_lua: &str) -> Self {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("twyx-nvim-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the test directory");
        let init = dir.join("init.lua");
        std::fs::write(&init, init_lua).expect("write init.lua");
        let server = dir.join("nvim.sock");

        let argv = vec![
            "nvim".to_owned(),
            "--clean".to_owned(),
            "-n".to_owned(),
            "-u".to_owned(),
            init.display().to_string(),
            "--listen".to_owned(),
            server.display().to_string(),
        ];
        let pane = Pane::start(argv, &dir, COLS, ROWS);
        let nvim = Self { dir, server, pane };
        // Eight Neovims start at once, and a small CI runner is slow to
        // bring them all up.
        nvim.wait_for_within(
            "Neovim answers on its socket",
            Duration::from_secs(30),
            || nvim.try_eval("v:vim_did_enter").as_deref() == Some("1"),
        );
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

    /// One slow call on a loaded machine is not a failure; ten seconds of
    /// them is.
    fn eval(&self, expr: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(value) = self.try_eval(expr) {
                return value;
            }
            assert!(Instant::now() < deadline, "Neovim did not evaluate {expr}");
        }
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
}

impl Drop for Nvim {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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

#[test]
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
fn an_osc_52_yank_reaches_the_holder_as_a_clipboard_write() {
    let mut nvim = Nvim::start(
        "vim.g.clipboard = 'osc52'\n\
         vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'yanked by twyx' })\n",
    );
    for code in ['"', '+', 'y', 'y'] {
        nvim.key(KeyCode::Char(code), Modifiers::empty());
    }
    nvim.wait_for("a clipboard write with the yanked line", || {
        nvim.pane
            .seen
            .lock()
            .unwrap()
            .clipboard
            .iter()
            .any(|text| text.trim_end() == "yanked by twyx")
    });
}

#[test]
fn a_mouse_drag_on_a_split_border_resizes_the_split() {
    let mut nvim = Nvim::start("vim.o.mouse = 'a'\nvim.cmd('vsplit')\n");
    nvim.wait_for("Neovim turns on mouse tracking", || {
        nvim.screen()
            .is_some_and(|screen| screen.modes.mouse != MouseMode::Off)
    });
    let border = nvim.eval("winwidth(1)").parse::<u16>().expect("a width");

    // Paced like a hand: Neovim missed the separator drag when the drags came
    // right behind the press, so each event waits until Neovim has taken the
    // one before it.
    let at = |col: u16| (u32::from(col) + 1).to_string();
    nvim.mouse(MouseAction::Press, border, 5);
    nvim.wait_for_eval("getmousepos().screencol", &at(border));
    for col in (border - 10..border).rev() {
        nvim.mouse(MouseAction::Motion, col, 5);
        nvim.wait_for_eval("getmousepos().screencol", &at(col));
    }
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

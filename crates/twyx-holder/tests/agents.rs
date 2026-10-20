//! Conformance: agent CLIs in a real holder, with the user's own sign-in.
//!
//! Ignored by default because each case runs a real agent against a real
//! model; `just conformance-agents` runs them on a machine where Claude Code,
//! Pi and OpenCode are installed and signed in. Each agent works in its own
//! fixed directory, so a folder trust prompt is answered once per machine.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};

use support::Pane;

const COLS: u16 = 100;
const ROWS: u16 = 30;

struct Agent {
    name: &'static str,
    /// Screen text of a startup dialog, and the keys that dismiss it.
    dialogs: &'static [(&'static str, &'static [Key])],
    /// The keys that stop an answer. Pi clears its editor on Ctrl-C and
    /// OpenCode exits on it; Pi stops on Escape, OpenCode on a second Escape.
    interrupt: &'static [Key],
}

#[derive(Clone, Copy)]
enum Key {
    Down,
    Enter,
    Escape,
    CtrlC,
}

impl Key {
    fn press(self, pane: &mut Pane) {
        let (code, mods) = match self {
            Key::Down => (KeyCode::Down, Modifiers::empty()),
            Key::Enter => (KeyCode::Enter, Modifiers::empty()),
            Key::Escape => (KeyCode::Escape, Modifiers::empty()),
            Key::CtrlC => (KeyCode::Char('c'), Modifiers::CTRL),
        };
        pane.key(code, mods);
    }
}

const CLAUDE: Agent = Agent {
    name: "claude",
    dialogs: &[("Yes, I trust this folder", &[Key::Down, Key::Enter])],
    interrupt: &[Key::CtrlC],
};

const PI: Agent = Agent {
    name: "pi",
    dialogs: &[],
    interrupt: &[Key::Escape],
};

const OPENCODE: Agent = Agent {
    name: "opencode",
    dialogs: &[("Update Available", &[Key::Escape])],
    interrupt: &[Key::Escape, Key::Escape],
};

const COUNT_PROMPT: &str =
    "Write the whole numbers from 1 to COUNT, one per line, with nothing else. Do not use tools.";

fn start(agent: &Agent) -> Pane {
    let dir = workdir(agent);
    // Run from inside an agent session, Claude Code inherits that session's
    // markers and leaves a typed prompt unsent; start each agent clean.
    let mut argv = vec!["env".to_owned()];
    for (name, _) in std::env::vars() {
        if name.starts_with("CLAUDE") {
            argv.extend(["-u".to_owned(), name]);
        }
    }
    argv.push(agent.name.to_owned());
    let mut pane = Pane::start(argv, &dir, COLS, ROWS);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        pane.wait_until_still(Duration::from_secs(4), Duration::from_secs(60));
        let screen = pane.text().join("\n");
        let Some((_, keys)) = agent.dialogs.iter().find(|(text, _)| screen.contains(text)) else {
            return pane;
        };
        for key in *keys {
            key.press(&mut pane);
            thread::sleep(Duration::from_millis(100));
        }
        assert!(
            Instant::now() < deadline,
            "{} never got past its dialogs",
            agent.name
        );
    }
}

fn workdir(agent: &Agent) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("twyx-conformance-agents")
        .join(agent.name);
    std::fs::create_dir_all(&dir).expect("create the agent's directory");
    dir
}

fn submit(pane: &mut Pane, text: &str) {
    pane.type_text(text);
    // Under load an agent can still be taking in the text; an Enter that
    // arrives then is lost.
    let start: String = text.chars().take(24).collect();
    pane.wait_for_within("the typed prompt", Duration::from_secs(20), || {
        pane.text().iter().any(|row| row.contains(&start))
    });
    thread::sleep(Duration::from_millis(300));
    pane.key(KeyCode::Enter, Modifiers::empty());
}

/// The largest number shown alone at the end of a row, if any.
fn highest_number(pane: &Pane) -> u32 {
    pane.text()
        .iter()
        .filter_map(|row| row.split_whitespace().last()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

fn row_of(pane: &Pane, needle: &str) -> Option<usize> {
    pane.text().iter().position(|row| row.contains(needle))
}

fn multiline_input(agent: &Agent) {
    let mut pane = start(agent);
    pane.type_text("twyx-line-one");
    pane.send(InputEvent::Key(KeyEvent::press(
        KeyCode::Enter,
        Modifiers::SHIFT,
    )));
    pane.type_text("twyx-line-two");
    pane.send(InputEvent::Key(KeyEvent::press(
        KeyCode::Enter,
        Modifiers::SHIFT,
    )));
    pane.type_text("twyx-line-three");

    pane.wait_for("three lines in the input", || {
        match (
            row_of(&pane, "twyx-line-one"),
            row_of(&pane, "twyx-line-two"),
            row_of(&pane, "twyx-line-three"),
        ) {
            (Some(one), Some(two), Some(three)) => one < two && two < three,
            _ => false,
        }
    });
    // Nothing was submitted: the input still holds the first line after a pause.
    thread::sleep(Duration::from_secs(2));
    assert!(row_of(&pane, "twyx-line-one").is_some());
}

fn interrupt_during_streaming(agent: &Agent) {
    let mut pane = start(agent);
    submit(&mut pane, &COUNT_PROMPT.replace("COUNT", "1000"));
    pane.wait_for_within("streaming output", Duration::from_secs(120), || {
        highest_number(&pane) >= 20
    });

    for key in agent.interrupt {
        key.press(&mut pane);
        thread::sleep(Duration::from_millis(100));
    }
    pane.wait_until_still(Duration::from_secs(3), Duration::from_secs(30));
    let reached = highest_number(&pane);
    assert!(reached < 1000, "the answer ran to the end: {reached}");
    assert!(
        !pane.exited(),
        "{} exited instead of stopping the answer",
        agent.name
    );
}

fn resize_during_streaming(agent: &Agent) {
    let mut pane = start(agent);
    submit(&mut pane, &COUNT_PROMPT.replace("COUNT", "400"));
    pane.wait_for_within("streaming output", Duration::from_secs(120), || {
        highest_number(&pane) >= 20
    });

    pane.resize(70, 24);
    thread::sleep(Duration::from_secs(1));
    pane.resize(COLS, ROWS);
    pane.wait_until_still(Duration::from_secs(5), Duration::from_secs(300));

    let screen = pane.screen().expect("a screen");
    assert_eq!((screen.cols, screen.rows), (COLS, ROWS));
    assert!(
        pane.text()
            .iter()
            .any(|row| row.split_whitespace().last() == Some("400")),
        "the last number is not on screen:\n{}",
        pane.text().join("\n")
    );
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn claude_takes_multiline_input() {
    multiline_input(&CLAUDE);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn claude_stops_on_ctrl_c_during_streaming() {
    interrupt_during_streaming(&CLAUDE);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn claude_survives_a_resize_during_streaming() {
    resize_during_streaming(&CLAUDE);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn pi_takes_multiline_input() {
    multiline_input(&PI);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn pi_stops_on_escape_during_streaming() {
    interrupt_during_streaming(&PI);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn pi_survives_a_resize_during_streaming() {
    resize_during_streaming(&PI);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn opencode_takes_multiline_input() {
    multiline_input(&OPENCODE);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn opencode_stops_on_escape_during_streaming() {
    interrupt_during_streaming(&OPENCODE);
}

#[test]
#[ignore = "runs a real agent; use just conformance-agents"]
fn opencode_survives_a_resize_during_streaming() {
    resize_during_streaming(&OPENCODE);
}

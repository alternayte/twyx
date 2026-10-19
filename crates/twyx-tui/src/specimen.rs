//! The theme specimen: the chrome drawn from the tokens over sample panes.
//!
//! The tabs are the specimen's scenes. Resize the terminal to check the rules
//! at every size.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use twyx_core::specimen::{AGENT, EDITOR, Run, SHELL};
use twyx_core::theme::{AgentState, Face, SPACE, Sheet, Theme, Tone, Variant};

use crate::chrome::bars::{HintBar, StatusBar, Tab, TabBar, put};
use crate::chrome::frames::{Frame, Frames};
use crate::chrome::palette::{Entry, Palette};
use crate::chrome::{Depth, Pen};

const SCENES: [&str; 5] = ["workspace", "prefix", "palette", "inbox", "sheet"];

/// Opens the specimen on the user's own paper, or on a sheet twyx paints.
pub fn run(painted: Option<Variant>) -> Result<()> {
    let answers = crate::app::probe()?;
    let depth = if answers.truecolor {
        Depth::Truecolor
    } else {
        Depth::Palette256
    };
    let pen = match painted {
        Some(variant) => Pen::painted(Theme::sheet(variant), depth),
        None => Pen::on_paper(Theme::on(answers.paper, answers.ink, Variant::Dark), depth),
    };

    let mut terminal = ratatui::init();
    let mut scene = 0;
    let result = loop {
        if let Err(error) = terminal.draw(|frame| {
            let area = frame.area();
            draw(&pen, scene, area, frame.buffer_mut());
        }) {
            break Err(error).context("draw the specimen");
        }
        match event::read().context("read a key") {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Tab | KeyCode::Right => scene = (scene + 1) % SCENES.len(),
                KeyCode::BackTab | KeyCode::Left => {
                    scene = (scene + SCENES.len() - 1) % SCENES.len();
                }
                KeyCode::Char(digit @ '1'..='5') => scene = usize::from(digit as u8 - b'1'),
                _ => {}
            },
            Ok(_) => {}
            Err(error) => break Err(error),
        }
    };
    ratatui::restore();
    result
}

fn draw(pen: &Pen, scene: usize, area: Rect, buf: &mut Buffer) {
    buf.set_style(area, pen.paper());
    if area.height < 3 {
        return;
    }
    let tabs: Vec<Tab<'_>> = SCENES
        .iter()
        .map(|name| Tab {
            name,
            agent: (*name == "inbox").then_some(AgentState::NeedsApproval),
        })
        .collect();
    TabBar {
        pen,
        tabs: &tabs,
        active: scene,
        workspace: "twyx · main",
    }
    .render(Rect { height: 1, ..area }, buf);

    let sheet = Rect {
        y: area.y + 1,
        height: area.height - 2,
        ..area
    };
    let mode = match SCENES[scene] {
        "prefix" => "command",
        "palette" => "palette",
        _ => "pane",
    };
    StatusBar {
        pen,
        mode,
        worktree: "twyx",
        branch: "main",
        dirty: true,
        note: "",
        roll_up: &[
            (AgentState::NeedsApproval, 1),
            (AgentState::Failed, 1),
            (AgentState::Done, 2),
            (AgentState::Working, 2),
            (AgentState::Idle, 1),
        ],
    }
    .render(
        Rect {
            y: area.bottom() - 1,
            height: 1,
            ..area
        },
        buf,
    );

    match SCENES[scene] {
        "inbox" => inbox(pen, sheet, buf),
        "sheet" => tokens(pen, sheet, buf),
        name => {
            workspace(pen, sheet, buf);
            if name == "prefix" && sheet.height >= 2 {
                HintBar {
                    pen,
                    hints: &[
                        ("p", "pane"),
                        ("t", "tab"),
                        ("r", "resize"),
                        ("m", "move"),
                        ("c", "copy"),
                        ("w", "workspace"),
                        ("f", "float"),
                        (":", "palette"),
                        ("esc", "back"),
                    ],
                }
                .render(
                    Rect {
                        y: sheet.bottom() - 1,
                        height: 1,
                        ..sheet
                    },
                    buf,
                );
            }
            if name == "palette" {
                palette(pen, sheet, buf);
            }
        }
    }
}

fn sample(pen: &Pen, inner: Rect, rows: &[&[Run]], buf: &mut Buffer) {
    for (number, row) in rows.iter().enumerate().take(usize::from(inner.height)) {
        let y = inner.y + number as u16;
        let mut x = inner.x;
        for (text, colour, bold) in row.iter() {
            let mut style = pen.paper();
            if let Some(index) = colour {
                style = style.fg(pen.ansi(*index));
            }
            if *bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            x = put(buf, x, y, inner.right(), text, style);
        }
    }
}

fn inner(outer: Rect) -> Rect {
    Rect {
        x: outer.x + 1,
        y: outer.y + 1,
        width: outer.width.saturating_sub(2),
        height: outer.height.saturating_sub(2),
    }
}

/// Three panes: a shell on the left, an editor and an agent on the right.
/// A sheet too small for three gets one.
fn workspace(pen: &Pen, sheet: Rect, buf: &mut Buffer) {
    if sheet.width >= 48 && sheet.height >= 9 {
        let left = sheet.width / 2;
        let top = sheet.height / 2;
        let frames = [
            Frame {
                outer: Rect {
                    width: left + 1,
                    ..sheet
                },
                title: "zsh",
            },
            Frame {
                outer: Rect {
                    x: sheet.x + left,
                    width: sheet.width - left,
                    height: top + 1,
                    ..sheet
                },
                title: "nvim theme.rs",
            },
            Frame {
                outer: Rect {
                    x: sheet.x + left,
                    y: sheet.y + top,
                    width: sheet.width - left,
                    height: sheet.height - top,
                },
                title: "claude",
            },
        ];
        Frames {
            pen,
            frames: &frames,
            focused: Some(2),
        }
        .render(sheet, buf);
        for (frame, rows) in frames.iter().zip([SHELL, EDITOR, AGENT]) {
            sample(pen, inner(frame.outer), rows, buf);
        }
    } else {
        let frames = [Frame {
            outer: sheet,
            title: "claude",
        }];
        Frames {
            pen,
            frames: &frames,
            focused: Some(0),
        }
        .render(sheet, buf);
        sample(pen, inner(sheet), AGENT, buf);
    }
}

fn palette(pen: &Pen, sheet: Rect, buf: &mut Buffer) {
    let entries = [
        Entry {
            text: "split pane right",
            kind: "command",
            matched: &[0, 1, 9],
        },
        Entry {
            text: "twyx / spec-theme",
            kind: "worktree",
            matched: &[7, 8, 9],
        },
        Entry {
            text: "claude · twyx / main",
            kind: "agent session",
            matched: &[],
        },
        Entry {
            text: "specimen",
            kind: "pane",
            matched: &[0, 1, 2],
        },
    ];
    Palette {
        pen,
        title: "palette",
        query: "spe",
        entries: &entries,
        selected: 0,
    }
    .render(sheet, buf);
}

/// The inbox: one agent session in each state.
fn inbox(pen: &Pen, sheet: Rect, buf: &mut Buffer) {
    let frames = [Frame {
        outer: sheet,
        title: "inbox",
    }];
    Frames {
        pen,
        frames: &frames,
        focused: Some(0),
    }
    .render(sheet, buf);
    let area = inner(sheet);
    let kind = pen.theme.type_;
    let sessions = [
        ("claude", "twyx / spec-theme", "wants to run cargo test"),
        ("opencode", "twyx / fix-lease", "exited with status 1"),
        ("pi", "twyx / main", "finished: 3 files changed"),
        ("claude", "twyx / bench", "writing"),
        ("codex", "notes", "waiting for a prompt"),
        ("cursor-agent", "scratch", "no rule matched the screen"),
    ];
    let left = area.x + SPACE.pad;
    for (row, (state, (agent, place, detail))) in AgentState::ALL.iter().zip(sessions).enumerate() {
        let y = area.y + 2 * row as u16;
        if y >= area.bottom() {
            break;
        }
        let right = area.right().saturating_sub(SPACE.pad);
        let tone = pen.fg(pen.theme.agent(*state));
        let mut x = put(buf, left, y, right, &format!("{} ", state.glyph()), tone);
        x = put(buf, x, y, right, &format!("{:<16}", state.word()), tone);
        x = put(
            buf,
            x,
            y,
            right,
            &format!("{agent:<14}"),
            pen.face(kind.label),
        );
        x = put(
            buf,
            x,
            y,
            right,
            &format!("{place:<20}"),
            pen.face(kind.caption),
        );
        put(buf, x, y, right, detail, pen.face(kind.caption));
    }
}

/// The tokens themselves: the text roles, the ramp, the accent, the states
/// and the sixteen terminal colours of both reference sheets.
fn tokens(pen: &Pen, sheet: Rect, buf: &mut Buffer) {
    let frames = [Frame {
        outer: sheet,
        title: "sheet",
    }];
    Frames {
        pen,
        frames: &frames,
        focused: Some(0),
    }
    .render(sheet, buf);
    let area = inner(sheet);
    let left = area.x + SPACE.pad;
    let right = area.right().saturating_sub(SPACE.pad);
    let theme = pen.theme;
    let kind = theme.type_;
    let mut y = area.y;
    let mut line = |entries: &[(String, Style)], buf: &mut Buffer| {
        if y < area.bottom() {
            let mut x = left;
            for (text, style) in entries {
                x = put(buf, x, y, right, text, *style);
            }
        }
        y += 1;
    };
    let label = |name: &str| (format!("{name:<12}"), pen.face(kind.caption));
    let face = |text: &str, face: Face| (text.to_owned(), pen.face(face));
    let swatch = |tone: Tone| ("████ ".to_owned(), pen.fg(tone));

    line(&[label("title"), face("twyx specimen", kind.title)], buf);
    line(
        &[
            label("label"),
            face("crates/twyx-core/src/theme.rs", kind.label),
        ],
        buf,
    );
    line(
        &[
            label("caption"),
            face("worktree · 3 panes · 2 agents", kind.caption),
        ],
        buf,
    );
    line(&[label("key"), face("ctrl-g", kind.key)], buf);
    line(&[label("mode"), face("COMMAND", kind.mode)], buf);
    line(&[], buf);
    line(
        &[
            label("ramp"),
            swatch(Tone::Ink),
            swatch(Tone::Rgb(theme.graphite)),
            swatch(Tone::Rgb(theme.rule)),
            face(" ink · graphite · rule", kind.caption),
        ],
        buf,
    );
    line(
        &[
            label("accent"),
            swatch(Tone::Rgb(theme.accent)),
            face(" the focused pane, tab and mode", kind.caption),
        ],
        buf,
    );
    line(&[], buf);
    let states: Vec<(String, Style)> = std::iter::once(label("agents"))
        .chain(AgentState::ALL.iter().map(|state| {
            (
                format!("{} {}   ", state.glyph(), state.word()),
                pen.fg(theme.agent(*state)),
            )
        }))
        .collect();
    line(&states, buf);
    line(&[], buf);
    let ansi = |name: &str, base: u8| -> Vec<(String, Style)> {
        std::iter::once(label(name))
            .chain(
                (base..base + 8).map(|index| ("███ ".to_owned(), pen.paper().fg(pen.ansi(index)))),
            )
            .collect()
    };
    line(&ansi("terminal", 0), buf);
    line(&ansi("bright", 8), buf);
    line(&[], buf);
    for (name, variant) in [
        ("light sheet", Variant::Light),
        ("dark sheet", Variant::Dark),
    ] {
        let reference = Sheet::of(variant);
        let mut row = vec![label(name)];
        row.extend(reference.ansi.iter().map(|rgb| {
            (
                "██".to_owned(),
                Style::new().fg(pen.rgb(*rgb)).bg(pen.rgb(reference.paper)),
            )
        }));
        line(&row, buf);
    }
}

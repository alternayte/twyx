//! Mouse events on the composed screen.
//!
//! A press on another pane focuses it. A press on a rule between panes drags
//! it; on a floating pane's top rule it moves the pane, and on its bottom
//! right corner it resizes it. Over the focused pane, a program that asked
//! for the mouse gets it; otherwise a drag selects text, and the release
//! copies it. Shift always selects.

use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use twyx_core::layout::{Cells, Fractions, LeafId, Side, rule_at, under};
use twyx_protocol::client::ToDaemon;
use twyx_protocol::grid::MouseMode;
use twyx_protocol::holder::CopyAction;
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};

use crate::compositor::Scene;
use crate::input;

/// Text being selected in one pane, in the pane's own cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub pane: u64,
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// The first and last cell, in reading order.
    pub fn span(&self) -> ((u16, u16), (u16, u16)) {
        let key = |(col, row): (u16, u16)| (row, col);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, col: u16, row: u16) -> bool {
        let (start, end) = self.span();
        (row, col) >= (start.1, start.0) && (row, col) <= (end.1, end.0)
    }
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    Rule {
        leaf: LeafId,
        side: Side,
        last: (u16, u16),
    },
    Floating {
        leaf: LeafId,
        start: (u16, u16),
        geometry: Fractions,
        resize: bool,
    },
    Select(Selection),
    /// The pane took the press; its drags and release go to it too.
    Pane,
}

/// What the client does with one mouse event.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send(ToDaemon),
    /// The wheel scrolled a pane back: the client is in copy mode now.
    OpenCopy,
    /// Put this text on the outer terminal's clipboard.
    Copy(String),
}

#[derive(Debug, Default)]
pub struct Mouse {
    drag: Option<Drag>,
}

impl Mouse {
    pub fn selection(&self) -> Option<Selection> {
        match self.drag {
            Some(Drag::Select(selection)) => Some(selection),
            _ => None,
        }
    }

    pub fn handle(&mut self, scene: &Scene<'_>, mouse: MouseEvent) -> Vec<Action> {
        let at = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.press(scene, mouse),
            MouseEventKind::Drag(MouseButton::Left) => self.drag(scene, mouse, at),
            MouseEventKind::Up(MouseButton::Left) => self.release(scene, mouse),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => wheel(scene, mouse),
            _ => to_focused(scene, mouse).into_iter().collect(),
        }
    }

    fn press(&mut self, scene: &Scene<'_>, mouse: MouseEvent) -> Vec<Action> {
        let (col, row) = (mouse.column, mouse.row);
        let placed = scene.placed();
        let mut actions = Vec::new();

        if let Some(floating) = placed
            .iter()
            .rev()
            .find(|p| p.floating && p.holds(col, row))
        {
            if Some(floating.leaf.id) != scene.focus {
                actions.push(Action::Send(ToDaemon::FocusLeaf {
                    leaf: floating.leaf.id,
                }));
            }
            let outer = floating.outer;
            let corner = col + 1 == outer.right() && row + 1 == outer.bottom();
            if (row == outer.y || corner)
                && let Some(geometry) = scene.floating_geometry(floating.leaf.id)
            {
                self.drag = Some(Drag::Floating {
                    leaf: floating.leaf.id,
                    start: (col, row),
                    geometry,
                    resize: corner,
                });
                return actions;
            }
            if !actions.is_empty() {
                return actions;
            }
        } else if let Some((leaf, side)) =
            tab_area(scene).and_then(|area| rule_at(&placed, area, col, row))
        {
            self.drag = Some(Drag::Rule {
                leaf,
                side,
                last: (col, row),
            });
            return actions;
        }

        let Some(target) = under(&placed, col, row) else {
            return actions;
        };
        if Some(target.leaf.id) != scene.focus {
            actions.push(Action::Send(ToDaemon::FocusLeaf {
                leaf: target.leaf.id,
            }));
            return actions;
        }
        let Some(local) = target.local(col, row) else {
            return actions;
        };
        let Some(pane) = target.leaf.terminal().map(|pane| pane.raw()) else {
            return actions;
        };
        if wants_mouse(scene, pane) && !mouse.modifiers.contains(KeyModifiers::SHIFT) {
            self.drag = Some(Drag::Pane);
            actions.extend(to_focused(scene, mouse));
        } else {
            self.drag = Some(Drag::Select(Selection {
                pane,
                anchor: local,
                head: local,
            }));
        }
        actions
    }

    fn drag(&mut self, scene: &Scene<'_>, mouse: MouseEvent, at: (u16, u16)) -> Vec<Action> {
        let Some(drag) = self.drag else {
            return to_focused(scene, mouse).into_iter().collect();
        };
        match drag {
            Drag::Rule { leaf, side, last } => {
                let cells = match side {
                    Side::Right => i32::from(at.0) - i32::from(last.0),
                    _ => i32::from(at.1) - i32::from(last.1),
                };
                if cells == 0 {
                    return Vec::new();
                }
                self.drag = Some(Drag::Rule {
                    leaf,
                    side,
                    last: at,
                });
                vec![Action::Send(ToDaemon::DragRule { leaf, side, cells })]
            }
            Drag::Floating {
                leaf,
                start,
                geometry,
                resize,
            } => {
                let Some((cols, rows)) = scene.tab_size() else {
                    return Vec::new();
                };
                let dx = (f32::from(at.0) - f32::from(start.0)) / f32::from(cols.max(1));
                let dy = (f32::from(at.1) - f32::from(start.1)) / f32::from(rows.max(1));
                let geometry = if resize {
                    Fractions {
                        width: geometry.width + dx,
                        height: geometry.height + dy,
                        ..geometry
                    }
                } else {
                    Fractions {
                        x: geometry.x + dx,
                        y: geometry.y + dy,
                        ..geometry
                    }
                };
                vec![Action::Send(ToDaemon::MoveFloating { leaf, geometry })]
            }
            Drag::Select(selection) => {
                let placed = scene.placed();
                if let Some(target) = placed
                    .iter()
                    .find(|p| p.leaf.terminal().map(|pane| pane.raw()) == Some(selection.pane))
                {
                    let inner = target.inner;
                    let col = at.0.clamp(inner.x, inner.right().saturating_sub(1)) - inner.x;
                    let row = at.1.clamp(inner.y, inner.bottom().saturating_sub(1)) - inner.y;
                    self.drag = Some(Drag::Select(Selection {
                        head: (col, row),
                        ..selection
                    }));
                }
                Vec::new()
            }
            Drag::Pane => to_focused(scene, mouse).into_iter().collect(),
        }
    }

    fn release(&mut self, scene: &Scene<'_>, mouse: MouseEvent) -> Vec<Action> {
        match self.drag.take() {
            Some(Drag::Select(selection)) if selection.anchor != selection.head => {
                let text = scene.selected_text(&selection);
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![Action::Copy(text)]
                }
            }
            Some(Drag::Pane) | None => to_focused(scene, mouse).into_iter().collect(),
            Some(_) => Vec::new(),
        }
    }
}

/// The wheel scrolls a program that asked for the mouse. A full-screen
/// program that did not gets arrow keys, as most terminals send.
fn wheel(scene: &Scene<'_>, mouse: MouseEvent) -> Vec<Action> {
    let placed = scene.placed();
    let Some(target) = under(&placed, mouse.column, mouse.row) else {
        return Vec::new();
    };
    if Some(target.leaf.id) != scene.focus {
        return Vec::new();
    }
    let Some(pane) = target.leaf.terminal().map(|pane| pane.raw()) else {
        return Vec::new();
    };
    if wants_mouse(scene, pane) {
        return to_focused(scene, mouse).into_iter().collect();
    }
    if !scene
        .views
        .get(&pane)
        .is_some_and(|view| view.modes.alt_screen)
    {
        // A shell's history scrolls in copy mode, as in tmux.
        let lines = if mouse.kind == MouseEventKind::ScrollUp {
            -3
        } else {
            3
        };
        return vec![
            Action::OpenCopy,
            Action::Send(ToDaemon::Copy(CopyAction::Scroll(lines))),
        ];
    }
    let code = if mouse.kind == MouseEventKind::ScrollUp {
        KeyCode::Up
    } else {
        KeyCode::Down
    };
    (0..3)
        .map(|_| {
            Action::Send(ToDaemon::Input(InputEvent::Key(KeyEvent::press(
                code,
                Modifiers::empty(),
            ))))
        })
        .collect()
}

fn wants_mouse(scene: &Scene<'_>, pane: u64) -> bool {
    scene
        .views
        .get(&pane)
        .is_some_and(|view| view.modes.mouse != MouseMode::Off)
}

/// The event in the focused pane's own cells, cut to its edges.
fn to_focused(scene: &Scene<'_>, mouse: MouseEvent) -> Option<Action> {
    let placed = scene.placed();
    let target = placed
        .iter()
        .find(|p| Some(p.leaf.id) == scene.focus && p.content)?;
    let inner = target.inner;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let local = MouseEvent {
        column: mouse.column.clamp(inner.x, inner.right() - 1) - inner.x,
        row: mouse.row.clamp(inner.y, inner.bottom() - 1) - inner.y,
        ..mouse
    };
    input::to_input(Event::Mouse(local)).map(|event| Action::Send(ToDaemon::Input(event)))
}

/// The area the scene placed the tab's leaves in.
fn tab_area(scene: &Scene<'_>) -> Option<Cells> {
    let (cols, rows) = scene.tab_size()?;
    Some(Cells {
        x: scene.origin(),
        y: 1,
        width: cols,
        height: rows,
    })
}

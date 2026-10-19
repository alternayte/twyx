//! Mouse events on the composed screen: a press on another pane focuses it;
//! everything over the focused pane goes to it in its own cells.

use crossterm::event::{Event, MouseEvent, MouseEventKind};
use twyx_core::layout::Placed;
use twyx_protocol::client::ToDaemon;

use crate::compositor::Scene;
use crate::input;

/// The request one mouse event makes, if any.
pub fn route(scene: &Scene<'_>, mouse: MouseEvent) -> Option<ToDaemon> {
    let placed = scene.placed();
    let target = under(&placed, mouse.column, mouse.row)?;
    let focused = Some(target.leaf.id) == scene.focus;
    if !focused {
        return matches!(mouse.kind, MouseEventKind::Down(_)).then_some(ToDaemon::FocusLeaf {
            leaf: target.leaf.id,
        });
    }
    let inner = target.inner;
    let inside = mouse.column >= inner.x
        && mouse.row >= inner.y
        && mouse.column < inner.right()
        && mouse.row < inner.bottom();
    if !inside || !target.content {
        return None;
    }
    let local = MouseEvent {
        column: mouse.column - inner.x,
        row: mouse.row - inner.y,
        ..mouse
    };
    input::to_input(Event::Mouse(local)).map(ToDaemon::Input)
}

/// The leaf whose frame holds a cell: the top floating one, else the tiled
/// one. A collapsed stack child holds only its title row.
fn under(placed: &[Placed], col: u16, row: u16) -> Option<&Placed> {
    let holds = |placed: &&Placed| {
        let outer = placed.outer;
        let bottom = if placed.content {
            outer.bottom()
        } else {
            outer.y + 1
        };
        placed.frame && col >= outer.x && col < outer.right() && row >= outer.y && row < bottom
    };
    // A collapsed child's title row is also the rule below its neighbour,
    // so the title wins.
    placed
        .iter()
        .rev()
        .filter(|placed| placed.floating)
        .find(holds)
        .or_else(|| {
            placed
                .iter()
                .filter(|placed| !placed.floating && !placed.content)
                .find(holds)
        })
        .or_else(|| placed.iter().filter(|placed| !placed.floating).find(holds))
}

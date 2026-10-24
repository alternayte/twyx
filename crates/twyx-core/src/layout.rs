//! The layout of one tab: a tiled tree and a floating layer.
//!
//! The daemon owns every layout and both clients place it with the same
//! geometry, in cells. Every pane has a one-cell frame, and neighbouring
//! tiled panes sit `SPACE.panes` blank cells apart, so the leaseholder's tab
//! area decides each pane's size exactly. The children of a stack share
//! their rules: a stack is one box.

use serde::{Deserialize, Serialize};

use crate::pane::PaneId;
use crate::theme::SPACE;
use crate::tuning;

/// One leaf of a layout. Unique across the daemon, so a client's focus names
/// it without its tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeafId(pub u64);

/// What a leaf shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawLeaf", into = "RawLeaf")]
pub struct Leaf {
    pub id: LeafId,
    pub content: Content,
}

/// A terminal, or a view the client draws itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Content {
    Pane(PaneId),
    /// The review of one worktree.
    Review(u64),
}

impl Leaf {
    pub fn pane(id: LeafId, pane: PaneId) -> Self {
        Self {
            id,
            content: Content::Pane(pane),
        }
    }

    /// The leaf's pane, if it is a terminal.
    pub fn terminal(&self) -> Option<PaneId> {
        match self.content {
            Content::Pane(pane) => Some(pane),
            Content::Review(_) => None,
        }
    }
}

/// A leaf as stored and sent. Layouts written before leaves had kinds hold
/// only `pane`.
#[derive(Serialize, Deserialize)]
struct RawLeaf {
    id: LeafId,
    #[serde(default)]
    pane: Option<PaneId>,
    #[serde(default)]
    content: Option<Content>,
}

impl From<RawLeaf> for Leaf {
    fn from(raw: RawLeaf) -> Self {
        let content = raw
            .content
            .or(raw.pane.map(Content::Pane))
            .unwrap_or(Content::Pane(PaneId::from_raw(0)));
        Self {
            id: raw.id,
            content,
        }
    }
}

impl From<Leaf> for RawLeaf {
    fn from(leaf: Leaf) -> Self {
        Self {
            id: leaf.id,
            pane: leaf.terminal(),
            content: Some(leaf.content),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Children side by side, left to right.
    Row,
    /// Children one above the other, top to bottom.
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    fn axis(self) -> Axis {
        match self {
            Side::Left | Side::Right => Axis::Row,
            Side::Up | Side::Down => Axis::Column,
        }
    }

    fn forward(self) -> bool {
        matches!(self, Side::Right | Side::Down)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Leaf(Leaf),
    Split {
        axis: Axis,
        children: Vec<Node>,
        /// One share per child; they sum to 1.
        ratios: Vec<f32>,
    },
    /// Leaves one above the other; only `expanded` shows its content, the
    /// others show their title row.
    Stack {
        children: Vec<Leaf>,
        expanded: usize,
    },
}

/// A rectangle in the tab area, as fractions of its width and height.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Fractions {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Floating {
    pub leaf: Leaf,
    pub geometry: Fractions,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub root: Option<Node>,
    /// The floating layer, bottom first.
    pub floating: Vec<Floating>,
    pub zoomed: Option<LeafId>,
}

/// A rectangle of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cells {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Cells {
    pub fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// The rectangle inside a one-cell frame.
    pub fn inner(self) -> Self {
        Self {
            x: self.x.saturating_add(1),
            y: self.y.saturating_add(1),
            width: self.width.saturating_sub(2),
            height: self.height.saturating_sub(2),
        }
    }
}

/// Where one leaf lands in the tab area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub leaf: Leaf,
    /// The frame, including the rules it shares with its neighbours.
    pub outer: Cells,
    /// The size the pane is given. A collapsed or hidden pane keeps the size
    /// it would have, so showing it again does not resize it.
    pub inner: Cells,
    /// Whether the content shows. A collapsed stack child shows only its
    /// title row; a pane hidden behind a zoom shows nothing.
    pub content: bool,
    /// Whether the frame shows.
    pub frame: bool,
    pub floating: bool,
}

impl Layout {
    pub fn leaves(&self) -> Vec<Leaf> {
        let mut leaves = Vec::new();
        if let Some(root) = &self.root {
            collect(root, &mut leaves);
        }
        leaves.extend(self.floating.iter().map(|floating| floating.leaf));
        leaves
    }

    pub fn find(&self, pane: PaneId) -> Option<Leaf> {
        self.leaves()
            .into_iter()
            .find(|leaf| leaf.terminal() == Some(pane))
    }

    /// The panes of the terminal leaves.
    pub fn panes(&self) -> Vec<PaneId> {
        self.leaves().iter().filter_map(Leaf::terminal).collect()
    }

    pub fn contains(&self, id: LeafId) -> bool {
        self.leaves().iter().any(|leaf| leaf.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.floating.is_empty()
    }

    pub fn is_floating(&self, id: LeafId) -> bool {
        self.floating.iter().any(|floating| floating.leaf.id == id)
    }

    /// The panes whose content shows: not behind a zoom, not a collapsed
    /// stack child.
    pub fn shown(&self) -> Vec<PaneId> {
        let area = Cells {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        };
        self.place(area)
            .into_iter()
            .filter(|placed| placed.content)
            .filter_map(|placed| placed.leaf.terminal())
            .collect()
    }

    /// Places every leaf in `area`: the tiled ones in tree order, then the
    /// floating ones bottom first.
    pub fn place(&self, area: Cells) -> Vec<Placed> {
        let mut placed = Vec::new();
        if let Some(root) = &self.root {
            place_node(root, area, &mut placed);
        }
        if let Some(zoomed) = self.zoomed
            && placed.iter().any(|placed| placed.leaf.id == zoomed)
        {
            for leaf in &mut placed {
                if leaf.leaf.id == zoomed {
                    leaf.outer = area;
                    leaf.inner = area.inner();
                    leaf.content = true;
                    leaf.frame = true;
                } else {
                    leaf.content = false;
                    leaf.frame = false;
                }
            }
        }
        for floating in &self.floating {
            let outer = floating_cells(floating.geometry, area);
            placed.push(Placed {
                leaf: floating.leaf,
                outer,
                inner: outer.inner(),
                content: true,
                frame: true,
                floating: true,
            });
        }
        placed
    }

    /// Splits `target` and puts `leaf` after it on `axis`. A target in a
    /// stack splits the whole stack. With no tiled target, the new leaf goes
    /// after the whole tiled tree.
    pub fn split(&mut self, target: Option<LeafId>, leaf: Leaf, axis: Axis) {
        self.zoomed = None;
        let Some(root) = self.root.take() else {
            self.root = Some(Node::Leaf(leaf));
            return;
        };
        let path = target.and_then(|target| path_to(&root, target));
        let mut root = root;
        match path {
            Some(mut path) => {
                // A leaf in a stack splits the whole stack.
                if let Some(Node::Stack { .. }) = parent_of(&root, &path) {
                    path.pop();
                }
                split_at(&mut root, &path, leaf, axis);
            }
            None => split_at(&mut root, &[], leaf, axis),
        }
        self.root = Some(root);
    }

    /// Puts `leaf` in a stack with `target`, after it, and expands it.
    pub fn stack(&mut self, target: LeafId, leaf: Leaf) {
        self.zoomed = None;
        let Some(root) = &mut self.root else {
            self.root = Some(Node::Leaf(leaf));
            return;
        };
        let Some(path) = path_to(root, target) else {
            return;
        };
        let parent_path = &path[..path.len().saturating_sub(1)];
        if let Some(Node::Stack { children, expanded }) = node_at_mut(root, parent_path)
            && !path.is_empty()
        {
            let at = path[path.len() - 1] + 1;
            children.insert(at, leaf);
            *expanded = at;
            return;
        }
        if let Some(node) = node_at_mut(root, &path)
            && let Node::Leaf(existing) = *node
        {
            *node = Node::Stack {
                children: vec![existing, leaf],
                expanded: 1,
            };
        }
    }

    /// Adds `leaf` to the floating layer, on top. Each new floating pane is
    /// offset from the last, so none hides another exactly.
    pub fn float(&mut self, leaf: Leaf) {
        let offset = tuning::FLOATING_CASCADE * (self.floating.len() % 5) as f32;
        let size = tuning::FLOATING_SIZE;
        self.floating.push(Floating {
            leaf,
            geometry: Fractions {
                x: (1.0 - size) / 2.0 + offset,
                y: (1.0 - size) / 2.0 + offset,
                width: size,
                height: size,
            },
        });
    }

    /// Takes a leaf out of the layout. Returns the leaf that should take the
    /// focus from it: a neighbour in the same parent, else any leaf.
    pub fn remove(&mut self, id: LeafId) -> Option<LeafId> {
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
        if let Some(index) = self
            .floating
            .iter()
            .position(|floating| floating.leaf.id == id)
        {
            self.floating.remove(index);
            return self
                .floating
                .last()
                .map(|floating| floating.leaf.id)
                .or_else(|| self.root.as_ref().and_then(first_leaf));
        }
        let root = self.root.take()?;
        let (root, next) = remove_from(root, id);
        self.root = root;
        next.or_else(|| self.root.as_ref().and_then(first_leaf))
            .or_else(|| self.floating.last().map(|floating| floating.leaf.id))
    }

    /// The leaf next to `from` on `side`, by geometry in `area`. Under a
    /// zoom, the leaves keep the places they have without it.
    pub fn neighbour(&self, from: LeafId, side: Side, area: Cells) -> Option<LeafId> {
        let placed = if self.zoomed.is_some() {
            Layout {
                zoomed: None,
                ..self.clone()
            }
            .place(area)
        } else {
            self.place(area)
        };
        let origin = placed.iter().find(|placed| placed.leaf.id == from)?;
        if origin.floating {
            return None;
        }
        let from_rect = origin.outer;
        placed
            .iter()
            .filter(|candidate| !candidate.floating && candidate.frame && candidate.leaf.id != from)
            .filter_map(|candidate| {
                let (gap, overlap) = beyond(from_rect, candidate.outer, side)?;
                Some((gap, std::cmp::Reverse(overlap), candidate.leaf.id))
            })
            .min()
            .map(|(_, _, id)| id)
    }

    /// Moves the rule on `side` of a tiled leaf by `step` of its parent's
    /// size. The nearest split on that axis with a sibling on that side
    /// takes the change.
    pub fn resize(&mut self, id: LeafId, side: Side, step: f32) {
        if let Some((path, index, other)) = self.rule_owner(id, side)
            && let Some(root) = &mut self.root
        {
            shift(root, &path, index, other, step);
        }
    }

    /// Moves the rule on `side` of a tiled leaf by whole cells, as a drag
    /// with the mouse does.
    pub fn drag_rule(&mut self, id: LeafId, side: Side, cells: i32, area: Cells) {
        let Some((path, index, other)) = self.rule_owner(id, side) else {
            return;
        };
        let Some(root) = &mut self.root else {
            return;
        };
        let Some((split, Node::Split { children, .. })) =
            node_area(root, &path, area).zip(node_at(root, &path))
        else {
            return;
        };
        let length = match side.axis() {
            Axis::Row => split.width,
            Axis::Column => split.height,
        };
        let inner = length.saturating_sub(frames(children.len() as u16)).max(1);
        let sign = if side.forward() { 1.0 } else { -1.0 };
        shift(
            root,
            &path,
            index,
            other,
            sign * cells as f32 / f32::from(inner),
        );
    }

    /// The split that owns the rule on `side` of a leaf: its path, the
    /// leaf's child index in it, and the neighbour's.
    fn rule_owner(&self, id: LeafId, side: Side) -> Option<(Vec<usize>, usize, usize)> {
        let root = self.root.as_ref()?;
        let mut path = path_to(root, id)?;
        while let Some(index) = path.pop() {
            if let Some(Node::Split { axis, children, .. }) = node_at(root, &path)
                && *axis == side.axis()
            {
                let other = if side.forward() {
                    (index + 1 < children.len()).then_some(index + 1)
                } else {
                    index.checked_sub(1)
                };
                if let Some(other) = other {
                    return Some((path, index, other));
                }
            }
        }
        None
    }

    /// Swaps a tiled leaf with its neighbour on `side`.
    pub fn swap(&mut self, id: LeafId, side: Side, area: Cells) {
        let Some(other) = self.neighbour(id, side, area) else {
            return;
        };
        let (Some(a), Some(b)) = (self.leaf(id), self.leaf(other)) else {
            return;
        };
        if let Some(root) = &mut self.root {
            replace_leaf(root, id, Leaf { id: LeafId(0), ..a });
            replace_leaf(root, other, a);
            replace_leaf(root, LeafId(0), b);
        }
    }

    pub fn toggle_zoom(&mut self, id: LeafId) {
        self.zoomed = if self.zoomed == Some(id) || self.is_floating(id) {
            None
        } else {
            Some(id)
        };
    }

    /// Moves a leaf between the tiled tree and the floating layer.
    pub fn toggle_floating(&mut self, id: LeafId) {
        let Some(leaf) = self.leaf(id) else {
            return;
        };
        let floating = self.is_floating(id);
        self.remove(id);
        if floating {
            self.split(None, leaf, Axis::Row);
        } else {
            self.float(leaf);
        }
    }

    /// Shows a leaf: expands it in its stack, raises it in the floating
    /// layer, and takes the zoom when another tiled leaf has it. Called
    /// whenever a client focuses it.
    pub fn reveal(&mut self, id: LeafId) {
        if let Some(index) = self
            .floating
            .iter()
            .position(|floating| floating.leaf.id == id)
        {
            let floating = self.floating.remove(index);
            self.floating.push(floating);
            return;
        }
        if self.zoomed.is_some() && self.contains(id) {
            self.zoomed = Some(id);
        }
        if let Some(root) = &mut self.root
            && let Some(path) = path_to(root, id)
            && let Some((&index, parent)) = path.split_last()
            && let Some(Node::Stack { expanded, .. }) = node_at_mut(root, parent)
        {
            *expanded = index;
        }
    }

    pub fn move_floating(&mut self, id: LeafId, geometry: Fractions) {
        if let Some(floating) = self
            .floating
            .iter_mut()
            .find(|floating| floating.leaf.id == id)
        {
            floating.geometry = clamp_fractions(geometry);
        }
    }

    /// Moves a floating leaf one step toward `side`.
    pub fn nudge_floating(&mut self, id: LeafId, side: Side) {
        let Some(floating) = self.floating.iter().find(|floating| floating.leaf.id == id) else {
            return;
        };
        let mut geometry = floating.geometry;
        let step = tuning::FLOATING_STEP;
        match side {
            Side::Left => geometry.x -= step,
            Side::Right => geometry.x += step,
            Side::Up => geometry.y -= step,
            Side::Down => geometry.y += step,
        }
        self.move_floating(id, geometry);
    }

    pub fn leaf(&self, id: LeafId) -> Option<Leaf> {
        self.leaves().into_iter().find(|leaf| leaf.id == id)
    }

    /// Points a leaf at another pane, for "Run again".
    pub fn replace_pane(&mut self, id: LeafId, pane: PaneId) {
        if let Some(root) = &mut self.root {
            replace_leaf(root, id, Leaf::pane(id, pane));
        }
        for floating in &mut self.floating {
            if floating.leaf.id == id {
                floating.leaf.content = Content::Pane(pane);
            }
        }
    }
}

/// Moves `step` of a split's share from one child to its neighbour, keeping
/// both above the floor.
fn shift(root: &mut Node, path: &[usize], index: usize, other: usize, step: f32) {
    if let Some(Node::Split { ratios, .. }) = node_at_mut(root, path) {
        let floor = tuning::MIN_RATIO;
        let moved = step
            .min(ratios[other] - floor)
            .max(-(ratios[index] - floor));
        ratios[index] += moved;
        ratios[other] -= moved;
    }
}

/// The cells the node at `path` gets when the root is placed in `area`.
fn node_area(node: &Node, path: &[usize], area: Cells) -> Option<Cells> {
    let Some((&index, rest)) = path.split_first() else {
        return Some(area);
    };
    let Node::Split {
        axis,
        children,
        ratios,
    } = node
    else {
        return None;
    };
    let length = match axis {
        Axis::Row => area.width,
        Axis::Column => area.height,
    };
    let sizes = share(length, ratios);
    let start: u16 = sizes[..index].iter().map(|size| size + SPACE.panes).sum();
    let child = match axis {
        Axis::Row => Cells {
            x: area.x + start,
            width: sizes[index],
            ..area
        },
        Axis::Column => Cells {
            y: area.y + start,
            height: sizes[index],
            ..area
        },
    };
    node_area(children.get(index)?, rest, child)
}

fn collect(node: &Node, leaves: &mut Vec<Leaf>) {
    match node {
        Node::Leaf(leaf) => leaves.push(*leaf),
        Node::Split { children, .. } => children.iter().for_each(|child| collect(child, leaves)),
        Node::Stack { children, .. } => leaves.extend(children.iter().copied()),
    }
}

fn first_leaf(node: &Node) -> Option<LeafId> {
    let mut leaves = Vec::new();
    collect(node, &mut leaves);
    leaves.first().map(|leaf| leaf.id)
}

/// The child indices from the root to a leaf. Inside a stack, the last index
/// is the leaf's position in the stack.
fn path_to(node: &Node, id: LeafId) -> Option<Vec<usize>> {
    match node {
        Node::Leaf(leaf) => (leaf.id == id).then(Vec::new),
        Node::Split { children, .. } => children.iter().enumerate().find_map(|(index, child)| {
            let mut path = path_to(child, id)?;
            path.insert(0, index);
            Some(path)
        }),
        Node::Stack { children, .. } => children
            .iter()
            .position(|leaf| leaf.id == id)
            .map(|index| vec![index]),
    }
}

fn node_at<'a>(node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    match path.split_first() {
        None => Some(node),
        Some((&index, rest)) => match node {
            Node::Split { children, .. } => node_at(children.get(index)?, rest),
            _ => None,
        },
    }
}

fn node_at_mut<'a>(node: &'a mut Node, path: &[usize]) -> Option<&'a mut Node> {
    match path.split_first() {
        None => Some(node),
        Some((&index, rest)) => match node {
            Node::Split { children, .. } => node_at_mut(children.get_mut(index)?, rest),
            _ => None,
        },
    }
}

fn parent_of<'a>(node: &'a Node, path: &[usize]) -> Option<&'a Node> {
    let (_, parent) = path.split_last()?;
    node_at(node, parent)
}

/// Splits the node at `path` (a split's child, or the root) and puts `leaf`
/// after it. A parent split on the same axis takes the leaf as a sibling and
/// gives it half of the target's share.
fn split_at(root: &mut Node, path: &[usize], leaf: Leaf, axis: Axis) {
    if let Some((&index, parent_path)) = path.split_last()
        && let Some(Node::Split {
            axis: parent_axis,
            children,
            ratios,
        }) = node_at_mut(root, parent_path)
        && *parent_axis == axis
    {
        let half = ratios[index] / 2.0;
        ratios[index] = half;
        ratios.insert(index + 1, half);
        children.insert(index + 1, Node::Leaf(leaf));
        return;
    }
    if let Some(node) = node_at_mut(root, path) {
        let existing = std::mem::replace(node, Node::Leaf(leaf));
        *node = Node::Split {
            axis,
            children: vec![existing, Node::Leaf(leaf)],
            ratios: vec![0.5, 0.5],
        };
    }
}

/// Removes a leaf and folds any split or stack left with one child.
fn remove_from(node: Node, id: LeafId) -> (Option<Node>, Option<LeafId>) {
    match node {
        Node::Leaf(leaf) if leaf.id == id => (None, None),
        Node::Leaf(_) => (Some(node), None),
        Node::Stack {
            mut children,
            expanded,
        } => {
            let Some(index) = children.iter().position(|leaf| leaf.id == id) else {
                return (Some(Node::Stack { children, expanded }), None);
            };
            children.remove(index);
            let next = children
                .get(index.min(children.len().saturating_sub(1)))
                .map(|leaf| leaf.id);
            let node = match children.len() {
                0 => None,
                1 => Some(Node::Leaf(children[0])),
                _ => Some(Node::Stack {
                    expanded: if expanded == index {
                        index.min(children.len() - 1)
                    } else if expanded > index {
                        expanded - 1
                    } else {
                        expanded
                    },
                    children,
                }),
            };
            (node, next)
        }
        Node::Split {
            axis,
            children,
            ratios,
        } => {
            let mut kept = Vec::new();
            let mut kept_ratios = Vec::new();
            let mut next = None;
            let mut removed_at = None;
            for (index, (child, ratio)) in children.into_iter().zip(ratios).enumerate() {
                let (child, found) = remove_from(child, id);
                if found.is_some() {
                    next = found;
                }
                match child {
                    Some(child) => {
                        kept.push(child);
                        kept_ratios.push(ratio);
                    }
                    None => removed_at = Some(index),
                }
            }
            if let Some(index) = removed_at {
                let neighbour = index.min(kept.len().saturating_sub(1));
                if next.is_none() {
                    next = kept.get(neighbour).and_then(first_leaf);
                }
            }
            let total: f32 = kept_ratios.iter().sum();
            if total > 0.0 {
                kept_ratios.iter_mut().for_each(|ratio| *ratio /= total);
            }
            let node = match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => Some(Node::Split {
                    axis,
                    children: kept,
                    ratios: kept_ratios,
                }),
            };
            (node, next)
        }
    }
}

fn replace_leaf(node: &mut Node, id: LeafId, with: Leaf) {
    match node {
        Node::Leaf(leaf) if leaf.id == id => *leaf = with,
        Node::Leaf(_) => {}
        Node::Split { children, .. } => children
            .iter_mut()
            .for_each(|child| replace_leaf(child, id, with)),
        Node::Stack { children, .. } => children
            .iter_mut()
            .filter(|leaf| leaf.id == id)
            .for_each(|leaf| *leaf = with),
    }
}

fn place_node(node: &Node, area: Cells, placed: &mut Vec<Placed>) {
    match node {
        Node::Leaf(leaf) => placed.push(Placed {
            leaf: *leaf,
            outer: area,
            inner: area.inner(),
            content: true,
            frame: true,
            floating: false,
        }),
        Node::Split {
            axis,
            children,
            ratios,
        } => {
            let length = match axis {
                Axis::Row => area.width,
                Axis::Column => area.height,
            };
            let sizes = share(length, ratios);
            let mut start = match axis {
                Axis::Row => area.x,
                Axis::Column => area.y,
            };
            for (child, outer) in children.iter().zip(sizes) {
                let rect = match axis {
                    Axis::Row => Cells {
                        x: start,
                        width: outer,
                        ..area
                    },
                    Axis::Column => Cells {
                        y: start,
                        height: outer,
                        ..area
                    },
                };
                place_node(child, rect, placed);
                start = start.saturating_add(outer + SPACE.panes);
            }
        }
        Node::Stack { children, expanded } => {
            let count = children.len() as u16;
            let open = area.height.saturating_sub(count.saturating_sub(1)).max(2);
            let open_inner = Cells {
                height: open,
                ..area
            }
            .inner();
            let mut top = area.y;
            for (index, leaf) in children.iter().enumerate() {
                let is_open = index == *expanded;
                let height = if is_open { open } else { 2 };
                let outer = Cells {
                    y: top,
                    height,
                    ..area
                };
                placed.push(Placed {
                    leaf: *leaf,
                    outer,
                    inner: Cells {
                        x: outer.x + 1,
                        y: outer.y + 1,
                        ..open_inner
                    },
                    content: is_open,
                    frame: true,
                    floating: false,
                });
                top = top.saturating_add(height - 1);
            }
        }
    }
}

/// The cells that `count` frames side by side take besides their insides:
/// two rules each and the blanks between them.
fn frames(count: u16) -> u16 {
    2 * count + count.saturating_sub(1) * SPACE.panes
}

/// Outer lengths for children sharing `length` cells, each in its own frame
/// with blanks between: the inner lengths follow the ratios, and leftover
/// cells go to the largest remainders.
fn share(length: u16, ratios: &[f32]) -> Vec<u16> {
    let count = ratios.len() as u16;
    let inner_total = length.saturating_sub(frames(count));
    let total: f32 = ratios.iter().sum::<f32>().max(f32::EPSILON);
    let exact: Vec<f32> = ratios
        .iter()
        .map(|ratio| f32::from(inner_total) * ratio / total)
        .collect();
    let mut inner: Vec<u16> = exact.iter().map(|value| value.floor() as u16).collect();
    let mut left = inner_total.saturating_sub(inner.iter().sum());
    let mut order: Vec<usize> = (0..inner.len()).collect();
    order.sort_by(|&a, &b| {
        (exact[b] - exact[b].floor())
            .total_cmp(&(exact[a] - exact[a].floor()))
            .then(a.cmp(&b))
    });
    for index in order {
        if left == 0 {
            break;
        }
        inner[index] += 1;
        left -= 1;
    }
    inner.iter().map(|inner| inner + 2).collect()
}

fn floating_cells(geometry: Fractions, area: Cells) -> Cells {
    let geometry = clamp_fractions(geometry);
    let scale = |fraction: f32, length: u16| (fraction * f32::from(length)).round() as u16;
    let width = scale(geometry.width, area.width).clamp(area.width.min(3), area.width);
    let height = scale(geometry.height, area.height).clamp(area.height.min(3), area.height);
    Cells {
        x: area.x + scale(geometry.x, area.width).min(area.width - width),
        y: area.y + scale(geometry.y, area.height).min(area.height - height),
        width,
        height,
    }
}

fn clamp_fractions(geometry: Fractions) -> Fractions {
    let width = geometry.width.clamp(0.05, 1.0);
    let height = geometry.height.clamp(0.05, 1.0);
    Fractions {
        x: geometry.x.clamp(0.0, 1.0 - width),
        y: geometry.y.clamp(0.0, 1.0 - height),
        width,
        height,
    }
}

/// How far `to` lies beyond `from` on `side`, and how much they overlap
/// across it. Frames with only the blanks between them are adjacent.
fn beyond(from: Cells, to: Cells, side: Side) -> Option<(u16, u16)> {
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a1.min(b1).saturating_sub(a0.max(b0));
    match side {
        Side::Right => (to.x + 1 >= from.right()).then(|| {
            (
                (to.x + 1).saturating_sub(from.right()),
                overlap(from.y, from.bottom(), to.y, to.bottom()),
            )
        }),
        Side::Left => (to.right() <= from.x + 1).then(|| {
            (
                (from.x + 1).saturating_sub(to.right()),
                overlap(from.y, from.bottom(), to.y, to.bottom()),
            )
        }),
        Side::Down => (to.y + 1 >= from.bottom()).then(|| {
            (
                (to.y + 1).saturating_sub(from.bottom()),
                overlap(from.x, from.right(), to.x, to.right()),
            )
        }),
        Side::Up => (to.bottom() <= from.y + 1).then(|| {
            (
                (from.y + 1).saturating_sub(to.bottom()),
                overlap(from.x, from.right(), to.x, to.right()),
            )
        }),
    }
    .filter(|(_, overlap)| *overlap > 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u64) -> Leaf {
        Leaf::pane(LeafId(n), PaneId::from_raw(n))
    }

    #[test]
    fn a_layout_saved_before_leaf_kinds_still_loads_and_a_review_leaf_round_trips() {
        let old: Leaf = serde_json::from_str(r#"{"id":3,"pane":7}"#).unwrap();
        assert_eq!(old, Leaf::pane(LeafId(3), PaneId::from_raw(7)));
        let review = Leaf {
            id: LeafId(4),
            content: Content::Review(9),
        };
        let text = serde_json::to_string(&review).unwrap();
        assert_eq!(serde_json::from_str::<Leaf>(&text).unwrap(), review);
    }

    fn area(width: u16, height: u16) -> Cells {
        Cells {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn outer(layout: &Layout, area: Cells, id: u64) -> (u16, u16, u16, u16) {
        let placed = layout
            .place(area)
            .into_iter()
            .find(|placed| placed.leaf.id == LeafId(id))
            .expect("placed");
        let rect = placed.outer;
        (rect.x, rect.y, rect.width, rect.height)
    }

    /// 1 on the left; 2 above 3 on the right.
    fn three() -> Layout {
        let mut layout = Layout::default();
        layout.split(None, leaf(1), Axis::Row);
        layout.split(Some(LeafId(1)), leaf(2), Axis::Row);
        layout.split(Some(LeafId(2)), leaf(3), Axis::Column);
        layout
    }

    #[test]
    fn each_pane_is_its_own_box_and_the_inner_cells_add_up() {
        let layout = Layout {
            root: Some(Node::Split {
                axis: Axis::Row,
                children: vec![
                    Node::Leaf(leaf(1)),
                    Node::Leaf(leaf(2)),
                    Node::Leaf(leaf(3)),
                ],
                ratios: vec![1.0 / 3.0; 3],
            }),
            ..Layout::default()
        };
        let placed = layout.place(area(21, 5));
        let rects: Vec<_> = placed.iter().map(|p| (p.outer.x, p.outer.width)).collect();
        // 21 cells: six rules, two blanks and 13 inner cells; the spare cell
        // goes to the first pane.
        assert_eq!(rects, [(0, 7), (8, 6), (15, 6)]);
        assert_eq!(placed.iter().map(|p| p.inner.width).sum::<u16>(), 21 - 8);
        assert_eq!(placed[2].outer.right(), 21);
    }

    #[test]
    fn splitting_on_the_same_axis_adds_a_sibling_with_half_the_share() {
        let mut layout = Layout::default();
        layout.split(None, leaf(1), Axis::Row);
        layout.split(Some(LeafId(1)), leaf(2), Axis::Row);
        layout.split(Some(LeafId(2)), leaf(3), Axis::Row);
        let Some(Node::Split {
            children, ratios, ..
        }) = &layout.root
        else {
            panic!("a split")
        };
        assert_eq!(children.len(), 3);
        assert_eq!(ratios, &[0.5, 0.25, 0.25]);
    }

    #[test]
    fn focus_moves_to_the_frame_beyond_the_blanks() {
        let layout = three();
        let tab = area(40, 20);
        assert_eq!(
            layout.neighbour(LeafId(1), Side::Right, tab),
            Some(LeafId(2))
        );
        assert_eq!(
            layout.neighbour(LeafId(3), Side::Left, tab),
            Some(LeafId(1))
        );
        assert_eq!(
            layout.neighbour(LeafId(2), Side::Down, tab),
            Some(LeafId(3))
        );
        assert_eq!(layout.neighbour(LeafId(3), Side::Up, tab), Some(LeafId(2)));
        assert_eq!(layout.neighbour(LeafId(1), Side::Left, tab), None);
    }

    #[test]
    fn removing_a_leaf_folds_its_split_and_hands_focus_to_a_sibling() {
        let mut layout = three();
        assert_eq!(layout.remove(LeafId(2)), Some(LeafId(3)));
        assert_eq!(
            layout.root,
            Some(Node::Split {
                axis: Axis::Row,
                children: vec![Node::Leaf(leaf(1)), Node::Leaf(leaf(3))],
                ratios: vec![0.5, 0.5],
            })
        );
        assert_eq!(layout.remove(LeafId(1)), Some(LeafId(3)));
        assert_eq!(layout.root, Some(Node::Leaf(leaf(3))));
        assert_eq!(layout.remove(LeafId(3)), None);
        assert!(layout.is_empty());
    }

    #[test]
    fn resize_moves_the_nearest_rule_on_that_side_and_stops_at_the_floor() {
        let mut layout = three();
        // Leaf 3 has no sibling to its right in its column split, so the
        // row split above it takes the change.
        layout.resize(LeafId(3), Side::Left, 0.1);
        let Some(Node::Split { ratios, .. }) = &layout.root else {
            panic!("a split")
        };
        assert!((ratios[0] - 0.4).abs() < 1e-6, "{ratios:?}");
        layout.resize(LeafId(1), Side::Right, 1.0);
        let Some(Node::Split { ratios, .. }) = &layout.root else {
            panic!("a split")
        };
        assert!((ratios[1] - tuning::MIN_RATIO).abs() < 1e-6, "{ratios:?}");
        assert!((ratios[0] + ratios[1] - 1.0).abs() < 1e-6, "{ratios:?}");
    }

    #[test]
    fn dragging_a_rule_moves_it_by_whole_cells() {
        let mut layout = three();
        let tab = area(43, 21);
        // 43 columns: four rules, one blank and 38 inner, 19 each.
        assert_eq!(outer(&layout, tab, 2).0, 22);
        layout.drag_rule(LeafId(1), Side::Right, 5, tab);
        assert_eq!(outer(&layout, tab, 2).0, 27);
        // The rule below leaf 2 lives in the right column's split; leaf 3
        // starts at row 11 and goes up three.
        assert_eq!(outer(&layout, tab, 3).1, 11);
        layout.drag_rule(LeafId(2), Side::Down, -3, tab);
        let (_, y, _, height) = outer(&layout, tab, 3);
        assert_eq!((y, height), (8, 13));
    }

    #[test]
    fn swap_exchanges_two_leaves_and_keeps_the_shape() {
        let mut layout = three();
        layout.swap(LeafId(1), Side::Right, area(40, 20));
        assert_eq!(outer(&layout, area(40, 20), 2), (0, 0, 20, 20));
        assert_eq!(outer(&layout, area(40, 20), 1), (21, 0, 19, 10));
    }

    #[test]
    fn a_stack_shows_one_child_and_the_title_rows_of_the_others() {
        let mut layout = Layout::default();
        layout.split(None, leaf(1), Axis::Row);
        layout.stack(LeafId(1), leaf(2));
        layout.stack(LeafId(2), leaf(3));
        layout.reveal(LeafId(2));
        let placed = layout.place(area(20, 10));
        let shape: Vec<_> = placed
            .iter()
            .map(|p| (p.outer.y, p.outer.height, p.content, p.inner.height))
            .collect();
        // Every child gets the open child's size, so expanding one resizes none.
        assert_eq!(shape, [(0, 2, false, 6), (1, 8, true, 6), (8, 2, false, 6)]);
        assert_eq!(layout.remove(LeafId(2)), Some(LeafId(3)));
        let Some(Node::Stack { children, expanded }) = &layout.root else {
            panic!("a stack")
        };
        assert_eq!((children.len(), *expanded), (2, 1));
    }

    #[test]
    fn zoom_gives_one_leaf_the_whole_area_and_hides_the_rest() {
        let mut layout = three();
        layout.toggle_zoom(LeafId(3));
        let placed = layout.place(area(40, 20));
        let shown: Vec<_> = placed
            .iter()
            .filter(|p| p.frame)
            .map(|p| p.leaf.id)
            .collect();
        assert_eq!(shown, [LeafId(3)]);
        assert_eq!(outer(&layout, area(40, 20), 3), (0, 0, 40, 20));
        layout.split(Some(LeafId(3)), leaf(4), Axis::Row);
        assert_eq!(layout.zoomed, None);
    }

    #[test]
    fn focus_moves_under_a_zoom_and_the_zoom_follows_it() {
        let mut layout = three();
        let plain: Vec<_> = [Side::Left, Side::Right, Side::Up, Side::Down]
            .map(|side| layout.neighbour(LeafId(3), side, area(40, 20)))
            .into();
        layout.toggle_zoom(LeafId(3));
        let zoomed: Vec<_> = [Side::Left, Side::Right, Side::Up, Side::Down]
            .map(|side| layout.neighbour(LeafId(3), side, area(40, 20)))
            .into();
        assert_eq!(zoomed, plain, "the hidden leaves keep their places");
        let next = zoomed.into_iter().flatten().next().expect("a neighbour");
        layout.reveal(next);
        assert_eq!(layout.zoomed, Some(next));
    }

    #[test]
    fn a_leaf_floats_and_returns_to_the_tiled_tree() {
        let mut layout = three();
        layout.toggle_floating(LeafId(2));
        assert!(layout.is_floating(LeafId(2)));
        let placed = layout.place(area(100, 50));
        let floating = placed.last().expect("placed");
        assert_eq!(
            (floating.leaf.id, floating.outer),
            (
                LeafId(2),
                Cells {
                    x: 20,
                    y: 10,
                    width: 60,
                    height: 30
                }
            )
        );
        layout.toggle_floating(LeafId(2));
        assert!(!layout.is_floating(LeafId(2)));
        assert_eq!(layout.leaves().len(), 3);
    }
}

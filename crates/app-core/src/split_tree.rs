//! One tab's panes as a binary split tree, plus the geometry used to render it and move focus.
//!
//! Geometry is in fractions of the tab's area (0..1 on both axes), so it is independent of pixels.

use serde::{Deserialize, Serialize};

/// Identifies a leaf or split node; unique within a workspace.
pub type NodeId = u64;

/// Smallest share of a split either side may have.
pub const MIN_RATIO: f32 = 0.1;
/// Largest share of a split the first side may have.
pub const MAX_RATIO: f32 = 0.9;

/// How a split arranges its two children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Side by side (the divider is vertical).
    Horizontal,
    /// Stacked (the divider is horizontal).
    Vertical,
}

/// A direction to move focus in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Towards smaller x.
    Left,
    /// Towards larger x.
    Right,
    /// Towards smaller y.
    Up,
    /// Towards larger y.
    Down,
}

/// A rectangle in fractions of the tab's area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// The whole tab.
    pub const UNIT: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn split(self, axis: Axis, ratio: f32) -> (Rect, Rect) {
        match axis {
            Axis::Horizontal => {
                let first = self.w * ratio;
                (Rect { w: first, ..self }, Rect { x: self.x + first, w: self.w - first, ..self })
            }
            Axis::Vertical => {
                let first = self.h * ratio;
                (Rect { h: first, ..self }, Rect { y: self.y + first, h: self.h - first, ..self })
            }
        }
    }
}

/// A draggable boundary between the two children of a split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Divider {
    /// The split it belongs to.
    pub split: NodeId,
    /// The split's axis.
    pub axis: Axis,
    /// The area the split divides.
    pub area: Rect,
    /// Share of `area` the first child gets.
    pub ratio: f32,
}

/// A node of the split tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    /// A pane.
    Leaf(NodeId),
    /// Two children sharing the node's area.
    Split {
        /// Split id.
        id: NodeId,
        /// Arrangement.
        axis: Axis,
        /// Share of the area the first child gets.
        ratio: f32,
        /// Left or top child.
        first: Box<Node>,
        /// Right or bottom child.
        second: Box<Node>,
    },
}

impl Node {
    /// Leaf ids, left/top first.
    pub fn leaves(&self) -> Vec<NodeId> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { first, second, .. } => {
                let mut leaves = first.leaves();
                leaves.extend(second.leaves());
                leaves
            }
        }
    }

    /// Every node id (splits before their children).
    pub fn ids(&self) -> Vec<NodeId> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { id, first, second, .. } => {
                let mut ids = vec![*id];
                ids.extend(first.ids());
                ids.extend(second.ids());
                ids
            }
        }
    }

    /// Whether `leaf` is one of this tree's leaves.
    pub fn contains(&self, leaf: NodeId) -> bool {
        match self {
            Node::Leaf(id) => *id == leaf,
            Node::Split { first, second, .. } => first.contains(leaf) || second.contains(leaf),
        }
    }

    /// Replace leaf `target` with an even split: `target` first, `new_leaf` second. False if `target` is absent.
    pub fn split(&mut self, target: NodeId, axis: Axis, split_id: NodeId, new_leaf: NodeId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    id: split_id,
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(target)),
                    second: Box::new(Node::Leaf(new_leaf)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split(target, axis, split_id, new_leaf) || second.split(target, axis, split_id, new_leaf)
            }
        }
    }

    /// The tree without `leaf` (its sibling takes the parent split's place); `None` when `leaf` was the whole tree.
    pub fn without(self, leaf: NodeId) -> Option<Node> {
        match self {
            Node::Leaf(id) if id == leaf => None,
            Node::Leaf(_) => Some(self),
            Node::Split { id, axis, ratio, first, second } => match (first.without(leaf), second.without(leaf)) {
                (Some(first), Some(second)) => {
                    Some(Node::Split { id, axis, ratio, first: Box::new(first), second: Box::new(second) })
                }
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// First leaf on the other side of `leaf`'s parent split (where focus goes when `leaf` closes).
    pub fn sibling_leaf(&self, leaf: NodeId) -> Option<NodeId> {
        let Node::Split { first, second, .. } = self else { return None };
        if **first == Node::Leaf(leaf) {
            return second.leaves().first().copied();
        }
        if **second == Node::Leaf(leaf) {
            return first.leaves().first().copied();
        }
        first.sibling_leaf(leaf).or_else(|| second.sibling_leaf(leaf))
    }

    /// Trade the places of leaves `a` and `b`; every ratio stays. False (nothing changed) if either is absent.
    pub fn swap_leaves(&mut self, a: NodeId, b: NodeId) -> bool {
        if !self.contains(a) || !self.contains(b) {
            return false;
        }
        self.relabel(a, b);
        true
    }

    fn relabel(&mut self, a: NodeId, b: NodeId) {
        match self {
            Node::Leaf(id) if *id == a => *id = b,
            Node::Leaf(id) if *id == b => *id = a,
            Node::Leaf(_) => {}
            Node::Split { first, second, .. } => {
                first.relabel(a, b);
                second.relabel(a, b);
            }
        }
    }

    /// Put leaf `new` where leaf `old` is. False (nothing changed) if `old` is absent.
    pub fn replace_leaf(&mut self, old: NodeId, new: NodeId) -> bool {
        match self {
            Node::Leaf(id) if *id == old => {
                *id = new;
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => first.replace_leaf(old, new) || second.replace_leaf(old, new),
        }
    }

    /// Replace leaf `target` with an even split holding `target` and `leaf`: `leaf` first for `Left`/`Up`, second for
    /// `Right`/`Down`. False if `target` is absent.
    pub fn insert_beside(&mut self, target: NodeId, side: Direction, split_id: NodeId, leaf: NodeId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let (axis, leaf_first) = match side {
                    Direction::Left => (Axis::Horizontal, true),
                    Direction::Right => (Axis::Horizontal, false),
                    Direction::Up => (Axis::Vertical, true),
                    Direction::Down => (Axis::Vertical, false),
                };
                let (first, second) = if leaf_first { (leaf, target) } else { (target, leaf) };
                *self = Node::Split {
                    id: split_id,
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(first)),
                    second: Box::new(Node::Leaf(second)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.insert_beside(target, side, split_id, leaf) || second.insert_beside(target, side, split_id, leaf)
            }
        }
    }

    /// Set a split's ratio (clamped to `MIN_RATIO..=MAX_RATIO`). False if `split` is not a split here.
    pub fn set_ratio(&mut self, split: NodeId, ratio: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { id, ratio: current, first, second, .. } => {
                if *id == split {
                    *current = ratio.clamp(MIN_RATIO, MAX_RATIO);
                    true
                } else {
                    first.set_ratio(split, ratio) || second.set_ratio(split, ratio)
                }
            }
        }
    }

    /// Each leaf's rectangle within `area`, left/top first.
    pub fn layout(&self, area: Rect) -> Vec<(NodeId, Rect)> {
        match self {
            Node::Leaf(id) => vec![(*id, area)],
            Node::Split { axis, ratio, first, second, .. } => {
                let (a, b) = area.split(*axis, *ratio);
                let mut rects = first.layout(a);
                rects.extend(second.layout(b));
                rects
            }
        }
    }

    /// Every split's divider within `area`, outermost first.
    pub fn dividers(&self, area: Rect) -> Vec<Divider> {
        match self {
            Node::Leaf(_) => Vec::new(),
            Node::Split { id, axis, ratio, first, second } => {
                let (a, b) = area.split(*axis, *ratio);
                let mut dividers = vec![Divider { split: *id, axis: *axis, area, ratio: *ratio }];
                dividers.extend(first.dividers(a));
                dividers.extend(second.dividers(b));
                dividers
            }
        }
    }

    /// The leaf touching `from`'s edge in `direction` with the most shared edge (ties: upper, then left).
    pub fn neighbor(&self, from: NodeId, direction: Direction) -> Option<NodeId> {
        const EPS: f32 = 1e-4;
        let rects = self.layout(Rect::UNIT);
        let (_, origin) = *rects.iter().find(|(id, _)| *id == from)?;
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|&(id, r)| {
                let (touches, overlap) = match direction {
                    Direction::Left => ((r.x + r.w - origin.x).abs() < EPS, overlap(r.y, r.h, origin.y, origin.h)),
                    Direction::Right => {
                        ((origin.x + origin.w - r.x).abs() < EPS, overlap(r.y, r.h, origin.y, origin.h))
                    }
                    Direction::Up => ((r.y + r.h - origin.y).abs() < EPS, overlap(r.x, r.w, origin.x, origin.w)),
                    Direction::Down => ((origin.y + origin.h - r.y).abs() < EPS, overlap(r.x, r.w, origin.x, origin.w)),
                };
                (touches && overlap > EPS).then_some((id, overlap, r))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.y.total_cmp(&a.2.y)).then(b.2.x.total_cmp(&a.2.x)))
            .map(|(id, ..)| id)
    }
}

/// Where on a pane a dragged pane is dropped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropZone {
    /// The middle: the two panes trade places.
    Center,
    /// Near an edge: the dragged pane splits the target on that side.
    Edge(Direction),
}

/// The zone of the pane filling `area` that the point (`x`, `y`) falls in: the middle half on both axes is
/// `Center`, anywhere else the nearest edge.
pub fn drop_zone(area: Rect, x: f32, y: f32) -> DropZone {
    let (rx, ry) = ((x - area.x) / area.w, (y - area.y) / area.h);
    if (0.25..=0.75).contains(&rx) && (0.25..=0.75).contains(&ry) {
        return DropZone::Center;
    }
    let edges = [(rx, Direction::Left), (1.0 - rx, Direction::Right), (ry, Direction::Up), (1.0 - ry, Direction::Down)];
    let (_, nearest) =
        edges.into_iter().reduce(|best, edge| if edge.0 < best.0 { edge } else { best }).expect("four edges");
    DropZone::Edge(nearest)
}

/// What a drop in `zone` of the pane filling `area` covers: all of it for `Center`, else the half on that side.
pub fn zone_rect(area: Rect, zone: DropZone) -> Rect {
    match zone {
        DropZone::Center => area,
        DropZone::Edge(Direction::Left) => Rect { w: area.w / 2.0, ..area },
        DropZone::Edge(Direction::Right) => Rect { x: area.x + area.w / 2.0, w: area.w / 2.0, ..area },
        DropZone::Edge(Direction::Up) => Rect { h: area.h / 2.0, ..area },
        DropZone::Edge(Direction::Down) => Rect { y: area.y + area.h / 2.0, h: area.h / 2.0, ..area },
    }
}

fn overlap(a: f32, a_len: f32, b: f32, b_len: f32) -> f32 {
    ((a + a_len).min(b + b_len) - a.max(b)).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    /// `1 | (2 over 3)`: leaf 1 on the left half, 2 above 3 on the right.
    fn three() -> Node {
        let mut tree = Node::Leaf(1);
        assert!(tree.split(1, Axis::Horizontal, 10, 2));
        assert!(tree.split(2, Axis::Vertical, 11, 3));
        tree
    }

    #[test]
    fn splitting_a_leaf_puts_the_new_leaf_second() {
        let mut tree = Node::Leaf(1);
        assert!(tree.split(1, Axis::Horizontal, 10, 2));
        assert_eq!(tree.leaves(), vec![1, 2]);
        assert_eq!(tree.layout(Rect::UNIT), vec![(1, rect(0.0, 0.0, 0.5, 1.0)), (2, rect(0.5, 0.0, 0.5, 1.0))]);
        assert!(!tree.split(99, Axis::Vertical, 12, 3), "unknown target");
    }

    #[test]
    fn nested_layout_divides_the_parent_area() {
        assert_eq!(
            three().layout(Rect::UNIT),
            vec![(1, rect(0.0, 0.0, 0.5, 1.0)), (2, rect(0.5, 0.0, 0.5, 0.5)), (3, rect(0.5, 0.5, 0.5, 0.5))]
        );
        assert_eq!(three().ids(), vec![10, 1, 11, 2, 3]);
    }

    #[test]
    fn removing_a_leaf_promotes_its_sibling() {
        let tree = three().without(2).unwrap();
        assert_eq!(tree.leaves(), vec![1, 3]);
        assert!(matches!(tree, Node::Split { id: 10, .. }), "outer split survives");
        let tree = three().without(1).unwrap();
        assert!(matches!(tree, Node::Split { id: 11, .. }), "right subtree takes the root");
        assert_eq!(Node::Leaf(5).without(5), None);
        assert_eq!(Node::Leaf(5).without(6), Some(Node::Leaf(5)));
    }

    #[test]
    fn sibling_leaf_is_the_first_leaf_of_the_other_side() {
        let tree = three();
        assert_eq!(tree.sibling_leaf(1), Some(2));
        assert_eq!(tree.sibling_leaf(3), Some(2));
        assert_eq!(tree.sibling_leaf(2), Some(3));
        assert_eq!(Node::Leaf(1).sibling_leaf(1), None);
    }

    #[test]
    fn ratio_is_clamped_and_moves_the_divider() {
        let mut tree = three();
        assert!(tree.set_ratio(10, 0.02));
        assert_eq!(
            tree.dividers(Rect::UNIT)[0],
            Divider { split: 10, axis: Axis::Horizontal, area: Rect::UNIT, ratio: MIN_RATIO }
        );
        assert!(tree.set_ratio(11, 0.75));
        assert_eq!(tree.layout(Rect::UNIT)[2].1.h, 0.25);
        assert!(!tree.set_ratio(1, 0.5), "leaves have no ratio");
    }

    #[test]
    fn neighbors_follow_geometry() {
        let tree = three();
        assert_eq!(tree.neighbor(1, Direction::Right), Some(2), "equal overlap prefers the upper pane");
        assert_eq!(tree.neighbor(3, Direction::Left), Some(1));
        assert_eq!(tree.neighbor(2, Direction::Down), Some(3));
        assert_eq!(tree.neighbor(3, Direction::Up), Some(2));
        assert_eq!(tree.neighbor(2, Direction::Up), None);
        assert_eq!(tree.neighbor(1, Direction::Left), None);
        assert_eq!(tree.neighbor(42, Direction::Left), None);
    }

    #[test]
    fn tree_round_trips_through_json() {
        let tree = three();
        let json = serde_json::to_string(&tree).unwrap();
        assert_eq!(serde_json::from_str::<Node>(&json).unwrap(), tree);
    }

    #[test]
    fn swapping_two_leaves_keeps_the_ratios() {
        let mut tree = three();
        assert!(tree.set_ratio(10, 0.3));
        assert!(tree.set_ratio(11, 0.7));
        assert!(tree.swap_leaves(1, 3));
        assert_eq!(tree.leaves(), vec![3, 2, 1]);
        assert_eq!(tree.dividers(Rect::UNIT)[0].ratio, 0.3);
        assert_eq!(tree.dividers(Rect::UNIT)[1].ratio, 0.7);
        assert!(!tree.swap_leaves(1, 99), "an absent leaf changes nothing");
        assert_eq!(tree.leaves(), vec![3, 2, 1]);
    }

    #[test]
    fn insert_beside_puts_the_leaf_on_the_dropped_side() {
        for (side, axis, order) in [
            (Direction::Left, Axis::Horizontal, vec![9, 1]),
            (Direction::Right, Axis::Horizontal, vec![1, 9]),
            (Direction::Up, Axis::Vertical, vec![9, 1]),
            (Direction::Down, Axis::Vertical, vec![1, 9]),
        ] {
            let mut tree = Node::Leaf(1);
            assert!(tree.insert_beside(1, side, 20, 9));
            assert_eq!(tree.leaves(), order, "{side:?}");
            assert!(
                matches!(tree, Node::Split { id: 20, axis: a, ratio, .. } if a == axis && ratio == 0.5),
                "{side:?}"
            );
        }
    }

    #[test]
    fn replacing_a_leaf_keeps_its_place_and_the_ratios() {
        let mut tree = three();
        assert!(tree.set_ratio(11, 0.7));
        assert!(tree.replace_leaf(2, 9));
        assert_eq!(tree.leaves(), vec![1, 9, 3]);
        assert_eq!(tree.dividers(Rect::UNIT)[1].ratio, 0.7);
        assert!(!tree.replace_leaf(2, 8), "the old leaf is gone");
        assert!(!tree.replace_leaf(99, 8));
    }

    #[test]
    fn insert_beside_an_absent_target_changes_nothing() {
        let mut tree = three();
        assert!(!tree.insert_beside(99, Direction::Left, 20, 9));
        assert_eq!(tree, three());
    }

    #[test]
    fn drop_zone_middle_band_and_edges() {
        let area = rect(0.5, 0.0, 0.5, 1.0);
        let at = |fx: f32, fy: f32| drop_zone(area, area.x + area.w * fx, area.y + area.h * fy);
        assert_eq!(at(0.5, 0.5), DropZone::Center);
        assert_eq!(at(0.26, 0.5), DropZone::Center);
        assert_eq!(at(0.74, 0.5), DropZone::Center);
        assert_eq!(at(0.1, 0.5), DropZone::Edge(Direction::Left));
        assert_eq!(at(0.9, 0.5), DropZone::Edge(Direction::Right));
        assert_eq!(at(0.5, 0.1), DropZone::Edge(Direction::Up));
        assert_eq!(at(0.5, 0.9), DropZone::Edge(Direction::Down));
        assert_eq!(at(0.2, 0.1), DropZone::Edge(Direction::Up), "a corner goes to the nearest edge");
        assert_eq!(at(0.05, 0.3), DropZone::Edge(Direction::Left));
    }

    #[test]
    fn zone_rect_is_the_half_on_that_side() {
        let area = rect(0.5, 0.0, 0.5, 1.0);
        assert_eq!(zone_rect(area, DropZone::Center), area);
        assert_eq!(zone_rect(area, DropZone::Edge(Direction::Left)), rect(0.5, 0.0, 0.25, 1.0));
        assert_eq!(zone_rect(area, DropZone::Edge(Direction::Right)), rect(0.75, 0.0, 0.25, 1.0));
        assert_eq!(zone_rect(area, DropZone::Edge(Direction::Up)), rect(0.5, 0.0, 0.5, 0.5));
        assert_eq!(zone_rect(area, DropZone::Edge(Direction::Down)), rect(0.5, 0.5, 0.5, 0.5));
    }
}

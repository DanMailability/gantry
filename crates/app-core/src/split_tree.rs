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
                    Direction::Right => ((origin.x + origin.w - r.x).abs() < EPS, overlap(r.y, r.h, origin.y, origin.h)),
                    Direction::Up => ((r.y + r.h - origin.y).abs() < EPS, overlap(r.x, r.w, origin.x, origin.w)),
                    Direction::Down => ((origin.y + origin.h - r.y).abs() < EPS, overlap(r.x, r.w, origin.x, origin.w)),
                };
                (touches && overlap > EPS).then_some((id, overlap, r))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.y.total_cmp(&a.2.y)).then(b.2.x.total_cmp(&a.2.x)))
            .map(|(id, ..)| id)
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
        assert_eq!(tree.dividers(Rect::UNIT)[0], Divider { split: 10, axis: Axis::Horizontal, area: Rect::UNIT, ratio: MIN_RATIO });
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
}

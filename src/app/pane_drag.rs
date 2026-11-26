//! Moving a pane by its header: dragged onto another pane of its tab,
//! it trades places with that pane (dropped on its middle) or settles
//! beside it (dropped near one of its edges); onto another tab in the
//! list, it joins that tab; onto the new-tab button, it gets its own.

use super::*;

/// How near an edge, as a share of the pane's width or height, a drop
/// puts the dragged pane on that side rather than swapping the two.
const EDGE: f32 = 0.3;

/// Where on a pane a dragged header would land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Drop {
    /// The two panes trade places.
    Swap,
    Left,
    Right,
    Top,
    Bottom,
}

/// Where a dragged header would put its pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Landing {
    /// On a pane of the same tab.
    Pane(u64, Drop),
    /// On another tab in the list: the pane joins it, on its right.
    Tab(usize),
    /// On the new-tab button: the pane gets a tab of its own.
    NewTab,
}

/// A press on a pane's header, and the drag it may turn into.
pub(super) struct HeaderDrag {
    pub(super) id: u64,
    pub(super) from: (f32, f32),
    pub(super) dragging: bool,
    /// An agent's header: a click that doesn't drag opens or closes it.
    pub(super) toggles: bool,
    /// Where the drop would land.
    pub(super) target: Option<Landing>,
}

impl Drop {
    /// Where on `rect` the point is: the edge it's nearest, if it's within
    /// EDGE of it, or the middle.
    pub(super) fn at(rect: Rect, x: f32, y: f32) -> Drop {
        let fx = ((x - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let fy = ((y - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
        let edges = [(fx, Drop::Left), (1.0 - fx, Drop::Right), (fy, Drop::Top), (1.0 - fy, Drop::Bottom)];
        let (near, drop) = edges.into_iter().fold((f32::MAX, Drop::Swap), |a, b| if b.0 < a.0 { b } else { a });
        if near < EDGE {
            drop
        } else {
            Drop::Swap
        }
    }

    /// The part of `rect` the dragged pane would take: all of it for a
    /// swap, the half on that side for an edge.
    pub(super) fn area(self, rect: Rect) -> Rect {
        let (w, h) = ((rect.w / 2.0).round(), (rect.h / 2.0).round());
        match self {
            Drop::Swap => rect,
            Drop::Left => Rect::new(rect.x, rect.y, w, rect.h),
            Drop::Right => Rect::new(rect.right() - w, rect.y, w, rect.h),
            Drop::Top => Rect::new(rect.x, rect.y, rect.w, h),
            Drop::Bottom => Rect::new(rect.x, rect.bottom() - h, rect.w, h),
        }
    }
}

impl Node {
    /// Leaves `a` and `b` trade places.
    pub(super) fn swap(&mut self, a: u64, b: u64) {
        match self {
            Node::Leaf(id) if *id == a => *id = b,
            Node::Leaf(id) if *id == b => *id = a,
            Node::Leaf(_) => {}
            Node::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }
}

/// Moves pane `id` to `drop` on pane `target`, in the tree `root`.
/// False, and nothing changed, when it can't (the same pane, or one of
/// them isn't in the tree).
pub(super) fn rearrange(root: &mut Node, id: u64, target: u64, drop: Drop) -> bool {
    if id == target || !root.contains(id) || !root.contains(target) {
        return false;
    }
    let (axis, before) = match drop {
        Drop::Swap => {
            root.swap(id, target);
            return true;
        }
        Drop::Left => (Axis::Row, true),
        Drop::Right => (Axis::Row, false),
        Drop::Top => (Axis::Column, true),
        Drop::Bottom => (Axis::Column, false),
    };
    // With two panes in the tree, `id` is never the whole of it, so it
    // always comes out.
    root.remove(id) && root.split(target, id, axis, before)
}

impl App {
    /// The pane of the active tab whose header bar is at the point.
    pub(super) fn header_bar_at(&self, x: f32, y: f32) -> Option<u64> {
        self.tab_slots(self.active)
            .0
            .into_iter()
            .filter(|(id, _)| self.panes.iter().any(|pane| pane.id == *id))
            .find(|(_, slot)| Rect::new(slot.x, slot.y, slot.w, AGENT_HEADER.min(slot.h)).contains(x, y))
            .map(|(id, _)| id)
    }

    /// Where pane `id`, dragged by its header, would land at the point.
    fn landing_at(&self, x: f32, y: f32, id: u64) -> Option<Landing> {
        let slots = self.tab_slots(self.active).0;
        if self.tab_area().contains(x, y) {
            if self.new_tab_hit.contains(x, y) {
                // A pane alone in its tab has its own tab already.
                return (slots.len() > 1).then_some(Landing::NewTab);
            }
            // A tab's row, or one of its pane lines under it.
            let tab = self
                .tab_hits
                .iter()
                .find(|(r, _)| r.contains(x, y))
                .map(|&(_, index)| index)
                .or_else(|| self.pane_hits.iter().find(|(r, _, _)| r.contains(x, y)).map(|&(_, index, _)| index))?;
            let open = tab != self.active && self.tabs.get(tab).is_some_and(|t| !t.root.contains(SETTINGS));
            return open.then_some(Landing::Tab(tab));
        }
        slots
            .into_iter()
            .find(|(pane, slot)| *pane != id && slot.contains(x, y))
            .map(|(pane, slot)| Landing::Pane(pane, Drop::at(slot, x, y)))
    }

    /// The pointer moved with a header pressed: past DRAG_SLOP it drags
    /// the pane, and what's under it shows where it would land.
    pub(super) fn header_dragged(&mut self, x: f32, y: f32) {
        let Pointer::Header(drag) = &self.pointer else { return };
        let moved = (x - drag.from.0).abs().max((y - drag.from.1).abs()) > DRAG_SLOP;
        if !(drag.dragging || moved) {
            return;
        }
        let target = self.landing_at(x, y, drag.id);
        let Pointer::Header(drag) = &mut self.pointer else { return };
        let changed = !drag.dragging || target != drag.target;
        drag.dragging = true;
        drag.target = target;
        if changed {
            self.dirty = true;
        }
        self.set_cursor(Cursor::Arrow);
    }

    /// The header let go: a click opens or closes an agent's header; a
    /// drag moves its pane where it was dropped.
    pub(super) fn header_released(&mut self, drag: HeaderDrag) {
        self.dirty = true;
        if !drag.dragging {
            if drag.toggles {
                if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == drag.id) {
                    pane.header_open = !pane.header_open;
                }
            }
            return;
        }
        match drag.target {
            Some(Landing::Pane(target, drop)) => {
                let tab = &mut self.tabs[self.active];
                if rearrange(&mut tab.root, drag.id, target, drop) {
                    tab.zoomed = false;
                    tab.focus = drag.id;
                    self.relayout();
                    self.save_tabs();
                }
            }
            Some(Landing::Tab(to)) => self.move_pane_to_tab(drag.id, Some(to)),
            Some(Landing::NewTab) => self.move_pane_to_tab(drag.id, None),
            None => {}
        }
    }

    /// Moves pane `id` out of the active tab: into tab `to`, on the right
    /// of what it shows, or (None) into a tab of its own after this one.
    /// A tab it leaves empty closes. Either way, its new tab comes up.
    fn move_pane_to_tab(&mut self, id: u64, to: Option<usize>) {
        let from = self.active;
        let count = self.tabs.len();
        let source = &mut self.tabs[from];
        if !source.root.contains(id) || to.is_some_and(|to| to == from || to >= count) {
            return;
        }
        let emptied = !source.root.remove(id);
        if emptied && to.is_none() {
            return;
        }
        if !emptied {
            source.zoomed = false;
            if source.focus == id {
                let mut leaves = Vec::new();
                source.root.leaves(&mut leaves);
                source.focus = leaves[0];
            }
        }
        let mut target = match to {
            Some(to) => {
                let tab = &mut self.tabs[to];
                let rest = std::mem::replace(&mut tab.root, Node::Leaf(id));
                tab.root = join_right(rest, id);
                tab.zoomed = false;
                tab.focus = id;
                to
            }
            None => {
                let tab = Tab {
                    root: Node::Leaf(id),
                    focus: id,
                    zoomed: false,
                    name: None,
                    color: None,
                    shown: None,
                    slide: None,
                    mark: None,
                    marked_at: None,
                    ai_title: AiTitle::default(),
                };
                self.tabs.insert(from + 1, tab);
                from + 1
            }
        };
        if emptied {
            self.tabs.remove(from);
            self.tab_menu = None;
            self.renaming = None;
            if target > from {
                target -= 1;
            }
            // The tab that was showing is gone: the moved pane's is now.
            self.active = target;
        }
        self.switch_tab(target);
        self.relayout();
        self.save_tabs();
    }

    /// While a header is dragged: its pane dims, and the place it would
    /// go is lit with the accent. Drawn over the tab list.
    pub(super) fn draw_header_drag(&mut self) {
        let Pointer::Header(drag) = &self.pointer else { return };
        if !drag.dragging {
            return;
        }
        let slots = self.tab_slots(self.active).0;
        let slot = |id: u64| slots.iter().find(|(pane, _)| *pane == id).map(|&(_, rect)| rect);
        let target = match drag.target {
            Some(Landing::Pane(id, drop)) => slot(id).map(|rect| drop.area(rect).inset(6.0, 6.0)),
            Some(Landing::Tab(index)) => self.tab_hits.iter().find(|(_, tab)| *tab == index).map(|&(rect, _)| rect),
            Some(Landing::NewTab) => Some(self.new_tab_hit),
            None => None,
        };
        let source = slot(drag.id);
        let Self { scene, ctx, .. } = self;
        let theme = &ctx.theme;
        scene.set_layer(LAYER_OVERLAY);
        if let Some(rect) = source {
            scene.fill(rect, theme.background.alpha(0.5));
        }
        if let Some(rect) = target {
            scene.quad(Quad::new(rect, theme.accent.alpha(0.14)).radius(8.0).border(1.5, theme.accent.alpha(0.7)));
        }
        scene.set_layer(LAYER_CONTENT);
    }
}

/// `rest` with pane `id` beside it on the right, taking half.
fn join_right(rest: Node, id: u64) -> Node {
    Node::Split { axis: Axis::Row, ratio: 0.5, first: Box::new(rest), second: Box::new(Node::Leaf(id)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(node: &Node) -> Vec<u64> {
        let mut out = Vec::new();
        node.leaves(&mut out);
        out
    }

    fn pair(axis: Axis, a: u64, b: u64) -> Node {
        Node::Split { axis, ratio: 0.5, first: Box::new(Node::Leaf(a)), second: Box::new(Node::Leaf(b)) }
    }

    fn axis_of(node: &Node) -> Option<Axis> {
        match node {
            Node::Split { axis, .. } => Some(*axis),
            Node::Leaf(_) => None,
        }
    }

    #[test]
    fn the_middle_swaps_and_the_edges_put_it_beside() {
        let rect = Rect::new(100.0, 100.0, 400.0, 200.0);
        assert_eq!(Drop::at(rect, 300.0, 200.0), Drop::Swap);
        assert_eq!(Drop::at(rect, 110.0, 200.0), Drop::Left);
        assert_eq!(Drop::at(rect, 490.0, 200.0), Drop::Right);
        assert_eq!(Drop::at(rect, 300.0, 110.0), Drop::Top);
        assert_eq!(Drop::at(rect, 300.0, 290.0), Drop::Bottom);
    }

    #[test]
    fn an_edge_lights_the_half_on_its_side() {
        let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
        let right = Drop::Right.area(rect);
        assert_eq!((right.x, right.w, right.h), (200.0, 200.0, 200.0));
        let top = Drop::Top.area(rect);
        assert_eq!((top.y, top.w, top.h), (0.0, 400.0, 100.0));
    }

    #[test]
    fn dropped_in_the_middle_two_panes_trade_places() {
        // 1 | (2 over 3): 1 dropped on 3's middle.
        let mut root = Node::Split {
            axis: Axis::Row,
            ratio: 0.3,
            first: Box::new(Node::Leaf(1)),
            second: Box::new(pair(Axis::Column, 2, 3)),
        };
        assert!(rearrange(&mut root, 1, 3, Drop::Swap));
        assert_eq!(leaves(&root), [3, 2, 1]);
        // The splits and their sizes stay as they were.
        assert!(matches!(root, Node::Split { axis: Axis::Row, ratio, .. } if ratio == 0.3));
    }

    #[test]
    fn dropped_on_an_edge_it_moves_beside_that_pane() {
        // 1 | 2 | 3 in a row; 3 dropped on top of 1: (3 over 1) | 2.
        let mut root = Node::Split {
            axis: Axis::Row,
            ratio: 0.5,
            first: Box::new(pair(Axis::Row, 1, 2)),
            second: Box::new(Node::Leaf(3)),
        };
        assert!(rearrange(&mut root, 3, 1, Drop::Top));
        assert_eq!(leaves(&root), [3, 1, 2]);
        let Node::Split { first, .. } = &root else { panic!("still split") };
        assert_eq!(axis_of(first), Some(Axis::Column));
    }

    #[test]
    fn two_side_by_side_panes_can_be_stacked() {
        let mut root = pair(Axis::Row, 1, 2);
        assert!(rearrange(&mut root, 1, 2, Drop::Bottom));
        assert_eq!(leaves(&root), [2, 1]);
        assert_eq!(axis_of(&root), Some(Axis::Column));
    }

    #[test]
    fn a_pane_joining_a_tab_takes_its_right_half() {
        let root = join_right(pair(Axis::Column, 1, 2), 3);
        assert_eq!(leaves(&root), [1, 2, 3]);
        assert_eq!(axis_of(&root), Some(Axis::Row));
    }

    #[test]
    fn a_pane_dropped_on_itself_or_a_stranger_changes_nothing() {
        let mut root = pair(Axis::Row, 1, 2);
        assert!(!rearrange(&mut root, 1, 1, Drop::Left));
        assert!(!rearrange(&mut root, 1, 9, Drop::Swap));
        assert_eq!(leaves(&root), [1, 2]);
        assert_eq!(axis_of(&root), Some(Axis::Row));
    }
}

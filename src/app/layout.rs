use super::*;

impl Node {
    pub(super) fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.leaves(out);
                second.leaves(out);
            }
        }
    }

    pub(super) fn contains(&self, id: u64) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    /// Replaces leaf `id` with a split of it and `new`.
    /// Splits leaf `id`, `new` after it (right or below), or before it.
    pub(super) fn split(&mut self, id: u64, new: u64, axis: Axis, before: bool) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == id => {
                let (first, second) = if before { (new, id) } else { (id, new) };
                *self = Node::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(first)),
                    second: Box::new(Node::Leaf(second)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split(id, new, axis, before) || second.split(id, new, axis, before)
            }
        }
    }

    /// Takes leaf `id` out; its sibling takes the parent's place. Returns
    /// false when `id` is this whole node (the caller drops it).
    pub(super) fn remove(&mut self, id: u64) -> bool {
        let Node::Split { first, second, .. } = self else { return false };
        let keep = if matches!(**first, Node::Leaf(leaf) if leaf == id) {
            std::mem::replace(&mut **second, Node::Leaf(0))
        } else if matches!(**second, Node::Leaf(leaf) if leaf == id) {
            std::mem::replace(&mut **first, Node::Leaf(0))
        } else {
            return first.remove(id) || second.remove(id);
        };
        *self = keep;
        true
    }

    /// Lays the tree out in `rect`: each pane's rectangle, and the dividers
    /// (with the path to their split, false = first child).
    pub(super) fn layout(
        &self,
        rect: Rect,
        path: &mut Vec<bool>,
        panes: &mut Vec<(u64, Rect)>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Node::Leaf(id) => panes.push((*id, rect)),
            Node::Split { axis, ratio, first, second } => {
                let (a, b, line) = split_rect(rect, *axis, *ratio);
                dividers.push(Divider { line, axis: *axis, parent: rect, path: path.clone() });
                path.push(false);
                first.layout(a, path, panes, dividers);
                path.pop();
                path.push(true);
                second.layout(b, path, panes, dividers);
                path.pop();
            }
        }
    }

    pub(super) fn at_path(&mut self, path: &[bool]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((&side, rest)) => match self {
                Node::Split { first, second, .. } => (if side { second } else { first }).at_path(rest),
                Node::Leaf(_) => None,
            },
        }
    }
}

pub(super) fn split_rect(rect: Rect, axis: Axis, ratio: f32) -> (Rect, Rect, Rect) {
    match axis {
        Axis::Row => {
            let w = ((rect.w - GAP) * ratio).round();
            let a = Rect::new(rect.x, rect.y, w, rect.h);
            let line = Rect::new(rect.x + w, rect.y, GAP, rect.h);
            let b = Rect::new(rect.x + w + GAP, rect.y, rect.w - w - GAP, rect.h);
            (a, b, line)
        }
        Axis::Column => {
            let h = ((rect.h - GAP) * ratio).round();
            let a = Rect::new(rect.x, rect.y, rect.w, h);
            let line = Rect::new(rect.x, rect.y + h, rect.w, GAP);
            let b = Rect::new(rect.x, rect.y + h + GAP, rect.w, rect.h - h - GAP);
            (a, b, line)
        }
    }
}

/// The sidebar width a drag to `x` asks for, between its minimum and the
/// lesser of its maximum and half the window. The upper end never drops
/// below the minimum: in a window narrower than twice the minimum, a
/// plain `clamp(SIDEBAR_MIN, …)` gets inverted bounds and panics.
pub(super) fn sidebar_drag_width(x: f32, window_width: f32) -> f32 {
    let max = SIDEBAR_MAX.min(window_width / 2.0).max(SIDEBAR_MIN);
    x.clamp(SIDEBAR_MIN, max)
}

impl Divider {
    pub(super) fn grab_area(&self) -> Rect {
        match self.axis {
            Axis::Row => Rect::new(self.line.x - GRAB, self.line.y, self.line.w + 2.0 * GRAB, self.line.h),
            Axis::Column => Rect::new(self.line.x, self.line.y - GRAB, self.line.w, self.line.h + 2.0 * GRAB),
        }
    }
}

impl App {
    pub(super) fn split(&mut self, axis: Axis, before: bool) {
        let Some(focus) = self.focused_id().filter(|&id| id != SETTINGS) else { return };
        match self.make_pane() {
            Ok(id) => {
                let tab = &mut self.tabs[self.active];
                tab.root.split(focus, id, axis, before);
                tab.zoomed = false;
                self.set_focus(id);
                self.save_tabs();
            }
            Err(error) => self.ctx.show_toast(&error),
        }
    }
    /// Closes a pane, its tab when it was the last, the window after that.
    pub(super) fn close_pane(&mut self, id: u64) {
        crate::debug_log!("pane {id}: closing");
        if let Some(index) = self.viewers.iter().position(|v| v.id == id) {
            let viewer = &mut self.viewers[index];
            if viewer.dirty && !viewer.close_armed {
                viewer.close_armed = true;
                let (save, close) = (platform::shortcut("S"), platform::shortcut("W"));
                let message =
                    format!("{} has unsaved changes: {save} saves, {close} again discards them", viewer.title);
                self.ctx.toast = Some((message, Instant::now() + Duration::from_secs(5)));
                self.dirty = true;
                return;
            }
            self.viewers.remove(index);
            platform::viewer_close(id);
        }
        if let Some(index) = self.panes.iter().position(|p| p.id == id) {
            let mut pane = self.panes.remove(index);
            pane.hangup();
        }
        let Some(tab_index) = self.tabs.iter().position(|t| t.root.contains(id)) else { return };
        let tab = &mut self.tabs[tab_index];
        if !tab.root.remove(id) {
            self.tabs.remove(tab_index);
            self.tab_menu = None;
            self.renaming = None;
            if !self.tabs.is_empty() {
                self.save_tabs();
            }
            if self.tabs.is_empty() {
                self.close_window();
                return;
            }
            self.switch_tab(if tab_index < self.active || self.active >= self.tabs.len() {
                self.active.saturating_sub(1).min(self.tabs.len() - 1)
            } else {
                self.active
            });
            return;
        }
        tab.zoomed = false;
        if tab.focus == id {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            tab.focus = leaves[0];
        }
        self.relayout();
        self.save_tabs();
    }
    pub(super) fn set_focus(&mut self, id: u64) {
        let before = self.focused_id();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus = id;
        }
        self.relayout();
        self.focus_moved(before);
    }
    /// Tells programs that asked (focus reporting) that focus moved.
    pub(super) fn focus_moved(&mut self, before: Option<u64>) {
        let after = self.focused_id();
        if before == after {
            return;
        }
        if before.is_some() {
            self.last_focus = before;
        }
        // The keyboard goes to a viewer's native view, or back from one.
        let viewer = after.filter(|&id| self.is_viewer(id));
        if viewer.is_some() || before.is_some_and(|id| self.is_viewer(id)) {
            platform::viewer_focus(viewer);
        }
        let now = Instant::now();
        self.ctx.blink_epoch = now;
        let window = self.focused;
        let idle = recap_idle();
        if let Some(pane) = before.and_then(|id| self.panes.iter_mut().find(|p| p.id == id)) {
            pane.focus_changed(false);
            pane.focus_lost(now);
        }
        let note = after.and_then(|id| self.mark_note(id, now));
        if let Some(pane) = after.and_then(|id| self.panes.iter_mut().find(|p| p.id == id)) {
            pane.focus_changed(window);
            if window {
                pane.focus_gained(now, Some(idle), note);
            }
        }
    }
    /// The panes of the active tab in reading order, for Command+[ and ].
    pub(super) fn cycle_focus(&mut self, delta: i32) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let mut leaves = Vec::new();
        tab.root.leaves(&mut leaves);
        let at = leaves.iter().position(|&id| id == tab.focus).unwrap_or(0) as i32;
        let next = leaves[(at + delta).rem_euclid(leaves.len() as i32) as usize];
        self.set_focus(next);
    }
    /// Command+`: the pane whose agent has waited on you longest, else the
    /// one longest done with a turn you haven't seen; with Shift, the
    /// newest. With nothing to see, back to the pane you were in before.
    pub(super) fn jump_to_needs(&mut self, newest: bool) {
        let focus = self.focused_id();
        let mut needs: Vec<(AgentState, Instant, u64)> = self
            .panes
            .iter()
            .filter(|pane| Some(pane.id) != focus)
            // Tabs set aside by hand wait their turn.
            .filter(|pane| !self.mark_of(pane.id).is_some_and(Mark::aside))
            .filter_map(|pane| pane.needs_since.map(|(state, at)| (state, at, pane.id)))
            .collect();
        let important = |id: u64| self.mark_of(id) == Some(Mark::Important);
        needs.sort_by(|a, b| {
            important(b.2).cmp(&important(a.2)).then(b.0.cmp(&a.0)).then(if newest {
                b.1.cmp(&a.1)
            } else {
                a.1.cmp(&b.1)
            })
        });
        let back = self.last_focus.filter(|id| Some(*id) != focus && self.panes.iter().any(|pane| pane.id == *id));
        match needs.first().map(|need| need.2).or(back) {
            Some(id) => self.reveal_pane(id),
            None => self.ctx.show_toast("Nothing needs you"),
        }
    }
    /// Focuses pane `id`, in whichever tab it's in.
    pub(super) fn reveal_pane(&mut self, id: u64) {
        let Some(index) = self.tabs.iter().position(|tab| {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            leaves.contains(&id)
        }) else {
            return;
        };
        let before = self.focused_id();
        self.active = index;
        self.tabs[index].focus = id;
        self.relayout();
        self.focus_moved(before);
    }
    /// Moves focus to the nearest pane in a direction (Command+Option+arrow).
    pub(super) fn focus_toward(&mut self, dx: f32, dy: f32) {
        let Some(focus) = self.focused_id() else { return };
        let Some(from) = self.panes.iter().find(|p| p.id == focus).map(|p| p.rect) else { return };
        let (cx, cy) = (from.x + from.w / 2.0, from.y + from.h / 2.0);
        let best = self
            .visible_panes()
            .into_iter()
            .filter(|(id, _)| *id != focus)
            .filter_map(|(id, r)| {
                let (px, py) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                let along = (px - cx) * dx + (py - cy) * dy;
                let across = ((px - cx) * dy).abs() + ((py - cy) * dx).abs();
                (along > 0.0).then_some((id, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((id, _)) = best {
            self.set_focus(id);
        }
    }
    pub(super) fn toggle_zoom(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            let mut leaves = Vec::new();
            tab.root.leaves(&mut leaves);
            tab.zoomed = !tab.zoomed && leaves.len() > 1;
            self.relayout();
        }
    }
    pub(super) fn tab_list_visible(&self) -> bool {
        self.tab_list.unwrap_or(self.tabs.len() > 1)
    }
    /// How much of the window's top the title bar takes, in zoomed pixels.
    pub(super) fn top(&self) -> f32 {
        platform::TITLEBAR / self.zoom
    }
    /// The tab list's area, under the title bar; empty while it is hidden.
    pub(super) fn tab_area(&self) -> Rect {
        if !self.tab_list_visible() {
            return Rect::default();
        }
        let (top, height) = (self.top(), (self.height - self.top()).max(1.0));
        match self.strip {
            TabStrip::Left if self.compact => Rect::new(0.0, top, SIDEBAR_COMPACT, height),
            TabStrip::Left => Rect::new(0.0, top, self.sidebar_width.min(self.width / 2.0), height),
            TabStrip::Top => Rect::new(0.0, top, self.width, TAB_BAR_HEIGHT),
        }
    }
    /// Where a press grabs the sidebar's edge; empty without a sidebar.
    pub(super) fn sidebar_edge(&self) -> Rect {
        let area = self.tab_area();
        if self.strip != TabStrip::Left || area.w == 0.0 {
            return Rect::default();
        }
        Rect::new(area.right() - GRAB, area.y, 2.0 * GRAB, area.h)
    }
    pub(super) fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
        self.relayout();
        self.save_sidebar();
    }
    pub(super) fn set_extended(&mut self, extended: bool) {
        self.extended = extended;
        if extended && self.compact {
            self.compact = false;
            self.relayout();
        }
        if extended && !self.tab_list_visible() {
            self.tab_list = Some(true);
            self.relayout();
        }
        self.dirty = true;
        self.save_sidebar();
    }
    pub(super) fn save_sidebar(&self) {
        let mode = match (self.compact, self.extended) {
            (true, _) => "compact",
            (false, true) => "extended",
            (false, false) => "full",
        };
        crate::save_setting("sidebar", mode);
        crate::save_setting("sidebar_width", &format!("{}", self.sidebar_width.round()));
    }
    pub(super) fn content_rect(&self) -> Rect {
        let (width, top) = (self.width, self.top());
        let height = (self.height - top).max(1.0);
        let area = self.tab_area();
        match self.strip {
            TabStrip::Left => Rect::new(area.w, top, (width - area.w).max(1.0), height),
            TabStrip::Top => Rect::new(0.0, top + area.h, width, (height - area.h).max(1.0)),
        }
    }
    /// Lays out one tab: its panes' rectangles, under their headers, and
    /// its dividers.
    pub(super) fn tab_layout(&self, index: usize) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let (mut panes, dividers) = self.tab_slots(index);
        for (id, rect) in &mut panes {
            if let Some(pane) = self.panes.iter().find(|pane| pane.id == *id) {
                *rect = under_header(*rect, pane.header_reach);
            }
        }
        (panes, dividers)
    }
    /// Each pane's whole place in tab `index`, its header's included, and
    /// the tab's dividers.
    pub(super) fn tab_slots(&self, index: usize) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let tab = &self.tabs[index];
        let rect = self.content_rect();
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        if tab.zoomed {
            panes.push((tab.focus, rect));
        } else {
            tab.root.layout(rect, &mut Vec::new(), &mut panes, &mut dividers);
        }
        (panes, dividers)
    }
    pub(super) fn visible_panes(&self) -> Vec<(u64, Rect)> {
        if self.tabs.is_empty() {
            return Vec::new();
        }
        self.tab_layout(self.active).0
    }
    /// Gives every pane (in every tab) its place, so switching tabs never
    /// has to rewrap. Zoomed-away panes keep their size.
    pub(super) fn relayout(&mut self) {
        for index in 0..self.tabs.len() {
            let (rects, dividers) = self.tab_layout(index);
            for (id, rect) in rects {
                let text = &self.ctx.text;
                if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                    pane.set_rect(rect, text);
                }
            }
            if index == self.active {
                self.dividers = dividers;
            }
        }
        self.dirty = true;
    }
    /// The tab whose slot in the list is under the pointer, along the list.
    pub(super) fn tab_slot_at(&self, x: f32, y: f32) -> Option<usize> {
        self.tab_hits
            .iter()
            .find(|(r, _)| match self.strip {
                TabStrip::Left => y >= r.y && y < r.bottom() + 4.0,
                TabStrip::Top => x >= r.x && x < r.right() + 4.0,
            })
            .map(|&(_, index)| index)
    }
    pub(super) fn pane_at(&self, x: f32, y: f32) -> Option<(u64, Rect)> {
        self.visible_panes().into_iter().find(|(_, rect)| rect.contains(x, y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree as text: `3` for a pane, `R(a,b)` / `C(a,b)` for a split,
    /// with its ratio when it isn't the even 0.5.
    fn shape(node: &Node) -> String {
        match node {
            Node::Leaf(id) => id.to_string(),
            Node::Split { axis, ratio, first, second } => {
                let axis = if *axis == Axis::Row { 'R' } else { 'C' };
                let ratio = if *ratio == 0.5 { String::new() } else { ratio.to_string() };
                format!("{axis}{ratio}({},{})", shape(first), shape(second))
            }
        }
    }

    fn split(axis: Axis, ratio: f32, first: Node, second: Node) -> Node {
        Node::Split { axis, ratio, first: Box::new(first), second: Box::new(second) }
    }

    fn leaves(node: &Node) -> Vec<u64> {
        let mut out = Vec::new();
        node.leaves(&mut out);
        out
    }

    fn lay_out(node: &Node, rect: Rect) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        node.layout(rect, &mut Vec::new(), &mut panes, &mut dividers);
        (panes, dividers)
    }

    #[test]
    fn sidebar_drag_width_holds_its_bounds_in_a_narrow_window() {
        assert_eq!(sidebar_drag_width(200.0, 1200.0), 200.0);
        assert_eq!(sidebar_drag_width(999.0, 1200.0), SIDEBAR_MAX);
        assert_eq!(sidebar_drag_width(50.0, 1200.0), SIDEBAR_MIN);
        // Half of a 250px window is under the minimum: the bounds must not
        // invert (a plain clamp panics there), the minimum wins.
        assert_eq!(sidebar_drag_width(200.0, 250.0), SIDEBAR_MIN);
        assert_eq!(sidebar_drag_width(200.0, 0.0), SIDEBAR_MIN);
    }

    #[test]
    fn a_new_pane_opens_after_the_focused_one_or_before_it() {
        let mut root = Node::Leaf(1);
        assert!(root.split(1, 2, Axis::Row, false));
        assert_eq!(shape(&root), "R(1,2)");
        // Splitting a pane inside a split nests there, in its place.
        assert!(root.split(1, 3, Axis::Column, true));
        assert_eq!(shape(&root), "R(C(3,1),2)");
        assert!(root.split(2, 4, Axis::Column, false));
        assert_eq!(shape(&root), "R(C(3,1),C(2,4))");
        assert_eq!(leaves(&root), [3, 1, 2, 4]);
    }

    #[test]
    fn splitting_a_pane_that_isnt_there_changes_nothing() {
        let mut root = split(Axis::Row, 0.3, Node::Leaf(1), Node::Leaf(2));
        assert!(!root.split(9, 5, Axis::Column, false));
        assert_eq!(shape(&root), "R0.3(1,2)");
    }

    #[test]
    fn a_closed_pane_hands_its_place_to_its_sibling() {
        // 1 | (2 over (3 | 4))
        let mut root = split(
            Axis::Row,
            0.3,
            Node::Leaf(1),
            split(Axis::Column, 0.7, Node::Leaf(2), split(Axis::Row, 0.5, Node::Leaf(3), Node::Leaf(4))),
        );
        assert!(root.remove(4));
        assert_eq!(shape(&root), "R0.3(1,C0.7(2,3))");
        // The last pane but one in a branch: the branch is now just its pane.
        assert!(root.remove(2));
        assert_eq!(shape(&root), "R0.3(1,3)");
        // The sibling keeps its own splits and ratios when it moves up.
        let mut root = split(Axis::Row, 0.3, Node::Leaf(1), split(Axis::Column, 0.7, Node::Leaf(2), Node::Leaf(3)));
        assert!(root.remove(1));
        assert_eq!(shape(&root), "C0.7(2,3)");
        assert!(root.remove(3));
        assert_eq!(shape(&root), "2");
    }

    #[test]
    fn closing_the_only_pane_leaves_the_tab_to_its_caller() {
        let mut root = Node::Leaf(7);
        assert!(!root.remove(7));
        assert_eq!(shape(&root), "7");
        // Nor does closing a pane that isn't there change anything.
        let mut root = split(Axis::Row, 0.5, Node::Leaf(1), Node::Leaf(2));
        assert!(!root.remove(3));
        assert_eq!(shape(&root), "R(1,2)");
    }

    #[test]
    fn panes_are_found_anywhere_in_the_tree() {
        let root = split(Axis::Row, 0.5, Node::Leaf(1), split(Axis::Column, 0.5, Node::Leaf(2), Node::Leaf(3)));
        for id in [1, 2, 3] {
            assert!(root.contains(id), "{id}");
        }
        assert!(!root.contains(4));
        assert!(!root.contains(0));
    }

    #[test]
    fn side_by_side_panes_share_the_width_around_a_one_pixel_line() {
        let rect = Rect::new(10.0, 20.0, 1001.0, 600.0);
        let (a, b, line) = split_rect(rect, Axis::Row, 0.5);
        assert_eq!(a, Rect::new(10.0, 20.0, 500.0, 600.0));
        assert_eq!(line, Rect::new(510.0, 20.0, GAP, 600.0));
        assert_eq!(b, Rect::new(511.0, 20.0, 500.0, 600.0));
        let (a, b, line) = split_rect(rect, Axis::Column, 0.25);
        assert_eq!(a, Rect::new(10.0, 20.0, 1001.0, 150.0));
        assert_eq!(line, Rect::new(10.0, 170.0, 1001.0, GAP));
        assert_eq!(b, Rect::new(10.0, 171.0, 1001.0, 449.0));
    }

    #[test]
    fn uneven_ratios_round_to_whole_pixels_and_lose_none() {
        let rect = Rect::new(0.0, 0.0, 777.0, 333.0);
        for ratio in [0.05, 0.1, 0.333, 0.5, 0.618, 0.95] {
            for axis in [Axis::Row, Axis::Column] {
                let (a, b, line) = split_rect(rect, axis, ratio);
                for r in [a, b, line] {
                    assert_eq!(r.x.fract() + r.y.fract() + r.w.fract() + r.h.fract(), 0.0, "{ratio} {axis:?}");
                }
                match axis {
                    Axis::Row => assert_eq!(a.w + line.w + b.w, rect.w),
                    Axis::Column => assert_eq!(a.h + line.h + b.h, rect.h),
                }
                assert_eq!(b.right(), rect.right());
                assert_eq!(b.bottom(), rect.bottom());
            }
        }
    }

    #[test]
    fn nested_panes_tile_the_tab_with_nothing_over_another() {
        // (1 over 2) | (3 | (4 over 5)), with uneven ratios.
        let root = split(
            Axis::Row,
            0.4,
            split(Axis::Column, 0.3, Node::Leaf(1), Node::Leaf(2)),
            split(Axis::Row, 0.6, Node::Leaf(3), split(Axis::Column, 0.5, Node::Leaf(4), Node::Leaf(5))),
        );
        let rect = Rect::new(100.0, 50.0, 1200.0, 800.0);
        let (panes, dividers) = lay_out(&root, rect);
        assert_eq!(panes.iter().map(|p| p.0).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
        assert_eq!(dividers.len(), 4);
        let area = |r: &Rect| r.w * r.h;
        let covered: f32 =
            panes.iter().map(|(_, r)| area(r)).sum::<f32>() + dividers.iter().map(|d| area(&d.line)).sum::<f32>();
        assert_eq!(covered, area(&rect));
        let all: Vec<Rect> = panes.iter().map(|p| p.1).chain(dividers.iter().map(|d| d.line)).collect();
        for (i, a) in all.iter().enumerate() {
            assert!(a.x >= rect.x && a.y >= rect.y && a.right() <= rect.right() && a.bottom() <= rect.bottom());
            for b in &all[i + 1..] {
                let apart = a.right() <= b.x || b.right() <= a.x || a.bottom() <= b.y || b.bottom() <= a.y;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
        // Pane 1 sits over pane 2 in the left 40%.
        let (one, two) = (panes[0].1, panes[1].1);
        assert_eq!((one.x, one.w), (two.x, two.w));
        assert_eq!(one.w, ((1200.0 - GAP) * 0.4f32).round());
        assert!(one.bottom() < two.y);
    }

    #[test]
    fn each_divider_leads_back_to_its_split() {
        let mut root = split(Axis::Row, 0.5, Node::Leaf(1), split(Axis::Column, 0.5, Node::Leaf(2), Node::Leaf(3)));
        let rect = Rect::new(0.0, 0.0, 801.0, 601.0);
        let (_, dividers) = lay_out(&root, rect);
        assert_eq!(dividers[0].path, Vec::<bool>::new());
        assert_eq!(dividers[0].axis, Axis::Row);
        assert_eq!(dividers[0].parent, rect);
        assert_eq!(dividers[1].path, [true]);
        assert_eq!(dividers[1].axis, Axis::Column);
        assert_eq!(dividers[1].parent, Rect::new(401.0, 0.0, 400.0, 601.0));
        // Dragging a divider sets its split's ratio, and the panes follow.
        let Some(Node::Split { ratio, .. }) = root.at_path(&dividers[1].path) else { panic!("no split there") };
        *ratio = 0.25;
        let (panes, _) = lay_out(&root, rect);
        assert_eq!(panes[1].1.h, 150.0);
        assert_eq!(panes[2].1.h, 450.0);
        // Paths into panes stop there; paths past a pane lead nowhere.
        assert!(matches!(root.at_path(&[false]), Some(Node::Leaf(1))));
        assert!(matches!(root.at_path(&[true, true]), Some(Node::Leaf(3))));
        assert!(root.at_path(&[false, true]).is_none());
    }

    #[test]
    fn a_divider_is_easier_to_grab_than_it_is_to_see() {
        let rect = Rect::new(0.0, 0.0, 1001.0, 601.0);
        let (_, _, line) = split_rect(rect, Axis::Row, 0.5);
        let divider = Divider { line, axis: Axis::Row, parent: rect, path: Vec::new() };
        let grab = divider.grab_area();
        assert_eq!(grab, Rect::new(line.x - GRAB, 0.0, GAP + 2.0 * GRAB, 601.0));
        assert!(grab.contains(line.x - GRAB + 0.5, 300.0) && grab.contains(line.x + GAP + GRAB - 0.5, 300.0));
        let (_, _, line) = split_rect(rect, Axis::Column, 0.5);
        let divider = Divider { line, axis: Axis::Column, parent: rect, path: Vec::new() };
        assert_eq!(divider.grab_area(), Rect::new(0.0, line.y - GRAB, 1001.0, GAP + 2.0 * GRAB));
    }
}

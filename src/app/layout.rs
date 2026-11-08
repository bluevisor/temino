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
        if let Some(index) = self.viewers.iter().position(|v| v.id == id) {
            let viewer = &mut self.viewers[index];
            if viewer.dirty && !viewer.close_armed {
                viewer.close_armed = true;
                let cmd = platform::COMMAND_NAME;
                let message = format!("{} has unsaved changes: {cmd}S saves, {cmd}W again discards them", viewer.title);
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
    /// Lays out one tab: its panes' rectangles and its dividers.
    pub(super) fn tab_layout(&self, index: usize) -> (Vec<(u64, Rect)>, Vec<Divider>) {
        let tab = &self.tabs[index];
        let rect = self.content_rect();
        let (mut panes, mut dividers) = (Vec::new(), Vec::new());
        if tab.zoomed {
            panes.push((tab.focus, rect));
        } else {
            tab.root.layout(rect, &mut Vec::new(), &mut panes, &mut dividers);
        }
        // A pane with an agent (or a listed program) in it gives its top
        // to the header; with headers on, every terminal does.
        for (id, rect) in &mut panes {
            let header = self.panes.iter().any(|pane| pane.id == *id && self.has_header(pane));
            if header && rect.h > 3.0 * AGENT_HEADER {
                rect.y += AGENT_HEADER;
                rect.h -= AGENT_HEADER;
            }
        }
        (panes, dividers)
    }
    /// Whether `pane` gets a header over it, by Settings ▸ Status header.
    pub(super) fn has_header(&self, pane: &Pane) -> bool {
        match self.headers {
            Headers::Off => false,
            Headers::Auto => pane.agent_now.is_some() || pane.program_now.is_some(),
            Headers::On => true,
        }
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

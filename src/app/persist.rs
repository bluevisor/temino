use super::*;

impl App {
    /// Reopens the tabs open at the last quit, each with its panes in their
    /// directories, its name and its color; or one new tab.
    pub(super) fn restore_tabs(&mut self) -> Result<(), String> {
        let saved = std::fs::read_to_string(tabs_path()).unwrap_or_default();
        let mut active = 0;
        for line in saved.lines() {
            // active?, color, panes, name, mark — separated by tabs.
            let mut fields = line.splitn(5, '\t');
            let (Some(current), Some(color), Some(layout), name, mark) =
                (fields.next(), fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let mut focus = None;
            let root = self.restore_node(&mut layout.split(LAYOUT_SEPARATOR), &mut focus)?;
            let mut leaves = Vec::new();
            root.leaves(&mut leaves);
            self.tabs.push(Tab {
                focus: focus.unwrap_or(leaves[0]),
                root,
                zoomed: false,
                name: name.filter(|n| !n.is_empty()).map(str::to_owned),
                color: color.parse().ok().filter(|&c: &usize| c < TAB_COLORS.len()),
                shown: None,
                slide: None,
                mark: mark.and_then(Mark::from_code),
                marked_at: None,
                ai_title: AiTitle::default(),
            });
            if current == "*" {
                active = self.tabs.len() - 1;
            }
        }
        if self.tabs.is_empty() {
            return self.new_tab();
        }
        if crate::setting("restore_output").as_deref() != Some("off") {
            for (index, id) in self.saved_panes().into_iter().enumerate() {
                if let Some(pane) = self.pane(id) {
                    pane.session.restore_blocks(&format!("{}/{index}", sessions_dir()));
                }
            }
        }
        self.switch_tab(active);
        Ok(())
    }
    /// Every shell pane, tab by tab, in the order the tabs file lists them.
    pub(super) fn saved_panes(&self) -> Vec<u64> {
        let mut leaves = Vec::new();
        for tab in self.tabs.iter().filter(|tab| tab.focus != SETTINGS) {
            tab.root.leaves(&mut leaves);
        }
        leaves
    }
    /// Each pane's finished blocks, for `restore_tabs` to bring back.
    pub(super) fn save_sessions(&mut self) {
        if !self.primary {
            return;
        }
        let dir = sessions_dir();
        let _ = std::fs::remove_dir_all(&dir);
        if crate::setting("restore_output").as_deref() == Some("off") {
            return;
        }
        crate::create_private_dir(&crate::settings_dir());
        crate::create_private_dir(&dir);
        for (index, id) in self.saved_panes().into_iter().enumerate() {
            if let Some(pane) = self.pane(id) {
                pane.session.save_blocks(&format!("{dir}/{index}"));
            }
        }
    }
    /// Rebuilds one node written by `save_node`, opening its panes. A bare
    /// directory (the older format, one pane a tab) is a focused pane.
    pub(super) fn restore_node<'a>(
        &mut self,
        tokens: &mut impl Iterator<Item = &'a str>,
        focus: &mut Option<u64>,
    ) -> Result<Node, String> {
        let token = tokens.next().unwrap_or("");
        let axis = match token.get(..1) {
            Some("R") => Some(Axis::Row),
            Some("C") => Some(Axis::Column),
            _ => None,
        };
        if let (Some(axis), Ok(ratio)) = (axis, token[1..].parse::<f32>()) {
            let first = Box::new(self.restore_node(tokens, focus)?);
            let second = Box::new(self.restore_node(tokens, focus)?);
            return Ok(Node::Split { axis, ratio: ratio.clamp(0.05, 0.95), first, second });
        }
        let (focused, cwd) = match token.get(..1) {
            Some("F") => (true, &token[1..]),
            Some("P") => (false, &token[1..]),
            _ => (true, token),
        };
        let cwd = Some(cwd.to_owned()).filter(|d| std::path::Path::new(d).is_dir());
        let id = self.make_pane_in(cwd)?;
        if focused && focus.is_none() {
            *focus = Some(id);
        }
        Ok(Node::Leaf(id))
    }
    /// Writes the tabs down for `restore_tabs`.
    pub(super) fn save_tabs(&self) {
        if !self.primary {
            return;
        }
        let lines: String = self
            .tabs
            .iter()
            .enumerate()
            // The Settings tab isn't reopened, nor viewers.
            .filter(|(_, tab)| tab.focus != SETTINGS && self.has_shell(&tab.root))
            .map(|(index, tab)| {
                let mut layout = Vec::new();
                self.save_node(&tab.root, tab.focus, &mut layout);
                let layout = layout.join(&LAYOUT_SEPARATOR.to_string());
                let current = if index == self.active { "*" } else { "" };
                let color = tab.color.map(|c| c.to_string()).unwrap_or_default();
                let name = tab.name.as_deref().unwrap_or("");
                let mark = tab.mark.map_or("", Mark::code);
                format!("{current}\t{color}\t{layout}\t{name}\t{mark}\n")
            })
            .collect();
        crate::create_private_dir(&crate::settings_dir());
        crate::write_private(&tabs_path(), lines);
    }
    /// A node in prefix order: `R`/`C` and the ratio for a split, then its
    /// two sides; `F` (focused) or `P` and the directory for a pane.
    pub(super) fn save_node(&self, node: &Node, focus: u64, out: &mut Vec<String>) {
        match node {
            Node::Leaf(id) => {
                let cwd = self.panes.iter().find(|p| p.id == *id).map_or("", |p| p.session.cwd.as_str());
                out.push(format!("{}{cwd}", if *id == focus { 'F' } else { 'P' }));
            }
            Node::Split { first, second, .. } if !self.has_shell(first) => self.save_node(second, focus, out),
            Node::Split { first, second, .. } if !self.has_shell(second) => self.save_node(first, focus, out),
            Node::Split { axis, ratio, first, second } => {
                out.push(format!("{}{ratio}", if *axis == Axis::Row { 'R' } else { 'C' }));
                self.save_node(first, focus, out);
                self.save_node(second, focus, out);
            }
        }
    }
    pub(super) fn is_viewer(&self, id: u64) -> bool {
        self.viewers.iter().any(|v| v.id == id)
    }
    /// Whether a part of a tab has a shell pane in it, not only viewers.
    pub(super) fn has_shell(&self, node: &Node) -> bool {
        let mut leaves = Vec::new();
        node.leaves(&mut leaves);
        leaves.iter().any(|&id| !self.is_viewer(id))
    }
    /// Whether anything in a viewer is unsaved, warning about it the first
    /// time it's asked, so that the second Quit goes ahead.
    pub(super) fn unsaved_blocks_quit(&mut self) -> bool {
        let unsaved: Vec<&mut Viewer> = self.viewers.iter_mut().filter(|v| v.dirty && !v.close_armed).collect();
        if unsaved.is_empty() {
            return false;
        }
        let names: Vec<String> = unsaved.iter().map(|v| v.title.clone()).collect();
        for viewer in unsaved {
            viewer.close_armed = true;
        }
        let message = format!("Unsaved: {}. Quit again to discard.", names.join(", "));
        self.ctx.toast = Some((message, Instant::now() + Duration::from_secs(5)));
        self.dirty = true;
        true
    }
}

/// Where each pane's output is kept between launches.
pub(super) fn sessions_dir() -> String {
    format!("{}/sessions", crate::settings_dir())
}

pub(super) fn tabs_path() -> String {
    format!("{}/tabs", crate::settings_dir())
}

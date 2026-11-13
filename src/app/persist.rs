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
        let restore_output = crate::setting("restore_output").as_deref() != Some("off");
        for (index, id) in self.saved_panes().into_iter().enumerate() {
            if let Some(pane) = self.pane(id) {
                if restore_output {
                    pane.session.restore_blocks(&format!("{}/{index}", sessions_dir()));
                }
                pane.restore_history(&format!("{}/{index}", pane_history_dir()));
            }
        }
        // Read once: a crash before the next quit mustn't run these again
        // in panes that by then hold something else.
        let resumes = std::fs::read_to_string(resume_path()).unwrap_or_default();
        let _ = std::fs::remove_file(resume_path());
        if resume_programs() {
            let panes = self.saved_panes();
            for (index, resume) in read_resumes(&resumes) {
                if let Some(pane) = panes.get(index).copied().and_then(|id| self.pane(id)) {
                    match resume {
                        Resume::Run(line) => pane.run_when_ready(line),
                        Resume::Type(line) => pane.type_when_ready(line),
                    }
                }
            }
        }
        self.switch_tab(active);
        Ok(())
    }
    /// Every shell pane, tab by tab, in the order the tabs file lists them:
    /// viewers and tabs of viewers alone left out, as `save_tabs` leaves
    /// them out, so the numbers match the panes they reopen as.
    pub(super) fn saved_panes(&self) -> Vec<u64> {
        let mut leaves = Vec::new();
        for tab in self.tabs.iter().filter(|tab| tab.focus != SETTINGS && self.has_shell(&tab.root)) {
            tab.root.leaves(&mut leaves);
        }
        leaves.retain(|&id| !self.is_viewer(id));
        leaves
    }
    /// Each pane's finished blocks and own history, for `restore_tabs` to
    /// bring back. The history is kept even with output restore off: it's
    /// what you typed, not what programs printed.
    pub(super) fn save_sessions(&mut self) {
        if !self.primary {
            return;
        }
        let history = pane_history_dir();
        let _ = std::fs::remove_dir_all(&history);
        crate::create_private_dir(&crate::settings_dir());
        crate::create_private_dir(&history);
        for (index, id) in self.saved_panes().into_iter().enumerate() {
            if let Some(pane) = self.pane(id) {
                pane.save_history(&format!("{history}/{index}"));
            }
        }
        let resumes: Vec<(usize, Resume)> = if resume_programs() {
            let panes = self.saved_panes();
            panes.iter().enumerate().filter_map(|(index, &id)| Some((index, self.pane(id)?.resume()?))).collect()
        } else {
            Vec::new()
        };
        if resumes.is_empty() {
            let _ = std::fs::remove_file(resume_path());
        } else {
            crate::write_private(&resume_path(), write_resumes(&resumes));
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
    /// Rebuilds one node written by `save_node`, opening its panes.
    pub(super) fn restore_node<'a>(
        &mut self,
        tokens: &mut impl Iterator<Item = &'a str>,
        focus: &mut Option<u64>,
    ) -> Result<Node, String> {
        read_node(tokens, focus, &mut |cwd| self.make_pane_in(cwd))
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
    /// Writes one node down for `restore_node`, see `write_node`.
    pub(super) fn save_node(&self, node: &Node, focus: u64, out: &mut Vec<String>) {
        let cwd = |id: u64| self.panes.iter().find(|p| p.id == id).map_or("", |p| p.session.cwd.as_str());
        write_node(node, focus, &|id| self.is_viewer(id), &cwd, out);
    }
    pub(super) fn is_viewer(&self, id: u64) -> bool {
        self.viewers.iter().any(|v| v.id == id)
    }
    /// Whether a part of a tab has a shell pane in it, not only viewers.
    pub(super) fn has_shell(&self, node: &Node) -> bool {
        node_has_shell(node, &|id| self.is_viewer(id))
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

/// Rebuilds one node written by `write_node`, opening each pane with
/// `open` in its directory (None when that's gone). A bare directory (the
/// older format, one pane a tab) is a focused pane.
pub(super) fn read_node<'a>(
    tokens: &mut impl Iterator<Item = &'a str>,
    focus: &mut Option<u64>,
    open: &mut impl FnMut(Option<String>) -> Result<u64, String>,
) -> Result<Node, String> {
    let token = tokens.next().unwrap_or("");
    let axis = match token.get(..1) {
        Some("R") => Some(Axis::Row),
        Some("C") => Some(Axis::Column),
        _ => None,
    };
    if let (Some(axis), Ok(ratio)) = (axis, token.get(1..).unwrap_or("").parse::<f32>()) {
        let first = Box::new(read_node(tokens, focus, open)?);
        let second = Box::new(read_node(tokens, focus, open)?);
        return Ok(Node::Split { axis, ratio: ratio.clamp(0.05, 0.95), first, second });
    }
    let (focused, cwd) = match token.get(..1) {
        Some("F") => (true, token.get(1..).unwrap_or("")),
        Some("P") => (false, token.get(1..).unwrap_or("")),
        _ => (true, token),
    };
    let cwd = Some(cwd.to_owned()).filter(|d| std::path::Path::new(d).is_dir());
    let id = open(cwd)?;
    if focused && focus.is_none() {
        *focus = Some(id);
    }
    Ok(Node::Leaf(id))
}

/// A node in prefix order: `R`/`C` and the ratio for a split, then its
/// two sides; `F` (focused) or `P` and the directory (`cwd`) for a pane.
/// Viewers aren't written; a split with one on a side is its other side.
pub(super) fn write_node<'a>(
    node: &Node,
    focus: u64,
    is_viewer: &impl Fn(u64) -> bool,
    cwd: &impl Fn(u64) -> &'a str,
    out: &mut Vec<String>,
) {
    match node {
        Node::Leaf(id) => out.push(format!("{}{}", if *id == focus { 'F' } else { 'P' }, cwd(*id))),
        Node::Split { first, second, .. } if !node_has_shell(first, is_viewer) => {
            write_node(second, focus, is_viewer, cwd, out)
        }
        Node::Split { first, second, .. } if !node_has_shell(second, is_viewer) => {
            write_node(first, focus, is_viewer, cwd, out)
        }
        Node::Split { axis, ratio, first, second } => {
            out.push(format!("{}{ratio}", if *axis == Axis::Row { 'R' } else { 'C' }));
            write_node(first, focus, is_viewer, cwd, out);
            write_node(second, focus, is_viewer, cwd, out);
        }
    }
}

/// Whether a part of a tab has a shell pane in it, not only viewers.
pub(super) fn node_has_shell(node: &Node, is_viewer: &impl Fn(u64) -> bool) -> bool {
    let mut leaves = Vec::new();
    node.leaves(&mut leaves);
    leaves.iter().any(|&id| !is_viewer(id))
}

/// Settings ▸ Terminal ▸ Reopen running programs: off unless turned on.
pub(super) fn resume_programs() -> bool {
    crate::setting("resume_programs").as_deref() == Some("on")
}

/// What each pane was running at the last quit, for `restore_tabs`.
pub(super) fn resume_path() -> String {
    format!("{}/resume", crate::settings_dir())
}

/// A line a pane: its number in `saved_panes`, `R` (run) or `T` (type),
/// and the command line.
pub(super) fn write_resumes(resumes: &[(usize, Resume)]) -> String {
    resumes
        .iter()
        .map(|(index, resume)| match resume {
            Resume::Run(line) => format!("{index}\tR\t{line}\n"),
            Resume::Type(line) => format!("{index}\tT\t{line}\n"),
        })
        .collect()
}

pub(super) fn read_resumes(text: &str) -> Vec<(usize, Resume)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let index = fields.next()?.parse().ok()?;
            let kind = fields.next()?;
            let line = fields.next().filter(|line| !line.trim().is_empty())?.to_owned();
            match kind {
                "R" => Some((index, Resume::Run(line))),
                "T" => Some((index, Resume::Type(line))),
                _ => None,
            }
        })
        .collect()
}

/// Where each pane's output is kept between launches.
pub(super) fn sessions_dir() -> String {
    format!("{}/sessions", crate::settings_dir())
}

/// Where each pane's own Up/Down history is kept between launches.
pub(super) fn pane_history_dir() -> String {
    format!("{}/pane_history", crate::settings_dir())
}

pub(super) fn tabs_path() -> String {
    format!("{}/tabs", crate::settings_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(node: &Node) -> String {
        match node {
            Node::Leaf(id) => id.to_string(),
            Node::Split { axis, ratio, first, second } => {
                let axis = if *axis == Axis::Row { 'R' } else { 'C' };
                format!("{axis}{ratio}({},{})", shape(first), shape(second))
            }
        }
    }

    fn split(axis: Axis, ratio: f32, first: Node, second: Node) -> Node {
        Node::Split { axis, ratio, first: Box::new(first), second: Box::new(second) }
    }

    /// A tab's layout field as `save_tabs` writes it.
    fn save(node: &Node, focus: u64, viewers: &[u64], cwd: &dyn Fn(u64) -> String) -> String {
        let mut ids = Vec::new();
        node.leaves(&mut ids);
        let cwds: Vec<(u64, String)> = ids.into_iter().map(|id| (id, cwd(id))).collect();
        let lookup = |id: u64| cwds.iter().find(|(i, _)| *i == id).map_or("", |(_, d)| d.as_str());
        let mut out = Vec::new();
        write_node(node, focus, &|id| viewers.contains(&id), &lookup, &mut out);
        out.join(&LAYOUT_SEPARATOR.to_string())
    }

    /// Reads a layout field back as `restore_tabs` does, numbering the new
    /// panes from 100 and noting the directory each was opened in.
    fn restore(layout: &str) -> (Result<Node, String>, Option<u64>, Vec<Option<String>>) {
        let mut opened = Vec::new();
        let mut focus = None;
        let node = read_node(&mut layout.split(LAYOUT_SEPARATOR), &mut focus, &mut |cwd| {
            opened.push(cwd);
            Ok(99 + opened.len() as u64)
        });
        (node, focus, opened)
    }

    /// Real directories for panes to be in, under the temp dir.
    fn dirs(test: &str, count: usize) -> Vec<String> {
        let base = std::env::temp_dir().join(format!("temino-persist-{test}-{}", std::process::id()));
        (0..count)
            .map(|i| {
                let dir = base.join(format!("pane {i}"));
                std::fs::create_dir_all(&dir).unwrap();
                dir.to_str().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn panes_are_written_in_prefix_order_with_ratios_and_focus() {
        let root = split(Axis::Row, 0.3, Node::Leaf(1), split(Axis::Column, 0.7, Node::Leaf(2), Node::Leaf(3)));
        let layout = save(&root, 2, &[], &|id| format!("/dir{id}"));
        assert_eq!(layout, "R0.3\u{1f}P/dir1\u{1f}C0.7\u{1f}F/dir2\u{1f}P/dir3");
        // A pane whose directory isn't known yet is still written.
        assert_eq!(save(&Node::Leaf(4), 4, &[], &|_| String::new()), "F");
    }

    #[test]
    fn a_split_tab_comes_back_with_its_layout_directories_and_focus() {
        let dirs = dirs("roundtrip", 4);
        // (1 over 2) | (3 | 4), focus on 3.
        let root = split(
            Axis::Row,
            0.25,
            split(Axis::Column, 0.6, Node::Leaf(1), Node::Leaf(2)),
            split(Axis::Row, 0.5, Node::Leaf(3), Node::Leaf(4)),
        );
        let layout = save(&root, 3, &[], &|id| dirs.get(id as usize - 1).cloned().unwrap_or_default());
        let (node, focus, opened) = restore(&layout);
        let node = node.unwrap();
        assert_eq!(shape(&node), "R0.25(C0.6(100,101),R0.5(102,103))");
        assert_eq!(focus, Some(102));
        assert_eq!(opened, dirs.iter().cloned().map(Some).collect::<Vec<_>>());
        // And it writes the same again.
        let again = save(&node, 102, &[], &|id| dirs.get(id.wrapping_sub(100) as usize).cloned().unwrap_or_default());
        assert_eq!(again, layout);
    }

    #[test]
    fn viewers_are_left_out_and_their_sibling_takes_the_split() {
        // 1 | (viewer 2 over (3 | viewer 4)): only panes 1 and 3 are shells.
        let root = split(
            Axis::Row,
            0.4,
            Node::Leaf(1),
            split(Axis::Column, 0.5, Node::Leaf(2), split(Axis::Row, 0.5, Node::Leaf(3), Node::Leaf(4))),
        );
        let layout = save(&root, 1, &[2, 4], &|id| format!("/d{id}"));
        assert_eq!(layout, "R0.4\u{1f}F/d1\u{1f}P/d3");
        // A branch of viewers alone drops out whole.
        let root = split(Axis::Column, 0.5, split(Axis::Row, 0.5, Node::Leaf(5), Node::Leaf(6)), Node::Leaf(7));
        assert_eq!(save(&root, 5, &[5, 6], &|id| format!("/d{id}")), "P/d7");
        let is_viewer = |id: u64| [5, 6].contains(&id);
        assert!(node_has_shell(&root, &is_viewer));
        assert!(!node_has_shell(&split(Axis::Row, 0.5, Node::Leaf(5), Node::Leaf(6)), &is_viewer));
    }

    #[test]
    fn a_pane_whose_directory_is_gone_opens_in_the_default_one() {
        let dirs = dirs("gone", 1);
        let missing = format!("{}/nope", dirs[0]);
        let (node, focus, opened) = restore(&format!("C0.5\u{1f}P{missing}\u{1f}F{}", dirs[0]));
        assert_eq!(shape(&node.unwrap()), "C0.5(100,101)");
        assert_eq!(opened, [None, Some(dirs[0].clone())]);
        assert_eq!(focus, Some(101));
        // So does a pane saved before its directory was known.
        let (_, _, opened) = restore("F");
        assert_eq!(opened, [None]);
    }

    #[test]
    fn a_tab_from_the_one_pane_format_is_a_focused_pane() {
        let dirs = dirs("old", 1);
        let (node, focus, opened) = restore(&dirs[0]);
        assert_eq!(shape(&node.unwrap()), "100");
        assert_eq!(focus, Some(100));
        assert_eq!(opened, [Some(dirs[0].clone())]);
    }

    #[test]
    fn focus_goes_to_the_first_focused_pane_or_none() {
        let (_, focus, _) = restore("R0.5\u{1f}P\u{1f}P");
        assert_eq!(focus, None);
        let (_, focus, _) = restore("R0.5\u{1f}P\u{1f}C0.5\u{1f}F\u{1f}F");
        assert_eq!(focus, Some(101));
    }

    #[test]
    fn ratios_out_of_range_are_brought_back_in() {
        let (node, _, _) = restore("R7\u{1f}P\u{1f}C-1\u{1f}P\u{1f}P");
        assert_eq!(shape(&node.unwrap()), "R0.95(100,C0.05(101,102))");
        // A split letter without a number is a pane in that directory.
        let (node, _, opened) = restore("Rx");
        assert_eq!(shape(&node.unwrap()), "100");
        assert_eq!(opened, [None]);
    }

    #[test]
    fn a_pane_that_cant_open_fails_the_restore() {
        let mut focus = None;
        let mut tokens = "R0.5\u{1f}F\u{1f}P".split(LAYOUT_SEPARATOR);
        let mut count = 0;
        let result = read_node(&mut tokens, &mut focus, &mut |_| {
            count += 1;
            if count == 2 {
                Err("no pty".to_owned())
            } else {
                Ok(count)
            }
        });
        assert_eq!(result.err().as_deref(), Some("no pty"));
    }

    #[test]
    fn what_panes_ran_is_written_and_read_back() {
        let resumes = vec![
            (0, Resume::Run("claude-max --resume 1234-abcd".to_owned())),
            (3, Resume::Type("npm run dev -- --port 3000".to_owned())),
        ];
        let text = write_resumes(&resumes);
        assert_eq!(text, "0\tR\tclaude-max --resume 1234-abcd\n3\tT\tnpm run dev -- --port 3000\n");
        assert_eq!(read_resumes(&text), resumes);
        // Damaged or unknown lines are skipped, not run.
        assert_eq!(
            read_resumes("x\tR\tls\n1\tZ\tls\n2\tR\t \n4\tT\tvim a\tb\n"),
            [(4, Resume::Type("vim a\tb".to_owned()))]
        );
    }

    #[test]
    fn a_damaged_layout_opens_a_pane_instead_of_crashing() {
        // An empty layout field (`*\t\t\tname`), and a split cut off before its second side.
        let (node, _, _) = restore("");
        assert_eq!(shape(&node.unwrap()), "100");
        let (node, _, _) = restore("R0.5\u{1f}F");
        assert_eq!(shape(&node.unwrap()), "R0.5(100,101)");
    }
}

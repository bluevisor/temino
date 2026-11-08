use super::*;

impl App {
    /// Opens a clicked link beside pane `from`: in a viewer, sliding in
    /// from the right, or in the system's app for what no viewer shows.
    /// The tab's viewer of the same kind shows it instead, if nothing in
    /// it is unsaved.
    pub(super) fn open_link(&mut self, from: u64, url: String) {
        let Some((kind, target)) = viewer::classify(&url).filter(|_| platform::VIEWERS) else {
            platform::open_url(&url);
            return;
        };
        let style = self.viewer_style();
        let mut leaves = Vec::new();
        self.tabs[self.active].root.leaves(&mut leaves);
        if let Some(viewer) = self.viewers.iter_mut().find(|v| v.kind == kind && !v.dirty && leaves.contains(&v.id)) {
            platform::viewer_open(viewer.id, kind, &target, style);
            viewer.retarget(target);
            let id = viewer.id;
            self.set_focus(id);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        platform::viewer_open(id, kind, &target, style);
        self.viewers.push(Viewer::new(id, kind, target));
        let tab = &mut self.tabs[self.active];
        tab.root.split(from, id, Axis::Row, false);
        tab.zoomed = false;
        self.set_focus(id);
    }
    /// This window as remotes see it: its tabs and their panes, what each
    /// agent is doing, and the screens of the panes a remote watches.
    pub(super) fn remote_state(&mut self) -> (String, std::collections::HashMap<String, String>) {
        let no = platform::window_no();
        let agents: Vec<Option<AgentState>> = self
            .tabs
            .iter()
            .map(|tab| {
                let mut leaves = Vec::new();
                tab.root.leaves(&mut leaves);
                self.panes.iter().filter(|p| leaves.contains(&p.id)).filter_map(|p| p.agent_now).max()
            })
            .collect();
        // Watched panes' screens first: they need the context (history,
        // commands) the outline below doesn't.
        let mut screens = std::collections::HashMap::new();
        let mut lines = std::collections::HashMap::new();
        for pane in self.panes.iter_mut().filter(|pane| remote::watching(no, pane.id)) {
            let prefix = format!("{no}:{}:", pane.id);
            screens.insert(pane.id, pane.remote_screen(&mut self.ctx, &prefix, &mut lines));
        }
        // Settings has no terminal to drive, so phones don't see it.
        let shown: Vec<usize> = (0..self.tabs.len()).filter(|&index| self.tabs[index].focus != SETTINGS).collect();
        let tabs: Vec<String> = shown
            .into_iter()
            .map(|index| {
                let tab = &self.tabs[index];
                let mut leaves = Vec::new();
                tab.root.leaves(&mut leaves);
                let panes: Vec<String> = leaves
                    .iter()
                    .filter_map(|&id| {
                        let pane = self.panes.iter_mut().find(|p| p.id == id)?;
                        let screen = screens.get(&id).map_or(String::new(), |screen| format!(r#","screen":{screen}"#));
                        Some(format!(
                            r#"{{"id":{id},"title":{},"cwd":{},"agent":{}{screen}}}"#,
                            remote::quote(&pane.title()),
                            remote::quote(&pane.session.cwd),
                            agent_name(pane.agent_now),
                        ))
                    })
                    .collect();
                format!(
                    r#"{{"index":{index},"title":{},"color":{},"agent":{},"focus":{},"panes":[{}]}}"#,
                    remote::quote(&self.tab_title(index)),
                    tab.color.map_or("null".to_owned(), |c| format!("\"#{:06x}\"", TAB_COLORS[c])),
                    agent_name(agents[index]),
                    tab.focus,
                    panes.join(","),
                )
            })
            .collect();
        let t = &self.ctx.theme;
        let palette = format!(
            r#"{{"background":"{}","surface":"{}","raised":"{}","divider":"{}","text":"{}","dim":"{}","faint":"{}","accent":"{}","success":"{}","error":"{}","warning":"{}","string":"{}","variable":"{}","cursor":"{}","selection":"{}"}}"#,
            t.background.css(),
            t.surface.css(),
            t.raised.css(),
            t.divider.css(),
            t.text.css(),
            t.text_dim.css(),
            t.text_faint.css(),
            t.accent.css(),
            t.success.css(),
            t.error.css(),
            t.warning.css(),
            t.string.css(),
            t.variable.css(),
            t.cursor.css(),
            t.selection.css(),
        );
        // The shells a new tab can start in, the usual one first.
        let usual = shells::Kind::of_path(&self.ctx.shell);
        let mut kinds: Vec<&str> = shells::KINDS
            .into_iter()
            .filter(|&kind| Some(kind) != usual && shells::find(kind).is_some())
            .map(shells::Kind::name)
            .collect();
        if let Some(usual) = usual {
            kinds.insert(0, usual.name());
        }
        let shells: Vec<String> = kinds.iter().map(|name| format!("\"{name}\"")).collect();
        let state = format!(
            r#"{{"no":{no},"title":{},"active":{},"theme":{palette},"shells":[{}],"tabs":[{}]}}"#,
            remote::quote(&self.title),
            self.active,
            shells.join(","),
            tabs.join(",")
        );
        (state, lines)
    }
    /// Carries out what a remote asked, as this window's own keyboard
    /// would have.
    pub(super) fn remote_command(&mut self, command: remote::Command) {
        self.dirty = true;
        // The pane's tab comes forward and the pane takes the keyboard.
        let focus = |app: &mut App, id: u64| -> bool {
            let Some(index) = app.tabs.iter().position(|tab| {
                let mut leaves = Vec::new();
                tab.root.leaves(&mut leaves);
                leaves.contains(&id)
            }) else {
                return false;
            };
            if index != app.active {
                app.switch_tab(index);
            }
            if app.focused_id() != Some(id) {
                app.set_focus(id);
            }
            true
        };
        match command {
            remote::Command::Tab(index) => self.switch_tab(index),
            remote::Command::Focus(id) => {
                focus(self, id);
            }
            remote::Command::Text(id, text) => {
                if focus(self, id) {
                    self.ui_event(UiEvent::Text(text));
                }
            }
            remote::Command::Key(id, key) => {
                if focus(self, id) {
                    self.ui_event(UiEvent::Key(key));
                }
            }
            remote::Command::NewTab(kind) => {
                // Another shell for just this tab: the usual one stays.
                let other = kind.and_then(shells::find);
                let usual = other.map(|path| std::mem::replace(&mut self.ctx.shell, path));
                if let Err(error) = self.new_tab() {
                    self.ctx.show_toast(&error);
                }
                if let Some(usual) = usual {
                    self.ctx.shell = usual;
                }
            }
            remote::Command::CloseTab(index) => {
                if index < self.tabs.len() {
                    self.close_tab(index);
                }
            }
            remote::Command::Refresh => self.remote_at = Instant::now(),
            remote::Command::Ask { device, name, code } => {
                self.asks.retain(|(d, _, _)| *d != device);
                self.asks.push((device, name, code));
                platform::beep();
            }
            remote::Command::Settled(device) => self.asks.retain(|(d, _, _)| *d != device),
            remote::Command::NewWindow => {
                let cwd = self.focused_id().and_then(|id| self.panes.iter().find(|p| p.id == id));
                platform::new_window(cwd.map(|pane| pane.session.cwd.clone()));
            }
        }
    }
    /// The theme, for a viewer's native view: its see-through colors
    /// flattened onto the background.
    pub(super) fn viewer_style(&self) -> platform::ViewerStyle {
        let theme = &self.ctx.theme;
        let background = theme.background;
        let rgb = |c: Color| {
            let flat = background.mix(Color { a: 1.0, ..c }, c.a);
            [flat.r, flat.g, flat.b]
        };
        let luma = 0.2126 * background.r + 0.7152 * background.g + 0.0722 * background.b;
        platform::ViewerStyle {
            background: [background.r, background.g, background.b],
            text: rgb(theme.text),
            accent: rgb(theme.accent),
            selection: rgb(theme.selection),
            header: rgb(theme.panel(SURFACE_LIFT)),
            divider: rgb(theme.divider),
            dim: rgb(theme.text_dim),
            faint: rgb(theme.text_faint),
            font_size: self.ctx.text.cell.size * self.zoom,
            dark: luma < 0.5,
        }
    }
    pub(super) fn viewer_event(&mut self, id: u64, event: platform::ViewerEvent) {
        use platform::ViewerEvent;
        let Some(viewer) = self.viewers.iter_mut().find(|v| v.id == id) else { return };
        match event {
            ViewerEvent::Focused => {
                let here = self.tabs.get(self.active).is_some_and(|tab| tab.root.contains(id));
                if here && self.focused_id() != Some(id) {
                    self.set_focus(id);
                }
            }
            ViewerEvent::Title(title) => viewer.title = title,
            ViewerEvent::Url(url) => viewer.target = url,
            ViewerEvent::Dirty(dirty) => {
                viewer.dirty = dirty;
                viewer.close_armed = false;
            }
            ViewerEvent::Failed(error) => {
                self.ctx.toast = Some((error, Instant::now() + Duration::from_secs(4)));
            }
            ViewerEvent::Close => self.close_pane(id),
        }
        self.dirty = true;
    }
    /// A pane someone is in from a phone: an outline in the accent color,
    /// and at its top right who it is, with their photo.
    pub(super) fn draw_remote_marks(&mut self, visible: &[(u64, Rect)]) {
        let no = platform::window_no();
        let Self { scene, ctx, .. } = self;
        for &(id, rect) in visible {
            let people = remote::in_control(no, id);
            if !people.is_empty() {
                draw_people(scene, &mut ctx.text, &ctx.theme, rect, &people);
            }
        }
    }
    /// Where each viewer's native view goes (hidden when its tab isn't
    /// showing). It slides itself in, header and all.
    pub(super) fn draw_viewers(&mut self, visible: &[(u64, Rect)]) {
        let zoom = self.zoom;
        let placed: Vec<_> = self
            .viewers
            .iter()
            .map(|viewer| match visible.iter().find(|(id, _)| *id == viewer.id) {
                Some(&(_, r)) => {
                    // What shows around it while it slides in.
                    self.scene.fill(r, self.ctx.theme.background);
                    (viewer.id, [r.x * zoom, r.y * zoom, r.w * zoom, r.h * zoom], true)
                }
                None => (viewer.id, [0.0; 4], false),
            })
            .collect();
        if placed != self.viewers_placed {
            platform::viewer_place(placed.clone());
            self.viewers_placed = placed;
        }
    }
}

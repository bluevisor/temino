use super::*;

impl App {
    pub(super) fn render(&mut self) {
        crate::trace("render: begin");
        self.preview_theme();
        self.dirty = false;
        let (width, height) = (self.width, self.height);
        self.scene.reset(width, height, self.scale * self.zoom, self.ctx.theme.window());
        self.ctx.composing = !self.preedit.is_empty();
        let agents = self.refresh_agents();
        let focus = self.focused_id();
        let visible = self.visible_panes();
        let several = visible.len() > 1;
        let mut animation: Option<Instant> = None;
        for &(id, rect) in &visible {
            if id == SETTINGS {
                let values = self.settings_values();
                let Self { settings, scene, ctx, mouse, .. } = self;
                settings.draw(scene, &mut ctx.text, &ctx.theme, rect, &values, *mouse);
                if std::mem::take(&mut settings.redraw) {
                    self.dirty = true;
                }
            }
        }
        for (id, _) in &visible {
            let focused = Some(*id) == focus && self.focused;
            let Self { panes, ctx, scene, .. } = self;
            let Some(pane) = panes.iter_mut().find(|p| p.id == *id) else { continue };
            pane.draw(scene, ctx, focused);
            if let Some(at) = pane.animation_deadline(ctx, focused) {
                animation = Some(animation.map_or(at, |a| a.min(at)));
            }
            if several && Some(*id) != focus {
                // Inactive panes recede a little, as in Warp.
                scene.set_layer(LAYER_OVERLAY);
                scene.fill(pane.rect, ctx.theme.background.alpha(0.35));
                scene.set_layer(LAYER_CONTENT);
            }
        }
        self.animation_at = animation;
        self.draw_viewers(&visible);
        self.wake_for_agents(&agents);
        self.pane_hits.clear();
        self.draw_agent_headers(&visible);
        self.draw_remote_marks(&visible);
        for divider in &self.dividers {
            self.scene.set_layer(LAYER_OVERLAY);
            self.scene.fill(divider.line, self.ctx.theme.divider.mix(self.ctx.theme.text_faint, 0.35));
            self.scene.set_layer(LAYER_CONTENT);
        }
        self.draw_tab_bar(&agents);
        self.draw_titlebar();
        self.draw_update();
        self.draw_asks();
        self.draw_tab_menu();
        self.draw_toast();
        self.draw_preedit();
        self.report_caret();
        crate::trace("render: scene built");
        let capture = std::env::var_os("TEMINO_CAPTURE");
        let on_screen = self.gpu.on_screen();
        match self.gpu.render(&self.scene, &mut self.ctx.text.atlas, &mut self.ctx.text.color_atlas, capture.is_some())
        {
            Ok(Some(pixels)) => {
                let (pw, ph) = self.scene.pixel_size();
                if let Some(path) = &capture {
                    write_ppm(path, pixels, pw, ph);
                }
                if !on_screen {
                    platform::present(pixels, pw, ph)
                }
            }
            // Already in the window (or skipped: no drawable came free).
            Ok(None) => {}
            Err(error) => {
                crate::fatal(&format!("rendering failed: {error}"));
                self.quit();
            }
        }
    }
    /// The bar over each pane with an agent: what it's doing and for how
    /// long, what you last asked it, its subagents and its session. A
    /// listed program gets its name and how long it has run; any other
    /// terminal (headers on) its running command, or its folder at rest.
    pub(super) fn draw_agent_headers(&mut self, visible: &[(u64, Rect)]) {
        self.header_hits.clear();
        if self.headers == Headers::Off {
            return;
        }
        let now = Instant::now();
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let focus = self.focused_id();
        let sender = self.ctx.sender.clone();
        for &(id, rect) in visible {
            let Some(index) = self.panes.iter().position(|pane| pane.id == id) else { continue };
            if !self.has_header(&self.panes[index]) || rect.h <= 2.0 * AGENT_HEADER {
                continue;
            }
            let pane = &mut self.panes[index];
            let info = pane.agent_info(now);
            let program = pane.program_now.clone();
            let shell = (info.is_none() && program.is_none()).then(|| {
                pane.refresh_git(&sender, now);
                let (busy, running) = pane.activity();
                ShellHeader {
                    busy,
                    failed: pane.last_failed(),
                    path: ui::short_path(&pane.session.cwd),
                    running: busy.then_some(running),
                    detail: if busy { String::new() } else { pane.idle_line(&pane.session.cwd, now) },
                }
            });
            let bar = Rect::new(rect.x, rect.y - AGENT_HEADER, rect.w, AGENT_HEADER);
            let hover = self.header_hover == Some(id);
            let Self { scene, ctx, pane_hits, header_hits, .. } = self;
            let Ctx { text, theme, .. } = ctx;
            match (info, program, shell) {
                (Some(info), _, _) => {
                    // Extended, it reaches over the top of the pane, at most
                    // most of the way down.
                    let room = (rect.h * 0.6).floor();
                    let reach = draw_agent_header(scene, text, theme, bar, &info, Some(id) == focus, hover, room, t);
                    header_hits.push((Rect::new(bar.x, bar.y, bar.w, bar.h + reach), id));
                }
                (None, Some((name, started)), _) => {
                    draw_program_header(scene, text, theme, bar, &name, now.duration_since(started))
                }
                (None, None, Some(shell)) => draw_shell_header(scene, text, theme, bar, &shell),
                (None, None, None) => {}
            }
            pane_hits.push((bar, self.active, id));
        }
    }
    /// The title bar over the window's top, where the system's would be.
    /// With the full sidebar, as in cmux, the sidebar runs from the very
    /// top (the traffic lights and the title sit in it) and the bar spans
    /// only the right side, on its background, with the tab's AI summary. Otherwise
    /// (compact, too narrow for the lights; the strip; no list) it goes
    /// across in the sidebar's color. AppKit keeps the traffic lights and
    /// the dragging; this is only the look.
    pub(super) fn draw_titlebar(&mut self) {
        let top = self.top();
        if top <= 0.0 {
            return;
        }
        let side = self.tab_area();
        let column = self.strip == TabStrip::Left && !self.compact && side.w > 0.0;
        let bar = if column { Rect::new(side.w, 0.0, self.width - side.w, top) } else { Rect::new(0.0, 0.0, self.width, top) };
        let summary = self.tabs.get(self.active).and_then(|tab| tab.ai_title.summary.clone());
        let Self { scene, ctx, title, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        if column {
            scene.fill(Rect::new(0.0, 0.0, side.w, top), theme.panel(SURFACE_LIFT / 2.0));
            scene.fill(Rect::new(side.w - 1.0, 0.0, 1.0, top), theme.divider);
            scene.fill(bar, theme.background);
        } else {
            scene.fill(bar, theme.panel(SURFACE_LIFT / 2.0));
        }
        scene.fill(Rect::new(bar.x, top - 1.0, bar.w, 1.0), theme.divider);
        // Fixed in points, like the bar: zoom leaves it alone. With the
        // sidebar, the title sits at the top of the tabs, beside the traffic
        // lights; the bar over the panes is left to drag the window by.
        // Otherwise it's centered clear of the lights.
        let lights = 84.0 / self.zoom;
        let baseline = |font: f32, text: &TextSystem| (top / 2.0 + text.ui_ascent(font) * 0.36).round();
        if column {
            // The title, without the app's name: the window is Temino's.
            let font = 13.0 / self.zoom;
            let room = (side.w - lights - 10.0).max(0.0);
            let name = title.strip_suffix(" — Temino").unwrap_or(title);
            let shown = text.elide(name, UI_BOLD, font, room);
            let y = baseline(font, text);
            text.draw_text(scene, &shown, lights - 6.0 / self.zoom, y, UI_BOLD, font, theme.text_dim);
            // Over the panes: the model's summary of what goes on in the tab.
            if let Some(summary) = summary {
                let font = 13.0 / self.zoom;
                let line = format!("✦ {summary}");
                let shown = text.elide(&line, UI, font, (bar.w - 32.0 / self.zoom).max(0.0));
                let w = text.measure(&shown, UI, font);
                let y = baseline(font, text);
                text.draw_text(scene, &shown, bar.x + (bar.w - w) / 2.0, y, UI, font, theme.text_dim);
            }
            return;
        }
        let font = 18.0 / self.zoom;
        let room = (bar.w - 2.0 * lights).max(0.0);
        let shown = text.elide(title, UI_BOLD, font, room);
        let w = text.measure(&shown, UI_BOLD, font);
        let y = baseline(font, text);
        text.draw_text(scene, &shown, bar.x + (bar.w - w) / 2.0, y, UI_BOLD, font, theme.text_dim);
    }
    pub(super) fn draw_tab_bar(&mut self, agents: &[Option<AgentState>]) {
        self.tab_hits.clear();
        self.close_hits.clear();
        self.new_tab_hit = Rect::default();
        self.collapse_hit = Rect::default();
        self.account_hit = Rect::default();
        if !self.tab_list_visible() {
            return;
        }
        let now = Instant::now();
        self.refresh_titles(now);
        let extended = self.extended && !self.compact && self.strip == TabStrip::Left;
        let tabs: Vec<TabView> = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                // Only the current tab grows its lines; the rest stay one row.
                let lines =
                    if extended && index == self.active { self.pane_lines(index, &leaves, now) } else { Vec::new() };
                let done_for = self
                    .panes
                    .iter()
                    .filter(|pane| leaves.contains(&pane.id))
                    .filter_map(|pane| match pane.needs_since {
                        Some((AgentState::Done, at)) => Some(now.duration_since(at).as_secs_f32()),
                        _ => None,
                    })
                    .reduce(f32::min)
                    .unwrap_or(f32::MAX);
                TabView {
                    lines,
                    agent: agents[index],
                    done_for,
                    title: self.tab_title(index),
                    path: self
                        .panes
                        .iter()
                        .find(|pane| pane.id == self.tabs[index].focus)
                        .map(|pane| ui::short_path(&pane.session.cwd))
                        .unwrap_or_default(),
                    panes: leaves.len(),
                    color: self.tabs[index].color.map(|c| Color::hex(TAB_COLORS[c])),
                    mark: self.tabs[index].mark,
                    editing: self
                        .renaming
                        .as_ref()
                        .filter(|rename| rename.tab == index)
                        .map(|rename| (rename.name.clone(), rename.selected)),
                }
            })
            .collect();
        match self.strip {
            TabStrip::Left => self.draw_tab_sidebar(&tabs),
            TabStrip::Top => self.draw_tab_strip(&tabs),
        }
    }
    /// What each of `leaves` is doing, a line apiece: an agent's state, turn
    /// and last prompt; a shell's running command, or its folder.
    pub(super) fn pane_lines(&mut self, index: usize, leaves: &[u64], now: Instant) -> Vec<PaneLine> {
        let sender = self.ctx.sender.clone();
        for pane in self.panes.iter_mut().filter(|pane| leaves.contains(&pane.id)) {
            pane.refresh_git(&sender, now);
        }
        // Idle lines say the folder only where it isn't the tab's own.
        let focus = self.tabs[index].focus;
        let base = self.panes.iter().find(|pane| pane.id == focus).map(|pane| pane.session.cwd.clone());
        let lines: Vec<PaneLine> = leaves
            .iter()
            .filter_map(|&id| {
                if id == SETTINGS {
                    return Some(PaneLine { id, agent: None, busy: false, failed: false, text: "Settings".to_owned() });
                }
                let pane = self.panes.iter().find(|pane| pane.id == id)?;
                if let Some(info) = pane.agent_info(now) {
                    let mut text = agent_summary(&info);
                    if let Some(prompt) = &info.prompt {
                        text = format!("{text} · {prompt}");
                    }
                    return Some(PaneLine { id, agent: Some(info.state), busy: true, failed: false, text });
                }
                let (busy, text) = pane.activity();
                if busy {
                    return Some(PaneLine { id, agent: None, busy, failed: false, text });
                }
                let text = pane.idle_line(base.as_deref().unwrap_or(&pane.session.cwd), now);
                Some(PaneLine { id, agent: None, busy, failed: pane.last_failed(), text })
            })
            .collect();
        // A lone idle shell: its title says it all.
        if let [line] = lines.as_slice() {
            if !line.busy && line.agent.is_none() {
                return Vec::new();
            }
        }
        // Idle lines tell how long ago; read them again as the minutes turn.
        if lines.iter().any(|line| !line.busy) {
            let at = now + Duration::from_secs(30);
            self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
        }
        lines
    }
    /// One tab's row: number, title (or the rename field), an optional
    /// badge, and × while hovered (its menu is a right-click); records the
    /// hit areas.
    pub(super) fn draw_tab(&mut self, index: usize, tab: &TabView, rect: Rect, badge: Option<String>) {
        let current = index == self.active;
        let hovered = self.hover_tab == Some(index);
        let left = self.strip == TabStrip::Left;
        let (font, number) = if left { (SIDEBAR_FONT, 22.0) } else { (12.0, 18.0) };
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let Self { scene, ctx, tab_hits, close_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(font);
        let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
        let mut x = rect.x + if left { 10.0 } else { 8.0 };
        if index < 9 || tab.agent.is_some() {
            let center = (x + number / 2.0, rect.y + rect.h / 2.0);
            draw_tab_number(scene, text, theme, index, center, number, tab.agent, tab.done_for, t);
            x += number + 8.0;
        }
        let close = Rect::new(rect.right() - 22.0, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
        let closable = hovered && tab.editing.is_none();
        let mut right = if closable { close.x - 4.0 } else { rect.right() - 8.0 };
        if let Some(label) = badge {
            // A small badge with the pane count.
            let w = text.measure(&label, UI, 10.0) + 10.0;
            let badge = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
            scene.quad(Quad::new(badge, theme.divider).radius(8.0));
            text.draw_text(scene, &label, badge.x + 5.0, badge.y + 12.0, UI, 10.0, theme.text_dim);
            right = badge.x - 6.0;
        }
        // In the sidebar the mark goes under the title (draw_tab_meta).
        if let Some(mark) = tab.mark.filter(|_| !left) {
            let ink = mark.color(theme);
            let w = text.measure(mark.label(), UI_BOLD, 10.0) + 12.0;
            let chip = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
            scene.quad(Quad::new(chip, ink.alpha(0.16)).radius(8.0).border(1.0, ink.alpha(0.45)));
            text.draw_text(scene, mark.label(), chip.x + 6.0, chip.y + 12.0, UI_BOLD, 10.0, ink);
            right = chip.x - 6.0;
        }
        if let Some((name, selected)) = &tab.editing {
            // Just taller than the text, centered on it.
            let h = (font + 6.0).round();
            let top = (baseline - ascent * 0.36 - h / 2.0).round();
            let field = Rect::new(x - 3.0, top, (close.x - x - 1.0).max(0.0), h);
            scene.quad(Quad::new(field, theme.background).radius(3.0).border(1.0, theme.accent));
            scene.push_clip(field.inset(1.0, 0.0));
            let w = text.measure(name, UI, font);
            if *selected && !name.is_empty() {
                scene.quad(
                    Quad::new(Rect::new(x - 1.0, field.y + 2.0, w + 2.0, field.h - 4.0), theme.selection).radius(2.0),
                );
            }
            text.draw_text(scene, name, x, baseline, UI, font, theme.text);
            scene.fill(Rect::new(x + w + 1.0, field.y + 3.0, 1.0, field.h - 6.0), theme.cursor);
            scene.pop_clip();
        } else {
            scene.push_clip(Rect::new(x, rect.y, (right - x).max(0.0), rect.h));
            let color = if tab.mark.is_some_and(Mark::aside) {
                theme.text_faint
            } else if current {
                theme.text
            } else {
                theme.text_dim
            };
            let shown = text.elide(&tab.title, UI, font, (right - x).max(0.0));
            text.draw_text(scene, &shown, x, baseline, UI, font, color);
            scene.pop_clip();
        }
        tab_hits.push((rect, index));
        if closable {
            text.draw_text(scene, "×", close.x + 4.0, baseline, UI, font + 1.0, theme.text_faint);
            close_hits.push((close, index));
        }
    }
    /// Where tab `index` draws, given the `rect` it belongs at: on its way
    /// there while it slides after a reorder.
    pub(super) fn slid(&mut self, index: usize, rect: Rect) -> Rect {
        let now = Instant::now();
        let Some(tab) = self.tabs.get_mut(index) else { return rect };
        let mut drawn = rect;
        if let Some(((x, y), at)) = tab.slide {
            let t = now.duration_since(at).as_secs_f32() / TAB_SLIDE.as_secs_f32();
            if t >= 1.0 {
                tab.slide = None;
            } else {
                // Ease out: quick to start, settling gently.
                let left = (1.0 - t).powi(3);
                drawn.x += (x - rect.x) * left;
                drawn.y += (y - rect.y) * left;
                let next = now + Duration::from_millis(16);
                self.animation_at = Some(self.animation_at.map_or(next, |a| a.min(next)));
            }
        }
        tab.shown = Some((drawn.x, drawn.y));
        drawn
    }
    /// The tabs as a strip across the top.
    pub(super) fn draw_tab_strip(&mut self, tabs: &[TabView]) {
        let width = self.width;
        let theme = &self.ctx.theme;
        let top = self.top();
        self.scene.fill(Rect::new(0.0, top, width, TAB_BAR_HEIGHT), theme.panel(SURFACE_LIFT / 2.0));
        self.scene.fill(Rect::new(0.0, top + TAB_BAR_HEIGHT - 1.0, width, 1.0), theme.divider);
        let plus = 34.0;
        let tab_width = ((width - 12.0 - plus) / tabs.len() as f32).min(TAB_MAX_WIDTH);
        for (index, tab) in tabs.iter().enumerate() {
            let place = Rect::new(6.0 + index as f32 * tab_width, top + 5.0, tab_width - 4.0, TAB_BAR_HEIGHT - 9.0);
            let rect = self.slid(index, place);
            draw_tab_background(&mut self.scene, &self.ctx.theme, rect, tab.color, index == self.active, false);
            let badge = (tab.panes > 1).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge);
            // Hits go by where it belongs, so a drag doesn't chase it.
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = place;
            }
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let px = 6.0 + tabs.len() as f32 * tab_width;
        let plus_rect = Rect::new(px, top + 5.0, 26.0, TAB_BAR_HEIGHT - 9.0);
        let ascent = text.ui_ascent(12.0);
        let baseline = (plus_rect.y + (plus_rect.h + ascent) / 2.0).round();
        text.draw_text(scene, "+", px + 8.0, baseline, UI, 15.0, theme.text_dim);
        *new_tab_hit = plus_rect;
    }
    /// The tabs as a list down the left side.
    pub(super) fn draw_tab_sidebar(&mut self, tabs: &[TabView]) {
        let area = self.tab_area();
        let theme = &self.ctx.theme;
        self.scene.fill(area, theme.panel(SURFACE_LIFT / 2.0));
        self.scene.fill(Rect::new(area.right() - 1.0, area.y, 1.0, area.h), theme.divider);
        self.scene.push_clip(Rect::new(0.0, area.y, area.w - 1.0, area.h));
        let dragged = match self.pointer {
            Pointer::Tab { index, dragging: true, .. } => Some(index),
            _ => None,
        };
        let mut y = area.y + 8.0;
        for (index, tab) in tabs.iter().enumerate() {
            let place = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
            let rect = self.slid(index, place);
            if self.compact {
                self.draw_compact_tab(index, tab, rect, dragged == Some(index));
                if let Some(hit) = self.tab_hits.last_mut() {
                    hit.0 = place;
                }
                y += SIDEBAR_ROW + 4.0;
                continue;
            }
            // The pane count (where the pane lines don't show the panes)
            // and the mark go on a line under the title; the title moves
            // up to make room.
            let count = (tab.panes > 1 && tab.lines.is_empty()).then(|| format!("{} panes", tab.panes));
            let meta = if count.is_some() || tab.mark.is_some() { SIDEBAR_META } else { 0.0 };
            let rect = Rect::new(rect.x, rect.y, rect.w, rect.h - meta / 2.0);
            let extra = if tab.lines.is_empty() { 0.0 } else { tab.lines.len() as f32 * SIDEBAR_LINE + 6.0 };
            let under = if meta > 0.0 { meta - 2.0 } else { 0.0 };
            let row = Rect::new(rect.x, rect.y, rect.w, rect.h + under + extra);
            draw_tab_background(
                &mut self.scene,
                &self.ctx.theme,
                row,
                tab.color,
                index == self.active,
                dragged == Some(index),
            );
            self.draw_tab(index, tab, rect, None);
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = Rect::new(place.x, place.y, row.w, row.h);
            }
            let below = rect.bottom() - 6.0;
            if meta > 0.0 {
                self.draw_tab_meta(index, tab, count.as_deref(), Rect::new(rect.x, below, rect.w, meta));
            }
            self.draw_pane_lines(index, tab, Rect::new(rect.x, below + meta + 2.0, rect.w, extra));
            y += row.h + 4.0;
        }
        let menu_open = self.tab_menu.as_ref().is_some_and(|menu| menu.kind == MenuKind::Account);
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let plus = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
        let ascent = text.ui_ascent(SIDEBAR_FONT);
        let baseline = (plus.y + (plus.h + ascent * 0.72) / 2.0).round();
        if self.compact {
            let w = text.measure("+", UI, 15.0);
            text.draw_text(scene, "+", plus.x + (plus.w - w) / 2.0, baseline, UI, 15.0, theme.text_faint);
        } else {
            let label = format!("+  New tab   {}+T", platform::COMMAND_NAME);
            text.draw_text(scene, &label, plus.x + 16.0, baseline, UI, SIDEBAR_FONT, theme.text_faint);
        }
        *new_tab_hit = plus;
        // The foot: the account button in the corner, and the button
        // that collapses to compact at the other end (above it when
        // compact, as it expands back).
        let foot = area.bottom() - 34.0;
        let (account, collapse) = if self.compact {
            let x = (area.w - 1.0 - 28.0) / 2.0;
            (Rect::new(x, foot, 28.0, 26.0), Rect::new(x, foot - 30.0, 28.0, 26.0))
        } else {
            (Rect::new(6.0, foot, 28.0, 26.0), Rect::new(area.w - 1.0 - 6.0 - 28.0, foot, 28.0, 26.0))
        };
        if account.contains(self.mouse.0, self.mouse.1) || menu_open {
            scene.quad(Quad::new(account, theme.raised).radius(5.0));
        }
        let face = Rect::new(account.x + 4.0, account.y + 3.0, 20.0, 20.0);
        draw_account_face(scene, text, theme, face, account_info().as_ref());
        self.account_hit = account;
        if collapse.contains(self.mouse.0, self.mouse.1) {
            scene.quad(Quad::new(collapse, theme.raised).radius(5.0));
        }
        let glyph = if self.compact { "»" } else { "«" };
        let w = text.measure(glyph, UI, 14.0);
        let baseline = (collapse.y + (collapse.h + ascent * 0.72) / 2.0).round();
        text.draw_text(scene, glyph, collapse.x + (collapse.w - w) / 2.0, baseline, UI, 14.0, theme.text_faint);
        self.collapse_hit = collapse;
        scene.pop_clip();
        self.draw_tab_hint(tabs);
    }
    /// A sidebar row's line under its title, lined up with it: how many
    /// panes, and the mark's chip.
    pub(super) fn draw_tab_meta(&mut self, index: usize, tab: &TabView, count: Option<&str>, area: Rect) {
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let numbered = index < 9 || tab.agent.is_some();
        let mut x = area.x + if numbered { 10.0 + 22.0 + 8.0 } else { 10.0 };
        let cy = area.y + area.h / 2.0;
        if let Some(count) = count {
            let baseline = (cy + text.ui_ascent(11.0) * 0.36).round();
            text.draw_text(scene, count, x, baseline, UI, 11.0, theme.text_faint);
            x += text.measure(count, UI, 11.0) + 8.0;
        }
        if let Some(mark) = tab.mark {
            let ink = mark.color(theme);
            let w = text.measure(mark.label(), UI_BOLD, 10.0) + 12.0;
            let chip = Rect::new(x, cy - 7.5, w, 15.0);
            scene.quad(Quad::new(chip, ink.alpha(0.16)).radius(7.5).border(1.0, ink.alpha(0.45)));
            let baseline = (cy + text.ui_ascent(10.0) * 0.36).round();
            text.draw_text(scene, mark.label(), chip.x + 6.0, baseline, UI_BOLD, 10.0, ink);
        }
    }
    /// The extended sidebar's lines under a tab's title, one per pane:
    /// a dot for what it's doing, then what that is.
    pub(super) fn draw_pane_lines(&mut self, index: usize, tab: &TabView, area: Rect) {
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let focus = self.tabs.get(index).map(|tab| tab.focus);
        let Self { scene, ctx, pane_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let font = 11.5;
        let ascent = text.ui_ascent(font);
        for (i, line) in tab.lines.iter().enumerate() {
            let rect = Rect::new(area.x, area.y + i as f32 * SIDEBAR_LINE, area.w, SIDEBAR_LINE);
            let cy = rect.y + rect.h / 2.0;
            let dot = Rect::new(rect.x + 16.0, cy - 3.0, 6.0, 6.0);
            let color = match line.agent {
                Some(state) => agent_color(theme, state, t),
                None if line.busy => theme.accent.alpha(0.7),
                None if line.failed => theme.error.alpha(0.85),
                None => theme.text_faint.alpha(0.5),
            };
            scene.quad(Quad::new(dot, color).radius(3.0));
            let x = rect.x + 30.0;
            let baseline = (cy + ascent * 0.36).round();
            let ink = if Some(line.id) == focus && tab.panes > 1 { theme.text } else { theme.text_dim };
            let room = (rect.right() - 8.0 - x).max(0.0);
            let shown = text.elide(&line.text, UI, font, room);
            text.draw_text(scene, &shown, x, baseline, UI, font, ink);
            pane_hits.push((rect, index, line.id));
        }
    }
    /// A compact sidebar row: the tab's number on its background.
    pub(super) fn draw_compact_tab(&mut self, index: usize, tab: &TabView, rect: Rect, dragged: bool) {
        let Self { scene, ctx, tab_hits, .. } = self;
        let Ctx { text, theme, blink_epoch, .. } = ctx;
        let current = index == self.active;
        draw_tab_background(scene, theme, rect, tab.color, current, dragged);
        let center = (rect.x + rect.w / 2.0 + 1.0, rect.y + rect.h / 2.0);
        let t = blink_epoch.elapsed().as_secs_f32();
        draw_tab_number(scene, text, theme, index, center, 24.0, tab.agent, tab.done_for, t);
        if let Some(mark) = tab.mark {
            let dot = Rect::new(rect.right() - 9.0, rect.y + 3.0, 6.0, 6.0);
            scene.quad(Quad::new(dot, mark.color(theme)).radius(3.0));
        }
        tab_hits.push((rect, index));
    }
    /// A label beside the hovered row: a compact row's title at once, a
    /// full row's folder after a moment, when its title doesn't show it.
    pub(super) fn draw_tab_hint(&mut self, tabs: &[TabView]) {
        // An open menu would sit under the hint.
        if !matches!(self.pointer, Pointer::Idle) || self.tab_menu.is_some() {
            return;
        }
        let (mx, my) = self.mouse;
        let Some(&(row, index)) = self.tab_hits.iter().find(|(r, _)| r.contains(mx, my)) else { return };
        let Some(tab) = tabs.get(index) else { return };
        let title = if self.compact {
            match tab.agent {
                Some(agent) => format!("{} · {}", agent.label(), tab.title),
                None => tab.title.clone(),
            }
        } else {
            if tab.path.is_empty() || tab.path == tab.title || tab.editing.is_some() {
                return;
            }
            let at = self.hover_at + TAB_HINT_DELAY;
            if Instant::now() < at {
                self.animation_at = Some(self.animation_at.map_or(at, |a| a.min(at)));
                return;
            }
            tab.path.clone()
        };
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        let w = (text.measure(&title, UI, 12.0) + 20.0).min(TAB_MAX_WIDTH * 1.5);
        let label = Rect::new(row.right() + 8.0, row.y + 2.0, w, row.h - 4.0);
        scene.shadow(label, Color::hex(0x000000).alpha(0.5), 8.0, 6.0);
        scene.quad(Quad::new(label, theme.raised).radius(6.0).border(1.0, theme.divider));
        scene.push_clip(label.inset(8.0, 0.0));
        let baseline = (label.y + (label.h + text.ui_ascent(12.0) * 0.72) / 2.0).round();
        text.draw_text(scene, &title, label.x + 10.0, baseline, UI, 12.0, theme.text);
        scene.pop_clip();
        scene.set_layer(LAYER_CONTENT);
    }
}

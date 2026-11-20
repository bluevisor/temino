use super::*;

impl App {
    pub(super) fn render(&mut self) {
        crate::trace("render: begin");
        self.preview();
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
        self.draw_agent_headers();
        self.draw_remote_marks(&visible);
        for divider in &self.dividers {
            self.scene.set_layer(LAYER_OVERLAY);
            let over = divider.grab_area().contains(self.mouse.0, self.mouse.1);
            let ink = if over { self.ctx.theme.accent.alpha(0.65) } else { self.ctx.theme.divider };
            self.scene.fill(divider.line, ink);
            self.scene.set_layer(LAYER_CONTENT);
        }
        self.draw_tab_bar(&agents);
        self.draw_titlebar();
        self.draw_update();
        self.draw_consent();
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
    /// The bar over every pane: over an agent, what it's doing and for
    /// how long, what you last asked it, its subagents and its session; over
    /// a listed program, its name and how long it has run; over any other
    /// terminal, its running command, or its folder at rest. The focused
    /// pane's is lit. An agent's, opened, pushes its terminal down.
    pub(super) fn draw_agent_headers(&mut self) {
        self.header_hits.clear();
        if self.tabs.is_empty() {
            return;
        }
        let now = Instant::now();
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let focus = self.focused_id();
        let sender = self.ctx.sender.clone();
        let mut moved = false;
        let slots = self.tab_slots(self.active).0;
        let alone = slots.len() == 1;
        for (id, slot) in slots {
            let Some(index) = self.panes.iter().position(|pane| pane.id == id) else { continue };
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
            let bar = Rect::new(slot.x, slot.y, slot.w, AGENT_HEADER.min(slot.h));
            let hover = self.header_hover == Some(id);
            let focused = match (Some(id) == focus, alone) {
                (false, _) => Lit::Off,
                (true, true) => Lit::Soft,
                (true, false) => Lit::Full,
            };
            let Self { scene, ctx, pane_hits, header_hits, panes, .. } = self;
            let Ctx { text, theme, .. } = ctx;
            let reach = match (info, program, shell) {
                (Some(info), _, _) => {
                    let room = header_room(slot);
                    let reach = draw_agent_header(scene, text, theme, bar, &info, focused, hover, room, t);
                    header_hits.push((Rect::new(bar.x, bar.y, bar.w, bar.h + reach), id));
                    reach
                }
                (None, Some((name, started)), _) => {
                    draw_program_header(scene, text, theme, bar, &name, now.duration_since(started), focused);
                    0.0
                }
                (None, None, Some(shell)) => {
                    draw_shell_header(scene, text, theme, bar, &shell, focused);
                    0.0
                }
                (None, None, None) => 0.0,
            };
            pane_hits.push((bar, self.active, id));
            // The terminal makes room for what the header now takes.
            let pane = &mut panes[index];
            if (reach - pane.header_reach).abs() >= 1.0 {
                pane.header_reach = reach;
                moved = true;
            }
        }
        if moved {
            self.relayout();
            self.dirty = true;
        }
    }
    /// One continuous title bar across the window, with the active tab's
    /// summary centered and room reserved for AppKit's traffic lights.
    pub(super) fn draw_titlebar(&mut self) {
        let top = self.top();
        if top <= 0.0 {
            return;
        }
        let summary = self.tabs.get(self.active).and_then(|tab| tab.ai_title.summary.clone());
        let Self { scene, ctx, title, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.fill(Rect::new(0.0, 0.0, self.width, top), theme.panel(SURFACE_LIFT / 2.0));
        scene.fill(Rect::new(0.0, top - 1.0, self.width, 1.0), theme.divider);
        // Fixed in points, like the native controls, even when zoomed.
        // Symmetric insets keep the title at the window's true center.
        let lights = 84.0 / self.zoom;
        let font = 14.0 / self.zoom;
        let room = (self.width - 2.0 * lights).max(0.0);
        let line = summary.map(|summary| format!("✦ {summary}"));
        let (label, face) = match &line {
            Some(line) => (line.as_str(), UI),
            None => (title.as_str(), UI_BOLD),
        };
        let shown = text.elide(label, face, font, room);
        let w = text.measure(&shown, face, font);
        let y = (top / 2.0 + text.ui_ascent(font) * 0.36).round();
        text.draw_text(scene, &shown, (self.width - w) / 2.0, y, face, font, theme.text_dim);
    }
    pub(super) fn draw_tab_bar(&mut self, agents: &[Option<AgentState>]) {
        self.tab_hits.clear();
        self.close_hits.clear();
        self.rename_field = Rect::default();
        self.new_tab_hit = Rect::default();
        self.collapse_hit = Rect::default();
        self.account_hit = Rect::default();
        if !self.tab_list_visible() {
            return;
        }
        let now = Instant::now();
        self.refresh_titles(now);
        let extended = self.extended && !self.compact && self.strip == TabStrip::Left;
        let mut tabs: Vec<TabView> = (0..self.tabs.len())
            .map(|index| {
                let mut leaves = Vec::new();
                self.tabs[index].root.leaves(&mut leaves);
                // Only the current tab grows its lines; the rest stay one row.
                let mut lines =
                    if extended && index == self.active { self.pane_lines(index, &leaves, now) } else { Vec::new() };
                // A split tab in the full sidebar: a row per pane, each
                // with its line.
                let group = self.lists_panes() && leaves.len() > 1;
                let rows: Vec<PaneRow> = if group {
                    leaves
                        .iter()
                        .map(|&id| {
                            let line = lines.iter().position(|line| line.id == id).map(|at| lines.remove(at));
                            self.pane_row(id, line, now)
                        })
                        .collect()
                } else {
                    Vec::new()
                };
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
                    number: 0,
                    lines,
                    rows,
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
        // The numbers run straight down the list, a group's panes taking
        // one apiece, as ⌘-digits count them (numbered_rows).
        let mut number = 0;
        for tab in &mut tabs {
            tab.number = number;
            number += tab.rows.len().max(1);
        }
        match self.strip {
            TabStrip::Left => self.draw_tab_sidebar(&tabs),
            TabStrip::Top => self.draw_tab_strip(&tabs),
        }
    }
    /// Pane `id`'s row in its tab's group: its name and its agent.
    pub(super) fn pane_row(&self, id: u64, line: Option<PaneLine>, now: Instant) -> PaneRow {
        let pane = self.panes.iter().find(|pane| pane.id == id);
        let title = if id == SETTINGS {
            "Settings".to_owned()
        } else if let Some(viewer) = self.viewers.iter().find(|viewer| viewer.id == id) {
            viewer.title.clone()
        } else {
            pane.map(Pane::row_title).unwrap_or_default()
        };
        let done_for = match pane.and_then(|pane| pane.needs_since) {
            Some((AgentState::Done, at)) => now.duration_since(at).as_secs_f32(),
            _ => f32::MAX,
        };
        PaneRow { id, title, agent: pane.and_then(|pane| pane.agent_now), done_for, line }
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
    /// As a group's `header`, it's the name alone, small, over the card.
    pub(super) fn draw_tab(&mut self, index: usize, tab: &TabView, rect: Rect, badge: Option<String>, header: bool) {
        let current = index == self.active;
        let hovered = self.hover_tab == Some(index);
        let left = self.strip == TabStrip::Left;
        let (font, number) = if header {
            (11.5, 0.0)
        } else if left {
            (SIDEBAR_FONT, 22.0)
        } else {
            (12.0, 18.0)
        };
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let Self { scene, ctx, tab_hits, close_hits, rename_field, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(font);
        let baseline = (rect.y + (rect.h + ascent * 0.72) / 2.0).round();
        let mut x = rect.x
            + if header {
                6.0
            } else if left {
                10.0
            } else {
                8.0
            };
        if !header {
            let center = (x + number / 2.0, rect.y + rect.h / 2.0);
            draw_tab_number(scene, text, theme, tab.number, center, number, tab.agent, tab.done_for, t);
            x += number + 8.0;
        }
        let close = Rect::new(rect.right() - 22.0, rect.y + (rect.h - 16.0) / 2.0, 16.0, 16.0);
        // Under 48px a top-strip tab can't hold both its number and the
        // ×: the two overlap, the × spilling onto the tab before it at
        // the smallest widths, and since close hits are checked first a
        // click meant to switch tabs closes one instead. No × there —
        // middle-click and the tab menu still close it.
        let closable = hovered && tab.editing.is_none() && rect.w >= 48.0;
        let mut right = if closable { close.x - 4.0 } else { rect.right() - 8.0 };
        if let Some(label) = badge {
            // A small badge with the pane count.
            let w = text.measure(&label, UI, 10.0) + 10.0;
            let badge = Rect::new(right - w, rect.y + (rect.h - 16.0) / 2.0, w, 16.0);
            scene.quad(Quad::new(badge, theme.divider).radius(8.0));
            text.draw_text(scene, &label, badge.x + 5.0, badge.y + 12.0, UI, 10.0, theme.text_dim);
            right = badge.x - 6.0;
        }
        // In the sidebar the mark goes under the title (draw_tab_meta),
        // or at the end of a group's header.
        if let Some(mark) = tab.mark.filter(|_| !left || header) {
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
            // To `right`, not to the close button: no × is drawn while
            // editing (closable is false), and `right` already stops
            // short of any badge or mark chip on the row.
            let field = Rect::new(x - 3.0, top, (right - x + 3.0).max(0.0), h);
            *rename_field = field;
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
            let color = if tab.mark.is_some_and(Mark::aside) || (header && !current) {
                theme.text_faint
            } else if current && !header {
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
            let hovered = self.hover_tab == Some(index);
            draw_tab_background(
                &mut self.scene,
                &self.ctx.theme,
                rect,
                tab.color,
                index == self.active,
                false,
                hovered,
            );
            let badge = (tab.panes > 1).then(|| format!("{}", tab.panes));
            self.draw_tab(index, tab, rect, badge, false);
            // Hits go by where it belongs, so a drag doesn't chase it.
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = place;
            }
        }
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let px = 6.0 + tabs.len() as f32 * tab_width;
        let plus_rect = Rect::new(px, top + 5.0, 26.0, TAB_BAR_HEIGHT - 9.0);
        let over = plus_rect.contains(self.mouse.0, self.mouse.1);
        if over {
            scene.quad(Quad::new(plus_rect, theme.panel(HOVER_LIFT)).radius(6.0));
        }
        let ascent = text.ui_ascent(12.0);
        let baseline = (plus_rect.y + (plus_rect.h + ascent) / 2.0).round();
        let w = text.measure("+", UI, 15.0);
        let ink = if over { theme.text } else { theme.text_dim };
        text.draw_text(scene, "+", px + (plus_rect.w - w) / 2.0, baseline, UI, 15.0, ink);
        *new_tab_hit = plus_rect;
    }
    /// The tabs as a list down the left side.
    pub(super) fn draw_tab_sidebar(&mut self, tabs: &[TabView]) {
        let area = self.tab_area();
        let theme = &self.ctx.theme;
        self.scene.fill(area, theme.panel(SURFACE_LIFT / 2.0));
        let edge = if self.sidebar_edge().contains(self.mouse.0, self.mouse.1) {
            theme.accent.alpha(0.65)
        } else {
            theme.divider
        };
        self.scene.fill(Rect::new(area.right() - 1.0, area.y, 1.0, area.h), edge);
        // The rows and the new-tab row scroll, above the foot's buttons.
        let foot = area.bottom() - 34.0;
        let list_bottom = if self.compact { foot - 30.0 } else { foot } - 6.0;
        let list = Rect::new(0.0, area.y, area.w - 1.0, (list_bottom - area.y).max(0.0));
        let heights: Vec<f32> = tabs.iter().map(|tab| sidebar_row_height(tab, self.compact)).collect();
        let content = 8.0 + heights.iter().map(|h| h + 4.0).sum::<f32>() + SIDEBAR_ROW + 4.0;
        self.sidebar_scroll_max = (content - list.h).max(0.0);
        // A tab switch or a new tab scrolls the current one into view.
        let shown = (self.active, tabs.len());
        if shown != self.sidebar_shown && self.active < heights.len() {
            self.sidebar_shown = shown;
            let top = 8.0 + heights[..self.active].iter().map(|h| h + 4.0).sum::<f32>();
            let bottom = top + heights[self.active] + 4.0;
            if top - 8.0 < self.sidebar_scroll {
                self.sidebar_scroll = top - 8.0;
            } else if bottom > self.sidebar_scroll + list.h {
                self.sidebar_scroll = bottom - list.h;
            }
        }
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, self.sidebar_scroll_max);
        let (tab_start, pane_start) = (self.tab_hits.len(), self.pane_hits.len());
        self.scene.push_clip(list);
        let dragged = match self.pointer {
            Pointer::Tab { index, dragging: true, .. } => Some(index),
            _ => None,
        };
        let mut y = area.y + 8.0 - self.sidebar_scroll;
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
            if !tab.rows.is_empty() {
                let group = Rect::new(rect.x, rect.y, rect.w, group_height(tab));
                let hovered = self.hover_tab == Some(index) && dragged.is_none();
                self.draw_tab_group(index, tab, group, dragged == Some(index), hovered);
                if let Some(hit) = self.tab_hits.last_mut() {
                    hit.0 = Rect::new(place.x, place.y, group.w, group.h);
                }
                y += group.h + 4.0;
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
                self.hover_tab == Some(index) && dragged.is_none(),
            );
            self.draw_tab(index, tab, rect, None, false);
            if let Some(hit) = self.tab_hits.last_mut() {
                hit.0 = Rect::new(place.x, place.y, row.w, row.h);
            }
            let below = rect.bottom() - 6.0;
            if meta > 0.0 {
                self.draw_tab_meta(tab, count.as_deref(), Rect::new(rect.x, below, rect.w, meta));
            }
            self.draw_pane_lines(index, tab, Rect::new(rect.x, below + meta + 2.0, rect.w, extra));
            y += row.h + 4.0;
        }
        let menu_open = self.tab_menu.as_ref().is_some_and(|menu| menu.kind == MenuKind::Account);
        let Self { scene, ctx, new_tab_hit, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let plus = Rect::new(6.0, y, area.w - 13.0, SIDEBAR_ROW);
        let plus_hit = clipped(plus, list);
        let ascent = text.ui_ascent(SIDEBAR_FONT);
        let baseline = (plus.y + (plus.h + ascent * 0.72) / 2.0).round();
        // Lit under the pointer, like the rows above it. The plus sits where
        // their numbers do, the label where their titles do, and the
        // shortcut at the far end.
        let over = plus_hit.contains(self.mouse.0, self.mouse.1) && matches!(self.pointer, Pointer::Idle);
        if over {
            scene.quad(Quad::new(plus, theme.panel(HOVER_LIFT)).radius(6.0));
        }
        let ink = if over { theme.text_dim } else { theme.text_faint };
        let w = text.measure("+", UI, 15.0);
        if self.compact {
            text.draw_text(scene, "+", plus.x + (plus.w - w) / 2.0, baseline, UI, 15.0, ink);
        } else {
            text.draw_text(scene, "+", plus.x + 10.0 + (22.0 - w) / 2.0, baseline, UI, 15.0, ink);
            let label_x = plus.x + 10.0 + 22.0 + 8.0;
            let label_w = text.draw_text(scene, "New tab", label_x, baseline, UI, SIDEBAR_FONT, ink);
            let keys = platform::shortcut("T");
            let keys_w = text.measure(&keys, UI, 11.5);
            let keys_x = plus.right() - 10.0 - keys_w;
            if keys_x > label_x + label_w + 12.0 {
                text.draw_text(scene, &keys, keys_x, baseline, UI, 11.5, theme.text_faint);
            }
        }
        *new_tab_hit = plus_hit;
        // Rows scrolled out of the list can't be clicked or hovered.
        for hit in &mut self.tab_hits[tab_start..] {
            hit.0 = clipped(hit.0, list);
        }
        self.tab_hits.retain(|(rect, _)| rect.h > 0.0);
        for hit in &mut self.pane_hits[pane_start..] {
            hit.0 = clipped(hit.0, list);
        }
        self.pane_hits.retain(|(rect, ..)| rect.h > 0.0);
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        // A thin thumb on the right edge says there's more to scroll to.
        if self.sidebar_scroll_max > 0.0 {
            let track = Rect::new(list.right() - 5.0, list.y + 4.0, 3.0, (list.h - 8.0).max(0.0));
            let h = (track.h * list.h / content).max(24.0).min(track.h);
            let at = track.y + (track.h - h) * self.sidebar_scroll / self.sidebar_scroll_max;
            scene.quad(Quad::new(Rect::new(track.x, at, track.w, h), theme.text_faint.alpha(0.45)).radius(1.5));
        }
        scene.pop_clip();
        scene.push_clip(Rect::new(0.0, area.y, area.w - 1.0, area.h));
        // The foot: the account button in the corner, and the button
        // that collapses to compact at the other end (above it when
        // compact, as it expands back).
        let (account, collapse) = if self.compact {
            let x = (area.w - 1.0 - 28.0) / 2.0;
            (Rect::new(x, foot, 28.0, 26.0), Rect::new(x, foot - 30.0, 28.0, 26.0))
        } else {
            (Rect::new(6.0, foot, 28.0, 26.0), Rect::new(area.w - 1.0 - 6.0 - 28.0, foot, 28.0, 26.0))
        };
        if account.contains(self.mouse.0, self.mouse.1) || menu_open {
            scene.quad(Quad::new(account, theme.panel(HOVER_LIFT)).radius(6.0));
        }
        let face = Rect::new(account.x + 4.0, account.y + 3.0, 20.0, 20.0);
        draw_account_face(scene, text, theme, face, account_info().as_ref());
        self.account_hit = account;
        let over = collapse.contains(self.mouse.0, self.mouse.1);
        if over {
            scene.quad(Quad::new(collapse, theme.panel(HOVER_LIFT)).radius(6.0));
        }
        draw_sidebar_icon(scene, collapse, if over { theme.text_dim } else { theme.text_faint });
        self.collapse_hit = collapse;
        scene.pop_clip();
        self.draw_tab_hint(tabs);
    }
    /// A sidebar row's line under its title, lined up with it: how many
    /// panes, and the mark's chip.
    pub(super) fn draw_tab_meta(&mut self, tab: &TabView, count: Option<&str>, area: Rect) {
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let mut x = area.x + 10.0 + 22.0 + 8.0;
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
    /// A split tab in the sidebar: its name on a header line, then a card
    /// in its color with a row per pane; the focused pane's row of the
    /// current tab is lit. Each row takes the next number down the list. A
    /// row's click goes to that pane.
    pub(super) fn draw_tab_group(&mut self, index: usize, tab: &TabView, area: Rect, dragged: bool, hovered: bool) {
        let current = index == self.active;
        let head = Rect::new(area.x, area.y, area.w, GROUP_HEAD);
        let card = Rect::new(area.x, head.bottom(), area.w, (area.bottom() - head.bottom()).max(0.0));
        draw_tab_background(&mut self.scene, &self.ctx.theme, card, tab.color, current, dragged, hovered);
        self.draw_tab(index, tab, head, None, true);
        let focus = self.tabs.get(index).map(|tab| tab.focus);
        let idle = matches!(self.pointer, Pointer::Idle) && self.tab_menu.is_none();
        let (mx, my) = self.mouse;
        let t = self.ctx.blink_epoch.elapsed().as_secs_f32();
        let Self { scene, ctx, pane_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let mut y = card.y + 2.0;
        for (pane, row) in tab.rows.iter().enumerate() {
            let rect = Rect::new(card.x, y, card.w, group_row_height(row));
            y = rect.bottom();
            // Clear of the card's rail.
            let fill = Rect::new(rect.x + 8.0, rect.y + 1.0, rect.w - 10.0, rect.h - 2.0);
            let lit = current && Some(row.id) == focus;
            if lit {
                let ink = tab.color.unwrap_or(theme.accent);
                scene.quad(Quad::new(fill, ink.alpha(0.10)).radius(5.0).border(1.0, ink.alpha(0.35)));
            } else if idle && rect.contains(mx, my) {
                scene.quad(Quad::new(fill, theme.text.alpha(0.04)).radius(5.0));
            }
            let title_h = if row.line.is_some() { GROUP_ROW - 10.0 } else { GROUP_ROW };
            let center = (rect.x + 21.0, rect.y + title_h / 2.0);
            draw_tab_number(scene, text, theme, tab.number + pane, center, 22.0, row.agent, row.done_for, t);
            let x = rect.x + 40.0;
            let room = (rect.right() - 10.0 - x).max(0.0);
            let baseline = (rect.y + (title_h + text.ui_ascent(SIDEBAR_FONT) * 0.72) / 2.0).round();
            let ink = if lit { theme.text } else { theme.text_dim };
            let shown = text.elide(&row.title, UI, SIDEBAR_FONT, room);
            text.draw_text(scene, &shown, x, baseline, UI, SIDEBAR_FONT, ink);
            if let Some(line) = &row.line {
                let font = 11.5;
                let cy = rect.y + title_h - 4.0 + SIDEBAR_LINE / 2.0;
                let baseline = (cy + text.ui_ascent(font) * 0.36).round();
                let ink = if line.failed { theme.error.alpha(0.85) } else { theme.text_faint };
                let shown = text.elide(&line.text, UI, font, room);
                text.draw_text(scene, &shown, x, baseline, UI, font, ink);
            }
            pane_hits.push((rect, index, row.id));
        }
    }
    /// A compact sidebar row: the tab's number on its background.
    pub(super) fn draw_compact_tab(&mut self, index: usize, tab: &TabView, rect: Rect, dragged: bool) {
        let Self { scene, ctx, tab_hits, .. } = self;
        let Ctx { text, theme, blink_epoch, .. } = ctx;
        let current = index == self.active;
        draw_tab_background(scene, theme, rect, tab.color, current, dragged, self.hover_tab == Some(index));
        let center = (rect.x + rect.w / 2.0 + 1.0, rect.y + rect.h / 2.0);
        let t = blink_epoch.elapsed().as_secs_f32();
        draw_tab_number(scene, text, theme, tab.number, center, 24.0, tab.agent, tab.done_for, t);
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
            // A group's rows already say what each pane is.
            if tab.path.is_empty() || tab.path == tab.title || tab.editing.is_some() || !tab.rows.is_empty() {
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
        let title = text.elide(&title, UI, 12.0, (w - 20.0).max(0.0));
        // Beside the row, but kept in the window the way the menus are:
        // a tab at the right edge (the top strip's last ones) would
        // otherwise push the hint off-screen.
        let x = (row.right() + 8.0).min((self.width - w - 4.0).max(4.0));
        let label = Rect::new(x, row.y + 2.0, w, row.h - 4.0);
        scene.shadow(label, theme.shadow(0.5), 8.0, 6.0);
        scene.quad(Quad::new(label, theme.raised).radius(6.0).border(1.0, theme.divider));
        scene.push_clip(label.inset(8.0, 0.0));
        let baseline = (label.y + (label.h + text.ui_ascent(12.0) * 0.72) / 2.0).round();
        text.draw_text(scene, &title, label.x + 10.0, baseline, UI, 12.0, theme.text);
        scene.pop_clip();
        scene.set_layer(LAYER_CONTENT);
    }
}

/// A sidebar row's height: the title row, the meta line under it (the pane
/// count or the mark), and the extended view's pane lines. As drawn by
/// `draw_tab_sidebar`.
fn sidebar_row_height(tab: &TabView, compact: bool) -> f32 {
    if compact {
        return SIDEBAR_ROW;
    }
    if !tab.rows.is_empty() {
        return group_height(tab);
    }
    let meta = if (tab.panes > 1 && tab.lines.is_empty()) || tab.mark.is_some() { SIDEBAR_META } else { 0.0 };
    let under = if meta > 0.0 { meta - 2.0 } else { 0.0 };
    let extra = if tab.lines.is_empty() { 0.0 } else { tab.lines.len() as f32 * SIDEBAR_LINE + 6.0 };
    SIDEBAR_ROW - meta / 2.0 + under + extra
}

/// The part of `rect` inside `clip` (empty when they don't meet).
fn clipped(rect: Rect, clip: Rect) -> Rect {
    let (x, y) = (rect.x.max(clip.x), rect.y.max(clip.y));
    Rect::new(x, y, (rect.right().min(clip.right()) - x).max(0.0), (rect.bottom().min(clip.bottom()) - y).max(0.0))
}

/// A split tab's group: its header, and a card of its pane rows.
fn group_height(tab: &TabView) -> f32 {
    GROUP_HEAD + 4.0 + tab.rows.iter().map(group_row_height).sum::<f32>()
}

/// A pane's row in a group, taller with its line under the name.
fn group_row_height(row: &PaneRow) -> f32 {
    GROUP_ROW + if row.line.is_some() { 10.0 } else { 0.0 }
}

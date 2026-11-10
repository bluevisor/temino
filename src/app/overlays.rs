use super::*;

impl App {
    /// How far along something that came up at `since` is: how opaque, and
    /// how many points short of its place. Keeps the frames coming until
    /// it has settled.
    pub(super) fn appear(&mut self, since: Instant) -> (f32, f32) {
        let t = since.elapsed().as_secs_f32() / APPEAR.as_secs_f32();
        if t >= 1.0 {
            return (1.0, 0.0);
        }
        let next = Instant::now() + Duration::from_millis(16);
        self.animation_at = Some(self.animation_at.map_or(next, |a| a.min(next)));
        // Ease out: quick to start, settling gently.
        let ease = 1.0 - (1.0 - t).powi(3);
        (ease, (1.0 - ease) * APPEAR_TRAVEL)
    }
    /// The open menu, fading in as it settles into place: down from the
    /// click, or up from the account button.
    pub(super) fn draw_tab_menu(&mut self) {
        let Some((opened, up)) = self.tab_menu.as_ref().map(|menu| (menu.opened, menu.kind == MenuKind::Account))
        else {
            self.menu_hits.clear();
            self.menu_rect = Rect::default();
            return;
        };
        let (fade, short) = self.appear(opened);
        self.scene.set_fade(fade);
        self.scene.set_origin(0.0, if up { short } else { -short });
        self.draw_open_menu();
        self.scene.set_origin(0.0, 0.0);
        self.scene.set_fade(1.0);
    }
    /// The open tab menu: Rename, a row of colors, Close.
    fn draw_open_menu(&mut self) {
        self.menu_hits.clear();
        self.menu_rect = Rect::default();
        let Some(menu) = &self.tab_menu else { return };
        match menu.kind {
            MenuKind::Pane => return self.draw_pane_menu(),
            MenuKind::Account => return self.draw_account_menu(),
            MenuKind::Tab => {}
        }
        let Some(tab) = self.tabs.get(menu.tab) else { return };
        let selected = tab.color;
        let first = if self.held & platform::mods::OPTION != 0 { SWATCHES } else { 0 };
        let marked = tab.mark;
        let several = self.tabs.len() > 1;
        let rows = if several { 5.0 } else { 4.0 };
        let h = rows * MENU_ROW + 12.0 + 9.0;
        let x = menu.x.min(self.width - MENU_WIDTH - 8.0).max(4.0);
        let y = menu.y.min(self.height - h - 8.0).max(4.0);
        let rect = Rect::new(x, y, MENU_WIDTH, h);
        let (mx, my) = self.mouse;
        let Self { scene, ctx, menu_hits, menu_rect, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(rect, theme.shadow(0.5), 8.0, 10.0);
        scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
        let ascent = text.ui_ascent(12.0);
        let mut row_y = y + 6.0;
        let mut item = |scene: &mut Scene, label: &str, action: MenuAction, row_y: f32| {
            let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, MENU_ROW);
            if row.contains(mx, my) {
                scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
            }
            let baseline = (row.y + (row.h + ascent * 0.72) / 2.0).round();
            text.draw_text(scene, label, row.x + 9.0, baseline, UI, 12.0, theme.text);
            menu_hits.push((row, action));
        };
        item(scene, "Rename Tab…", MenuAction::Rename, row_y);
        row_y += MENU_ROW;
        // The colors: none, then a row of TAB_COLORS (the alternates with Option).
        let size = 16.0;
        let step = (MENU_WIDTH - 28.0 - size) / SWATCHES as f32;
        let mut swatches = Vec::new();
        for choice in std::iter::once(None).chain((first..first + SWATCHES).map(Some)) {
            let i = choice.map_or(0.0, |c| (c - first) as f32 + 1.0);
            let swatch = Rect::new(x + 14.0 + i * step, row_y + (MENU_ROW - size) / 2.0, size, size);
            let hit = swatch.inset(-3.0, -3.0);
            let ring = if selected == choice {
                theme.text
            } else if hit.contains(mx, my) {
                theme.text_dim
            } else {
                theme.divider
            };
            match choice {
                Some(c) => {
                    scene.quad(Quad::new(swatch, Color::hex(TAB_COLORS[c])).radius(size / 2.0).border(1.5, ring))
                }
                None => {
                    scene.quad(Quad::new(swatch, theme.background).radius(size / 2.0).border(1.5, ring));
                    let slash = Rect::new(swatch.x + size / 2.0 - 0.75, swatch.y + 3.0, 1.5, size - 6.0);
                    scene.fill(slash, theme.text_faint);
                }
            }
            swatches.push((hit, MenuAction::Color(choice)));
        }
        row_y += MENU_ROW;
        // The marks go here, drawn once the rows are done with the text.
        let marks_y = row_y;
        row_y += MENU_ROW + 4.0;
        scene.fill(Rect::new(x + 10.0, row_y, MENU_WIDTH - 20.0, 1.0), theme.divider);
        row_y += 5.0;
        item(scene, "Close Tab", MenuAction::Close, row_y);
        if several {
            item(scene, "Close Other Tabs", MenuAction::CloseOthers, row_y + MENU_ROW);
        }
        // The marks, each a chip that toggles.
        let mut chip_x = x + 10.0;
        for mark in Mark::ALL {
            let ink = mark.color(theme);
            let w = text.measure(mark.label(), UI_BOLD, 10.5) + 14.0;
            let chip = Rect::new(chip_x, marks_y + (MENU_ROW - 18.0) / 2.0, w, 18.0);
            let on = marked == Some(mark);
            let fill = if on {
                ink.alpha(0.3)
            } else if chip.contains(mx, my) {
                ink.alpha(0.14)
            } else {
                theme.background
            };
            scene.quad(Quad::new(chip, fill).radius(9.0).border(1.0, ink.alpha(if on { 0.9 } else { 0.4 })));
            text.draw_text(scene, mark.label(), chip.x + 7.0, chip.y + 13.0, UI_BOLD, 10.5, ink);
            swatches.push((chip, MenuAction::Mark(mark)));
            chip_x += w + 6.0;
        }
        menu_hits.extend(swatches);
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
    }
    /// The focused pane's menu: Copy, Paste, Clear, the splits, Close.
    /// The account menu, opening up from its button: who's signed in (to
    /// Settings ▸ Remote Control), Settings, and signing in or out.
    pub(super) fn draw_account_menu(&mut self) {
        let Some(menu) = &self.tab_menu else { return };
        let who = account_info();
        let remote = cfg!(not(target_os = "haiku"));
        let profile = 46.0;
        let mut h = 12.0 + MENU_ROW;
        if who.is_some() {
            h += profile + 9.0 + 9.0 + MENU_ROW;
        } else if remote {
            h += MENU_ROW;
        }
        let x = menu.x.min(self.width - MENU_WIDTH - 8.0).max(4.0);
        let y = (menu.y - h).max(4.0);
        let rect = Rect::new(x, y, MENU_WIDTH, h);
        let (mx, my) = self.mouse;
        let Self { scene, ctx, menu_hits, menu_rect, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(rect, theme.shadow(0.5), 8.0, 10.0);
        scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
        let ascent = text.ui_ascent(12.0);
        let mut row_y = y + 6.0;
        type Hits = Vec<(Rect, MenuAction)>;
        let item =
            |scene: &mut Scene, text: &mut TextSystem, hits: &mut Hits, label: &str, shortcut: &str, action, row_y| {
                let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, MENU_ROW);
                if row.contains(mx, my) {
                    scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
                }
                let baseline = (row.y + (row.h + ascent * 0.72) / 2.0).round();
                text.draw_text(scene, label, row.x + 9.0, baseline, UI, 12.0, theme.text);
                let w = text.measure(shortcut, UI, 11.0);
                text.draw_text(scene, shortcut, row.right() - 9.0 - w, baseline, UI, 11.0, theme.text_faint);
                hits.push((row, action));
            };
        let divider = |scene: &mut Scene, row_y: f32| {
            scene.fill(Rect::new(x + 10.0, row_y + 4.0, MENU_WIDTH - 20.0, 1.0), theme.divider);
        };
        if let Some(who) = &who {
            // Who: their photo and name; it opens Settings at Remote Control.
            let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, profile);
            if row.contains(mx, my) {
                scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
            }
            let face = Rect::new(row.x + 8.0, row.y + (profile - 28.0) / 2.0, 28.0, 28.0);
            draw_account_face(scene, text, theme, face, Some(who));
            let tx = face.right() + 10.0;
            let room = (row.right() - 8.0 - tx).max(0.0);
            let name = text.elide(&who.0, UI_BOLD, 12.5, room);
            text.draw_text(scene, &name, tx, row.y + 20.0, UI_BOLD, 12.5, theme.text);
            text.draw_text(scene, "Remote Control", tx, row.y + 35.0, UI, 11.0, theme.text_faint);
            menu_hits.push((row, MenuAction::Account));
            row_y += profile + 4.0;
            divider(scene, row_y);
            row_y += 9.0;
        } else if remote {
            item(scene, text, menu_hits, "Sign In…", "", MenuAction::SignIn, row_y);
            row_y += MENU_ROW;
        }
        item(scene, text, menu_hits, "Settings…", &platform::shortcut(","), MenuAction::Settings, row_y);
        row_y += MENU_ROW;
        if who.is_some() {
            divider(scene, row_y);
            row_y += 9.0;
            item(scene, text, menu_hits, "Sign Out", "", MenuAction::SignOut, row_y);
        }
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
    }
    pub(super) fn draw_pane_menu(&mut self) {
        let Some(menu) = &self.tab_menu else { return };
        let cmd = platform::shortcut;
        let rows: [(&str, String, MenuAction); 7] = [
            ("Copy", cmd("C"), MenuAction::Copy),
            ("Paste", cmd("V"), MenuAction::Paste),
            ("Clear", cmd("K"), MenuAction::Clear),
            ("Split Right", cmd("D"), MenuAction::SplitRight),
            ("Split Down", cmd("E"), MenuAction::SplitDown),
            // The built-in browser is macOS's (platform::VIEWERS).
            (
                "Open Browser Pane",
                if platform::VIEWERS { "⌥⌘B".to_owned() } else { String::new() },
                MenuAction::BrowserPane,
            ),
            ("Close Pane", cmd("W"), MenuAction::ClosePane),
        ];
        let h = rows.len() as f32 * MENU_ROW + 12.0 + 2.0 * 9.0;
        let x = menu.x.min(self.width - MENU_WIDTH - 8.0).max(4.0);
        let y = menu.y.min(self.height - h - 8.0).max(4.0);
        let rect = Rect::new(x, y, MENU_WIDTH, h);
        let (mx, my) = self.mouse;
        let Self { scene, ctx, menu_hits, menu_rect, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(rect, theme.shadow(0.5), 8.0, 10.0);
        scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
        let ascent = text.ui_ascent(12.0);
        let mut row_y = y + 6.0;
        for (i, (label, shortcut, action)) in rows.into_iter().enumerate() {
            if i == 3 || i == 6 {
                scene.fill(Rect::new(x + 10.0, row_y + 4.0, MENU_WIDTH - 20.0, 1.0), theme.divider);
                row_y += 9.0;
            }
            let row = Rect::new(x + 5.0, row_y, MENU_WIDTH - 10.0, MENU_ROW);
            if row.contains(mx, my) {
                scene.quad(Quad::new(row, theme.accent.alpha(0.22)).radius(5.0));
            }
            let baseline = (row.y + (row.h + ascent * 0.72) / 2.0).round();
            text.draw_text(scene, label, row.x + 9.0, baseline, UI, 12.0, theme.text);
            let w = text.measure(&shortcut, UI, 11.0);
            text.draw_text(scene, &shortcut, row.right() - 9.0 - w, baseline, UI, 11.0, theme.text_faint);
            menu_hits.push((row, action));
            row_y += MENU_ROW;
        }
        scene.set_layer(LAYER_CONTENT);
        *menu_rect = rect;
    }
    /// An offered update: a pill at the foot of the sidebar, and above it
    /// the card with the release's version, size and date.
    pub(super) fn draw_update(&mut self) {
        self.update_hits.clear();
        self.update_card = Rect::default();
        let Some(offer) = &self.update else { return };
        let sidebar = self.tab_list_visible() && self.strip == TabStrip::Left;
        let area = self.tab_area();
        let (mx, my) = self.mouse;
        let height = self.height;
        let Self { scene, ctx, update_hits, update_card, compact, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        let ascent = text.ui_ascent(12.0);
        // The baseline that centers `size` text in `rect`.
        let centered = |rect: Rect, size: f32| (rect.y + (rect.h + ascent * size / 12.0 * 0.72) / 2.0).round();
        // A deeper blue than the accent, so white text reads on it.
        let blue = theme.accent.mix(Color::hex(0x0a4fd6), 0.65);
        let blue_hover = |rect: Rect| if rect.contains(mx, my) { blue.mix(Color::hex(0xffffff), 0.12) } else { blue };

        // The pill, beside the sidebar's collapse button (above it when compact).
        let pill = if !sidebar {
            None
        } else if *compact {
            Some(Rect::new((area.w - 1.0 - 28.0) / 2.0, area.bottom() - 94.0, 28.0, 26.0))
        } else {
            let label = "Update Available…";
            let w = (text.measure(label, UI_BOLD, 12.0) + 38.0).min(area.w - 48.0 - 34.0);
            Some(Rect::new(40.0, area.bottom() - 34.0, w, 26.0))
        };
        if let Some(pill) = pill {
            scene.quad(Quad::new(pill, blue_hover(pill)).radius(pill.h / 2.0));
            let baseline = centered(pill, 12.0);
            if *compact {
                let w = text.measure("↑", UI_BOLD, 13.0);
                text.draw_text(scene, "↑", pill.x + (pill.w - w) / 2.0, baseline, UI_BOLD, 13.0, Color::hex(0xffffff));
            } else {
                scene.push_clip(pill.inset(10.0, 0.0));
                text.draw_text(scene, "↑", pill.x + 12.0, baseline, UI_BOLD, 12.0, Color::hex(0xffffff));
                text.draw_text(
                    scene,
                    "Update Available…",
                    pill.x + 26.0,
                    baseline,
                    UI_BOLD,
                    12.0,
                    Color::hex(0xffffff),
                );
                scene.pop_clip();
            }
            update_hits.push((pill, UpdateAction::Toggle));
        }
        if !offer.open {
            return;
        }

        const W: f32 = 290.0;
        const PAD: f32 = 14.0;
        const ROW: f32 = 18.0;
        const BUTTON: f32 = 24.0;
        const FOOTER: f32 = 34.0;
        let h = 40.0 + 3.0 * ROW + 12.0 + BUTTON + 14.0 + FOOTER;
        let (x, bottom) = match pill {
            Some(pill) if *compact => (pill.right() + 10.0, pill.bottom() + 8.0),
            Some(pill) => (8.0, pill.y - 9.0),
            None => (12.0, height - 12.0),
        };
        let card = Rect::new(x, bottom - h, W, h);
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(card, theme.shadow(0.55), 10.0, 14.0);
        scene.quad(Quad::new(card, theme.raised).radius(10.0).border(1.0, theme.divider));
        // The card's tail, pointing at the pill.
        if let Some(pill) = pill.filter(|_| !*compact) {
            let cx = pill.x + pill.w / 2.0;
            for i in 0..7 {
                let half = 7.0 - i as f32;
                scene.fill(Rect::new(cx - half, card.bottom() - 1.0 + i as f32, half * 2.0, 1.0), theme.raised);
            }
        }

        text.draw_text(scene, "Update Available", card.x + PAD, card.y + 26.0, UI_BOLD, 13.0, theme.text);
        let release = &offer.release;
        let rows = [
            ("Version:", release.version.clone()),
            ("Size:", release.size_label()),
            ("Released:", release.date_label()),
        ];
        let column = card.x + PAD + text.measure("Released:", UI, 11.5) + 8.0;
        let mut y = card.y + 40.0;
        for (label, value) in rows {
            let baseline = y + 13.0;
            let w = text.measure(label, UI, 11.5);
            text.draw_text(scene, label, column - 6.0 - w, baseline, UI, 11.5, theme.text_dim);
            text.draw_text(scene, &value, column, baseline, UI, 11.5, theme.text);
            y += ROW;
        }

        // Skip and Later on the left, the install on the right.
        y += 12.0;
        let busy = offer.installing;
        let mut button = |scene: &mut Scene, x: f32, label: &str, action: UpdateAction| -> f32 {
            let w = text.measure(label, UI, 11.5) + 22.0;
            let rect = Rect::new(x, y, w, BUTTON);
            let hover = rect.contains(mx, my) && !busy;
            let fill = if hover {
                theme.raised.mix(Color::hex(0xffffff), 0.1)
            } else {
                theme.raised.mix(Color::hex(0xffffff), 0.05)
            };
            scene.quad(Quad::new(rect, fill).radius(6.0).border(1.0, theme.divider));
            let color = if busy { theme.text_faint } else { theme.text };
            text.draw_text(scene, label, rect.x + 11.0, centered(rect, 11.5), UI, 11.5, color);
            update_hits.push((rect, action));
            w
        };
        let skip = button(scene, card.x + PAD, "Skip", UpdateAction::Skip);
        button(scene, card.x + PAD + skip + 8.0, "Later", UpdateAction::Later);
        let label = if busy { "Installing…" } else { "Install and Relaunch" };
        let w = text.measure(label, UI_BOLD, 11.5) + 24.0;
        let install = Rect::new(card.right() - PAD - w, y, w, BUTTON);
        let fill = if busy { blue.alpha(0.6) } else { blue_hover(install) };
        scene.quad(Quad::new(install, fill).radius(6.0));
        text.draw_text(scene, label, install.x + 12.0, centered(install, 11.5), UI_BOLD, 11.5, Color::hex(0xffffff));
        update_hits.push((install, UpdateAction::Install));

        // View Release Notes, under a rule.
        y += BUTTON + 14.0;
        scene.fill(Rect::new(card.x, y, card.w, 1.0), theme.divider);
        let footer = Rect::new(card.x + 1.0, y + 1.0, card.w - 2.0, FOOTER - 2.0);
        if footer.contains(mx, my) {
            scene.quad(Quad::new(footer, theme.text.alpha(0.05)).radius(9.0));
        }
        let baseline = centered(footer, 11.5);
        text.draw_text(scene, "View Release Notes", footer.x + PAD - 1.0, baseline, UI_BOLD, 11.5, theme.text);
        let w = text.measure("↗", UI, 12.0);
        text.draw_text(scene, "↗", footer.right() - PAD - w, baseline, UI, 12.0, theme.text_dim);
        update_hits.push((footer, UpdateAction::Notes));
        scene.set_layer(LAYER_CONTENT);
        *update_card = card;
    }
    /// A card at the top for each phone asking to control this machine:
    /// its name, the code to compare with what it shows, Deny and Allow.
    pub(super) fn draw_asks(&mut self) {
        self.ask_hits.clear();
        let (mx, my) = self.mouse;
        let width = self.width;
        let top = self.top();
        let Self { scene, ctx, asks, ask_hits, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        const W: f32 = 340.0;
        const H: f32 = 132.0;
        let blue = theme.accent.mix(Color::hex(0x0a4fd6), 0.65);
        scene.set_layer(LAYER_OVERLAY);
        for (i, (device, name, code)) in asks.iter().enumerate() {
            let card = Rect::new(((width - W) / 2.0).round(), top + 12.0 + i as f32 * (H + 10.0), W, H);
            scene.shadow(card, theme.shadow(0.55), 10.0, 14.0);
            scene.quad(Quad::new(card, theme.raised).radius(10.0).border(1.0, theme.divider));
            let title = text.elide(&format!("“{name}” wants to control this computer"), UI_BOLD, 13.0, W - 28.0);
            text.draw_text(scene, &title, card.x + 14.0, card.y + 26.0, UI_BOLD, 13.0, theme.text);
            text.draw_text(
                scene,
                "Allow only if it shows this code:",
                card.x + 14.0,
                card.y + 46.0,
                UI,
                11.5,
                theme.text_dim,
            );
            let spaced = format!("{} {}", &code[..3.min(code.len())], &code[3.min(code.len())..]);
            text.draw_text(scene, &spaced, card.x + 14.0, card.y + 80.0, UI_BOLD, 26.0, theme.text);
            let mut x = card.right() - 14.0;
            for (label, allow) in [("Allow", true), ("Deny", false)] {
                let w = text.measure(label, UI_BOLD, 12.0) + 26.0;
                let rect = Rect::new(x - w, card.bottom() - 38.0, w, 26.0);
                let hover = rect.contains(mx, my);
                let fill = match (allow, hover) {
                    (true, false) => blue,
                    (true, true) => blue.mix(Color::hex(0xffffff), 0.12),
                    (false, false) => theme.raised.mix(Color::hex(0xffffff), 0.05),
                    (false, true) => theme.raised.mix(Color::hex(0xffffff), 0.1),
                };
                scene.quad(Quad::new(rect, fill).radius(6.0).border(1.0, if allow { blue } else { theme.divider }));
                let ink = if allow { Color::hex(0xffffff) } else { theme.text };
                let baseline = (rect.y + (rect.h + text.ui_ascent(12.0) * 0.72) / 2.0).round();
                text.draw_text(scene, label, rect.x + 13.0, baseline, UI_BOLD, 12.0, ink);
                ask_hits.push((rect, device.clone(), allow));
                x = rect.x - 8.0;
            }
        }
        scene.set_layer(LAYER_CONTENT);
    }
    pub(super) fn draw_toast(&mut self) {
        let Some((message, until)) = &self.ctx.toast else {
            self.toast_shown = None;
            return;
        };
        let (message, until) = (message.clone(), *until);
        // In: up into place as it fades in. Out: fading over its last moments.
        let now = Instant::now();
        let shown = match &self.toast_shown {
            Some((last, at)) if *last == message => *at,
            _ => now,
        };
        self.toast_shown = Some((message.clone(), shown));
        let (mut fade, short) = self.appear(shown);
        let left = until.saturating_duration_since(now);
        let wake = if left < TOAST_FADE {
            fade = fade.min(left.as_secs_f32() / TOAST_FADE.as_secs_f32());
            now + Duration::from_millis(16)
        } else {
            until - TOAST_FADE
        };
        self.animation_at = Some(self.animation_at.map_or(wake, |a| a.min(wake)));
        let (right, bottom) = match self.focused_id().and_then(|id| self.panes.iter().find(|p| p.id == id)) {
            Some(pane) => (pane.rect.right(), pane.rect.y + pane.input_top()),
            None => (self.width, self.height),
        };
        let Self { scene, ctx, .. } = self;
        let Ctx { text, theme, .. } = ctx;
        scene.set_layer(LAYER_OVERLAY);
        scene.set_fade(fade);
        // Capped at the room from the window's left margin to the anchor:
        // toasts carry error strings (a failed save names its whole path),
        // and an uncapped pill would start off-screen, hiding the start
        // of the very message it exists to show.
        let w = (text.measure(&message, UI, 12.0) + 24.0).min((right - 28.0).max(0.0));
        let message = text.elide(&message, UI, 12.0, (w - 24.0).max(0.0));
        let rect = Rect::new(right - w - 14.0, bottom - 40.0 + short, w, 28.0);
        scene.shadow(rect, theme.shadow(0.5), 14.0, 8.0);
        scene.quad(Quad::new(rect, theme.raised).radius(14.0).border(1.0, theme.divider));
        text.draw_text(scene, &message, rect.x + 12.0, rect.y + 18.0, UI, 12.0, theme.text);
        scene.set_fade(1.0);
        scene.set_layer(LAYER_CONTENT);
    }
}

/// Who's signed in to Remote Control here: their first name, and their
/// photo where the sign-in gave one (Google's does, Apple's doesn't).
pub(super) fn account_info() -> Option<(String, Option<String>)> {
    #[cfg(not(target_os = "haiku"))]
    if crate::relay::signed_in() {
        let name = crate::setting("account_name").filter(|n| !n.is_empty()).unwrap_or_else(|| "Signed in".to_owned());
        let picture = crate::setting("account_picture").filter(|p| p.starts_with("https://"));
        if let Some(picture) = &picture {
            crate::avatar::fetch(picture);
        }
        return Some((name, picture));
    }
    None
}

/// The account's face in `rect` (a square): the photo, else the name's
/// initial on the accent; signed out, the outline of a person.
pub(super) fn draw_account_face(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    rect: Rect,
    who: Option<&(String, Option<String>)>,
) {
    let size = rect.w;
    let Some((name, picture)) = who else {
        scene.quad(Quad::new(rect, Color::hex(0x000000).alpha(0.0)).radius(size / 2.0).border(1.5, theme.text_faint));
        let head = size * 0.32;
        scene.quad(
            Quad::new(Rect::new(rect.x + (size - head) / 2.0, rect.y + size * 0.2, head, head), theme.text_faint)
                .radius(head / 2.0),
        );
        let body = Rect::new(rect.x + size * 0.24, rect.y + size * 0.58, size * 0.52, size * 0.26);
        scene.quad(Quad::new(body, theme.text_faint).radius(size * 0.13));
        return;
    };
    #[cfg(not(target_os = "haiku"))]
    let photo = picture.as_deref().and_then(crate::avatar::get);
    #[cfg(target_os = "haiku")]
    let photo: Option<Arc<Picture>> = {
        let _ = picture;
        None
    };
    match photo {
        Some(photo) => text.draw_picture(scene, &photo, rect),
        None => {
            scene.quad(Quad::new(rect, theme.accent).radius(size / 2.0));
            let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            let font = size * 0.55;
            let w = text.measure(&initial, UI_BOLD, font);
            let baseline = (rect.y + (rect.h + text.ui_ascent(font) * 0.72) / 2.0).round();
            text.draw_text(scene, &initial, rect.x + (rect.w - w) / 2.0, baseline, UI_BOLD, font, Color::hex(0x0b0d12));
        }
    }
}

/// Over pane `rect`: an accent outline, and at the top right a pill for
/// each person in it from a phone, their photo (or initial) and name.
pub fn draw_people(scene: &mut Scene, text: &mut TextSystem, theme: &Theme, rect: Rect, people: &[remote::Who]) {
    scene.set_layer(LAYER_OVERLAY);
    scene.quad(Quad::new(rect.inset(1.0, 1.0), Color::TRANSPARENT).radius(6.0).border(2.0, theme.accent));
    let (size, avatar, pad) = (11.5, 18.0, 4.0);
    let ascent = text.ui_ascent(size);
    let mut right = rect.right() - 10.0;
    for who in people {
        let label = text.measure(&who.name, UI, size);
        let x = right - (pad + avatar + 7.0 + label + 10.0);
        let pill = Rect::new(x, rect.y + 8.0, right - x, avatar + 2.0 * pad);
        scene.shadow(pill, theme.shadow(0.3), pill.h / 2.0, 6.0);
        scene.quad(Quad::new(pill, theme.raised).radius(pill.h / 2.0).border(1.0, theme.accent));
        let face = Rect::new(pill.x + pad, pill.y + pad, avatar, avatar);
        #[cfg(not(target_os = "haiku"))]
        let photo = who.picture.as_deref().and_then(crate::avatar::get);
        #[cfg(target_os = "haiku")]
        let photo: Option<std::sync::Arc<crate::picture::Picture>> = None;
        match photo {
            Some(photo) => text.draw_picture(scene, &photo, face),
            None => {
                // No photo (Apple's sign-in has none): their initial.
                scene.quad(Quad::new(face, theme.accent).radius(avatar / 2.0));
                let initial: String = who.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                let w = text.measure(&initial, UI_BOLD, 10.5);
                let baseline = (face.y + (face.h + text.ui_ascent(10.5) * 0.72) / 2.0).round();
                text.draw_text(
                    scene,
                    &initial,
                    face.x + (face.w - w) / 2.0,
                    baseline,
                    UI_BOLD,
                    10.5,
                    Color::hex(0x0b0d12),
                );
            }
        }
        let baseline = (pill.y + (pill.h + ascent * 0.72) / 2.0).round();
        text.draw_text(scene, &who.name, face.right() + 7.0, baseline, UI, size, theme.text);
        right = pill.x - 6.0;
    }
    scene.set_layer(LAYER_CONTENT);
}

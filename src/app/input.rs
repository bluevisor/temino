use super::*;

impl App {
    pub(super) fn ui_event(&mut self, event: UiEvent) {
        self.dirty = true;
        match event {
            UiEvent::Key(key) => {
                self.ctx.blink_epoch = Instant::now();
                self.key(key);
            }
            UiEvent::Resize { width, height, scale } => {
                if (width as f32, height as f32) != self.window || scale != self.scale {
                    crate::debug_log!("window: {width}x{height} at {scale}x");
                }
                self.window = (width as f32, height as f32);
                self.scale = scale;
                self.set_zoom(self.zoom, false);
            }
            UiEvent::Modifiers(mods) => self.modifiers_changed(mods),
            UiEvent::Focus(focused) => {
                let now = Instant::now();
                self.focused = focused;
                self.ctx.blink_epoch = now;
                if let Some(id) = self.focused_id() {
                    let note = self.mark_note(id, now);
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                        pane.focus_changed(focused);
                        // Back from another app counts as coming back to the pane.
                        if focused {
                            pane.focus_gained(now, Some(recap_idle()), note);
                        } else {
                            pane.focus_lost(now);
                        }
                    }
                }
                self.dirty = true;
            }
            UiEvent::Close => {
                if !self.unsaved_blocks_quit() {
                    self.close_window()
                }
            }
            UiEvent::Quit => {
                // Another window quits the app; one with unsaved work stays.
                if !self.unsaved_blocks_quit() {
                    self.shut_down();
                    self.leave = Some(platform::close_window);
                }
            }
            UiEvent::Primary => {
                if !self.quitting {
                    self.primary = true;
                    self.save_tabs();
                }
            }
            UiEvent::Text(text) => {
                self.preedit.clear();
                self.ctx.blink_epoch = Instant::now();
                if let Some((_, field, selected)) = self.field.as_mut() {
                    if std::mem::take(selected) {
                        field.clear();
                    }
                    field.extend(text.chars().filter(|c| !c.is_control()));
                    self.dirty = true;
                    return;
                }
                if let Some(id) = self.focused_id() {
                    if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
                        pane.text_input(&text);
                    }
                }
            }
            UiEvent::Preedit(text) => self.preedit = text,
            UiEvent::Background(index) => self.set_background(index),
            UiEvent::Shell(index) => self.set_shell(index),
            UiEvent::Wheel { dy, x, y, precise, .. } => {
                let (x, y) = (x / self.zoom, y / self.zoom);
                if let Some((SETTINGS, _)) = self.pane_at(x, y) {
                    self.settings.wheel(dy, x, y);
                } else if let Some((id, rect)) = self.pane_at(x, y) {
                    let Self { panes, ctx, .. } = self;
                    if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                        pane.wheel(ctx, dy, precise, x - rect.x, y - rect.y);
                    }
                }
            }
            UiEvent::MouseDown { x, y, buttons, clicks, mods } => {
                self.mouse_down(x / self.zoom, y / self.zoom, buttons, clicks, mods)
            }
            UiEvent::MouseMoved { x, y, buttons } => self.mouse_moved(x / self.zoom, y / self.zoom, buttons),
            UiEvent::MouseUp { x, y } => self.mouse_up(x / self.zoom, y / self.zoom),
        }
    }
    /// A key: the keys file's binding for it, else the default.
    pub(super) fn key(&mut self, key: KeyEvent) {
        // Settings ▸ Keys waiting for keys takes them, shortcuts and all.
        if self.record_key(&key) {
            return;
        }
        if self.renaming.is_none() && self.field.is_none() {
            if self.bindings.refresh() && !self.bindings.errors.is_empty() {
                let errors = &self.bindings.errors;
                self.ctx.show_toast(&format!("Keys file: {} not understood ({})", errors.len(), errors[0]));
            }
            if let Some(bind) = self.bindings.find(&key).cloned() {
                self.dirty = true;
                match bind {
                    Bind::Ignore => {}
                    Bind::Action(action) => self.run_action(action),
                    Bind::Text(text) => self.type_text(&text),
                }
                return;
            }
        }
        self.default_key(key);
    }
    /// A bound action: its default shortcut, sent past the bindings.
    pub(super) fn run_action(&mut self, action: &str) {
        match action {
            "quick_terminal" => platform::toggle_quick_terminal(),
            _ => {
                if let Some(event) = bindings::default_event(action) {
                    self.default_key(event);
                }
            }
        }
    }
    /// A `text:` binding: typed into the focused pane; a line end at the
    /// prompt runs the line.
    /// A `text:` binding: typed into the command editor, or sent to the
    /// running program byte for byte.
    pub(super) fn type_text(&mut self, bytes: &[u8]) {
        let Some(id) = self.focused_id() else { return };
        let Self { panes, ctx, .. } = self;
        let Some(pane) = panes.iter_mut().find(|p| p.id == id) else { return };
        let text = String::from_utf8_lossy(bytes);
        if pane.session.active().is_some() {
            pane.type_bytes(bytes);
        } else if pane.session.editor_active() {
            for (i, line) in text.split(['\r', '\n']).enumerate() {
                if i > 0 {
                    let enter = KeyEvent { named: Some(Named::Enter), mods: 0, raw_char: 13, bytes: vec![b'\r'] };
                    pane.key(ctx, enter);
                }
                pane.text_input(line);
            }
        } else {
            pane.text_input(&text);
        }
    }
    /// Window shortcuts (tabs, splits, font size); the rest go to the pane.
    pub(super) fn default_key(&mut self, key: KeyEvent) {
        if self.tab_menu.take().is_some() && key.named == Some(Named::Escape) {
            return;
        }
        if key.named == Some(Named::Escape) {
            if self.settings.open.take().is_some() {
                self.dirty = true;
                return;
            }
            if let Some(offer) = self.update.as_mut().filter(|offer| offer.open) {
                offer.open = false;
                return;
            }
        }
        if self.rename_key(&key) || self.field_key(&key) {
            return;
        }
        let named = key.named;
        let several_tabs = self.tabs.len() > 1;
        if key.ctrl() && !key.cmd() {
            let handled = match named {
                Some(Named::Tab) => {
                    self.step_tab(if key.shift() { -1 } else { 1 });
                    true
                }
                Some(Named::PageUp) if several_tabs && !key.shift() => {
                    self.step_tab(-1);
                    true
                }
                Some(Named::PageDown) if several_tabs && !key.shift() => {
                    self.step_tab(1);
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        if key.cmd() && !key.ctrl() {
            let handled = match (named, key.base_char()) {
                (Some(Named::Left), _) if key.option() => {
                    self.focus_toward(-1.0, 0.0);
                    true
                }
                (Some(Named::Right), _) if key.option() => {
                    self.focus_toward(1.0, 0.0);
                    true
                }
                (Some(Named::Up), _) if key.option() => {
                    self.focus_toward(0.0, -1.0);
                    true
                }
                (Some(Named::Down), _) if key.option() => {
                    self.focus_toward(0.0, 1.0);
                    true
                }
                (Some(Named::Enter), _) => {
                    self.toggle_zoom();
                    true
                }
                (Some(Named::PageUp), _) => {
                    self.step_tab(-1);
                    true
                }
                (Some(Named::PageDown), _) => {
                    self.step_tab(1);
                    true
                }
                (_, Some('t')) if key.option() => {
                    self.open_browser(true);
                    true
                }
                (_, Some('b')) if key.option() => {
                    self.open_browser(false);
                    true
                }
                (_, Some('t')) => {
                    if let Err(error) = self.new_tab() {
                        self.ctx.show_toast(&error);
                    }
                    true
                }
                (_, Some('n')) => {
                    // Another window, its first tab where you are.
                    let cwd = self.focused_id().and_then(|id| self.panes.iter().find(|p| p.id == id));
                    platform::new_window(cwd.map(|pane| pane.session.cwd.clone()));
                    true
                }
                (_, Some('i')) if key.shift() => {
                    self.toggle_mark(self.active, Mark::Important);
                    true
                }
                (_, Some('l')) if key.shift() => {
                    self.toggle_mark(self.active, Mark::Later);
                    true
                }
                (_, Some('l')) => {
                    self.toggle_mark(self.active, Mark::Waiting);
                    true
                }
                (_, Some('i')) => {
                    // "Where was I?" on demand.
                    if let Some(id) = self.focused_id() {
                        let now = Instant::now();
                        let note = self.mark_note(id, now);
                        if let Some(pane) = self.pane(id) {
                            pane.focus_gained(now, None, note);
                        }
                    }
                    true
                }
                (_, Some('b')) if key.shift() => {
                    self.set_extended(!self.extended);
                    true
                }
                (_, Some('b')) => {
                    self.toggle_tab_list();
                    true
                }
                (_, Some('d')) => {
                    self.split(Axis::Row, false);
                    true
                }
                (_, Some('e')) => {
                    self.split(Axis::Column, key.shift());
                    true
                }
                (_, Some('w')) => {
                    if let Some(id) = self.focused_id() {
                        self.close_pane(id);
                    }
                    true
                }
                (_, Some('q')) => {
                    if !self.unsaved_blocks_quit() {
                        self.quit();
                    }
                    true
                }
                (_, Some(',')) => {
                    self.open_settings();
                    true
                }
                (_, Some('u')) => {
                    self.check_for_updates();
                    true
                }
                // Command+Shift+[ ] (macOS-style) moves between tabs.
                (_, Some('[' | '{')) if key.shift() => {
                    self.step_tab(-1);
                    true
                }
                (_, Some(']' | '}')) if key.shift() => {
                    self.step_tab(1);
                    true
                }
                (_, Some('[')) => {
                    self.cycle_focus(-1);
                    true
                }
                (_, Some(']')) => {
                    self.cycle_focus(1);
                    true
                }
                (_, Some('`' | '~')) => {
                    self.jump_to_needs(key.shift());
                    true
                }
                (_, Some(c @ '0'..='9')) if self.tab_digit(c as usize - '0' as usize) => true,
                (_, Some('=' | '+')) => {
                    self.step_font_size(1.0);
                    true
                }
                (_, Some('-')) => {
                    self.step_font_size(-1.0);
                    true
                }
                (_, Some('0')) => {
                    self.change_font_size(DEFAULT_FONT_SIZE);
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        let Some(id) = self.focused_id() else { return };
        // Typing in a tab set aside means it's being dealt with.
        if let Some(tab) = self.tabs.get_mut(self.active).filter(|tab| tab.mark.is_some_and(Mark::aside)) {
            tab.mark = None;
            self.save_tabs();
        }
        let Self { panes, ctx, .. } = self;
        if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
            pane.key(ctx, key);
        }
    }
    pub(super) fn mouse_down(&mut self, x: f32, y: f32, pressed: i32, clicks: i32, mods: u32) {
        // A click off the field being typed in ends it.
        let on_field = matches!(self.pane_at(x, y), Some((SETTINGS, _)))
            && matches!(self.settings.action_at(x, y), Some(Action::Edit(_)));
        if !on_field {
            self.finish_field();
        }
        let in_settings = matches!(self.pane_at(x, y), Some((SETTINGS, _)));
        let action = if in_settings { self.settings.action_at(x, y) } else { None };
        // A click anywhere but a shortcut stops waiting for keys.
        if self.recording.is_some() && !matches!(action, Some(Action::RecordKey(_))) {
            self.recording = None;
            self.dirty = true;
        }
        // A click off the open list closes it, and does nothing else.
        if self.settings.open.is_some() {
            let on_list = matches!(
                action,
                Some(Action::PickTheme(_) | Action::PickFont(_) | Action::List(_) | Action::Customize)
            );
            if !on_list {
                self.settings.open = None;
                self.dirty = true;
                return;
            }
        }
        if let Some(menu) = self.tab_menu.take() {
            // A click anywhere else only closes the menu.
            if let Some(&(_, action)) = self.menu_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.menu_action(menu.tab, action);
            } else if self.menu_rect.contains(x, y) {
                self.tab_menu = Some(menu);
            }
            return;
        }
        if let Some((_, device, allow)) = self.ask_hits.iter().find(|(r, _, _)| r.contains(x, y)).cloned() {
            crate::relay::decide(&device, allow);
            self.asks.retain(|(d, _, _)| *d != device);
            self.dirty = true;
            return;
        }
        if let Some(&(_, action)) = self.consent_hits.iter().find(|(r, _)| r.contains(x, y)) {
            self.consent_action(action);
            return;
        }
        if self.consent_card.contains(x, y) {
            return;
        }
        // A click elsewhere puts the consent card away without an
        // answer, and goes on to what it hit; it asks again next launch.
        if let Some(offer) = self.consent.as_mut().filter(|offer| offer.open) {
            offer.open = false;
        }
        if let Some(&(_, action)) = self.update_hits.iter().find(|(r, _)| r.contains(x, y)) {
            self.update_action(action);
            return;
        }
        if self.update_card.contains(x, y) {
            return;
        }
        // A click elsewhere puts the card away, and goes on to what it hit.
        if let Some(offer) = self.update.as_mut().filter(|offer| offer.open && !offer.installing) {
            offer.open = false;
        }
        let on_renamed = self
            .renaming
            .as_ref()
            .is_some_and(|rename| self.tab_hits.iter().any(|(r, i)| *i == rename.tab && r.contains(x, y)));
        if !on_renamed {
            self.finish_rename();
        }
        if self.sidebar_edge().contains(x, y) {
            self.pointer = Pointer::Sidebar;
            return;
        }
        if let Some(&(_, id)) = self.header_hits.iter().find(|(r, _)| r.contains(x, y)) {
            // An agent's header, or its extension: open or close it, and go
            // to that pane.
            if let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == id) {
                pane.header_open = !pane.header_open;
            }
            if self.focused_id() != Some(id) {
                self.set_focus(id);
            }
            self.dirty = true;
            return;
        }
        // Control+click is the secondary click on macOS.
        let secondary =
            pressed & buttons::SECONDARY != 0 || (cfg!(target_os = "macos") && mods & platform::mods::CONTROL != 0);
        if let Some(&(_, tab, id)) = self.pane_hits.iter().find(|(r, _, _)| r.contains(x, y)) {
            // A pane's line in the extended sidebar, or its agent header:
            // go to that pane. A right click on the line is the tab's, as
            // on the rest of its row: its menu.
            if tab != self.active {
                self.switch_tab(tab);
            }
            if self.focused_id() != Some(id) {
                self.set_focus(id);
            }
            if secondary && self.tab_area().contains(x, y) {
                self.open_tab_menu(tab, x, y + 2.0);
                return;
            }
            if self.tab_area().contains(x, y) {
                self.pointer = Pointer::Tab { index: tab, x, y, dragging: false };
            }
            return;
        }
        if self.tab_area().contains(x, y) {
            if self.collapse_hit.contains(x, y) {
                self.set_compact(!self.compact);
                return;
            }
            if self.account_hit.contains(x, y) {
                let hit = self.account_hit;
                self.tab_menu = Some(TabMenu {
                    tab: self.active,
                    x: hit.x,
                    y: hit.y - 6.0,
                    kind: MenuKind::Account,
                    opened: Instant::now(),
                });
                self.dirty = true;
                return;
            }
            if let Some(&(_, index)) = self.close_hits.iter().find(|(r, _)| r.contains(x, y)) {
                self.close_tab(index);
            } else if let Some(&(_, index)) = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)) {
                if secondary {
                    // The tab's menu opens at the pointer.
                    self.open_tab_menu(index, x, y + 2.0);
                } else if pressed & buttons::TERTIARY != 0 {
                    self.close_tab(index);
                } else if clicks == 2 && !on_renamed {
                    self.start_rename(index);
                } else if !on_renamed {
                    self.switch_tab(index);
                    self.pointer = Pointer::Tab { index, x, y, dragging: false };
                }
            } else if self.new_tab_hit.contains(x, y) {
                if let Err(error) = self.new_tab() {
                    self.ctx.show_toast(&error);
                }
            }
            return;
        }
        if let Some(divider) = self.dividers.iter().find(|d| d.grab_area().contains(x, y)) {
            self.pointer = Pointer::Divider(divider.clone());
            return;
        }
        let Some((id, rect)) = self.pane_at(x, y) else { return };
        if id == SETTINGS {
            if let Some(action) = self.settings.action_at(x, y) {
                self.settings_action(action);
            }
            return;
        }
        if self.focused_id() != Some(id) {
            self.set_focus(id);
        }
        // A right click (Control+click on macOS) opens the pane's menu,
        // unless the program takes the mouse itself.
        let secondary =
            pressed & buttons::SECONDARY != 0 || (cfg!(target_os = "macos") && mods & platform::mods::CONTROL != 0);
        let reports = self.panes.iter().find(|p| p.id == id).is_some_and(|p| p.reports_mouse());
        if secondary && !(reports && mods & platform::mods::SHIFT == 0) {
            self.finish_rename();
            self.tab_menu = Some(TabMenu { tab: self.active, x, y, kind: MenuKind::Pane, opened: Instant::now() });
            return;
        }
        self.pointer = Pointer::Pane(id);
        let Self { panes, ctx, .. } = self;
        if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
            pane.mouse_down(ctx, x - rect.x, y - rect.y, pressed, clicks, mods);
        }
    }
    /// Option pressed while dragging a divider turns its split, side by side
    /// to stacked and back, the way Option turns a guide being dragged in
    /// Photoshop; the divider then follows the pointer the new way.
    pub(super) fn modifiers_changed(&mut self, mods: u32) {
        let pressed = mods & platform::mods::OPTION != 0 && self.held & platform::mods::OPTION == 0;
        if self.tab_menu.is_some() && (mods ^ self.held) & platform::mods::OPTION != 0 {
            // The menu's colors swap to their alternates while Option is held.
            self.dirty = true;
        }
        self.held = mods;
        if !pressed {
            return;
        }
        let Pointer::Divider(divider) = &mut self.pointer else { return };
        let axis = if divider.axis == Axis::Row { Axis::Column } else { Axis::Row };
        divider.axis = axis;
        let path = divider.path.clone();
        if let Some(Node::Split { axis: a, .. }) = self.tabs[self.active].root.at_path(&path) {
            *a = axis;
        }
        self.set_cursor(if axis == Axis::Row { Cursor::ResizeColumn } else { Cursor::ResizeRow });
        let (x, y) = self.mouse;
        self.mouse_moved(x, y, buttons::PRIMARY);
        self.relayout();
        self.save_tabs();
    }
    pub(super) fn mouse_moved(&mut self, x: f32, y: f32, pressed: i32) {
        let edge_hover = |px, py| {
            self.sidebar_edge().contains(px, py) || self.dividers.iter().any(|d| d.grab_area().contains(px, py))
        };
        if edge_hover(self.mouse.0, self.mouse.1) || edge_hover(x, y) {
            self.dirty = true;
        }
        self.mouse = (x, y);
        // A choice under the pointer previews, and one left stops (see `preview`).
        let previewable = matches!(self.pane_at(x, y), Some((SETTINGS, _)))
            && self.settings.action_at(x, y).and_then(|a| settings::Preview::of(a, self.settings.open)).is_some();
        if previewable || self.previewing.is_some() || self.settings.open.is_some() {
            self.dirty = true;
        }
        if !self.consent_hits.is_empty() {
            self.dirty = true;
            if self.consent_card.contains(x, y) || self.consent_hits.iter().any(|(r, _)| r.contains(x, y)) {
                self.set_cursor(Cursor::Arrow);
                return;
            }
        }
        if !self.update_hits.is_empty() {
            // The pill and the card's buttons light up under the pointer.
            self.dirty = true;
            if self.update_card.contains(x, y) || self.update_hits.iter().any(|(r, _)| r.contains(x, y)) {
                self.set_cursor(Cursor::Arrow);
                return;
            }
        }
        if let Pointer::Tab { index, x: x0, y: y0, dragging } = self.pointer {
            if pressed == 0 {
                self.pointer = Pointer::Idle;
            } else {
                let dragging = dragging || (x - x0).abs().max((y - y0).abs()) > DRAG_SLOP;
                let mut index = index;
                if dragging {
                    if let Some(to) = self.tab_slot_at(x, y).filter(|&to| to != index) {
                        self.move_tab(index, to);
                        index = to;
                    }
                    self.set_cursor(Cursor::Arrow);
                }
                self.pointer = Pointer::Tab { index, x: x0, y: y0, dragging };
                return;
            }
        }
        let hover = self.tab_hits.iter().find(|(r, _)| r.contains(x, y)).map(|&(_, index)| index);
        if hover != self.hover_tab {
            self.dirty = true;
            self.hover_at = Instant::now();
        }
        self.hover_tab = hover;
        match &self.pointer {
            Pointer::Divider(divider) if pressed != 0 => {
                let divider = divider.clone();
                let parent = divider.parent;
                let ratio = match divider.axis {
                    Axis::Row => (x - parent.x) / parent.w,
                    Axis::Column => (y - parent.y) / parent.h,
                };
                let extent = if divider.axis == Axis::Row { parent.w } else { parent.h };
                let low = (MIN_PANE / extent).min(0.5);
                let ratio = ratio.clamp(low, 1.0 - low);
                let ratio = if ((ratio - 0.5) * extent).abs() < MIDDLE_SNAP { 0.5 } else { ratio };
                let tab = &mut self.tabs[self.active];
                if let Some(Node::Split { ratio: r, .. }) = tab.root.at_path(&divider.path) {
                    *r = ratio;
                }
                self.relayout();
                return;
            }
            Pointer::Sidebar if pressed != 0 => {
                self.compact = x < SIDEBAR_SNAP;
                if !self.compact {
                    self.sidebar_width = sidebar_drag_width(x, self.width);
                }
                self.set_cursor(Cursor::ResizeColumn);
                self.relayout();
                return;
            }
            Pointer::Pane(id) if pressed != 0 => {
                let id = *id;
                let Self { panes, ctx, .. } = self;
                if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                    let (ox, oy) = (pane.rect.x, pane.rect.y);
                    pane.mouse_moved(ctx, x - ox, y - oy, pressed);
                }
                return;
            }
            _ => {}
        }
        // Only the pane under the pointer underlines a link.
        let over_chrome = self.sidebar_edge().contains(x, y)
            || self.tab_area().contains(x, y)
            || self.dividers.iter().any(|d| d.grab_area().contains(x, y));
        let hovered = self.pane_at(x, y).map(|(id, _)| id).filter(|_| !over_chrome);
        for pane in self.panes.iter_mut().filter(|pane| Some(pane.id) != hovered) {
            pane.pointer_left();
        }
        if self.sidebar_edge().contains(x, y) {
            self.set_cursor(Cursor::ResizeColumn);
            return;
        }
        if self.tab_area().contains(x, y) {
            self.set_cursor(Cursor::Arrow);
            // The collapse button and compact rows' titles follow the pointer.
            self.dirty = true;
            return;
        }
        if let Some(divider) = self.dividers.iter().find(|d| d.grab_area().contains(x, y)) {
            let cursor = if divider.axis == Axis::Row { Cursor::ResizeColumn } else { Cursor::ResizeRow };
            self.set_cursor(cursor);
            return;
        }
        let header = self.header_hits.iter().find(|(r, _)| r.contains(x, y)).map(|&(_, id)| id);
        if header != self.header_hover {
            // The header's toggle lights up under the pointer.
            self.header_hover = header;
            self.dirty = true;
        }
        if header.is_some() {
            self.set_cursor(Cursor::Arrow);
            return;
        }
        let Some((id, rect)) = self.pane_at(x, y) else { return };
        if id == SETTINGS {
            // Hover highlights follow the pointer.
            self.set_cursor(Cursor::Arrow);
            self.dirty = true;
            return;
        }
        let Self { panes, ctx, .. } = self;
        let cursor = panes.iter_mut().find(|p| p.id == id).map(|pane| pane.mouse_moved(ctx, x - rect.x, y - rect.y, 0));
        if let Some(cursor) = cursor {
            self.set_cursor(cursor);
        }
    }
    pub(super) fn mouse_up(&mut self, x: f32, y: f32) {
        let pointer = std::mem::replace(&mut self.pointer, Pointer::Idle);
        if matches!(pointer, Pointer::Tab { dragging: true, .. } | Pointer::Divider(_)) {
            self.save_tabs();
        }
        if matches!(pointer, Pointer::Sidebar) {
            self.save_sidebar();
        }
        if let Pointer::Pane(id) = pointer {
            let Self { panes, ctx, .. } = self;
            let mut link = None;
            if let Some(pane) = panes.iter_mut().find(|p| p.id == id) {
                let (ox, oy) = (pane.rect.x, pane.rect.y);
                pane.mouse_up(ctx, x - ox, y - oy);
                link = pane.clicked_link.take();
            }
            if let Some(url) = link {
                self.open_link(id, url);
            }
        }
    }
    /// The input method's composition, over the text at the cursor, underlined.
    pub(super) fn draw_preedit(&mut self) {
        let Some(caret) = self.scene.caret else { return };
        if self.preedit.is_empty() {
            return;
        }
        let Self { scene, ctx, preedit, .. } = self;
        let cell = ctx.text.cell;
        let width = crate::editor::columns(preedit) as f32 * cell.width;
        scene.set_origin(0.0, 0.0);
        scene.set_layer(LAYER_OVERLAY);
        scene.fill(Rect::new(caret.x, caret.y, width, cell.height), ctx.theme.background);
        let mut x = caret.x;
        for c in preedit.chars() {
            let span = crate::editor::width(c);
            ctx.text.draw_cell(scene, c, x, caret.y, crate::text::Style::default(), ctx.theme.text, span.max(1));
            x += span as f32 * cell.width;
        }
        scene.fill(Rect::new(caret.x, caret.y + cell.height - 2.0, width, 1.0), ctx.theme.text);
        scene.set_layer(LAYER_CONTENT);
    }
    /// Tells the platform where the cursor is, so an input method's
    /// candidate window opens beside it.
    pub(super) fn report_caret(&mut self) {
        let Some(caret) = self.scene.caret else { return };
        let zoom = self.zoom;
        let at = [(caret.x * zoom) as i32, (caret.y * zoom) as i32, (caret.h * zoom) as i32];
        if self.caret_sent != Some(at) {
            self.caret_sent = Some(at);
            platform::set_text_caret(at[0], at[1], at[2]);
        }
    }
}

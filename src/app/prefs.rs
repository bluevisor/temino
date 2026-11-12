use super::*;

impl App {
    /// Light or Dark: the one picked, else the theme's own.
    pub(super) fn shown_mode(&self) -> Mode {
        self.mode.unwrap_or_else(|| self.themes.get(self.palette).map_or(Mode::Dark, Palette::mode))
    }
    /// Settings ▸ Appearance ▸ Light or dark: every theme in `mode` from now on.
    pub(super) fn set_mode(&mut self, mode: Mode) {
        self.mode = Some(mode);
        crate::save_setting("appearance", &mode.name().to_lowercase());
        self.apply_theme();
    }
    pub(super) fn set_background(&mut self, index: usize) {
        let Some(&(name, _)) = backgrounds(self.shown_mode()).get(index) else { return };
        self.background = Some(index);
        crate::save_setting("background", name);
        self.apply_theme();
    }
    /// While the theme list is open, the theme under the pointer shows
    /// everywhere at once, as picking it would; off the list, or with it
    /// closed, the theme in use is back.
    pub(super) fn preview_theme(&mut self) {
        let (x, y) = self.mouse;
        let hovered = match self.settings.action_at(x, y) {
            Some(Action::PickTheme(index))
                if self.settings.open == Some(List::Themes) && matches!(self.pane_at(x, y), Some((SETTINGS, _))) =>
            {
                Some(index)
            }
            _ => None,
        };
        if hovered == self.previewing {
            return;
        }
        self.previewing = hovered;
        match hovered.and_then(|index| self.themes.get(index)) {
            Some(palette) => {
                // Picked, a theme drops the background preset.
                let mode = self.mode.unwrap_or_else(|| palette.mode());
                self.ctx.theme = build_theme(palette, None, self.ctx.theme.opacity, mode);
                platform::set_background(self.ctx.theme.background.to_rgb8());
                self.dirty = true;
            }
            None => self.apply_theme(),
        }
    }
    /// The fonts to pick from: the bundled JetBrains Mono (None) first,
    /// then every installed monospaced family.
    pub(super) fn font_choices() -> Vec<Option<String>> {
        let mut choices: Vec<Option<String>> = vec![None];
        // Not the system's hidden faces (".LastResort") or emoji.
        let usable = |f: &String| f != "JetBrains Mono" && !f.starts_with('.') && !f.contains("Emoji");
        choices.extend(crate::fonts::families().into_iter().filter(usable).map(Some));
        choices
    }
    /// Where the font in use is in `choices`.
    fn font_at(choices: &[Option<String>]) -> usize {
        let current = crate::setting("font").filter(|f| !f.is_empty());
        choices.iter().position(|c| *c == current).unwrap_or(0)
    }
    /// Settings ▸ Text ▸ Font: the monospaced family `step` places along
    /// the installed ones, the bundled JetBrains Mono first.
    pub(super) fn step_font(&mut self, step: i32) {
        let choices = App::font_choices();
        let at = App::font_at(&choices) as i32;
        let n = choices.len() as i32;
        // A family that won't load is passed over, in the same direction.
        for tries in 1..n {
            let next = &choices[(at + step * tries).rem_euclid(n) as usize];
            if self.use_font(next.as_deref()) {
                return;
            }
        }
        self.ctx.show_toast("No other monospaced fonts to choose from");
    }
    /// The font at `index` in the font list, picked.
    pub(super) fn pick_font(&mut self, index: usize) {
        let Some(choice) = self.font_choices.get(index).cloned() else { return };
        if !self.use_font(choice.as_deref()) {
            self.ctx.show_toast(&format!("{} won't load", choice.as_deref().unwrap_or("JetBrains Mono")));
        }
    }
    /// The terminal's font, if it loads; false if it doesn't.
    fn use_font(&mut self, family: Option<&str>) -> bool {
        match self.ctx.text.set_mono(family) {
            Ok(()) => {
                crate::save_setting("font", family.unwrap_or(""));
                for pane in &mut self.panes {
                    pane.cells_changed(&self.ctx.text);
                }
                self.relayout();
                true
            }
            Err(error) => {
                crate::trace(&format!("font {family:?}: {error}"));
                false
            }
        }
    }
    /// Settings ▸ Theme: the scheme `step` places along the library; its
    /// own background replaces one picked before.
    pub(super) fn step_theme(&mut self, step: i32) {
        self.themes = theme::library(&themes_dir());
        let n = self.themes.len() as i32;
        self.palette = (self.palette as i32 + step).rem_euclid(n) as usize;
        self.background = None;
        crate::save_setting("background", "");
        crate::save_setting("theme", &self.themes[self.palette].name);
        // The editor stays open from one theme of your own to the next.
        self.open_theme_editor(self.theme_edit.is_some());
        self.apply_theme();
    }
    /// `quick_terminal=<keys>` in the settings file: the hotkey that brings
    /// the window from anywhere (off when unset or `off`).
    pub(super) fn quick_terminal_key() -> Option<String> {
        crate::setting("quick_terminal").filter(|keys| !keys.is_empty() && keys != "off")
    }
    pub(super) fn apply_quick_terminal(&mut self) {
        let Some(keys) = App::quick_terminal_key() else {
            platform::set_quick_terminal(None);
            return;
        };
        let combo = bindings::parse_combo(&keys);
        let registered = combo.is_some() && platform::set_quick_terminal(combo.as_ref());
        if !registered && platform::QUICK_TERMINAL {
            self.ctx.show_toast(&format!("Quick terminal: can't use {keys} as a hotkey"));
        }
    }
    pub(super) fn apply_theme(&mut self) {
        let mode = self.shown_mode();
        let palette = self.themes.get(self.palette).unwrap_or(&self.themes[0]);
        self.ctx.theme = build_theme(palette, self.background, self.ctx.theme.opacity, mode);
        platform::set_background(self.ctx.theme.background.to_rgb8());
        let names = backgrounds(mode).map(|(name, _)| name);
        platform::set_background_menu(&names, self.background.unwrap_or(usize::MAX));
        self.dirty = true;
    }
    pub(super) fn set_opacity(&mut self, index: usize) {
        let Some(&percent) = OPACITIES.get(index) else { return };
        self.ctx.theme.opacity = percent as f32 / 100.0;
        self.opacity = index;
        crate::save_setting("opacity", &percent.to_string());
        self.apply_blur();
        self.dirty = true;
    }
    /// The window blurs what's behind it only while it is see-through.
    pub(super) fn apply_blur(&self) {
        platform::set_blur(self.blur && self.ctx.theme.opacity < 1.0);
    }
    /// Settings ▸ Shell: new panes start this shell; a missing one is
    /// installed first, in a new tab, and picked once that succeeds.
    pub(super) fn set_shell(&mut self, index: usize) {
        let Some(&kind) = KINDS.get(index) else { return };
        if let Some(path) = shells::find(kind) {
            self.use_shell(path);
            return;
        }
        self.install(shells::install_command(kind), Install::Shell(kind));
        self.refresh_shell_menu();
    }
    /// Runs an install command from Settings in a new tab, and remembers
    /// what it installs so check_install can pick it once it's done.
    pub(super) fn install(&mut self, command: Result<String, String>, what: Install) {
        match command {
            Ok(command) => match self.new_tab() {
                Ok(()) => {
                    if let Some(id) = self.focused_id() {
                        if let Some(pane) = self.pane(id) {
                            pane.run_when_ready(command);
                        }
                        self.installing = Some((id, what));
                    }
                }
                Err(error) => self.ctx.show_toast(&error),
            },
            Err(why) => self.ctx.show_toast(&why),
        }
    }
    /// Settings ▸ Shell ▸ Fetch: what new tabs open with (`index` 0 is
    /// none); a missing one is installed first, in a new tab.
    pub(super) fn set_fetch(&mut self, index: usize) {
        let Some(fetch) = index.checked_sub(1).and_then(|i| FETCHES.get(i)) else {
            crate::save_setting("fetch", "off");
            return;
        };
        if shells::which(fetch.name).is_some() {
            crate::save_setting("fetch", fetch.name);
            self.ctx.show_toast(&format!("New tabs open with {}", fetch.name));
        } else {
            self.install(fetch.install_command(), Install::Fetch(index - 1));
        }
    }
    pub(super) fn use_shell(&mut self, path: String) {
        let Some(kind) = shells::Kind::of_path(&path) else { return };
        crate::save_setting("shell", kind.name());
        self.ctx.shell = path;
        self.refresh_shell_menu();
        self.ctx.show_toast(&format!("New tabs and panes open {}", kind.name()));
        self.dirty = true;
    }
    pub(super) fn refresh_shell_menu(&self) {
        let (names, selected) = shells::menu(&self.ctx.shell);
        platform::set_shell_menu(&names, selected);
    }
    /// After a Settings ▸ Shell install command finishes: use the shell if
    /// it is there now.
    pub(super) fn check_install(&mut self, id: u64) {
        let Some((pane_id, what)) = self.installing else { return };
        if pane_id != id {
            return;
        }
        let Some(code) = self.pane(id).and_then(|pane| pane.take_queued_exit()) else { return };
        self.installing = None;
        let kind = match what {
            Install::Shell(kind) => kind,
            Install::Fetch(index) => {
                let fetch = &FETCHES[index];
                self.fetches_installed = fetch::installed();
                if shells::which(fetch.name).is_some() {
                    crate::save_setting("fetch", fetch.name);
                    self.ctx.show_toast(&format!("New tabs open with {}", fetch.name));
                } else {
                    self.ctx.show_toast(&format!("Installing {} failed (status {code})", fetch.name));
                }
                self.dirty = true;
                return;
            }
        };
        match shells::find(kind) {
            Some(path) => self.use_shell(path),
            None => {
                self.refresh_shell_menu();
                self.ctx.show_toast(&format!("Installing {} failed (status {code})", kind.name()));
            }
        }
    }
    /// Zooms the whole window, text and chrome alike, so the terminal font
    /// is `size` points.
    pub(super) fn change_font_size(&mut self, size: f32) {
        self.set_zoom(size / BASE_FONT_SIZE, true);
    }
    /// Steps the zoom 10% of the default size up or down, onto that grid,
    /// so a step each way comes back to where it started.
    pub(super) fn step_font_size(&mut self, step: f32) {
        let tenths = (self.zoom_percent() / 10.0 + step).round();
        self.change_font_size(DEFAULT_FONT_SIZE * tenths / 10.0);
    }
    /// The zoom as Settings and the toast show it: 100% at the default size.
    pub(super) fn zoom_percent(&self) -> f32 {
        self.zoom * BASE_FONT_SIZE / DEFAULT_FONT_SIZE * 100.0
    }
    /// Lays everything out at `zoom`: a smaller logical window drawn at a
    /// larger scale, so every size in the UI grows together.
    pub(super) fn set_zoom(&mut self, zoom: f32, announce: bool) {
        let zoom = zoom.clamp(8.0 / BASE_FONT_SIZE, 32.0 / BASE_FONT_SIZE);
        let changed = zoom != self.zoom;
        self.zoom = zoom;
        self.width = self.window.0 / zoom;
        self.height = self.window.1 / zoom;
        if self.ctx.text.scale != self.scale * zoom {
            self.ctx.text.set_scale(self.scale * zoom);
            for pane in &mut self.panes {
                pane.cells_changed(&self.ctx.text);
            }
        }
        self.relayout();
        if changed && announce {
            crate::save_setting("zoom", &format!("{zoom}"));
            self.ctx.show_toast(&format!("Zoom {:.0}%", self.zoom_percent()));
        }
    }
    /// Command+,: the Settings tab, opened after the others or switched to.
    pub(super) fn open_settings(&mut self) {
        self.installed = KINDS.map(|kind| shells::find(kind).is_some());
        self.fetches_installed = fetch::installed();
        if let Some(index) = self.tabs.iter().position(|tab| tab.focus == SETTINGS) {
            self.switch_tab(index);
            return;
        }
        self.tabs.push(Tab {
            root: Node::Leaf(SETTINGS),
            focus: SETTINGS,
            zoomed: false,
            name: None,
            color: None,
            shown: None,
            slide: None,
            mark: None,
            marked_at: None,
            ai_title: AiTitle::default(),
        });
        self.switch_tab(self.tabs.len() - 1);
    }
    pub(super) fn settings_values(&self) -> settings::Values {
        settings::Values {
            background: self.background,
            theme: self.themes.get(self.palette).map(|t| t.name.clone()).unwrap_or_default(),
            themes: self.themes.len(),
            palettes: match self.settings.open {
                Some(List::Themes) => self.themes.iter().map(|p| p.in_mode(self.shown_mode())).collect(),
                _ => Vec::new(),
            },
            palette: self.palette,
            editor: self.theme_edit_values(),
            mode: self.shown_mode(),
            keys: self.bindings.list.clone(),
            recording: self.recording,
            ligatures: self.ctx.text.ligatures,
            font: crate::setting("font").filter(|f| !f.is_empty()).unwrap_or_else(|| "JetBrains Mono".to_owned()),
            fonts: match self.settings.open {
                Some(List::Fonts) => self
                    .font_choices
                    .iter()
                    .map(|c| c.clone().unwrap_or_else(|| "JetBrains Mono (built in)".to_owned()))
                    .collect(),
                _ => Vec::new(),
            },
            font_index: App::font_at(&self.font_choices),
            quick_terminal: platform::QUICK_TERMINAL.then(App::quick_terminal_key),
            opacity: self.opacity,
            blur: self.blur,
            shell: shells::Kind::of_path(&self.ctx.shell).and_then(|kind| KINDS.iter().position(|&k| k == kind)),
            installed: self.installed,
            fetch: fetch::chosen().map_or(0, |index| index + 1),
            fetches_installed: self.fetches_installed,
            tabs_top: self.strip == TabStrip::Top,
            extended: self.extended,
            notifications: crate::notify::enabled(),
            zoom: self.zoom_percent(),
            points: self.zoom * BASE_FONT_SIZE,
            line_spacing: crate::text::LINE_SPACINGS
                .iter()
                .position(|&(_, leading)| leading == self.ctx.text.leading())
                .unwrap_or(1),
            cursor: crate::cursor::Shape::ALL.iter().position(|&s| s == crate::cursor::shape()).unwrap_or(0),
            blink: crate::cursor::blinks(),
            updates: crate::setting("updates").as_deref() != Some("off"),
            analytics: crate::analytics::enabled(),
            see_through: cfg!(target_os = "macos"),
            provider: crate::llm::PROVIDERS.iter().position(|&p| p == crate::llm::chosen()).unwrap_or(0),
            fields: Field::ALL.map(|field| crate::setting(&field.key()).unwrap_or_default()),
            editing: self.field.clone(),
            remote: Some(crate::relay::signed_in()),
            remote_account: ["account_login", "account_name"]
                .into_iter()
                .find_map(|key| crate::setting(key).filter(|v| !v.is_empty()))
                .unwrap_or_default(),
            remote_url: crate::relay::app_url(),
            remote_phones: crate::relay::connected().into_iter().map(|(_, name)| name).collect(),
        }
    }
    pub(super) fn settings_action(&mut self, action: Action) {
        self.dirty = true;
        // A settings change, counted as one — never which setting or
        // what it became. Opening the theme list, starting to type in
        // a field and checking for updates aren't changes.
        // Nor are opening the theme editor and picking a color in it.
        let browsing = matches!(action, Action::ThemeEditor(_) | Action::Slot(_) | Action::RecordKey(_));
        if !browsing && !matches!(action, Action::Edit(_) | Action::List(_) | Action::CheckForUpdates) {
            crate::analytics::bump(crate::analytics::Counter::SettingsChanges);
        }
        match action {
            Action::Background(index) => self.set_background(index),
            Action::Theme(step) => self.step_theme(step),
            Action::Font(step) => self.step_font(step),
            Action::List(list) => {
                match list {
                    List::Themes => self.themes = theme::library(&themes_dir()),
                    List::Fonts => self.font_choices = App::font_choices(),
                }
                self.settings.toggle(list);
            }
            Action::PickTheme(index) => {
                self.settings.open = None;
                self.previewing = None;
                self.step_theme(index as i32 - self.palette as i32);
            }
            Action::PickFont(index) => {
                self.settings.open = None;
                self.pick_font(index);
            }
            Action::Customize => {
                self.settings.open = None;
                self.previewing = None;
                self.finish_field();
                self.customize_theme();
            }
            Action::Mode(mode) => self.set_mode(mode),
            Action::RecordKey(action) => {
                self.finish_field();
                // A second click on the one waiting stops waiting.
                self.recording = if self.recording == Some(action) { None } else { Some(action) };
            }
            Action::ResetKey(action) => self.rebind(action, bindings::default_combo(action)),
            Action::ResetKeys => self.reset_keys(),
            Action::RemoveTyped(combo) => self.remove_typed(combo),
            Action::QuickTerminal(on) => {
                let keys = crate::setting("quick_terminal").filter(|k| !k.is_empty() && k != "off");
                let keys = if on { keys.unwrap_or_else(|| QUICK_TERMINAL_KEY.to_owned()) } else { "off".to_owned() };
                crate::save_setting("quick_terminal", &keys);
                self.apply_quick_terminal();
            }
            Action::Ligatures(on) => {
                self.ctx.text.ligatures = on;
                crate::save_setting("ligatures", if on { "on" } else { "off" });
            }
            Action::EditKeys => {
                let path = bindings::path();
                if !std::path::Path::new(&path).exists() {
                    crate::create_private_dir(&crate::settings_dir());
                    let _ = std::fs::write(&path, bindings::template());
                }
                platform::open_url(&path);
            }
            Action::OpenThemes => {
                let _ = std::fs::create_dir_all(themes_dir());
                platform::open_url(&themes_dir());
            }
            Action::Opacity(index) => self.set_opacity(index),
            Action::Blur(on) => {
                self.blur = on;
                crate::save_setting("blur", if on { "on" } else { "off" });
                self.apply_blur();
            }
            Action::Shell(index) => {
                self.set_shell(index);
                self.installed = KINDS.map(|kind| shells::find(kind).is_some());
            }
            Action::Fetch(index) => self.set_fetch(index),
            Action::FetchConfig => {
                let Some(index) = fetch::chosen() else { return };
                match FETCHES[index].config_file() {
                    Ok(path) => platform::open_url(&path),
                    Err(why) => self.ctx.show_toast(&why),
                }
            }
            Action::TabsTop(top) => {
                self.strip = if top { TabStrip::Top } else { TabStrip::Left };
                crate::save_setting("tabs", if top { "top" } else { "left" });
                self.relayout();
            }
            Action::Zoom(0) => self.change_font_size(DEFAULT_FONT_SIZE),
            Action::Zoom(step) => self.step_font_size(step as f32),
            Action::Updates(on) => crate::save_setting("updates", if on { "on" } else { "off" }),
            Action::Analytics(on) => crate::analytics::set_enabled(on),
            Action::CheckForUpdates => {
                self.check_for_updates();
            }
            Action::Extended(on) => self.set_extended(on),
            Action::Notifications(on) => {
                crate::save_setting("notifications", if on { "on" } else { "off" });
                if on {
                    crate::notify::show("Temino", "Notifications are on");
                }
            }
            Action::Remote(true) => crate::relay::sign_in(),
            Action::Remote(false) => crate::relay::sign_out(),
            Action::Disconnect(index) => {
                if let Some((device, name)) = crate::relay::connected().into_iter().nth(index) {
                    crate::relay::disconnect(&device);
                    self.ctx.show_toast(&format!("Disconnected {name}; it has to ask again to come back"));
                }
            }
            Action::Edit(field) => {
                self.finish_field();
                let text = match field {
                    Field::ThemeName | Field::ThemeHex => self.theme_field_text(field),
                    _ => crate::setting(&field.key()).unwrap_or_default(),
                };
                self.field = Some((field, text, true));
            }
            Action::ThemeEditor(open) => {
                self.finish_field();
                self.open_theme_editor(open);
            }
            Action::DuplicateTheme => {
                self.finish_field();
                self.duplicate_theme();
            }
            Action::DeleteTheme => {
                self.finish_field();
                self.delete_theme();
            }
            Action::Slot(slot) => {
                self.finish_field();
                self.pick_slot(slot);
            }
            Action::Nudge(channel, step) => {
                self.finish_field();
                self.nudge_color(channel, step);
            }
            Action::Cursor(index) => {
                let shape = crate::cursor::Shape::ALL[index.min(crate::cursor::Shape::ALL.len() - 1)];
                crate::cursor::set_shape(shape);
                self.ctx.blink_epoch = Instant::now();
            }
            Action::Blink(on) => {
                crate::cursor::set_blinks(on);
                self.ctx.blink_epoch = Instant::now();
            }
            Action::LineSpacing(index) => {
                let Some(&(name, leading)) = crate::text::LINE_SPACINGS.get(index) else { return };
                crate::save_setting("line_spacing", &name.to_lowercase());
                self.ctx.text.set_leading(leading);
                for pane in &mut self.panes {
                    pane.cells_changed(&self.ctx.text);
                }
                self.relayout();
            }
            Action::Provider(index) => {
                self.finish_field();
                crate::save_setting("llm_provider", crate::llm::PROVIDERS[index].id);
                crate::llm::forget_model();
            }
        }
    }
    /// Ends typing in a Settings field, keeping what was typed.
    pub(super) fn finish_field(&mut self) {
        let Some((field, text, _)) = self.field.take() else { return };
        if matches!(field, Field::ThemeName | Field::ThemeHex) {
            self.finish_theme_field(field, &text);
            self.dirty = true;
            return;
        }
        crate::save_setting(&field.key(), text.trim());
        if matches!(field, Field::LlmUrl | Field::LlmKey | Field::LlmModel) {
            crate::llm::forget_model();
        }
        self.dirty = true;
    }
    /// A key while typing in a Settings field; false lets it through.
    pub(super) fn field_key(&mut self, key: &KeyEvent) -> bool {
        let Some((_, text, selected)) = self.field.as_mut() else { return false };
        self.dirty = true;
        if key.cmd() {
            match key.base_char() {
                Some('a') => *selected = true,
                Some('v') => {
                    // Keys come pasted: the first line, without spaces round it.
                    let pasted = platform::clipboard_get().unwrap_or_default();
                    let pasted = pasted.lines().next().unwrap_or("").trim();
                    if *selected {
                        text.clear();
                    }
                    text.push_str(pasted);
                    *selected = false;
                }
                _ => {
                    self.finish_field();
                    return false;
                }
            }
            return true;
        }
        match key.named {
            Some(Named::Enter | Named::Tab) => self.finish_field(),
            Some(Named::Escape) => self.field = None,
            Some(Named::Backspace | Named::Delete) if *selected => text.clear(),
            Some(Named::Backspace) => {
                crate::editor::pop_grapheme(text);
            }
            Some(_) => {}
            None if !key.ctrl() => {
                let typed: String = String::from_utf8_lossy(&key.bytes).chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return true;
                }
                if *selected {
                    text.clear();
                }
                text.push_str(&typed);
            }
            None => {}
        }
        if let Some((_, _, selected)) = self.field.as_mut() {
            *selected = false;
        }
        true
    }
    /// Command+U, the menu and Settings: the card again if an update is
    /// already offered, else a fresh check.
    pub(super) fn check_for_updates(&mut self) {
        if let Some(offer) = &mut self.update {
            offer.open = true;
            return;
        }
        self.ctx.show_toast("Checking for updates…");
        update::check(self.ctx.sender.clone(), true);
    }
    pub(super) fn consent_action(&mut self, action: ConsentAction) {
        match action {
            ConsentAction::Toggle => {
                if let Some(offer) = &mut self.consent {
                    offer.open = !offer.open;
                }
            }
            ConsentAction::Share => {
                crate::analytics::grant();
                self.consent = None;
                self.ctx.show_toast("Thanks — anonymous usage data is shared; change it any time in Settings");
            }
            ConsentAction::NoThanks => {
                crate::analytics::deny();
                self.consent = None;
            }
        }
        self.dirty = true;
    }
    pub(super) fn update_action(&mut self, action: UpdateAction) {
        let Some(offer) = &mut self.update else { return };
        match action {
            UpdateAction::Toggle => offer.open = !offer.open,
            UpdateAction::Notes => platform::open_url(&offer.release.notes_url()),
            // The download can't be called back once started.
            _ if offer.installing => {}
            UpdateAction::Later => offer.open = false,
            UpdateAction::Skip => {
                update::skip(&offer.release.version);
                self.update = None;
            }
            UpdateAction::Install => {
                offer.installing = true;
                update::install(self.ctx.sender.clone(), offer.release.clone());
            }
        }
    }
}

/// How long a pane sits idle before coming back to it shows a recap:
/// `TEMINO_RECAP_MINUTES`, else the `recap_minutes` setting, else 10.
/// Zero turns it off (Command+I still shows one).
/// A scheme from the library in `mode`, on the picked background if there
/// is one.
pub(super) fn build_theme(palette: &Palette, background: Option<usize>, opacity: f32, mode: Mode) -> Theme {
    let theme = palette.in_mode(mode).theme();
    let theme = match background.and_then(|i| backgrounds(mode).get(i)) {
        Some(&(_, rgb)) => theme.with_background(rgb),
        None => theme,
    };
    theme.with_opacity(opacity)
}

/// What a Settings install tab is installing.
#[derive(Clone, Copy)]
pub(super) enum Install {
    Shell(shells::Kind),
    /// One of fetch::FETCHES.
    Fetch(usize),
}

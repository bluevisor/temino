//! Settings ▸ Appearance's theme editor: copying a theme to make it your
//! own, changing its colors and name, and deleting it. Every change is
//! written to the theme's file at once and shows everywhere. Presets are
//! never written: editing one starts from a copy.

use super::*;
use crate::theme::{Channel, Slot};

impl App {
    /// The theme in use, if it's one of your own.
    fn own_theme(&self) -> Option<&Palette> {
        self.themes.get(self.palette).filter(|t| t.file.is_some())
    }

    /// What the editor draws, while it's open on a theme of your own.
    pub(super) fn theme_edit_values(&self) -> Option<settings::ThemeEdit> {
        let (slot, confirm_delete) = self.theme_edit?;
        let palette = self.own_theme()?.clone();
        Some(settings::ThemeEdit { palette, slot, confirm_delete })
    }

    /// Opens the editor on the theme in use if it's yours, or closes it.
    pub(super) fn open_theme_editor(&mut self, open: bool) {
        self.theme_edit = (open && self.own_theme().is_some()).then_some((Slot::Background, false));
    }

    /// Picks the theme called `name` from a freshly read library.
    fn select_theme(&mut self, name: &str) {
        self.themes = theme::library(&themes_dir());
        self.palette = self.themes.iter().position(|t| t.name == name).unwrap_or(0);
        crate::save_setting("theme", &self.themes[self.palette].name);
        self.apply_theme();
    }

    /// A copy of the theme in use, as it looks now (a background picked
    /// over it included), saved as a theme of your own and opened in the
    /// editor.
    pub(super) fn duplicate_theme(&mut self) {
        self.themes = theme::library(&themes_dir());
        let Some(current) = self.themes.get(self.palette) else { return };
        let name = theme::copy_name(&self.themes, &current.name);
        let mut copy = current.copy_as(&name);
        if let Some(&(_, rgb)) = self.background.and_then(|i| BACKGROUNDS.get(i)) {
            copy.background = rgb;
        }
        let dir = themes_dir();
        let _ = std::fs::create_dir_all(&dir);
        copy.file = Some(theme::file_for(&dir, &name));
        if let Err(error) = copy.save() {
            self.ctx.show_toast(&error);
            return;
        }
        // The copy has the background in it now.
        self.background = None;
        crate::save_setting("background", "");
        self.select_theme(&name);
        self.open_theme_editor(true);
        self.ctx.show_toast(&format!("{name} is yours to edit"));
    }

    /// Delete asks once; the second click removes the theme's file and
    /// goes back to the theme before it in the list.
    pub(super) fn delete_theme(&mut self) {
        let Some((slot, armed)) = self.theme_edit else { return };
        if !armed {
            self.theme_edit = Some((slot, true));
            return;
        }
        let Some(own) = self.own_theme() else { return };
        let (name, path) = (own.name.clone(), own.file.clone().unwrap_or_default());
        if let Err(error) = std::fs::remove_file(&path) {
            crate::store::log(&format!("delete theme {}: {error}", path.display()));
            self.ctx.show_toast(&format!("Couldn't delete {name}"));
            return;
        }
        let before = self.palette.saturating_sub(1);
        self.themes = theme::library(&themes_dir());
        let previous = self.themes.get(before.min(self.themes.len() - 1)).map(|t| t.name.clone()).unwrap_or_default();
        self.select_theme(&previous);
        self.theme_edit = None;
        self.ctx.show_toast(&format!("Deleted {name}"));
    }

    pub(super) fn pick_slot(&mut self, slot: Slot) {
        if let Some(edit) = &mut self.theme_edit {
            *edit = (slot, false);
        }
    }

    /// Sets the picked color of the theme being edited, saves it, and shows it.
    fn set_color(&mut self, rgb: u32) {
        let Some((slot, _)) = self.theme_edit else { return };
        let Some(own) = self.themes.get_mut(self.palette).filter(|t| t.file.is_some()) else { return };
        slot.set(own, rgb);
        if let Err(error) = own.save() {
            self.ctx.show_toast(&error);
        }
        self.theme_edit = Some((slot, false));
        self.apply_theme();
    }

    pub(super) fn nudge_color(&mut self, channel: Channel, step: i32) {
        let (Some((slot, _)), Some(own)) = (self.theme_edit, self.own_theme()) else { return };
        if step != 0 {
            self.set_color(theme::nudge(slot.get(own), channel, step));
        }
    }

    /// What a theme editor field starts with when clicked.
    pub(super) fn theme_field_text(&self, field: Field) -> String {
        let (Some((slot, _)), Some(own)) = (self.theme_edit, self.own_theme()) else { return String::new() };
        match field {
            Field::ThemeName => own.name.clone(),
            _ => theme::hex(slot.get(own)),
        }
    }

    /// A theme editor field done with: the typed color, or the new name.
    /// Something that can't be used is said in a toast, and left as it was.
    pub(super) fn finish_theme_field(&mut self, field: Field, text: &str) {
        if field == Field::ThemeHex {
            match theme::parse_hex(text) {
                Some(rgb) => self.set_color(rgb),
                None => self.ctx.show_toast(&format!("{} isn't a color; try #1e90ff", text.trim())),
            }
            return;
        }
        let Some(own) = self.own_theme().cloned() else { return };
        let name = match theme::check_name(&self.themes, text, Some(self.palette)) {
            Ok(name) if name == own.name => return,
            Ok(name) => name,
            Err(why) => return self.ctx.show_toast(&why),
        };
        let mut renamed = own.clone();
        renamed.name = name.clone();
        renamed.file = Some(theme::file_for(&themes_dir(), &name));
        if let Err(error) = renamed.save() {
            return self.ctx.show_toast(&error);
        }
        if let (Some(old), Some(new)) = (&own.file, &renamed.file) {
            // Only the case changed: on a disk that ignores case (macOS's)
            // that's the file just saved, so it's renamed, not removed.
            let same = old.to_string_lossy().eq_ignore_ascii_case(&new.to_string_lossy());
            let done = if same { std::fs::rename(old, new) } else { std::fs::remove_file(old) };
            if let Err(error) = done {
                crate::store::log(&format!("rename theme {}: {error}", old.display()));
            }
        }
        self.select_theme(&name);
    }
}

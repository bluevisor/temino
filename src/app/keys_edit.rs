//! Settings ▸ Keys: waiting for the keys to bind an action to, and the
//! keys file rewritten for each change (it's read again at once).

use super::*;
use crate::bindings::{Combo, GROUPS};
use crate::platform::mods;

impl App {
    /// A key while Settings ▸ Keys waits for one: it becomes the action's
    /// shortcut. False when nothing is waiting.
    pub(super) fn record_key(&mut self, key: &KeyEvent) -> bool {
        let Some(action) = self.recording else { return false };
        self.dirty = true;
        let held = key.mods & (mods::COMMAND | mods::CONTROL | mods::OPTION | mods::SHIFT);
        match key.named {
            Some(Named::Escape) if held == 0 => {
                self.recording = None;
                return true;
            }
            Some(Named::Backspace | Named::Delete) if held == 0 => {
                self.recording = None;
                self.rebind(action, None);
                return true;
            }
            _ => {}
        }
        match bindings::from_event(key) {
            Ok(combo) => {
                self.recording = None;
                self.rebind(action, Some(combo));
            }
            // Still waiting: the toast says what would do.
            Err(why) => self.ctx.show_toast(why),
        }
        true
    }

    /// The keys file as it is, or the commented template a new one starts as.
    fn keys_text() -> String {
        std::fs::read_to_string(bindings::path()).unwrap_or_else(|_| bindings::template())
    }

    fn write_keys(&mut self, text: &str) {
        crate::create_private_dir(&crate::settings_dir());
        if !crate::store::write_atomic(&bindings::path(), text.as_bytes(), true) {
            self.ctx.show_toast("Couldn't save the keys file");
        }
        self.bindings.refresh();
        self.dirty = true;
    }

    /// `action` on `combo` alone (None: on no key), said in a toast when
    /// the key was another action's.
    pub(super) fn rebind(&mut self, action: &'static str, combo: Option<Combo>) {
        self.bindings.refresh();
        let taken = combo.and_then(|c| bindings::owner(&self.bindings.list, &c)).filter(|&other| other != action);
        self.write_keys(&bindings::set(&App::keys_text(), action, combo));
        let name = bindings::label(action);
        match (combo, taken) {
            (Some(combo), Some(other)) => self.ctx.show_toast(&format!(
                "{} is {name} now · {} has no shortcut",
                bindings::display(&combo),
                bindings::label(other)
            )),
            (None, _) => self.ctx.show_toast(&format!("{name} has no shortcut now")),
            _ => {}
        }
    }

    /// Settings ▸ Keys ▸ Reset All: every action on its default keys; keys
    /// that type text stay.
    pub(super) fn reset_keys(&mut self) {
        let mut text = App::keys_text();
        for (_, actions) in GROUPS {
            for action in actions.iter() {
                text = bindings::set(&text, action, bindings::default_combo(action));
            }
        }
        self.write_keys(&text);
        self.ctx.show_toast("Every shortcut is back to its default");
    }

    pub(super) fn remove_typed(&mut self, combo: Combo) {
        self.write_keys(&bindings::remove(&App::keys_text(), &combo));
    }
}

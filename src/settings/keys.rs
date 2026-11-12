//! Settings ▸ Keys: every action and the keys that run it, a section per
//! group. A click on a shortcut waits for the new keys; Reset puts one
//! back. Keys that type text (from the keys file) are listed after.

use super::*;
use crate::bindings::{self, GROUPS};

/// Only macOS has the built-in browser, so only there are its keys shown.
pub(super) fn shown(action: &str) -> bool {
    crate::platform::VIEWERS || !matches!(action, "new_browser_tab" | "browser_pane")
}

/// A typed binding's text as it reads: the words in quotes, and Enter,
/// Tab and Escape by name: “git status” Enter.
fn typed_text(bytes: &[u8]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut words = String::new();
    for c in String::from_utf8_lossy(bytes).chars() {
        let key = match c {
            '\r' | '\n' => "Enter",
            '\t' => "Tab",
            '\x1b' => "Esc",
            c if c.is_control() => "Ctrl key",
            c => {
                words.push(c);
                continue;
            }
        };
        if !words.is_empty() {
            parts.push(format!("“{}”", std::mem::take(&mut words)));
        }
        parts.push(key.to_owned());
    }
    if !words.is_empty() {
        parts.push(format!("“{words}”"));
    }
    parts.join(" ")
}

impl Ui<'_> {
    /// The Keys sections; returns where the next one goes.
    pub(super) fn keys(&mut self, x: f32, mut y: f32, width: f32, values: &Values) -> f32 {
        let mut rows = vec![Row::KeysHelp];
        if values.quick_terminal.is_some() {
            rows.push(Row::QuickTerminal);
        }
        y = self.section(x, y, width, "Keys", &rows, values);
        for (title, actions) in GROUPS {
            let rows: Vec<Row> = actions.iter().filter(|a| shown(a)).map(|&a| Row::Key(a)).collect();
            y = self.section(x, y, width, &format!("Keys · {title}"), &rows, values);
        }
        let typed = bindings::typed(&values.keys);
        if !typed.is_empty() {
            let rows: Vec<Row> = (0..typed.len()).map(Row::Typed).collect();
            y = self.section(x, y, width, "Keys · Typed Text", &rows, values);
        }
        y
    }

    pub(super) fn key_label(&self, row: Row, values: &Values) -> (&'static str, String) {
        match row {
            Row::KeysHelp => {
                let detail = match values.recording {
                    Some(action) => {
                        format!(
                            "Press the new keys for {} · Esc cancels, Delete leaves it on none",
                            bindings::label(action)
                        )
                    }
                    None => match changed(values) {
                        0 => "Click a shortcut, then press the new keys".to_owned(),
                        n => format!("{n} changed · click a shortcut, then press the new keys"),
                    },
                };
                ("Shortcuts", detail)
            }
            Row::Key(action) => (bindings::label(action), String::new()),
            Row::Typed(index) => {
                let typed = bindings::typed(&values.keys);
                let text = typed.get(index).map(|(_, text)| typed_text(text)).unwrap_or_default();
                ("Types text", format!("{text} · from the keys file"))
            }
            _ => ("", String::new()),
        }
    }

    pub(super) fn key_control(&mut self, right: f32, y: f32, row: Row, values: &Values) {
        match row {
            Row::KeysHelp => {
                let left = self.placed_button(right, y, "Open Keys File", Action::EditKeys);
                if changed(values) > 0 {
                    self.button(left - 8.0, y, "Reset All", Action::ResetKeys);
                }
            }
            Row::Key(action) => {
                let mut x = right;
                if values.recording == Some(action) {
                    let chip = self.key_chip(x, y, "Press keys…", true);
                    self.hit(chip, Action::RecordKey(action));
                    return;
                }
                let keys = bindings::keys_for(&values.keys, action);
                if keys.is_empty() {
                    let chip = self.key_chip(x, y, "None", false);
                    self.hit(chip, Action::RecordKey(action));
                    x = chip.x - 6.0;
                }
                for combo in keys.iter().rev() {
                    let chip = self.key_chip(x, y, &bindings::display(combo), false);
                    self.hit(chip, Action::RecordKey(action));
                    x = chip.x - 6.0;
                }
                if bindings::changed(&values.keys, action) {
                    self.button(x - 4.0, y, "Reset", Action::ResetKey(action));
                }
            }
            Row::Typed(index) => {
                let typed = bindings::typed(&values.keys);
                let Some((combo, _)) = typed.get(index) else { return };
                let left = self.placed_button(right, y, "Remove", Action::RemoveTyped(*combo));
                // The key itself, not a button: it's changed in the file.
                self.key_chip(left - 8.0, y, &bindings::display(combo), false);
            }
            _ => {}
        }
    }

    /// A shortcut as a key cap, right-aligned at `right`: lit while it
    /// waits for keys. Returns where it is.
    fn key_chip(&mut self, right: f32, y: f32, label: &str, lit: bool) -> Rect {
        let theme = self.theme;
        let size = 12.0;
        let w = (self.text.measure(label, UI, size) + 20.0).max(44.0);
        let rect = Rect::new(right - w, y, w, CONTROL_HEIGHT);
        let border = if lit {
            theme.accent
        } else if self.hovered(rect) {
            theme.text_faint
        } else {
            theme.divider
        };
        self.scene.quad(Quad::new(rect, self.well()).radius(6.0).border(if lit { 1.5 } else { 1.0 }, border));
        let baseline = (rect.y + (rect.h + self.text.ui_ascent(size) * 0.72) / 2.0).round();
        let color = if lit { theme.accent } else { theme.text };
        let tx = rect.x + (w - self.text.measure(label, UI, size)) / 2.0;
        self.text.draw_text(self.scene, label, tx, baseline, UI, size, color);
        rect
    }
}

/// How many shown actions aren't on their default keys.
fn changed(values: &Values) -> usize {
    GROUPS
        .iter()
        .flat_map(|(_, actions)| actions.iter())
        .filter(|a| shown(a) && bindings::changed(&values.keys, a))
        .count()
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_text_names_its_keys() {
        assert_eq!(super::typed_text(b"git status\r"), "“git status” Enter");
        assert_eq!(super::typed_text(b"\x1b[A"), "Esc “[A”");
        assert_eq!(super::typed_text(b""), "");
    }
}

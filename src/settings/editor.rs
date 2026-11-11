//! The theme editor, opened from Customize… in the theme list: a theme of
//! your own's name, every color it sets as a dot to pick, the picked one's
//! hex, nudges for its hue, saturation and lightness, and copying or
//! deleting it. Presets are never edited: Customize… starts from a copy.

use super::*;
use crate::theme::{hex, ANSI_NAMES};

/// The theme editor's state, for drawing it.
#[derive(Clone)]
pub struct ThemeEdit {
    /// The theme being edited, as it is now.
    pub palette: Palette,
    /// The color the hex field and nudges change.
    pub slot: Slot,
    /// Delete was clicked once: the next click deletes.
    pub confirm_delete: bool,
}

/// What a theme editor field shows while it isn't being typed in.
pub(super) fn field_text(field: Field, values: &Values) -> String {
    let Some(edit) = &values.editor else { return String::new() };
    match field {
        Field::ThemeName => edit.palette.name.clone(),
        _ => hex(edit.slot.get(&edit.palette)),
    }
}

/// The nudges, in the order they're drawn left to right.
const NUDGES: [(Channel, &str); 3] =
    [(Channel::Hue, "Hue"), (Channel::Saturation, "Saturation"), (Channel::Lightness, "Lightness")];
const DOT: f32 = 26.0;
/// The color dots start this far into the row, after its label.
const DOTS_LEFT: f32 = 150.0;

impl Ui<'_> {
    pub(super) fn editor_label(&self, row: Row, values: &Values) -> (&'static str, String) {
        let edit = values.editor.as_ref();
        match row {
            Row::ThemeName => ("Name", "In the theme list, and its file's name".to_owned()),
            Row::SlotHex => {
                let Some(edit) = edit else { return ("", String::new()) };
                let palette = &edit.palette;
                let detail = match edit.slot {
                    Slot::Cursor if palette.cursor.is_none() => "Follows the accent until you set it",
                    Slot::Selection if palette.selection.is_none() => "Follows the accent until you set it",
                    Slot::Accent if palette.accent.is_none() => "Follows bright blue until you set it",
                    _ => "Type a hex color: #1e90ff, or #19f",
                };
                let detail = detail.to_owned();
                // The label borrows nothing: the names are fixed words.
                (slot_label(edit.slot), detail)
            }
            Row::Nudge => ("Adjust", "A step at a time".to_owned()),
            Row::EditorActions if edit.is_some_and(|e| e.confirm_delete) => {
                ("Delete this theme?", "Its file goes too; this can't be undone".to_owned())
            }
            Row::EditorActions => ("Saved as you go", "Customize… in the theme list opens this again".to_owned()),
            _ => ("", String::new()),
        }
    }

    pub(super) fn editor_control(&mut self, right: f32, y: f32, row: Row, values: &Values) {
        let edit = values.editor.as_ref();
        match row {
            Row::ThemeName => self.field(right, y, FIELD_WIDTH, Field::ThemeName, values),
            Row::SlotHex => {
                let left = right - 110.0;
                self.field(right, y, 110.0, Field::ThemeHex, values);
                if let Some(edit) = edit {
                    // The color itself beside its hex, as big as the field is tall.
                    let dot = Rect::new(left - 10.0 - CONTROL_HEIGHT, y, CONTROL_HEIGHT, CONTROL_HEIGHT);
                    let rgb = edit.slot.get(&edit.palette);
                    self.scene.quad(Quad::new(dot, Color::hex(rgb)).radius(7.0).border(1.0, self.ring()));
                }
            }
            Row::Nudge => {
                let mut x = right;
                for &(channel, name) in NUDGES.iter().rev() {
                    let labels = ["−".to_owned(), name.to_owned(), "+".to_owned()];
                    x = self.segments(x, y, &labels, None, |i| Action::Nudge(channel, i as i32 - 1)) - 8.0;
                }
            }
            Row::EditorActions => {
                let armed = edit.is_some_and(|e| e.confirm_delete);
                let left = self.placed_button(right, y, "Done", Action::ThemeEditor(false));
                let left = self.placed_button(left - 8.0, y, "Duplicate", Action::DuplicateTheme);
                let label = if armed { "Click Again to Delete" } else { "Delete…" };
                self.button(left - 8.0, y, label, Action::DeleteTheme);
            }
            _ => {}
        }
    }

    /// The faint ring round a color dot, so a dot the color of the page
    /// still shows.
    fn ring(&self) -> Color {
        if self.theme.light() {
            Color::hex(0x000000).alpha(0.22)
        } else {
            Color::hex(0xffffff).alpha(0.22)
        }
    }

    /// One row of color dots, each named under it, the picked one ringed.
    pub(super) fn colors(&mut self, rect: Rect, group: Group, values: &Values) {
        let Some(edit) = &values.editor else { return };
        let theme = self.theme;
        let (label, detail, slots): (&str, &str, Vec<Slot>) = match group {
            Group::Base => ("Window and text", "And the UI's accent", Slot::BASE.to_vec()),
            Group::Normal => ("Colors", "ANSI 0–7", (0..8).map(Slot::Ansi).collect()),
            Group::Bright => ("Bright colors", "ANSI 8–15", (8..16).map(Slot::Ansi).collect()),
        };
        let left = rect.x + 16.0;
        self.text.draw_text(self.scene, label, left, rect.y + 31.0, UI, 13.0, theme.text);
        self.text.draw_text(self.scene, detail, left, rect.y + 48.0, UI, 11.5, theme.text_dim);
        let area = Rect::new(rect.x + DOTS_LEFT, rect.y, rect.w - DOTS_LEFT - 8.0, rect.h);
        let cell = area.w / slots.len() as f32;
        for (i, &slot) in slots.iter().enumerate() {
            let cx = area.x + cell * (i as f32 + 0.5);
            let dot = Rect::new((cx - DOT / 2.0).round(), rect.y + 12.0, DOT, DOT);
            let hit = Rect::new(area.x + cell * i as f32, rect.y + 4.0, cell, rect.h - 8.0);
            let picked = slot == edit.slot;
            if picked {
                let halo = Quad::new(dot.inset(-3.0, -3.0), Color::TRANSPARENT).radius(DOT).border(2.0, theme.accent);
                self.scene.quad(halo);
            } else if self.hovered(hit) {
                let halo = Quad::new(dot.inset(-3.0, -3.0), Color::TRANSPARENT).radius(DOT);
                self.scene.quad(halo.border(1.0, theme.text_dim));
            }
            let rgb = slot.get(&edit.palette);
            self.scene.quad(Quad::new(dot, Color::hex(rgb)).radius(DOT / 2.0).border(1.0, self.ring()));
            let name = slot.short();
            let size = 10.5;
            let w = self.text.measure(name, UI, size);
            let color = if picked { theme.text } else { theme.text_dim };
            self.text.draw_text(self.scene, name, (cx - w / 2.0).round(), dot.bottom() + 20.0, UI, size, color);
            self.hit(hit, Action::Slot(slot));
        }
    }
}

/// A color's whole name, as a fixed word: "Bright Red", "Selection".
fn slot_label(slot: Slot) -> &'static str {
    const BRIGHT: [&str; 8] = [
        "Bright Black",
        "Bright Red",
        "Bright Green",
        "Bright Yellow",
        "Bright Blue",
        "Bright Magenta",
        "Bright Cyan",
        "Bright White",
    ];
    match slot {
        Slot::Ansi(i @ 8..) => BRIGHT[(i as usize - 8) % 8],
        Slot::Ansi(i) => ANSI_NAMES[i as usize % 8],
        other => other.short(),
    }
}

//! Themes of your own: the colors the theme editor changes, the Ghostty
//! file each one is kept in, and the rules for naming them. The built-in
//! presets are never written; editing starts from a copy.

use super::*;
use std::path::{Path, PathBuf};

/// One color the theme editor sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    Background,
    Foreground,
    Cursor,
    Selection,
    Accent,
    /// ANSI 0-15.
    Ansi(u8),
}

/// The ANSI colors' names, 0-7; 8-15 are their bright versions.
pub const ANSI_NAMES: [&str; 8] = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];

impl Slot {
    pub const BASE: [Slot; 5] = [Slot::Background, Slot::Foreground, Slot::Cursor, Slot::Selection, Slot::Accent];

    /// Its name under its swatch.
    pub fn short(self) -> &'static str {
        match self {
            Slot::Background => "Background",
            Slot::Foreground => "Text",
            Slot::Cursor => "Cursor",
            Slot::Selection => "Selection",
            Slot::Accent => "Accent",
            Slot::Ansi(i) => ANSI_NAMES[i as usize % 8],
        }
    }

    /// What it shows as now: an unset cursor, selection or accent is the
    /// color it falls back to.
    pub fn get(self, palette: &Palette) -> u32 {
        let accent = palette.accent.unwrap_or(palette.ansi[12]);
        match self {
            Slot::Background => palette.background,
            Slot::Foreground => palette.foreground,
            Slot::Cursor => palette.cursor.unwrap_or(accent),
            Slot::Selection => palette.selection.unwrap_or(accent),
            Slot::Accent => accent,
            Slot::Ansi(i) => palette.ansi[i as usize & 15],
        }
    }

    pub fn set(self, palette: &mut Palette, rgb: u32) {
        let rgb = rgb & 0xffffff;
        match self {
            Slot::Background => palette.background = rgb,
            Slot::Foreground => palette.foreground = rgb,
            Slot::Cursor => palette.cursor = Some(rgb),
            Slot::Selection => palette.selection = Some(rgb),
            Slot::Accent => palette.accent = Some(rgb),
            Slot::Ansi(i) => palette.ansi[i as usize & 15] = rgb,
        }
    }
}

/// Which way a color is nudged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Hue,
    Saturation,
    Lightness,
}

/// `rgb` moved `step` notches along `channel`: 10° of hue (round the
/// wheel), or 5% of saturation or lightness (stopping at the ends).
pub fn nudge(rgb: u32, channel: Channel, step: i32) -> u32 {
    let (mut h, mut s, mut l) = to_hsl(rgb);
    match channel {
        Channel::Hue => h = (h + 10.0 * step as f32).rem_euclid(360.0),
        Channel::Saturation => s = (s + 0.05 * step as f32).clamp(0.0, 1.0),
        Channel::Lightness => l = (l + 0.05 * step as f32).clamp(0.0, 1.0),
    }
    from_hsl(h, s, l)
}

fn to_hsl(rgb: u32) -> (f32, f32, f32) {
    let [r, g, b] = [(rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff].map(|v| v as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

fn from_hsl(h: f32, s: f32, l: f32) -> u32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(r) << 16 | byte(g) << 8 | byte(b)
}

/// A typed color: `#1e90ff`, `1e90ff` or the short `#19f`.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.trim().trim_start_matches('#');
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        6 => u32::from_str_radix(digits, 16).ok(),
        3 => {
            let short = u32::from_str_radix(digits, 16).ok()?;
            let (r, g, b) = ((short >> 8) & 0xf, (short >> 4) & 0xf, short & 0xf);
            Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
        }
        _ => None,
    }
}

pub fn hex(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0xffffff)
}

fn to_u32(color: Color) -> u32 {
    let [r, g, b] = color.to_rgb8();
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

impl Palette {
    /// A theme of your own to start from this one: every color spelled
    /// out (Temino's default is worked out in code, not listed), under
    /// `name`, not yet in a file.
    pub fn copy_as(&self, name: &str) -> Palette {
        let mut copy = self.clone();
        if self.name == BUILT_IN[0].0 && self.ansi == [0; 16] {
            let dark = Theme::dark();
            copy.background = to_u32(dark.background);
            copy.foreground = to_u32(dark.text);
            copy.cursor = Some(to_u32(dark.cursor));
            copy.accent = Some(to_u32(dark.accent));
            copy.ansi = dark.ansi.map(to_u32);
        }
        copy.name = name.to_owned();
        copy.file = None;
        copy
    }

    /// The Ghostty theme file that reads back as this palette.
    pub fn to_file(&self) -> String {
        let mut text = format!("# {} · made in Temino's theme editor\n", self.name);
        text.push_str(&format!("background = {}\nforeground = {}\n", hex(self.background), hex(self.foreground)));
        if let Some(cursor) = self.cursor {
            text.push_str(&format!("cursor-color = {}\n", hex(cursor)));
        }
        if let Some(selection) = self.selection {
            text.push_str(&format!("selection-background = {}\n", hex(selection)));
        }
        if let Some(accent) = self.accent {
            text.push_str(&format!("accent = {}\n", hex(accent)));
        }
        for (i, &rgb) in self.ansi.iter().enumerate() {
            text.push_str(&format!("palette = {i}={}\n", hex(rgb)));
        }
        text
    }

    /// Writes a theme of your own to its file. A preset has none, and is
    /// never written.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.file else { return Err("Presets can't be changed; duplicate one to edit".into()) };
        let path = path.to_str().ok_or("The theme file's path isn't text")?;
        if crate::store::write_atomic(path, self.to_file().as_bytes(), false) {
            Ok(())
        } else {
            Err(format!("Couldn't save {}", self.name))
        }
    }
}

/// "<base> Copy", or "<base> Copy 2" and on, whichever no theme has yet.
pub fn copy_name(library: &[Palette], base: &str) -> String {
    let base = base.strip_suffix(" (Custom)").unwrap_or(base);
    let taken = |name: &str| library.iter().any(|t| t.name.eq_ignore_ascii_case(name));
    let first = format!("{base} Copy");
    if !taken(&first) {
        return first;
    }
    (2..).map(|n| format!("{base} Copy {n}")).find(|name| !taken(name)).unwrap()
}

/// A name typed for a theme, trimmed, if it can be one: not empty, not too
/// long, usable as a file name, and not another theme's (`except` is the
/// one being renamed).
pub fn check_name(library: &[Palette], name: &str, except: Option<usize>) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A theme needs a name".into());
    }
    if name.chars().count() > 48 {
        return Err("That name is too long (48 characters at most)".into());
    }
    if name.starts_with('.') || name.chars().any(|c| matches!(c, '/' | '\\' | ':') || c.is_control()) {
        return Err("A theme's name can't start with a dot or hold / \\ or :".into());
    }
    let clash = library.iter().enumerate().any(|(i, t)| Some(i) != except && t.name.eq_ignore_ascii_case(name));
    if clash {
        return Err(format!("There's a theme called {name} already"));
    }
    Ok(name.to_owned())
}

/// Where a new theme called `name` is kept in `dir`.
pub fn file_for(dir: &str, name: &str) -> PathBuf {
    Path::new(dir).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("temino-custom-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_theme_of_your_own_reads_back_as_written() {
        let dir = dir("roundtrip");
        let library = library("/nonexistent");
        let mut mine = library[3].copy_as("Mine");
        mine.file = Some(dir.join("Mine"));
        Slot::Accent.set(&mut mine, 0x123456);
        Slot::Ansi(9).set(&mut mine, 0xabcdef);
        mine.save().unwrap();
        let again = super::library(dir.to_str().unwrap()).pop().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(again, mine);
    }

    #[test]
    fn presets_are_never_written() {
        let library = library("/nonexistent");
        assert!(library.iter().all(|t| t.file.is_none()));
        assert!(library[1].save().is_err());
    }

    #[test]
    fn a_copy_of_the_default_spells_out_its_colors() {
        let copy = library("/nonexistent")[0].copy_as("Temino Copy");
        assert_ne!(copy.ansi, [0; 16]);
        let theme = copy.theme();
        let dark = Theme::dark();
        assert_eq!((theme.background, theme.text, theme.accent), (dark.background, dark.text, dark.accent));
        assert_eq!(theme.ansi, dark.ansi);
    }

    #[test]
    fn unset_colors_show_what_they_fall_back_to() {
        let mut palette = library("/nonexistent")[7].clone();
        palette.accent = None;
        palette.cursor = None;
        palette.selection = None;
        let ansi12 = palette.ansi[12];
        assert_eq!([Slot::Accent, Slot::Cursor, Slot::Selection].map(|s| s.get(&palette)), [ansi12; 3]);
        Slot::Cursor.set(&mut palette, 0xff0000);
        assert_eq!((palette.cursor, Slot::Accent.get(&palette)), (Some(0xff0000), ansi12));
    }

    #[test]
    fn copies_get_a_free_name() {
        let mut library = library("/nonexistent");
        assert_eq!(copy_name(&library, "Amiga 500"), "Amiga 500 Copy");
        library.push(library[0].copy_as("Amiga 500 Copy"));
        assert_eq!(copy_name(&library, "Amiga 500"), "Amiga 500 Copy 2");
        assert_eq!(copy_name(&library, "Apple II (Custom)"), "Apple II Copy");
    }

    #[test]
    fn names_must_be_usable_and_free() {
        let library = library("/nonexistent");
        assert_eq!(check_name(&library, "  Dusk  ", None), Ok("Dusk".to_owned()));
        for bad in ["", "   ", ".hidden", "a/b", "a:b", "a\\b", &"x".repeat(49)] {
            assert!(check_name(&library, bad, None).is_err(), "{bad:?}");
        }
        // A preset's name is taken, whatever the case; a theme keeps its own.
        assert!(check_name(&library, "commodore 64", None).is_err());
        assert!(check_name(&library, "Commodore 64", Some(4)).is_ok());
    }

    #[test]
    fn typed_colors_are_read() {
        assert_eq!(parse_hex("#1e90ff"), Some(0x1e90ff));
        assert_eq!(parse_hex(" 1E90FF "), Some(0x1e90ff));
        assert_eq!(parse_hex("#19f"), Some(0x1199ff));
        for bad in ["", "#", "12345", "#1234567", "#gggggg", "+12345", "red"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn nudges_move_one_channel_and_stop_at_the_ends() {
        // Pure red: hue goes round, lightness stops at white.
        assert_eq!(nudge(0xff0000, Channel::Hue, 12), 0x00ff00);
        assert_eq!(nudge(0xff0000, Channel::Hue, 18), 0x00ffff);
        assert_eq!(nudge(0xff0000, Channel::Hue, -12), 0x0000ff);
        assert_eq!(nudge(0xff0000, Channel::Hue, 36), 0xff0000);
        assert_eq!(nudge(0xff0000, Channel::Lightness, 100), 0xffffff);
        assert_eq!(nudge(0xff0000, Channel::Saturation, -100), 0x808080);
        // A gray has no hue to turn.
        assert_eq!(nudge(0x808080, Channel::Hue, 3), 0x808080);
        // Every byte value survives the trip through HSL.
        for rgb in [0x000000, 0xffffff, 0x1e90ff, 0x0f1117, 0x7bd88f, 0xc47fd5] {
            assert_eq!(nudge(rgb, Channel::Hue, 0), rgb, "{rgb:06x}");
        }
    }
}

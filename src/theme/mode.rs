//! Light and Dark: every theme comes in both. A theme's own colors are one
//! of them; the other is worked out from it (Temino's own has a light
//! version drawn by hand). The background presets follow along.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    /// In the order Settings shows them.
    pub const ALL: [Mode; 2] = [Mode::Light, Mode::Dark];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Light => "Light",
            Mode::Dark => "Dark",
        }
    }

    /// `appearance=light|dark` in the settings file.
    pub fn parse(text: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|mode| mode.name().eq_ignore_ascii_case(text.trim()))
    }
}

/// Settings ▸ Background in Light mode: a light color in place of each
/// dark one, in the same places.
pub const LIGHT_BACKGROUNDS: [(&str, u32); 6] = [
    ("Mist", 0xf3f5f8),
    ("White", 0xffffff),
    ("Silver", 0xeaeaed),
    ("Sky", 0xe8f0fb),
    ("Lilac", 0xf3ecf7),
    ("Mint", 0xe9f4ee),
];

/// The background presets for `mode`.
pub fn backgrounds(mode: Mode) -> &'static [(&'static str, u32); 6] {
    match mode {
        Mode::Light => &LIGHT_BACKGROUNDS,
        Mode::Dark => &BACKGROUNDS,
    }
}

/// A background preset by its name in either mode: `background=Navy`
/// is Sky in Light mode.
pub fn background_index(name: &str) -> Option<usize> {
    let find = |list: &[(&str, u32); 6]| list.iter().position(|(n, _)| n.eq_ignore_ascii_case(name));
    find(&BACKGROUNDS).or_else(|| find(&LIGHT_BACKGROUNDS))
}

/// Temino's own, in Light mode: dark text on a cool white.
const TEMINO_LIGHT: (u32, u32, u32, [u32; 16]) = (
    0xf6f7f9,
    0x1f2328,
    0x0969da,
    [
        0x24292f, 0xcf222e, 0x116329, 0x7d4e00, 0x0969da, 0x8250df, 0x1b7c83, 0x5c6570, //
        0x57606a, 0xa40e26, 0x1a7f37, 0x6e4a00, 0x218bff, 0x8a5cf0, 0x2a8a95, 0x6e7781,
    ],
);

/// How much lighter than black a color looks, 0 to 1, as WCAG counts it.
fn luminance(rgb: u32) -> f32 {
    let linear = |v: u32| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear((rgb >> 16) & 0xff) + 0.7152 * linear((rgb >> 8) & 0xff) + 0.0722 * linear(rgb & 0xff)
}

/// The WCAG contrast ratio of two colors, 1 to 21.
pub fn contrast(a: u32, b: u32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `rgb` made lighter or darker (away from `background`), its hue kept,
/// until it reads against `background` at `ratio`, or it can't go further.
fn readable(rgb: u32, background: u32, ratio: f32) -> u32 {
    let darker = luminance(background) > 0.18;
    let (h, s, mut l) = to_hsl(rgb);
    let mut out = rgb;
    while contrast(out, background) < ratio {
        l += if darker { -0.01 } else { 0.01 };
        if !(0.0..=1.0).contains(&l) {
            break;
        }
        out = from_hsl(h, s, l);
    }
    out
}

/// `rgb` with its lightness turned over (dark becomes light), then kept
/// within `range`.
fn turned(rgb: u32, range: (f32, f32)) -> u32 {
    let (h, s, l) = to_hsl(rgb);
    from_hsl(h, s, (1.0 - l).clamp(range.0, range.1))
}

impl Palette {
    /// Light or Dark, by its own background, as `Theme::light` reads it.
    pub fn mode(&self) -> Mode {
        let [r, g, b] = [(self.background >> 16) & 0xff, (self.background >> 8) & 0xff, self.background & 0xff];
        let lightness = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
        if lightness > 0.5 {
            Mode::Light
        } else {
            Mode::Dark
        }
    }

    /// This theme in `mode`: itself when that's its own, else its other
    /// version — background and text turned over, every other color made
    /// as light or dark as it needs to be to read on the new background.
    pub fn in_mode(&self, mode: Mode) -> Palette {
        if self.mode() == mode {
            return self.clone();
        }
        if self.file.is_none() && self.name == BUILT_IN[0].0 && self.ansi == [0; 16] {
            let (background, foreground, accent, ansi) = TEMINO_LIGHT;
            return Palette {
                background,
                foreground,
                cursor: Some(accent),
                selection: None,
                accent: Some(accent),
                ansi,
                ..self.clone()
            };
        }
        let light = mode == Mode::Light;
        let background = turned(self.background, if light { (0.9, 0.97) } else { (0.04, 0.13) });
        let foreground = readable(turned(self.foreground, (0.0, 1.0)), background, 7.0);
        let mut ansi = self.ansi;
        for (i, color) in ansi.iter_mut().enumerate().skip(1) {
            // The normal colors are text; the bright ones may be fainter.
            *color = readable(*color, background, if i < 8 { 4.5 } else { 3.0 });
        }
        let accent = self.accent.unwrap_or(self.ansi[12]);
        Palette {
            background,
            foreground,
            cursor: self.cursor.map(|rgb| readable(rgb, background, 3.0)),
            selection: self.selection.map(|rgb| turned(rgb, if light { (0.72, 0.88) } else { (0.14, 0.34) })),
            accent: Some(readable(accent, background, 3.0)),
            ansi,
            ..self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_theme_comes_in_both_modes() {
        for palette in library("/nonexistent") {
            for mode in Mode::ALL {
                let shown = palette.in_mode(mode);
                assert_eq!(shown.mode(), mode, "{} {mode:?}", palette.name);
                assert_eq!(shown.theme().light(), mode == Mode::Light, "{} {mode:?}", palette.name);
                assert_eq!(shown.name, palette.name);
                // Text reads on the turned-over background.
                if shown.mode() != palette.mode() {
                    let ratio = contrast(shown.foreground, shown.background);
                    assert!(ratio >= 7.0, "{} {mode:?}: {ratio:.1}", palette.name);
                    for (i, &color) in shown.ansi.iter().enumerate().skip(1) {
                        let want = if i < 8 { 4.5 } else { 3.0 };
                        let ratio = contrast(color, shown.background);
                        assert!(ratio >= want - 0.05, "{} {mode:?} ansi {i}: {ratio:.1}", palette.name);
                    }
                }
            }
        }
    }

    #[test]
    fn a_theme_in_its_own_mode_is_unchanged() {
        let library = library("/nonexistent");
        let mac = library.iter().find(|p| p.name == "Macintosh").unwrap();
        assert_eq!(mac.mode(), Mode::Light);
        assert_eq!(&mac.in_mode(Mode::Light), mac);
        assert_eq!(library[0].in_mode(Mode::Dark), library[0]);
        assert_eq!(library[0].in_mode(Mode::Dark).theme().accent, Theme::dark().accent);
    }

    #[test]
    fn temino_light_text_meets_wcag_contrast() {
        let theme = library("/nonexistent")[0].in_mode(Mode::Light).theme();
        assert!(theme.light());
        for ink in [theme.text, theme.text_dim, theme.text_faint] {
            assert!(ink.contrast(theme.background) >= 4.5, "{:.2}", ink.contrast(theme.background));
        }
    }

    #[test]
    fn backgrounds_follow_the_mode() {
        assert_eq!(background_index("Navy"), Some(3));
        assert_eq!(background_index("sky"), Some(3));
        assert_eq!(background_index("Nope"), None);
        for (name, rgb) in LIGHT_BACKGROUNDS {
            assert!(luminance(rgb) > 0.5, "{name}");
        }
        assert_eq!(Mode::parse(" dark"), Some(Mode::Dark));
        assert_eq!(Mode::parse("auto"), None);
    }
}

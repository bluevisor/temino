//! Colors: the UI palette and the terminal's 256-color table.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};

mod custom;
pub use custom::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// `#rrggbb`, for remote clients.
    pub fn css(self) -> String {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(self.r), byte(self.g), byte(self.b))
    }

    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    pub const fn hex(rgb: u32) -> Color {
        Color {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub fn rgb(rgb: Rgb) -> Color {
        Color { r: rgb.r as f32 / 255.0, g: rgb.g as f32 / 255.0, b: rgb.b as f32 / 255.0, a: 1.0 }
    }

    pub fn alpha(self, a: f32) -> Color {
        Color { a: self.a * a, ..self }
    }

    /// The WCAG contrast ratio against another color, 1.0 (none) to
    /// 21.0 (black on white); alpha is ignored, both colors read as
    /// opaque. Small text wants at least 4.5. Only the theme tests call
    /// this — it exists to keep the built-in theme's text legible.
    #[cfg(test)]
    pub fn contrast(self, other: Color) -> f32 {
        let luminance = |c: Color| {
            let linear = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
        };
        let (a, b) = (luminance(self), luminance(other));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    pub fn mix(self, other: Color, t: f32) -> Color {
        Color {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
            a: self.a + (other.a - self.a) * t,
        }
    }

    /// Blends through hue rather than RGB, so green to orange passes
    /// through yellow instead of olive.
    pub fn mix_hue(self, other: Color, t: f32) -> Color {
        let (h0, s0, v0) = self.hsv();
        let (h1, s1, v1) = other.hsv();
        let mut dh = h1 - h0;
        if dh > 180.0 {
            dh -= 360.0;
        } else if dh < -180.0 {
            dh += 360.0;
        }
        Color::from_hsv((h0 + dh * t).rem_euclid(360.0), s0 + (s1 - s0) * t, v0 + (v1 - v0) * t)
    }

    fn hsv(self) -> (f32, f32, f32) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if max == self.r {
            60.0 * ((self.g - self.b) / delta).rem_euclid(6.0)
        } else if max == self.g {
            60.0 * ((self.b - self.r) / delta + 2.0)
        } else {
            60.0 * ((self.r - self.g) / delta + 4.0)
        };
        (hue, if max == 0.0 { 0.0 } else { delta / max }, max)
    }

    fn from_hsv(hue: f32, saturation: f32, value: f32) -> Color {
        let c = value * saturation;
        let x = c * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let (r, g, b) = match (hue / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = value - c;
        Color { r: r + m, g: g + m, b: b + m, a: 1.0 }
    }

    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn to_rgb8(self) -> [u8; 3] {
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [channel(self.r), channel(self.g), channel(self.b)]
    }

    fn to_term(self) -> Rgb {
        let [r, g, b] = self.to_rgb8();
        Rgb { r, g, b }
    }
}

#[derive(Clone)]
pub struct Theme {
    pub background: Color,
    pub surface: Color,
    pub raised: Color,
    pub divider: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub string: Color,
    pub variable: Color,
    pub selection: Color,
    pub cursor: Color,
    /// How much the window's background covers what's behind it (macOS).
    pub opacity: f32,
    /// ANSI 0-15.
    pub ansi: [Color; 16],
}

/// Settings ▸ Background: the names in the menu and their colors.
pub const BACKGROUNDS: [(&str, u32); 6] = [
    ("Midnight", 0x0f1117),
    ("Black", 0x000000),
    ("Graphite", 0x1c1c1f),
    ("Navy", 0x0b1528),
    ("Plum", 0x1a1122),
    ("Forest", 0x0c1a14),
];

/// How far `surface` is lifted from the background toward white.
pub const SURFACE_LIFT: f32 = 0.025;

/// Settings ▸ Background opacity: the percentages to pick from.
pub const OPACITIES: [u32; 9] = [100, 95, 90, 85, 80, 75, 70, 60, 50];

impl Theme {
    /// The dark theme on another background; panels and dividers are
    /// lifted from it by the same steps as on the default one.
    pub fn with_background(mut self, rgb: u32) -> Theme {
        let background = Color::hex(rgb);
        self.background = background;
        let lift = self.lift();
        self.surface = background.mix(lift, SURFACE_LIFT);
        self.raised = background.mix(lift, 0.0625);
        self.divider = background.mix(lift, 0.083 * if self.light() { 1.6 } else { 1.0 });
        self
    }

    /// The window background and the panels laid on it, see-through by
    /// `opacity`.
    pub fn with_opacity(mut self, opacity: f32) -> Theme {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// The window background, as see-through as the settings say.
    pub fn window(&self) -> Color {
        self.background.alpha(self.opacity)
    }

    /// A panel lifted from the background by `lift`, to draw over it: the
    /// color of `background.mix(white, lift)`, but only as opaque as the
    /// background under it.
    pub fn panel(&self, lift: f32) -> Color {
        self.lift().alpha(lift)
    }

    /// A light background (the Macintosh's white): panels are lifted
    /// toward black instead of white.
    pub fn light(&self) -> bool {
        let b = self.background;
        0.2126 * b.r + 0.7152 * b.g + 0.0722 * b.b > 0.5
    }

    /// A drop shadow's color, `strength` dark under a menu or card on a dark
    /// background; under half that on a light one, where black shows far more.
    pub fn shadow(&self, strength: f32) -> Color {
        Color::hex(0x000000).alpha(strength * if self.light() { 0.4 } else { 1.0 })
    }

    /// What panels are lifted toward: white, or black on a light background.
    pub fn lift(&self) -> Color {
        Color::hex(if self.light() { 0x000000 } else { 0xffffff })
    }

    pub fn dark() -> Theme {
        Theme {
            background: Color::hex(0x0f1117),
            surface: Color::hex(0x151922),
            raised: Color::hex(0x1e2430),
            divider: Color::hex(0x232a36),
            text: Color::hex(0xd8dee9),
            text_dim: Color::hex(0x8b95a7),
            text_faint: Color::hex(0x76879e),
            accent: Color::hex(0x6cb6ff),
            success: Color::hex(0x7bd88f),
            error: Color::hex(0xff6b6b),
            warning: Color::hex(0xf2cc60),
            string: Color::hex(0xe5c07b),
            variable: Color::hex(0xc792ea),
            selection: Color::hex(0x6cb6ff).alpha(0.30),
            cursor: Color::hex(0x6cb6ff),
            opacity: 1.0,
            ansi: [
                Color::hex(0x1d2028),
                Color::hex(0xf2737a),
                Color::hex(0x8ccf7e),
                Color::hex(0xe5c76b),
                Color::hex(0x67b0e8),
                Color::hex(0xc47fd5),
                Color::hex(0x6cbfbf),
                Color::hex(0xc4c9d1),
                Color::hex(0x4a5160),
                Color::hex(0xff8a92),
                Color::hex(0xa0e09a),
                Color::hex(0xf5d98a),
                Color::hex(0x8cc5f5),
                Color::hex(0xd79be6),
                Color::hex(0x8fd6d6),
                Color::hex(0xeef0f3),
            ],
        }
    }

    /// One entry of the xterm 256-color table.
    pub fn indexed(&self, index: u8) -> Color {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0.0 } else { (55.0 + 40.0 * v as f32) / 255.0 };
                Color { r: level(i / 36), g: level((i / 6) % 6), b: level(i % 6), a: 1.0 }
            }
            232..=255 => {
                let v = (8.0 + 10.0 * (index - 232) as f32) / 255.0;
                Color { r: v, g: v, b: v, a: 1.0 }
            }
        }
    }

    /// Resolves a cell color. `None` means the default background.
    pub fn resolve(&self, color: TermColor, overrides: &Colors, foreground: bool) -> Option<Color> {
        let pick = |index: usize, fallback: Color| overrides[index].map(Color::rgb).unwrap_or(fallback);
        Some(match color {
            TermColor::Spec(rgb) => Color::rgb(rgb),
            TermColor::Indexed(i) => pick(i as usize, self.indexed(i)),
            TermColor::Named(named) => match named {
                NamedColor::Foreground | NamedColor::BrightForeground => {
                    pick(NamedColor::Foreground as usize, self.text)
                }
                NamedColor::Background => {
                    // Inverse text uses the background as its foreground.
                    let set = overrides[NamedColor::Background as usize].map(Color::rgb);
                    return if foreground { Some(set.unwrap_or(self.background)) } else { set };
                }
                NamedColor::Cursor => self.cursor,
                NamedColor::DimForeground => self.text.mix(self.background, 0.35),
                NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite => {
                    let base = named as usize - NamedColor::DimBlack as usize;
                    pick(base, self.ansi[base]).mix(self.background, 0.35)
                }
                other => {
                    let index = other as usize;
                    pick(index, self.ansi[index.min(15)])
                }
            },
        })
    }

    /// The RGB a terminal program gets back from an OSC 4/10/11/12 query.
    pub fn query_color(&self, index: usize, overrides: &Colors) -> Rgb {
        if let Some(rgb) = overrides[index] {
            return rgb;
        }
        let color = match index {
            0..=255 => self.indexed(index as u8),
            i if i == NamedColor::Foreground as usize => self.text,
            i if i == NamedColor::Background as usize => self.background,
            i if i == NamedColor::Cursor as usize => self.cursor,
            _ => self.text,
        };
        color.to_term()
    }
}

/// A terminal color scheme: what the theme library holds. The UI's
/// colors are worked out from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub name: String,
    pub background: u32,
    pub foreground: u32,
    pub cursor: Option<u32>,
    pub selection: Option<u32>,
    /// The UI's highlight color (ANSI 12 when unset).
    pub accent: Option<u32>,
    pub ansi: [u32; 16],
    /// The theme file it came from: a theme of your own, which the theme
    /// editor can change. None for the built-in presets, which stay as
    /// they are.
    pub file: Option<std::path::PathBuf>,
}

/// The built-in schemes, each the screen of a real machine: name,
/// background, foreground, cursor, selection, UI accent (0: ANSI 12) and
/// ANSI 0-15, from the machine's own palette where it had one. Temino's own
/// comes first and is the default.
type BuiltIn = (&'static str, u32, u32, u32, u32, u32, [u32; 16]);
const BUILT_IN: [BuiltIn; 11] = [
    ("Temino", 0x0f1117, 0xd8dee9, 0x6cb6ff, 0, 0, [0; 16]),
    // Green phosphor (Monitor II), the lo-res colors for the rest.
    (
        "Apple II",
        0x050c06,
        0x41ff53,
        0x41ff53,
        0x1c4a22,
        0x66aaff,
        [
            0x0c140d, 0xdd0033, 0x11dd00, 0xff6600, 0x2222ff, 0xdd22dd, 0x44ff99, 0xaaaaaa, 0x555555, 0xff9988,
            0x41ff53, 0xffff00, 0x66aaff, 0xff88ff, 0x88ffcc, 0xffffff,
        ],
    ),
    // The 1984 Macintosh: one bit, black on white. Its colors are the
    // grays QuickDraw dithered them to, so red and green still differ.
    (
        "Macintosh",
        0xffffff,
        0x000000,
        0x000000,
        0x000000,
        0x000000,
        [
            0x000000, 0x5a5a5a, 0x3c3c3c, 0x6e6e6e, 0x282828, 0x505050, 0x464646, 0x8c8c8c, //
            0x646464, 0x2a2a2a, 0x1e1e1e, 0x3a3a3a, 0x141414, 0x323232, 0x282828, 0x000000,
        ],
    ),
    // The CGA/VGA text palette, light grey on black.
    (
        "IBM PC (MS-DOS)",
        0x000000,
        0xaaaaaa,
        0xaaaaaa,
        0x0000aa,
        0x5555ff,
        [
            0x000000, 0xaa0000, 0x00aa00, 0xaa5500, 0x0000aa, 0xaa00aa, 0x00aaaa, 0xaaaaaa, 0x555555, 0xff5555,
            0x55ff55, 0xffff55, 0x5555ff, 0xff55ff, 0x55ffff, 0xffffff,
        ],
    ),
    // Light blue on blue, the VIC-II's colors (Pepto's measurements).
    (
        "Commodore 64",
        0x352879,
        0x6c5eb5,
        0x6c5eb5,
        0x6c5eb5,
        0x70a4b2,
        [
            0x000000, 0x9a6759, 0x588d43, 0xb8c76f, 0x6c5eb5, 0x8a5aa6, 0x70a4b2, 0x959595, 0x444444, 0xc28b7d,
            0x9ad284, 0xd8e48f, 0xa497f0, 0xb07ccc, 0x9ad0dc, 0xffffff,
        ],
    ),
    // Workbench 1.3: white on blue, orange and black.
    (
        "Amiga 500",
        0x0055aa,
        0xffffff,
        0xff8800,
        0xff8800,
        0xff8800,
        [
            0x000022, 0xff6644, 0x66dd66, 0xff8800, 0x99ccff, 0xff88cc, 0x88eeff, 0xffffff, 0x335f8f, 0xff8877,
            0x99ff99, 0xffaa33, 0xbbddff, 0xffaadd, 0xaaffff, 0xffffff,
        ],
    ),
    // Black ink on white paper, the ULA's eight colors and their brights.
    (
        "ZX Spectrum",
        0xd7d7d7,
        0x000000,
        0x000000,
        0x00d7d7,
        0x0000d7,
        [
            0x000000, 0xd70000, 0x008a00, 0x8a6a00, 0x0000d7, 0xd700d7, 0x008a8a, 0x5a5a5a, 0x3a3a3a, 0xff0000,
            0x00a800, 0xa08800, 0x0000ff, 0xff00ff, 0x00a0a0, 0x000000,
        ],
    ),
    // Amber phosphor; the terminal had no colors, so they're shades of it.
    (
        "DEC VT220",
        0x140c00,
        0xffb000,
        0xffb000,
        0x4a3000,
        0,
        [
            0x2a1a00, 0xff7a00, 0xffc040, 0xffd060, 0xc88a00, 0xff9a40, 0xffc880, 0xffb000, 0x6a4800, 0xff9a30,
            0xffd070, 0xffe090, 0xe0a020, 0xffb870, 0xffe0b0, 0xfff0d0,
        ],
    ),
    // IRIX on an Indy: winterm's light text on Indigo Magic slate.
    (
        "SGI Indy",
        0x1d2733,
        0xe6e1cf,
        0xa68ad6,
        0x3d4a66,
        0xa68ad6,
        [
            0x2b3640, 0xe0605a, 0x8fc36a, 0xe8c56a, 0x7d9fd6, 0xa68ad6, 0x6cc3c8, 0xd9d4c3, 0x5b6874, 0xf08078,
            0xa8d888, 0xf2d890, 0x9cb8e8, 0xbfa6ea, 0x8ad8dc, 0xffffff,
        ],
    ),
    // NeXTstation: black on the 2-bit gray of its windows; the Color
    // model's muted tones for the rest.
    (
        "NeXTstation",
        0xaaaaaa,
        0x000000,
        0x000000,
        0x555555,
        0x1a2f7a,
        [
            0x000000, 0x8b1a1a, 0x1f5f1f, 0x5a4a00, 0x1a2f7a, 0x6a1f6a, 0x1a5f5f, 0x555555, //
            0x2a2a2a, 0xa52a2a, 0x2a7a2a, 0x6b5a00, 0x2a46a0, 0x8a2a8a, 0x2a7a7a, 0x333333,
        ],
    ),
    // Windows PowerShell's console: light gray on its dark blue, the
    // console's Campbell colors.
    (
        "Windows PowerShell",
        0x012456,
        0xcccccc,
        0xfedba9,
        0x264f78,
        0x3b78ff,
        [
            0x0c0c0c, 0xc50f1f, 0x13a10e, 0xc19c00, 0x0037da, 0x881798, 0x3a96dd, 0xcccccc, //
            0x767676, 0xe74856, 0x16c60c, 0xf9f1a5, 0x3b78ff, 0xb4009e, 0x61d6d6, 0xf2f2f2,
        ],
    ),
];

/// The theme library: the built-in schemes, then any Ghostty-format theme
/// files in `dir` (one per file, named after it), alphabetically. A file
/// named like a preset joins as "<name> (Custom)": presets are never
/// replaced.
pub fn library(dir: &str) -> Vec<Palette> {
    let mut themes: Vec<Palette> = BUILT_IN
        .iter()
        .map(|&(name, background, foreground, cursor, selection, accent, ansi)| Palette {
            name: name.to_owned(),
            background,
            foreground,
            cursor: Some(cursor),
            selection: (selection != 0).then_some(selection),
            accent: (accent != 0).then_some(accent),
            ansi,
            file: None,
        })
        .collect();
    let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).filter(|n| !n.starts_with('.')) else { continue };
        let mut name = name.trim_end_matches(".conf").to_owned();
        if themes.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
            name.push_str(" (Custom)");
        }
        if themes.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let text = std::fs::read_to_string(&path).ok();
        if let Some(mut palette) = text.and_then(|text| parse_palette(&name, &text)) {
            palette.file = Some(path);
            themes.push(palette);
        }
    }
    themes
}

/// A Ghostty theme file: `background = #282a36`, `foreground = …`,
/// `cursor-color = …`, `selection-background = …`, `palette = 0=#21222c`,
/// and Temino's own `accent = …`. None without a background and foreground.
pub fn parse_palette(name: &str, text: &str) -> Option<Palette> {
    let hex = |value: &str| u32::from_str_radix(value.trim().trim_start_matches('#'), 16).ok();
    let (mut background, mut foreground, mut cursor, mut selection, mut accent) = (None, None, None, None, None);
    let mut ansi = Theme::dark().ansi.map(|color| {
        let [r, g, b] = color.to_rgb8();
        (r as u32) << 16 | (g as u32) << 8 | b as u32
    });
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "background" => background = hex(value),
            "foreground" => foreground = hex(value),
            "cursor-color" => cursor = hex(value),
            "selection-background" => selection = hex(value),
            "accent" => accent = hex(value),
            "palette" => {
                if let Some((index, color)) = value.split_once('=') {
                    if let (Ok(index @ 0..=15), Some(color)) = (index.trim().parse::<usize>(), hex(color)) {
                        ansi[index] = color;
                    }
                }
            }
            _ => {}
        }
    }
    Some(Palette {
        name: name.to_owned(),
        background: background?,
        foreground: foreground?,
        cursor,
        selection,
        accent,
        ansi,
        file: None,
    })
}

impl Palette {
    /// The whole theme, UI included, in this scheme's colors.
    pub fn theme(&self) -> Theme {
        if self.name == BUILT_IN[0].0 && self.ansi == [0; 16] {
            return Theme::dark();
        }
        let background = Color::hex(self.background);
        let text = Color::hex(self.foreground);
        let ansi = self.ansi.map(Color::hex);
        let accent = Color::hex(self.accent.unwrap_or(self.ansi[12]));
        let theme = Theme {
            background,
            surface: background,
            raised: background,
            divider: background,
            text,
            text_dim: text.mix(background, 0.38),
            text_faint: text.mix(background, 0.6),
            accent,
            success: ansi[2],
            error: ansi[1],
            warning: ansi[3],
            string: ansi[3],
            variable: ansi[5],
            selection: match self.selection {
                Some(rgb) => Color::hex(rgb).alpha(0.75),
                None => accent.alpha(0.3),
            },
            cursor: self.cursor.map(Color::hex).unwrap_or(accent),
            opacity: 1.0,
            ansi,
        };
        theme.with_background(self.background)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghostty_theme_files_are_read() {
        let text = "# a comment\nbackground = #101010\nforeground=ffffff\npalette = 1=#ff0000\npalette = 99=#00ff00\n";
        let palette = parse_palette("Test", text).unwrap();
        assert_eq!((palette.background, palette.foreground, palette.ansi[1]), (0x101010, 0xffffff, 0xff0000));
        assert!(parse_palette("Bad", "foreground = #ffffff").is_none());
    }

    #[test]
    fn the_default_themes_small_text_meets_wcag_contrast() {
        // text_faint carries real reading text — Settings section
        // headings at 10.5px, timestamps, the sidebar's counts — so all
        // three text colors must clear 4.5:1 on the surfaces they sit
        // on. (The machine palettes are exempt: their colors are the
        // machines' own, picked knowingly.)
        let theme = Theme::dark();
        for ink in [theme.text, theme.text_dim, theme.text_faint] {
            assert!(ink.contrast(theme.background) >= 4.5, "on background: {:.2}", ink.contrast(theme.background));
            assert!(ink.contrast(theme.surface) >= 4.5, "on surface: {:.2}", ink.contrast(theme.surface));
        }
        // The helper itself: black on white is the maximum, 21:1.
        assert!((Color::hex(0x000000).contrast(Color::hex(0xffffff)) - 21.0).abs() < 0.1);
        assert!((theme.text.contrast(theme.text) - 1.0).abs() < 0.01);
    }

    #[test]
    fn the_default_theme_is_unchanged() {
        let library = library("/nonexistent");
        assert_eq!(library[0].theme().accent, Theme::dark().accent);
        assert_eq!(library.len(), BUILT_IN.len());
    }

    #[test]
    fn every_color_a_ghostty_theme_sets_is_read() {
        let mut text = String::from("background = #282a36\nforeground = f8f8f2\ncursor-color = #ff79c6\n");
        text.push_str("selection-background =   #44475a  \n");
        for i in 0..16 {
            text.push_str(&format!("palette = {i}=#{:06x}\n", 0x10 * (i + 1)));
        }
        let palette = parse_palette("Dracula", &text).unwrap();
        assert_eq!(palette.name, "Dracula");
        assert_eq!((palette.background, palette.foreground), (0x282a36, 0xf8f8f2));
        assert_eq!((palette.cursor, palette.selection, palette.accent), (Some(0xff79c6), Some(0x44475a), None));
        assert_eq!(palette.ansi, std::array::from_fn(|i| 0x10 * (i as u32 + 1)));
    }

    #[test]
    fn broken_lines_in_a_theme_file_are_skipped() {
        let defaults = parse_palette("x", "background=#000000\nforeground=#ffffff").unwrap().ansi;
        let text = "\
            # palette = 2=#00ff00\n\
            just some words\n\
            background = #101010\r\n\
            foreground = #eeeeee\n\
            font-family = Menlo\n\
            palette = 16=#123456\n\
            palette = -1=#123456\n\
            palette = x=#123456\n\
            palette = #123456\n\
            palette = 3=#zzzzzz\n\
            palette = 4 = #abcdef\n\
            cursor-color = not a color\n\
            =\n";
        let palette = parse_palette("Broken", text).unwrap();
        assert_eq!((palette.background, palette.foreground), (0x101010, 0xeeeeee));
        assert_eq!(palette.cursor, None);
        // Only the one good palette line took; the rest keep Temino's own.
        let mut expected = defaults;
        expected[4] = 0xabcdef;
        assert_eq!(palette.ansi, expected);
        // Without a readable background and foreground there's no theme.
        assert!(parse_palette("x", "background = #12345g\nforeground = #ffffff").is_none());
        assert!(parse_palette("x", "background = #000000").is_none());
        assert!(parse_palette("x", "").is_none());
    }

    #[test]
    fn a_theme_files_colors_become_the_ui() {
        let mut text = String::from("background = #fdf6e3\nforeground = #657b83\n");
        for (i, rgb) in [(1, 0xdc322f), (2, 0x859900), (3, 0xb58900), (5, 0xd33682), (12, 0x268bd2)] {
            text.push_str(&format!("palette = {i}=#{rgb:06x}\n"));
        }
        let theme = parse_palette("Solarized Light", &text).unwrap().theme();
        assert_eq!(theme.background, Color::hex(0xfdf6e3));
        assert_eq!(theme.text, Color::hex(0x657b83));
        assert_eq!(
            (theme.error, theme.success, theme.warning),
            (Color::hex(0xdc322f), Color::hex(0x859900), Color::hex(0xb58900))
        );
        // No accent or cursor set: both are ANSI 12; selection is the accent, faint.
        assert_eq!(theme.accent, Color::hex(0x268bd2));
        assert_eq!(theme.cursor, theme.accent);
        assert_eq!(theme.selection, theme.accent.alpha(0.3));
        // A light background lifts panels toward black.
        assert!(theme.light());
        assert_eq!(theme.lift(), Color::hex(0x000000));
    }

    #[test]
    fn theme_files_join_the_library_by_name() {
        let dir = std::env::temp_dir().join(format!("temino-themes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = "background = #111111\nforeground = #eeeeee\n";
        std::fs::write(dir.join("Zenburn.conf"), good).unwrap();
        std::fs::write(dir.join("Ayu"), good).unwrap();
        std::fs::write(dir.join(".DS_Store"), good).unwrap();
        std::fs::write(dir.join("Notes"), "not a theme").unwrap();
        std::fs::write(dir.join("Apple II"), "background = #000000\nforeground = #00ff00\n").unwrap();
        let library = library(dir.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let names: Vec<&str> = library.iter().map(|p| p.name.as_str()).collect();
        // Hidden files and files that aren't themes are left out; one named
        // like a built-in joins beside it, the preset left as it was.
        assert_eq!(&names[BUILT_IN.len()..], ["Apple II (Custom)", "Ayu", "Zenburn"]);
        assert_eq!(library.len(), BUILT_IN.len() + 3);
        let preset = library.iter().find(|p| p.name == "Apple II").unwrap();
        assert_eq!((preset.foreground, preset.file.is_none()), (0x41ff53, true));
        let custom = library.iter().find(|p| p.name == "Apple II (Custom)").unwrap();
        assert_eq!(custom.foreground, 0x00ff00);
        assert_eq!(custom.file.as_deref(), Some(dir.join("Apple II").as_path()));
        assert_eq!(names[0], "Temino");
    }
}

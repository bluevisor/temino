//! Colors: the UI palette and the terminal's 256-color table.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
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
            text_faint: Color::hex(0x566073),
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
}

/// The built-in schemes, each the screen of a real machine: name,
/// background, foreground, cursor, selection, UI accent (0: ANSI 12) and
/// ANSI 0-15, from the machine's own palette where it had one. Temino's own
/// comes first and is the default.
type BuiltIn = (&'static str, u32, u32, u32, u32, u32, [u32; 16]);
const BUILT_IN: [BuiltIn; 13] = [
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
    // Black on white, in the Macintosh II's 16-color system palette.
    (
        "Macintosh II",
        0xffffff,
        0x000000,
        0x000000,
        0xb4c8f0,
        0x0000d3,
        [
            0x000000, 0xdd0907, 0x006412, 0x90713a, 0x0000d3, 0xf20884, 0x02abea, 0x808080, 0x404040, 0xff6403,
            0x1fb714, 0x562c05, 0x4700a5, 0xf20884, 0x02abea, 0x404040,
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
    // READY: light blue on blue, GTIA hues.
    (
        "Atari 800",
        0x1a4f9a,
        0x9ccbff,
        0x9ccbff,
        0x3f74c0,
        0,
        [
            0x0a1f40, 0xe05050, 0x50c050, 0xd0c040, 0x6090ff, 0xc060c0, 0x50c0c0, 0xc0d8ff, 0x405880, 0xff7070,
            0x80e080, 0xf0e060, 0x90b8ff, 0xe090e0, 0x80e0e0, 0xffffff,
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
    // Mode 7 teletext: white on black and the seven pure colors.
    (
        "BBC Micro",
        0x000000,
        0xffffff,
        0xffffff,
        0x0000ff,
        0x00ffff,
        [
            0x000000, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff, 0x808080, 0xff0000,
            0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
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
    // The 3279's seven field colors, green the default.
    (
        "IBM 3279",
        0x000000,
        0x00ff00,
        0x00ff00,
        0x1f4f1f,
        0x40e0d0,
        [
            0x000000, 0xff3030, 0x00ff00, 0xffff00, 0x5b8cff, 0xff73ff, 0x40e0d0, 0xffffff, 0x4a4a4a, 0xff6060,
            0x60ff60, 0xffff80, 0x7aa4ff, 0xff9aff, 0x70f0e0, 0xffffff,
        ],
    ),
    // OpenWindows: dark text on the OPEN LOOK cream, Sun purple.
    (
        "Sun SPARCstation",
        0xf3efe2,
        0x1a1a1a,
        0x1a1a1a,
        0xc8bfe0,
        0x5a3f9e,
        [
            0x1a1a1a, 0xb00000, 0x007000, 0x8a5a00, 0x0000b0, 0x8a008a, 0x007070, 0x707070, 0x404040, 0xd00000,
            0x009000, 0x6a4a00, 0x5a3f9e, 0xa000a0, 0x009090, 0x202020,
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
];

/// The theme library: the built-in schemes, then any Ghostty-format theme
/// files in `dir` (one per file, named after it), alphabetically.
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
        })
        .collect();
    let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for path in files {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).filter(|n| !n.starts_with('.')) else { continue };
        let name = name.trim_end_matches(".conf").to_owned();
        if let Some(palette) = std::fs::read_to_string(&path).ok().and_then(|text| parse_palette(&name, &text)) {
            themes.retain(|t| t.name != palette.name);
            themes.push(palette);
        }
    }
    themes
}

/// A Ghostty theme file: `background = #282a36`, `foreground = …`,
/// `cursor-color = …`, `selection-background = …`, `palette = 0=#21222c`.
/// None without a background and foreground.
pub fn parse_palette(name: &str, text: &str) -> Option<Palette> {
    let hex = |value: &str| u32::from_str_radix(value.trim().trim_start_matches('#'), 16).ok();
    let (mut background, mut foreground, mut cursor, mut selection) = (None, None, None, None);
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
        accent: None,
        ansi,
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
    fn the_default_theme_is_unchanged() {
        let library = library("/nonexistent");
        assert_eq!(library[0].theme().accent, Theme::dark().accent);
        assert_eq!(library.len(), BUILT_IN.len());
    }
}

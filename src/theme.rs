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
        let white = Color::hex(0xffffff);
        self.background = background;
        self.surface = background.mix(white, SURFACE_LIFT);
        self.raised = background.mix(white, 0.0625);
        self.divider = background.mix(white, 0.083);
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
        Color::hex(0xffffff).alpha(lift)
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

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
    /// ANSI 0-15.
    pub ansi: [Color; 16],
}

impl Theme {
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

//! Fonts, glyph rasterization (fontdue) and the R8 glyph atlas the GPU
//! samples. Box-drawing and block characters are drawn procedurally so they
//! join seamlessly across cells, as in Alacritty and Warp.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

use crate::scene::{Rect, Scene};
use crate::theme::Color;

pub const ATLAS_SIZE: usize = 2048;
/// The RGBA atlas for color emoji, drawn by CoreText on macOS.
pub const COLOR_ATLAS_SIZE: usize = 1024;

const MONO_FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/JetBrainsMonoNL-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMonoNL-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMonoNL-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMonoNL-BoldItalic.ttf"),
];

const EMOJI: &str = "NotoEmoji-Regular.ttf";
const EMOJI_FONT: &[u8] = include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf");

/// Where each system keeps the UI and fallback faces, searched in order.
/// `~` is $HOME.
const SYSTEM_FONTS: &[&str] = &[
    "/boot/system/data/fonts/ttfonts",
    "~/config/non-packaged/data/fonts/ttfonts",
    "~/config/non-packaged/data/fonts",
    "/boot/system/non-packaged/data/fonts/ttfonts",
    "/usr/share/fonts/truetype/noto",
    "/usr/share/fonts/noto",
    "/usr/share/fonts/truetype/dejavu",
    "/usr/share/fonts/TTF",
    "/System/Library/Fonts/Supplemental",
    "/System/Library/Fonts",
    "/Library/Fonts",
];

/// Chinese, Japanese and Korean faces: (path, face in the collection). The
/// monospace Noto CJK SC face first; it's large, so it loads the first time
/// so it parses on a thread at startup (a quarter second on an M5) and joins
/// the fallbacks when the first CJK character is drawn. `~` is $HOME.
const CJK_FONTS: &[(&str, u32)] = &[
    ("~/config/non-packaged/data/fonts/NotoSansCJK/NotoSansCJK-Regular.ttc", 7),
    ("/boot/system/non-packaged/data/fonts/NotoSansCJK/NotoSansCJK-Regular.ttc", 7),
    ("/boot/system/data/fonts/otfonts/NotoSansCJK-Regular.ttc", 7),
    ("/boot/system/data/fonts/ttfonts/NotoSansCJK-Regular.ttc", 7),
    // Stock Haiku: Japanese-default variable fonts (weight axis defaults to 400).
    ("/boot/system/data/fonts/otfonts/NotoSansMonoCJKjp-VF.otf", 0),
    ("/boot/system/data/fonts/otfonts/NotoSansCJKjp-VF.otf", 0),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 7),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 7),
    ("/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc", 7),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/STHeiti Light.ttc", 0),
];

fn load_cjk_font() -> Option<Font> {
    let home = std::env::var("HOME").unwrap_or_default();
    CJK_FONTS.iter().find_map(|(path, index)| {
        let path = path.replacen('~', &home, 1);
        let data = std::fs::read(path).ok()?;
        Font::from_bytes(data, FontSettings { collection_index: *index, ..FontSettings::default() }).ok()
    })
}

/// Whether `c` is from the CJK blocks (or fullwidth forms) the CJK font covers.
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2e80..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0xfe30..=0xfe4f
        | 0xff00..=0xffef | 0x20000..=0x3134f)
}

/// Index of a font in `TextSystem::fonts`.
pub type FontId = u8;
pub const UI: FontId = 4;
pub const UI_BOLD: FontId = 5;
const BUILTIN: FontId = 255;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const BOLD: Style = Style { bold: true, italic: false };

    fn mono_font(self) -> FontId {
        self.bold as u8 | (self.italic as u8) << 1
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the cell top to the baseline.
    pub baseline: f32,
    pub size: f32,
    pub underline: f32,
    pub stroke: f32,
}

#[derive(Clone, Copy)]
struct CachedGlyph {
    uv: [f32; 4],
    width: f32,
    height: f32,
    /// Offset of the bitmap's left edge from the pen.
    left: f32,
    /// Offset of the bitmap's top edge from the baseline (negative = above).
    top: f32,
    advance: f32,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GlyphKey {
    font: FontId,
    glyph: u32,
    size: u32,
}

/// A color emoji's place in the color atlas, and its width and height in
/// logical pixels.
type Emoji = ([f32; 4], f32, f32);

/// The CPU copy of an atlas, `channels` bytes a texel (1 for coverage, 4
/// for color); the renderer uploads `dirty` rows.
pub struct Atlas {
    pub pixels: Vec<u8>,
    pub dirty: Option<(usize, usize)>,
    size: usize,
    channels: usize,
    shelf_x: usize,
    shelf_y: usize,
    shelf_height: usize,
    pub generation: u64,
}

impl Atlas {
    fn new(size: usize, channels: usize) -> Atlas {
        Atlas {
            pixels: vec![0; size * size * channels],
            dirty: Some((0, size)),
            size,
            channels,
            shelf_x: 1,
            shelf_y: 1,
            shelf_height: 0,
            generation: 0,
        }
    }

    fn clear(&mut self) {
        self.pixels.fill(0);
        self.dirty = Some((0, self.size));
        self.shelf_x = 1;
        self.shelf_y = 1;
        self.shelf_height = 0;
        self.generation += 1;
    }

    /// Reserves a `w`x`h` slot (1 px apart); `None` when the atlas is full.
    fn allocate(&mut self, w: usize, h: usize) -> Option<(usize, usize)> {
        if w + 2 > self.size || h + 2 > self.size {
            return None;
        }
        if self.shelf_x + w + 1 > self.size {
            self.shelf_y += self.shelf_height + 1;
            self.shelf_x = 1;
            self.shelf_height = 0;
        }
        if self.shelf_y + h + 1 > self.size {
            return None;
        }
        let slot = (self.shelf_x, self.shelf_y);
        self.shelf_x += w + 1;
        self.shelf_height = self.shelf_height.max(h);
        Some(slot)
    }

    fn write(&mut self, x: usize, y: usize, w: usize, h: usize, texels: &[u8]) {
        let (row_bytes, c) = (w * self.channels, self.channels);
        for row in 0..h {
            let start = ((y + row) * self.size + x) * c;
            self.pixels[start..start + row_bytes].copy_from_slice(&texels[row * row_bytes..(row + 1) * row_bytes]);
        }
        let (lo, hi) = self.dirty.unwrap_or((y, y + h));
        self.dirty = Some((lo.min(y), hi.max(y + h)));
    }
}

pub struct TextSystem {
    fonts: Vec<Font>,
    /// Fonts searched, in order, for characters the primary font lacks.
    fallbacks: Vec<FontId>,
    cache: HashMap<GlyphKey, Option<CachedGlyph>>,
    lookup: HashMap<(char, FontId), (FontId, u16)>,
    /// The CJK font, still parsing until the first CJK character asks.
    cjk_loading: Option<std::sync::mpsc::Receiver<Option<Font>>>,
    pub atlas: Atlas,
    /// Color emoji (macOS): by text and physical size, where they are in
    /// `color_atlas` and their size in logical pixels; `None` when Apple
    /// Color Emoji has no glyph for the text.
    pub color_atlas: Atlas,
    emoji: HashMap<(String, u32), Option<Emoji>>,
    /// In logical pixels, though each is a whole number of physical ones.
    pub cell: CellMetrics,
    /// Physical pixels per logical one: glyphs rasterize at this density.
    pub scale: f32,
}

fn load_system_font(name: &str) -> Option<Font> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data =
        SYSTEM_FONTS.iter().find_map(|dir| std::fs::read(format!("{}/{name}", dir.replacen('~', &home, 1))).ok())?;
    Font::from_bytes(data, FontSettings::default()).ok()
}

/// The first of `names` that loads.
fn first_font(names: &[&str]) -> Option<Font> {
    names.iter().find_map(|name| load_system_font(name))
}

impl TextSystem {
    pub fn new(size: f32) -> TextSystem {
        let mut fonts: Vec<Font> =
            MONO_FONTS.iter().map(|data| Font::from_bytes(*data, FontSettings::default()).unwrap()).collect();
        // The UI face; the bundled mono stands in when Noto Sans is missing.
        let ui = first_font(&["NotoSans-Regular.ttf", "Ubuntu-R.ttf", "DejaVuSans.ttf", "Arial.ttf"])
            .unwrap_or_else(|| Font::from_bytes(MONO_FONTS[0], FontSettings::default()).unwrap());
        let ui_bold = first_font(&["NotoSans-Bold.ttf", "Ubuntu-B.ttf", "DejaVuSans-Bold.ttf", "Arial Bold.ttf"])
            .unwrap_or_else(|| Font::from_bytes(MONO_FONTS[1], FontSettings::default()).unwrap());
        fonts.push(ui);
        fonts.push(ui_bold);
        let mut fallbacks = Vec::new();
        for name in [
            "NotoSansSymbols-Regular.ttf",
            "NotoSansSymbols2-Regular.ttf",
            "NotoSans-Regular.ttf",
            "NotoMono-Regular.ttf",
            "DejaVuSans.ttf",
            "DejaVuSansMono.ttf",
            EMOJI,
            "Apple Symbols.ttf",
            "Arial Unicode.ttf",
        ] {
            // Monochrome Noto Emoji is bundled: fontdue can't draw color
            // emoji fonts, and macOS has only Apple Color Emoji.
            let font = if name == EMOJI {
                Font::from_bytes(EMOJI_FONT, FontSettings::default()).ok()
            } else {
                load_system_font(name)
            };
            if let Some(font) = font {
                fallbacks.push(fonts.len() as FontId);
                fonts.push(font);
            }
        }
        let mut system = TextSystem {
            fonts,
            fallbacks,
            cache: HashMap::new(),
            lookup: HashMap::new(),
            cjk_loading: {
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = sender.send(load_cjk_font());
                });
                Some(receiver)
            },
            atlas: Atlas::new(ATLAS_SIZE, 1),
            color_atlas: Atlas::new(if cfg!(target_os = "macos") { COLOR_ATLAS_SIZE } else { 0 }, 4),
            emoji: HashMap::new(),
            cell: CellMetrics { width: 0.0, height: 0.0, baseline: 0.0, size, underline: 0.0, stroke: 1.0 },
            scale: 1.0,
        };
        system.set_size(size);
        system
    }

    pub fn set_scale(&mut self, scale: f32) {
        if scale > 0.0 && scale != self.scale {
            self.scale = scale;
            self.set_size(self.cell.size);
        }
    }

    /// Rounds a logical coordinate to the nearest physical pixel.
    fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    pub fn set_size(&mut self, size: f32) {
        // Metrics are worked out in physical pixels, so cells tile exactly.
        let s = self.scale;
        let physical = size * s;
        let font = &self.fonts[0];
        let line = font.horizontal_line_metrics(physical).expect("mono font has horizontal metrics");
        let width = font.metrics('M', physical).advance_width.round().max(1.0);
        // A little extra leading reads better than the font's tight default.
        let height = (line.new_line_size * 1.12).round();
        let extra = height - (line.ascent - line.descent);
        let baseline = (extra / 2.0 + line.ascent).round();
        self.cell = CellMetrics {
            width: width / s,
            height: height / s,
            baseline: baseline / s,
            size,
            underline: (baseline + (physical * 0.12).max(1.0)).round() / s,
            stroke: (physical / 14.0).round().max(1.0) / s,
        };
        self.cache.clear();
        self.atlas.clear();
        self.emoji.clear();
        self.color_atlas.clear();
    }

    /// Which font draws `c` for a request in `primary`.
    fn resolve(&mut self, c: char, primary: FontId) -> (FontId, u16) {
        if let Some(found) = self.lookup.get(&(c, primary)) {
            return *found;
        }
        if is_cjk(c) {
            if let Some(loading) = self.cjk_loading.take() {
                if let Ok(Some(font)) = loading.recv() {
                    // Ahead of the other fallbacks: Noto Sans has a few CJK
                    // punctuation marks in another style.
                    self.fallbacks.insert(0, self.fonts.len() as FontId);
                    self.fonts.push(font);
                }
            }
        }
        let mut found = self.find(c, primary);
        if found.1 == 0 {
            // No installed font has it: draw the nearest symbol one does.
            if let Some(substitute) = substitute(c) {
                found = self.find(substitute, primary);
            }
        }
        self.lookup.insert((c, primary), found);
        found
    }

    fn find(&self, c: char, primary: FontId) -> (FontId, u16) {
        let index = self.fonts[primary as usize].lookup_glyph_index(c);
        if index != 0 || c == ' ' {
            return (primary, index);
        }
        let candidates = std::iter::once(0).chain(self.fallbacks.iter().copied());
        for font in candidates {
            let index = self.fonts[font as usize].lookup_glyph_index(c);
            if index != 0 {
                return (font, index);
            }
        }
        (primary, 0)
    }

    pub fn has_glyph(&mut self, c: char, font: FontId) -> bool {
        self.resolve(c, font).1 != 0
    }

    fn glyph(&mut self, font: FontId, glyph: u16, size: f32) -> Option<CachedGlyph> {
        let s = self.scale;
        let key = GlyphKey { font, glyph: glyph as u32, size: (size * s * 8.0) as u32 };
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }
        let (metrics, coverage) = self.fonts[font as usize].rasterize_indexed(glyph, size * s);
        let entry = self.store(metrics.width, metrics.height, &coverage).map(|uv| CachedGlyph {
            uv,
            width: metrics.width as f32 / s,
            height: metrics.height as f32 / s,
            left: metrics.xmin as f32 / s,
            top: -(metrics.ymin as f32 + metrics.height as f32) / s,
            advance: metrics.advance_width / s,
        });
        let entry = entry.or({
            // Zero-size glyphs (spaces) still need their advance.
            Some(CachedGlyph {
                uv: [0.0; 4],
                width: 0.0,
                height: 0.0,
                left: 0.0,
                top: 0.0,
                advance: metrics.advance_width / s,
            })
        });
        self.cache.insert(key, entry);
        entry
    }

    fn store(&mut self, w: usize, h: usize, coverage: &[u8]) -> Option<[f32; 4]> {
        if w == 0 || h == 0 {
            return None;
        }
        let (x, y) = match self.atlas.allocate(w, h) {
            Some(slot) => slot,
            None => {
                // Full: start over. Glyphs come back as they are drawn.
                self.cache.clear();
                self.atlas.clear();
                self.atlas.allocate(w, h)?
            }
        };
        self.atlas.write(x, y, w, h, coverage);
        let s = ATLAS_SIZE as f32;
        Some([x as f32 / s, y as f32 / s, (x + w) as f32 / s, (y + h) as f32 / s])
    }

    /// Draws one terminal cell's character with its top-left at (x, y).
    /// `span` is 2 for wide characters.
    pub fn draw_cell(&mut self, scene: &mut Scene, c: char, x: f32, y: f32, style: Style, color: Color, span: usize) {
        // alacritty_terminal leaves '\t' in the cell a tab starts from.
        if c == ' ' || c.is_control() {
            return;
        }
        let cell = self.cell;
        if let Some(glyph) = self.builtin(c) {
            scene.glyph(Rect::new(x, y, glyph.width, glyph.height), glyph.uv, color);
            return;
        }
        let (font, index) = self.resolve(c, style.mono_font());
        // Fallback faces are scaled so their capitals match the mono font.
        let size = if font < 4 { cell.size } else { cell.size * 0.95 };
        let Some(glyph) = self.glyph(font, index, size) else { return };
        if glyph.width == 0.0 {
            return;
        }
        let mut gx = x + glyph.left;
        let slot = cell.width * span as f32;
        if glyph.advance > slot + 0.5 || font >= 4 {
            // Center glyphs that don't share the mono advance.
            gx = x + ((slot - glyph.width) / 2.0).max(0.0);
        }
        let gy = y + cell.baseline + glyph.top;
        scene.glyph(Rect::new(self.snap(gx), self.snap(gy), glyph.width, glyph.height), glyph.uv, color);
    }

    /// A character and the zero-width marks on it (variation selectors,
    /// skin tones, joined emoji): a color emoji where there is one, else
    /// each drawn in its cell.
    pub fn draw_cluster(
        &mut self,
        scene: &mut Scene,
        c: char,
        marks: &[char],
        x: f32,
        y: f32,
        style: Style,
        color: Color,
        span: usize,
    ) {
        let slot = self.cell.width * span as f32;
        if let Some((uv, w, h)) = self.color_emoji(c, marks, self.cell.size, slot, self.cell.height) {
            let rect = Rect::new(self.snap(x + (slot - w) / 2.0), self.snap(y + (self.cell.height - h) / 2.0), w, h);
            scene.color_glyph(rect, uv, color.a);
            return;
        }
        self.draw_cell(scene, c, x, y, style, color, span);
        for &mark in marks {
            self.draw_cell(scene, mark, x, y, style, color, span);
        }
    }

    /// `c` (with `marks`) in Apple Color Emoji at `size`, fit in `max_w` x
    /// `max_h`: its place in the color atlas and its size. Only on macOS,
    /// and only for wide (emoji-presentation) characters or ones asking for
    /// emoji with U+FE0F.
    fn color_emoji(&mut self, c: char, marks: &[char], size: f32, max_w: f32, max_h: f32) -> Option<Emoji> {
        #[cfg(target_os = "macos")]
        {
            let wide = crate::editor::width(c) == 2 && !is_cjk(c);
            if !wide && !marks.contains(&'\u{fe0f}') {
                return None;
            }
            let s = self.scale;
            let text: String = std::iter::once(c).chain(marks.iter().copied()).collect();
            let key = (text, (size * s * 8.0) as u32);
            if let Some(found) = self.emoji.get(&key) {
                return *found;
            }
            let found = crate::platform::emoji(&key.0, size * s, max_w * s, max_h * s).and_then(|(w, h, rgba)| {
                let (x, y) = match self.color_atlas.allocate(w, h) {
                    Some(slot) => slot,
                    None => {
                        // Full: start over; emoji come back as they are drawn.
                        self.emoji.clear();
                        self.color_atlas.clear();
                        self.color_atlas.allocate(w, h)?
                    }
                };
                self.color_atlas.write(x, y, w, h, &rgba);
                let n = COLOR_ATLAS_SIZE as f32;
                let uv = [x as f32 / n, y as f32 / n, (x + w) as f32 / n, (y + h) as f32 / n];
                Some((uv, w as f32 / s, h as f32 / s))
            });
            self.emoji.insert(key, found);
            found
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (c, marks, size, max_w, max_h);
            None
        }
    }

    /// Lays out proportional text on a baseline; returns the advance.
    pub fn draw_text(
        &mut self,
        scene: &mut Scene,
        text: &str,
        x: f32,
        baseline: f32,
        font: FontId,
        size: f32,
        color: Color,
    ) -> f32 {
        let mut pen = x;
        for c in text.chars() {
            if let Some((uv, w, h)) = self.color_emoji(c, &[], size, size * 1.4, size * 1.4) {
                let top = baseline - self.ui_ascent(size) * 0.72 / 2.0 - h / 2.0;
                scene.color_glyph(Rect::new(self.snap(pen), self.snap(top), w, h), uv, color.a);
                pen += w;
                continue;
            }
            let (face, index) = self.resolve(c, font);
            let Some(glyph) = self.glyph(face, index, size) else { continue };
            if glyph.width > 0.0 {
                let rect =
                    Rect::new(self.snap(pen + glyph.left), self.snap(baseline + glyph.top), glyph.width, glyph.height);
                scene.glyph(rect, glyph.uv, color);
            }
            pen += glyph.advance;
        }
        pen - x
    }

    /// `line` cut to fit `width`, with an ellipsis where it was cut.
    pub fn elide(&mut self, line: &str, font: FontId, size: f32, width: f32) -> String {
        if self.measure(line, font, size) <= width {
            return line.to_owned();
        }
        let chars: Vec<char> = line.chars().collect();
        let (mut low, mut high) = (0, chars.len());
        while low < high {
            let mid = (low + high).div_ceil(2);
            let candidate: String = chars[..mid].iter().collect::<String>() + "…";
            if self.measure(&candidate, font, size) <= width {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        if low == 0 {
            return String::new();
        }
        chars[..low].iter().collect::<String>().trim_end().to_owned() + "…"
    }

    pub fn measure(&mut self, text: &str, font: FontId, size: f32) -> f32 {
        let mut width = 0.0;
        for c in text.chars() {
            if let Some((_, w, _)) = self.color_emoji(c, &[], size, size * 1.4, size * 1.4) {
                width += w;
                continue;
            }
            let (face, index) = self.resolve(c, font);
            if let Some(glyph) = self.glyph(face, index, size) {
                width += glyph.advance;
            }
        }
        width
    }

    /// Mono text on the cell grid (one cell per char); returns the width.
    pub fn draw_mono(&mut self, scene: &mut Scene, text: &str, x: f32, y: f32, style: Style, color: Color) -> f32 {
        let mut pen = x;
        for c in text.chars() {
            // CJK takes two cells.
            let span = crate::editor::width(c);
            if span > 0 {
                self.draw_cluster(scene, c, &[], pen, y, style, color, span);
            }
            pen += span as f32 * self.cell.width;
        }
        pen - x
    }

    /// Ascent of the UI font at `size`, for vertical centering.
    pub fn ui_ascent(&self, size: f32) -> f32 {
        self.fonts[UI as usize].horizontal_line_metrics(size).map(|m| m.ascent).unwrap_or(size * 0.8)
    }

    /// A procedurally drawn cell-sized glyph for box drawing and blocks.
    fn builtin(&mut self, c: char) -> Option<CachedGlyph> {
        let code = c as u32;
        if !(0x2500..=0x259f).contains(&code) {
            return None;
        }
        let s = self.scale;
        let key = GlyphKey { font: BUILTIN, glyph: code, size: (self.cell.size * s * 8.0) as u32 };
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }
        let w = (self.cell.width * s).round() as usize;
        let h = (self.cell.height * s).round() as usize;
        let coverage = draw_builtin(code, w, h, (self.cell.stroke * s).round() as usize)?;
        let entry = self.store(w, h, &coverage).map(|uv| CachedGlyph {
            uv,
            width: w as f32 / s,
            height: h as f32 / s,
            left: 0.0,
            top: 0.0,
            advance: w as f32 / s,
        });
        self.cache.insert(key, entry);
        entry
    }
}

/// Stand-ins for symbols that terminal programs use but Haiku's fonts lack,
/// such as the tool-call markers of Claude Code-style agents (⏺ ⎿).
fn substitute(c: char) -> Option<char> {
    Some(match c {
        '⏺' | '⬤' | '🔴' | '🟢' | '🟡' => '●',
        '⎿' => '└',
        '⏵' | '⯈' => '▶',
        '⏴' | '⯇' => '◀',
        '⏸' => '‖',
        '⏹' | '⬛' => '■',
        '⏎' => '↵',
        '✻' | '✽' | '✶' | '✳' | '✢' => '*',
        '✔' | '✅' => '✓',
        '✘' | '❌' => '✗',
        '⚠' => '!',
        _ => return None,
    })
}

/// Line weights per arm (left, right, up, down): 0 none, 1 light, 2 heavy,
/// 3 double. Indexed by `code - 0x2500` for U+2500..U+257F.
const BOX: [[u8; 4]; 128] = {
    const fn b(l: u8, r: u8, u: u8, d: u8) -> [u8; 4] {
        [l, r, u, d]
    }
    [
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // ─ ━ │ ┃
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // dashed
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2),
        b(0, 1, 0, 1),
        b(0, 2, 0, 1),
        b(0, 1, 0, 2),
        b(0, 2, 0, 2), // ┌ ┍ ┎ ┏
        b(1, 0, 0, 1),
        b(2, 0, 0, 1),
        b(1, 0, 0, 2),
        b(2, 0, 0, 2), // ┐ ┑ ┒ ┓
        b(0, 1, 1, 0),
        b(0, 2, 1, 0),
        b(0, 1, 2, 0),
        b(0, 2, 2, 0), // └ ┕ ┖ ┗
        b(1, 0, 1, 0),
        b(2, 0, 1, 0),
        b(1, 0, 2, 0),
        b(2, 0, 2, 0), // ┘ ┙ ┚ ┛
        b(0, 1, 1, 1),
        b(0, 2, 1, 1),
        b(0, 1, 2, 1),
        b(0, 1, 1, 2), // ├ ┝ ┞ ┟
        b(0, 1, 2, 2),
        b(0, 2, 2, 1),
        b(0, 2, 1, 2),
        b(0, 2, 2, 2), // ┠ ┡ ┢ ┣
        b(1, 0, 1, 1),
        b(2, 0, 1, 1),
        b(1, 0, 2, 1),
        b(1, 0, 1, 2), // ┤ ┥ ┦ ┧
        b(1, 0, 2, 2),
        b(2, 0, 2, 1),
        b(2, 0, 1, 2),
        b(2, 0, 2, 2), // ┨ ┩ ┪ ┫
        b(1, 1, 0, 1),
        b(2, 1, 0, 1),
        b(1, 2, 0, 1),
        b(2, 2, 0, 1), // ┬ ┭ ┮ ┯
        b(1, 1, 0, 2),
        b(2, 1, 0, 2),
        b(1, 2, 0, 2),
        b(2, 2, 0, 2), // ┰ ┱ ┲ ┳
        b(1, 1, 1, 0),
        b(2, 1, 1, 0),
        b(1, 2, 1, 0),
        b(2, 2, 1, 0), // ┴ ┵ ┶ ┷
        b(1, 1, 2, 0),
        b(2, 1, 2, 0),
        b(1, 2, 2, 0),
        b(2, 2, 2, 0), // ┸ ┹ ┺ ┻
        b(1, 1, 1, 1),
        b(2, 1, 1, 1),
        b(1, 2, 1, 1),
        b(2, 2, 1, 1), // ┼ ┽ ┾ ┿
        b(1, 1, 2, 1),
        b(1, 1, 1, 2),
        b(1, 1, 2, 2),
        b(2, 1, 2, 1), // ╀ ╁ ╂ ╃
        b(1, 2, 2, 1),
        b(2, 1, 1, 2),
        b(1, 2, 1, 2),
        b(2, 2, 2, 1), // ╄ ╅ ╆ ╇
        b(2, 2, 1, 2),
        b(2, 1, 2, 2),
        b(1, 2, 2, 2),
        b(2, 2, 2, 2), // ╈ ╉ ╊ ╋
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // ╌ ╍ ╎ ╏
        b(3, 3, 0, 0),
        b(0, 0, 3, 3),
        b(0, 3, 0, 1),
        b(0, 1, 0, 3), // ═ ║ ╒ ╓
        b(0, 3, 0, 3),
        b(3, 0, 0, 1),
        b(1, 0, 0, 3),
        b(3, 0, 0, 3), // ╔ ╕ ╖ ╗
        b(0, 3, 1, 0),
        b(0, 1, 3, 0),
        b(0, 3, 3, 0),
        b(3, 0, 1, 0), // ╘ ╙ ╚ ╛
        b(1, 0, 3, 0),
        b(3, 0, 3, 0),
        b(0, 3, 1, 1),
        b(0, 1, 3, 3), // ╜ ╝ ╞ ╟
        b(0, 3, 3, 3),
        b(3, 0, 1, 1),
        b(1, 0, 3, 3),
        b(3, 0, 3, 3), // ╠ ╡ ╢ ╣
        b(3, 3, 0, 1),
        b(1, 1, 0, 3),
        b(3, 3, 0, 3),
        b(3, 3, 1, 0), // ╤ ╥ ╦ ╧
        b(1, 1, 3, 0),
        b(3, 3, 3, 0),
        b(3, 3, 1, 1),
        b(1, 1, 3, 3), // ╨ ╩ ╪ ╫
        b(3, 3, 3, 3),
        b(0, 1, 0, 1),
        b(1, 0, 0, 1),
        b(1, 0, 1, 0), // ╬ ╭ ╮ ╯
        b(0, 1, 1, 0),
        b(0, 0, 0, 0),
        b(0, 0, 0, 0),
        b(0, 0, 0, 0), // ╰ ╱ ╲ ╳
        b(1, 0, 0, 0),
        b(0, 0, 1, 0),
        b(0, 1, 0, 0),
        b(0, 0, 0, 1), // ╴ ╵ ╶ ╷
        b(2, 0, 0, 0),
        b(0, 0, 2, 0),
        b(0, 2, 0, 0),
        b(0, 0, 0, 2), // ╸ ╹ ╺ ╻
        b(1, 2, 0, 0),
        b(0, 0, 1, 2),
        b(2, 1, 0, 0),
        b(0, 0, 2, 1), // ╼ ╽ ╾ ╿
    ]
};

struct Canvas {
    out: Vec<u8>,
    w: usize,
    h: usize,
}

impl Canvas {
    fn fill(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, value: u8) {
        for y in y0.min(self.h)..y1.min(self.h) {
            for x in x0.min(self.w)..x1.min(self.w) {
                let pixel = &mut self.out[y * self.w + x];
                *pixel = (*pixel).max(value);
            }
        }
    }
}

fn draw_builtin(code: u32, w: usize, h: usize, stroke: usize) -> Option<Vec<u8>> {
    let mut canvas = Canvas { out: vec![0u8; w * h], w, h };
    let light = stroke.max(1);
    let heavy = light * 2 + 1;
    // Pixel spans (lo, hi) of the line(s) for an arm of `weight` centered
    // on `center`.
    let span = |weight: u8, center: usize| -> Vec<(usize, usize)> {
        match weight {
            1 => vec![(center - light / 2, center - light / 2 + light)],
            2 => vec![(center.saturating_sub(heavy / 2), center.saturating_sub(heavy / 2) + heavy)],
            3 => {
                let gap = light + 1;
                vec![
                    (center.saturating_sub(gap + light / 2), center.saturating_sub(gap + light / 2) + light),
                    (center + gap - light / 2, center + gap - light / 2 + light),
                ]
            }
            _ => vec![],
        }
    };
    match code {
        0x2571..=0x2573 => return None, // diagonals: the font's glyphs are fine
        0x256d..=0x2570 => {
            // Rounded corner: a quarter circle joining the two arms.
            let (sx, sy): (f32, f32) = match code {
                0x256d => (1.0, 1.0),   // ╭ arms right and down
                0x256e => (-1.0, 1.0),  // ╮ left and down
                0x256f => (-1.0, -1.0), // ╯ left and up
                _ => (1.0, -1.0),       // ╰ right and up
            };
            let (v0, v1) = span(1, w / 2)[0];
            let (h0, h1) = span(1, h / 2)[0];
            let line_x = (v0 + v1) as f32 / 2.0;
            let line_y = (h0 + h1) as f32 / 2.0;
            let radius = (w.min(h) / 2) as f32;
            let (ccx, ccy) = (line_x + sx * radius, line_y + sy * radius);
            let half = light as f32 / 2.0;
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    if (px - ccx) * sx <= 0.0 && (py - ccy) * sy <= 0.0 {
                        let distance = ((px - ccx).powi(2) + (py - ccy).powi(2)).sqrt();
                        let edge = (half + 0.5 - (distance - radius).abs()).clamp(0.0, 1.0);
                        canvas.fill(x, y, x + 1, y + 1, (edge * 255.0) as u8);
                    }
                }
            }
            if sy > 0.0 {
                canvas.fill(v0, ccy as usize, v1, h, 255);
            } else {
                canvas.fill(v0, 0, v1, ccy.ceil() as usize, 255);
            }
            if sx > 0.0 {
                canvas.fill(ccx as usize, h0, w, h1, 255);
            } else {
                canvas.fill(0, h0, ccx.ceil() as usize, h1, 255);
            }
        }
        0x2500..=0x257f => {
            let [l, r, u, d] = BOX[(code - 0x2500) as usize];
            let (cx, cy) = (w / 2, h / 2);
            let gap = (light + 1) as i32;
            // The lines of an arm: (offset from the center, thickness).
            let lines = |weight: u8| -> Vec<(i32, usize)> {
                match weight {
                    1 => vec![(0, light)],
                    2 => vec![(0, heavy)],
                    3 => vec![(-gap, light), (gap, light)],
                    _ => vec![],
                }
            };
            let line_span = |center: usize, offset: i32, thickness: usize| -> (usize, usize) {
                let lo = (center as i32 + offset - (thickness / 2) as i32).max(0) as usize;
                (lo, lo + thickness)
            };
            // The crossing arms' lines sit at ±offset with this thickness.
            let crossing = |a: u8, b: u8| -> (i32, usize) {
                (if a.max(b) == 3 { gap } else { 0 }, if a.max(b) == 2 { heavy } else { light })
            };
            // A line meets the crossing lines either at the near one (when
            // an arm continues on its side, as the inner line of a corner)
            // or runs on to the far one (the outer line), so double lines
            // nest at corners and tees instead of crossing.
            let near = |offset: i32, before: u8, after: u8| -> bool {
                match offset {
                    o if o < 0 => before > 0,
                    o if o > 0 => after > 0,
                    _ => before > 0 && after > 0,
                }
            };
            let (vertical_offset, vertical_thickness) = crossing(u, d);
            let (horizontal_offset, horizontal_thickness) = crossing(l, r);
            for (offset, thickness) in lines(l) {
                let (y0, y1) = line_span(cy, offset, thickness);
                let end = if u | d == 0 {
                    cx + 1
                } else {
                    let to = if near(offset, u, d) { -vertical_offset } else { vertical_offset };
                    line_span(cx, to, vertical_thickness).1
                };
                canvas.fill(0, y0, end, y1, 255);
            }
            for (offset, thickness) in lines(r) {
                let (y0, y1) = line_span(cy, offset, thickness);
                let start = if u | d == 0 {
                    cx
                } else {
                    let from = if near(offset, u, d) { vertical_offset } else { -vertical_offset };
                    line_span(cx, from, vertical_thickness).0
                };
                canvas.fill(start, y0, w, y1, 255);
            }
            for (offset, thickness) in lines(u) {
                let (x0, x1) = line_span(cx, offset, thickness);
                let end = if l | r == 0 {
                    cy + 1
                } else {
                    let to = if near(offset, l, r) { -horizontal_offset } else { horizontal_offset };
                    line_span(cy, to, horizontal_thickness).1
                };
                canvas.fill(x0, 0, x1, end, 255);
            }
            for (offset, thickness) in lines(d) {
                let (x0, x1) = line_span(cx, offset, thickness);
                let start = if l | r == 0 {
                    cy
                } else {
                    let from = if near(offset, l, r) { horizontal_offset } else { -horizontal_offset };
                    line_span(cy, from, horizontal_thickness).0
                };
                canvas.fill(x0, start, x1, h, 255);
            }
        }
        0x2580 => canvas.fill(0, 0, w, h / 2, 255),
        0x2581..=0x2588 => {
            let eighths = (code - 0x2580) as usize;
            canvas.fill(0, h - (h * eighths + 4) / 8, w, h, 255);
        }
        0x2589..=0x258f => {
            let eighths = (0x2590 - code) as usize;
            canvas.fill(0, 0, (w * eighths + 4) / 8, h, 255);
        }
        0x2590 => canvas.fill(w / 2, 0, w, h, 255),
        0x2591 => canvas.fill(0, 0, w, h, 64),
        0x2592 => canvas.fill(0, 0, w, h, 128),
        0x2593 => canvas.fill(0, 0, w, h, 192),
        0x2594 => canvas.fill(0, 0, w, (h + 4) / 8, 255),
        0x2595 => canvas.fill(w - (w + 4) / 8, 0, w, h, 255),
        0x2596..=0x259f => {
            // Quadrant bits: upper-left, upper-right, lower-left, lower-right.
            let quadrants: u8 = match code {
                0x2596 => 0b0100,
                0x2597 => 0b1000,
                0x2598 => 0b0001,
                0x2599 => 0b1101,
                0x259a => 0b1001,
                0x259b => 0b0111,
                0x259c => 0b1011,
                0x259d => 0b0010,
                0x259e => 0b0110,
                _ => 0b1110,
            };
            let (mx, my) = (w / 2, h / 2);
            for (bit, (x0, y0, x1, y1)) in
                [(0, 0, mx, my), (mx, 0, w, my), (0, my, mx, h), (mx, my, w, h)].into_iter().enumerate()
            {
                if quadrants & (1 << bit) != 0 {
                    canvas.fill(x0, y0, x1, y1, 255);
                }
            }
        }
        _ => return None,
    }
    Some(canvas.out)
}

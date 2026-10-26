//! A frame's draw list: instanced rectangles and glyph quads, in layers
//! (each layer draws its rectangles, then its glyphs, over the one below).
//! Callers draw in logical pixels relative to an origin (each pane draws
//! as if it had the window to itself); instances come out in physical
//! pixels, `scale` per logical one.

use crate::theme::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RectInstance {
    pub bounds: [f32; 4],
    pub color: [f32; 4],
    pub color2: [f32; 4],
    pub border_color: [f32; 4],
    pub radii: [f32; 4],
    pub params: [f32; 4],
    pub clip: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GlyphInstance {
    pub rect: [f32; 4],
    pub uv: [f32; 4],
    pub color: [f32; 4],
    pub clip: [f32; 4],
}

#[derive(Default)]
pub struct Layer {
    pub rects: Vec<RectInstance>,
    pub glyphs: Vec<GlyphInstance>,
}

pub const LAYER_CONTENT: usize = 0;
pub const LAYER_OVERLAY: usize = 1;

pub struct Scene {
    pub layers: [Layer; 2],
    pub clear: Color,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    origin: (f32, f32),
    layer: usize,
    /// In logical pixels, window-relative.
    clips: Vec<[f32; 4]>,
    /// Where the focused text cursor is (logical pixels, window-relative),
    /// for an input method to put its candidates beside.
    pub caret: Option<Rect>,
}

/// A rounded rectangle, optionally bordered or with a vertical gradient.
#[derive(Clone, Copy)]
pub struct Quad {
    pub rect: Rect,
    pub color: Color,
    pub color2: Option<Color>,
    pub border: f32,
    pub border_color: Color,
    pub radius: [f32; 4],
}

impl Quad {
    pub fn new(rect: Rect, color: Color) -> Quad {
        Quad { rect, color, color2: None, border: 0.0, border_color: Color::TRANSPARENT, radius: [0.0; 4] }
    }
    pub fn radius(mut self, r: f32) -> Quad {
        self.radius = [r; 4];
        self
    }
    pub fn border(mut self, width: f32, color: Color) -> Quad {
        self.border = width;
        self.border_color = color;
        self
    }
}

impl Scene {
    pub fn new() -> Scene {
        Scene {
            layers: [Layer::default(), Layer::default()],
            clear: Color::TRANSPARENT,
            width: 0.0,
            height: 0.0,
            scale: 1.0,
            origin: (0.0, 0.0),
            caret: None,
            layer: 0,
            clips: Vec::new(),
        }
    }

    pub fn reset(&mut self, width: f32, height: f32, scale: f32, clear: Color) {
        self.scale = scale;
        self.origin = (0.0, 0.0);
        for layer in &mut self.layers {
            layer.rects.clear();
            layer.glyphs.clear();
        }
        self.width = width;
        self.height = height;
        self.clear = clear;
        self.layer = LAYER_CONTENT;
        self.clips.clear();
        self.caret = None;
    }

    pub fn mark_caret(&mut self, rect: Rect) {
        self.caret = Some(self.place(rect));
    }

    /// The frame's size in physical pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (((self.width * self.scale).round() as u32).max(1), ((self.height * self.scale).round() as u32).max(1))
    }

    pub fn set_layer(&mut self, layer: usize) {
        self.layer = layer;
    }

    /// Where (0, 0) is for what's drawn next, in window coordinates.
    pub fn set_origin(&mut self, x: f32, y: f32) {
        self.origin = (x, y);
    }

    fn clip(&self) -> [f32; 4] {
        *self.clips.last().unwrap_or(&[0.0, 0.0, self.width, self.height])
    }

    /// The current clip in physical pixels, for the shaders.
    fn pixel_clip(&self) -> [f32; 4] {
        self.clip().map(|v| v * self.scale)
    }

    pub fn push_clip(&mut self, rect: Rect) {
        let rect = self.place(rect);
        let [x0, y0, x1, y1] = self.clip();
        self.clips.push([x0.max(rect.x), y0.max(rect.y), x1.min(rect.right()), y1.min(rect.bottom())]);
    }

    /// A local rectangle in window coordinates.
    fn place(&self, rect: Rect) -> Rect {
        Rect::new(rect.x + self.origin.0, rect.y + self.origin.1, rect.w, rect.h)
    }

    fn pixels(&self, rect: Rect) -> [f32; 4] {
        let s = self.scale;
        [rect.x * s, rect.y * s, rect.w * s, rect.h * s]
    }

    pub fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn visible(&self, rect: &Rect) -> bool {
        let [x0, y0, x1, y1] = self.clip();
        rect.w > 0.0 && rect.h > 0.0 && rect.right() > x0 && rect.x < x1 && rect.bottom() > y0 && rect.y < y1
    }

    pub fn fill(&mut self, rect: Rect, color: Color) {
        self.quad(Quad::new(rect, color));
    }

    pub fn quad(&mut self, quad: Quad) {
        let rect = self.place(quad.rect);
        if !self.visible(&rect) || (quad.color.a <= 0.0 && quad.border_color.a <= 0.0) {
            return;
        }
        let clip = self.pixel_clip();
        let s = self.scale;
        self.layers[self.layer].rects.push(RectInstance {
            bounds: self.pixels(rect),
            color: quad.color.to_array(),
            color2: quad.color2.unwrap_or(quad.color).to_array(),
            border_color: quad.border_color.to_array(),
            radii: quad.radius.map(|r| r * s),
            params: [quad.border * s, 0.0, 0.0, 0.0],
            clip,
        });
    }

    /// A soft drop shadow under a rounded rectangle.
    pub fn shadow(&mut self, rect: Rect, color: Color, radius: f32, sigma: f32) {
        let rect = self.place(rect);
        let clip = self.pixel_clip();
        let s = self.scale;
        self.layers[self.layer].rects.push(RectInstance {
            bounds: self.pixels(rect),
            color: color.to_array(),
            color2: color.to_array(),
            border_color: [0.0; 4],
            radii: [radius * s; 4],
            params: [0.0, (sigma * s).max(0.5), 0.0, 0.0],
            clip,
        });
    }

    /// A glyph bitmap; its top-left snaps to a physical pixel so the atlas
    /// texels land 1:1.
    pub fn glyph(&mut self, rect: Rect, uv: [f32; 4], color: Color) {
        let rect = self.place(rect);
        if !self.visible(&rect) {
            return;
        }
        let clip = self.pixel_clip();
        let s = self.scale;
        self.layers[self.layer].glyphs.push(GlyphInstance {
            rect: [(rect.x * s).round(), (rect.y * s).round(), (rect.w * s).round(), (rect.h * s).round()],
            uv,
            color: color.to_array(),
            clip,
        });
    }
}

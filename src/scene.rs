//! A frame's draw list: instanced rectangles and glyph quads, in layers
//! (each layer draws its rectangles, then its glyphs, over the one below).

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
    layer: usize,
    clips: Vec<[f32; 4]>,
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
            layer: 0,
            clips: Vec::new(),
        }
    }

    pub fn reset(&mut self, width: f32, height: f32, clear: Color) {
        for layer in &mut self.layers {
            layer.rects.clear();
            layer.glyphs.clear();
        }
        self.width = width;
        self.height = height;
        self.clear = clear;
        self.layer = LAYER_CONTENT;
        self.clips.clear();
    }

    pub fn set_layer(&mut self, layer: usize) {
        self.layer = layer;
    }

    fn clip(&self) -> [f32; 4] {
        *self.clips.last().unwrap_or(&[0.0, 0.0, self.width, self.height])
    }

    pub fn push_clip(&mut self, rect: Rect) {
        let [x0, y0, x1, y1] = self.clip();
        self.clips.push([x0.max(rect.x), y0.max(rect.y), x1.min(rect.right()), y1.min(rect.bottom())]);
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
        if !self.visible(&quad.rect) || (quad.color.a <= 0.0 && quad.border_color.a <= 0.0) {
            return;
        }
        let clip = self.clip();
        self.layers[self.layer].rects.push(RectInstance {
            bounds: [quad.rect.x, quad.rect.y, quad.rect.w, quad.rect.h],
            color: quad.color.to_array(),
            color2: quad.color2.unwrap_or(quad.color).to_array(),
            border_color: quad.border_color.to_array(),
            radii: quad.radius,
            params: [quad.border, 0.0, 0.0, 0.0],
            clip,
        });
    }

    /// A soft drop shadow under a rounded rectangle.
    pub fn shadow(&mut self, rect: Rect, color: Color, radius: f32, sigma: f32) {
        let clip = self.clip();
        self.layers[self.layer].rects.push(RectInstance {
            bounds: [rect.x, rect.y, rect.w, rect.h],
            color: color.to_array(),
            color2: color.to_array(),
            border_color: [0.0; 4],
            radii: [radius; 4],
            params: [0.0, sigma.max(0.5), 0.0, 0.0],
            clip,
        });
    }

    pub fn glyph(&mut self, rect: Rect, uv: [f32; 4], color: Color) {
        if !self.visible(&rect) {
            return;
        }
        let clip = self.clip();
        self.layers[self.layer].glyphs.push(GlyphInstance {
            rect: [rect.x, rect.y, rect.w, rect.h],
            uv,
            color: color.to_array(),
            clip,
        });
    }
}

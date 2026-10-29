//! The Metal renderer, for macOS: the same interface as the Vulkan one in
//! gpu.rs, drawn through src/macos/shim.m with the shaders in
//! shaders/shaders.metal. Once the window has its layer, frames go straight
//! there instead of coming back as pixels.

use std::ffi::{c_char, c_void, CStr, CString};
use std::mem::size_of;

use crate::scene::{GlyphInstance, RectInstance, Scene};
use crate::text::{Atlas, ATLAS_SIZE, COLOR_ATLAS_SIZE};

pub type Result<T> = std::result::Result<T, String>;

extern "C" {
    fn temino_metal_new(
        shaders: *const c_char,
        atlas_size: u32,
        color_atlas_size: u32,
        name: *mut c_char,
        name_length: usize,
        error: *mut c_char,
        error_length: usize,
    ) -> *mut c_void;
    fn temino_metal_free(gpu: *mut c_void);
    fn temino_metal_render(
        gpu: *mut c_void,
        width: u32,
        height: u32,
        clear: *const f32,
        atlas: *const u8,
        atlas_size: u32,
        dirty_lo: i64,
        dirty_hi: i64,
        color_pixels: *const u8,
        color_size: u32,
        color_lo: i64,
        color_hi: i64,
        rects: *const c_void,
        rect_size: usize,
        rect_count: usize,
        glyphs: *const c_void,
        glyph_size: usize,
        glyph_count: usize,
        ranges: *const u32,
        layer_count: usize,
        readback: bool,
        pixels: *mut *const u8,
        error: *mut c_char,
        error_length: usize,
    ) -> bool;
    fn temino_metal_on_screen() -> bool;
}

fn message(buffer: &[u8]) -> String {
    CStr::from_bytes_until_nul(buffer).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub struct Gpu {
    handle: *mut c_void,
    rects: Vec<RectInstance>,
    glyphs: Vec<GlyphInstance>,
    pub device_name: String,
}

impl Gpu {
    pub fn new() -> Result<Gpu> {
        crate::trace("gpu: creating the Metal device");
        let shaders = CString::new(include_str!("../shaders/shaders.metal")).unwrap();
        let mut name = [0u8; 256];
        let mut error = [0u8; 1024];
        let handle = unsafe {
            temino_metal_new(
                shaders.as_ptr(),
                ATLAS_SIZE as u32,
                COLOR_ATLAS_SIZE as u32,
                name.as_mut_ptr().cast(),
                name.len(),
                error.as_mut_ptr().cast(),
                error.len(),
            )
        };
        if handle.is_null() {
            return Err(format!("Metal: {}", message(&error)));
        }
        let device_name = message(&name);
        crate::trace(&format!("gpu: Metal on {device_name}"));
        Ok(Gpu { handle, rects: Vec::new(), glyphs: Vec::new(), device_name })
    }

    /// Whether `render` shows frames itself, in the window's layer.
    pub fn on_screen(&self) -> bool {
        unsafe { temino_metal_on_screen() }
    }

    /// Renders `scene` into the window. The frame comes back as tightly
    /// packed BGRA rows when `readback` asks, or when there is no window to
    /// show it in yet.
    pub fn render(
        &mut self,
        scene: &Scene,
        atlas: &mut Atlas,
        color: &mut Atlas,
        readback: bool,
    ) -> Result<Option<&[u8]>> {
        let (width, height) = scene.pixel_size();
        self.rects.clear();
        self.glyphs.clear();
        let mut ranges = Vec::new();
        for layer in &scene.layers {
            ranges
                .extend([self.rects.len(), layer.rects.len(), self.glyphs.len(), layer.glyphs.len()].map(|n| n as u32));
            self.rects.extend_from_slice(&layer.rects);
            self.glyphs.extend_from_slice(&layer.glyphs);
        }
        let (lo, hi) = atlas.dirty.take().map_or((-1, -1), |(lo, hi)| (lo as i64, hi as i64));
        let (color_lo, color_hi) = color.dirty.take().map_or((-1, -1), |(lo, hi)| (lo as i64, hi as i64));
        // The pipelines blend premultiplied colors; clear with one too.
        let [r, g, b, a] = scene.clear.to_array();
        let clear = [r * a, g * a, b * a, a];
        let mut error = [0u8; 1024];
        let mut pixels = std::ptr::null();
        let ok = unsafe {
            temino_metal_render(
                self.handle,
                width,
                height,
                clear.as_ptr(),
                atlas.pixels.as_ptr(),
                ATLAS_SIZE as u32,
                lo,
                hi,
                color.pixels.as_ptr(),
                COLOR_ATLAS_SIZE as u32,
                color_lo,
                color_hi,
                self.rects.as_ptr().cast(),
                size_of::<RectInstance>(),
                self.rects.len(),
                self.glyphs.as_ptr().cast(),
                size_of::<GlyphInstance>(),
                self.glyphs.len(),
                ranges.as_ptr(),
                scene.layers.len(),
                readback,
                &mut pixels,
                error.as_mut_ptr().cast(),
                error.len(),
            )
        };
        if !ok {
            return Err(message(&error));
        }
        Ok((!pixels.is_null())
            .then(|| unsafe { std::slice::from_raw_parts(pixels, width as usize * height as usize * 4) }))
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe { temino_metal_free(self.handle) };
    }
}

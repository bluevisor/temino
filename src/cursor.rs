//! Settings ▸ Cursor: the caret's shape at the prompt, the shape programs
//! start with until they pick their own, and whether it blinks. Kept here,
//! read from the settings file once, so terminals made on any thread see
//! the same.

use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// As it always was: a bar at the prompt, a block in programs.
    Auto,
    Block,
    Bar,
    Underline,
}

impl Shape {
    /// In Settings' order, with the words in the settings file.
    pub const ALL: [Shape; 4] = [Shape::Auto, Shape::Bar, Shape::Block, Shape::Underline];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Auto => "Auto",
            Shape::Block => "Block",
            Shape::Bar => "Bar",
            Shape::Underline => "Underline",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Shape::Auto => "auto",
            Shape::Block => "block",
            Shape::Bar => "bar",
            Shape::Underline => "underline",
        }
    }
}

/// An index into Shape::ALL: Auto until changed.
static SHAPE: AtomicU8 = AtomicU8::new(0);
static BLINK: AtomicBool = AtomicBool::new(true);

/// `cursor=block|bar|underline` and `cursor_blink=off` from the settings file.
pub fn load() {
    let shape = crate::setting("cursor").and_then(|word| Shape::ALL.iter().position(|s| s.word() == word));
    SHAPE.store(shape.unwrap_or(0) as u8, Ordering::Relaxed);
    BLINK.store(crate::setting("cursor_blink").as_deref() != Some("off"), Ordering::Relaxed);
}

/// The caret at the prompt: a bar unless another shape is picked.
pub fn prompt() -> Shape {
    match shape() {
        Shape::Auto => Shape::Bar,
        other => other,
    }
}

pub fn shape() -> Shape {
    Shape::ALL[SHAPE.load(Ordering::Relaxed) as usize % Shape::ALL.len()]
}

pub fn set_shape(shape: Shape) {
    let index = Shape::ALL.iter().position(|&s| s == shape).unwrap_or(0);
    SHAPE.store(index as u8, Ordering::Relaxed);
    crate::save_setting("cursor", shape.word());
}

pub fn blinks() -> bool {
    BLINK.load(Ordering::Relaxed)
}

pub fn set_blinks(on: bool) {
    BLINK.store(on, Ordering::Relaxed);
    crate::save_setting("cursor_blink", if on { "on" } else { "off" });
}

/// What a terminal's cursor looks like until its program asks for another
/// (DECSCUSR); asking for the default (0) comes back to this.
pub fn term_style() -> CursorStyle {
    let shape = match shape() {
        Shape::Auto | Shape::Block => CursorShape::Block,
        Shape::Bar => CursorShape::Beam,
        Shape::Underline => CursorShape::Underline,
    };
    CursorStyle { shape, blinking: blinks() }
}

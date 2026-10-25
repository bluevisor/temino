//! The window system, behind one interface: Haiku's Be API through
//! `src/haiku/shim.cpp`, or winit + softbuffer on Linux and macOS. Both
//! deliver the same events and take finished BGRA frames to show.

use crate::keys::Named;

#[cfg(target_os = "haiku")]
mod haiku;
#[cfg(target_os = "haiku")]
pub use haiku::*;

#[cfg(not(target_os = "haiku"))]
mod desktop;
#[cfg(not(target_os = "haiku"))]
pub use desktop::*;

/// Modifier bits, in Haiku's encoding (`B_SHIFT_KEY` and friends). Command
/// is the shortcut modifier: Alt on Haiku, Cmd on macOS, Ctrl+Shift (or
/// Super) on Linux. Option is the Meta key the shell sees.
pub mod mods {
    pub const SHIFT: u32 = 0x01;
    pub const COMMAND: u32 = 0x02;
    pub const CONTROL: u32 = 0x04;
    pub const OPTION: u32 = 0x40;
}

/// The shortcut modifier's name, for hints on screen.
pub const COMMAND_NAME: &str = if cfg!(target_os = "haiku") {
    "Alt"
} else if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+Shift"
};

/// What the welcome screen says Temino is for.
pub const OS_NAME: &str = if cfg!(target_os = "haiku") {
    "Haiku"
} else if cfg!(target_os = "macos") {
    "macOS"
} else {
    "Linux"
};

#[derive(Debug, Clone)]
pub struct KeyEvent {
    /// The special key it is (arrows, Enter, F-keys…), if any.
    pub named: Option<Named>,
    pub mods: u32,
    /// The unmodified character of the key.
    pub raw_char: u32,
    /// The UTF-8 the keymap produced (control characters included).
    pub bytes: Vec<u8>,
}

impl KeyEvent {
    pub fn shift(&self) -> bool {
        self.mods & mods::SHIFT != 0
    }
    pub fn ctrl(&self) -> bool {
        self.mods & mods::CONTROL != 0
    }
    pub fn cmd(&self) -> bool {
        self.mods & mods::COMMAND != 0
    }
    pub fn option(&self) -> bool {
        self.mods & mods::OPTION != 0
    }
    /// The unmodified character of the key, lowercased.
    pub fn base_char(&self) -> Option<char> {
        char::from_u32(self.raw_char).map(|c| c.to_ascii_lowercase())
    }
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Key(KeyEvent),
    MouseDown {
        x: f32,
        y: f32,
        buttons: i32,
        clicks: i32,
        mods: u32,
    },
    MouseUp {
        x: f32,
        y: f32,
    },
    MouseMoved {
        x: f32,
        y: f32,
        buttons: i32,
    },
    Wheel {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
        mods: u32,
    },
    /// The window's size in logical pixels, and physical pixels per logical one.
    Resize {
        width: u32,
        height: u32,
        scale: f32,
    },
    Focus(bool),
    Close,
    /// Settings ▸ Background picked this entry of `theme::BACKGROUNDS`.
    Background(usize),
}

/// Mouse button bits, as Haiku numbers them.
pub mod buttons {
    #[allow(dead_code)] // only the desktop backend sets it by name
    pub const PRIMARY: i32 = 1;
    pub const SECONDARY: i32 = 2;
    pub const TERTIARY: i32 = 4;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    IBeam = 0,
    Arrow = 1,
    ResizeColumn = 2,
    ResizeRow = 3,
}

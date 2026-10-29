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

/// The OS's name: told to the model, and the host name when there's none.
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
    /// Mac editing keys: Option+←/→ moves by word. On Haiku in UTM a Mac
    /// keyboard's Option arrives as the Command (Alt) key and ⌘ as Option,
    /// so the two swap there. Both together stay pane navigation.
    pub fn word_mod(&self) -> bool {
        if cfg!(target_os = "haiku") {
            self.cmd() && !self.option()
        } else if cfg!(target_os = "macos") {
            self.option() && !self.cmd()
        } else {
            false
        }
    }
    /// Mac editing keys: ⌘+←/→ jumps to the start or end of the line.
    pub fn line_mod(&self) -> bool {
        if cfg!(target_os = "haiku") {
            self.option() && !self.cmd()
        } else if cfg!(target_os = "macos") {
            self.cmd() && !self.option()
        } else {
            false
        }
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
    /// In wheel notches (3 lines each), positive down; `precise` for a
    /// trackpad's continuous, fractional deltas.
    Wheel {
        dx: f32,
        dy: f32,
        x: f32,
        y: f32,
        mods: u32,
        precise: bool,
    },
    /// The window's size in logical pixels, and physical pixels per logical one.
    Resize {
        width: u32,
        height: u32,
        scale: f32,
    },
    Focus(bool),
    /// The modifier keys held now, when that changes (desktop only).
    #[allow(dead_code)]
    Modifiers(u32),
    Close,
    /// Another window is quitting the app: finish and close this one too
    /// (desktop only).
    #[allow(dead_code)]
    Quit,
    /// This window now keeps the tabs for the next launch: the one that did
    /// closed (desktop only).
    #[allow(dead_code)]
    Primary,
    /// Settings ▸ Background picked this entry of `theme::BACKGROUNDS`
    /// (Haiku's menu; elsewhere the Settings tab sets it).
    #[allow(dead_code)]
    Background(usize),
    /// An input method committed this text.
    #[allow(dead_code)]
    Text(String),
    /// An input method is composing this (empty: nothing).
    #[allow(dead_code)]
    Preedit(String),
    /// Settings ▸ Shell picked this entry of `shells::KINDS` (Haiku's menu).
    #[allow(dead_code)]
    Shell(usize),
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
    Link = 4,
}

/// Whether links open in built-in viewers (native views laid over the
/// pane's place); elsewhere they go to the system's app.
pub const VIEWERS: bool = cfg!(target_os = "macos");

/// What a viewer shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewerKind {
    /// A text file to edit; RTF stays rich.
    Text = 0,
    /// A web page, or a local page, PDF or SVG.
    Web = 1,
    Image = 2,
}

/// The theme a viewer is drawn in: sRGB colors, the text size in points.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ViewerStyle {
    pub background: [f32; 3],
    pub text: [f32; 3],
    pub accent: [f32; 3],
    pub selection: [f32; 3],
    /// Its header bar, the line under it, and its secondary text.
    pub header: [f32; 3],
    pub divider: [f32; 3],
    pub dim: [f32; 3],
    pub faint: [f32; 3],
    pub font_size: f32,
    pub dark: bool,
}

/// What a viewer tells the app.
#[derive(Clone, Debug)]
pub enum ViewerEvent {
    /// Clicked into: it has the keyboard now.
    Focused,
    Title(String),
    /// A page went to this address.
    Url(String),
    /// Whether it has unsaved changes.
    Dirty(bool),
    Failed(String),
    /// Its close button was clicked.
    Close,
}

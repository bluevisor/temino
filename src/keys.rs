//! Haiku key events → bytes for the program in the terminal (xterm
//! encoding). Special keys are identified by Haiku's physical key codes,
//! since their byte values collide with control characters (B_HOME is ^A).

use alacritty_terminal::term::TermMode;

use crate::platform::KeyEvent;

/// Physical key codes from Haiku's keyboard layout.
pub mod code {
    pub const ESCAPE: i32 = 0x01;
    pub const F1: i32 = 0x02;
    pub const F12: i32 = 0x0d;
    pub const BACKSPACE: i32 = 0x1e;
    pub const INSERT: i32 = 0x1f;
    pub const HOME: i32 = 0x20;
    pub const PAGE_UP: i32 = 0x21;
    pub const TAB: i32 = 0x26;
    pub const DELETE: i32 = 0x34;
    pub const END: i32 = 0x35;
    pub const PAGE_DOWN: i32 = 0x36;
    pub const ENTER: i32 = 0x47;
    pub const UP: i32 = 0x57;
    pub const KEYPAD_ENTER: i32 = 0x5b;
    pub const LEFT: i32 = 0x61;
    pub const DOWN: i32 = 0x62;
    pub const RIGHT: i32 = 0x63;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Named {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Backspace,
    Tab,
    Enter,
    Escape,
    F(u8),
}

/// The special key an event stands for, if any.
pub fn named(event: &KeyEvent) -> Option<Named> {
    let by_code = match event.key {
        code::UP => Some(Named::Up),
        code::DOWN => Some(Named::Down),
        code::LEFT => Some(Named::Left),
        code::RIGHT => Some(Named::Right),
        code::HOME => Some(Named::Home),
        code::END => Some(Named::End),
        code::PAGE_UP => Some(Named::PageUp),
        code::PAGE_DOWN => Some(Named::PageDown),
        code::INSERT => Some(Named::Insert),
        code::DELETE => Some(Named::Delete),
        code::BACKSPACE => Some(Named::Backspace),
        code::TAB => Some(Named::Tab),
        code::ENTER | code::KEYPAD_ENTER => Some(Named::Enter),
        code::ESCAPE => Some(Named::Escape),
        k @ code::F1..=code::F12 => Some(Named::F((k - code::F1 + 1) as u8)),
        _ => None,
    };
    if by_code.is_some() {
        return by_code;
    }
    // The keypad with Num Lock off sends navigation bytes.
    let keypad = matches!(event.key, 0x37..=0x39 | 0x48..=0x4a | 0x58..=0x5a | 0x64 | 0x65);
    if keypad && event.bytes.len() == 1 {
        return match event.bytes[0] {
            0x1e => Some(Named::Up),
            0x1f => Some(Named::Down),
            0x1c => Some(Named::Left),
            0x1d => Some(Named::Right),
            0x01 => Some(Named::Home),
            0x04 => Some(Named::End),
            0x0b => Some(Named::PageUp),
            0x0c => Some(Named::PageDown),
            0x05 => Some(Named::Insert),
            0x7f => Some(Named::Delete),
            _ => None,
        };
    }
    None
}

/// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn modifier_param(event: &KeyEvent) -> u8 {
    1 + event.shift() as u8 + 2 * (event.cmd() || event.option()) as u8 + 4 * event.ctrl() as u8
}

pub fn encode(event: &KeyEvent, mode: TermMode) -> Vec<u8> {
    let m = modifier_param(event);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let meta = event.cmd() || event.option();
    let csi_letter = |letter: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{}", letter as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let csi_tilde = |number: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{number};{m}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };
    if let Some(key) = named(event) {
        return match key {
            Named::Up => csi_letter(b'A'),
            Named::Down => csi_letter(b'B'),
            Named::Right => csi_letter(b'C'),
            Named::Left => csi_letter(b'D'),
            Named::Home => csi_letter(b'H'),
            Named::End => csi_letter(b'F'),
            Named::Insert => csi_tilde(2),
            Named::Delete => csi_tilde(3),
            Named::PageUp => csi_tilde(5),
            Named::PageDown => csi_tilde(6),
            Named::Backspace => match (event.ctrl(), meta) {
                (true, _) => vec![0x08],
                (false, true) => vec![0x1b, 0x7f],
                _ => vec![0x7f],
            },
            Named::Tab => {
                if event.shift() {
                    b"\x1b[Z".to_vec()
                } else {
                    vec![b'\t']
                }
            }
            Named::Enter => {
                if meta {
                    vec![0x1b, b'\r']
                } else {
                    vec![b'\r']
                }
            }
            Named::Escape => vec![0x1b],
            Named::F(n @ 1..=4) => {
                let letter = b"PQRS"[(n - 1) as usize];
                if m > 1 {
                    format!("\x1b[1;{m}{}", letter as char).into_bytes()
                } else {
                    vec![0x1b, b'O', letter]
                }
            }
            Named::F(n) => csi_tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5).min(7) as usize]),
        };
    }

    let mut bytes = event.bytes.clone();
    if event.ctrl() && bytes.len() == 1 {
        // Make sure Ctrl+letter is a control character even if the keymap
        // didn't map it.
        let b = bytes[0];
        bytes[0] = match b {
            b'a'..=b'z' => b - b'a' + 1,
            b'A'..=b'Z' => b - b'A' + 1,
            b' ' | b'@' | b'2' | 0 => 0,
            b'[' | b'3' => 0x1b,
            b'\\' | b'4' => 0x1c,
            b']' | b'5' => 0x1d,
            b'^' | b'6' => 0x1e,
            b'_' | b'-' | b'7' => 0x1f,
            b'?' | b'8' => 0x7f,
            other => other,
        };
    }
    if meta && !bytes.is_empty() {
        // Alt as Meta: ESC prefix, using the unshifted key when Haiku's
        // keymap turned the Command combination into something else.
        if let Some(base) = event.base_char().filter(|c| c.is_ascii_graphic()) {
            if !event.shift() && bytes.len() == 1 && !bytes[0].is_ascii_graphic() {
                bytes = vec![base as u8];
            }
        }
        bytes.insert(0, 0x1b);
    }
    bytes
}

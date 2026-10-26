//! Key events → bytes for the program in the terminal (xterm encoding).
//! The platform layer names the special keys; this turns them into escapes.

use alacritty_terminal::term::TermMode;

use crate::platform::KeyEvent;

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
    event.named
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
        // Alt as Meta: ESC prefix, using the unshifted key when the
        // keymap turned the combination into something else.
        if let Some(base) = event.base_char().filter(|c| c.is_ascii_graphic()) {
            if !event.shift() && bytes.len() == 1 && !bytes[0].is_ascii_graphic() {
                bytes = vec![base as u8];
            }
        }
        bytes.insert(0, 0x1b);
    }
    bytes
}

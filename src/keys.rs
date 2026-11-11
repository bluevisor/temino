//! Key events → bytes for the program in the terminal: xterm's encoding,
//! or the Kitty keyboard protocol's when the program asked for it (CSI > u;
//! alacritty_terminal keeps its flags in the mode). The platform layer
//! names the special keys; this turns them into escapes.

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
    if mode.intersects(TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_ALL_KEYS_AS_ESC) {
        if let Some(bytes) = kitty(event, mode) {
            return bytes;
        }
    }
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
    if meta && !event.shift() && bytes.len() == 1 && !bytes[0].is_ascii_graphic() {
        // Alt as Meta: the unshifted key when the keymap turned the
        // combination into something else (Ctrl is applied to it below).
        if let Some(base) = event.base_char().filter(|c| c.is_ascii_graphic()) {
            bytes = vec![base as u8];
        }
    }
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
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// The Kitty keyboard protocol's encoding of a key press, or None where it
/// keeps the legacy bytes (plain typing, unless every key is to be an
/// escape code). Temino sees presses only, never releases, so the event
/// type (flag 2) is always the default, a press, and goes unsaid.
/// https://sw.kovidgoyal.net/kitty/keyboard-protocol/
fn kitty(event: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    let all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let (shift, alt, ctrl) = (event.shift(), event.cmd() || event.option(), event.ctrl());
    let mods = 1 + shift as u32 + 2 * alt as u32 + 4 * ctrl as u32;
    let csi = |body: String| Some(format!("\x1b[{body}").into_bytes());
    // Arrows, Home/End and F1-F4 keep their letter forms; others are numbered.
    let letter = |l: char| csi(if mods > 1 { format!("1;{mods}{l}") } else { l.to_string() });
    let tilde = |n: u32| csi(if mods > 1 { format!("{n};{mods}~") } else { format!("{n}~") });
    let u = |code: u32| csi(if mods > 1 { format!("{code};{mods}u") } else { format!("{code}u") });
    if let Some(key) = event.named {
        return match key {
            Named::Up => letter('A'),
            Named::Down => letter('B'),
            Named::Right => letter('C'),
            Named::Left => letter('D'),
            Named::Home => letter('H'),
            Named::End => letter('F'),
            Named::F(1) => letter('P'),
            Named::F(2) => letter('Q'),
            Named::F(3) => tilde(13),
            Named::F(4) => letter('S'),
            Named::F(n) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(n.max(5) - 5).min(7) as usize]),
            Named::Insert => tilde(2),
            Named::Delete => tilde(3),
            Named::PageUp => tilde(5),
            Named::PageDown => tilde(6),
            Named::Escape => u(27),
            // Unmodified, these stay as they were, so a shell left in this
            // mode by a crashed program still takes `reset` and Enter.
            Named::Enter if mods == 1 && !all => None,
            Named::Tab if mods == 1 && !all => None,
            Named::Backspace if mods == 1 && !all => None,
            Named::Enter => u(13),
            Named::Tab => u(9),
            Named::Backspace => u(127),
        };
    }
    // A text key: its code is the unshifted character, in lower case.
    let base = char::from_u32(event.raw_char).filter(|c| !c.is_control())?;
    let code = base.to_lowercase().next().unwrap_or(base) as u32;
    let text = std::str::from_utf8(&event.bytes).ok().filter(|t| t.chars().all(|c| !c.is_control()));
    // Typing (alone or with Shift) stays text unless every key is an escape.
    if !all && !alt && !ctrl {
        return None;
    }
    let mut key = code.to_string();
    // The shifted key too, when asked for and Shift made another character.
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) && shift {
        if let Some(shifted) = text.and_then(|t| t.chars().next()).filter(|&c| c as u32 != code) {
            key = format!("{code}:{}", shifted as u32);
        }
    }
    let mut body = if mods > 1 { format!("{key};{mods}") } else { key };
    // The text the key typed, as code points, when every key is an escape.
    if all && mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) && !ctrl && !alt {
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            let points: Vec<String> = text.chars().map(|c| (c as u32).to_string()).collect();
            // No modifiers: their field stays empty, as Kitty writes it.
            if mods == 1 {
                body.push(';');
            }
            body.push(';');
            body.push_str(&points.join(":"));
        }
    }
    csi(format!("{body}u"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::mods;

    fn key(named: Option<Named>, held: u32, raw: char, bytes: &[u8]) -> KeyEvent {
        KeyEvent { named, mods: held, raw_char: raw as u32, bytes: bytes.to_vec() }
    }

    fn special(named: Named, held: u32) -> Vec<u8> {
        encode(&key(Some(named), held, '\0', b""), TermMode::empty())
    }

    fn typed(raw: char, held: u32, bytes: &[u8]) -> Vec<u8> {
        encode(&key(None, held, raw, bytes), TermMode::empty())
    }

    #[test]
    fn plain_characters_pass_through_unchanged() {
        assert_eq!(typed('a', 0, b"a"), b"a");
        assert_eq!(typed('a', mods::SHIFT, b"A"), b"A");
        assert_eq!(typed('\0', 0, "中".as_bytes()), "中".as_bytes());
        assert_eq!(typed('\0', 0, "👍🏽".as_bytes()), "👍🏽".as_bytes());
        assert_eq!(typed('a', 0, b""), b"");
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        assert_eq!(special(Named::Up, 0), b"\x1b[A");
        assert_eq!(special(Named::Down, 0), b"\x1b[B");
        assert_eq!(special(Named::Right, 0), b"\x1b[C");
        assert_eq!(special(Named::Left, 0), b"\x1b[D");
        assert_eq!(special(Named::Home, 0), b"\x1b[H");
        assert_eq!(special(Named::End, 0), b"\x1b[F");
        let app = |named| encode(&key(Some(named), 0, '\0', b""), TermMode::APP_CURSOR);
        assert_eq!(app(Named::Up), b"\x1bOA");
        assert_eq!(app(Named::Left), b"\x1bOD");
        assert_eq!(app(Named::Home), b"\x1bOH");
        assert_eq!(app(Named::End), b"\x1bOF");
    }

    #[test]
    fn modified_arrows_carry_the_xterm_modifier_parameter() {
        assert_eq!(special(Named::Up, mods::SHIFT), b"\x1b[1;2A");
        assert_eq!(special(Named::Left, mods::OPTION), b"\x1b[1;3D");
        assert_eq!(special(Named::Right, mods::CONTROL), b"\x1b[1;5C");
        assert_eq!(special(Named::Down, mods::CONTROL | mods::SHIFT), b"\x1b[1;6B");
        assert_eq!(special(Named::Up, mods::CONTROL | mods::OPTION | mods::SHIFT), b"\x1b[1;8A");
        // The shortcut modifier counts as Alt, the same as Option.
        assert_eq!(special(Named::Left, mods::COMMAND), b"\x1b[1;3D");
        assert_eq!(special(Named::Left, mods::COMMAND | mods::OPTION), b"\x1b[1;3D");
        // A modifier wins over application cursor mode.
        let event = key(Some(Named::Up), mods::CONTROL, '\0', b"");
        assert_eq!(encode(&event, TermMode::APP_CURSOR), b"\x1b[1;5A");
    }

    #[test]
    fn editing_keys_send_tilde_sequences() {
        assert_eq!(special(Named::Insert, 0), b"\x1b[2~");
        assert_eq!(special(Named::Delete, 0), b"\x1b[3~");
        assert_eq!(special(Named::PageUp, 0), b"\x1b[5~");
        assert_eq!(special(Named::PageDown, 0), b"\x1b[6~");
        assert_eq!(special(Named::Delete, mods::CONTROL), b"\x1b[3;5~");
        assert_eq!(special(Named::PageUp, mods::SHIFT), b"\x1b[5;2~");
        // Application cursor mode only changes arrows, Home and End.
        let event = key(Some(Named::Delete), 0, '\0', b"");
        assert_eq!(encode(&event, TermMode::APP_CURSOR), b"\x1b[3~");
    }

    #[test]
    fn function_keys_match_xterm() {
        assert_eq!(special(Named::F(1), 0), b"\x1bOP");
        assert_eq!(special(Named::F(2), 0), b"\x1bOQ");
        assert_eq!(special(Named::F(3), 0), b"\x1bOR");
        assert_eq!(special(Named::F(4), 0), b"\x1bOS");
        assert_eq!(special(Named::F(1), mods::SHIFT), b"\x1b[1;2P");
        assert_eq!(special(Named::F(4), mods::CONTROL), b"\x1b[1;5S");
        let codes = [15, 17, 18, 19, 20, 21, 23, 24];
        for (n, code) in (5..=12).zip(codes) {
            assert_eq!(special(Named::F(n), 0), format!("\x1b[{code}~").as_bytes(), "F{n}");
        }
        assert_eq!(special(Named::F(5), mods::SHIFT), b"\x1b[15;2~");
        assert_eq!(special(Named::F(12), mods::CONTROL), b"\x1b[24;5~");
        let event = key(Some(Named::F(1)), 0, '\0', b"");
        assert_eq!(encode(&event, TermMode::APP_CURSOR), b"\x1bOP");
    }

    #[test]
    fn backspace_enter_tab_and_escape() {
        assert_eq!(special(Named::Backspace, 0), b"\x7f");
        assert_eq!(special(Named::Backspace, mods::CONTROL), b"\x08");
        assert_eq!(special(Named::Backspace, mods::OPTION), b"\x1b\x7f");
        assert_eq!(special(Named::Enter, 0), b"\r");
        assert_eq!(special(Named::Enter, mods::SHIFT), b"\r");
        assert_eq!(special(Named::Enter, mods::OPTION), b"\x1b\r");
        assert_eq!(special(Named::Tab, 0), b"\t");
        assert_eq!(special(Named::Tab, mods::SHIFT), b"\x1b[Z");
        assert_eq!(special(Named::Escape, 0), b"\x1b");
        assert_eq!(special(Named::Escape, mods::SHIFT), b"\x1b");
    }

    #[test]
    fn ctrl_turns_keys_into_control_characters_when_the_keymap_did_not() {
        assert_eq!(typed('c', mods::CONTROL, b"c"), [0x03]);
        assert_eq!(typed('c', mods::CONTROL | mods::SHIFT, b"C"), [0x03]);
        assert_eq!(typed('a', mods::CONTROL, b"a"), [0x01]);
        assert_eq!(typed('z', mods::CONTROL, b"z"), [0x1a]);
        assert_eq!(typed(' ', mods::CONTROL, b" "), [0x00]);
        assert_eq!(typed('2', mods::CONTROL, b"2"), [0x00]);
        assert_eq!(typed('[', mods::CONTROL, b"["), [0x1b]);
        assert_eq!(typed('\\', mods::CONTROL, b"\\"), [0x1c]);
        assert_eq!(typed(']', mods::CONTROL, b"]"), [0x1d]);
        assert_eq!(typed('6', mods::CONTROL, b"^"), [0x1e]);
        assert_eq!(typed('-', mods::CONTROL, b"_"), [0x1f]);
        assert_eq!(typed('8', mods::CONTROL, b"?"), [0x7f]);
        // Already a control character: left alone.
        assert_eq!(typed('c', mods::CONTROL, &[0x03]), [0x03]);
        // Nothing a control character could be: sent as typed.
        assert_eq!(typed('.', mods::CONTROL, b"."), b".");
        // Ctrl doesn't touch multi-byte text.
        assert_eq!(typed('\0', mods::CONTROL, "é".as_bytes()), "é".as_bytes());
    }

    #[test]
    fn alt_prefixes_escape_as_meta() {
        assert_eq!(typed('b', mods::OPTION, b"b"), b"\x1bb");
        assert_eq!(typed('b', mods::OPTION | mods::SHIFT, b"B"), b"\x1bB");
        assert_eq!(typed('.', mods::OPTION, b"."), b"\x1b.");
        assert_eq!(typed('b', mods::COMMAND, b"b"), b"\x1bb");
        // A keymap that turned Alt+x into a control byte: the plain key is sent.
        assert_eq!(typed('x', mods::OPTION, &[0x00]), b"\x1bx");
        // No text, no lone Escape.
        assert_eq!(typed('b', mods::OPTION, b""), b"");
    }

    #[test]
    fn ctrl_alt_letter_keeps_the_control_character() {
        assert_eq!(typed('a', mods::CONTROL | mods::OPTION, b"a"), b"\x1b\x01");
        assert_eq!(typed('a', mods::CONTROL | mods::OPTION, &[0x01]), b"\x1b\x01");
    }

    fn press(named: Option<Named>, raw: char, text: &str, mods: u32) -> KeyEvent {
        KeyEvent { named, mods, raw_char: raw as u32, bytes: text.as_bytes().to_vec() }
    }

    fn sent(event: &KeyEvent, mode: TermMode) -> String {
        String::from_utf8(encode(event, mode)).unwrap().replace('\x1b', "^[")
    }

    #[test]
    fn a_program_turns_it_on_and_off() {
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::{test::TermSize, Config, Term};
        use alacritty_terminal::vte::ansi::Processor;
        let config = Config { kitty_keyboard: true, ..Config::default() };
        let mut term = Term::new(config, &TermSize::new(80, 24), VoidListener);
        let mut parser: Processor = Processor::new();
        let shift_enter = press(Some(Named::Enter), '\r', "\r", mods::SHIFT);
        parser.advance(&mut term, b"\x1b[>1u");
        assert_eq!(sent(&shift_enter, *term.mode()), "^[[13;2u");
        parser.advance(&mut term, b"\x1b[<u");
        assert_eq!(sent(&shift_enter, *term.mode()), "\r");
    }

    #[test]
    fn legacy_until_a_program_asks() {
        let mode = TermMode::empty();
        assert_eq!(sent(&press(Some(Named::Enter), '\r', "\r", mods::SHIFT), mode), "\r");
        assert_eq!(sent(&press(None, 'a', "\x01", mods::CONTROL), mode), "\x01");
        assert_eq!(sent(&press(Some(Named::Escape), '\x1b', "\x1b", 0), mode), "^[");
    }

    #[test]
    fn disambiguate_tells_apart_what_legacy_runs_together() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES;
        // Shift+Enter is not Enter: agents' newline-without-sending.
        assert_eq!(sent(&press(Some(Named::Enter), '\r', "\r", mods::SHIFT), mode), "^[[13;2u");
        assert_eq!(sent(&press(Some(Named::Enter), '\r', "\r", 0), mode), "\r");
        assert_eq!(sent(&press(Some(Named::Escape), '\x1b', "\x1b", 0), mode), "^[[27u");
        assert_eq!(sent(&press(None, 'i', "\t", mods::CONTROL), mode), "^[[105;5u");
        assert_eq!(sent(&press(None, 'x', "x", mods::OPTION), mode), "^[[120;3u");
        assert_eq!(sent(&press(Some(Named::Tab), '\t', "\t", mods::SHIFT), mode), "^[[9;2u");
        // Typing stays typing; arrows keep their forms.
        assert_eq!(sent(&press(None, 'a', "A", mods::SHIFT), mode), "A");
        assert_eq!(sent(&press(Some(Named::Up), '\0', "", mods::CONTROL), mode), "^[[1;5A");
        assert_eq!(sent(&press(Some(Named::F(5)), '\0', "", 0), mode), "^[[15~");
    }

    #[test]
    fn every_key_as_an_escape_with_its_text() {
        let mode = TermMode::DISAMBIGUATE_ESC_CODES
            | TermMode::REPORT_ALL_KEYS_AS_ESC
            | TermMode::REPORT_ALTERNATE_KEYS
            | TermMode::REPORT_ASSOCIATED_TEXT;
        assert_eq!(sent(&press(None, 'a', "a", 0), mode), "^[[97;;97u");
        assert_eq!(sent(&press(None, 'a', "A", mods::SHIFT), mode), "^[[97:65;2;65u");
        assert_eq!(sent(&press(Some(Named::Enter), '\r', "\r", 0), mode), "^[[13u");
        assert_eq!(sent(&press(Some(Named::Backspace), '\x7f', "", 0), mode), "^[[127u");
    }
}

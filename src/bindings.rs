//! Custom key bindings: the `keys` file next to the settings, one binding
//! a line, `cmd+shift+d = split_down`. A binding runs one of Temino's
//! actions, types text (`text:ls\r`), or turns a shortcut off (`ignore`).
//! The file is read again whenever it changes.

use std::time::SystemTime;

use crate::keys::Named;
use crate::platform::{mods, KeyEvent};

/// What a binding does.
#[derive(Clone, Debug, PartialEq)]
pub enum Bind {
    /// One of ACTIONS, by name.
    Action(&'static str),
    /// Typed into the pane, escapes decoded.
    Text(Vec<u8>),
    /// Nothing: the key's default shortcut is off.
    Ignore,
}

/// A key and the modifiers held with it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Combo {
    pub mods: u32,
    pub named: Option<Named>,
    pub c: char,
}

impl Combo {
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let held = event.mods & (mods::SHIFT | mods::COMMAND | mods::CONTROL | mods::OPTION);
        if held != self.mods {
            return false;
        }
        match self.named {
            Some(named) => event.named == Some(named),
            None => event.named.is_none() && event.base_char() == Some(self.c),
        }
    }

    /// The key event the combination sends: how actions reuse the default
    /// shortcuts.
    pub fn event(&self) -> KeyEvent {
        let bytes = if self.named.is_some() { Vec::new() } else { self.c.to_string().into_bytes() };
        KeyEvent { named: self.named, mods: self.mods, raw_char: self.c as u32, bytes }
    }
}

/// ⌘1…9 picks a tab. Linux desktops usually keep Super+1…9 for their dock,
/// so there the shortcut adds Alt: Super+Alt+1 (Command+Option on a Mac
/// keyboard) or Ctrl+Shift+Alt+1. Plain ⌘1 still works where it gets through.
const TAB_KEYS: [&str; 9] = if cfg!(target_os = "linux") {
    [
        "cmd+alt+1",
        "cmd+alt+2",
        "cmd+alt+3",
        "cmd+alt+4",
        "cmd+alt+5",
        "cmd+alt+6",
        "cmd+alt+7",
        "cmd+alt+8",
        "cmd+alt+9",
    ]
} else {
    ["cmd+1", "cmd+2", "cmd+3", "cmd+4", "cmd+5", "cmd+6", "cmd+7", "cmd+8", "cmd+9"]
};

/// Every action a key can be bound to, and the default shortcut that does
/// it ("cmd" is the shortcut modifier on every platform).
pub const ACTIONS: &[(&str, &str)] = &[
    ("new_window", "cmd+n"),
    ("new_tab", "cmd+t"),
    ("close_pane", "cmd+w"),
    ("split_right", "cmd+d"),
    ("split_down", "cmd+e"),
    ("new_browser_tab", "cmd+alt+t"),
    ("browser_pane", "cmd+alt+b"),
    ("zoom_pane", "cmd+enter"),
    ("next_tab", "cmd+shift+]"),
    ("previous_tab", "cmd+shift+["),
    ("next_pane", "cmd+]"),
    ("previous_pane", "cmd+["),
    ("pane_left", "cmd+alt+left"),
    ("pane_right", "cmd+alt+right"),
    ("pane_up", "cmd+alt+up"),
    ("pane_down", "cmd+alt+down"),
    ("tab_1", TAB_KEYS[0]),
    ("tab_2", TAB_KEYS[1]),
    ("tab_3", TAB_KEYS[2]),
    ("tab_4", TAB_KEYS[3]),
    ("tab_5", TAB_KEYS[4]),
    ("tab_6", TAB_KEYS[5]),
    ("tab_7", TAB_KEYS[6]),
    ("tab_8", TAB_KEYS[7]),
    ("tab_9", TAB_KEYS[8]),
    ("toggle_tab_list", "cmd+b"),
    ("extend_tab_list", "cmd+shift+b"),
    ("jump_to_agent", "cmd+`"),
    ("jump_to_newest_agent", "cmd+shift+`"),
    ("recap", "cmd+i"),
    ("copy", "cmd+c"),
    ("copy_command", "cmd+shift+c"),
    ("paste", "cmd+v"),
    ("clear", "cmd+k"),
    ("find", "cmd+f"),
    ("find_next", "cmd+g"),
    ("find_previous", "cmd+shift+g"),
    ("previous_block", "cmd+up"),
    ("next_block", "cmd+down"),
    ("bigger_text", "cmd+="),
    ("smaller_text", "cmd+-"),
    ("default_text_size", "cmd+0"),
    ("settings", "cmd+,"),
    ("check_for_updates", "cmd+u"),
    ("quick_terminal", ""),
    ("quit", "cmd+q"),
];

/// The default shortcut of an action, as a key event.
pub fn default_event(action: &str) -> Option<KeyEvent> {
    let (_, keys) = ACTIONS.iter().find(|(name, _)| *name == action)?;
    parse_combo(keys).map(|combo| combo.event())
}

pub fn parse_combo(text: &str) -> Option<Combo> {
    let text = text.trim().to_lowercase();
    if text.is_empty() {
        return None;
    }
    let (mut held, mut key) = (0, None);
    // "cmd++" binds plus: split on the last '+' that has a key after it.
    let mut parts: Vec<&str> = text.split('+').collect();
    if text.ends_with("++") || text == "+" {
        parts.retain(|p| !p.is_empty());
        parts.push("+");
    }
    for part in parts {
        match part.trim() {
            "cmd" | "command" | "super" | "shortcut" => held |= mods::COMMAND,
            "ctrl" | "control" => held |= mods::CONTROL,
            "alt" | "opt" | "option" | "meta" => held |= mods::OPTION,
            "shift" => held |= mods::SHIFT,
            // Two keys (`cmd+a+b`) or a misspelled modifier (`ctlr+t`): not a
            // combo, rather than quietly binding the last key on its own.
            _ if key.is_some() => return None,
            other => key = Some(other.to_owned()),
        }
    }
    let key = key?;
    let named = match key.as_str() {
        "up" => Some(Named::Up),
        "down" => Some(Named::Down),
        "left" => Some(Named::Left),
        "right" => Some(Named::Right),
        "home" => Some(Named::Home),
        "end" => Some(Named::End),
        "pageup" | "page_up" => Some(Named::PageUp),
        "pagedown" | "page_down" => Some(Named::PageDown),
        "insert" => Some(Named::Insert),
        "delete" => Some(Named::Delete),
        "backspace" => Some(Named::Backspace),
        "tab" => Some(Named::Tab),
        "enter" | "return" => Some(Named::Enter),
        "escape" | "esc" => Some(Named::Escape),
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) => {
            Some(Named::F(f[1..].parse().unwrap()))
        }
        _ => None,
    };
    let c = match (named, key.as_str()) {
        (Some(_), _) => '\0',
        (None, "space") => ' ',
        (None, "plus") => '+',
        (None, "minus") => '-',
        (None, "backtick" | "grave") => '`',
        (None, "comma") => ',',
        (None, other) if other.chars().count() == 1 => other.chars().next().unwrap(),
        _ => return None,
    };
    Some(Combo { mods: held, named, c })
}

/// `\n`, `\r`, `\t`, `\e`, `\\` and `\xNN` in a `text:` binding. `\xNN` is
/// that one byte, even above 0x7f (Meta-style keys send raw high bytes).
fn unescape(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<u8>, c: char| out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            push(&mut out, c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('e') => out.push(0x1b),
            Some('x') => {
                let hex: String = (0..2).filter_map(|_| chars.next_if(char::is_ascii_hexdigit)).collect();
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    out.push(byte);
                }
            }
            Some(other) => push(&mut out, other),
            None => out.push(b'\\'),
        }
    }
    out
}

/// The bindings in a keys file, and the lines that didn't make sense.
pub fn parse(text: &str) -> (Vec<(Combo, Bind)>, Vec<String>) {
    let mut bindings = Vec::new();
    let mut errors = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = line.split_once('=').and_then(|(keys, action)| {
            // `cmd+= = bigger_text`: the key is '=', the separator the next one.
            let (keys, action) = if action.trim_start().starts_with('=') && keys.ends_with('+') {
                (format!("{keys}="), action.trim_start()[1..].to_owned())
            } else {
                (keys.to_owned(), action.to_owned())
            };
            let combo = parse_combo(&keys)?;
            let action = action.trim();
            let bind = if let Some(text) = action.strip_prefix("text:") {
                Bind::Text(unescape(text))
            } else if matches!(action, "ignore" | "unbind" | "none") {
                Bind::Ignore
            } else {
                Bind::Action(ACTIONS.iter().find(|(name, _)| *name == action)?.0)
            };
            Some((combo, bind))
        });
        match parsed {
            Some(binding) => bindings.push(binding),
            None => errors.push(format!("line {}: {line}", number + 1)),
        }
    }
    (bindings, errors)
}

pub fn path() -> String {
    format!("{}/keys", crate::settings_dir())
}

/// What a new keys file says, every action listed with its default.
pub fn template() -> String {
    let mut text = String::from(
        "# Temino key bindings: one a line, `keys = action`.\n\
         # Modifiers: cmd (the shortcut key: ⌘ on macOS, Alt on Haiku, Ctrl+Shift on Linux),\n\
         # ctrl, alt (Option), shift. Keys: a letter or symbol, enter, tab, escape, space,\n\
         # backspace, delete, up, down, left, right, home, end, pageup, pagedown, f1-f12.\n\
         # An action can also be `text:...` (typed; \\r is Enter, \\e Escape, \\xNN a byte)\n\
         # or `ignore` (the shortcut does nothing). Saved changes apply at once.\n\
         #\n\
         # Examples:\n\
         # ctrl+shift+t = new_tab\n\
         # cmd+shift+k = text:clear\\r\n\
         # cmd+k = ignore\n\
         #\n\
         # Actions and their default keys:\n",
    );
    for (name, keys) in ACTIONS {
        let keys = if keys.is_empty() { "(none)" } else { keys };
        text.push_str(&format!("#   {name:<22} {keys}\n"));
    }
    text
}

/// The keys file as last read.
#[derive(Default)]
pub struct Bindings {
    pub list: Vec<(Combo, Bind)>,
    pub errors: Vec<String>,
    modified: Option<SystemTime>,
}

impl Bindings {
    /// Reads the file again if it changed; true when it did.
    pub fn refresh(&mut self) -> bool {
        let modified = std::fs::metadata(path()).and_then(|m| m.modified()).ok();
        if modified == self.modified {
            return false;
        }
        self.modified = modified;
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        (self.list, self.errors) = parse(&text);
        true
    }

    pub fn find(&self, event: &KeyEvent) -> Option<&Bind> {
        // The last binding of a key wins, as it would read top to bottom.
        self.list.iter().rev().find(|(combo, _)| combo.matches(event)).map(|(_, bind)| bind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos_parse() {
        let combo = parse_combo("Cmd+Shift+D").unwrap();
        assert_eq!((combo.mods, combo.named, combo.c), (mods::COMMAND | mods::SHIFT, None, 'd'));
        assert_eq!(parse_combo("ctrl+pageup").unwrap().named, Some(Named::PageUp));
        assert_eq!(parse_combo("cmd++").unwrap().c, '+');
        assert_eq!(parse_combo("f5").unwrap().named, Some(Named::F(5)));
        assert!(parse_combo("cmd+nonsense").is_none());
    }

    #[test]
    fn files_parse_with_errors_reported() {
        let (list, errors) = parse(
            "# hi\ncmd+= = bigger_text\nctrl+t=new_tab\ncmd+j = text:ls\\r\ncmd+k=ignore\nbad line\ncmd+x = nope\n",
        );
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].0.c, '=');
        assert_eq!(list[0].1, Bind::Action("bigger_text"));
        assert_eq!(list[2].1, Bind::Text(b"ls\r".to_vec()));
        assert_eq!(list[3].1, Bind::Ignore);
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn every_default_parses() {
        for (name, keys) in ACTIONS {
            if !keys.is_empty() {
                assert!(default_event(name).is_some(), "{name}");
            }
        }
    }
    fn press(named: Option<Named>, held: u32, raw: char) -> KeyEvent {
        KeyEvent { named, mods: held, raw_char: raw as u32, bytes: Vec::new() }
    }

    #[test]
    fn modifier_names_and_their_aliases() {
        for (text, held) in [
            ("cmd+a", mods::COMMAND),
            ("command+a", mods::COMMAND),
            ("super+a", mods::COMMAND),
            ("shortcut+a", mods::COMMAND),
            ("ctrl+a", mods::CONTROL),
            ("Control+a", mods::CONTROL),
            ("alt+a", mods::OPTION),
            ("opt+a", mods::OPTION),
            ("option+a", mods::OPTION),
            ("meta+a", mods::OPTION),
            ("shift+a", mods::SHIFT),
            ("a", 0),
            (" ctrl + alt + a ", mods::CONTROL | mods::OPTION),
        ] {
            assert_eq!(parse_combo(text), Some(Combo { mods: held, named: None, c: 'a' }), "{text}");
        }
    }

    #[test]
    fn named_keys_and_symbol_words() {
        let named = |text: &str| parse_combo(text).and_then(|combo| combo.named);
        assert_eq!(named("cmd+Enter"), Some(Named::Enter));
        assert_eq!(named("return"), Some(Named::Enter));
        assert_eq!(named("esc"), Some(Named::Escape));
        assert_eq!(named("page_down"), Some(Named::PageDown));
        assert_eq!(named("backspace"), Some(Named::Backspace));
        assert_eq!(named("f1"), Some(Named::F(1)));
        assert_eq!(named("F12"), Some(Named::F(12)));
        let c = |text: &str| parse_combo(text).map(|combo| combo.c);
        assert_eq!(c("ctrl+space"), Some(' '));
        assert_eq!(c("cmd+plus"), Some('+'));
        assert_eq!(c("cmd+minus"), Some('-'));
        assert_eq!(c("cmd+backtick"), Some('`'));
        assert_eq!(c("cmd+grave"), Some('`'));
        assert_eq!(c("cmd+comma"), Some(','));
        assert_eq!(c("cmd+-"), Some('-'));
        assert_eq!(c("+"), Some('+'));
        assert_eq!(c("cmd+é"), Some('é'));
    }

    #[test]
    fn malformed_combos_are_rejected() {
        for text in ["", "   ", "cmd", "ctrl+shift", "cmd+", "f0", "f13", "fx", "cmd+ab", "cmd+page up"] {
            assert_eq!(parse_combo(text), None, "{text:?}");
        }
    }

    #[test]
    fn misspelled_modifiers_and_extra_keys_are_rejected() {
        let (list, errors) = parse("ctlr+t = new_tab");
        assert_eq!((list.len(), errors.len()), (0, 1));
        assert_eq!(parse_combo("cmd+a+b"), None);
    }

    #[test]
    fn text_bindings_decode_escapes() {
        let text = |line: &str| match parse(line).0.pop() {
            Some((_, Bind::Text(text))) => String::from_utf8(text).unwrap(),
            other => panic!("{line}: {other:?}"),
        };
        assert_eq!(text(r"cmd+j = text:a\nb\tc\rd"), "a\nb\tc\rd");
        assert_eq!(text(r"cmd+j = text:\e[A"), "\x1b[A");
        assert_eq!(text(r"cmd+j = text:\x1b\x41"), "\x1bA");
        assert_eq!(text(r"cmd+j = text:\x4g"), "\x04g");
        assert_eq!(text(r"cmd+j = text:\xzz"), "zz");
        assert_eq!(text(r"cmd+j = text:\\ and \q"), r"\ and q");
        assert_eq!(text(r"cmd+j = text:end\"), r"end\");
        // `=` in the text, and a trailing space through \x20.
        assert_eq!(text(r"cmd+j = text:a=b"), "a=b");
        assert_eq!(text(r"cmd+j = text:git commit -m\x20"), "git commit -m ");
        assert_eq!(text("cmd+j = text:中文"), "中文");
        assert_eq!(text("cmd+j = text:"), "");
    }

    #[test]
    fn hex_escapes_above_ascii_are_single_bytes() {
        let (list, _) = parse(r"cmd+j = text:\xff");
        let Bind::Text(text) = &list[0].1 else { panic!() };
        assert_eq!(text, &[0xff]);
    }

    #[test]
    fn bad_lines_are_reported_with_their_numbers() {
        let (list, errors) = parse("\n  # comment\n= new_tab\ncmd+t\ncmd+t = \ncmd+t = New_Tab\nctrl+t = new_tab\n");
        assert_eq!(list, [(parse_combo("ctrl+t").unwrap(), Bind::Action("new_tab"))]);
        assert_eq!(errors, ["line 3: = new_tab", "line 4: cmd+t", "line 5: cmd+t =", "line 6: cmd+t = New_Tab"]);
        assert_eq!(parse(""), (Vec::new(), Vec::new()));
    }

    #[test]
    fn every_way_to_turn_a_shortcut_off() {
        let (list, errors) = parse("cmd+k = ignore\ncmd+l=unbind\ncmd+m =none");
        assert!(errors.is_empty());
        assert!(list.iter().all(|(_, bind)| *bind == Bind::Ignore));
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn combos_match_only_the_exact_modifiers_and_key() {
        let combo = parse_combo("cmd+shift+d").unwrap();
        assert!(combo.matches(&press(None, mods::COMMAND | mods::SHIFT, 'd')));
        // The key's own character, whatever case or symbol shift produced.
        assert!(combo.matches(&press(None, mods::COMMAND | mods::SHIFT, 'D')));
        assert!(!combo.matches(&press(None, mods::COMMAND, 'd')));
        assert!(!combo.matches(&press(None, mods::COMMAND | mods::SHIFT | mods::CONTROL, 'd')));
        assert!(!combo.matches(&press(None, mods::COMMAND | mods::SHIFT, 'e')));
        assert!(!combo.matches(&press(Some(Named::Enter), mods::COMMAND | mods::SHIFT, 'd')));
        // Bits other than the four modifiers are ignored.
        assert!(combo.matches(&press(None, mods::COMMAND | mods::SHIFT | 0x1000, 'd')));
        let enter = parse_combo("cmd+enter").unwrap();
        assert!(enter.matches(&press(Some(Named::Enter), mods::COMMAND, '\r')));
        assert!(!enter.matches(&press(None, mods::COMMAND, '\r')));
        // A combination's own event matches it.
        for (_, keys) in ACTIONS.iter().filter(|(_, keys)| !keys.is_empty()) {
            let combo = parse_combo(keys).unwrap();
            assert!(combo.matches(&combo.event()), "{keys}");
        }
    }

    #[test]
    fn the_last_binding_of_a_key_wins() {
        let (list, errors) = parse("cmd+t = new_window\ncmd+t = text:hi\nctrl+t = ignore");
        assert!(errors.is_empty());
        let bindings = Bindings { list, ..Bindings::default() };
        assert_eq!(bindings.find(&press(None, mods::COMMAND, 't')), Some(&Bind::Text(b"hi".to_vec())));
        assert_eq!(bindings.find(&press(None, mods::CONTROL, 't')), Some(&Bind::Ignore));
        assert_eq!(bindings.find(&press(None, mods::OPTION, 't')), None);
    }

    #[test]
    fn default_events_and_the_template() {
        let event = default_event("new_tab").unwrap();
        assert_eq!((event.mods, event.named, event.bytes.as_slice()), (mods::COMMAND, None, &b"t"[..]));
        assert_eq!(default_event("zoom_pane").unwrap().named, Some(Named::Enter));
        assert!(default_event("quick_terminal").is_none());
        assert!(default_event("no_such_action").is_none());
        // Tabs: ⌘1, or on Linux (where Super+1 is the dock's) ⌘⌥1.
        let tab = default_event("tab_1").unwrap();
        let alt = if cfg!(target_os = "linux") { mods::OPTION } else { 0 };
        assert_eq!((tab.mods, tab.bytes.as_slice()), (mods::COMMAND | alt, &b"1"[..]));
        // The template is all comments, so a new keys file binds nothing.
        let template = template();
        assert_eq!(parse(&template), (Vec::new(), Vec::new()));
        for (name, _) in ACTIONS {
            assert!(template.contains(name), "{name}");
        }
        // Its examples parse once uncommented.
        let examples: String = template
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .filter(|line| line.contains(" = ") && !line.contains('`'))
            .map(|line| format!("{line}\n"))
            .collect();
        let (list, errors) = parse(&examples);
        assert_eq!((list.len(), errors), (3, Vec::<String>::new()));
    }
}

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
    Text(String),
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

/// Every action a key can be bound to, and the default shortcut that does
/// it ("cmd" is the shortcut modifier on every platform).
pub const ACTIONS: &[(&str, &str)] = &[
    ("new_tab", "cmd+t"),
    ("close_pane", "cmd+w"),
    ("split_right", "cmd+d"),
    ("split_down", "cmd+e"),
    ("zoom_pane", "cmd+enter"),
    ("next_tab", "cmd+shift+]"),
    ("previous_tab", "cmd+shift+["),
    ("next_pane", "cmd+]"),
    ("previous_pane", "cmd+["),
    ("pane_left", "cmd+alt+left"),
    ("pane_right", "cmd+alt+right"),
    ("pane_up", "cmd+alt+up"),
    ("pane_down", "cmd+alt+down"),
    ("tab_1", "cmd+1"),
    ("tab_2", "cmd+2"),
    ("tab_3", "cmd+3"),
    ("tab_4", "cmd+4"),
    ("tab_5", "cmd+5"),
    ("tab_6", "cmd+6"),
    ("tab_7", "cmd+7"),
    ("tab_8", "cmd+8"),
    ("last_tab", "cmd+9"),
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

/// `\n`, `\r`, `\t`, `\e`, `\\` and `\xNN` in a `text:` binding.
fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some('x') => {
                let hex: String = (0..2).filter_map(|_| chars.next_if(char::is_ascii_hexdigit)).collect();
                if let Some(c) = u8::from_str_radix(&hex, 16).ok().map(char::from) {
                    out.push(c);
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
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
        assert_eq!(list[2].1, Bind::Text("ls\r".into()));
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
}

//! Settings ▸ Keys: what each action's shortcut is now, and the keys file
//! rewritten when one is changed there. Only the lines for what changed
//! are touched; comments and every other binding stay as they were.

use super::*;

/// Settings ▸ Keys, section by section: a title and its actions.
pub const GROUPS: [(&str, &[&str]); 4] = [
    (
        "Tabs and Windows",
        &[
            "new_tab",
            "new_window",
            "new_browser_tab",
            "close_pane",
            "next_tab",
            "previous_tab",
            "toggle_tab_list",
            "extend_tab_list",
            "tab_1",
            "tab_2",
            "tab_3",
            "tab_4",
            "tab_5",
            "tab_6",
            "tab_7",
            "tab_8",
            "tab_9",
        ],
    ),
    (
        "Panes",
        &[
            "split_right",
            "split_down",
            "browser_pane",
            "zoom_pane",
            "next_pane",
            "previous_pane",
            "pane_left",
            "pane_right",
            "pane_up",
            "pane_down",
            "jump_to_agent",
            "jump_to_newest_agent",
            "recap",
        ],
    ),
    (
        "Editing",
        &[
            "copy",
            "copy_command",
            "paste",
            "clear",
            "find",
            "find_next",
            "find_previous",
            "previous_block",
            "next_block",
        ],
    ),
    (
        "Text Size and App",
        &["bigger_text", "smaller_text", "default_text_size", "settings", "check_for_updates", "quit"],
    ),
];

/// Every action's name as Settings shows it.
const LABELS: &[(&str, &str)] = &[
    ("new_window", "New window"),
    ("new_tab", "New tab"),
    ("close_pane", "Close pane"),
    ("split_right", "Split right"),
    ("split_down", "Split down"),
    ("new_browser_tab", "New browser tab"),
    ("browser_pane", "Open browser pane"),
    ("zoom_pane", "Zoom pane"),
    ("next_tab", "Next tab"),
    ("previous_tab", "Previous tab"),
    ("next_pane", "Next pane"),
    ("previous_pane", "Previous pane"),
    ("pane_left", "Pane to the left"),
    ("pane_right", "Pane to the right"),
    ("pane_up", "Pane above"),
    ("pane_down", "Pane below"),
    ("tab_1", "Go to tab 1"),
    ("tab_2", "Go to tab 2"),
    ("tab_3", "Go to tab 3"),
    ("tab_4", "Go to tab 4"),
    ("tab_5", "Go to tab 5"),
    ("tab_6", "Go to tab 6"),
    ("tab_7", "Go to tab 7"),
    ("tab_8", "Go to tab 8"),
    ("tab_9", "Go to tab 9"),
    ("toggle_tab_list", "Show or hide tabs"),
    ("extend_tab_list", "Extended tab list"),
    ("jump_to_agent", "Jump to what needs you"),
    ("jump_to_newest_agent", "Jump to newest need"),
    ("recap", "Recap this pane"),
    ("copy", "Copy"),
    ("copy_command", "Copy command"),
    ("paste", "Paste"),
    ("clear", "Clear"),
    ("find", "Find"),
    ("find_next", "Find next"),
    ("find_previous", "Find previous"),
    ("previous_block", "Previous command"),
    ("next_block", "Next command"),
    ("bigger_text", "Bigger text"),
    ("smaller_text", "Smaller text"),
    ("default_text_size", "Default text size"),
    ("settings", "Settings"),
    ("check_for_updates", "Check for updates"),
    ("quick_terminal", "Quick terminal"),
    ("quit", "Quit Temino"),
];

/// An action's name as Settings shows it.
pub fn label(action: &str) -> &'static str {
    LABELS.iter().find(|(name, _)| *name == action).map_or("", |(_, label)| label)
}

/// An action's default shortcut.
pub fn default_combo(action: &str) -> Option<Combo> {
    let (_, keys) = ACTIONS.iter().find(|(name, _)| *name == action)?;
    parse_combo(keys)
}

/// What the keys file makes `combo` do, the last line for it winning.
fn bound<'a>(list: &'a [(Combo, Bind)], combo: &Combo) -> Option<&'a Bind> {
    list.iter().rev().find(|(c, _)| c == combo).map(|(_, bind)| bind)
}

/// The keys that run `action` now: the keys file's own for it, and its
/// default unless the file gives that key to something else.
pub fn keys_for(list: &[(Combo, Bind)], action: &str) -> Vec<Combo> {
    let mine = Bind::Action(ACTIONS.iter().find(|(name, _)| *name == action).map_or("", |(name, _)| name));
    let mut keys = Vec::new();
    if let Some(default) = default_combo(action) {
        if bound(list, &default).is_none_or(|bind| *bind == mine) {
            keys.push(default);
        }
    }
    for (combo, bind) in list {
        if *bind == mine && bound(list, combo) == Some(bind) && !keys.contains(combo) {
            keys.push(*combo);
        }
    }
    keys
}

/// The action `combo` runs now, if it runs one.
pub fn owner(list: &[(Combo, Bind)], combo: &Combo) -> Option<&'static str> {
    match bound(list, combo) {
        Some(Bind::Action(action)) => Some(action),
        Some(_) => None,
        None => ACTIONS.iter().find(|(name, _)| default_combo(name).as_ref() == Some(combo)).map(|(name, _)| *name),
    }
}

/// Whether `action`'s keys aren't its default any more.
pub fn changed(list: &[(Combo, Bind)], action: &str) -> bool {
    keys_for(list, action) != default_combo(action).into_iter().collect::<Vec<_>>()
}

/// The keys that type text, each once, as the file has them.
pub fn typed(list: &[(Combo, Bind)]) -> Vec<(Combo, Vec<u8>)> {
    let mut out: Vec<(Combo, Vec<u8>)> = Vec::new();
    for (combo, _) in list {
        if let (Some(Bind::Text(text)), false) = (bound(list, combo), out.iter().any(|(c, _)| c == combo)) {
            out.push((*combo, text.clone()));
        }
    }
    out
}

/// The keys file with `action` on `combo` alone (None: on no key at all).
/// A key taken from another action leaves that one without it.
pub fn set(text: &str, action: &str, combo: Option<Combo>) -> String {
    let default = default_combo(action);
    let mut lines: Vec<String> = text
        .lines()
        .filter(|line| match parse_line(line) {
            Some((c, bind)) => {
                let mine = matches!(bind, Bind::Action(name) if name == action);
                let off = Some(c) == default && bind == Bind::Ignore;
                !(mine || off || Some(c) == combo)
            }
            None => true,
        })
        .map(str::to_owned)
        .collect();
    if let Some(default) = default.filter(|&d| Some(d) != combo) {
        // The default key off, unless the file has it doing something else.
        if !lines.iter().any(|line| parse_line(line).is_some_and(|(c, _)| c == default)) {
            lines.push(format!("{} = ignore", file_keys(&default)));
        }
    }
    if let Some(combo) = combo.filter(|&c| Some(c) != default) {
        lines.push(format!("{} = {action}", file_keys(&combo)));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The keys file without any line for `combo`.
pub fn remove(text: &str, combo: &Combo) -> String {
    let lines: Vec<&str> = text.lines().filter(|line| parse_line(line).is_none_or(|(c, _)| c != *combo)).collect();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A key as the keys file writes it: `cmd+shift+d`.
pub fn file_keys(combo: &Combo) -> String {
    let mut out = String::new();
    for (bit, name) in [(mods::COMMAND, "cmd"), (mods::CONTROL, "ctrl"), (mods::OPTION, "alt"), (mods::SHIFT, "shift")]
    {
        if combo.mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    let key = match (combo.named, combo.c) {
        (Some(named), _) => named_key(named).0.to_owned(),
        (None, ' ') => "space".to_owned(),
        (None, '+') => "plus".to_owned(),
        (None, '-') => "minus".to_owned(),
        (None, ',') => "comma".to_owned(),
        (None, '`') => "backtick".to_owned(),
        (None, c) => c.to_string(),
    };
    out + &key
}

/// A special key's name in the keys file, and on screen (macOS's symbol,
/// then everywhere else's word).
fn named_key(named: Named) -> (&'static str, &'static str, &'static str) {
    match named {
        Named::Up => ("up", "↑", "Up"),
        Named::Down => ("down", "↓", "Down"),
        Named::Left => ("left", "←", "Left"),
        Named::Right => ("right", "→", "Right"),
        Named::Home => ("home", "↖", "Home"),
        Named::End => ("end", "↘", "End"),
        Named::PageUp => ("pageup", "⇞", "PgUp"),
        Named::PageDown => ("pagedown", "⇟", "PgDn"),
        Named::Insert => ("insert", "Ins", "Ins"),
        Named::Delete => ("delete", "⌦", "Del"),
        Named::Backspace => ("backspace", "⌫", "Backspace"),
        Named::Tab => ("tab", "⇥", "Tab"),
        Named::Enter => ("enter", "↩", "Enter"),
        Named::Escape => ("escape", "⎋", "Esc"),
        Named::F(n) => (
            F_KEYS[(n as usize).clamp(1, 12) - 1],
            F_KEYS[(n as usize).clamp(1, 12) - 1],
            F_NAMES[(n as usize).clamp(1, 12) - 1],
        ),
    }
}

const F_KEYS: [&str; 12] = ["f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12"];
const F_NAMES: [&str; 12] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];

/// A key as the OS writes it: "⇧⌘D" on macOS, "Ctrl+Shift+D" on Linux
/// (whose command modifier is Ctrl+Shift already), "Alt+Shift+D" on Haiku.
pub fn display(combo: &Combo) -> String {
    let key = match (combo.named, combo.c) {
        (Some(named), _) if cfg!(target_os = "macos") => named_key(named).1.to_owned(),
        (Some(named), _) => named_key(named).2.to_owned(),
        (None, ' ') => "Space".to_owned(),
        (None, c) => c.to_uppercase().collect(),
    };
    if cfg!(target_os = "macos") {
        let mut out = String::new();
        for (bit, symbol) in [(mods::CONTROL, "⌃"), (mods::OPTION, "⌥"), (mods::SHIFT, "⇧"), (mods::COMMAND, "⌘")]
        {
            if combo.mods & bit != 0 {
                out.push_str(symbol);
            }
        }
        return out + &key;
    }
    let option = if cfg!(target_os = "haiku") { "Option" } else { "Alt" };
    let mut held: Vec<&str> = Vec::new();
    if combo.mods & mods::COMMAND != 0 {
        held.extend(crate::platform::COMMAND_NAME.split('+'));
    }
    for (bit, name) in [(mods::CONTROL, "Ctrl"), (mods::OPTION, option), (mods::SHIFT, "Shift")] {
        if combo.mods & bit != 0 {
            held.push(name);
        }
    }
    let order = ["Ctrl", "Alt", "Option", "Shift"];
    held.sort_by_key(|name| order.iter().position(|o| o == name).unwrap_or(order.len()));
    held.dedup();
    held.push(&key);
    held.join("+")
}

/// A key pressed to bind: Err says why it can't be one. A plain letter
/// would stop it being typed, so a character needs ⌘, Ctrl or Option.
pub fn from_event(event: &KeyEvent) -> Result<Combo, &'static str> {
    let held = event.mods & (mods::SHIFT | mods::COMMAND | mods::CONTROL | mods::OPTION);
    let combo = match event.named {
        Some(named) => Combo { mods: held, named: Some(named), c: '\0' },
        None => match event.base_char().filter(|c| !c.is_control()) {
            Some(c) => Combo { mods: held, named: None, c },
            None => return Err("That key can't be a shortcut"),
        },
    };
    let function_key = matches!(combo.named, Some(Named::F(_)));
    if held & (mods::COMMAND | mods::CONTROL | mods::OPTION) == 0 && !function_key {
        return Err("Hold a modifier with it, so it isn't typed instead");
    }
    Ok(combo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> Combo {
        parse_combo(text).unwrap()
    }

    #[test]
    fn every_action_but_the_hotkey_is_in_a_group() {
        let grouped: Vec<&str> = GROUPS.iter().flat_map(|(_, actions)| actions.iter().copied()).collect();
        for (name, _) in ACTIONS.iter().filter(|(name, _)| *name != "quick_terminal") {
            assert_eq!(grouped.iter().filter(|a| *a == name).count(), 1, "{name}");
            assert!(!label(name).is_empty(), "{name}");
        }
        assert_eq!(grouped.len(), ACTIONS.len() - 1);
        assert_eq!(label("split_right"), "Split right");
        assert_eq!(label("tab_3"), "Go to tab 3");
    }

    #[test]
    fn keys_now_follow_the_file() {
        let (list, _) = parse("ctrl+t = new_tab\ncmd+d = split_down\ncmd+k = ignore\ncmd+j = text:ls\\r\n");
        // A key added keeps the default too.
        assert_eq!(keys_for(&list, "new_tab"), [combo("cmd+t"), combo("ctrl+t")]);
        // A default taken by another action leaves its own without it.
        assert_eq!(keys_for(&list, "split_right"), []);
        assert_eq!(keys_for(&list, "split_down"), [combo("cmd+e"), combo("cmd+d")]);
        assert_eq!(keys_for(&list, "clear"), []);
        assert!(changed(&list, "clear") && !changed(&list, "paste"));
        assert_eq!(owner(&list, &combo("cmd+d")), Some("split_down"));
        assert_eq!(owner(&list, &combo("cmd+v")), Some("paste"));
        assert_eq!(owner(&list, &combo("cmd+k")), None);
        assert_eq!(typed(&list), [(combo("cmd+j"), b"ls\r".to_vec())]);
    }

    #[test]
    fn setting_a_key_rewrites_only_its_lines() {
        let file = "# my keys\nctrl+t = new_tab\ncmd+j = text:hi\n";
        let out = set(file, "new_tab", Some(combo("cmd+shift+n")));
        assert_eq!(out, "# my keys\ncmd+j = text:hi\ncmd+t = ignore\ncmd+shift+n = new_tab\n");
        let (list, errors) = parse(&out);
        assert!(errors.is_empty());
        assert_eq!(keys_for(&list, "new_tab"), [combo("cmd+shift+n")]);
        // Back to the default: nothing of it is left in the file.
        assert_eq!(set(&out, "new_tab", default_combo("new_tab")), "# my keys\ncmd+j = text:hi\n");
        // On no key: its default turned off.
        let (list, _) = parse(&set("", "clear", None));
        assert_eq!(keys_for(&list, "clear"), []);
        // Taking a key that types text replaces it.
        let (list, _) = parse(&set(file, "find", Some(combo("cmd+j"))));
        assert_eq!(keys_for(&list, "find"), [combo("cmd+j")]);
        assert!(typed(&list).is_empty());
        assert_eq!(remove(file, &combo("ctrl+t")), "# my keys\ncmd+j = text:hi\n");
    }

    #[test]
    fn a_default_given_away_stays_given_away() {
        // Split right's ⌘D was given to Split down; moving Split right
        // elsewhere doesn't turn ⌘D off.
        let file = "cmd+d = split_down\n";
        let (list, _) = parse(&set(file, "split_right", Some(combo("cmd+alt+d"))));
        assert_eq!(keys_for(&list, "split_down"), [combo("cmd+e"), combo("cmd+d")]);
        assert_eq!(keys_for(&list, "split_right"), [combo("cmd+alt+d")]);
    }

    #[test]
    fn written_keys_read_back() {
        for text in
            ["cmd+shift+]", "ctrl+alt+up", "cmd+=", "cmd+-", "cmd++", "cmd+,", "cmd+`", "ctrl+space", "f5", "cmd+enter"]
        {
            let combo = combo(text);
            let line = format!("{} = find", file_keys(&combo));
            assert_eq!(parse_line(&line), Some((combo, Bind::Action("find"))), "{line}");
        }
    }

    #[test]
    fn keys_show_as_the_os_writes_them() {
        let shown = display(&combo("cmd+shift+d"));
        if cfg!(target_os = "macos") {
            assert_eq!(shown, "⇧⌘D");
            assert_eq!(display(&combo("ctrl+alt+left")), "⌃⌥←");
        } else if cfg!(target_os = "haiku") {
            assert_eq!(shown, "Alt+Shift+D");
        } else {
            assert_eq!(shown, "Ctrl+Shift+D");
            assert_eq!(display(&combo("ctrl+alt+left")), "Ctrl+Alt+Left");
        }
    }

    #[test]
    fn a_pressed_key_needs_a_modifier() {
        let press = |named, held, c: char| KeyEvent { named, mods: held, raw_char: c as u32, bytes: Vec::new() };
        assert_eq!(from_event(&press(None, mods::COMMAND | mods::SHIFT, 'D')), Ok(combo("cmd+shift+d")));
        assert!(from_event(&press(None, 0, 'd')).is_err());
        assert!(from_event(&press(None, mods::SHIFT, 'd')).is_err());
        assert_eq!(from_event(&press(Some(Named::F(5)), 0, '\0')), Ok(combo("f5")));
        assert_eq!(from_event(&press(Some(Named::Up), mods::OPTION, '\0')), Ok(combo("alt+up")));
    }
}

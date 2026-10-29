//! Tab completion for the editor: commands for the first word, paths
//! (relative to the shell's working directory) everywhere else.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const BUILTINS: &[&str] = &[
    "alias", "bg", "bind", "break", "builtin", "cd", "command", "continue", "declare", "dirs", "disown", "echo",
    "enable", "eval", "exec", "exit", "export", "false", "fc", "fg", "getopts", "hash", "help", "history", "jobs",
    "kill", "let", "local", "logout", "popd", "printf", "pushd", "pwd", "read", "readonly", "return", "set", "shift",
    "shopt", "source", "suspend", "test", "times", "trap", "true", "type", "typeset", "ulimit", "umask", "unalias",
    "unset", "wait",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Command,
    Directory,
    File,
    History,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    /// Text that replaces the word being completed.
    pub replacement: String,
    /// What the menu shows.
    pub label: String,
    pub kind: Kind,
}

pub struct Completion {
    /// Byte range of the word in the editor text.
    pub start: usize,
    pub end: usize,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
}

/// Commands on $PATH, rescanned at most every 30 seconds. The shell's
/// $PATH once it reported one: Temino's own, launched from the Dock, lacks
/// Homebrew and whatever the user's profile adds.
pub struct Commands {
    names: BTreeSet<String>,
    scanned: Option<Instant>,
    path: Option<String>,
}

impl Commands {
    pub fn new() -> Commands {
        Commands { names: BTreeSet::new(), scanned: None, path: None }
    }

    /// Searches `path` from now on, when the shell reported one.
    pub fn use_path(&mut self, path: Option<&str>) {
        if let Some(path) = path.filter(|path| self.path.as_deref() != Some(path)) {
            self.path = Some(path.to_owned());
            self.scanned = None;
        }
    }

    fn refresh(&mut self) {
        if self.scanned.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
            return;
        }
        self.names = BUILTINS.iter().map(|s| s.to_string()).collect();
        let path = self.path.clone().or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        for dir in path.split(':').filter(|d| !d.is_empty() && *d != ".") {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    self.names.insert(name.to_owned());
                }
            }
        }
        self.scanned = Some(Instant::now());
    }

    pub fn exists(&mut self, name: &str) -> bool {
        if name.contains('/') {
            return std::path::Path::new(name).exists();
        }
        self.refresh();
        self.names.contains(name)
    }

    fn matching(&mut self, prefix: &str) -> Vec<String> {
        self.refresh();
        self.names.range(prefix.to_owned()..).take_while(|n| n.starts_with(prefix)).cloned().collect()
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if " \t'\"\\$`&|;<>()[]*?!#~{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else if c != '\'' && c != '"' {
            out.push(c);
        }
    }
    out
}

/// Start of the shell word ending at `cursor` (backslash-escaped spaces
/// stay inside the word).
fn word_start(text: &str, cursor: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = cursor;
    while i > 0 {
        let b = bytes[i - 1];
        let escaped = i >= 2 && bytes[i - 2] == b'\\';
        if (b == b' ' || b == b'\t' || b == b'\n' || b == b'|' || b == b';' || b == b'&' || b == b'(') && !escaped {
            break;
        }
        i -= 1;
    }
    i
}

/// Whether the word starting at `start` is in command position.
fn command_position(text: &str, start: usize) -> bool {
    let before = text[..start].trim_end();
    before.is_empty() || before.ends_with(['|', ';', '&', '(', '\n']) || before.ends_with("sudo")
}

pub fn complete(text: &str, cursor: usize, cwd: &str, commands: &mut Commands) -> Option<Completion> {
    let start = word_start(text, cursor);
    let word = unescape(&text[start..cursor]);
    let mut candidates = Vec::new();

    if command_position(text, start) && !word.contains('/') {
        for name in commands.matching(&word) {
            candidates.push(Candidate { replacement: escape(&name) + " ", label: name, kind: Kind::Command });
        }
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into());
        let (dir_part, prefix) = match word.rfind('/') {
            Some(i) => (&word[..=i], &word[i + 1..]),
            None => ("", word.as_str()),
        };
        let dir = if dir_part.starts_with('/') {
            dir_part.to_owned()
        } else if let Some(rest) = dir_part.strip_prefix("~/") {
            format!("{home}/{rest}")
        } else if dir_part == "~" {
            home.clone()
        } else {
            format!("{cwd}/{dir_part}")
        };
        let Ok(entries) = std::fs::read_dir(if dir.is_empty() { "." } else { &dir }) else { return None };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                continue;
            }
            let is_dir = std::fs::metadata(entry.path()).map(|m| m.is_dir()).unwrap_or(false);
            let replacement = format!("{}{}{}", escape(dir_part), escape(&name), if is_dir { "/" } else { " " });
            let label = if is_dir { format!("{name}/") } else { name };
            candidates.push(Candidate { replacement, label, kind: if is_dir { Kind::Directory } else { Kind::File } });
        }
        candidates.sort_by(|a, b| (a.kind != Kind::Directory, &a.label).cmp(&(b.kind != Kind::Directory, &b.label)));
    }
    if candidates.is_empty() {
        return None;
    }
    candidates.truncate(500);
    Some(Completion { start, end: cursor, candidates, selected: 0 })
}

/// What Tab would add to the end of `text`, for showing in grey: the rest
/// of the only match, or of what every match shares.
pub fn hint(text: &str, cwd: &str, commands: &mut Commands) -> Option<String> {
    let completion = complete(text, text.len(), cwd, commands)?;
    let fill = match completion.candidates.as_slice() {
        [only] => only.replacement.clone(),
        many => common_prefix(many),
    };
    let rest = fill.strip_prefix(&text[completion.start..])?.trim_end_matches(' ');
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// Longest common prefix of every candidate's replacement.
pub fn common_prefix(candidates: &[Candidate]) -> String {
    let first = &candidates[0].replacement;
    let mut length = first.len();
    for candidate in &candidates[1..] {
        length = first
            .char_indices()
            .zip(candidate.replacement.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
            .min(length);
    }
    first[..length].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_fill_in_what_tab_would() {
        let dir = std::env::temp_dir().join(format!("temino-hint-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("projects")).unwrap();
        std::fs::write(dir.join("notes-one.txt"), "").unwrap();
        std::fs::write(dir.join("notes-two.txt"), "").unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/claude"), "").unwrap();
        let cwd = dir.to_str().unwrap();
        let mut commands = Commands::new();
        commands.use_path(Some(&format!("{cwd}/bin")));
        assert_eq!(hint("cd pro", cwd, &mut commands).as_deref(), Some("jects/"));
        assert_eq!(hint("cat no", cwd, &mut commands).as_deref(), Some("tes-"));
        assert_eq!(hint("cat notes-", cwd, &mut commands), None);
        assert_eq!(hint("cla", cwd, &mut commands).as_deref(), Some("ude"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

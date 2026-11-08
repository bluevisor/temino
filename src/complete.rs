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
    // Spaces only: a newline before the word starts a new command.
    let before = text[..start].trim_end_matches([' ', '\t']);
    let last_word = before.rsplit(|c: char| c.is_whitespace() || matches!(c, '|' | ';' | '&' | '(')).next();
    before.is_empty() || before.ends_with(['|', ';', '&', '(', '\n']) || last_word == Some("sudo")
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
        let entries: Vec<_> = entries
            .flatten()
            .filter_map(|entry| Some((entry.file_name().to_str()?.to_owned(), entry)))
            .filter(|(name, _)| !name.starts_with('.') || prefix.starts_with('.'))
            .collect();
        // Names that start with the prefix as typed; only when there are
        // none, names that start with it in another case ("is" finds iSay).
        let folded = prefix.to_lowercase();
        let exact = entries.iter().any(|(name, _)| name.starts_with(prefix));
        for (name, entry) in entries {
            let matches = if exact { name.starts_with(prefix) } else { name.to_lowercase().starts_with(&folded) };
            if !matches {
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

    #[test]
    fn paths_match_in_another_case_only_when_nothing_matches_as_typed() {
        let dir = std::env::temp_dir().join(format!("temino-case-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("iSay")).unwrap();
        std::fs::write(dir.join("Notes.txt"), "").unwrap();
        std::fs::write(dir.join("notebook"), "").unwrap();
        let cwd = dir.to_str().unwrap();
        let mut commands = Commands::new();
        let labels = |text: &str, commands: &mut Commands| {
            let completion = complete(text, text.len(), cwd, commands).unwrap();
            completion.candidates.into_iter().map(|c| c.replacement).collect::<Vec<_>>()
        };
        assert_eq!(labels("cd is", &mut commands), ["iSay/"]);
        assert_eq!(labels("cat no", &mut commands), ["notebook "]);
        assert_eq!(labels("cat NO", &mut commands), ["Notes.txt ", "notebook "]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// A scratch directory for one test, removed when the test ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str, files: &[&str]) -> Scratch {
            let dir = std::env::temp_dir().join(format!("temino-complete-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for file in files {
                let path = dir.join(file);
                if let Some(folder) = file.strip_suffix('/') {
                    std::fs::create_dir_all(dir.join(folder)).unwrap();
                } else {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, "").unwrap();
                }
            }
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn replacements(text: &str, cwd: &str, commands: &mut Commands) -> Vec<String> {
        complete(text, text.len(), cwd, commands)
            .map_or(Vec::new(), |c| c.candidates.into_iter().map(|c| c.replacement).collect())
    }

    fn candidate(replacement: &str) -> Candidate {
        Candidate { replacement: replacement.into(), label: replacement.into(), kind: Kind::File }
    }

    #[test]
    fn shell_specials_are_escaped_and_unescaped() {
        assert_eq!(escape("my file (1).txt"), r"my\ file\ \(1\).txt");
        assert_eq!(
            escape("a'b\"c$d`e&f|g;h<i>j*k?l!m#n~o{p}q[r]s\\t"),
            r#"a\'b\"c\$d\`e\&f\|g\;h\<i\>j\*k\?l\!m\#n\~o\{p\}q\[r\]s\\t"#
        );
        assert_eq!(escape("中文-ok_1.rs"), "中文-ok_1.rs");
        assert_eq!(unescape(r"my\ file"), "my file");
        assert_eq!(unescape(r"'quoted dir'/x"), "quoted dir/x");
        assert_eq!(unescape(r#""a"\\b"#), r"a\b");
        assert_eq!(unescape("trailing\\"), "trailing");
        for name in ["plain", "with space", "it's", "$HOME", "a\\b", "中 文"] {
            assert_eq!(unescape(&escape(name)), name);
        }
    }

    #[test]
    fn words_end_at_unescaped_separators() {
        assert_eq!(word_start("ls foo", 6), 3);
        assert_eq!(word_start(r"cat my\ fi", 10), 4);
        assert_eq!(word_start("a|b", 3), 2);
        assert_eq!(word_start("a;b", 3), 2);
        assert_eq!(word_start("a&&b", 4), 3);
        assert_eq!(word_start("(cd x", 3), 1);
        assert_eq!(word_start("one\ntwo", 7), 4);
        assert_eq!(word_start("one\ttwo", 7), 4);
        assert_eq!(word_start("", 0), 0);
        assert_eq!(word_start("ls 中文", "ls 中文".len()), 3);
        // Up to the caret only.
        assert_eq!(word_start("ls foo bar", 6), 3);
    }

    #[test]
    fn the_first_word_and_words_after_operators_are_commands() {
        assert!(command_position("", 0));
        assert!(command_position("  ", 2));
        assert!(command_position("ls | gr", 5));
        assert!(command_position("a; b", 3));
        assert!(command_position("a && b", 5));
        assert!(command_position("(cd", 1));
        assert!(command_position("sudo ap", 5));
        assert!(!command_position("ls fo", 3));
        assert!(!command_position("git commit", 4));
    }

    #[test]
    fn only_sudo_itself_starts_a_command() {
        assert!(!command_position("visudo fi", 7));
        assert!(command_position("sudo fi", 5));
    }

    #[test]
    fn the_first_word_of_a_new_line_is_a_command() {
        assert!(command_position("ls\ngit", 3));
    }

    #[test]
    fn commands_come_from_the_given_path_and_the_builtins() {
        let bin = Scratch::new("bin", &["bin/mytool", "bin/mytask", "other/mything"]);
        let mut commands = Commands::new();
        commands.use_path(Some(&format!("{0}/bin::.:{0}/missing", bin.path())));
        assert_eq!(replacements("myt", "/", &mut commands), ["mytask ", "mytool "]);
        assert_eq!(replacements("ls | myto", "/", &mut commands), ["mytool "]);
        assert_eq!(replacements("sudo myto", "/", &mut commands), ["mytool "]);
        assert!(replacements("ech", "/", &mut commands).contains(&"echo ".to_owned()));
        assert!(commands.exists("mytool"));
        assert!(commands.exists("cd"));
        assert!(!commands.exists("mything"));
        // A name with a slash is checked as a path.
        assert!(commands.exists(&format!("{}/other/mything", bin.path())));
        assert!(!commands.exists("./no/such/tool"));
        // A new path is scanned at once.
        commands.use_path(Some(&format!("{}/other", bin.path())));
        assert!(commands.exists("mything"));
        assert!(!commands.exists("mytool"));
    }

    #[test]
    fn files_are_completed_relative_to_the_working_directory() {
        let dir =
            Scratch::new("files", &["src/", "src/main.rs", "src/menu.rs", "Cargo.toml", "my file.txt", ".hidden"]);
        let cwd = dir.path();
        let mut commands = Commands::new();
        assert_eq!(replacements("cat src/m", cwd, &mut commands), ["src/main.rs ", "src/menu.rs "]);
        assert_eq!(replacements("cat src/ma", cwd, &mut commands), ["src/main.rs "]);
        // Spaces are escaped in the replacement, and an escaped space typed matches.
        assert_eq!(replacements("cat my", cwd, &mut commands), [r"my\ file.txt "]);
        assert_eq!(replacements(r"cat my\ f", cwd, &mut commands), [r"my\ file.txt "]);
        // Hidden files only when the word starts with a dot.
        assert!(!replacements("cat ", cwd, &mut commands).iter().any(|r| r.contains("hidden")));
        assert_eq!(replacements("cat .h", cwd, &mut commands), [".hidden "]);
        // Directories first, then files, each sorted.
        assert_eq!(replacements("cat ", cwd, &mut commands), ["src/", "Cargo.toml ", r"my\ file.txt "]);
        assert_eq!(replacements("cat nothing", cwd, &mut commands), Vec::<String>::new());
        assert!(complete("cat no/such/dir/x", 17, cwd, &mut commands).is_none());
    }

    #[test]
    fn absolute_paths_and_a_slash_in_the_first_word_complete_files() {
        let dir = Scratch::new("abs", &["run.sh", "sub/"]);
        let mut commands = Commands::new();
        let text = format!("{}/ru", dir.path());
        assert_eq!(replacements(&text, "/", &mut commands), [format!("{}/run.sh ", escape(dir.path()))]);
        assert_eq!(replacements("./ru", dir.path(), &mut commands), ["./run.sh "]);
        assert_eq!(replacements("cd s", dir.path(), &mut commands), ["sub/"]);
    }

    #[test]
    fn completion_covers_the_word_up_to_the_caret() {
        let dir = Scratch::new("caret", &["alpha", "beta"]);
        let mut commands = Commands::new();
        let completion = complete("cat al tail", 6, dir.path(), &mut commands).unwrap();
        assert_eq!((completion.start, completion.end, completion.selected), (4, 6, 0));
        assert_eq!(completion.candidates[0].replacement, "alpha ");
        assert_eq!(completion.candidates[0].label, "alpha");
        assert_eq!(completion.candidates[0].kind, Kind::File);
    }

    #[test]
    fn the_common_prefix_respects_character_boundaries() {
        assert_eq!(common_prefix(&[candidate("中文a"), candidate("中文b")]), "中文");
        assert_eq!(common_prefix(&[candidate("中a"), candidate("文a")]), "");
        assert_eq!(common_prefix(&[candidate("only")]), "only");
        assert_eq!(common_prefix(&[candidate("abc"), candidate("ab"), candidate("abd")]), "ab");
        assert_eq!(common_prefix(&[candidate("é1"), candidate("é2")]), "é");
    }

    #[test]
    fn hints_need_something_to_add() {
        let dir = Scratch::new("hint", &["readme.md"]);
        let mut commands = Commands::new();
        commands.use_path(Some(dir.path()));
        assert_eq!(hint("cat read", dir.path(), &mut commands).as_deref(), Some("me.md"));
        // Already complete: the trailing space isn't hinted.
        assert_eq!(hint("cat readme.md", dir.path(), &mut commands), None);
        assert_eq!(hint("cat zzz", dir.path(), &mut commands), None);
    }
}

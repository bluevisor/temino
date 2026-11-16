//! Temino as the system's terminal: what other apps hand it, and the
//! setting that makes it the one they hand things to.
//!
//! - macOS: a double-clicked script (`.command`, `.tool`, an executable),
//!   an `ssh://` or `telnet://` link, or an `x-man-page://` link arrives as
//!   an Apple event (src/macos/opener.m) and opens a new tab running it.
//! - Linux: `temino -e COMMAND…` (xdg-terminal-exec, KDE, Cinnamon, MATE)
//!   opens a tab in the running Temino, or starts one with it.
//!
//! Settings ▸ Shell ▸ Default terminal sets the system's choice: Launch
//! Services' handlers on macOS; `xdg-terminals.list`, KDE's `kdeglobals`
//! and the GNOME-family `gsettings` keys on Linux. Haiku has no choice to make.

use std::time::Duration;

use crate::remote::quote;

/// What to open: a new tab, in `cwd`, running `command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    pub cwd: Option<String>,
    pub command: Option<String>,
}

impl Open {
    /// The `new_tab` API call's params.
    fn params(&self) -> String {
        let mut fields = Vec::new();
        if let Some(cwd) = &self.cwd {
            fields.push(format!(r#""cwd":{}"#, quote(cwd)));
        }
        if let Some(command) = &self.command {
            fields.push(format!(r#""command":{}"#, quote(command)));
        }
        format!("{{{}}}", fields.join(","))
    }
}

/// One word for the shell (bash, zsh and fish all take it): quoted unless
/// it's plainly safe. Quoting doesn't stop a word reading as an option:
/// what's passed here is checked for that first.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c));
    if plain {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The launch arguments a terminal is called with (`-e COMMAND…`,
/// `--working-directory DIR`, `-- COMMAND…`), if any: None for a plain
/// launch, or one Temino handles itself (`--new-window`).
pub fn from_args(args: &[String]) -> Option<Open> {
    let mut open = Open { cwd: None, command: None };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-e" | "-x" | "--execute" | "--" => {
                let words: Vec<&String> = rest.by_ref().collect();
                open.command = command_of(&words);
                break;
            }
            "--working-directory" | "--workdir" | "-d" => open.cwd = rest.next().cloned(),
            _ => open.cwd = Some(arg.strip_prefix("--working-directory=")?.to_owned()),
        }
    }
    (open.cwd.is_some() || open.command.is_some()).then_some(open)
}

/// The command line `-e` was given: one word is a whole command line
/// (`-e "htop -d 5"`, as KDE passes it) unless it names a file; several are
/// separate words (xdg-terminal-exec), each quoted.
fn command_of(words: &[&String]) -> Option<String> {
    match words {
        [] => None,
        [one] if !std::path::Path::new(one.as_str()).exists() => Some((*one).clone()),
        _ => Some(words.iter().map(|w| shell_word(w)).collect::<Vec<_>>().join(" ")),
    }
}

#[cfg(any(target_os = "macos", test))]
/// What a file another app opened in Temino becomes: a folder, a tab in
/// it; a script or program, a tab that runs it.
pub fn from_file(path: &str) -> Open {
    if std::path::Path::new(path).is_dir() {
        return Open { cwd: Some(path.to_owned()), command: None };
    }
    Open { cwd: None, command: Some(shell_word(path)) }
}

#[cfg(any(target_os = "macos", test))]
/// What a link becomes: `ssh://user@host:port`, `telnet://host:port` and
/// `x-man-page://section/page`. None for anything else, or anything that
/// isn't plainly a host, user, port or page — a web page can send these
/// links, and a "host" of `-oProxyCommand=…` must not reach ssh.
pub fn from_url(url: &str) -> Option<Open> {
    let (scheme, rest) = url.split_once("://")?;
    let rest = rest.trim_end_matches('/');
    let command = match scheme.to_ascii_lowercase().as_str() {
        "ssh" | "telnet" => {
            let (user, host_port) = match rest.rsplit_once('@') {
                // ssh URIs may carry `;fingerprint=…` after the user.
                Some((user, host)) => (Some(user.split(';').next()?), host),
                None => (None, rest),
            };
            let (host, port) = split_port(host_port)?;
            if user.is_some_and(|u| !is_name(u)) || !is_host(host) || port.is_some_and(|p| p.parse::<u16>().is_err()) {
                return None;
            }
            let mut words = vec![scheme.to_ascii_lowercase()];
            if scheme.eq_ignore_ascii_case("ssh") {
                if let Some(port) = port {
                    words.extend(["-p".to_owned(), port.to_owned()]);
                }
                words.push(match user {
                    Some(user) => format!("{user}@{host}"),
                    None => host.to_owned(),
                });
            } else {
                if let Some(user) = user {
                    words.extend(["-l".to_owned(), user.to_owned()]);
                }
                words.push(host.to_owned());
                words.extend(port.map(str::to_owned));
            }
            words.iter().map(|w| shell_word(w)).collect::<Vec<_>>().join(" ")
        }
        "x-man-page" => {
            let (section, page) = match rest.split_once('/') {
                Some((section, page)) => (Some(section), page),
                None => (None, rest),
            };
            let section_ok = section.is_none_or(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()));
            if !section_ok || !is_name(page) {
                return None;
            }
            match section {
                Some(section) => format!("man {section} {}", shell_word(page)),
                None => format!("man {}", shell_word(page)),
            }
        }
        _ => return None,
    };
    Some(Open { cwd: None, command: Some(command) })
}

#[cfg(any(target_os = "macos", test))]
/// `host:port`, `[v6]:port` or a bare host.
fn split_port(text: &str) -> Option<(&str, Option<&str>)> {
    if let Some(v6) = text.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        return match after {
            "" => Some((host, None)),
            _ => Some((host, Some(after.strip_prefix(':')?))),
        };
    }
    Some(match text.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (text, None),
    })
}

#[cfg(any(target_os = "macos", test))]
/// A user or man page name: letters, digits and `._-+:`, not led by `-`.
fn is_name(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && text.chars().all(|c| c.is_ascii_alphanumeric() || "._-+:".contains(c))
}

#[cfg(any(target_os = "macos", test))]
/// A host name or address.
fn is_host(text: &str) -> bool {
    !text.is_empty() && !text.starts_with('-') && text.chars().all(|c| c.is_ascii_alphanumeric() || ".-:%".contains(c))
}

/// Hands `open` to a Temino already running, if there is one; false if
/// none answered (this process should then start and open it itself).
pub fn hand_to_running(open: &Open) -> bool {
    crate::api::call("new_tab", &open.params()).is_ok()
}

/// Opens a new tab in this Temino, once it has a window to open it in.
pub fn open(open: Open) {
    let _ = std::thread::Builder::new().name("opener".into()).spawn(move || {
        // Launched to open it, the first window takes a moment.
        for _ in 0..150 {
            if let Some(&no) = crate::remote::windows().first() {
                if let Err(error) = crate::api::ask(no, "new_tab", &open.params()) {
                    crate::store::log(&format!("opener: couldn't open {open:?}: {error}"));
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        crate::store::log(&format!("opener: no window came to open {open:?} in"));
    });
}

// ---- macOS ---------------------------------------------------------------

#[cfg(target_os = "macos")]
mod system {
    use std::ffi::{c_char, CStr};

    extern "C" {
        fn temino_opener_install(callback: extern "C" fn(i32, *const c_char));
        fn temino_default_terminal_is() -> bool;
        /// 0 on success, else Launch Services' OSStatus.
        fn temino_default_terminal_set(on: bool) -> i32;
    }

    /// 0 a link, 1 a file's path.
    extern "C" fn opened(kind: i32, text: *const c_char) {
        if text.is_null() {
            return;
        }
        let text = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
        let open = match kind {
            1 => Some(super::from_file(&text)),
            _ => super::from_url(&text),
        };
        match open {
            Some(open) => super::open(open),
            None => crate::store::log(&format!("opener: won't open {text}")),
        }
    }

    /// Takes the files and links other apps open in Temino. Before the
    /// event loop runs, so the one it was launched for isn't missed.
    pub fn install() {
        unsafe { temino_opener_install(opened) };
    }

    pub fn is_default() -> Option<bool> {
        Some(unsafe { temino_default_terminal_is() })
    }

    pub fn set_default(on: bool) -> Result<(), String> {
        match unsafe { temino_default_terminal_set(on) } {
            0 => Ok(()),
            status => Err(format!("macOS wouldn't change it (error {status})")),
        }
    }

    pub const DETAIL: &str = "Scripts you double-click, and ssh:// and man page links, open here";
}

// ---- Linux ---------------------------------------------------------------

#[cfg(not(any(target_os = "macos", target_os = "haiku")))]
mod system {
    use std::process::{Command, Stdio};

    /// The launcher's file name: install.sh and the packages install it so.
    const DESKTOP_ID: &str = "temino.desktop";

    pub fn install() {}

    /// xdg-terminal-exec's list of terminals, most wanted first.
    fn list_path() -> String {
        let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|d| !d.is_empty());
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/xdg-terminals.list", config.unwrap_or_else(|| format!("{home}/.config")))
    }

    /// Lines naming Temino: `temino.desktop`, or one of its actions.
    fn names_us(line: &str) -> bool {
        let id = line.trim();
        id == DESKTOP_ID || id.starts_with(&format!("{DESKTOP_ID}:"))
    }

    /// The list with Temino first (`on`), or without it.
    pub(super) fn listed(list: &str, on: bool) -> String {
        let mut lines: Vec<&str> = list.lines().filter(|line| !names_us(line)).collect();
        if on {
            lines.insert(0, DESKTOP_ID);
        }
        let mut out = lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        out
    }

    pub fn is_default() -> Option<bool> {
        let list = std::fs::read_to_string(list_path()).unwrap_or_default();
        let first = list.lines().map(str::trim).find(|line| !line.is_empty() && !line.starts_with('#'));
        Some(first.is_some_and(names_us))
    }

    fn exe() -> String {
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "temino".to_owned())
    }

    fn quiet(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// The GNOME family's terminal keys (Cinnamon's and MATE's file
    /// managers still read theirs): schema, command key, argument key.
    const GSETTINGS: [(&str, &str, &str); 3] = [
        ("org.gnome.desktop.default-applications.terminal", "exec", "exec-arg"),
        ("org.cinnamon.desktop.default-applications.terminal", "exec", "exec-arg"),
        ("org.mate.applications-terminal", "exec", "exec-arg"),
    ];

    pub fn set_default(on: bool) -> Result<(), String> {
        let path = list_path();
        let list = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(dir) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if !crate::store::write_atomic(&path, listed(&list, on).as_bytes(), false) {
            return Err(format!("couldn't write {path}"));
        }
        let exe = exe();
        // Each desktop's own key, where it has one; only ever reset when
        // it still names Temino.
        for (schema, exec, arg) in GSETTINGS {
            let Some(now) = quiet("gsettings", &["get", schema, exec]) else { continue };
            if on {
                quiet("gsettings", &["set", schema, exec, &exe]);
                quiet("gsettings", &["set", schema, arg, "-e"]);
            } else if now.trim_matches('\'') == exe {
                quiet("gsettings", &["reset", schema, exec]);
                quiet("gsettings", &["reset", schema, arg]);
            }
        }
        let Some((read, write)) = [("kreadconfig6", "kwriteconfig6"), ("kreadconfig5", "kwriteconfig5")]
            .into_iter()
            .find(|(read, _)| crate::shells::which(read).is_some())
        else {
            return Ok(());
        };
        let key = |name: &str| ["--file", "kdeglobals", "--group", "General", "--key", name].map(str::to_owned);
        let now = quiet(read, &key("TerminalApplication").each_ref().map(String::as_str)).unwrap_or_default();
        for (name, value) in [("TerminalApplication", exe.as_str()), ("TerminalService", DESKTOP_ID)] {
            let mut args = key(name).to_vec();
            if on {
                args.push(value.to_owned());
            } else if now == exe {
                args.push("--delete".to_owned());
            } else {
                continue;
            }
            quiet(write, &args.iter().map(String::as_str).collect::<Vec<_>>());
        }
        Ok(())
    }

    pub const DETAIL: &str = "Apps and the desktop open their terminal windows here";
}

// ---- Haiku ---------------------------------------------------------------

#[cfg(target_os = "haiku")]
mod system {
    pub fn install() {}
    pub fn is_default() -> Option<bool> {
        None
    }
    pub fn set_default(_on: bool) -> Result<(), String> {
        Err("Haiku has no default terminal to set".to_owned())
    }
    pub const DETAIL: &str = "";
}

pub use system::{install, is_default, set_default, DETAIL};

#[cfg(test)]
mod tests {
    use super::*;

    fn command(url: &str) -> Option<String> {
        from_url(url).and_then(|open| open.command)
    }

    #[test]
    fn links_become_commands() {
        assert_eq!(command("ssh://me@example.com").as_deref(), Some("ssh me@example.com"));
        assert_eq!(command("ssh://example.com:2222/").as_deref(), Some("ssh -p 2222 example.com"));
        assert_eq!(command("ssh://me;fingerprint=SHA256-abc@[::1]:22").as_deref(), Some("ssh -p 22 me@::1"));
        assert_eq!(command("telnet://bbs.example.org:23").as_deref(), Some("telnet bbs.example.org 23"));
        assert_eq!(command("x-man-page://ls").as_deref(), Some("man ls"));
        assert_eq!(command("x-man-page://3/printf").as_deref(), Some("man 3 printf"));
    }

    #[test]
    fn hostile_links_open_nothing() {
        for url in [
            "ssh://-oProxyCommand=touch%20x@host",
            "ssh://user@-oProxyCommand=x",
            "ssh://host;rm -rf ~",
            "ssh://host:22;id",
            "ssh://ho$(id)st",
            "telnet://host:notaport",
            "x-man-page://-P cat/ls",
            "x-man-page://1/ls;id",
            "http://example.com",
            "ssh://",
        ] {
            assert_eq!(from_url(url), None, "{url}");
        }
    }

    #[test]
    fn files_run_and_folders_open() {
        let open = from_file("/Users/me/My Scripts/build it.command");
        assert_eq!(open.command.as_deref(), Some("'/Users/me/My Scripts/build it.command'"));
        assert_eq!(from_file("/tmp"), Open { cwd: Some("/tmp".into()), command: None });
        assert_eq!(shell_word("it's"), r"'it'\''s'");
    }

    #[test]
    fn launch_arguments() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(from_args(&args(&[])), None);
        assert_eq!(from_args(&args(&["--new-window"])), None);
        assert_eq!(from_args(&args(&["-e", "htop -d 5"])).unwrap().command.as_deref(), Some("htop -d 5"));
        let open = from_args(&args(&["--working-directory", "/tmp", "-e", "vim", "my file.txt"])).unwrap();
        assert_eq!(open, Open { cwd: Some("/tmp".into()), command: Some("vim 'my file.txt'".into()) });
        let open = from_args(&args(&["--working-directory=/srv"])).unwrap();
        assert_eq!(open, Open { cwd: Some("/srv".into()), command: None });
        assert_eq!(open.params(), r#"{"cwd":"/srv"}"#);
    }

    #[cfg(not(any(target_os = "macos", target_os = "haiku")))]
    #[test]
    fn terminal_list_puts_temino_first() {
        assert_eq!(system::listed("", true), "temino.desktop\n");
        assert_eq!(system::listed("kitty.desktop\ntemino.desktop\n", true), "temino.desktop\nkitty.desktop\n");
        assert_eq!(system::listed("temino.desktop:new\nkitty.desktop\n", false), "kitty.desktop\n");
    }
}

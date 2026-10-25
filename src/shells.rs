//! The shells Temino can run (each has its own integration script, see
//! session.rs), where they are installed, and how to install a missing one.
//! Settings ▸ Shell picks the one new panes start; `shell=<name>` in the
//! settings file remembers it, and `TEMINO_SHELL` overrides it.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Bash,
    Zsh,
    Fish,
}

/// In the order of the menu.
pub const KINDS: [Kind; 3] = [Kind::Bash, Kind::Zsh, Kind::Fish];

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Bash => "bash",
            Kind::Zsh => "zsh",
            Kind::Fish => "fish",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        KINDS.into_iter().find(|kind| kind.name() == name)
    }

    /// The kind of shell at `path`, by its file name.
    pub fn of_path(path: &str) -> Option<Kind> {
        Kind::from_name(path.rsplit('/').next()?)
    }
}

/// Where packaged shells live, newest first: on macOS Homebrew's before the
/// system's (whose bash is 3.2), and these before $PATH, which an app
/// started from the Finder doesn't have.
fn search_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = ["/opt/homebrew/bin", "/usr/local/bin"].map(String::from).to_vec();
    if let Ok(path) = std::env::var("PATH") {
        dirs.extend(path.split(':').filter(|dir| !dir.is_empty()).map(String::from));
    }
    dirs.extend(["/usr/bin", "/bin", "/boot/system/bin", "/boot/home/config/non-packaged/bin"].map(String::from));
    dirs
}

/// The installed shell of this kind, if any.
pub fn find(kind: Kind) -> Option<String> {
    search_dirs()
        .into_iter()
        .map(|dir| format!("{dir}/{}", kind.name()))
        .find(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file()))
}

/// The shell new panes start: TEMINO_SHELL, else the one picked in
/// Settings ▸ Shell, else bash.
pub fn default_shell() -> String {
    if let Ok(shell) = std::env::var("TEMINO_SHELL") {
        return shell;
    }
    crate::setting("shell")
        .and_then(|name| Kind::from_name(&name))
        .and_then(find)
        .or_else(|| find(Kind::Bash))
        .unwrap_or_else(|| "/bin/bash".into())
}

/// The command that installs a shell of this kind, or why there is none.
pub fn install_command(kind: Kind) -> Result<String, String> {
    let name = kind.name();
    if cfg!(target_os = "haiku") {
        return Ok(format!("pkgman install -y {name}"));
    }
    let tool = |names: &[&str]| {
        names.iter().find_map(|tool| {
            ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
                .iter()
                .map(|dir| format!("{dir}/{tool}"))
                .find(|path| std::path::Path::new(path).exists())
        })
    };
    if cfg!(target_os = "macos") {
        return tool(&["brew"])
            .map(|brew| format!("{brew} install {name}"))
            .ok_or_else(|| format!("Installing {name} needs Homebrew (brew.sh)"));
    }
    let managers = [
        ("apt-get", "sudo apt-get install -y"),
        ("dnf", "sudo dnf install -y"),
        ("pacman", "sudo pacman -S --noconfirm"),
        ("zypper", "sudo zypper install -y"),
        ("apk", "sudo apk add"),
    ];
    managers
        .iter()
        .find(|(tool_name, _)| tool(&[tool_name]).is_some())
        .map(|(_, install)| format!("{install} {name}"))
        .ok_or_else(|| format!("No package manager found to install {name}"))
}

/// Settings ▸ Shell's entries (missing shells offer to install), and the
/// index of `current` among them.
pub fn menu(current: &str) -> (Vec<String>, usize) {
    let names = KINDS
        .into_iter()
        .map(|kind| match find(kind) {
            Some(_) => kind.name().to_owned(),
            None => format!("Install {}…", kind.name()),
        })
        .collect();
    let selected = Kind::of_path(current).and_then(|kind| KINDS.iter().position(|k| *k == kind));
    (names, selected.unwrap_or(usize::MAX))
}

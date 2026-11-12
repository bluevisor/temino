//! The system-info "fetch" a new tab can open with: fastfetch, neofetch or
//! haikufetch. Settings ▸ Shell picks one (`fetch=<name>` in the settings
//! file, `off` or unset for none), installs a missing one, and opens its
//! config file.

use std::process::{Command, Stdio};

use crate::shells;

/// One fetch program.
pub struct Fetch {
    pub name: &'static str,
    /// Its config file, under the config directory `dirs` gives it.
    config: &'static str,
    /// Makes the default config when there is none yet.
    gen_config: &'static [&'static str],
    /// Installed with cargo from this repository, not a package manager.
    cargo_git: Option<&'static str>,
    /// Keeps its config in Haiku's own ~/config/settings rather than
    /// ~/.config.
    haiku_settings: bool,
    /// Older names it went by: a `fetch=` setting naming one still picks it,
    /// and an installed copy runs until this one is installed.
    old_names: &'static [&'static str],
    /// The PPA to add first where apt doesn't have it (older Ubuntus).
    ppa: Option<&'static str>,
}

/// In the order of the menu, after Off.
pub const FETCHES: [Fetch; 3] = [
    Fetch {
        name: "fastfetch",
        config: "fastfetch/config.jsonc",
        gen_config: &["--gen-config"],
        cargo_git: None,
        haiku_settings: false,
        old_names: &[],
        ppa: Some("ppa:zhangsongcui3371/fastfetch"),
    },
    // neofetch writes its config the first time it runs.
    Fetch {
        name: "neofetch",
        config: "neofetch/config.conf",
        gen_config: &[],
        cargo_git: None,
        haiku_settings: false,
        old_names: &[],
        ppa: None,
    },
    // haikufetch runs on defaults until asked to write its config. It was
    // called rustfetch before.
    Fetch {
        name: "haikufetch",
        config: "haikufetch/config.toml",
        gen_config: &["--init-config"],
        cargo_git: Some("https://github.com/bluevisor/haikufetch"),
        haiku_settings: true,
        old_names: &["rustfetch"],
        ppa: None,
    },
];

/// The fetch picked in Settings, as an index into FETCHES.
pub fn chosen() -> Option<usize> {
    let name = crate::setting("fetch")?;
    FETCHES.iter().position(|fetch| fetch.name == name || fetch.old_names.contains(&name.as_str()))
}

/// Which of FETCHES are installed (under their current names: an old copy
/// still offers the install).
pub fn installed() -> [bool; FETCHES.len()] {
    FETCHES.each_ref().map(|fetch| shells::which(fetch.name).is_some())
}

/// The command a new tab runs: the picked fetch, if it's installed, or a
/// copy under an older name until it is.
pub fn command() -> Option<String> {
    let fetch = &FETCHES[chosen()?];
    std::iter::once(&fetch.name)
        .chain(fetch.old_names)
        .find(|name| shells::which(name).is_some())
        .map(|name| (*name).to_owned())
}

impl Fetch {
    /// The command that installs it, or why there is none.
    pub fn install_command(&self) -> Result<String, String> {
        match self.cargo_git {
            Some(repo) => match shells::which("cargo") {
                Some(cargo) => Ok(format!("{cargo} install --locked --git {repo}")),
                None => Err(format!("Installing {} needs Rust's cargo (rustup.rs)", self.name)),
            },
            None => {
                let command = shells::install_package(self.name)?;
                match self.ppa {
                    Some(ppa) if command.starts_with("sudo apt-get") && !apt_has(self.name) => {
                        Ok(format!("sudo add-apt-repository -y {ppa} && sudo apt-get update && {command}"))
                    }
                    _ => Ok(command),
                }
            }
        }
    }

    /// Its config file, made with the defaults first if there is none.
    pub fn config_file(&self) -> Result<String, String> {
        let path = format!("{}/{}", config_dir(self.haiku_settings), self.config);
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        let program = shells::which(self.name).ok_or_else(|| format!("{} isn't installed", self.name))?;
        let _ = Command::new(program)
            .args(self.gen_config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if std::path::Path::new(&path).exists() {
            Ok(path)
        } else {
            Err(format!("{} made no config at {path}", self.name))
        }
    }
}

/// $XDG_CONFIG_HOME or ~/.config, where fastfetch and neofetch look on
/// every system and haikufetch everywhere but Haiku, where it uses
/// ~/config/settings like Haiku's own apps.
fn config_dir(haiku_settings: bool) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if haiku_settings && cfg!(target_os = "haiku") {
        return format!("{home}/config/settings");
    }
    std::env::var("XDG_CONFIG_HOME").ok().filter(|dir| !dir.is_empty()).unwrap_or_else(|| format!("{home}/.config"))
}

/// Whether apt has a package called `name` to install.
fn apt_has(name: &str) -> bool {
    Command::new("apt-cache").args(["policy", name]).stderr(Stdio::null()).output().is_ok_and(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim().strip_prefix("Candidate:").is_some_and(|candidate| candidate.trim() != "(none)"))
    })
}

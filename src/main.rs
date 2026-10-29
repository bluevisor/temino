//! Temino: a modern GPU-accelerated block terminal for Haiku, Linux and
//! macOS, rendered with Vulkan (Metal on macOS).

// Drawing helpers take a scene, the text system, a position and a style.
#![allow(clippy::too_many_arguments)]

mod agent;
mod app;
mod ask;
mod complete;
mod editor;
#[cfg(not(target_os = "macos"))]
mod gpu;
#[cfg(target_os = "macos")]
#[path = "metal.rs"]
mod gpu;
mod graphemes;
mod json;
mod keys;
mod llm;
mod pane;
mod platform;
mod scene;
mod session;
mod settings;
mod shells;
mod text;
mod theme;
mod ui;
mod update;

use std::sync::mpsc;
use std::thread;

pub enum Msg {
    Ui(platform::UiEvent),
    /// Output from the shell of the pane with this id.
    Pty(u64, Vec<u8>),
    ShellExited(u64, i32),
    /// A message for the user from another thread (the updater).
    Notice(String),
    Update(update::Event),
    /// How many files changed in a pane's repository: pane, folder, count.
    Git(u64, String, usize),
}

/// History, the shell integration script and logs: ~/config/settings/Temino
/// on Haiku, ~/Library/Application Support/Temino on macOS, and
/// $XDG_CONFIG_HOME/temino (~/.config/temino) on Linux.
pub fn settings_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into());
    if cfg!(target_os = "haiku") {
        format!("{home}/config/settings/Temino")
    } else if cfg!(target_os = "macos") {
        format!("{home}/Library/Application Support/Temino")
    } else {
        let config = std::env::var("XDG_CONFIG_HOME").ok().filter(|d| !d.is_empty());
        format!("{}/temino", config.unwrap_or_else(|| format!("{home}/.config")))
    }
}

/// `key=value` lines in the settings file: the value of `key`, if set.
pub fn setting(key: &str) -> Option<String> {
    let settings = std::fs::read_to_string(format!("{}/settings", settings_dir())).ok()?;
    let prefix = format!("{key}=");
    settings.lines().find_map(|line| line.strip_prefix(&prefix)).map(|value| value.trim().to_owned())
}

pub fn save_setting(key: &str, value: &str) {
    let path = format!("{}/settings", settings_dir());
    let prefix = format!("{key}=");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.starts_with(&prefix))
        .map(str::to_owned)
        .collect();
    lines.push(format!("{prefix}{value}"));
    let _ = std::fs::create_dir_all(settings_dir());
    let _ = std::fs::write(&path, lines.join("\n") + "\n");
}

/// Reports a fatal error the way native apps do, with an alert.
pub fn fatal(message: &str) {
    eprintln!("temino: {message}");
    let _ = std::fs::create_dir_all(settings_dir());
    let _ = std::fs::write(format!("{}/crash.log", settings_dir()), format!("{message}\n"));
    platform::alert(&format!("Temino: {message}"));
}

/// With TEMINO_TRACE set, appends a timestamped line to trace.log in the
/// settings directory (flushed, so it survives a hang).
pub fn trace(message: &str) {
    use std::io::Write;
    use std::sync::OnceLock;
    static START: OnceLock<Option<std::time::Instant>> = OnceLock::new();
    let Some(start) = START.get_or_init(|| std::env::var_os("TEMINO_TRACE").map(|_| std::time::Instant::now())) else {
        return;
    };
    let _ = std::fs::create_dir_all(settings_dir());
    let path = format!("{}/trace.log", settings_dir());
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{:9.3} {message}", start.elapsed().as_secs_f64());
    }
}

const WIDTH: u32 = 980;
const HEIGHT: u32 = 640;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(target_os = "haiku")]
    if args.get(1).map(String::as_str) == Some("--pty-exec") {
        std::process::exit(platform::pty_exec(&args[2..]));
    }
    if args.get(1).map(String::as_str) == Some("--selftest") {
        let path = args.get(2).map_or("/tmp/temino-selftest.ppm", String::as_str);
        // Optional WIDTHxHEIGHT and frame count, for soak tests.
        let (width, height) = args
            .get(3)
            .and_then(|s| s.split_once('x'))
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((480, 160));
        let frames = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(3);
        std::process::exit(app::selftest(path, width, height, frames));
    }
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        fatal(&format!("crashed: {payload}{location}"));
        platform::quit();
    }));

    let (sender, receiver) = mpsc::channel();
    platform::app_new("application/x-vnd.bluevisor-Temino");
    platform::window_new("Temino", WIDTH, HEIGHT, sender.clone());
    thread::Builder::new()
        .name("temino core".into())
        .spawn(move || app::run(receiver, sender, WIDTH, HEIGHT))
        .expect("core thread");
    platform::app_run();
    std::process::exit(0);
}

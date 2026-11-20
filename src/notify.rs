//! Desktop notifications: a program in a pane asked for one (OSC 9, 777 or
//! 99), or an agent started waiting on you or finished, while you weren't
//! looking at it. Each system shows them its own way:
//!
//! - Linux: `notify-send`, else `gdbus` straight to the notification daemon
//! - macOS: as Temino's own (src/macos/notify.m), so a click opens the pane
//!   it's about. From a dev binary, or when macOS won't let Temino post,
//!   `osascript`'s `display notification`; those are Script Editor's, and a
//!   click on one opens Script Editor.
//! - Haiku: `notify`
//!
//! `notifications=off` in the settings file (Settings ▸ Tabs) turns them off.

use std::process::{Command, Stdio};

/// Whether Settings has them on (the default).
pub fn enabled() -> bool {
    crate::setting("notifications").as_deref() != Some("off")
}

/// The pane a notification is about, in which window: where a click on it
/// goes (macOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    /// From 1.
    pub window: u32,
    pub pane: Option<u64>,
}

impl Source {
    /// The window and pane as a notification carries them: window 0 and
    /// pane `u64::MAX` for none.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn carried(source: Option<Source>) -> (u32, u64) {
        source.map_or((0, u64::MAX), |source| (source.window, source.pane.unwrap_or(u64::MAX)))
    }
    /// Back from what a clicked notification carried.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn from_carried(window: u32, pane: u64) -> Option<Source> {
        (window > 0).then_some(Source { window, pane: (pane != u64::MAX).then_some(pane) })
    }
}

/// Shows `title` and `body` as a desktop notification, in the background;
/// a click on it opens `source` where the system says so (macOS).
pub fn show(title: &str, body: &str, source: Option<Source>) {
    if !enabled() {
        return;
    }
    let title = clean(if title.is_empty() { "Temino" } else { title });
    let body = clean(body);
    let _ = std::thread::Builder::new().name("notify".into()).spawn(move || {
        #[cfg(target_os = "macos")]
        if system::post(&title, &body, source) {
            return;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = source;
        for mut command in commands(&title, &body) {
            let ran = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            if ran.is_ok_and(|status| status.success()) {
                return;
            }
        }
    });
}

/// One line each, without control characters, and not too long.
fn clean(text: &str) -> String {
    let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    text.trim().chars().take(240).collect()
}

/// What to try, in order, until one works.
fn commands(title: &str, body: &str) -> Vec<Command> {
    if cfg!(target_os = "macos") {
        let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
        let mut osascript = Command::new("osascript");
        osascript.arg("-e").arg(format!("display notification {} with title {}", quote(body), quote(title)));
        return vec![osascript];
    }
    if cfg!(target_os = "haiku") {
        let mut notify = Command::new("notify");
        notify.args(["--type", "information", "--group", "Temino", "--title", title, body]);
        return vec![notify];
    }
    let mut send = Command::new("notify-send");
    send.args(["--app-name=Temino", "--icon=utilities-terminal", "--", title, body]);
    // No libnotify tools: the daemon's D-Bus interface directly.
    let mut gdbus = Command::new("gdbus");
    gdbus.args([
        "call",
        "--session",
        "--dest=org.freedesktop.Notifications",
        "--object-path=/org/freedesktop/Notifications",
        "--method=org.freedesktop.Notifications.Notify",
        "Temino",
        "0",
        "utilities-terminal",
        title,
        body,
        "[]",
        "{}",
        "-1",
    ]);
    vec![send, gdbus]
}

/// Takes the clicks on Temino's notifications. Before the event loop runs,
/// so the click that launches Temino isn't missed.
pub fn install() {
    #[cfg(target_os = "macos")]
    system::install();
}

#[cfg(target_os = "macos")]
mod system {
    use super::Source;
    use std::ffi::{c_char, CString};

    extern "C" {
        fn temino_notify_install(clicked: extern "C" fn(u32, u64));
        fn temino_notify_post(title: *const c_char, body: *const c_char, window: u32, pane: u64) -> i32;
    }

    pub fn install() {
        unsafe { temino_notify_install(clicked) };
    }

    /// Posts it as Temino's own; false when it can't (a dev binary, or
    /// macOS won't allow it), to show it another way. The first one asks
    /// you for permission, and waits for your answer.
    pub fn post(title: &str, body: &str, source: Option<Source>) -> bool {
        let (Ok(title), Ok(body)) = (CString::new(title), CString::new(body)) else { return false };
        let (window, pane) = Source::carried(source);
        unsafe { temino_notify_post(title.as_ptr(), body.as_ptr(), window, pane) == 1 }
    }

    /// A click, on the main thread, Temino already in front: its window
    /// comes up, at its pane.
    extern "C" fn clicked(window: u32, pane: u64) {
        let Some(source) = Source::from_carried(window, pane) else { return };
        crate::platform::raise_window(source.window);
        let Some(pane) = source.pane else { return };
        // The window's core answers on its own thread; don't hold this one.
        let _ = std::thread::Builder::new().name("notify click".into()).spawn(move || {
            if let Err(error) = crate::api::ask(source.window, "focus", &format!(r#"{{"pane":{pane}}}"#)) {
                crate::store::log(&format!("notification click: {error}"));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_one_short_line() {
        assert_eq!(clean("Build\nfailed\x07 "), "Build failed");
        assert_eq!(clean(&"x".repeat(500)).len(), 240);
    }

    #[test]
    fn a_click_comes_back_to_the_pane_it_was_about() {
        let round = |source: Option<Source>| {
            let (window, pane) = Source::carried(source);
            Source::from_carried(window, pane)
        };
        let pane = Some(Source { window: 2, pane: Some(7) });
        assert_eq!(round(pane), pane);
        // Settings is pane 0 in every window: still a pane.
        let settings = Some(Source { window: 1, pane: Some(0) });
        assert_eq!(round(settings), settings);
        // A window, no pane (a script that didn't say which).
        let window = Some(Source { window: 3, pane: None });
        assert_eq!(Source::carried(window), (3, u64::MAX));
        assert_eq!(round(window), window);
        // From nowhere in particular: a click just brings Temino forward.
        assert_eq!(Source::carried(None), (0, u64::MAX));
        assert_eq!(Source::from_carried(0, 5), None);
    }
}

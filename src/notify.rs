//! Desktop notifications: a program in a pane asked for one (OSC 9, 777 or
//! 99), or an agent started waiting on you or finished, while you weren't
//! looking at it. Each system's own tool shows them, so nothing here needs
//! linking against a notification library:
//!
//! - Linux: `notify-send`, else `gdbus` straight to the notification daemon
//! - macOS: `osascript`'s `display notification`
//! - Haiku: `notify`
//!
//! `notifications=off` in the settings file (Settings ▸ Tabs) turns them off.

use std::process::{Command, Stdio};

/// Whether Settings has them on (the default).
pub fn enabled() -> bool {
    crate::setting("notifications").as_deref() != Some("off")
}

/// Shows `title` and `body` as a desktop notification, in the background.
pub fn show(title: &str, body: &str) {
    if !enabled() {
        return;
    }
    let title = clean(if title.is_empty() { "Temino" } else { title });
    let body = clean(body);
    let _ = std::thread::Builder::new().name("notify".into()).spawn(move || {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_one_short_line() {
        assert_eq!(clean("Build\nfailed\x07 "), "Build failed");
        assert_eq!(clean(&"x".repeat(500)).len(), 240);
    }
}

//! Over-the-air updates from the GitHub releases: a check at launch (once a
//! day, unless the settings file says `updates=off`) and from the menu.
//! A newer release is offered in a card (Skip, Later, Install and Relaunch);
//! installing downloads this system's build with curl, swaps it in and
//! relaunches. On Haiku, replacing the package stops the running app, so
//! that waits for quit.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Msg;

const LATEST: &str = "https://api.github.com/repos/bluevisor/temino/releases/latest";
const DAY: u64 = 24 * 60 * 60;

/// The end of this system's asset name in a release.
fn asset_suffix() -> Option<&'static str> {
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "macos") {
        Some(if arm { "-macos-arm64.dmg" } else { "-macos-x86_64.dmg" })
    } else if cfg!(target_os = "haiku") {
        // 32-bit Haiku gets the x86_gcc2 package (built for its x86 secondary arch).
        Some(if arm {
            "-arm64.hpkg"
        } else if cfg!(target_arch = "x86") {
            "-x86_gcc2.hpkg"
        } else {
            "-x86_64.hpkg"
        })
    } else if cfg!(target_os = "linux") {
        Some(if arm { "-linux-aarch64.tar.gz" } else { "-linux-x86_64.tar.gz" })
    } else {
        None
    }
}

/// A newer release with a build for this system.
#[derive(Clone)]
pub struct Release {
    pub version: String,
    tag: String,
    url: String,
    /// The download's size in bytes, and the day it was published (YYYY-MM-DD).
    pub size: u64,
    pub date: String,
}

impl Release {
    pub fn notes_url(&self) -> String {
        format!("https://github.com/bluevisor/temino/releases/tag/{}", self.tag)
    }

    /// "77.9 MB".
    pub fn size_label(&self) -> String {
        let mb = self.size as f64 / 1_000_000.0;
        if mb >= 1.0 {
            format!("{mb:.1} MB")
        } else {
            format!("{} KB", self.size.div_ceil(1000))
        }
    }

    /// "Oct 27, 2026".
    pub fn date_label(&self) -> String {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let mut parts = self.date.split('-').map(|p| p.parse::<usize>().unwrap_or(0));
        match (parts.next(), parts.next(), parts.next()) {
            (Some(year), Some(month @ 1..=12), Some(day)) => format!("{} {day}, {year}", MONTHS[month - 1]),
            _ => self.date.clone(),
        }
    }
}

/// What the updater thread tells the app.
pub enum Event {
    /// A newer release to offer.
    Available(Release),
    /// The offered release is in place: relaunch to use it.
    Installed,
    Failed(String),
}

/// Checks in the background; `manual` (the menu) skips the once-a-day
/// limit and a skipped version, and also reports "up to date" and failures.
pub fn check(sender: Sender<Msg>, manual: bool) {
    if !manual {
        if crate::setting("updates").as_deref() == Some("off") {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let last = crate::setting("update_checked").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if now.saturating_sub(last) < DAY {
            return;
        }
        crate::save_setting("update_checked", &now.to_string());
    }
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        let skipped = crate::setting("update_skipped");
        let message = match latest() {
            Ok(Some(release)) if manual || skipped.as_deref() != Some(release.version.as_str()) => {
                Msg::Update(Event::Available(release))
            }
            Ok(_) if manual => Msg::Notice(format!("Temino {} is up to date", env!("CARGO_PKG_VERSION"))),
            Err(error) if manual => Msg::Notice(format!("Update check failed: {error}")),
            _ => return,
        };
        let _ = sender.send(message);
    });
}

/// "Skip": the daily check stops offering this version.
pub fn skip(version: &str) {
    crate::save_setting("update_skipped", version);
}

/// Downloads and installs `release` in the background.
pub fn install(sender: Sender<Msg>, release: Release) {
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        let event = match download(&release) {
            Ok(()) => Event::Installed,
            Err(error) => Event::Failed(error),
        };
        let _ = sender.send(Msg::Update(event));
    });
}

/// The latest release, if it is newer than this build.
fn latest() -> Result<Option<Release>, String> {
    let suffix = asset_suffix().ok_or("no builds for this system")?;
    // TEMINO_UPDATE_URL stands in for GitHub (a file:// URL works), for testing.
    let json = curl_text(&std::env::var("TEMINO_UPDATE_URL").unwrap_or_else(|_| LATEST.to_owned()))?;
    let tag = json_strings(&json, "tag_name").into_iter().next().ok_or("no release found")?;
    let version = tag.trim_start_matches('v').to_owned();
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url = json_strings(&json, "browser_download_url")
        .into_iter()
        .find(|url| url.ends_with(suffix))
        .ok_or_else(|| format!("{tag} has no build for this system"))?;
    let size = asset_size(&json, &url).unwrap_or(0);
    let date = json_strings(&json, "published_at").into_iter().next().unwrap_or_default();
    let date = date.split('T').next().unwrap_or_default().to_owned();
    Ok(Some(Release { version, tag, url, size, date }))
}

fn download(release: &Release) -> Result<(), String> {
    let dir = PathBuf::from(format!("{}/update", crate::settings_dir()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let download = dir.join(release.url.rsplit('/').next().unwrap_or("download"));
    run_command(Command::new("curl").args(["-fsSL", "--max-time", "600", "-o"]).arg(&download).arg(&release.url))?;
    install_download(&dir, &download)
}

/// The `"size"` of the asset whose download is `url`: GitHub lists it
/// before the URL in the asset's object.
fn asset_size(json: &str, url: &str) -> Option<u64> {
    let before = &json[..json.find(&format!("\"{url}\""))?];
    let after = before[before.rfind("\"size\"")? + 6..].trim_start().strip_prefix(':')?.trim_start();
    after[..after.find(|c: char| !c.is_ascii_digit())?].parse().ok()
}

/// Whether dotted version `a` is past `b`.
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parts(a) > parts(b)
}

fn curl_text(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "30", "-H", "Accept: application/vnd.github+json", url])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("couldn't reach GitHub ({})", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run_command(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Every string value of `"key"` in `json`, enough for GitHub's answer.
fn json_strings(json: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(at) = rest.find(&needle) {
        rest = rest[at + needle.len()..].trim_start();
        let Some(value) = rest.strip_prefix(':').map(str::trim_start).and_then(|v| v.strip_prefix('"')) else {
            continue;
        };
        if let Some(end) = value.find('"') {
            out.push(value[..end].to_owned());
        }
        rest = value;
    }
    out
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| e.to_string())
}

/// Swaps Temino.app for the one in the disk image; the running app keeps
/// its files.
#[cfg(target_os = "macos")]
fn install_download(dir: &Path, dmg: &Path) -> Result<(), String> {
    let exe = current_exe()?;
    let bundle = exe
        .ancestors()
        .nth(3)
        .filter(|b| b.extension().is_some_and(|e| e == "app"))
        .ok_or("Temino isn't running from Temino.app")?;
    let parent = bundle.parent().ok_or("no folder above Temino.app")?;
    let mount = dir.join("mount");
    let attach = ["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"];
    run_command(Command::new("hdiutil").args(attach).arg(&mount).arg(dmg))?;
    let staged = parent.join(".Temino-new.app");
    let old = parent.join(".Temino-old.app");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    // Next to the old bundle, so the swap is two renames on one volume.
    let copied = run_command(Command::new("ditto").arg(mount.join("Temino.app")).arg(&staged));
    let _ = run_command(Command::new("hdiutil").args(["detach", "-force"]).arg(&mount));
    copied?;
    std::fs::rename(bundle, &old).map_err(|e| format!("can't replace {}: {e}", bundle.display()))?;
    if let Err(e) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Replaces the temino binary; the running one keeps its inode.
#[cfg(not(any(target_os = "macos", target_os = "haiku")))]
fn install_download(dir: &Path, tarball: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let exe = current_exe()?;
    run_command(Command::new("tar").arg("xzf").arg(tarball).arg("-C").arg(dir))?;
    let binary = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path().join("temino"))
        .find(|path| path.is_file())
        .ok_or("the download has no temino binary")?;
    let staged = exe.with_file_name(".temino-new");
    std::fs::copy(&binary, &staged).map_err(|e| format!("can't write {}: {e}", staged.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    std::fs::rename(&staged, &exe).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(dir);
    Ok(())
}

/// Leaves the package for `finish`.
#[cfg(target_os = "haiku")]
fn install_download(_dir: &Path, _package: &Path) -> Result<(), String> {
    if !current_exe()?.starts_with("/boot/home/config/apps") {
        return Err("only a Temino package in ~/config/packages updates itself".into());
    }
    Ok(())
}

/// At quit: on Haiku, moves a downloaded package into ~/config/packages in
/// place of the old one, from a shell that outlives the app (the swap stops
/// it). With `relaunch`, that shell then starts Temino again once this one
/// is gone.
pub fn finish(relaunch: bool) {
    let mut script = String::from("while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; ");
    if cfg!(target_os = "haiku") {
        let dir = format!("{}/update", crate::settings_dir());
        let package = std::fs::read_dir(&dir).ok().and_then(|entries| {
            entries.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "hpkg"))
        });
        let Some(package) = package else { return };
        let packages = "/boot/home/config/packages";
        // The package swap stops the app, so this doesn't wait for it.
        script = format!(
            "sleep 1; for old in {packages}/temino-*.hpkg; do rm -f \"$old\"; done; mv \"{}\" {packages}/ && rm -rf \"{dir}\"; sleep 2; ",
            package.display()
        );
    } else if !relaunch {
        return;
    }
    let Ok(exe) = current_exe() else { return };
    let bundle = exe.ancestors().nth(3).filter(|b| b.extension().is_some_and(|e| e == "app"));
    if relaunch {
        script += match bundle {
            Some(_) => "open -n \"$2\"",
            None => "\"$2\" >/dev/null 2>&1 &",
        };
    }
    let target = bundle.unwrap_or(&exe).to_owned();
    let _ = Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.3.0", "0.2.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn json() {
        let json = r#"{"tag_name": "v0.3.0", "assets": [{"browser_download_url":"https://x/a.zip"}, {"browser_download_url": "https://x/b.hpkg"}]}"#;
        assert_eq!(json_strings(json, "tag_name"), ["v0.3.0"]);
        assert_eq!(json_strings(json, "browser_download_url"), ["https://x/a.zip", "https://x/b.hpkg"]);
        let json = r#"{"assets": [{"size": 12, "browser_download_url": "https://x/a.zip"}, {"size":77912345,"browser_download_url":"https://x/b.dmg"}]}"#;
        assert_eq!(asset_size(json, "https://x/b.dmg"), Some(77912345));
        assert_eq!(asset_size(json, "https://x/a.zip"), Some(12));
    }

    #[test]
    fn labels() {
        let release = Release {
            version: "0.2.6".into(),
            tag: "v0.2.6".into(),
            url: String::new(),
            size: 77_912_345,
            date: "2026-10-27".into(),
        };
        assert_eq!(release.size_label(), "77.9 MB");
        assert_eq!(release.date_label(), "Oct 27, 2026");
    }
}

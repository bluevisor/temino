//! Over-the-air updates from the GitHub releases: a check at launch (once a
//! day, unless the settings file says `updates=off`) and from the menu.
//! A newer release's build for this system is downloaded with curl and
//! swapped in; it runs from the next launch. On Haiku, replacing the package
//! stops the running app, so that waits for quit.

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

/// Checks in the background; `manual` (the menu) skips the once-a-day
/// limit and also reports "up to date" and failures.
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
        let message = match run() {
            Ok(Some(version)) => Some(format!("Temino {version} is installed. Restart Temino to use it.")),
            Ok(None) if manual => Some(format!("Temino {} is up to date", env!("CARGO_PKG_VERSION"))),
            Err(error) if manual => Some(format!("Update failed: {error}")),
            _ => None,
        };
        if let Some(message) = message {
            let _ = sender.send(Msg::Notice(message));
        }
    });
}

/// Installs the latest release if it is newer; its version, if so.
fn run() -> Result<Option<String>, String> {
    let suffix = asset_suffix().ok_or("no builds for this system")?;
    let json = curl_text(LATEST)?;
    let tag = json_strings(&json, "tag_name").into_iter().next().ok_or("no release found")?;
    let version = tag.trim_start_matches('v').to_owned();
    if !newer(&version, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let url = json_strings(&json, "browser_download_url")
        .into_iter()
        .find(|url| url.ends_with(suffix))
        .ok_or_else(|| format!("{tag} has no build for this system"))?;
    let dir = PathBuf::from(format!("{}/update", crate::settings_dir()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let download = dir.join(url.rsplit('/').next().unwrap_or("download"));
    run_command(Command::new("curl").args(["-fsSL", "--max-time", "600", "-o"]).arg(&download).arg(&url))?;
    install(&dir, &download)?;
    Ok(Some(version))
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
fn install(dir: &Path, dmg: &Path) -> Result<(), String> {
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
fn install(dir: &Path, tarball: &Path) -> Result<(), String> {
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

/// Leaves the package for `apply_at_quit`.
#[cfg(target_os = "haiku")]
fn install(_dir: &Path, _package: &Path) -> Result<(), String> {
    if !current_exe()?.starts_with("/boot/home/config/apps") {
        return Err("only a Temino package in ~/config/packages updates itself".into());
    }
    Ok(())
}

/// On Haiku, moves a downloaded package into ~/config/packages in place of
/// the old one, from a shell that outlives the app (the swap stops it).
pub fn apply_at_quit() {
    if !cfg!(target_os = "haiku") {
        return;
    }
    let dir = format!("{}/update", crate::settings_dir());
    let Some(package) = std::fs::read_dir(&dir)
        .ok()
        .and_then(|entries| entries.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "hpkg")))
    else {
        return;
    };
    let packages = "/boot/home/config/packages";
    let script = format!(
        "sleep 1; for old in {packages}/temino-*.hpkg; do rm -f \"$old\"; done; mv \"{}\" {packages}/ && rm -rf \"{dir}\"",
        package.display()
    );
    let _ = Command::new("sh").arg("-c").arg(script).spawn();
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
    }
}

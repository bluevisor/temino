//! One anonymous usage ping a day, so we know Temino has users at all.
//! It carries a random install id, the version, the OS and the CPU
//! architecture — never commands, output, file paths, or anything tied
//! to an account. The relay hashes the id before storing it and drops
//! it after 40 days. Off when the settings file says `analytics=off`
//! (Settings ▸ Share anonymous usage); silent on failure, like the
//! updater's daily check, whose shape this mirrors.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DAY: u64 = 24 * 60 * 60;
const DEFAULT_URL: &str = "https://temino-relay.bluevisor.workers.dev/analytics";

/// Sends today's ping in the background, at most once a day.
pub fn ping() {
    if crate::setting("analytics").as_deref() == Some("off") {
        return;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let last = crate::setting("analytics_sent").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    if now.saturating_sub(last) < DAY {
        return;
    }
    crate::save_setting("analytics_sent", &now.to_string());
    let id = install_id();
    let _ = std::thread::Builder::new().name("analytics".into()).spawn(move || {
        let url = std::env::var("TEMINO_ANALYTICS_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned());
        let _ = post(&url, &payload(&id, env!("CARGO_PKG_VERSION"), os_name(), std::env::consts::ARCH));
    });
}

/// This install's random id, made and remembered on first use.
fn install_id() -> String {
    if let Some(id) = crate::setting("analytics_id").filter(|id| valid_id(id)) {
        return id;
    }
    let id = new_id();
    crate::save_setting("analytics_id", &id);
    id
}

/// A ping id: 32 lowercase hex characters, nothing else.
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 16 random bytes as hex. /dev/urandom where the system has one; the
/// fallback stirs the clock and the pid, which is enough for an id
/// nobody should be able to predict wanting to.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok();
    if !read {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let mut state = (nanos as u64) ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for byte in &mut bytes {
            // xorshift64*: cheap, and only a fallback.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The OS name the relay expects.
fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "haiku") {
        "haiku"
    } else {
        "linux"
    }
}

/// The whole payload: these four fields, and no others.
fn payload(id: &str, version: &str, os: &str, arch: &str) -> String {
    format!("{{\"id\":\"{id}\",\"version\":\"{version}\",\"os\":\"{os}\",\"arch\":\"{arch}\"}}")
}

/// POSTs `body` as JSON with curl, the config on its stdin (the same
/// pattern as llm.rs), so nothing shows in the process list.
fn post(url: &str, body: &str) -> Result<(), String> {
    let quoted = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let config =
        format!("url = {}\nheader = \"Content-Type: application/json\"\ndata-binary = {}\n", quoted(url), quoted(body));
    let mut child = Command::new("curl")
        .args(["-fsS", "--max-time", "15", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(config.as_bytes());
    }
    let output = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payload_is_exactly_the_four_fields() {
        let body = payload("0123456789abcdef0123456789abcdef", "0.2.12", "macos", "aarch64");
        assert_eq!(
            body,
            "{\"id\":\"0123456789abcdef0123456789abcdef\",\"version\":\"0.2.12\",\"os\":\"macos\",\"arch\":\"aarch64\"}"
        );
        // Nothing else rides along: no room for a fifth field to sneak in.
        assert_eq!(body.matches('"').count(), 16);
        assert!(!body.contains("command") && !body.contains("path"));
    }

    #[test]
    fn the_os_name_is_one_the_relay_accepts() {
        assert!(["macos", "linux", "haiku"].contains(&os_name()));
    }

    #[test]
    fn a_made_id_passes_and_lookalikes_do_not() {
        let id = new_id();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("0123456789abcdef0123456789abcde")); // 31 chars
        assert!(!valid_id("0123456789ABCDEF0123456789abcdef")); // uppercase
        assert!(!valid_id("0123456789abcdef0123456789abcdeg")); // not hex
        assert!(!valid_id(""));
    }
}

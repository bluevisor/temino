//! Anonymous usage analytics, behind an explicit yes.
//!
//! The first launch of a build with analytics shows a card (Help
//! improve Temino) asking permission. Until the user says yes, nothing
//! is counted and nothing is sent — there is no backfill of usage from
//! before the answer. After a yes, Temino counts how its features get
//! used and sends the counts once a day: a random install id, the
//! version, the OS, the CPU architecture, the default shell kind and
//! the GPU backend, plus aggregate counters (launches, commands
//! finished, tabs opened, …). Never commands, terminal content, file
//! paths, or anything tied to an account: counters come from a fixed
//! list in code (`Counter`), so free text can't ride along. The relay
//! hashes the install id before storing it and drops it after 40
//! days. Settings ▸ Share anonymous usage data changes the answer at
//! any time; saying no stops counting and clears what's pending.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Locked;

const DAY: u64 = 24 * 60 * 60;
const DEFAULT_URL: &str = "https://temino-relay.bluevisor.workers.dev/analytics";
const COUNTS_FILE: &str = "analytics-counts";

/// The user's answer to the permission card, from the settings file:
/// unset is not-yet-asked, and counts as no for everything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Consent {
    Undecided,
    On,
    Off,
}

/// The answer a settings value means (pure, so it can be tested).
fn consent_of(value: Option<&str>) -> Consent {
    match value {
        Some("on") => Consent::On,
        Some("off") => Consent::Off,
        _ => Consent::Undecided,
    }
}

pub fn consent() -> Consent {
    consent_of(crate::setting("analytics").as_deref())
}

/// Whether counting and sending may happen at all. The one gate every
/// `bump` and the ping itself pass through.
pub fn enabled() -> bool {
    consent() == Consent::On
}

/// "Share": remember the yes, make the install id now (not before),
/// and let counting start from this moment.
pub fn grant() {
    crate::save_setting("analytics", "on");
    install_id();
}

/// "No thanks", or the Settings switch turned off: remember the no,
/// stop counting, and clear what's pending — counts and the id.
pub fn deny() {
    crate::save_setting("analytics", "off");
    clear_counts();
    crate::save_setting("analytics_id", "");
    crate::save_setting("analytics_sent", "");
}

/// The Settings switch: on is a grant, off is a denial.
pub fn set_enabled(on: bool) {
    if on {
        grant();
    } else {
        deny();
    }
}

/// One thing Temino counts. The fixed list is the privacy contract:
/// `bump` takes this enum, so no call site can invent a name or send
/// text. The names are what the relay accepts — keep the two in step
/// (relay/src/analytics.js).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Counter {
    Launches,
    ActiveMinutes,
    CommandsCompleted,
    BlocksCreated,
    TabsOpened,
    PanesOpened,
    AiQueries,
    RemoteSessions,
    ViewerOpens,
    FindUses,
    UpdateInstalls,
    SettingsChanges,
    Crashes,
    UpdateFailures,
}

impl Counter {
    pub const ALL: [Counter; 14] = [
        Counter::Launches,
        Counter::ActiveMinutes,
        Counter::CommandsCompleted,
        Counter::BlocksCreated,
        Counter::TabsOpened,
        Counter::PanesOpened,
        Counter::AiQueries,
        Counter::RemoteSessions,
        Counter::ViewerOpens,
        Counter::FindUses,
        Counter::UpdateInstalls,
        Counter::SettingsChanges,
        Counter::Crashes,
        Counter::UpdateFailures,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Counter::Launches => "launches",
            Counter::ActiveMinutes => "active_minutes",
            Counter::CommandsCompleted => "commands_completed",
            Counter::BlocksCreated => "blocks_created",
            Counter::TabsOpened => "tabs_opened",
            Counter::PanesOpened => "panes_opened",
            Counter::AiQueries => "ai_queries",
            Counter::RemoteSessions => "remote_sessions",
            Counter::ViewerOpens => "viewer_opens",
            Counter::FindUses => "find_uses",
            Counter::UpdateInstalls => "update_installs",
            Counter::SettingsChanges => "settings_changes",
            Counter::Crashes => "crashes",
            Counter::UpdateFailures => "update_failures",
        }
    }

    fn from_name(name: &str) -> Option<Counter> {
        Counter::ALL.into_iter().find(|c| c.name() == name)
    }
}

static COUNTS_LOCK: Mutex<()> = Mutex::new(());

/// Adds one to a counter, persisted in the settings directory. A
/// no-op until the user has said yes (and after they say no), so no
/// call site has to think about consent.
pub fn bump(counter: Counter) {
    if !enabled() {
        return;
    }
    let _guard = COUNTS_LOCK.locked();
    let mut counts = load_counts();
    match counts.iter_mut().find(|(c, _)| *c == counter) {
        Some((_, n)) => *n = n.saturating_add(1),
        None => counts.push((counter, 1)),
    }
    save_counts(&counts);
}

fn counts_path() -> String {
    format!("{}/{COUNTS_FILE}", crate::settings_dir())
}

/// The counts file as text: one `name=count` line per counter (pure).
fn counts_text(counts: &[(Counter, u64)]) -> String {
    let mut out = String::new();
    for counter in Counter::ALL {
        if let Some((_, n)) = counts.iter().find(|(c, _)| *c == counter) {
            if *n > 0 {
                out.push_str(&format!("{}={n}\n", counter.name()));
            }
        }
    }
    out
}

/// Reads `counts_text` back; unknown names and bad numbers are dropped.
fn parse_counts(text: &str) -> Vec<(Counter, u64)> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            let counter = Counter::from_name(name)?;
            let n: u64 = value.trim().parse().ok()?;
            (n > 0).then_some((counter, n))
        })
        .collect()
}

fn load_counts() -> Vec<(Counter, u64)> {
    std::fs::read_to_string(counts_path()).map_or_else(|_| Vec::new(), |text| parse_counts(&text))
}

fn save_counts(counts: &[(Counter, u64)]) {
    crate::create_private_dir(&crate::settings_dir());
    crate::write_private(&counts_path(), counts_text(counts));
}

fn clear_counts() {
    let _guard = COUNTS_LOCK.locked();
    let _ = std::fs::remove_file(counts_path());
}

/// Takes back what a successful ping sent: subtracts the snapshot, so
/// counts bumped while the ping was in flight survive.
fn forget_counts(sent: &[(Counter, u64)]) {
    let _guard = COUNTS_LOCK.locked();
    let mut counts = load_counts();
    for (counter, n) in sent {
        if let Some((_, have)) = counts.iter_mut().find(|(c, _)| c == counter) {
            *have = have.saturating_sub(*n);
        }
    }
    save_counts(&counts);
}

/// Sends the accumulated counts in the background, at most once a day,
/// and only with consent. Throttled by the `analytics_sent` timestamp,
/// like the updater's daily check, whose shape this mirrors. Counts
/// are only forgotten after a send the relay accepted.
pub fn ping() {
    if !enabled() {
        return;
    }
    bump(Counter::Launches);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let last = crate::setting("analytics_sent").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    if now.saturating_sub(last) < DAY {
        return;
    }
    let Some(arch) = arch_name() else { return };
    crate::save_setting("analytics_sent", &now.to_string());
    let id = install_id();
    let sent = {
        let _guard = COUNTS_LOCK.locked();
        load_counts()
    };
    let body = payload(&id, env!("CARGO_PKG_VERSION"), os_name(), arch, shell_name().as_deref(), gpu_name(), &sent);
    let _ = std::thread::Builder::new().name("analytics".into()).spawn(move || {
        let url = std::env::var("TEMINO_ANALYTICS_URL").unwrap_or_else(|_| DEFAULT_URL.to_owned());
        if post(&url, &body).is_ok() {
            forget_counts(&sent);
        }
    });
}

/// This install's random id, made and remembered on first use. Only
/// called with consent (from `grant` and `ping`), never before.
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

/// The CPU architecture the relay expects (its ARCHES list, mirrored),
/// or None on one it doesn't know: a ping from there would be refused
/// every day for good, so none is sent and the counts stay pending,
/// rather than hammering a guaranteed refusal.
fn arch_name() -> Option<&'static str> {
    match std::env::consts::ARCH {
        arch @ ("aarch64" | "x86_64" | "x86") => Some(arch),
        _ => None,
    }
}

/// The GPU backend the relay expects: Metal on macOS, Vulkan elsewhere.
fn gpu_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "metal"
    } else {
        "vulkan"
    }
}

/// The default shell's kind name (`bash` / `zsh` / `fish`): the
/// Settings choice when there is one, else the login shell's. Never a
/// path — only the kind's name from shells::KINDS.
fn shell_name() -> Option<String> {
    if let Some(name) = crate::setting("shell").filter(|name| crate::shells::KINDS.iter().any(|k| k.name() == name)) {
        return Some(name);
    }
    let shell = std::env::var("SHELL").ok()?;
    crate::shells::Kind::of_path(&shell).map(|kind| kind.name().to_owned())
}

/// The whole payload: the environment fields and the counters, and no
/// others. `shell` rides along only when its kind is known.
fn payload(
    id: &str,
    version: &str,
    os: &str,
    arch: &str,
    shell: Option<&str>,
    gpu: &str,
    counts: &[(Counter, u64)],
) -> String {
    let mut out = format!("{{\"id\":\"{id}\",\"version\":\"{version}\",\"os\":\"{os}\",\"arch\":\"{arch}\"");
    if let Some(shell) = shell {
        out.push_str(&format!(",\"shell\":\"{shell}\""));
    }
    out.push_str(&format!(",\"gpu\":\"{gpu}\",\"counters\":{{"));
    let mut first = true;
    for counter in Counter::ALL {
        if let Some((_, n)) = counts.iter().find(|(c, _)| *c == counter) {
            if *n > 0 {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("\"{}\":{n}", counter.name()));
            }
        }
    }
    out.push_str("}}");
    out
}

/// POSTs `body` as JSON with curl, the config on its stdin (the same
/// pattern as llm.rs), so nothing shows in the process list. `-f`
/// makes an HTTP error a failure, so counts survive a refused ping.
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
    fn consent_comes_from_the_setting_and_unset_is_no() {
        assert_eq!(consent_of(Some("on")), Consent::On);
        assert_eq!(consent_of(Some("off")), Consent::Off);
        assert_eq!(consent_of(None), Consent::Undecided);
        assert_eq!(consent_of(Some("later")), Consent::Undecided);
        // The gate itself: only an explicit yes enables anything.
        assert!(consent_of(Some("on")) == Consent::On);
        assert!(consent_of(Some("off")) != Consent::On);
        assert!(consent_of(None) != Consent::On);
    }

    #[test]
    fn counter_names_roundtrip_and_stay_on_the_list() {
        for counter in Counter::ALL {
            assert_eq!(Counter::from_name(counter.name()), Some(counter));
        }
        assert_eq!(Counter::from_name("commands"), None);
        assert_eq!(Counter::from_name("file_paths"), None);
        // The list the relay mirrors (relay/src/analytics.js).
        let names: Vec<&str> = Counter::ALL.map(|c| c.name()).into_iter().collect();
        assert_eq!(
            names,
            [
                "launches",
                "active_minutes",
                "commands_completed",
                "blocks_created",
                "tabs_opened",
                "panes_opened",
                "ai_queries",
                "remote_sessions",
                "viewer_opens",
                "find_uses",
                "update_installs",
                "settings_changes",
                "crashes",
                "update_failures",
            ]
        );
    }

    #[test]
    fn counts_survive_a_roundtrip_and_drop_unknowns() {
        let counts = vec![(Counter::Launches, 2), (Counter::AiQueries, 5)];
        assert_eq!(counts_text(&counts), "launches=2\nai_queries=5\n");
        assert_eq!(parse_counts(&counts_text(&counts)), counts);
        assert_eq!(parse_counts("typed_text=9\nlaunches=x\nlaunches=3\n"), [(Counter::Launches, 3)]);
        assert_eq!(parse_counts(""), Vec::new());
    }

    #[test]
    fn the_payload_is_exactly_the_agreed_fields() {
        let counts = vec![(Counter::CommandsCompleted, 12), (Counter::Launches, 1)];
        let body =
            payload("0123456789abcdef0123456789abcdef", "0.2.12", "macos", "aarch64", Some("zsh"), "metal", &counts);
        assert_eq!(
            body,
            "{\"id\":\"0123456789abcdef0123456789abcdef\",\"version\":\"0.2.12\",\"os\":\"macos\",\"arch\":\"aarch64\",\"shell\":\"zsh\",\"gpu\":\"metal\",\"counters\":{\"launches\":1,\"commands_completed\":12}}"
        );
        // An unknown shell kind is left out rather than sent.
        let body = payload("0123456789abcdef0123456789abcdef", "0.2.12", "linux", "x86_64", None, "vulkan", &[]);
        assert!(body.ends_with("\"gpu\":\"vulkan\",\"counters\":{}}"), "{body}");
        assert!(!body.contains("shell"));
    }

    #[test]
    fn the_environment_names_are_ones_the_relay_accepts() {
        assert!(["macos", "linux", "haiku"].contains(&os_name()));
        assert!(["metal", "vulkan"].contains(&gpu_name()));
        // An architecture it doesn't know means no ping at all.
        if let Some(arch) = arch_name() {
            assert!(["aarch64", "x86_64", "x86"].contains(&arch));
        }
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

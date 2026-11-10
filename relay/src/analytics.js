// The anonymous usage ping: what a ping may say, how an install id is
// hidden before it is stored, and how the stored days add up. Pure on
// purpose — index.js does the KV and HTTP, this file only decides, so
// it can be tested on its own (test/analytics.mjs).
//
// A ping is environment fields and counters, from fixed lists, and
// nothing else: a random install id, the version, the OS, the CPU
// architecture, the default shell kind, the GPU backend, and counts of
// how features get used. Extra fields and unknown counters are
// rejected outright, so nothing else can be stored: no commands, no
// terminal content, no file paths, no account.

export const OSES = ["macos", "linux", "haiku"];
export const ARCHES = ["aarch64", "x86_64", "x86"];
export const SHELLS = ["bash", "zsh", "fish"];
export const GPUS = ["vulkan", "metal"];
export const RETENTION_DAYS = 40;

/// The counters a ping may report — the client's list (src/analytics.rs
/// `Counter`), mirrored. Anything else is refused.
export const COUNTERS = [
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
];

const ID = /^[0-9a-f]{32}$/;
const VERSION = /^[0-9][0-9A-Za-z.\-+]*$/;
const MAX_COUNTER = 1_000_000;

/// Counters as sent, or null: a plain object whose keys are all on the
/// list and whose values are whole numbers in range.
export function validCounters(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const out = {};
  for (const [name, n] of Object.entries(value)) {
    if (!COUNTERS.includes(name)) return null;
    if (!Number.isInteger(n) || n < 0 || n > MAX_COUNTER) return null;
    out[name] = n;
  }
  return out;
}

/// A parsed ping body, or null when it says anything unexpected.
/// `shell` is the only optional field (absent when its kind is unknown
/// on the client); everything else must be there, and nothing else may.
export function validPing(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const keys = Object.keys(value).sort().join(",");
  if (keys !== "arch,counters,gpu,id,os,version" && keys !== "arch,counters,gpu,id,os,shell,version") return null;
  const { id, version, os, arch, shell, gpu } = value;
  if (typeof id !== "string" || !ID.test(id)) return null;
  if (typeof version !== "string" || version.length > 16 || !VERSION.test(version)) return null;
  if (!OSES.includes(os) || !ARCHES.includes(arch)) return null;
  if (shell !== undefined && !SHELLS.includes(shell)) return null;
  if (!GPUS.includes(gpu)) return null;
  const counters = validCounters(value.counters);
  if (!counters) return null;
  return { id, version, os, arch, shell: shell ?? null, gpu, counters };
}

/// The day a ping counts for, in UTC: "2026-11-10".
export function dayKey(date) {
  return date.toISOString().slice(0, 10);
}

/// The id as stored: HMAC-SHA256 with the relay secret, hex. The raw
/// install id never reaches KV, and the stored hash can't be turned
/// back into it (or matched against an id from anywhere else).
export async function hashId(secret, id) {
  const key = await crypto.subtle.importKey(
    "raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"],
  );
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(id)));
  return [...mac].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/// `count` days ending at `today` ("YYYY-MM-DD"), oldest first.
export function lastDays(today, count) {
  const days = [];
  const at = new Date(`${today}T00:00:00Z`);
  for (let i = count - 1; i >= 0; i--) {
    const day = new Date(at);
    day.setUTCDate(day.getUTCDate() - i);
    days.push(dayKey(day));
  }
  return days;
}

const round2 = (n) => Math.round(n * 100) / 100;

/// Adds up stored pings. `records` is one entry per stored install-day:
/// { day, hash, os, version, counters } — os/version/counters are only
/// read for today's entries, so the caller only fetches those values.
/// `aggregates` is { day: { counter: total } }, the summed counters
/// per day across installs. Returns the distinct installs per day for
/// the last 30 days, the counter totals per day, and today's splits,
/// averages and returning count.
export function summarize(records, aggregates, today) {
  const window = lastDays(today, 30);
  const days = {};
  const counters = {};
  for (const day of window) {
    days[day] = 0;
    counters[day] = aggregates[day] || {};
  }
  const seenBefore = new Set();
  for (const record of records) {
    if (record.day < today) seenBefore.add(record.hash);
  }
  const byOs = {};
  const byVersion = {};
  let total = 0;
  let returning = 0;
  let commands = 0;
  let minutes = 0;
  let ai = 0;
  let remote = 0;
  for (const record of records) {
    if (record.day in days) days[record.day] += 1;
    if (record.day !== today) continue;
    total += 1;
    if (seenBefore.has(record.hash)) returning += 1;
    if (record.os) byOs[record.os] = (byOs[record.os] || 0) + 1;
    if (record.version) byVersion[record.version] = (byVersion[record.version] || 0) + 1;
    const c = record.counters || {};
    commands += c.commands_completed || 0;
    minutes += c.active_minutes || 0;
    if (c.ai_queries > 0) ai += 1;
    if (c.remote_sessions > 0) remote += 1;
  }
  const pct = (n) => (total ? round2((n / total) * 100) : 0);
  return {
    days,
    counters,
    today: {
      total,
      returning,
      byOs,
      byVersion,
      averages: {
        commandsPerInstall: total ? round2(commands / total) : 0,
        activeMinutesPerInstall: total ? round2(minutes / total) : 0,
        aiAdoptionPct: pct(ai),
        remoteAdoptionPct: pct(remote),
      },
    },
  };
}

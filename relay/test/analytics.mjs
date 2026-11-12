// The analytics ping's rules: what a ping may say, how ids are hashed,
// and how stored days add up. Run: node test/analytics.mjs
import assert from "node:assert/strict";
import { COUNTERS, dayKey, hashId, lastDays, recordPing, summarize, validCounters, validPing } from "../src/analytics.js";

const id = "0123456789abcdef0123456789abcdef";
const ping = { id, version: "0.2.12", os: "macos", arch: "aarch64", shell: "zsh", gpu: "metal", counters: { launches: 1, commands_completed: 42 } };
assert.deepEqual(validPing(ping), { ...ping });
const noShell = { ...ping };
delete noShell.shell;
assert.equal(validPing(noShell).shell, null);
assert.equal(validPing({ ...ping, counters: {} }).counters !== null, true);

// Anything unexpected is refused: extra fields, free text, bad shapes.
assert.equal(validPing({ ...ping, typed_text: "ls -la" }), null);
assert.equal(validPing({ ...ping, shell: "powershell" }), null);
assert.equal(validPing({ ...ping, shell: "/bin/zsh" }), null); // a kind name, never a path
assert.equal(validPing({ ...ping, gpu: "directx" }), null);
assert.equal(validPing({ ...ping, os: "windows" }), null);
assert.equal(validPing({ ...ping, id: "me@example.com" }), null);
assert.equal(validPing({ ...ping, version: "0.2.12; rm" }), null);
assert.equal(validPing(null), null);
assert.equal(validPing([ping]), null);

// Counters: the fixed list, whole numbers, in range — nothing else.
assert.deepEqual(validCounters({ launches: 0, active_minutes: 1_000_000 }), { launches: 0, active_minutes: 1_000_000 });
assert.equal(validCounters({ keystrokes: 10 }), null);
assert.equal(validCounters({ launches: 1_000_001 }), null);
assert.equal(validCounters({ launches: -1 }), null);
assert.equal(validCounters({ launches: 1.5 }), null);
assert.equal(validCounters({ launches: "3" }), null);
assert.equal(COUNTERS.length, 14);

// The stored id is a hash: stable for one secret+id, useless without it.
const hashed = await hashId("secret", id);
assert.match(hashed, /^[0-9a-f]{64}$/);
assert.equal(hashed, await hashId("secret", id));
assert.notEqual(hashed, await hashId("other secret", id));
assert.notEqual(hashed, await hashId("secret", "fedcba9876543210fedcba9876543210"));

assert.equal(dayKey(new Date("2026-11-10T23:59:59Z")), "2026-11-10");
assert.deepEqual(lastDays("2026-11-10", 3), ["2026-11-08", "2026-11-09", "2026-11-10"]);
assert.equal(lastDays("2026-11-10", 30).length, 30);

// Summaries: per-day installs, the day's counter totals, today's splits,
// averages, adoption, and who came back.
const today = "2026-11-10";
const records = [
  { day: "2026-11-09", hash: "a", os: null, version: null, counters: {} },
  { day: today, hash: "a", os: "macos", version: "0.2.12", counters: { commands_completed: 30, active_minutes: 90, ai_queries: 2 } },
  { day: today, hash: "b", os: "linux", version: "0.2.12", counters: { commands_completed: 10, active_minutes: 30, remote_sessions: 1 } },
  { day: "2026-10-01", hash: "z", os: null, version: null, counters: {} }, // outside the window
];
const aggregates = { [today]: { commands_completed: 40, launches: 5 }, "2026-11-09": { launches: 2 } };
const summary = summarize(records, aggregates, today);
assert.equal(Object.keys(summary.days).length, 30);
assert.equal(summary.days[today], 2);
assert.equal(summary.days["2026-11-09"], 1);
assert.deepEqual(summary.counters[today], { commands_completed: 40, launches: 5 });
assert.deepEqual(summary.counters["2026-11-09"], { launches: 2 });
assert.equal(summary.today.total, 2);
assert.equal(summary.today.returning, 1); // "a" was here yesterday, "b" is new
assert.deepEqual(summary.today.byOs, { macos: 1, linux: 1 });
assert.deepEqual(summary.today.byVersion, { "0.2.12": 2 });
assert.deepEqual(summary.today.averages, {
  commandsPerInstall: 20,
  activeMinutesPerInstall: 60,
  aiAdoptionPct: 50,
  remoteAdoptionPct: 50,
});

// An empty store still answers, with zeroes.
const empty = summarize([], {}, today);
assert.equal(empty.today.total, 0);
assert.equal(empty.today.averages.aiAdoptionPct, 0);

// Recording: a stand-in store with the slice of KV recordPing uses.
const kv = new Map();
const store = {
  get: async (key) => kv.get(key) ?? null,
  put: async (key, value) => { kv.set(key, value); },
};
const aggKey = `analytics:agg:${today}`;
const totalsOf = async () => JSON.parse(await store.get(aggKey)) || {};

// One install pinging twice in a day — its counters grew between the
// pings — leaves the day's aggregate at the latest totals, not the
// sum of both pings. Replaying the second body changes nothing.
await recordPing(store, "secret", ping, today);
assert.deepEqual(await totalsOf(), { launches: 1, commands_completed: 42 });
const grown = { ...ping, counters: { launches: 2, commands_completed: 50 } };
await recordPing(store, "secret", grown, today);
assert.deepEqual(await totalsOf(), { launches: 2, commands_completed: 50 });
await recordPing(store, "secret", grown, today);
assert.deepEqual(await totalsOf(), { launches: 2, commands_completed: 50 });

// A counters dip (the client restarted its day) adds nothing either.
await recordPing(store, "secret", ping, today);
assert.deepEqual(await totalsOf(), { launches: 2, commands_completed: 50 });

// A second install still adds its own totals on top.
const other = { ...ping, id: "fedcba9876543210fedcba9876543210", counters: { launches: 1, commands_completed: 5 } };
await recordPing(store, "secret", other, today);
assert.deepEqual(await totalsOf(), { launches: 3, commands_completed: 55 });

// The install's record is stored under its hash, never the raw id.
assert.equal(kv.has(`analytics:${today}:${id}`), false);
assert.equal(JSON.parse(kv.get(`analytics:${today}:${hashed}`)).counters.commands_completed, 42);

console.log("analytics: ok");

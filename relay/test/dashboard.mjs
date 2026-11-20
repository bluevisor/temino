// The dashboard's numbers: what buildStats adds up from stored pings and
// releases, and the axis ticks. Run: node test/dashboard.mjs
import assert from "node:assert/strict";
import { buildStats, dashboardPage, niceTicks, platformOf } from "../src/dashboard.js";

const today = "2026-11-19";
const rec = (day, hash, os = "macos", arch = "aarch64", version = "0.3.7", counters = {}) => ({ day, hash, os, arch, version, counters });
const records = [
  rec("2026-11-19", "a"), rec("2026-11-18", "a", "macos", "aarch64", "0.3.6"),
  rec("2026-11-19", "b", "linux", "x86_64"),
  rec("2026-11-10", "c", "haiku", "aarch64", "0.3.5"), // 9 days ago: in 30, not in 7
  rec("2026-10-01", "d"), // past the 30-day window
];
const aggregates = { "2026-11-19": { launches: 3, active_minutes: 120 }, "2026-11-12": { launches: 9 } };
const releases = [
  { tag: "v0.3.6", date: "2026-11-16T00:00:00Z", assets: [{ name: "Temino-0.3.6-macos-arm64.dmg", count: 4 }, { name: "temino-0.3.6-1-arm64.hpkg", count: 1 }] },
  { tag: "v0.3.7", date: "2026-11-20T00:00:00Z", assets: [{ name: "temino-0.3.7-linux-x86_64.tar.gz", count: 2 }] },
];
const s = buildStats(records, aggregates, releases, today);
assert.equal(Object.keys(s.daily).length, 30);
assert.equal(s.daily["2026-11-19"], 2);
assert.equal(s.activeToday, 2);
assert.equal(s.active7, 2);
assert.equal(s.active30, 3);
assert.equal(s.new7, 2, "a and b were first seen this week; c and d earlier");
assert.deepEqual(s.platforms, { "macOS Apple silicon": 1, "Linux x86_64": 1 });
assert.deepEqual(s.versions, { "0.3.7": 2 }, "each install counts once, at its latest version");
assert.equal(s.features.launches, 3, "only the last 7 days' totals");
assert.equal(s.features.active_minutes, 120);
assert.equal(s.downloads.total, 7);
assert.equal(s.downloads.latest.tag, "v0.3.7");
assert.deepEqual(s.downloads.byPlatform, { "macOS Apple silicon": 4, "Haiku arm64": 1, "Linux x86_64": 2 });
assert.equal(buildStats([], {}, null, today).downloads, null);

assert.equal(platformOf("Temino-0.3.7-macos-x86_64.dmg"), "macOS Intel");
assert.equal(platformOf("temino-0.3.7-linux-aarch64.tar.gz"), "Linux arm64");
assert.equal(platformOf("temino-0.3.7-1-x86_64.hpkg"), "Haiku x86_64");
assert.equal(platformOf("notes.txt"), "Other");

assert.deepEqual(niceTicks(0), [0, 1]);
assert.deepEqual(niceTicks(3), [0, 1, 2, 3]);
assert.deepEqual(niceTicks(7), [0, 2, 4, 6, 8]);
assert.deepEqual(niceTicks(130), [0, 50, 100, 150]);

// The page escapes what it prints and never carries a script.
const page = dashboardPage("https://d.example", "a<b@example.com", s);
assert.ok(page.includes("a&lt;b@example.com"));
assert.ok(!/<script/i.test(page));
console.log("dashboard: ok");

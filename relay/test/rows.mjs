// The tab list's rows and numbers (public/rows.js): node test/rows.mjs
import assert from "node:assert/strict";
import { tabRows, rowsInOrder, stripHtml, groupHtml } from "../public/rows.js";

const pane = (id, more) => ({ id, title: `p${id}`, cwd: "~", ...more });

// A Mac that sends numbers: Settings (tab 1, not sent) keeps its number 2,
// so the list goes 1, 3, 4, 5; a split tab is a group, its panes 3 and 4.
const mac = { tabs: [
  { index: 0, number: 1, group: false, focus: 1, panes: [pane(1, { number: 1 })] },
  { index: 2, number: 3, group: true, focus: 4, panes: [pane(3, { number: 3 }), pane(4, { number: 4 })] },
  { index: 3, number: 5, group: false, focus: 5, panes: [pane(5, { number: 5 })] },
] };
const rows = tabRows(mac);
assert.deepEqual(rows.map((g) => [g.number, g.group]), [[1, false], [3, true], [5, false]]);
assert.deepEqual(rows[1].panes.map((p) => p.number), [3, 4]);
assert.deepEqual(rowsInOrder(mac), [
  { tab: 0, pane: 1, number: 1 }, { tab: 2, pane: 3, number: 3 }, { tab: 2, pane: 4, number: 4 }, { tab: 3, pane: 5, number: 5 },
]);

// A compact sidebar on the Mac: a split tab is one row with one number.
const compact = { tabs: [
  { index: 0, number: 1, group: false, focus: 2, panes: [pane(1, { number: 1 }), pane(2, { number: 1 })] },
  { index: 1, number: 2, group: false, focus: 3, panes: [pane(3, { number: 2 })] },
] };
assert.deepEqual(rowsInOrder(compact), [{ tab: 0, pane: 2, number: 1 }, { tab: 1, pane: 3, number: 2 }]);

// An older Mac sends no numbers: count them here, a split tab a group.
// The second group starts at 4, not at 1 again.
const old = { tabs: [
  { index: 0, focus: 1, panes: [pane(1), pane(2)] },
  { index: 1, focus: 3, panes: [pane(3)] },
  { index: 2, focus: 4, panes: [] }, // no terminal: not listed
  { index: 3, focus: 5, panes: [pane(5), pane(6)] },
] };
assert.deepEqual(rowsInOrder(old).map((r) => r.number), [1, 2, 3, 4, 5]);
assert.deepEqual(tabRows(old).map((g) => g.number), [1, 3, 4]);

// Titles are escaped; the current pane's chip is lit, × on its group.
const evil = { tabs: [
  { index: 0, number: 1, group: true, focus: 1, title: "<b>", panes: [pane(1, { number: 1, title: "a&b" }), pane(2, { number: 2 })] },
  { index: 1, number: 3, group: false, focus: 3, title: "x", panes: [pane(3, { number: 3 })] },
] };
const strip = stripHtml(tabRows(evil), { tab: 0, pane: 2 });
assert.ok(strip.includes("&lt;b&gt;") && strip.includes("a&amp;b") && !strip.includes("<b>"));
assert.ok(/class="tab current" data-tab="0" data-pane="2"/.test(strip));
assert.equal((strip.match(/data-close=/g) || []).length, 1);
assert.ok(groupHtml(tabRows(evil)[0], true, 1, true).includes('class="row current" data-tab="0" data-pane="1"'));

console.log("rows ok");

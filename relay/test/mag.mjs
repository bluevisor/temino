// The magnifier's lines (public/mag.js): node test/mag.mjs
import assert from "node:assert/strict";
import { MAG_DEFAULT, aroundCursor, clampLines, lastOutput, magRows, tailStart, texty } from "../public/mag.js";

const row = (t) => [[t, "#fff", null, 0]];
const blank = [];

assert.equal(MAG_DEFAULT, 5);
assert.ok(texty(row("hello")));
assert.ok(!texty(row("╭──────╮")) && !texty(blank) && !texty(row("   ")));

// Claude Code: output, a blank row, the input box. 5 text rows above the
// input row, the box's top border and the blank don't count.
const cc = [row("one"), row("two"), row("three"), row("four"), row("five"), row("six"), blank,
  row("╭────────╮"), row("│ > typing │"), row("╰────────╯"), row("? for shortcuts")];
assert.deepEqual(aroundCursor(cc, 8, 5), { from: 1, to: 10 }); // two … six, blank, border, input, bottom border
assert.deepEqual(aroundCursor(cc, 8, 0), { from: 8, to: 10 });
assert.deepEqual(aroundCursor(cc, 8, 30), { from: 0, to: 10 }); // as many as there are
assert.equal(aroundCursor(cc, -1, 5), null);

// The shell: the last 5 text rows across blocks, no trailing blank rows.
const blocks = [{ lines: [row("a1"), row("a2"), row("a3")] }, { lines: [row("b1"), blank, row("b2"), blank, blank] }];
const out = lastOutput(blocks, 5).map((r) => r.length ? r[0][0] : "");
assert.deepEqual(out, ["a1", "a2", "a3", "b1", "", "b2"]);
assert.deepEqual(lastOutput(blocks, 0), []);
assert.deepEqual(lastOutput([], 5), []);

// History: everything before the last 5 text rows is above the view.
const all = lastOutput(blocks, 200);
assert.equal(tailStart(all, 5), all.length - lastOutput(blocks, 5).length);
assert.equal(tailStart(all, 2), all.indexOf(all.find((r) => r.length && r[0][0] === "b1")));
assert.equal(tailStart(all, 0), all.length);
assert.equal(tailStart(all, 99), 0);

assert.equal(clampLines(-3), 0);
assert.equal(clampLines(7.4), 7);
assert.equal(clampLines(99), 30);
console.log("mag: ok");

// What shows: no blank or border-only rows, a box's walls trimmed, the
// cursor moved along with the trim.
const text = (m) => m.rows.map((r) => r.map((x) => x[0]).join(""));
const shown = magRows(cc.slice(1, 10), [7, 10]); // two … bottom border; cursor after "│ > typing"
assert.deepEqual(text(shown), ["two", "three", "four", "five", "six", "> typing "]);
assert.deepEqual(shown.cursor, [5, 8]);
const end = magRows([row("│ > hi       │")], [0, 6]); // the cursor after the text, before the padding
assert.deepEqual(text(end), ["> hi "]);
assert.deepEqual(end.cursor, [0, 4]);
const wide = magRows([[["你好", "#fff", null, 16]]], [0, 4]); // wide characters: two columns each
assert.deepEqual(text(wide), ["你好"]);
assert.deepEqual(wide.cursor, [0, 4]);
console.log("magRows: ok");

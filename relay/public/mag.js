// What the magnifier shows: the input line (the command line, or the
// cursor's row in a full-screen program or running command) with the output
// above it. Rows are [text, fg, bg, flags] runs, as the Mac sends them.
//
// "Lines" count rows with text: blank rows and box borders (Claude Code's
// input box, a table rule) don't use up the count, so 5 means 5 lines you
// can read, not 5 rows that may be mostly frame. The magnifier wraps long
// lines rather than scrolling sideways, so it also leaves out rows with no
// text and a box's side walls (`magRows`).

export const MAG_DEFAULT = 5;
export const MAG_MAX = 80;
// Text rows of shell output kept above the shown lines, to drag back through.
export const MAG_HISTORY = 200;

export const texty = (row) => (row || []).some(([t]) => /[\p{L}\p{N}]/u.test(t));

// Rows [from, to) around the cursor row `cr`: `want` text rows above it, the
// row itself, and one below (the bottom of an input box, the next line).
export function aroundCursor(lines, cr, want) {
  if (cr < 0 || cr >= lines.length) return null;
  let from = cr, got = 0;
  while (from > 0 && got < want) {
    from--;
    if (texty(lines[from])) got++;
  }
  return { from, to: Math.min(lines.length, cr + 2) };
}

// The last `want` text rows of output in the shell's blocks (newest last),
// from as far back as it takes, with no trailing blank rows.
export function lastOutput(blocks, want) {
  const rows = [];
  let got = 0;
  for (let b = blocks.length - 1; b >= 0 && got < want; b--) {
    const lines = blocks[b].lines || [];
    for (let i = lines.length - 1; i >= 0 && got < want; i--) {
      if (!rows.length && !texty(lines[i])) continue; // trailing blank rows
      rows.unshift(lines[i]);
      if (texty(lines[i])) got++;
    }
  }
  return rows;
}

// Where the last `want` text rows of `rows` start (rows.length for none, 0
// when there are fewer): the rows before it are history, above the view.
export function tailStart(rows, want) {
  if (want <= 0) return rows.length;
  let got = 0;
  for (let i = rows.length - 1; i >= 0; i--) if (texty(rows[i]) && ++got === want) return i;
  return 0;
}

export const clampLines = (n) => Math.min(MAG_MAX, Math.max(0, Math.round(n)));

// A row's characters with the column each starts at (wide ones take two).
function cellsOf(runs) {
  const out = [];
  let col = 0;
  runs.forEach((run, r) => {
    const span = run[3] & 16 ? 2 : 1;
    for (const ch of Array.from(run[0])) { out.push({ ch, col, r }); col += span; }
  });
  return out;
}

// The runs of `runs` between columns [from, to).
function sliceRuns(runs, from, to) {
  const out = [];
  for (const { ch, col, r } of cellsOf(runs)) {
    if (col < from || col >= to) continue;
    const last = out[out.length - 1];
    if (last && last.r === r) last.text += ch;
    else out.push({ r, text: ch });
  }
  return out.map(({ r, text }) => [text, ...runs[r].slice(1)]);
}

const WALL = /[│┃║╎╏┆┇┊┋]/;

// Rows to show, and where the cursor is among them: rows with no text are
// left out (the cursor's row stays), a box's left wall and the spaces and
// wall at the end are trimmed, never past the cursor.
export function magRows(rows, cursor) {
  const out = [];
  let at = null;
  rows.forEach((runs, i) => {
    const here = cursor && cursor[0] === i;
    if (!here && !texty(runs)) return;
    const cells = cellsOf(runs || []);
    let from = 0, to = cells.length ? cells[cells.length - 1].col + 1 : 0;
    const first = cells.findIndex((c) => c.ch !== " ");
    if (first >= 0 && WALL.test(cells[first].ch)) {
      from = cells[first].col + 1;
      if (cells[first + 1] && cells[first + 1].ch === " ") from++;
    }
    let end = cells.length - 1;
    while (end >= 0 && (cells[end].ch === " " || WALL.test(cells[end].ch))) end--;
    to = end >= 0 ? cells[end].col + (runs[cells[end].r][3] & 16 ? 2 : 1) : from;
    if (here) {
      from = Math.min(from, cursor[1]);
      to = Math.max(to, cursor[1] + 1);
      at = [out.length, cursor[1] - from];
    }
    out.push(sliceRuns(runs || [], from, to));
  });
  return { rows: out, cursor: at };
}

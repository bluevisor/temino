// The window's tab list as the Mac's sidebar shows it: a row per tab, or
// for a split tab, a group with a row per pane. The numbers run straight
// down the list, a group's panes one apiece, as ⌘1…9 count them on the
// Mac, which sends each one. An older Mac sends none: count them here, a
// split tab always a group.

export const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const color = (c) => /^#[0-9a-f]{6}$/i.test(c || "") ? c : "";

// [{ tab, number, group, panes: [{ ...pane, number }] }], one per tab with
// a terminal; a group's panes in the order the Mac lists them.
export function tabRows(w) {
  let next = 1;
  return w.tabs.filter((t) => t.panes.length).map((t) => {
    const group = "group" in t ? t.group : t.panes.length > 1;
    const number = t.number || next;
    const panes = t.panes.map((p, i) => ({ ...p, number: p.number || (group ? number + i : number) }));
    next = number + (group ? t.panes.length : 1);
    return { tab: t, number, group, panes };
  });
}

// Every row in order, as {tab, pane, number}: where a sideways swipe goes
// next. A one-pane tab's row is its focused pane.
export function rowsInOrder(w) {
  return tabRows(w).flatMap((g) => g.group
    ? g.panes.map((p) => ({ tab: g.tab.index, pane: p.id, number: p.number }))
    : [{ tab: g.tab.index, pane: g.tab.focus, number: g.number }]);
}

// A row's number, marked as the Mac marks it: plain at rest, a dot going
// round while its agent works, amber while it waits on you, green when done.
export const num = (n, state) => `<span class="num ${esc(state || "")}${String(n).length > 1 ? " wide" : ""}">${n}</span>`;

// A one-pane tab's card: its number and title, and `sub` under them.
export function cardHtml(g, current, sub, swatch) {
  const tc = color(g.tab.color);
  return `<div class="card${current ? " current" : ""}" data-tab="${g.tab.index}">${swatch ? `<span class="swatch" style="background:${tc || "transparent"}"></span>` : ""}${num(g.number, g.tab.agent)}
    <div class="t"><div>${esc(g.tab.title)}</div>${sub ? `<div>${esc(sub)}</div>` : ""}</div></div>`;
}

// A split tab, as the Mac's sidebar draws it: its name over a card in its
// color with a row per pane, pane `lit` lit when it's the current tab.
// With `folders`, each pane's folder goes under its title.
export function groupHtml(g, current, lit, folders) {
  const tc = color(g.tab.color);
  return `<div class="group${current ? " current" : ""}">
    <div class="gh" data-tab="${g.tab.index}">${esc(g.tab.title)}</div>
    <div class="gcard"${tc ? ` style="--tc:${tc}"` : ""}>${g.panes.map((p) => `
      <div class="row${current && p.id === lit ? " current" : ""}" data-tab="${g.tab.index}" data-pane="${p.id}">${num(p.number, p.agent)}
        <div class="t"><div>${esc(p.title)}</div>${folders ? `<div>${esc(p.cwd)}</div>` : ""}</div></div>`).join("")}
    </div></div>`;
}

// The phone's strip along the top: a chip per row, a group's chips boxed
// together under its name; × on the current tab closes it (a whole group,
// as its header's × does on the Mac).
export function stripHtml(groups, at) {
  const close = (t) => t.index === at.tab && groups.length > 1 ? `<span class="x" data-close="${t.index}">×</span>` : "";
  const bar = (t) => color(t.color) ? `<span class="bar" style="background:${t.color}"></span>` : "";
  return groups.map((g) => g.group
    ? `<div class="tabgroup${g.tab.index === at.tab ? " current" : ""}">${bar(g.tab)}<span class="gn" data-tab="${g.tab.index}">${esc(g.tab.title)}</span>${g.panes.map((p) => `
      <div class="tab${g.tab.index === at.tab && p.id === at.pane ? " current" : ""}" data-tab="${g.tab.index}" data-pane="${p.id}">${num(p.number, p.agent)}<span class="tt">${esc(p.title)}</span></div>`).join("")}${close(g.tab)}</div>`
    : `<div class="tab${g.tab.index === at.tab ? " current" : ""}" data-tab="${g.tab.index}">${bar(g.tab)}${num(g.number, g.tab.agent)}<span class="tt">${esc(g.tab.title)}</span>${close(g.tab)}</div>`).join("");
}

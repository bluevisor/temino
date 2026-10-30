// Temino Remote, the web client: signs in, finds your machines through
// the relay, and drives one end to end encrypted. The relay sees only
// sealed frames; the desktop shows six digits to compare the first time.
//
// Zoom levels, innermost first: one pane (the default), its tab, the
// window, the machine, and you (every machine on your account).

import { Session, generateKeys, hex, b64, fromB64 } from "/noise.js";

const LEVELS = ["pane", "tab", "window", "machine", "user"];
const query = new URLSearchParams(location.search);
let level = LEVELS.includes(query.get("level")) ? query.get("level") : "pane";
let at = null;              // {window, tab, pane} on the machine
let machine = null;         // the desktop device id being driven
let devices = [];
let me = null;
let windows = [];
let relay = null;
let session = null;         // Noise session with `machine`
let pending = null;         // the code while the desktop waits for approval
let keys = null;

const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const store = {
  get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch {} },
};

// ---- this device's key: made once, kept in IndexedDB, never readable
function idb() {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open("temino", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("keys");
    open.onsuccess = () => resolve(open.result);
    open.onerror = () => reject(open.error);
  });
}
async function loadKeys() {
  const db = await idb();
  const get = (k) => new Promise((r) => { const q = db.transaction("keys").objectStore("keys").get(k); q.onsuccess = () => r(q.result); q.onerror = () => r(null); });
  const found = await get("device");
  if (found) return found;
  const fresh = await generateKeys(false);
  await new Promise((r) => { const t = db.transaction("keys", "readwrite"); t.objectStore("keys").put(fresh, "device"); t.oncomplete = r; t.onerror = r; });
  return fresh;
}

// ---- signing in
function signIn() {
  const state = hex(crypto.getRandomValues(new Uint8Array(12)));
  store.set("temino-state", state);
  const name = /iPad/.test(navigator.userAgent) ? "iPad" : /iPhone/.test(navigator.userAgent) ? "iPhone" : "Browser";
  const q = new URLSearchParams({ return: `${location.origin}/app`, state, pub: hex(keys.public), name, kind: "phone" });
  location.href = `/login?${q}`;
}

function takeToken() {
  const back = new URLSearchParams(location.hash.slice(1));
  if (!back.get("token")) return;
  if (back.get("state") === store.get("temino-state")) {
    store.set("temino-token", back.get("token"));
  }
  store.set("temino-state", null);
  history.replaceState(null, "", location.pathname + location.search);
}

function center(html) {
  $("main").innerHTML = `<div class="center">${html}</div>`;
  $("footer").hidden = true;
  document.body.classList.add("wide-off");
}

// ---- the relay
function connect() {
  const token = store.get("temino-token");
  if (!token) {
    center(`<h1>Temino Remote</h1><p>Sign in with the account you use in Temino on your Mac.</p><button id="signin">Sign In</button>`);
    $("signin").onclick = signIn;
    return;
  }
  relay = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/connect?token=${encodeURIComponent(token)}`);
  relay.onmessage = (event) => onRelay(JSON.parse(event.data));
  relay.onclose = (event) => {
    session = null;
    if (event.code === 4001 || event.code === 1008) { store.set("temino-token", null); return connect(); }
    $("crumbs").textContent = "Reconnecting…";
    setTimeout(connect, 1500);
  };
}

const toRelay = (to, kind, bytes) => relay && relay.readyState === 1 && relay.send(JSON.stringify({ type: "send", to, kind, data: b64(bytes) }));

async function onRelay(message) {
  if (message.type === "devices") {
    me = message.you;
    devices = message.devices;
    const desktops = devices.filter((d) => d.kind === "desktop");
    const wanted = store.get("temino-machine");
    const pick = desktops.find((d) => d.id === machine && d.online) || desktops.find((d) => d.id === wanted && d.online)
      || desktops.find((d) => d.online);
    if (pick && (pick.id !== machine || !session)) await start(pick.id);
    if (!pick) { machine = null; session = null; }
    render();
  } else if (message.type === "recv" && message.from === machine) {
    const data = fromB64(message.data);
    try {
      if (message.kind === "hs") {
        const reply = await session.handshake(data);
        if (reply) toRelay(machine, "hs", reply);
        $("lock").hidden = !session.open;
      } else if (session && session.open) {
        const plain = await session.openFrame(data);
        if (plain) onMachine(JSON.parse(new TextDecoder().decode(plain)));
      }
    } catch (error) {
      console.warn("session failed, starting over", error);
      await start(machine);
    }
  }
}

// A session with desktop `id`: the handshake starts at once. The desktop
// key the relay lists must be the one the handshake proves.
async function start(id) {
  machine = id;
  store.set("temino-machine", id);
  windows = [];
  at = null;
  pending = null;
  $("lock").hidden = true;
  const [fresh, first] = await Session.initiate(keys);
  session = fresh;
  toRelay(id, "hs", first);
}

async function send(message) {
  if (!session || !session.open) return;
  const desktop = devices.find((d) => d.id === machine);
  if (desktop && hex(session.peer) !== desktop.pub) return;
  for (const frame of await session.seal(JSON.stringify(message))) toRelay(machine, "msg", frame);
}

function onMachine(message) {
  if (message.type === "pending") { pending = message.code; render(); }
  else if (message.type === "hello") { pending = null; watch(); render(); }
  else if (message.type === "state") { pending = null; windows = message.windows; settle(); render(); }
}

// ---- what's on screen
const win = (no) => windows.find((w) => w.no === no);
const tabOf = (w, index) => w && w.tabs[index];
const paneOf = (t, id) => t && t.panes.find((p) => p.id === id);

function settle() {
  const w = (at && win(at.window)) || windows[0];
  if (!w) { at = null; return; }
  let t = at && at.window === w.no ? tabOf(w, at.tab) : null;
  if (!t) t = tabOf(w, w.active) || w.tabs[0];
  let p = at && at.window === w.no ? paneOf(t, at.pane) : null;
  if (!p) p = paneOf(t, t.focus) || t.panes[0];
  const next = { window: w.no, tab: t.index, pane: p ? p.id : null };
  const moved = !at || next.window !== at.window || next.tab !== at.tab || next.pane !== at.pane;
  at = next;
  if (moved) watch();
}

function watch() {
  if (!at) return send({ type: "watch", panes: [] });
  const t = tabOf(win(at.window), at.tab);
  const panes = level === "pane" ? [[at.window, at.pane]]
    : level === "tab" && t ? t.panes.map((p) => [at.window, p.id]) : [];
  send({ type: "watch", panes });
}

function go(newLevel, where) {
  level = newLevel;
  if (where) at = { ...at, ...where };
  watch();
  render();
}

const dot = (state) => `<span class="dot ${state || "none"}"></span>`;
const RANK = { idle: 1, running: 2, waiting: 3, done: 3 };
const urgency = (a, b) => (RANK[a] || 0) - (RANK[b] || 0);
const machineName = () => (devices.find((d) => d.id === machine) || {}).name || "Temino";

function render() {
  if (!relay) return;
  const main = $("main");
  $("out").disabled = level === "user";
  if (level === "user" || !machine) return renderUser();
  if (pending) {
    center(`<h1>Approve on ${esc(machineName())}</h1><p>Temino on your Mac is asking whether to let this device control it. Check that it shows the same code:</p><div class="code">${esc(pending)}</div><p>Then choose Allow there.</p>`);
    return;
  }
  if (!at) { center(`<p>Connecting to ${esc(machineName())}…</p>`); return; }
  const w = win(at.window), t = tabOf(w, at.tab), p = paneOf(t, at.pane);
  const crumbs = ["You", machineName(), `Window ${w.no}`, t && t.title, p && p.title];
  const depth = LEVELS.length - LEVELS.indexOf(level);
  $("crumbs").innerHTML = crumbs.slice(0, depth).map((c, i) => i === depth - 1 ? `<b>${esc(c)}</b>` : esc(c)).join(" › ");
  $("footer").hidden = level !== "pane";
  document.body.classList.toggle("wide-off", level !== "pane");
  side(w);
  if (level === "pane") {
    main.innerHTML = `<div id="screen"><pre>${esc((p && p.lines || []).join("\n"))}</pre></div>`;
    fit(main.querySelector("pre"), p && p.columns);
    main.scrollTop = main.scrollHeight;
  } else if (level === "tab") {
    main.innerHTML = `<div class="grid">${t.panes.map((q) => `
      <div class="mini" data-pane="${q.id}"><div class="h">${dot(q.agent)}${esc(q.title)}</div>
      <pre>${esc((q.lines || []).slice(-24).join("\n"))}</pre></div>`).join("")}</div>`;
    main.querySelectorAll("[data-pane]").forEach((el) => el.onclick = () => go("pane", { pane: +el.dataset.pane }));
  } else if (level === "window") {
    main.innerHTML = `<div class="cards">` + w.tabs.map((u) => `
      <div class="card ${u.index === w.active ? "current" : ""}" data-tab="${u.index}">
        <span class="swatch" style="background:${/^#[0-9a-f]{6}$/i.test(u.color) ? u.color : "transparent"}"></span>${dot(u.agent)}
        <div class="t"><div>${u.index + 1}. ${esc(u.title)}</div><div>${u.panes.length} pane${u.panes.length === 1 ? "" : "s"} · ${esc(u.panes[0] && u.panes[0].cwd)}</div></div>
      </div>`).join("") + `</div><button id="newtab">New Tab</button>`;
    main.querySelectorAll("main [data-tab]").forEach((el) => el.onclick = () => {
      const u = w.tabs[+el.dataset.tab];
      go("tab", { tab: u.index, pane: u.focus });
    });
    $("newtab").onclick = () => send({ type: "new_tab", window: w.no });
  } else {
    main.innerHTML = `<div class="cards">` + windows.map((v) => `
      <div class="card" data-window="${v.no}">${dot(v.tabs.map((u) => u.agent).sort(urgency).pop())}
        <div class="t"><div>Window ${v.no}</div><div>${v.tabs.length} tab${v.tabs.length === 1 ? "" : "s"} · ${esc(v.title)}</div></div>
      </div>`).join("") + `</div><button id="newwin">New Window</button>`;
    main.querySelectorAll("[data-window]").forEach((el) => el.onclick = () => {
      const v = win(+el.dataset.window);
      const u = tabOf(v, v.active);
      go("window", { window: v.no, tab: u.index, pane: u.focus });
    });
    $("newwin").onclick = () => send({ type: "new_window", window: at.window });
  }
}

// Every machine on your account.
function renderUser() {
  $("crumbs").innerHTML = "<b>You</b>";
  $("footer").hidden = true;
  document.body.classList.add("wide-off");
  const desktops = devices.filter((d) => d.kind === "desktop");
  $("main").innerHTML = (desktops.length ? `<div class="cards">` + desktops.map((d) => `
    <div class="card ${d.id === machine ? "current" : ""}" data-machine="${esc(d.id)}">${dot(d.online ? "idle" : null)}
      <div class="t"><div>${esc(d.name)}</div><div>${d.online ? "Online" : "Offline"}</div></div>
    </div>`).join("") + `</div>` : `<div class="center"><p>No Mac on this account yet. In Temino, open Settings and sign in to Remote Control.</p></div>`)
    + `<button id="signout">Sign Out</button>`;
  $("main").querySelectorAll("[data-machine]").forEach((el) => el.onclick = async () => {
    const d = devices.find((x) => x.id === el.dataset.machine);
    if (!d || !d.online) return;
    if (d.id !== machine || !session) await start(d.id);
    go("machine");
  });
  $("signout").onclick = () => {
    if (relay && me) relay.send(JSON.stringify({ type: "forget", id: me }));
    store.set("temino-token", null);
    location.reload();
  };
}

function side(w) {
  $("side").innerHTML = w.tabs.map((u) => `
    <div class="card ${u.index === at.tab ? "current" : ""}" data-tab="${u.index}">${dot(u.agent)}
      <div class="t"><div>${u.index + 1}. ${esc(u.title)}</div></div></div>` +
    (u.index === at.tab && u.panes.length > 1 ? u.panes.map((q) => `
    <div class="card pane ${q.id === at.pane ? "current" : ""}" data-pane="${q.id}">${dot(q.agent)}
      <div class="t"><div>${esc(q.title)}</div></div></div>`).join("") : "")).join("");
  $("side").querySelectorAll("[data-tab]").forEach((el) => el.onclick = () => {
    const u = w.tabs[+el.dataset.tab];
    go("pane", { tab: u.index, pane: u.focus });
  });
  $("side").querySelectorAll("[data-pane]").forEach((el) => el.onclick = () => go("pane", { pane: +el.dataset.pane }));
}

function fit(pre, columns) {
  if (!columns) return;
  // As wide as the longest line on screen, not the pane: a wide desktop
  // pane's text stays readable; past the smallest size it scrolls sideways.
  const used = Math.max(40, ...pre.textContent.split("\n").map((l) => l.length));
  const width = pre.parentElement.clientWidth - 12;
  pre.style.fontSize = Math.max(9, Math.min(14, width / (Math.min(columns, used) * 0.6))) + "px";
}

$("out").onclick = () => {
  const next = LEVELS[LEVELS.indexOf(level) + 1];
  if (next) go(next);
};
let pinch = 1;
document.addEventListener("gesturestart", (e) => { e.preventDefault(); pinch = 1; });
document.addEventListener("gesturechange", (e) => { e.preventDefault(); pinch = e.scale; });
document.addEventListener("gestureend", (e) => {
  e.preventDefault();
  const i = LEVELS.indexOf(level);
  if (pinch < 0.7 && i < LEVELS.length - 1) go(LEVELS[i + 1]);
  if (pinch > 1.4 && i > 0) go(LEVELS[i - 1]);
});

const KEYS = [["Esc", "Escape"], ["Tab", "Tab"], ["↑", "Up"], ["↓", "Down"], ["←", "Left"], ["→", "Right"],
  ["^C", "c", ["ctrl"]], ["⌫", "Backspace"], ["⏎", "Enter"]];
$("keys").innerHTML = KEYS.map(([label], i) => `<button data-key="${i}">${label}</button>`).join("");
$("keys").onclick = (e) => {
  const i = e.target.dataset.key;
  if (i === undefined || !at) return;
  const [, key, mods] = KEYS[i];
  send({ type: "key", window: at.window, pane: at.pane, key, mods: mods || [] });
};
$("send").onsubmit = (e) => {
  e.preventDefault();
  if (!at) return;
  const text = $("text").value;
  if (text) send({ type: "text", window: at.window, pane: at.pane, text });
  send({ type: "key", window: at.window, pane: at.pane, key: "Enter" });
  $("text").value = "";
};
addEventListener("resize", render);

keys = await loadKeys();
takeToken();
connect();

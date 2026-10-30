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

// ---- this device's key: made once, kept in IndexedDB
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
  const usable = async (found) => {
    if (!found || !(found.public instanceof Uint8Array) || found.public.length !== 32) return null;
    if (found.wrapped && found.wrapKey instanceof CryptoKey) {
      const privateKey = await crypto.subtle.unwrapKey("pkcs8", found.wrapped, found.wrapKey, { name: "AES-GCM", iv: found.iv },
        { name: "X25519" }, false, ["deriveBits"]).catch(() => null);
      return privateKey && { privateKey, public: found.public };
    }
    return found.privateKey instanceof CryptoKey ? found : null;
  };
  const stored = await get("device");
  const found = await usable(stored);
  if (found) return found;
  // Safari can't read an X25519 CryptoKey back out of IndexedDB after a
  // reload, but can an AES one. So the key is kept wrapped under a
  // non-extractable AES key and unwrapped, non-extractable, on load.
  const made = await generateKeys(true);
  const wrapKey = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["wrapKey", "unwrapKey"]);
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const wrapped = new Uint8Array(await crypto.subtle.wrapKey("pkcs8", made.privateKey, wrapKey, { name: "AES-GCM", iv }));
  // add, not put: of two tabs making a key at once, the second reads back
  // the first's. One it can't use (an older format) is replaced.
  await new Promise((r) => {
    const t = db.transaction("keys", "readwrite");
    const record = { wrapped, wrapKey, iv, public: made.public };
    const store = t.objectStore("keys");
    (stored ? store.put(record, "device") : store.add(record, "device")).onerror = (e) => e.preventDefault();
    t.oncomplete = r; t.onerror = r; t.onabort = r;
  });
  const kept = await usable(await get("device"));
  if (!kept) throw new Error("this browser can't keep a device key");
  return kept;
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
    // Another tab or window took over this device: don't fight it back.
    if (event.code === 4000) {
      center(`<h1>Open in another tab</h1><p>Temino Remote is running in another tab or window.</p><button id="here">Use Here</button>`);
      $("crumbs").textContent = "Paused";
      $("here").onclick = connect;
      return;
    }
    $("crumbs").textContent = "Reconnecting…";
    setTimeout(connect, 1500);
  };
}

const toRelay = (to, kind, bytes) => relay && relay.readyState === 1 && relay.send(JSON.stringify({ type: "send", to, kind, data: b64(bytes) }));

async function onRelay(message) {
  if (message.type === "devices") {
    me = message.you;
    devices = message.devices;
    // The account knows this device by another key (storage was cleared, or
    // another tab made a key first): no Mac would let it in, so sign in again.
    const self = devices.find((d) => d.id === me);
    if (self && self.pub !== hex(keys.public)) {
      relay.onclose = null;
      relay.send(JSON.stringify({ type: "forget", id: me }));
      relay.close();
      store.set("temino-token", null);
      return connect();
    }
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
  else if (message.type === "state") { pending = null; windows = message.windows; paint(windows[0] && windows[0].theme); settle(); render(); }
}

// ---- what's on screen
const win = (no) => windows.find((w) => w.no === no);
const tabOf = (w, index) => w && w.tabs.find((t) => t.index === index);
const paneOf = (t, id) => t && t.panes.find((p) => p.id === id);

function settle() {
  const w = (at && win(at.window)) || windows[0];
  if (!w) { at = null; return; }
  let t = at && at.window === w.no ? tabOf(w, at.tab) : null;
  if (!t) t = tabOf(w, w.active) || w.tabs[0];
  // A tab with no terminal (Settings) can't take input: use the first that has one.
  if (t && !t.panes.length) t = w.tabs.find((x) => x.panes.length) || t;
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
    const stick = !main.firstElementChild || main.scrollHeight - main.scrollTop - main.clientHeight < 40;
    main.innerHTML = `<div id="screen" class="${p && p.screen && p.screen.mode === "grid" ? "full" : ""}">${screenHtml(p, true)}</div>`;
    fit(main.querySelector("#screen"), p);
    if (stick) main.scrollTop = main.scrollHeight;
    $("screen").onclick = () => $("type").focus();
  } else if (level === "tab") {
    main.innerHTML = `<div class="grid">${t.panes.map((q) => `
      <div class="mini" data-pane="${q.id}"><div class="h">${dot(q.agent)}${esc(q.title)}</div>
      <div class="m">${screenHtml(q, false)}</div></div>`).join("")}</div>`;
    main.querySelectorAll(".mini .m").forEach((m) => { m.scrollTop = m.scrollHeight; });
    main.querySelectorAll("[data-pane]").forEach((el) => el.onclick = () => go("pane", { pane: +el.dataset.pane }));
  } else if (level === "window") {
    main.innerHTML = `<div class="cards">` + w.tabs.map((u) => `
      <div class="card ${u.index === w.active ? "current" : ""}" data-tab="${u.index}">
        <span class="swatch" style="background:${/^#[0-9a-f]{6}$/i.test(u.color) ? u.color : "transparent"}"></span>${dot(u.agent)}
        <div class="t"><div>${u.index + 1}. ${esc(u.title)}</div><div>${u.panes.length} pane${u.panes.length === 1 ? "" : "s"} · ${esc(u.panes[0] && u.panes[0].cwd)}</div></div>
      </div>`).join("") + `</div><button id="newtab">New Tab</button>`;
    main.querySelectorAll("main [data-tab]").forEach((el) => el.onclick = () => {
      const u = tabOf(w, +el.dataset.tab);
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
    + `<div class="signout"><button id="signout">Sign Out</button></div>`;
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
    const u = tabOf(w, +el.dataset.tab);
    go("pane", { tab: u.index, pane: u.focus });
  });
  $("side").querySelectorAll("[data-pane]").forEach((el) => el.onclick = () => go("pane", { pane: +el.dataset.pane }));
}

// ---- drawing a pane the way the Mac does

// One output line: [text, fg, bg, flags] runs.
function rowHtml(runs, cursorAt) {
  let html = "", col = 0;
  for (const [text, fg, bg, flags] of runs || []) {
    const chars = Array.from(text);
    const style = `color:${fg}${bg ? `;background:${bg}` : ""}${flags & 1 ? ";font-weight:700" : ""}${flags & 2 ? ";font-style:italic" : ""}`
      + (flags & 12 ? `;text-decoration:${flags & 4 ? "underline" : ""} ${flags & 8 ? "line-through" : ""}` : "");
    if (cursorAt != null && cursorAt >= col && cursorAt < col + chars.length) {
      const i = cursorAt - col;
      html += `<span style="${style}">${esc(chars.slice(0, i).join(""))}<span class="cur">${esc(chars[i])}</span>${esc(chars.slice(i + 1).join(""))}</span>`;
      cursorAt = null;
    } else {
      html += `<span style="${style}">${esc(text)}</span>`;
    }
    col += chars.length;
  }
  if (cursorAt != null) html += " ".repeat(Math.max(0, cursorAt - col)) + `<span class="cur"> </span>`;
  return html;
}

const seconds = (s) => s < 60 ? `${s.toFixed(1)}s` : `${Math.floor(s / 60)}m ${Math.round(s % 60)}s`;
const home = (cwd) => (cwd || "").replace(/^\/Users\/[^/]+/, "~");
const chips = (cwd, branch) => `<span class="chip">${esc(home(cwd))}</span>${branch ? `<span class="chip br">${esc(branch)}</span>` : ""}`;

function screenHtml(p, live) {
  const sc = p && p.screen;
  if (!sc) return `<div class="wait">…</div>`;
  if (sc.mode === "grid") {
    const [cr, cc] = sc.cursor || [-1, -1];
    return `<pre class="term">${sc.lines.map((r, i) => rowHtml(r, live && i === cr ? cc : null)).join("\n")}</pre>`;
  }
  const blocks = sc.blocks.map((b) => {
    const status = b.running ? `<span class="st run">● ${seconds(b.secs)}</span>`
      : b.exit == null ? "" : b.exit === 0 ? `<span class="st ok">✓ ${seconds(b.secs)}</span>`
      : `<span class="st bad">✗ ${b.exit} · ${seconds(b.secs)}</span>`;
    const head = b.kind === "command" && b.command
      ? `<div class="bh">${chips(b.cwd, b.branch)}${status}</div><div class="cmd"><span class="pr">❯</span> ${esc(b.command)}</div>` : "";
    const [cr, cc] = (live && b.cursor) || [-1, -1];
    const out = b.lines.length ? `<pre class="term">${b.lines.map((r, i) => rowHtml(r, i === cr ? cc : null)).join("\n")}</pre>` : "";
    return `<div class="block ${b.exit && b.exit !== 0 ? "failed" : ""} ${b.running ? "running" : ""}">${head}${out}</div>`;
  }).join("");
  const ed = sc.editor;
  const prompt = ed ? `<div class="block prompt"><div class="bh">${chips(ed.cwd, ed.branch)}</div>
    <div class="cmd"><span class="pr">❯</span> ${live ? rowHtml([[ed.text, "var(--text)", null, 0]], ed.cursor) : esc(ed.text)}${!ed.text && live ? `<span class="hint">Tap to type</span>` : ""}</div></div>` : "";
  return blocks + prompt;
}

// Text size: a full-screen program is laid out for its columns, so they
// all fit (down to a floor, then it scrolls sideways); block output fits
// its longest line so a wide desktop pane stays readable.
function fit(el, p) {
  const sc = p && p.screen;
  if (!el || !sc) return;
  const width = el.clientWidth - (sc.mode === "grid" ? 4 : 26); // a block's padding and border
  let cols = sc.columns;
  if (sc.mode !== "grid") {
    const used = Math.max(40, ...Array.from(el.querySelectorAll("pre")).flatMap((pre) => pre.textContent.split("\n").map((l) => l.length)));
    cols = Math.min(cols, used);
  }
  const floor = sc.mode === "grid" ? 7 : 9;
  el.style.setProperty("--fs", Math.max(floor, Math.min(14, width / (cols * 0.6))) + "px");
}

// The Mac's theme, so the page looks like the app it drives.
let painted = "";
function paint(theme) {
  if (!theme || JSON.stringify(theme) === painted) return;
  painted = JSON.stringify(theme);
  const r = document.documentElement.style;
  const set = { bg: "background", panel: "surface", raised: "raised", line: "divider", text: "text", dim: "dim",
    faint: "faint", accent: "accent", done: "success", error: "error", waiting: "warning", string: "string",
    variable: "variable", cursor: "cursor" };
  for (const [v, k] of Object.entries(set)) if (/^#[0-9a-f]{6}$/i.test(theme[k])) r.setProperty(`--${v}`, theme[k]);
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

const KEYS = [["Esc", "Escape"], ["Tab", "Tab"], ["^C", "c", ["ctrl"]], ["↑", "Up"], ["↓", "Down"], ["←", "Left"], ["→", "Right"],
  ["⌫", "Backspace"], ["⏎", "Enter"], ["^D", "d", ["ctrl"]], ["^R", "r", ["ctrl"]], ["^L", "l", ["ctrl"]]];
$("keys").innerHTML = `<button id="kb" aria-label="Keyboard">⌨︎</button>` + KEYS.map(([label], i) => `<button data-key="${i}">${label}</button>`).join("");
const key = (k, mods = []) => at && send({ type: "key", window: at.window, pane: at.pane, key: k, mods });
const text = (t) => at && t && send({ type: "text", window: at.window, pane: at.pane, text: t });
$("keys").onpointerdown = (e) => e.preventDefault(); // keep the keyboard up
$("keys").onclick = (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  if (b.id === "kb") { const t = $("type"); document.activeElement === t ? t.blur() : t.focus(); return; }
  const [, k, mods] = KEYS[b.dataset.key];
  key(k, mods || []);
};

// Typing goes straight to the pane, a key at a time, like at the Mac: the
// Mac's own editor shows it, completes it, and runs it on Return.
const NAMED = { ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", Escape: "Escape", Tab: "Tab",
  Enter: "Enter", Backspace: "Backspace", Delete: "Delete", Home: "Home", End: "End", PageUp: "PageUp", PageDown: "PageDown" };
const type = $("type");
type.addEventListener("keydown", (e) => {
  if (e.isComposing || e.keyCode === 229) return;
  const mods = [e.ctrlKey && "ctrl", e.altKey && "alt", e.metaKey && "cmd", e.shiftKey && "shift"].filter(Boolean);
  if (NAMED[e.key]) { e.preventDefault(); key(NAMED[e.key], mods); return; }
  if ((e.ctrlKey || e.metaKey) && e.key.length === 1) {
    if (e.metaKey && e.key === "v") return; // paste comes as a paste event
    e.preventDefault();
    key(e.key.toLowerCase(), mods.filter((m) => m !== "shift"));
  }
});
type.addEventListener("beforeinput", (e) => {
  if (e.isComposing) return;
  if (e.inputType === "insertText" && e.data) { e.preventDefault(); text(e.data); }
  else if (e.inputType === "insertLineBreak" || e.inputType === "insertParagraph") { e.preventDefault(); key("Enter"); }
  else if (e.inputType === "deleteContentBackward") { e.preventDefault(); key("Backspace"); }
  else if (e.inputType === "insertReplacementText" || e.inputType === "insertFromPaste") {
    e.preventDefault();
    text(e.data || (e.dataTransfer && e.dataTransfer.getData("text/plain")) || "");
  }
});
// CJK and other input methods: what the IME commits goes over whole.
type.addEventListener("compositionend", (e) => { text(e.data); type.value = ""; });
type.addEventListener("paste", (e) => { e.preventDefault(); text(e.clipboardData.getData("text/plain")); });
type.addEventListener("input", () => { if (!type.matches(":focus") || !type.value) return; /* leftovers */ });
type.addEventListener("focus", () => document.body.classList.add("typing"));
type.addEventListener("blur", () => document.body.classList.remove("typing"));
addEventListener("resize", render);

try {
  keys = await loadKeys();
  takeToken();
  connect();
} catch (error) {
  center(`<h1>Can't start</h1><p>${esc(error.message || error)}</p>`);
}

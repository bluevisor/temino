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
  relay.onopen = visible;
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
  else if (message.type === "state") {
    pending = null; unsure = false; windows = resolve(message.windows); paint(windows[0] && windows[0].theme); settle(); notice();
    const sc = starting && currentPane() && currentPane().screen;
    if (sc && (sc.editor || sc.mode === "grid")) flushHeld();
    render();
  }
}

// Screen lines come whole once, then as just their key while unchanged.
// The cache keeps exactly the lines the last state used, as the Mac does.
let lineCache = new Map();
function resolve(windows) {
  const next = new Map();
  const line = (ref) => {
    if (Array.isArray(ref)) return ref; // an older Mac sends lines whole
    const v = "v" in ref ? ref.v : lineCache.get(ref.k) || [];
    next.set(ref.k, v);
    return v;
  };
  for (const w of windows) for (const t of w.tabs) for (const p of t.panes) {
    const sc = p.screen;
    if (!sc) continue;
    if (sc.mode === "grid") sc.lines = sc.lines.map(line);
    else for (const b of sc.blocks) b.lines = b.lines.map(line);
  }
  lineCache = next;
  return windows;
}

// ---- what's on screen
const win = (no) => windows.find((w) => w.no === no);
const tabOf = (w, index) => w && w.tabs.find((t) => t.index === index);
const paneOf = (t, id) => t && t.panes.find((p) => p.id === id);

// A tab opened from here: go to it once the Mac has it, and send what was
// typed meanwhile once its shell is at the prompt (a starting shell drops it).
let follow = 0, starting = false;
function flushHeld() {
  starting = false;
  setTimeout(() => held.splice(0).forEach((f) => f()));
}
function settle() {
  const w = (at && win(at.window)) || windows[0];
  if (!w) { at = null; return; }
  let jumped = false;
  if (follow && w.tabs.length > follow) {
    follow = 0;
    const u = tabOf(w, w.active);
    if (u && u.panes.length) { at = { window: w.no, tab: u.index, pane: u.focus }; jumped = true; }
    starting = true;
  }
  let t = at && at.window === w.no ? tabOf(w, at.tab) : null;
  if (!t) t = tabOf(w, w.active) || w.tabs[0];
  // A tab with no terminal (Settings) can't take input: use the first that has one.
  if (t && !t.panes.length) t = w.tabs.find((x) => x.panes.length) || t;
  let p = at && at.window === w.no ? paneOf(t, at.pane) : null;
  if (!p) p = paneOf(t, t.focus) || t.panes[0];
  const next = { window: w.no, tab: t.index, pane: p ? p.id : null };
  const moved = jumped || !at || next.window !== at.window || next.tab !== at.tab || next.pane !== at.pane;
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
  strip(w);
  if (level === "pane") {
    const stick = !main.firstElementChild || main.scrollHeight - main.scrollTop - main.clientHeight < 40;
    main.innerHTML = `<div id="screen" class="${p && p.screen && p.screen.mode === "grid" ? "full" : ""}">${screenHtml(p, true)}</div>`;
    fit(main.querySelector("#screen"), p);
    if (stick) main.scrollTop = main.scrollHeight;
    $("screen").onclick = (e) => {
      const item = e.target.closest("[data-mi]");
      const ed = p.screen && p.screen.editor;
      if (item && ed && ed.menu) {
        // The Mac's own keys: move to the item, then take it.
        const n = ed.menu.items.length, steps = (+item.dataset.mi - ed.menu.selected + n) % n;
        for (let i = 0; i < steps; i++) key("Down");
        key("Enter");
      } else if (e.target.id === "ghost") {
        key("End");
      }
      $("type").focus();
    };
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
  $("tabs").hidden = true;
  $("crumbs").innerHTML = "<b>You</b>";
  $("footer").hidden = true;
  document.body.classList.add("wide-off");
  const desktops = devices.filter((d) => d.kind === "desktop");
  $("main").innerHTML = (desktops.length ? `<div class="cards">` + desktops.map((d) => `
    <div class="card ${d.id === machine ? "current" : ""}" data-machine="${esc(d.id)}">${dot(d.online ? "idle" : null)}
      <div class="t"><div>${esc(d.name)}</div><div>${d.online ? "Online" : "Offline"}</div></div>
    </div>`).join("") + `</div>` : `<div class="center"><p>No Mac on this account yet. In Temino, open Settings and sign in to Remote Control.</p></div>`)
    + `<div class="signout"><button id="notify">${notifyLabel()}</button><button id="signout">Sign Out</button></div>`
    + `<p class="note" id="notifynote">${notifyNote()}</p>`;
  $("notify").onclick = toggleNotify;
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

// The window's tabs, along the top on a phone: tap to switch, × closes
// the current one, + opens one. Swiping the screen sideways switches too.
function strip(w) {
  const el = $("tabs");
  el.hidden = level !== "pane" && level !== "tab";
  if (el.hidden) return;
  const live = w.tabs.filter((u) => u.panes.length);
  el.innerHTML = live.map((u) => `
    <div class="tab ${u.index === at.tab ? "current" : ""}" data-tab="${u.index}">
      ${/^#[0-9a-f]{6}$/i.test(u.color) ? `<span class="bar" style="background:${u.color}"></span>` : ""}
      <span class="n">${u.index + 1}</span>${dot(u.agent)}<span class="tt">${esc(u.title)}</span>
      ${u.index === at.tab && live.length > 1 ? `<span class="x" data-close="${u.index}">×</span>` : ""}
    </div>`).join("") + `<div class="tab add" id="addtab">${choosing ? "×" : "+"}</div>`
    + (choosing ? (w.shells || []).map((name, i) => `<div class="tab shell" data-shell="${esc(name)}">${esc(name)}${i === 0 ? ` <span class="n">usual</span>` : ""}</div>`).join("") : "");
  el.onclick = (e) => {
    const close = e.target.closest("[data-close]");
    if (close) return send({ type: "close_tab", window: w.no, index: +close.dataset.close });
    const shell = e.target.closest("[data-shell]");
    if (shell) { choosing = false; return newTab(w, shell.dataset.shell); }
    if (e.target.closest("#addtab")) {
      // One shell: just open it. Several: + shows them, the usual one first.
      if ((w.shells || []).length < 2) return newTab(w);
      choosing = !choosing;
      return render();
    }
    const t = e.target.closest("[data-tab]");
    if (t) switchTab(w, +t.dataset.tab);
  };
  const cur = el.querySelector(".current");
  if (cur && !strip.scrolled) { cur.scrollIntoView({ inline: "center", block: "nearest" }); }
}

let choosing = false;
function newTab(w, shell) {
  follow = w.tabs.length;
  send({ type: "new_tab", window: w.no, ...(shell ? { shell } : {}) });
  // Ready to type in it: focus now, inside the tap (iOS raises the keyboard
  // only then); keys wait until the Mac has the tab.
  if (level === "tab") level = "pane";
  $("type").focus();
  // No tab after all: type where you were.
  setTimeout(() => { if (follow || starting) { follow = 0; flushHeld(); } }, 6000);
}

function switchTab(w, index) {
  const u = tabOf(w, index);
  if (!u) return;
  send({ type: "tab", window: w.no, index });
  go(level === "tab" ? "tab" : "pane", { tab: u.index, pane: u.focus });
}

// Swipe sideways across the pane: the next or previous tab.
let swipe = null;
$("main").addEventListener("touchstart", (e) => {
  const t = e.touches[0];
  swipe = e.touches.length === 1 ? { x: t.clientX, y: t.clientY, at: Date.now() } : null;
}, { passive: true });
$("main").addEventListener("touchend", (e) => {
  if (!swipe || level !== "pane") return;
  const t = e.changedTouches[0], dx = t.clientX - swipe.x, dy = t.clientY - swipe.y;
  const quick = Date.now() - swipe.at < 500;
  swipe = null;
  // Only a clear, quick sideways flick, and not over something that scrolls sideways.
  if (!quick || Math.abs(dx) < 70 || Math.abs(dy) > Math.abs(dx) / 2) return;
  const scroller = e.target.closest(".block, #screen.full");
  if (scroller && scroller.scrollWidth > scroller.clientWidth + 2) return;
  const w = win(at.window), live = w.tabs.filter((u) => u.panes.length);
  const i = live.findIndex((u) => u.index === at.tab);
  const next = live[i + (dx < 0 ? 1 : -1)];
  if (next) switchTab(w, next.index);
}, { passive: true });

// ---- notifications while the page is closed (Web Push)
// iOS delivers them only to the page added to the Home Screen.
const standalone = matchMedia("(display-mode: standalone)").matches || navigator.standalone;
const pushable = "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
const notifyOn = () => store.get("temino-push") === "on" && pushable && Notification.permission === "granted";
const notifyLabel = () => notifyOn() ? "Notifications On" : "Notify Me";
function notifyNote() {
  if (notifyOn()) return "You'll hear when an agent waits on you or finishes while you're away from the Mac.";
  if (!pushable && /iPhone|iPad/.test(navigator.userAgent) && !standalone) return "To get notifications, tap Share ▸ Add to Home Screen, then open Temino from there.";
  if (!pushable) return "This browser can't show notifications.";
  return "Get a notification when an agent waits on you or finishes.";
}
async function toggleNotify() {
  try {
    const registration = await navigator.serviceWorker.register("/sw.js");
    if (notifyOn()) {
      const sub = await registration.pushManager.getSubscription();
      if (sub) await sub.unsubscribe();
      relay.send(JSON.stringify({ type: "push", sub: null }));
      store.set("temino-push", null);
    } else {
      if (await Notification.requestPermission() !== "granted") throw new Error("Notifications are turned off for this page in Settings.");
      const key = await (await fetch("/vapid")).text();
      const sub = await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: fromB64url(key) });
      relay.send(JSON.stringify({ type: "push", sub: sub.toJSON() }));
      store.set("temino-push", "on");
    }
  } catch (error) {
    $("notifynote").textContent = error.message || String(error);
    return;
  }
  render();
}
const fromB64url = (t) => Uint8Array.from(atob(t.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((t.length + 3) % 4)), (c) => c.charCodeAt(0));

// The relay pushes only to phones not looking at the page.
const visible = () => relay && relay.readyState === 1 && relay.send(JSON.stringify({ type: "visible", on: !document.hidden }));
document.addEventListener("visibilitychange", visible);

function side(w) {
  $("side").innerHTML = w.tabs.filter((u) => u.panes.length).map((u) => `
    <div class="card ${u.index === at.tab ? "current" : ""}" data-tab="${u.index}">${dot(u.agent)}
      <div class="t"><div>${u.index + 1}. ${esc(u.title)}</div></div></div>` +
    (u.index === at.tab && u.panes.length > 1 ? u.panes.map((q) => `
    <div class="card pane ${q.id === at.pane ? "current" : ""}" data-pane="${q.id}">${dot(q.agent)}
      <div class="t"><div>${esc(q.title)}</div></div></div>`).join("") : "")).join("");
  $("side").insertAdjacentHTML("beforeend", (w.shells || []).length < 2 ? `<button class="add" data-shell="">+ New Tab</button>`
    : (w.shells || []).map((name, i) => `<button class="add" data-shell="${esc(name)}">+ New ${esc(name)} Tab</button>`).join(""));
  $("side").querySelectorAll("[data-shell]").forEach((b) => b.onclick = () => newTab(w, b.dataset.shell || undefined));
  $("side").querySelectorAll("[data-tab]").forEach((el) => el.onclick = () => switchTab(w, +el.dataset.tab));
  $("side").querySelectorAll("[data-pane]").forEach((el) => el.onclick = () => go("pane", { pane: +el.dataset.pane }));
}

// ---- drawing a pane the way the Mac does

// Characters as cells. ASCII sits on the monospace grid by itself; anything
// else (box drawing, braille graphs, symbols, CJK) may come from a fallback
// font of another width, so it gets a cell of its own: one wide, or two
// for a wide character (flag 16), so every line keeps its columns.
const cells = (chars, wide) => chars.map((c) => /^[\x20-\x7e]$/.test(c) ? esc(c)
  : `<span class="c${wide ? " w" : ""}">${esc(c)}</span>`).join("");

// One output line: [text, fg, bg, flags] runs. `cursorAt` is a column.
function rowHtml(runs, cursorAt) {
  let html = "", col = 0;
  for (const [text, fg, bg, flags] of runs || []) {
    const chars = Array.from(text), wide = flags & 16, span = wide ? 2 : 1;
    const style = `color:${fg}${bg ? `;background:${bg}` : ""}${flags & 1 ? ";font-weight:700" : ""}${flags & 2 ? ";font-style:italic" : ""}`
      + (flags & 12 ? `;text-decoration:${flags & 4 ? "underline" : ""} ${flags & 8 ? "line-through" : ""}` : "");
    const width = chars.length * span;
    if (cursorAt != null && cursorAt >= col && cursorAt < col + width) {
      const i = Math.floor((cursorAt - col) / span);
      html += `<span style="${style}">${cells(chars.slice(0, i), wide)}<span class="cur">${cells([chars[i]], wide)}</span>${cells(chars.slice(i + 1), wide)}</span>`;
      cursorAt = null;
    } else {
      html += `<span style="${style}">${cells(chars, wide)}</span>`;
    }
    col += width;
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
    const head = b.kind === "ai" && b.command ? `<div class="cmd"><span class="pr">✦</span> ${esc(b.command)}</div>`
      : b.kind === "command" && b.command
      ? `<div class="bh">${chips(b.cwd, b.branch)}${status}</div><div class="cmd"><span class="pr">❯</span> ${esc(b.command)}</div>` : "";
    const [cr, cc] = (live && b.cursor) || [-1, -1];
    const out = b.lines.length ? `<pre class="term">${b.lines.map((r, i) => rowHtml(r, i === cr ? cc : null)).join("\n")}</pre>` : "";
    return `<div class="block ${b.exit && b.exit !== 0 ? "failed" : ""} ${b.running ? "running" : ""}">${head}${out}</div>`;
  }).join("");
  const ed = sc.editor && (live ? shown(p.id, sc.editor) : sc.editor);
  if (!ed) return blocks;
  const atEnd = ed.cursor >= Array.from(ed.text).length;
  const ghost = live && atEnd && ed.suggestion ? `<span class="ghost" id="ghost">${esc(ed.suggestion)}</span>` : "";
  const runs = colored(ed);
  const line = live ? rowHtml(runs, atEnd && ghost ? null : ed.cursor) : rowHtml(runs, null);
  const caret = live && atEnd && ghost ? `<span class="cur"> </span>` : "";
  const menu = live && ed.menu ? `<div class="menu">${ed.menu.items.map(([label, kind], i) =>
    `<div class="mi ${i === ed.menu.selected ? "sel" : ""}" data-mi="${i}"><span class="k ${kind}"></span>${esc(label)}</div>`).join("")}</div>` : "";
  return blocks + `<div class="block prompt ${ed.ai ? "ai" : ""}"><div class="bh">${chips(ed.cwd, ed.branch)}${ed.ai ? `<span class="chip ai">AI</span>` : ""}</div>
    <div class="cmd"><span class="pr">${ed.ai ? "✦" : "❯"}</span> ${line}${caret}${ghost}${!ed.text && !ghost && live ? `<span class="hint">Tap to type</span>` : ""}</div>${menu}</div>`;
}

// ---- an agent waiting on you, or done
// A tab whose agent goes from running to waiting or done gets a banner,
// unless you're already looking at it. Waiting ones answer from here.
const agents = new Map(); // "window:pane" -> last state
const alerts = new Map(); // same key -> { w, t, p, state }
function notice() {
  const seen = new Set();
  for (const w of windows) for (const t of w.tabs) for (const p of t.panes) {
    const id = `${w.no}:${p.id}`, was = agents.get(id);
    seen.add(id);
    agents.set(id, p.agent);
    const looking = level === "pane" && at && at.window === w.no && at.pane === p.id && !document.hidden;
    if ((p.agent === "waiting" || p.agent === "done") && was && was !== p.agent && !looking) {
      alerts.set(id, { w: w.no, t: t.index, p: p.id, state: p.agent, title: t.title, what: p.title });
      if (navigator.vibrate) navigator.vibrate(60);
    } else if (p.agent !== "waiting" && p.agent !== "done" || looking) {
      alerts.delete(id);
    }
  }
  for (const id of alerts.keys()) if (!seen.has(id)) alerts.delete(id);
  drawAlerts();
}
function drawAlerts() {
  $("alerts").innerHTML = [...alerts.entries()].slice(-3).map(([id, a]) => `
    <div class="alert ${a.state}" data-alert="${id}">${dot(a.state)}
      <div class="t"><div>${a.t + 1}. ${esc(a.title)} ${a.state === "waiting" ? "needs you" : "is done"}</div><div>${esc(a.what)}</div></div>
      ${a.state === "waiting" ? `<button data-do="deny">Deny</button><button class="yes" data-do="allow">Allow</button>` : ""}
      <button data-do="open">Open</button>
    </div>`).join("");
}
$("alerts").onclick = (e) => {
  const el = e.target.closest("[data-alert]"), act = e.target.dataset.do;
  if (!el || !act) return;
  const a = alerts.get(el.dataset.alert);
  alerts.delete(el.dataset.alert);
  drawAlerts();
  if (!a) return;
  // Claude Code's prompt: Return takes the first choice (yes), Esc declines.
  if (act === "allow") send({ type: "key", window: a.w, pane: a.p, key: "Enter", mods: [] });
  else if (act === "deny") send({ type: "key", window: a.w, pane: a.p, key: "Escape", mods: [] });
  else go("pane", { window: a.w, tab: a.t, pane: a.p });
};

// The command line in the Mac's colors. Typed ahead of the Mac, the part
// it has colored keeps them; the rest is plain until it catches up.
function colored(ed) {
  const runs = ed.runs || [], have = runs.map((r) => r[0]).join("");
  if (have === ed.text) return runs;
  if (have && ed.text.startsWith(have)) return [...runs, [ed.text.slice(have.length), "var(--text)", null, 0]];
  return [[ed.text, "var(--text)", null, 0]];
}

// Typing shows at once: keys that edit the line are applied here too, and
// the Mac's line takes over as soon as it catches up (or after a moment,
// if it went another way).
const predicted = new Map(); // pane id -> { text, cursor, at, base }
function shown(id, ed) {
  const guess = predicted.get(id);
  if (!guess) return ed;
  if ((guess.text === ed.text && guess.cursor === ed.cursor) || Date.now() - guess.at > 1500) { predicted.delete(id); return ed; }
  // The Mac's suggestion still fits if what's been typed since is its start.
  let suggestion = null;
  if (ed.suggestion && guess.text.startsWith(ed.text)) {
    const extra = guess.text.slice(ed.text.length);
    if (ed.suggestion.startsWith(extra) && ed.suggestion.length > extra.length) suggestion = ed.suggestion.slice(extra.length);
  }
  return { ...ed, text: guess.text, cursor: guess.cursor, suggestion, menu: null };
}
// After a key that can't be guessed (Tab, ^U…), guesses wait for the Mac's
// next state: built on the old line, they'd be wrong.
let unsure = false;
function predict(op, arg) {
  const p = currentPane(), sc = p && p.screen;
  if (!sc || sc.mode !== "blocks" || !sc.editor || sc.editor.menu || unsure) return;
  const now = shown(p.id, sc.editor);
  const chars = Array.from(now.text);
  let cursor = Math.min(now.cursor, chars.length);
  if (op === "text") { if (/[\r\n]/.test(arg)) return predicted.delete(p.id); chars.splice(cursor, 0, ...Array.from(arg)); cursor += Array.from(arg).length; }
  else if (op === "Backspace") { if (cursor > 0) { chars.splice(cursor - 1, 1); cursor--; } }
  else if (op === "Delete") chars.splice(cursor, 1);
  else if (op === "Left") cursor = Math.max(0, cursor - 1);
  else if (op === "Right" && cursor < chars.length) cursor++;
  else if (op === "Home") cursor = 0;
  else if (op === "End" && !now.suggestion) cursor = chars.length;
  else { unsure = true; return predicted.delete(p.id); } // Enter, Tab, Up…: wait for the Mac
  predicted.set(p.id, { text: chars.join(""), cursor, at: Date.now() });
  render();
}
function currentPane() {
  const w = at && win(at.window);
  return w && paneOf(tabOf(w, at.tab), at.pane);
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
const key = (k, mods = []) => {
  if (!at) return;
  if (follow || starting) return held.push(() => key(k, mods));
  if (!mods.length) predict(k); else { const p = currentPane(); if (p) predicted.delete(p.id); unsure = true; }
  send({ type: "key", window: at.window, pane: at.pane, key: k, mods });
};
const held = []; // typed before a new tab arrived: for that tab
const text = (t) => { if (!at || !t) return; if (follow || starting) return held.push(() => text(t)); predict("text", t); send({ type: "text", window: at.window, pane: at.pane, text: t }); };
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

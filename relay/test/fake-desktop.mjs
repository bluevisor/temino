// A stand-in Mac for working on the web app (/app) without a desktop build:
// signs in to a local relay (`npm run dev`, DEV_LOGIN=1) as a desktop,
// lets any phone in, and shows two tabs: Claude Code (a full-screen
// program, the cursor in its input box) and a shell (finished commands
// and a command line being typed).
//   node test/fake-desktop.mjs        then open http://127.0.0.1:8787/app
import { Session, generateKeys, hex, b64, fromB64 } from "../public/noise.js";

const RELAY = process.env.RELAY || "http://127.0.0.1:8787";

async function devSignIn(pub) {
  const q = new URLSearchParams({ return: "http://127.0.0.1:9/done", state: "s", pub: hex(pub), name: "Fake Mac", kind: "desktop" });
  const page = await (await fetch(`${RELAY}/login?${q}`)).text();
  const href = page.match(/href="(\/auth\/dev\?req=[^"]+)"/)[1].replace(/&amp;/g, "&");
  const back = (await fetch(RELAY + href, { redirect: "manual" })).headers.get("location");
  return new URL(back).searchParams.get("token");
}

const W = "#e6e6e6", DIM = "#8a8a8a", ACC = "#d97757";
const row = (text, fg = W) => [[text, fg, null, 0]];
let key = 0;
const lines = (rows) => rows.map((v) => ({ k: `l${key++}`, v }));

const claude = [
  row("● I'll split the session code into its own file.", W),
  row(""),
  row("  Read portald.c (490 lines)", DIM),
  row("  Update(server/session.c)", DIM),
  row("  ⎿  Added 180 lines", DIM),
  row(""),
  row("● Done: sessions, takeover and accept now live in session.c;", W),
  row("  portald.c keeps config and main. All 231 checks pass.", W),
  row(""),
  row("╭──────────────────────────────────────────────╮", DIM),
  row("│ > now run the clipboard tests too            │", W),
  row("╰──────────────────────────────────────────────╯", DIM),
  row("  ? for shortcuts                ◯ IDE disconnected", DIM),
];
const shell = {
  mode: "blocks", columns: 80,
  blocks: [
    { kind: "command", command: "ls", cwd: "/Users/me/portal", branch: "main", exit: 0, secs: 0.02,
      lines: lines([row("README.md  client  server  server-haiku  server-mac")]) },
    { kind: "command", command: "git log --oneline -4", cwd: "/Users/me/portal", branch: "main", exit: 0, secs: 0.05,
      lines: lines([row("912731e Haiku host; Options per host OS"), row("643528f Split the three files over 1,000 lines"),
        row("9963539 Tests and CI"), row("f57fd07 Clipboard sync between the viewer and every host"), row("")]) },
  ],
  editor: { text: "git status --short", cursor: 18, runs: [["git status --short", W, null, 0]], cwd: "/Users/me/portal", branch: "main" },
};
const state = () => ({
  type: "state",
  windows: [{ no: 1, active: 0, tabs: [
    { index: 0, title: "claude", focus: 1, panes: [{ id: 1, title: "Claude Code", agent: "waiting",
      screen: { mode: "grid", columns: 50, cursor: [10, 34], lines: lines(claude) } }] },
    { index: 1, title: "zsh", focus: 2, panes: [{ id: 2, title: "~/portal", screen: shell }] },
  ] }],
});

const keys = await generateKeys(true);
const token = await devSignIn(keys.public);
const ws = new WebSocket(`${RELAY.replace("http", "ws")}/connect?token=${encodeURIComponent(token)}`);
const sessions = new Map(); // phone device -> Session
const seal = async (to, message) => {
  for (const f of await sessions.get(to).seal(JSON.stringify(message)))
    ws.send(JSON.stringify({ type: "send", to, kind: "msg", data: b64(f) }));
};
ws.onmessage = async (event) => {
  const m = JSON.parse(event.data);
  if (m.type !== "recv") return;
  const data = fromB64(m.data);
  if (m.kind === "hs") {
    let s = sessions.get(m.from);
    if (!s || s.open) { s = await Session.respond(keys); sessions.set(m.from, s); }
    const reply = await s.handshake(data);
    if (reply) ws.send(JSON.stringify({ type: "send", to: m.from, kind: "hs", data: b64(reply) }));
    if (s.open) { await seal(m.from, { type: "hello", device: "Fake Mac", version: "test" }); await seal(m.from, state()); }
    return;
  }
  const s = sessions.get(m.from);
  const plain = s && s.open && await s.openFrame(data);
  if (!plain) return;
  const msg = JSON.parse(new TextDecoder().decode(plain));
  if (msg.type === "watch" || msg.type === "focus" || msg.type === "tab") await seal(m.from, state());
};
ws.onopen = () => console.log("fake desktop online");
ws.onclose = (e) => { console.log("relay closed the connection", e.code, e.reason); process.exit(1); };
setInterval(() => ws.readyState === 1 && ws.send(JSON.stringify({ type: "visible", on: true })), 20000); // stay connected

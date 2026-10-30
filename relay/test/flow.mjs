// End to end through a local relay (`npm run dev`, DEV_LOGIN=1):
//   node test/flow.mjs desktop <home>   sign a desktop in, write its
//                                       settings and key under <home>
//   node test/flow.mjs phone   <home>   sign a phone in and drive that
//                                       desktop: handshake, approval, a
//                                       command and its output
import { writeFileSync, appendFileSync, mkdirSync } from "node:fs";
import { Session, generateKeys, hex, b64, fromB64 } from "../public/noise.js";

const RELAY = process.env.RELAY || "http://127.0.0.1:8787";
const [, , role, home] = process.argv;
const dir = `${home}/Library/Application Support/Temino`;

async function devSignIn(pub, kind, name) {
  const q = new URLSearchParams({ return: "http://127.0.0.1:9/done", state: "s", pub: hex(pub), name, kind });
  const page = await (await fetch(`${RELAY}/login?${q}`)).text();
  const href = page.match(/href="(\/auth\/dev\?req=[^"]+)"/)[1].replace(/&amp;/g, "&");
  const back = (await fetch(RELAY + href, { redirect: "manual" })).headers.get("location");
  const params = new URL(back).searchParams;
  return { token: params.get("token"), device: params.get("device") };
}

if (role === "desktop") {
  const keys = await generateKeys(true);
  const pkcs8 = new Uint8Array(await crypto.subtle.exportKey("pkcs8", keys.privateKey));
  const { token, device } = await devSignIn(keys.public, "desktop", "Test Mac");
  mkdirSync(dir, { recursive: true });
  writeFileSync(`${dir}/remote_key`, `${hex(pkcs8.slice(-32))}\n${hex(keys.public)}\n`);
  writeFileSync(`${dir}/settings`, `relay=${RELAY}\nrelay_token=${token}\nrelay_device=${device}\n`);
  console.log("desktop signed in as", device);
  process.exit(0);
}

const keys = await generateKeys();
const { token } = await devSignIn(keys.public, "phone", "Test Phone");
const ws = new WebSocket(`${RELAY.replace("http", "ws")}/connect?token=${encodeURIComponent(token)}`);
let session, machine, stage = "handshake", approvedOnce = false;
const send = (to, kind, bytes) => ws.send(JSON.stringify({ type: "send", to, kind, data: b64(bytes) }));
const seal = async (message) => { for (const f of await session.seal(JSON.stringify(message))) send(machine, "msg", f); };
async function start() {
  const [s, first] = await Session.initiate(keys);
  session = s;
  send(machine, "hs", first);
}
ws.onmessage = async (event) => {
  const m = JSON.parse(event.data);
  if (m.type === "devices" && !machine) {
    const desktop = m.devices.find((d) => d.kind === "desktop" && d.online);
    if (!desktop) return;
    machine = desktop.id;
    console.log("phone sees", desktop.name);
    await start();
  } else if (m.type === "recv" && m.from === machine) {
    const data = fromB64(m.data);
    if (m.kind === "hs") {
      const reply = await session.handshake(data);
      if (reply) send(machine, "hs", reply);
      return;
    }
    const message = JSON.parse(new TextDecoder().decode(await session.openFrame(data)));
    if (message.type === "pending") {
      console.log("pending, code", message.code);
      if (approvedOnce) throw new Error("still pending after approval");
      // Stand in for Allow on the desktop: approve this key, then start over.
      approvedOnce = true;
      appendFileSync(`${dir}/remote_peers`, `${hex(keys.public)}\tTest Phone\n`);
      await start();
    } else if (message.type === "hello") {
      console.log("in:", message.device, message.version);
    } else if (message.type === "state") {
      const w = message.windows[0];
      if (!w) return;
      const t = w.tabs.find((u) => u.index === w.active) || w.tabs[0], pane = t.focus, p = t.panes.find((q) => q.id === pane);
      const lines = p.screen && (p.screen.mode === "grid" ? p.screen.lines : p.screen.blocks.flatMap((b) => b.lines))
        .map((runs) => runs.map((r) => r[0]).join(""));
      if (stage === "handshake") {
        stage = "watch";
        await seal({ type: "watch", panes: [[w.no, pane]] });
      } else if (stage === "watch" && lines) {
        stage = "typed";
        await seal({ type: "text", window: w.no, pane, text: "echo through-the-relay" });
        await seal({ type: "key", window: w.no, pane, key: "Enter" });
      } else if (stage === "typed" && lines && lines.some((l) => l.trim() === "through-the-relay")) {
        console.log("screen:\n" + lines.slice(-3).join("\n"));
        console.log("OK");
        process.exit(0);
      }
    }
  }
};
setTimeout(() => { console.log("timeout at", stage); process.exit(1); }, 20000);

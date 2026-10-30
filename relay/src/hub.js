// One account's hub: its devices, and the sockets of those online.
//
// To devices:
//   {"type":"devices","you":id,"devices":[{id,name,kind,pub,online}]}
//   {"type":"recv","from":id,"kind":"hs"|"msg","data":base64}
// From devices:
//   {"type":"send","to":id,"kind":"hs"|"msg","data":base64}
//   {"type":"forget","id":id}     sign a device out of the account
//
// `data` is a Noise frame: the hub sees who talks to whom, and how much,
// never what. It doesn't vouch for keys either: the desktop asks you
// before it trusts a phone's.

import { DurableObject } from "cloudflare:workers";

// Cloudflare takes up to 1 MiB a message; Noise frames are under 64 KiB.
const MAX_MESSAGE = 200 * 1024;

export class Hub extends DurableObject {
  async devices() {
    return (await this.ctx.storage.get("devices")) || {};
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/register") {
      const device = await request.json();
      const devices = await this.devices();
      devices[device.id] = { name: device.name, kind: device.kind, pub: device.pub, added: Date.now() };
      await this.ctx.storage.put("devices", devices);
      await this.announce();
      return new Response("ok");
    }
    if (url.pathname === "/connect") {
      const id = request.headers.get("x-device");
      const devices = await this.devices();
      if (!devices[id]) return new Response("signed out", { status: 401 });
      const [client, server] = Object.values(new WebSocketPair());
      // A second socket for the same device replaces the first.
      for (const old of this.ctx.getWebSockets(id)) old.close(4000, "replaced");
      this.ctx.acceptWebSocket(server, [id]);
      await this.announce();
      return new Response(null, { status: 101, webSocket: client });
    }
    return new Response("not found", { status: 404 });
  }

  async webSocketMessage(socket, raw) {
    if (typeof raw !== "string" || raw.length > MAX_MESSAGE) return;
    let message;
    try {
      message = JSON.parse(raw);
    } catch {
      return;
    }
    const [from] = this.ctx.getTags(socket);
    if (message.type === "send" && typeof message.to === "string" && typeof message.data === "string") {
      const kind = message.kind === "hs" ? "hs" : "msg";
      const out = JSON.stringify({ type: "recv", from, kind, data: message.data });
      for (const peer of this.ctx.getWebSockets(message.to)) {
        try {
          peer.send(out);
        } catch {}
      }
    } else if (message.type === "forget" && typeof message.id === "string") {
      const devices = await this.devices();
      delete devices[message.id];
      await this.ctx.storage.put("devices", devices);
      for (const gone of this.ctx.getWebSockets(message.id)) gone.close(4001, "signed out");
      await this.announce();
    }
  }

  async webSocketClose() {
    await this.announce();
  }

  async webSocketError() {
    await this.announce();
  }

  // Every device online hears who's in the account and who's online.
  async announce() {
    const devices = await this.devices();
    const sockets = this.ctx.getWebSockets();
    const online = new Set(sockets.filter((s) => s.readyState === 1).map((s) => this.ctx.getTags(s)[0]));
    const list = Object.entries(devices).map(([id, d]) => ({ id, name: d.name, kind: d.kind, pub: d.pub, online: online.has(id) }));
    for (const socket of sockets) {
      try {
        socket.send(JSON.stringify({ type: "devices", you: this.ctx.getTags(socket)[0], devices: list }));
      } catch {}
    }
  }
}

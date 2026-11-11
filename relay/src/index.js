// Temino's relay: signs devices in with Apple or Google, and passes
// sealed frames between the devices of one account. It can't read them:
// the desktop and the phone run Noise end to end (see noise.js and
// src/e2e.rs), and the desktop only obeys phones you approved on it.
//
// Routes:
//   GET  /login            pick Apple or Google; the device's public key,
//                          name and where to return ride along, signed
//   GET  /auth/google      → Google → /auth/google/callback
//   GET  /auth/apple       → Apple  → POST /auth/apple/callback
//   GET  /auth/dev         local `wrangler dev` only (DEV_LOGIN=1)
//   GET  /connect          WebSocket, ?token=… : the account's hub
//   GET  /app              the web client (public/)
//   POST /analytics        anonymous usage data per install (with its
//                          permission): a random id (hashed before
//                          storage), version, OS, arch, shell kind, GPU,
//                          and feature-use counters from a fixed list
//   GET  /analytics/summary the last 30 days of pings; needs the relay
//                          secret as a Bearer token
//
// A sign-in ends with a device token, sent back to the device's own
// loopback port, its app's URL scheme, or this origin's /app: nowhere
// else, so a link can't send someone's token away.

import { Hub } from "./hub.js";
import { verifyIdToken, appleClientSecret } from "./jwt.js";
import { RETENTION_DAYS, dayKey, hashId, summarize, validPing } from "./analytics.js";
export { Hub };

const enc = new TextEncoder();
const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const fromB64url = (text) => Uint8Array.from(atob(text.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
const random = (n = 16) => b64url(crypto.getRandomValues(new Uint8Array(n)));

async function hmacKey(secret) {
  return crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
}

// A small signed blob: the login request through the OAuth round trip,
// and a device's token.
export async function sign(env, value) {
  const body = b64url(enc.encode(JSON.stringify(value)));
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", await hmacKey(env.RELAY_SECRET), enc.encode(body)));
  return `${body}.${b64url(mac)}`;
}

export async function unsign(env, token) {
  const [body, mac] = String(token || "").split(".");
  if (!body || !mac) return null;
  let ok = false;
  try {
    ok = await crypto.subtle.verify("HMAC", await hmacKey(env.RELAY_SECRET), fromB64url(mac), enc.encode(body));
  } catch {}
  if (!ok) return null;
  const value = JSON.parse(new TextDecoder().decode(fromB64url(body)));
  return value.exp && value.exp < Date.now() ? null : value;
}

// Where a token may go: the device's loopback port, the app's scheme, or
// this origin's web client.
function returnAllowed(url, origin) {
  try {
    const u = new URL(url);
    if (u.protocol === "http:" && (u.hostname === "127.0.0.1" || u.hostname === "localhost")) return true;
    if (u.protocol === "temino:") return true;
    return u.origin === origin && u.pathname === "/app";
  } catch {
    return false;
  }
}

const html = (body, status = 200) =>
  new Response(body, { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } });
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function page(title, inner) {
  return html(`<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1"><title>${esc(title)}</title>
<style>
:root{--bg:#f6f7f9;--card:#fff;--text:#16181d;--dim:#5b6270;--line:#e3e6eb;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root{--bg:#111317;--card:#1a1d23;--text:#e6e8ec;--dim:#9aa1ad;--line:#2c313a}}
*{box-sizing:border-box}body{margin:0;min-height:100dvh;display:grid;place-items:center;background:var(--bg);color:var(--text);font:16px -apple-system,system-ui,sans-serif;padding:16px}
main{width:100%;max-width:360px;background:var(--card);border:1px solid var(--line);border-radius:14px;padding:28px 24px;text-align:center}
h1{font-size:20px;margin:0 0 6px}p{color:var(--dim);margin:0 0 22px;line-height:1.4}
a.b{display:block;padding:12px;border-radius:10px;margin-top:10px;text-decoration:none;font-weight:600;border:1px solid var(--line);color:var(--text)}
a.apple{background:#000;color:#fff;border-color:#000}
</style></head><body><main>${inner}</main></body></html>`);
}

async function login(request, env, url) {
  const p = url.searchParams;
  const ret = p.get("return") || "";
  if (!returnAllowed(ret, url.origin)) return page("Temino", "<h1>Can't sign in</h1><p>That sign-in link isn't one of Temino's.</p>");
  const pub = p.get("pub") || "";
  // The account the phone's Mac is signed in with, "you@example.com (Google)".
  const mac = (p.get("for") || "").slice(0, 160);
  const google = /^(\S+@\S+) \(Google\)$/.exec(mac);
  if (!/^[0-9a-f]{64}$/.test(pub)) return page("Temino", "<h1>Can't sign in</h1><p>The device's key is missing.</p>");
  const req = await sign(env, {
    ret, state: p.get("state") || "", pub, nonce: random(),
    name: (p.get("name") || "Device").slice(0, 60), kind: p.get("kind") === "desktop" ? "desktop" : "phone",
    // Google's chooser starts on the account the Mac uses.
    hint: google ? google[1] : "",
    exp: Date.now() + 10 * 60 * 1000,
  });
  const q = `?req=${encodeURIComponent(req)}`;
  const dev = env.DEV_LOGIN === "1" ? `<a class="b" href="/auth/dev${q}">Dev sign-in (local only)</a>` : "";
  return page("Sign in to Temino", `<h1>Sign in to Temino</h1>
<p>Connect <b>${esc(p.get("name") || "this device")}</b> to your other Temino devices.</p>${mac
    ? `<p>Your Mac is signed in as <b>${esc(mac)}</b>. Use that same account here.</p>` : ""}
<a class="b apple" href="/auth/apple${q}">Continue with Apple</a>
<a class="b" href="/auth/google${q}">Continue with Google</a>${dev}`);
}

// The login request waits here through the OAuth round trip; Google or
// Apple carries only its short id as `state`. The whole signed request
// was nearly 400 characters, and switching accounts on Google's page
// nests the sign-in URL in its own until Google calls it malformed.
// Each id works once, for ten minutes.
async function hold(env, req) {
  const id = random();
  await env.ACCOUNTS.put(`login:${id}`, JSON.stringify(req), { expirationTtl: 600 });
  return id;
}

async function claim(env, id) {
  if (!/^[A-Za-z0-9_-]{22}$/.test(id || "")) return null;
  const key = `login:${id}`;
  const raw = await env.ACCOUNTS.get(key);
  if (!raw) return null;
  await env.ACCOUNTS.delete(key);
  const req = JSON.parse(raw);
  return req.exp && req.exp < Date.now() ? null : req;
}

async function google(env, url) {
  const req = await unsign(env, url.searchParams.get("req"));
  if (!req) return page("Temino", "<h1>Sign-in expired</h1><p>Start again from Temino.</p>");
  const q = new URLSearchParams({
    client_id: env.GOOGLE_CLIENT_ID, redirect_uri: `${url.origin}/auth/google/callback`,
    response_type: "code", scope: "openid email profile", state: await hold(env, req), nonce: req.nonce,
    prompt: "select_account",
  });
  if (req.hint) q.set("login_hint", req.hint);
  return Response.redirect(`https://accounts.google.com/o/oauth2/v2/auth?${q}`, 302);
}

async function googleCallback(env, url) {
  const req = await claim(env, url.searchParams.get("state"));
  const code = url.searchParams.get("code");
  if (!req || !code) return page("Temino", "<h1>Sign-in didn't finish</h1><p>Start again from Temino.</p>");
  const response = await fetch("https://oauth2.googleapis.com/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      code, client_id: env.GOOGLE_CLIENT_ID, client_secret: env.GOOGLE_CLIENT_SECRET,
      redirect_uri: `${url.origin}/auth/google/callback`, grant_type: "authorization_code",
    }),
  });
  const tokens = await response.json();
  const claims = await verifyIdToken(tokens.id_token, {
    jwks: "https://www.googleapis.com/oauth2/v3/certs",
    issuers: ["https://accounts.google.com", "accounts.google.com"],
    audience: env.GOOGLE_CLIENT_ID, nonce: req.nonce,
  });
  if (!claims) return page("Temino", "<h1>Google sign-in failed</h1><p>Start again from Temino.</p>");
  return finish(env, url, req, {
    provider: "google", sub: claims.sub, email: claims.email, verified: claims.email_verified === true,
    person: claims.given_name || claims.name || "",
    picture: /^https:\/\//.test(claims.picture || "") ? claims.picture : "",
  });
}

async function apple(env, url) {
  const req = await unsign(env, url.searchParams.get("req"));
  if (!req) return page("Temino", "<h1>Sign-in expired</h1><p>Start again from Temino.</p>");
  const q = new URLSearchParams({
    client_id: env.APPLE_CLIENT_ID, redirect_uri: `${url.origin}/auth/apple/callback`,
    response_type: "code", response_mode: "form_post", scope: "email",
    state: await hold(env, req), nonce: req.nonce,
  });
  return Response.redirect(`https://appleid.apple.com/auth/authorize?${q}`, 302);
}

async function appleCallback(request, env, url) {
  const form = await request.formData();
  const req = await claim(env, form.get("state"));
  const code = form.get("code");
  if (!req || !code) return page("Temino", "<h1>Sign-in didn't finish</h1><p>Start again from Temino.</p>");
  const response = await fetch("https://appleid.apple.com/auth/token", {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      code, client_id: env.APPLE_CLIENT_ID, client_secret: await appleClientSecret(env),
      redirect_uri: `${url.origin}/auth/apple/callback`, grant_type: "authorization_code",
    }),
  });
  const tokens = await response.json();
  const claims = await verifyIdToken(tokens.id_token, {
    jwks: "https://appleid.apple.com/auth/keys", issuers: ["https://appleid.apple.com"],
    audience: env.APPLE_CLIENT_ID, nonce: req.nonce,
  });
  if (!claims) return page("Temino", "<h1>Apple sign-in failed</h1><p>Start again from Temino.</p>");
  const verified = claims.email_verified === true || claims.email_verified === "true";
  return finish(env, url, req, { provider: "apple", sub: claims.sub, email: claims.email, verified });
}

async function dev(env, url) {
  if (env.DEV_LOGIN !== "1") return new Response("not found", { status: 404 });
  const req = await unsign(env, url.searchParams.get("req"));
  if (!req) return page("Temino", "<h1>Sign-in expired</h1>");
  return finish(env, url, req, { provider: "dev", sub: "dev", email: "dev@example.com", verified: false });
}

// The account a sign-in belongs to: the one this identity used before;
// else, for a verified real address (not Apple's relay), the account that
// address already has, so Apple and Google with the same email meet;
// else a new one.
async function account(env, who) {
  const byId = `id:${who.provider}:${who.sub}`;
  let id = await env.ACCOUNTS.get(byId);
  const email = who.verified && who.email && !/@privaterelay\.appleid\.com$/i.test(who.email)
    ? `email:${who.email.toLowerCase()}` : null;
  if (!id && email) id = await env.ACCOUNTS.get(email);
  if (!id) id = random();
  await env.ACCOUNTS.put(byId, id);
  if (email && !(await env.ACCOUNTS.get(email))) await env.ACCOUNTS.put(email, id);
  return id;
}

function hub(env, accountId) {
  return env.HUB.get(env.HUB.idFromName(accountId), { locationHint: "wnam" });
}

async function finish(env, url, req, who) {
  const accountId = await account(env, who);
  const device = random(12);
  await hub(env, accountId).fetch("https://hub/register", {
    method: "POST",
    body: JSON.stringify({
      id: device, name: req.name, kind: req.kind, pub: req.pub,
      person: String(who.person || "").slice(0, 40), picture: String(who.picture || "").slice(0, 500),
      // Which account it signed in with, for "Signed in as …".
      email: String(who.email || "").slice(0, 120), provider: who.provider,
    }),
  });
  const token = await sign(env, { a: accountId, d: device });
  const back = new URL(req.ret);
  const params = new URLSearchParams({ token, device, state: req.state });
  // To the web client in the fragment, so it never reaches a server log.
  if (back.origin === url.origin) back.hash = params.toString();
  else for (const [k, v] of params) back.searchParams.set(k, v);
  return Response.redirect(back.toString(), 302);
}

async function connect(request, env, url) {
  if (request.headers.get("upgrade") !== "websocket") return new Response("expected a WebSocket", { status: 426 });
  const token = await unsign(env, url.searchParams.get("token"));
  if (!token) return new Response("bad token", { status: 401 });
  const headers = new Headers(request.headers);
  headers.set("x-device", token.d);
  return hub(env, token.a).fetch(new Request("https://hub/connect", { headers }));
}

// One ping a day per install, stored under the day and the hashed id —
// so one install counts once a day however often it pings, and the raw
// id is never stored. Its counters also add into the day's aggregate.
// Entries expire after RETENTION_DAYS.
async function analytics(request, env) {
  const body = await request.text();
  if (body.length > 4096) return new Response("too large", { status: 400 });
  let value;
  try {
    value = JSON.parse(body);
  } catch {
    return new Response("bad json", { status: 400 });
  }
  const ping = validPing(value);
  if (!ping) return new Response("bad ping", { status: 400 });
  const day = dayKey(new Date());
  const ttl = { expirationTtl: RETENTION_DAYS * 24 * 60 * 60 };
  const key = `analytics:${day}:${await hashId(env.RELAY_SECRET, ping.id)}`;
  const stored = JSON.stringify({ version: ping.version, os: ping.os, arch: ping.arch, shell: ping.shell, gpu: ping.gpu, counters: ping.counters });
  await env.ACCOUNTS.put(key, stored, ttl);
  // The day's totals across installs (a read-modify-write; at this
  // scale a lost race just undercounts a ping).
  const aggKey = `analytics:agg:${day}`;
  let totals = {};
  try {
    totals = JSON.parse(await env.ACCOUNTS.get(aggKey)) || {};
  } catch {}
  for (const [name, n] of Object.entries(ping.counters)) totals[name] = (totals[name] || 0) + n;
  await env.ACCOUNTS.put(aggKey, JSON.stringify(totals), ttl);
  return new Response(null, { status: 204 });
}

// The last 30 days of pings, for the one person with the relay secret.
async function analyticsSummary(request, env) {
  if (request.headers.get("authorization") !== `Bearer ${env.RELAY_SECRET}`) {
    return new Response("unauthorized", { status: 401 });
  }
  const today = dayKey(new Date());
  const records = [];
  const aggregates = {};
  let cursor;
  do {
    const page = await env.ACCOUNTS.list({ prefix: "analytics:", cursor, limit: 1000 });
    for (const key of page.keys) {
      const [, day, hash] = key.name.split(":");
      if (day === "agg") {
        try {
          aggregates[hash] = JSON.parse(await env.ACCOUNTS.get(key.name)) || {};
        } catch {}
        continue;
      }
      const record = { day, hash, os: null, version: null, counters: {} };
      if (day === today) {
        try {
          const value = JSON.parse(await env.ACCOUNTS.get(key.name));
          record.os = value.os;
          record.version = value.version;
          record.counters = value.counters || {};
        } catch {}
      }
      records.push(record);
    }
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  const body = JSON.stringify(summarize(records, aggregates, today), null, 2);
  return new Response(body, { headers: { "content-type": "application/json" } });
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    switch (`${request.method} ${url.pathname}`) {
      case "GET /": return Response.redirect(`${url.origin}/app`, 302);
      case "GET /login": return login(request, env, url);
      case "GET /auth/google": return google(env, url);
      case "GET /auth/google/callback": return googleCallback(env, url);
      case "GET /auth/apple": return apple(env, url);
      case "POST /auth/apple/callback": return appleCallback(request, env, url);
      case "GET /auth/dev": return dev(env, url);
      case "GET /connect": return connect(request, env, url);
      case "GET /vapid": return new Response(env.VAPID_PUBLIC || "", { headers: { "content-type": "text/plain" } });
      case "POST /analytics": return analytics(request, env);
      case "GET /analytics/summary": return analyticsSummary(request, env);
    }
    return env.ASSETS.fetch(request);
  },
};

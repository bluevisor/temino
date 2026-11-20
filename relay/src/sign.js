// Small signed blobs, HMAC'd with the relay secret: the login request
// through the OAuth round trip, a device's token, and the dashboard's
// ticket and session.

const enc = new TextEncoder();
export const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const fromB64url = (text) => Uint8Array.from(atob(text.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
export const random = (n = 16) => b64url(crypto.getRandomValues(new Uint8Array(n)));

async function hmacKey(secret) {
  return crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
}

export async function sign(env, value) {
  const body = b64url(enc.encode(JSON.stringify(value)));
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", await hmacKey(env.RELAY_SECRET), enc.encode(body)));
  return `${body}.${b64url(mac)}`;
}

/// The value, or null when the signature is wrong or it has expired.
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

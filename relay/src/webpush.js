// Web Push from a Worker: a VAPID-signed request (RFC 8292) carrying a
// payload encrypted for the browser's subscription (RFC 8291, aes128gcm).

const enc = new TextEncoder();
const b64url = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const fromB64url = (text) => Uint8Array.from(atob(text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4)), (c) => c.charCodeAt(0));
const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
};

async function hkdf(salt, ikm, info, length) {
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  return new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info }, key, length * 8));
}

// The VAPID key pair: VAPID_PRIVATE is the private key as a JWK's `d`,
// VAPID_PUBLIC the uncompressed public point, both base64url.
async function vapid(env, audience) {
  const pub = fromB64url(env.VAPID_PUBLIC);
  const jwk = { kty: "EC", crv: "P-256", d: env.VAPID_PRIVATE, x: b64url(pub.slice(1, 33)), y: b64url(pub.slice(33, 65)), ext: true };
  const key = await crypto.subtle.importKey("jwk", jwk, { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const head = b64url(enc.encode(JSON.stringify({ typ: "JWT", alg: "ES256" })));
  const body = b64url(enc.encode(JSON.stringify({ aud: audience, exp: Math.floor(Date.now() / 1000) + 12 * 3600, sub: "mailto:bluevisor@gmail.com" })));
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, enc.encode(`${head}.${body}`));
  return `vapid t=${head}.${body}.${b64url(sig)}, k=${env.VAPID_PUBLIC}`;
}

async function encrypt(subscription, plain) {
  const uaPublic = fromB64url(subscription.keys.p256dh);
  const auth = fromB64url(subscription.keys.auth);
  const mine = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"]);
  const asPublic = new Uint8Array(await crypto.subtle.exportKey("raw", mine.publicKey));
  const theirs = await crypto.subtle.importKey("raw", uaPublic, { name: "ECDH", namedCurve: "P-256" }, false, []);
  const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: theirs }, mine.privateKey, 256));
  const ikm = await hkdf(auth, shared, concat(enc.encode("WebPush: info\0"), uaPublic, asPublic), 32);
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const cek = await hkdf(salt, ikm, enc.encode("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, ikm, enc.encode("Content-Encoding: nonce\0"), 12);
  const key = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["encrypt"]);
  const sealed = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, key, concat(enc.encode(plain), new Uint8Array([2]))));
  const header = new Uint8Array(21);
  header.set(salt);
  new DataView(header.buffer).setUint32(16, 4096);
  header[20] = asPublic.length;
  return concat(header, asPublic, sealed);
}

// Sends `message` (a small JSON object) to one subscription. Returns the
// push service's status: 404 and 410 mean the subscription is gone.
export async function push(env, subscription, message) {
  const url = new URL(subscription.endpoint);
  const response = await fetch(subscription.endpoint, {
    method: "POST",
    headers: {
      authorization: await vapid(env, url.origin),
      "content-encoding": "aes128gcm",
      "content-type": "application/octet-stream",
      ttl: "3600",
      urgency: "high",
    },
    body: await encrypt(subscription, JSON.stringify(message)),
  });
  return response.status;
}

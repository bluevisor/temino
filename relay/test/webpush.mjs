// The Web Push payload the relay builds decrypts the way a browser does it
// (RFC 8291), and its VAPID header verifies. Run: node test/webpush.mjs
import { push } from "../src/webpush.js";

const enc = new TextEncoder();
const b64url = (b) => Buffer.from(b).toString("base64url");
const hkdf = async (salt, ikm, info, n) => new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info },
  await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]), n * 8));
const concat = (...p) => Uint8Array.from(p.flatMap((x) => [...x]));

// A browser's subscription, and a VAPID key pair.
const ua = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"]);
const uaPublic = new Uint8Array(await crypto.subtle.exportKey("raw", ua.publicKey));
const auth = crypto.getRandomValues(new Uint8Array(16));
const v = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
const vj = await crypto.subtle.exportKey("jwk", v.privateKey);
const env = { VAPID_PRIVATE: vj.d, VAPID_PUBLIC: b64url(concat([4], Buffer.from(vj.x, "base64url"), Buffer.from(vj.y, "base64url"))) };

let request;
globalThis.fetch = async (url, init) => { request = { url, init }; return new Response(null, { status: 201 }); };
const status = await push(env, { endpoint: "https://web.push.apple.com/abc", keys: { p256dh: b64url(uaPublic), auth: b64url(auth) } }, { title: "Mac", body: "Tab 2 needs you" });

// Decrypt as the browser would.
const body = new Uint8Array(request.init.body);
const salt = body.slice(0, 16), idlen = body[20], asPublic = body.slice(21, 21 + idlen), sealed = body.slice(21 + idlen);
const theirs = await crypto.subtle.importKey("raw", asPublic, { name: "ECDH", namedCurve: "P-256" }, false, []);
const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: theirs }, ua.privateKey, 256));
const ikm = await hkdf(auth, shared, concat(enc.encode("WebPush: info\0"), uaPublic, asPublic), 32);
const cek = await hkdf(salt, ikm, enc.encode("Content-Encoding: aes128gcm\0"), 16);
const nonce = await hkdf(salt, ikm, enc.encode("Content-Encoding: nonce\0"), 12);
const plain = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce },
  await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["decrypt"]), sealed));
const message = JSON.parse(new TextDecoder().decode(plain.slice(0, plain.lastIndexOf(2))));

// And the VAPID JWT verifies against the public key, for the right audience.
const [, t, k] = request.init.headers.authorization.match(/^vapid t=([^,]+), k=(.+)$/);
const [h, b, sig] = t.split(".");
const ok = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, v.publicKey, Buffer.from(sig, "base64url"), enc.encode(`${h}.${b}`));
const claims = JSON.parse(Buffer.from(b, "base64url"));

const pass = status === 201 && message.body === "Tab 2 needs you" && ok && k === env.VAPID_PUBLIC && claims.aud === "https://web.push.apple.com";
console.log(pass ? "OK" : "FAIL", message, claims.aud, ok);
process.exit(pass ? 0 : 1);

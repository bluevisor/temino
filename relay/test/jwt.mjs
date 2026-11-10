// Malformed ID tokens are refused (null), never a throw: the token is
// untrusted text until its signature checks out, and the sign-in routes
// answer a failed sign-in page, not a 500. Run: node test/jwt.mjs
import assert from "node:assert/strict";
import { verifyIdToken } from "../src/jwt.js";

const b64url = (bytes) => Buffer.from(bytes).toString("base64url");
const opts = { jwks: "https://example.invalid/keys", issuers: ["https://issuer.example"], audience: "client-1", nonce: "n-1" };

// No JWKS fetch may happen for a token that isn't even three parts.
let fetches = 0;
globalThis.fetch = async () => { fetches += 1; throw new Error("must not be called"); };

for (const bad of [undefined, null, 42, "", "a.b", "a.b.c.d", "..", "x.y.z", "e30.e30.e30"]) {
  assert.equal(await verifyIdToken(bad, opts), null, JSON.stringify(bad));
}
assert.equal(fetches, 0);

// A properly signed token whose claims aren't JSON: the signature checks
// out, then the claims can't be read — still a refusal, not a throw.
const pair = await crypto.subtle.generateKey(
  { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
  true, ["sign", "verify"]);
const jwk = await crypto.subtle.exportKey("jwk", pair.publicKey);
jwk.kid = "key-1";
globalThis.fetch = async () => new Response(JSON.stringify({ keys: [jwk] }), { headers: { "content-type": "application/json" } });

const enc = new TextEncoder();
const head = b64url(enc.encode(JSON.stringify({ alg: "RS256", kid: "key-1" })));
const body = b64url(enc.encode("this is not json"));
const sig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, enc.encode(`${head}.${body}`));
assert.equal(await verifyIdToken(`${head}.${body}.${b64url(new Uint8Array(sig))}`, opts), null);

// And a fully valid one still verifies, so the refusals above aren't
// just everything failing.
const now = Math.floor(Date.now() / 1000);
const claims = { iss: "https://issuer.example", aud: "client-1", exp: now + 600, nonce: "n-1", sub: "user-1" };
const goodBody = b64url(enc.encode(JSON.stringify(claims)));
const goodSig = await crypto.subtle.sign("RSASSA-PKCS1-v1_5", pair.privateKey, enc.encode(`${head}.${goodBody}`));
assert.deepEqual(await verifyIdToken(`${head}.${goodBody}.${b64url(new Uint8Array(goodSig))}`, opts), claims);

console.log("jwt: malformed tokens refused, valid token verified");

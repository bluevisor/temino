// Just enough JWT for Sign in with Apple and Google: checking the ID
// tokens they hand back (RS256, against their published keys), and the
// ES256 client secret Apple asks for in place of a password.

const enc = new TextEncoder();
const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const fromB64url = (text) => Uint8Array.from(atob(text.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
const part = (text) => JSON.parse(new TextDecoder().decode(fromB64url(text)));

// The token's claims if it's signed by one of `jwks`'s keys, from one of
// `issuers`, for `audience`, unexpired and with our `nonce`; else null.
export async function verifyIdToken(token, { jwks, issuers, audience, nonce }) {
  if (typeof token !== "string") return null;
  const [head, body, signature] = token.split(".");
  if (!signature) return null;
  const header = part(head);
  if (header.alg !== "RS256") return null;
  const keys = await (await fetch(jwks, { cf: { cacheTtl: 3600 } })).json();
  const jwk = keys.keys.find((k) => k.kid === header.kid);
  if (!jwk) return null;
  const key = await crypto.subtle.importKey("jwk", jwk, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
  const ok = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, fromB64url(signature), enc.encode(`${head}.${body}`));
  if (!ok) return null;
  const claims = part(body);
  const now = Date.now() / 1000;
  if (!issuers.includes(claims.iss)) return null;
  if (claims.aud !== audience && !(Array.isArray(claims.aud) && claims.aud.includes(audience))) return null;
  if (!(claims.exp > now - 60)) return null;
  if (claims.nonce !== nonce) return null;
  return claims;
}

// Apple's client secret: a short-lived JWT signed with the Sign in with
// Apple key (.p8, PKCS#8 PEM) from the developer account.
export async function appleClientSecret(env) {
  const pem = env.APPLE_PRIVATE_KEY.replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const key = await crypto.subtle.importKey("pkcs8", fromB64url(pem.replace(/\+/g, "-").replace(/\//g, "_")),
    { name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
  const now = Math.floor(Date.now() / 1000);
  const head = b64url(enc.encode(JSON.stringify({ alg: "ES256", kid: env.APPLE_KEY_ID })));
  const body = b64url(enc.encode(JSON.stringify({
    iss: env.APPLE_TEAM_ID, iat: now, exp: now + 300, aud: "https://appleid.apple.com", sub: env.APPLE_CLIENT_ID,
  })));
  const signature = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, enc.encode(`${head}.${body}`)));
  return `${head}.${body}.${b64url(signature)}`;
}

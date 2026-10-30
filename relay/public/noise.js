// Noise_XX_25519_AESGCM_SHA256 on WebCrypto: the web client's end of the
// end-to-end encryption, matching src/e2e.rs on the desktop. The relay
// carries these frames and can't read them.
//
//   -> e
//   <- e, ee, s, es
//   -> s, se
//
// Then each message is sealed in pieces of at most 65518 bytes, each
// starting with a byte that says whether it's the last.

const NAME = "Noise_XX_25519_AESGCM_SHA256";
export const PROLOGUE = new TextEncoder().encode("temino-remote-1");
const PIECE = 65535 - 16 - 1;
const subtle = globalThis.crypto.subtle;

const concat = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) { out.set(p, at); at += p.length; }
  return out;
};
const sha256 = async (data) => new Uint8Array(await subtle.digest("SHA-256", data));
const hmac = async (key, data) => {
  const k = await subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await subtle.sign("HMAC", k, data));
};
// Noise's HKDF: two (or three) 32-byte outputs.
const hkdf = async (ck, ikm) => {
  const temp = await hmac(ck, ikm);
  const one = await hmac(temp, Uint8Array.of(1));
  const two = await hmac(temp, concat(one, Uint8Array.of(2)));
  return [one, two];
};
const nonce = (n) => {
  const iv = new Uint8Array(12);
  new DataView(iv.buffer).setBigUint64(4, BigInt(n), false);
  return iv;
};

// An X25519 key pair: generated, or the one stored for this device. The
// private key stays a non-extractable CryptoKey when stored that way.
export async function generateKeys(extractable = false) {
  const pair = await subtle.generateKey({ name: "X25519" }, extractable, ["deriveBits"]);
  const pub = new Uint8Array(await subtle.exportKey("raw", pair.publicKey));
  return { privateKey: pair.privateKey, public: pub };
}

async function dh(privateKey, publicBytes) {
  const pub = await subtle.importKey("raw", publicBytes, { name: "X25519" }, true, []);
  return new Uint8Array(await subtle.deriveBits({ name: "X25519", public: pub }, privateKey, 256));
}

class Cipher {
  constructor(key) { this.key = key; this.n = 0; }
  static async of(raw) {
    return new Cipher(await subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]));
  }
  async seal(ad, plain) {
    const out = await subtle.encrypt({ name: "AES-GCM", iv: nonce(this.n++), additionalData: ad }, this.key, plain);
    return new Uint8Array(out);
  }
  async open(ad, sealed) {
    const out = await subtle.decrypt({ name: "AES-GCM", iv: nonce(this.n++), additionalData: ad }, this.key, sealed);
    return new Uint8Array(out);
  }
}

class Symmetric {
  async init() {
    const name = new TextEncoder().encode(NAME);
    this.h = name.length <= 32 ? concat(name, new Uint8Array(32 - name.length)) : await sha256(name);
    this.ck = this.h;
    this.cipher = null;
    await this.mixHash(PROLOGUE);
    return this;
  }
  async mixHash(data) { this.h = await sha256(concat(this.h, data)); }
  async mixKey(ikm) {
    const [ck, k] = await hkdf(this.ck, ikm);
    this.ck = ck;
    this.cipher = await Cipher.of(k);
  }
  async encryptAndHash(plain) {
    const out = this.cipher ? await this.cipher.seal(this.h, plain) : plain;
    await this.mixHash(out);
    return out;
  }
  async decryptAndHash(data) {
    const out = this.cipher ? await this.cipher.open(this.h, data) : data;
    await this.mixHash(data);
    return out;
  }
  async split() {
    const [a, b] = await hkdf(this.ck, new Uint8Array(0));
    return [await Cipher.of(a), await Cipher.of(b)];
  }
}

// Six digits from the handshake hash, as src/e2e.rs makes them.
const code = (h) => String(new DataView(h.buffer, h.byteOffset).getUint32(0, false) % 1000000).padStart(6, "0");

// One end of a session. The web client initiates; `respond` exists for
// tests and is what the desktop does in Rust.
export class Session {
  constructor(keys, initiator) {
    this.s = keys;
    this.initiator = initiator;
    this.step = 0;
    this.partial = [];
  }

  static async initiate(keys) {
    const session = new Session(keys, true);
    session.sym = await new Symmetric().init();
    session.e = await generateKeys(false);
    await session.sym.mixHash(session.e.public);
    await session.sym.encryptAndHash(new Uint8Array(0));
    session.step = 1;
    return [session, session.e.public];
  }

  static async respond(keys) {
    const session = new Session(keys, false);
    session.sym = await new Symmetric().init();
    return session;
  }

  // Takes a handshake message, returns the reply (or null); `open` turns
  // true once it's done.
  async handshake(message) {
    const sym = this.sym;
    if (this.initiator && this.step === 1) {
      // <- e, ee, s, es
      this.re = message.slice(0, 32);
      await sym.mixHash(this.re);
      await sym.mixKey(await dh(this.e.privateKey, this.re));
      this.rs = await sym.decryptAndHash(message.slice(32, 32 + 48));
      await sym.mixKey(await dh(this.e.privateKey, this.rs));
      await sym.decryptAndHash(message.slice(80));
      // -> s, se
      const s = await sym.encryptAndHash(this.s.public);
      await sym.mixKey(await dh(this.s.privateKey, this.re));
      const payload = await sym.encryptAndHash(new Uint8Array(0));
      [this.send, this.recv] = await sym.split();
      this.finish();
      return concat(s, payload);
    }
    if (!this.initiator && this.step === 0) {
      // -> e
      this.re = message.slice(0, 32);
      await sym.mixHash(this.re);
      await sym.decryptAndHash(message.slice(32));
      // <- e, ee, s, es
      this.e = await generateKeys(false);
      await sym.mixHash(this.e.public);
      await sym.mixKey(await dh(this.e.privateKey, this.re));
      const s = await sym.encryptAndHash(this.s.public);
      await sym.mixKey(await dh(this.s.privateKey, this.re));
      const payload = await sym.encryptAndHash(new Uint8Array(0));
      this.step = 2;
      return concat(this.e.public, s, payload);
    }
    if (!this.initiator && this.step === 2) {
      // -> s, se
      this.rs = await sym.decryptAndHash(message.slice(0, 48));
      await sym.mixKey(await dh(this.e.privateKey, this.rs));
      await sym.decryptAndHash(message.slice(48));
      [this.recv, this.send] = await sym.split();
      this.finish();
      return null;
    }
    throw new Error("unexpected handshake message");
  }

  finish() {
    this.code = code(this.sym.h);
    this.peer = this.rs;
    this.open = true;
    this.sym = null;
    this.e = null;
  }

  // A message as frames to send.
  async seal(message) {
    const bytes = typeof message === "string" ? new TextEncoder().encode(message) : message;
    const frames = [];
    for (let at = 0; at < bytes.length || frames.length === 0; at += PIECE) {
      const piece = bytes.subarray(at, at + PIECE);
      const last = at + PIECE >= bytes.length ? 1 : 0;
      frames.push(await this.send.seal(new Uint8Array(0), concat(Uint8Array.of(last), piece)));
    }
    return frames;
  }

  // A frame in; the whole message once its last piece is.
  async openFrame(frame) {
    const plain = await this.recv.open(new Uint8Array(0), frame);
    this.partial.push(plain.subarray(1));
    if (plain[0] !== 1) return null;
    const whole = concat(...this.partial);
    this.partial = [];
    return whole;
  }
}

export const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
export const fromHex = (text) => Uint8Array.from(text.match(/../g) || [], (b) => parseInt(b, 16));
export const b64 = (bytes) => {
  let text = "";
  for (let at = 0; at < bytes.length; at += 0x8000) text += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  return btoa(text);
};
export const fromB64 = (text) => Uint8Array.from(atob(text), (c) => c.charCodeAt(0));

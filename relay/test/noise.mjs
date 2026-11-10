// The web client's Noise sessions (public/noise.js) against each other:
// a handshake, a message both ways, and the reassembly limit — pieces of
// a message past MAX_MESSAGE are refused (and dropped), so a peer that
// never sends the last piece can't grow the page's memory forever, and
// the session still works afterwards. Run: node test/noise.mjs
import assert from "node:assert/strict";
import { MAX_MESSAGE, Session, generateKeys } from "../public/noise.js";

const [phone, first] = await Session.initiate(await generateKeys(true));
const mac = await Session.respond(await generateKeys(true));
const second = await mac.handshake(first);
const third = await phone.handshake(second);
assert.equal(await mac.handshake(third), null);
assert.ok(phone.open && mac.open);
assert.equal(phone.code, mac.code);

// A message each way.
for (const frame of await phone.seal("hello")) {
  assert.deepEqual(new TextDecoder().decode(await mac.openFrame(frame)), "hello");
}

// One byte past the limit: refused, never delivered whole…
const big = new Uint8Array(MAX_MESSAGE + 1).map((_, i) => i % 251);
let refused = false;
for (const frame of await mac.seal(big)) {
  try {
    assert.equal(await phone.openFrame(frame), null);
  } catch {
    refused = true;
    break;
  }
}
assert.ok(refused, "the over-limit message was never refused");

// …and the session is usable for the next message.
for (const frame of await mac.seal("ls")) {
  assert.deepEqual(new TextDecoder().decode(await phone.openFrame(frame)), "ls");
}

console.log("noise: handshake, messages, and the reassembly limit ok");

// The web client's Noise end, talking hex lines over stdin/stdout: the
// desktop's interop test (cargo test -- --ignored interop) drives it.
import { createInterface } from "node:readline";
import { Session, generateKeys, hex, fromHex } from "../public/noise.js";

const lines = createInterface({ input: process.stdin })[Symbol.asyncIterator]();
const next = async () => fromHex((await lines.next()).value.trim());
const say = (bytes) => console.log(hex(bytes));

const keys = await generateKeys();
const [session, first] = await Session.initiate(keys);
say(first);
say(await session.handshake(await next()));
console.log(session.code);
console.log(hex(session.peer));
console.log(hex(keys.public));
// Echo back each message, reversed, until an empty one.
for (;;) {
  const frames = Number((await lines.next()).value);
  let message = null;
  for (let i = 0; i < frames; i++) message = await session.openFrame(await next());
  if (message.length === 0) break;
  const out = await session.seal(message.slice().reverse());
  console.log(out.length);
  for (const frame of out) say(frame);
}
process.exit(0);

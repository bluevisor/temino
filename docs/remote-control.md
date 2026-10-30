# Remote control

Drive a desktop Temino from an iPhone or iPad, from anywhere: every window,
tab and pane, with full control, end to end encrypted. Later, share machines
with a team.

## How it fits together

```
 iPhone / iPad ──wss──▶  relay (Cloudflare Worker + Durable Object per account)  ◀──wss── Temino (Mac)
        └──────────── Noise XX, end to end: the relay forwards sealed frames ───────────┘
```

- **Sign-in**: Apple or Google, through the relay (`relay/src/index.js`).
  Each device sends its public key along; the relay ties it to the
  account and hands back a device token. The token only ever goes back
  to the device's own loopback port, the app's `temino://` scheme, or
  the relay's own `/app`. Apple and Google sign-ins with the same verified
  email (not Apple's private relay address) land in one account.
- **Relay** (`relay/src/hub.js`): one Durable Object per account, placed
  in western North America. It knows the account's devices (name, kind,
  public key), who's online, and forwards `{to, kind, data}` frames. It
  can't read them and doesn't vouch for keys.
- **End to end** (`src/e2e.rs`, `relay/public/noise.js`):
  `Noise_XX_25519_AESGCM_SHA256` with prologue `temino-remote-1`, phone
  as initiator. Each device has a long-term X25519 key (the Mac's in
  `remote_key`, mode 0600; the web client's in IndexedDB, wrapped under a
  non-extractable AES key because Safari can't reload an X25519 CryptoKey,
  and unwrapped non-extractable on load). Every session gets fresh keys (forward secrecy).
- **Approval**: the handshake proves which key is at the other end; the
  Mac only lets a phone in once you approve it **on the Mac**, after
  checking both screens show the same six digits. They're drawn from the
  handshake hash and two random values fixed commit-then-reveal (the
  phone sends SHA-256 of its value and its long-term key in message 1,
  the Mac its value in message 2, the phone its value in message 3, where
  the Mac checks both against the hash), so a relay in the middle
  can't grind keys until both sessions' digits match: one guess in a
  million per attempt, and each attempt is a prompt you see.
  Approved keys live in `remote_peers`. The Mac also refuses a phone whose
  proven key isn't the one the relay lists for that device.
- **Inside the session**: the protocol below, unchanged. Messages bigger
  than a Noise frame (65 535 bytes) go in pieces, each led by a byte that
  says whether more follow.

- **Notifications**: when an agent starts waiting on you or finishes
  while no Temino window is in front, the Mac tells the relay only
  `{"type":"notify","kind":"waiting"|"done","tab":n}`, and the relay sends
  a Web Push ("Mac name: Tab 3 needs you") to each phone that subscribed
  and isn't showing the page. No screen content goes this way: a phone
  that isn't connected can't be reached end to end. On iPhone, Web Push
  needs the page added to the Home Screen; then Notify Me on the You view.

On the Mac: Settings ▸ Remote Control ▸ Sign In…  On the phone: open the
relay's `/app`, sign in with the same account, then Allow on the Mac.

Haiku has no relay yet (rustls's ring isn't known to build there); the
local host below still works.

## Local host (no relay)

With `remote=on`, Temino also serves the protocol on `remote_bind`
(default `127.0.0.1:7690`) behind `remote_token`, and the test page at
`/`. It's not end to end encrypted: keep it on localhost, or bind it to
the Mac's Tailscale address. Browser sockets from other origins are
refused.

## Running the relay

```
cd relay && npm install
npm run dev            # local, with .dev.vars: RELAY_SECRET=…, DEV_LOGIN=1
node test/flow.mjs desktop <home>   # sign a test desktop in (dev sign-in)
HOME=<home> temino                  # it connects to the local relay
node test/flow.mjs phone <home>     # a phone drives it end to end
cargo test -- --ignored interop     # Rust Noise against noise.js
```

Deploying needs `wrangler login`, a KV namespace for `ACCOUNTS`, and the
secrets `RELAY_SECRET`, `GOOGLE_CLIENT_SECRET`, `APPLE_PRIVATE_KEY`, with
`GOOGLE_CLIENT_ID`, `APPLE_CLIENT_ID` (Services ID), `APPLE_TEAM_ID`,
`APPLE_KEY_ID` in `wrangler.jsonc`; for notifications, `VAPID_PUBLIC` there
and the secret `VAPID_PRIVATE` (a P-256 key's JWK `d`). `node
test/webpush.mjs` checks the push encryption and signature. `DEV_LOGIN` must never be set there.

## Zoom levels

The phone looks at **one pane** by default. Zooming out (the ⤢ button, or
pinching in) steps through:

| Level   | Shows                                              | Screens sent |
|---------|----------------------------------------------------|--------------|
| pane    | the pane's screen, keys and an input line           | that pane    |
| tab     | every pane of the tab, small, live                  | the tab's panes |
| window  | the window's tabs: color, agent state, pane count   | none         |
| machine | this Temino's windows                               | none         |
| user    | every machine signed in as you (needs the relay)    | none         |

Pinching out or tapping goes back in. On iPad (768 pt and wider) the
pane view keeps the window's tabs, and the tab's panes, in a sidebar, and
the zoomed-out views use wider grids.

The outline (windows, tabs, panes, agent states) always goes out; only the
**watched** panes' text does, so zoomed out costs next to nothing.

## Protocol

JSON text frames over one WebSocket.

Client → host:

```json
{"type":"hello","token":"…"}
{"type":"watch","panes":[[1, 5]]}            // [window, pane] pairs; replaces the last watch
{"type":"tab","window":1,"index":2}          // switch the desktop to that tab
{"type":"focus","window":1,"pane":5}
{"type":"text","window":1,"pane":5,"text":"ls"}
{"type":"key","window":1,"pane":5,"key":"Enter"}
{"type":"key","window":1,"pane":5,"key":"c","mods":["ctrl"]}
{"type":"new_tab","window":1}                // in the usual shell
{"type":"new_tab","window":1,"shell":"zsh"}  // or another installed one
{"type":"close_tab","window":1,"index":2}
{"type":"new_window","window":1}
```

Keys: `Up Down Left Right Home End PageUp PageDown Delete Backspace Tab
Enter Escape`, or one character. Mods: `shift ctrl alt cmd`; `cmd` keys go
through the desktop's own shortcuts (⌘T, ⌘D, ⌘1…).

Text, keys and focus bring the pane's tab forward on the desktop and give
the pane the keyboard: it's control, not a second view.

Host → client:

```json
{"type":"hello","device":"Johns-M5-MacBook-Pro","version":"0.2.8"}
{"type":"state","windows":[{
  "no":1,"title":"~ — Temino","active":0,"theme":{"background":"#0f1117","text":"#e6e8ec","accent":"#67b0e8","…":"…"},"shells":["fish","bash","zsh"],
  "tabs":[{"index":0,"title":"silverride","color":"#67b0e8","agent":"running","focus":5,
    "panes":[{"id":5,"title":"silverride · claude","cwd":"/Users/…","agent":"running",
              "screen":{…}}]}]}]}
{"type":"error","error":"bad token"}
```

`agent` is `null`, `idle`, `running`, `waiting` or `done`. Watched panes
also carry `screen`, drawn the way the Mac draws them:

```json
{"mode":"blocks","columns":120,
 "blocks":[{"id":3,"kind":"command","command":"ls","cwd":"/Users/…","branch":"main",
            "exit":0,"running":false,"secs":0.1,"lines":[[["src","#67b0e8",null,1]]],"cursor":null}],
 "editor":{"text":"git st","runs":[["git","#7bd88f",null,0],[" st","#d8dee9",null,0]],"cursor":6,"cwd":"/Users/…","branch":"main"}}
{"mode":"grid","columns":120,"lines":[…],"cursor":[0,4]}
```

Each line comes as `{"k":key,"v":runs}` the first time and while it
changes, then as `{"k":key}` alone: the client keeps the lines the last
state used. The editor also carries the Mac's grey `suggestion`, its
completion `menu` (`items` of `[label, kind]`, `selected`), and `ai`.

A line's runs are `[text, fg, bg, flags]` (flags: 1 bold, 2 italic,
4 underline, 8 strikeout, 16 two cells wide), colors already resolved through the Mac's
theme. Each window also carries `theme`, the Mac's palette, so the client
looks like the app. Typing goes over a key or a run of text at a time
into the Mac's own editor, which completes and runs it. A full state goes out whenever
anything in it changes, at most every 150 ms per window.

## Next

1. **Deploy the relay** with the Apple and Google credentials; point
   `relay::DEFAULT_RELAY` at its address.
2. **iOS/iPadOS app** (SwiftUI): the same protocol and Noise (CryptoKit
   has X25519, AES-GCM, HKDF), key in the Keychain/Secure Enclave, real
   cell grid with colors and cursor, hardware keyboard on iPad, split view,
   notifications when an agent waits on you or finishes. Sign in with
   Apple there, as App Store review requires next to Google.
4. **Managing devices**: list and remove approved phones on the Mac, and
   devices on the account.
5. **Team sharing**: machines shared with teammates, view-only or control,
   with who's connected shown on the desktop.

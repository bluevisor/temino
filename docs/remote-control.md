# Remote control

Drive a desktop Temino from an iPhone or iPad: every window, tab and pane,
with full control. Later, share machines with a team.

## What works now (branch `remote-control`)

- **Host in Temino** (`src/remote.rs`). With `remote=on` in the settings
  file, Temino listens on `remote_bind` (default `127.0.0.1:7690`).
  A client must say hello with `remote_token` (made on first start, 32 hex
  characters, in the settings file) before it gets anything.
- **Every window**: each window's core thread registers with the host,
  builds its own state while someone watches, and carries out commands
  meant for it as if from its own keyboard. New windows join on their own.
- **Test client** (`src/remote.html`), served at `/` by the same port.
  Open `http://<host>:<port>/#<token>` in Safari. It stands in for the
  native app until that exists, and it's where the layout gets tried out.

Off by default. There's no TLS yet, so for a phone, bind to the Mac's
Tailscale address (`remote_bind=100.x.y.z:7690`) rather than the LAN.

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
{"type":"new_tab","window":1}
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
  "no":1,"title":"~ — Temino","active":0,
  "tabs":[{"index":0,"title":"silverride","color":"#67b0e8","agent":"running","focus":5,
    "panes":[{"id":5,"title":"silverride · claude","cwd":"/Users/…","agent":"running",
              "columns":120,"lines":["…"]}]}]}]}
{"type":"error","error":"bad token"}
```

`agent` is `null`, `idle`, `running`, `waiting` or `done`. `columns` and
`lines` come only for watched panes. A full state goes out whenever
anything in it changes, at most every 150 ms per window.

## Next

1. **Accounts and the relay** (decision pending). The phone has to reach
   the Mac from anywhere, signed in as the same user, so both connect *out*
   to a relay that pairs them by account. That lifts the localhost/Tailscale
   limit and gives the user level its list of machines.
2. **End-to-end encryption.** The relay passes frames it can't read: a key
   pair per device, with devices approved from one already signed in.
   Terminal output is full of secrets, so this isn't optional.
3. **iOS/iPadOS app** (SwiftUI). Renders the same state natively: real
   cell grid with colors and cursor, hardware-keyboard support on iPad,
   iPad split view and Stage Manager, notifications when an agent waits on
   you or finishes.
4. **Richer screens and less traffic**: per-cell colors and attributes,
   the cursor, and diffs instead of whole states.
5. **Team sharing**: machines shared with teammates, view-only or control,
   with who's connected shown on the desktop.

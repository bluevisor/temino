# Temino

<img src="docs/icon.png" width="64" align="right" alt="">

A modern GPU-accelerated terminal for [Haiku](https://www.haiku-os.org), drawn with Vulkan.

Temino works like [Warp](https://github.com/warpdotdev/warp): every command and
its output is a **block** you can select and copy, and commands are typed in a
real **editor** at the bottom of the window instead of the shell's line editor.
Full-screen programs (`less`, `vim`, `top`) get the whole window. On the
Haiku arm64 VM in UTM it renders on the Mac's GPU, through the Venus Vulkan
stack from [haiku-arm64-chromium](https://github.com/bluevisor/haiku-arm64-chromium/tree/main/gpu).

![The launch screen](docs/welcome.png)

![Blocks: a failed command, background output, a running loop](docs/blocks.png)

## What it does

- **Blocks.** Each command gets its own terminal grid, so output never mixes
  with prompts. A block's header shows where it ran (directory and git
  branch), how long it took and how it ended. Failed commands are marked red.
  Output that arrives while the prompt is idle (background jobs) gets its own
  block.
- **An editor for commands.** Syntax colors (known commands green, unknown
  red, flags, strings, variables), multi-line input (Shift+Enter), undo,
  word motions, selection, and fish-style **autosuggestions** from history.
- **Completion and history.** Tab completes commands and paths in a menu;
  Ctrl+R searches history; Up/Down walks it.
- **A real terminal underneath.** The grid is
  [alacritty_terminal](https://crates.io/crates/alacritty_terminal): 256 and
  true colors, the alternate screen, mouse reporting, bracketed paste, OSC 52
  copy (not paste), and color and size queries.
- **GPU text.** Glyphs come from a 2048² atlas (fontdue) and are drawn as
  instanced quads; rounded rectangles, borders and shadows are a signed
  distance field shader. Box-drawing and block characters are drawn
  procedurally so they join across cells.

## Keys

| Key | |
|---|---|
| Enter / Shift+Enter | run / new line |
| Tab, Ctrl+R, ↑ ↓ | complete, search history, browse history |
| → or End at the end of the line | accept the suggestion (Ctrl+→ one word) |
| Alt+C, Alt+Shift+C, Alt+V | copy (selection, else the selected block's output), copy the block's command, paste |
| Alt+↑ / Alt+↓ | select the previous / next block |
| Alt+K, Ctrl+L | clear the blocks |
| Alt+= / Alt+- / Alt+0 | font size |
| Ctrl+C | interrupt the running program (at the prompt: clear the line) |
| Shift+PageUp / PageDown | scroll while a program runs |

Alt is Haiku's Command key. Click a block to select it; drag over output to
select text (double-click: word, triple: line).

## How it works

```
 BWindow / BView  ──events──▶  core thread  ◀──bytes──  pty reader ◀── bash
 (src/haiku/shim.cpp)          (src/app.rs)                 │
        ▲                          │                        │ OSC 133 / 7 markers
        │ BBitmap copy             ▼                        ▼
        └──────────────  Vulkan offscreen frame  ◀──  blocks (one alacritty Term each)
                         (src/gpu.rs, Venus)            (src/session.rs)
```

- **Shell integration.** bash starts with `--rcfile` pointing at
  [`src/shell/temino.bash`](src/shell/temino.bash). It sources the usual login
  files, then reduces the prompt to invisible OSC 133 markers (prompt start,
  prompt end, output start, done with exit status), OSC 7 for the working
  directory and OSC 6973 for PS2 continuations. Temino pulls these out of the
  byte stream and routes the bytes in between: output goes to the running
  block's grid, and the shell's echo of the command is dropped.
- **Sending commands.** Haiku's tty line discipline caps a canonical line at
  255 bytes (`MAX_CANON`), so commands go through readline in raw mode, bound
  so that Tab characters stay text.
- **Rendering.** Haiku has no Vulkan window-system integration, so each frame
  renders into an offscreen `B8G8R8A8` image, is copied into a host-visible
  buffer and handed to the window thread, which copies it into one of two
  `BBitmap`s and draws it. Frames are drawn only when something changed (or a
  spinner or the caret blinks); a 980×640 frame takes about 1 ms on an M5 Pro
  through Venus.
- **No fork().** The shell is started with `load_image()`, through a
  `temino --pty-exec` helper that makes the session and controlling tty and
  then execs bash. On this Haiku build, fork() makes the parent's Venus memory
  mappings copy-on-write, which stalls the Venus command ring (Venus aborts)
  and hung the VM when the process exited. See the
  [driver note](https://github.com/bluevisor/haiku-arm64-chromium/blob/main/gpu/README.md).

## Building

On Haiku (tested on arm64, hrev60290) with Rust, a C++ compiler and a Vulkan driver
(`libvulkan.so.1` on the library path):

```sh
cargo build --release
target/release/temino --selftest /tmp/frame.ppm   # one offscreen frame, no window
target/release/temino
haiku/package.sh                                 # dist/temino-<version>-1-<arch>.hpkg
cp dist/temino-*.hpkg ~/config/packages/          # adds Temino to Deskbar
```

From a Mac, `haiku/deploy.sh` copies the tree to the VM (`HOST`, default
`haiku-dbg`), builds it there and starts it. `haiku/keysend.cpp` posts key
presses to a window for scripted tests. After editing a shader, run
`shaders/build.sh` (needs `glslc`).

Set `TEMINO_TRACE=1` to log startup and frame stages to
`~/config/settings/Temino/trace.log`. History, the shell integration script
and crash logs live in the same directory. `TEMINO_SHELL` picks another bash.

## Credits

- The rounded-rectangle and shadow shader math follows `rect_shader.wgsl`
  from Warp's UI framework (`warpui`, MIT).
- [alacritty_terminal](https://github.com/alacritty/alacritty) (Apache-2.0),
  [fontdue](https://github.com/mooman219/fontdue), [ash](https://github.com/ash-rs/ash).
- [JetBrains Mono](https://www.jetbrains.com/lp/mono/) (SIL OFL 1.1), bundled;
  UI text uses Haiku's Noto Sans.

Temino is MIT licensed.

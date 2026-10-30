//! Linux and macOS: a winit window, the clipboard through arboard, and a
//! Unix pseudo-terminal. On Linux each finished frame is shown through
//! softbuffer; on macOS frames render straight into the window's Metal
//! layer (src/macos/shim.m, which also has the menu bar).
//!
//! winit wants the main thread, so the core thread talks to it through an
//! event-loop proxy: `present` parks the newest frame and wakes the loop,
//! which copies it into the window. Events go the other way as `UiEvent`s.
//!
//! Each window has a core thread of its own (an `App`), numbered from 1:
//! what a core thread asks for goes to its window, by `window_no`.

use std::cell::{Cell, RefCell};
use std::io;
use std::num::NonZeroU32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, KeyEvent as WinitKey, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

use super::{buttons, mods, Cursor, KeyEvent, UiEvent, ViewerKind, ViewerStyle};
use crate::keys::Named;
use crate::Msg;

#[cfg(target_os = "macos")]
extern "C" {
    /// Gives window `window`'s view the Metal layer that src/metal.rs
    /// renders into; detach lets it go with the window.
    fn temino_metal_attach(view: *mut std::ffi::c_void, window: u32);
    fn temino_metal_detach(window: u32);
    /// Makes the title bar `height` points tall, traffic lights centered.
    fn temino_titlebar(view: *mut std::ffi::c_void, height: f64);
    /// Blurs what shows through the background, within the rounded corners.
    fn temino_blur(view: *mut std::ffi::c_void, on: bool);
    fn temino_hotkey(keycode: u32, modifiers: u32);
    fn temino_toggle_window();
    fn temino_emoji(
        text: *const std::ffi::c_char,
        size: f32,
        max_width: f32,
        max_height: f32,
        out: *mut u8,
        capacity: usize,
        width: *mut u32,
        height: *mut u32,
    ) -> bool;
    /// Puts a host for window `window`'s viewers over the view; their events
    /// go to `callback`. Viewer ids carry their window in the top 32 bits.
    fn temino_viewer_init(
        view: *mut std::ffi::c_void,
        window: u32,
        callback: extern "C" fn(u64, i32, *const std::ffi::c_char),
    );
    fn temino_viewer_detach(window: u32);
    fn temino_viewer_open(id: u64, kind: i32, target: *const std::ffi::c_char, style: *const ViewerStyle);
    fn temino_viewer_place(id: u64, x: f64, y: f64, width: f64, height: f64, visible: bool);
    fn temino_viewer_close(id: u64);
    /// Gives the keyboard to viewer `id`, or with 0 back to the terminal.
    fn temino_viewer_focus(id: u64);
}

/// `text` in Apple Color Emoji at `size` pixels, shrunk to fit: its width,
/// height and premultiplied RGBA rows, or `None` if it isn't an emoji.
#[cfg(target_os = "macos")]
pub fn emoji(text: &str, size: f32, max_width: f32, max_height: f32) -> Option<(usize, usize, Vec<u8>)> {
    let text = std::ffi::CString::new(text).ok()?;
    let mut out = vec![0u8; 512 * 512 * 4];
    let (mut width, mut height) = (0u32, 0u32);
    let ok = unsafe {
        temino_emoji(text.as_ptr(), size, max_width, max_height, out.as_mut_ptr(), out.len(), &mut width, &mut height)
    };
    if !ok {
        return None;
    }
    out.truncate(width as usize * height as usize * 4);
    Some((width as usize, height as usize, out))
}

/// What the core thread asks of the window thread.
#[derive(Debug)]
enum Request {
    Present,
    /// The quick terminal's hotkey, as a macOS key code and Carbon modifiers.
    #[allow(dead_code)]
    Hotkey(Option<(u32, u32)>),
    #[allow(dead_code)]
    ToggleWindow,
    Title(String),
    Cursor(Cursor),
    Blur(bool),
    /// Where the text caret is, in logical pixels: x, y, height.
    Caret(i32, i32, i32),
    ViewerOpen(u64, ViewerKind, String, ViewerStyle),
    ViewerPlace(Vec<(u64, [f32; 4], bool)>),
    ViewerClose(u64),
    ViewerFocus(Option<u64>),
    /// A new window, its first tab in this directory.
    NewWindow(Option<String>),
    /// This window's core is done: close it (and quit with the last one).
    CloseWindow,
    /// Close every window: this one's core is done, the others are asked.
    Quit,
}

struct Frame {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

static PROXY: OnceLock<Mutex<EventLoopProxy<(u32, Request)>>> = OnceLock::new();
/// Each window's newest frame, and whether a Present for it is queued (so
/// frames don't pile up).
static FRAMES: Mutex<Vec<(u32, Frame, bool)>> = Mutex::new(Vec::new());
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
/// Whether the window blurs what's behind it, kept for when it opens.
static BLUR: AtomicBool = AtomicBool::new(false);
/// Where menu picks go, the core of the window in front; the menu calls
/// back on the main thread.
static MENU_SENDER: Mutex<Option<Sender<Msg>>> = Mutex::new(None);
/// Each window's core, by window number, for its viewers' events.
static SENDERS: Mutex<Vec<(u32, Sender<Msg>)>> = Mutex::new(Vec::new());
/// How many windows are open.
static WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Starts a new window's core thread: given its events and the sender for
/// them, and the directory its first tab opens in.
type CoreStart = fn(Receiver<Msg>, Sender<Msg>, Option<String>);
/// `on_new_window` sets it.
static NEW_CORE: OnceLock<CoreStart> = OnceLock::new();

/// The window `window_new` asked for: title, size, where events go.
type Setup = (String, u32, u32, Sender<Msg>);

thread_local! {
    static EVENT_LOOP: RefCell<Option<EventLoop<(u32, Request)>>> = const { RefCell::new(None) };
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
    /// Which window this thread is the core of: the first, unless it was
    /// started for another.
    static WINDOW: Cell<u32> = const { Cell::new(1) };
}

fn request(request: Request) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.lock().unwrap().send_event((window_no(), request));
    }
}

/// The window the calling core thread draws (1 for the first).
pub fn window_no() -> u32 {
    WINDOW.with(Cell::get)
}

/// How many windows are open.
pub fn window_count() -> usize {
    WINDOWS.load(Ordering::Acquire)
}

/// How a new window's core thread starts.
pub fn on_new_window(start: CoreStart) {
    let _ = NEW_CORE.set(start);
}

/// Opens another window, its first tab in `cwd`.
pub fn new_window(cwd: Option<String>) {
    request(Request::NewWindow(cwd));
}

/// Closes the calling core's window, once it's done; the last one quits.
pub fn close_window() {
    request(Request::CloseWindow);
}

pub fn app_new(_signature: &str) {
    let mut builder = EventLoop::<(u32, Request)>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        // Temino installs its own menu bar.
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_default_menu(false);
    }
    let event_loop = builder.build().expect("creating the event loop");
    let _ = PROXY.set(Mutex::new(event_loop.create_proxy()));
    EVENT_LOOP.with(|slot| *slot.borrow_mut() = Some(event_loop));
}

/// The window's last position and size (logical x, y, width, height).
fn saved_frame() -> Option<(f64, f64, f64, f64)> {
    let frame = crate::setting("window")?;
    let parts: Vec<f64> = frame.split(',').filter_map(|part| part.parse().ok()).collect();
    match parts[..] {
        [x, y, width, height] if width >= 200.0 && height >= 120.0 => Some((x, y, width, height)),
        _ => None,
    }
}

pub fn window_new(title: &str, width: u32, height: u32, sender: Sender<Msg>) {
    SETUP.with(|slot| *slot.borrow_mut() = Some((title.to_owned(), width, height, sender)));
}

/// Runs the event loop on the calling (main) thread until quit.
pub fn app_run() {
    let event_loop = EVENT_LOOP.with(|slot| slot.borrow_mut().take()).expect("app_new first");
    let (title, width, height, sender) = SETUP.with(|slot| slot.borrow_mut().take()).expect("window_new first");
    *MENU_SENDER.lock().unwrap() = Some(sender.clone());
    let mut handler =
        Handler { title, size: (width, height), first: Some(sender), windows: Vec::new(), next: 1, primary: 1 };
    let _ = event_loop.run_app(&mut handler);
}

pub fn quit() {
    request(Request::Quit);
}

/// The window is always covered by the frame; nothing to do.
pub fn set_background(_rgb: [u8; 3]) {}

// Settings are picked in the Settings tab here, not from menus.
pub fn set_background_menu(_names: &[&str], _selected: usize) {}
pub fn set_shell_menu(_names: &[String], _selected: usize) {}

/// Whether a hotkey from anywhere can bring the window (macOS).
pub const QUICK_TERMINAL: bool = cfg!(target_os = "macos");

/// The quick terminal's hotkey, from anywhere on the desktop: it brings
/// Temino's window to the front, and hides it again. False if the key
/// can't be registered.
pub fn set_quick_terminal(combo: Option<&crate::bindings::Combo>) -> bool {
    let Some(combo) = combo else {
        request(Request::Hotkey(None));
        return true;
    };
    let Some(code) = mac_key_code(combo) else { return false };
    // Carbon's modifier bits: cmd, shift, option, control.
    let mut modifiers = 0;
    for (bit, carbon) in [(mods::COMMAND, 0x100), (mods::SHIFT, 0x200), (mods::OPTION, 0x800), (mods::CONTROL, 0x1000)]
    {
        if combo.mods & bit != 0 {
            modifiers |= carbon;
        }
    }
    request(Request::Hotkey(Some((code, modifiers))));
    QUICK_TERMINAL
}

pub fn toggle_quick_terminal() {
    request(Request::ToggleWindow);
}

/// The macOS virtual key code (ANSI layout) of a key.
fn mac_key_code(combo: &crate::bindings::Combo) -> Option<u32> {
    if let Some(named) = combo.named {
        return Some(match named {
            Named::Enter => 0x24,
            Named::Tab => 0x30,
            Named::Escape => 0x35,
            Named::Backspace => 0x33,
            Named::Delete => 0x75,
            Named::Home => 0x73,
            Named::End => 0x77,
            Named::PageUp => 0x74,
            Named::PageDown => 0x79,
            Named::Left => 0x7b,
            Named::Right => 0x7c,
            Named::Down => 0x7d,
            Named::Up => 0x7e,
            Named::F(n) => {
                [0x7a, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6d, 0x67, 0x6f].get(n as usize - 1).copied()?
            }
            Named::Insert => return None,
        });
    }
    const KEYS: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0\0`";
    if combo.c == ' ' {
        return Some(0x31);
    }
    KEYS.chars().position(|c| c == combo.c && c != '\0').map(|i| i as u32)
}

/// Blurs what shows through a see-through background (macOS).
pub fn set_blur(on: bool) {
    BLUR.store(on, Ordering::Release);
    request(Request::Blur(on));
}

#[cfg(target_os = "macos")]
mod menu {
    use std::ffi::{c_char, CString};

    use super::{KeyEvent, UiEvent, MENU_SENDER};
    use crate::keys::Named;
    use crate::platform::mods;
    use crate::Msg;

    #[repr(C)]
    struct Item {
        menu: *const c_char,
        label: *const c_char,
        key: *const c_char,
        modifiers: u32,
        tag: i32,
    }

    extern "C" {
        fn temino_menu_install(items: *const Item, count: usize, callback: extern "C" fn(i32));
    }

    const SHIFT: u32 = 1 << 17;
    const COMMAND: u32 = 1 << 20;

    /// Menu, label, key equivalent, extra modifiers, and the key the item
    /// sends, as if typed with Command: menus and shortcuts share one path.
    /// A None label is a separator.
    type MenuItem = (&'static str, Option<&'static str>, &'static str, u32, Option<Named>, char);
    const ITEMS: &[MenuItem] = &[
        ("Temino", Some("Check for Updates…"), "u", 0, None, 'u'),
        ("Temino", Some("Settings…"), ",", 0, None, ','),
        ("Temino", None, "", 0, None, '\0'),
        ("Temino", Some("Quit Temino"), "q", 0, None, 'q'),
        ("Shell", Some("New Window"), "n", 0, None, 'n'),
        ("Shell", Some("New Tab"), "t", 0, None, 't'),
        ("Shell", Some("Split Right"), "d", 0, None, 'd'),
        ("Shell", Some("Split Down"), "e", 0, None, 'e'),
        ("Shell", Some("Split Up"), "e", SHIFT, None, 'e'),
        ("Shell", None, "", 0, None, '\0'),
        ("Shell", Some("Close Pane"), "w", 0, None, 'w'),
        ("Shell", None, "", 0, None, '\0'),
        ("Shell", Some("Mark Tab Important"), "I", SHIFT, None, 'i'),
        ("Shell", Some("Mark Tab Waiting"), "l", 0, None, 'l'),
        ("Shell", Some("Mark Tab for Later"), "L", SHIFT, None, 'l'),
        ("Edit", Some("Copy"), "c", 0, None, 'c'),
        ("Edit", Some("Copy Command"), "c", SHIFT, None, 'c'),
        ("Edit", Some("Paste"), "v", 0, None, 'v'),
        ("Edit", Some("Select All"), "a", 0, None, 'a'),
        ("Edit", None, "", 0, None, '\0'),
        ("Edit", Some("Find…"), "f", 0, None, 'f'),
        ("Edit", Some("Find Next"), "g", 0, None, 'g'),
        ("Edit", Some("Find Previous"), "g", SHIFT, None, 'g'),
        ("Edit", None, "", 0, None, '\0'),
        ("Edit", Some("Clear"), "k", 0, None, 'k'),
        ("View", Some("Show/Hide Tabs"), "b", 0, None, 'b'),
        ("View", Some("Previous Tab"), "{", SHIFT, None, '['),
        ("View", Some("Next Tab"), "}", SHIFT, None, ']'),
        ("View", Some("Previous Pane"), "[", 0, None, '['),
        ("View", Some("Next Pane"), "]", 0, None, ']'),
        ("View", Some("Jump to What Needs You"), "`", 0, None, '`'),
        ("View", Some("Jump to Newest Need"), "~", SHIFT, None, '`'),
        ("View", Some("Zoom Pane"), "\r", 0, Some(Named::Enter), '\r'),
        ("View", None, "", 0, None, '\0'),
        ("View", Some("Bigger Text"), "+", 0, None, '='),
        ("View", Some("Smaller Text"), "-", 0, None, '-'),
        ("View", Some("Default Text Size"), "0", 0, None, '0'),
    ];

    extern "C" fn pick(tag: i32) {
        let Some(&(_, _, _, extra, named, c)) = ITEMS.get(tag as usize) else { return };
        let shift = if extra & SHIFT != 0 { mods::SHIFT } else { 0 };
        let bytes = if named.is_some() { Vec::new() } else { c.to_string().into_bytes() };
        let event = UiEvent::Key(KeyEvent { named, mods: mods::COMMAND | shift, raw_char: c as u32, bytes });
        if let Some(sender) = MENU_SENDER.lock().unwrap().as_ref() {
            let _ = sender.send(Msg::Ui(event));
        }
    }

    pub fn install() {
        let strings: Vec<[CString; 3]> = ITEMS
            .iter()
            .map(|(menu, label, key, ..)| [*menu, label.unwrap_or(""), *key].map(|s| CString::new(s).unwrap()))
            .collect();
        let items: Vec<Item> = strings
            .iter()
            .enumerate()
            .map(|(i, [menu, label, key])| {
                let separator = ITEMS[i].1.is_none();
                Item {
                    menu: menu.as_ptr(),
                    label: if separator { std::ptr::null() } else { label.as_ptr() },
                    key: key.as_ptr(),
                    modifiers: COMMAND | ITEMS[i].3,
                    tag: i as i32,
                }
            })
            .collect();
        unsafe { temino_menu_install(items.as_ptr(), items.len(), pick) };
    }
}

/// Takes a BGRA frame (physical pixels) and has the window show it.
pub fn present(pixels: &[u8], width: u32, height: u32) {
    debug_assert!(pixels.len() >= (width * height * 4) as usize);
    let no = window_no();
    let queued = {
        let mut frames = FRAMES.lock().unwrap();
        let index = frames.iter().position(|(n, ..)| *n == no).unwrap_or_else(|| {
            frames.push((no, Frame { pixels: Vec::new(), width: 0, height: 0 }, false));
            frames.len() - 1
        });
        let (_, frame, pending) = &mut frames[index];
        frame.pixels.clear();
        // Little-endian BGRA is 0xAARRGGBB; softbuffer wants the top byte clear.
        frame.pixels.extend(
            pixels[..(width * height * 4) as usize].chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0])),
        );
        frame.width = width;
        frame.height = height;
        std::mem::replace(pending, true)
    };
    if !queued {
        request(Request::Present);
    }
}

/// The title bar Temino draws over the top of the window, in points: as
/// tall as Chrome's tab strip. Elsewhere the window manager draws one.
pub const TITLEBAR: f32 = if cfg!(target_os = "macos") { 40.0 } else { 0.0 };

pub fn set_title(title: &str) {
    request(Request::Title(title.replace('\0', "")));
}

pub fn set_cursor(cursor: Cursor) {
    request(Request::Cursor(cursor));
}

/// Where an input method's candidate window opens: beside the caret.
pub fn set_text_caret(x: i32, y: i32, height: i32) {
    request(Request::Caret(x, y, height));
}

pub fn beep() {}

/// A viewer's id as the shim knows it: its pane id, which is only unique
/// within a window, with the window's number above it.
fn viewer_id(id: u64) -> u64 {
    (window_no() as u64) << 32 | (id & 0xffff_ffff)
}

/// Opens a link in the default browser (or whatever handles its scheme).
pub fn viewer_open(id: u64, kind: ViewerKind, target: &str, style: ViewerStyle) {
    request(Request::ViewerOpen(viewer_id(id), kind, target.to_owned(), style));
}

/// Where each viewer goes, in points from the window's top left, and
/// whether it shows.
pub fn viewer_place(frames: Vec<(u64, [f32; 4], bool)>) {
    request(Request::ViewerPlace(frames.into_iter().map(|(id, frame, shown)| (viewer_id(id), frame, shown)).collect()));
}

pub fn viewer_close(id: u64) {
    request(Request::ViewerClose(viewer_id(id)));
}

/// Gives the keyboard to viewer `id`, or with None back to the window's terminal.
pub fn viewer_focus(id: Option<u64>) {
    request(Request::ViewerFocus(Some(viewer_id(id.unwrap_or(0)))));
}

/// A viewer's news, on the main thread, to the core thread.
#[cfg(target_os = "macos")]
extern "C" fn viewer_event(id: u64, kind: i32, text: *const std::ffi::c_char) {
    use super::ViewerEvent;
    let text = if text.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy().into_owned()
    };
    let event = match kind {
        0 => ViewerEvent::Focused,
        1 => ViewerEvent::Title(text),
        2 => ViewerEvent::Url(text),
        3 => ViewerEvent::Dirty(true),
        4 => ViewerEvent::Dirty(false),
        6 => ViewerEvent::Close,
        _ => ViewerEvent::Failed(text),
    };
    // To the core of the viewer's window, by the pane id it knows.
    let window = (id >> 32) as u32;
    if let Some((_, sender)) = SENDERS.lock().unwrap().iter().find(|(no, _)| *no == window) {
        let _ = sender.send(Msg::Viewer(id & 0xffff_ffff, event));
    }
}

pub fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

fn with_clipboard<T>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<T>) -> Option<T> {
    let mut slot = CLIPBOARD.lock().unwrap();
    if slot.is_none() {
        // Kept for the life of the process: on X11 the owner serves pastes.
        *slot = arboard::Clipboard::new().ok();
    }
    f(slot.as_mut()?)
}

pub fn clipboard_set(text: &str) {
    with_clipboard(|clipboard| clipboard.set_text(text).ok());
}

pub fn clipboard_get() -> Option<String> {
    with_clipboard(|clipboard| clipboard.get_text().ok())
}

/// The X11/Wayland primary selection (select to copy, middle-click to
/// paste). Other systems have none.
pub fn selection_set(text: &str) {
    #[cfg(target_os = "linux")]
    with_clipboard(|clipboard| {
        use arboard::{LinuxClipboardKind, SetExtLinux};
        clipboard.set().clipboard(LinuxClipboardKind::Primary).text(text.to_owned()).ok()
    });
    #[cfg(not(target_os = "linux"))]
    let _ = text;
}

pub fn selection_get() -> Option<String> {
    #[cfg(target_os = "linux")]
    return with_clipboard(|clipboard| {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        clipboard.get().clipboard(LinuxClipboardKind::Primary).text().ok()
    });
    #[cfg(not(target_os = "linux"))]
    None
}

pub fn alert(text: &str) {
    eprintln!("{text}");
}

pub fn user_name() -> Option<String> {
    None
}

/// Starts `argv` on a new pseudo-terminal; returns (pid, master fd).
pub fn spawn(argv: &[String], env: &[String], cwd: &str, columns: u16, rows: u16) -> io::Result<(i32, i32)> {
    let (program, args) = argv.split_first().ok_or(io::ErrorKind::InvalidInput)?;
    let size = libc::winsize { ws_row: rows, ws_col: columns, ws_xpixel: 0, ws_ypixel: 0 };
    let (mut master, mut slave) = (-1, -1);
    let result = unsafe {
        libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &size as *const _ as *mut _)
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // Neither end may leak into the shells of other panes.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let mut command = Command::new(program);
    command.args(args).env_clear().current_dir(cwd);
    for pair in env {
        if let Some((key, value)) = pair.split_once('=') {
            command.env(key, value);
        }
    }
    command.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().inspect_err(|_| unsafe {
        libc::close(master);
    })?;
    // `wait` reaps it by pid; dropping the Child neither waits nor kills.
    Ok((child.id() as i32, master))
}

pub fn pty_resize(master: i32, columns: u16, rows: u16, pixel_width: u32, pixel_height: u32) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: pixel_width.min(u16::MAX as u32) as u16,
        ws_ypixel: pixel_height.min(u16::MAX as u32) as u16,
    };
    unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &size) };
}

/// Blocks until `pid` exits and returns its exit code.
pub fn wait(pid: i32) -> i32 {
    let mut status = 0;
    loop {
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            break;
        }
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return -1;
        }
    }
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        -1
    }
}

pub fn hangup(pid: i32) {
    unsafe { libc::kill(pid, libc::SIGHUP) };
}

/// The process group in front on the terminal `master`: the running job.
pub fn foreground_group(master: i32) -> i32 {
    unsafe { libc::tcgetpgrp(master) }
}

pub fn process_group(pid: i32) -> i32 {
    unsafe { libc::getpgid(pid) }
}

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// One open window: its winit window, how frames reach it, its core, and
/// the pointer and keyboard state its events are made from.
struct Win {
    no: u32,
    window: Arc<Window>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// Frames go straight to the window's Metal layer, not through softbuffer.
    #[cfg(target_os = "macos")]
    on_layer: bool,
    sender: Sender<Msg>,
    scale: f64,
    modifiers: ModifiersState,
    /// The pointer, in logical pixels.
    pointer: (f32, f32),
    buttons: i32,
    last_click: Option<(Instant, (f32, f32), i32)>,
    clicks: i32,
    /// An input method is composing: its keys aren't ours.
    composing: bool,
}

struct Handler {
    title: String,
    size: (u32, u32),
    /// The first window's core, which main started, until its window opens.
    first: Option<Sender<Msg>>,
    windows: Vec<Win>,
    /// The next window's number.
    next: u32,
    /// The window that keeps the tabs for the next launch: the first, then
    /// the oldest left once it closes.
    primary: u32,
}

impl Win {
    fn send(&self, event: UiEvent) {
        let _ = self.sender.send(Msg::Ui(event));
    }

    /// Remembers where the window is and how big, for the next launch.
    fn save_frame(&self) {
        let window = &self.window;
        if window.is_minimized() == Some(true) || window.fullscreen().is_some() {
            return;
        }
        // winit on macOS places a new window by its content, not its frame.
        let position = if cfg!(target_os = "macos") { window.inner_position() } else { window.outer_position() };
        let Ok(position) = position else { return };
        let position = position.to_logical::<f64>(self.scale);
        let size = window.inner_size().to_logical::<f64>(self.scale);
        let frame = format!("{:.0},{:.0},{:.0},{:.0}", position.x, position.y, size.width, size.height);
        if crate::setting("window").as_deref() != Some(frame.as_str()) {
            crate::save_setting("window", &frame);
        }
    }

    fn send_size(&self) {
        let size = self.window.inner_size();
        let scale = self.scale;
        self.send(UiEvent::Resize {
            width: ((size.width as f64 / scale).round() as u32).max(1),
            height: ((size.height as f64 / scale).round() as u32).max(1),
            scale: scale as f32,
        });
    }

    fn mods(&self) -> u32 {
        let m = self.modifiers;
        let mut bits = 0;
        if m.shift_key() {
            bits |= mods::SHIFT;
        }
        if m.control_key() {
            bits |= mods::CONTROL;
        }
        if m.alt_key() {
            bits |= mods::OPTION;
        }
        if m.super_key() {
            bits |= mods::COMMAND;
        }
        bits
    }

    fn draw(&mut self) {
        let mut frames = FRAMES.lock().unwrap();
        let Some((_, frame, pending)) = frames.iter_mut().find(|(no, ..)| *no == self.no) else { return };
        *pending = false;
        #[cfg(target_os = "macos")]
        if self.on_layer {
            return;
        }
        let Some(surface) = &mut self.surface else { return };
        let (Some(width), Some(height)) = (NonZeroU32::new(frame.width), NonZeroU32::new(frame.height)) else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        if let Ok(mut buffer) = surface.buffer_mut() {
            if buffer.len() == frame.pixels.len() {
                buffer.copy_from_slice(&frame.pixels);
                let _ = buffer.present();
            }
        }
    }

    fn key(&self, event: &WinitKey) {
        let named = match &event.logical_key {
            Key::Named(key) => match key {
                NamedKey::ArrowUp => Some(Named::Up),
                NamedKey::ArrowDown => Some(Named::Down),
                NamedKey::ArrowLeft => Some(Named::Left),
                NamedKey::ArrowRight => Some(Named::Right),
                NamedKey::Home => Some(Named::Home),
                NamedKey::End => Some(Named::End),
                NamedKey::PageUp => Some(Named::PageUp),
                NamedKey::PageDown => Some(Named::PageDown),
                NamedKey::Insert => Some(Named::Insert),
                NamedKey::Delete => Some(Named::Delete),
                NamedKey::Backspace => Some(Named::Backspace),
                NamedKey::Tab => Some(Named::Tab),
                NamedKey::Enter => Some(Named::Enter),
                NamedKey::Escape => Some(Named::Escape),
                NamedKey::F1 => Some(Named::F(1)),
                NamedKey::F2 => Some(Named::F(2)),
                NamedKey::F3 => Some(Named::F(3)),
                NamedKey::F4 => Some(Named::F(4)),
                NamedKey::F5 => Some(Named::F(5)),
                NamedKey::F6 => Some(Named::F(6)),
                NamedKey::F7 => Some(Named::F(7)),
                NamedKey::F8 => Some(Named::F(8)),
                NamedKey::F9 => Some(Named::F(9)),
                NamedKey::F10 => Some(Named::F(10)),
                NamedKey::F11 => Some(Named::F(11)),
                NamedKey::F12 => Some(Named::F(12)),
                NamedKey::Space => None,
                // Modifiers and the like produce nothing.
                _ => return,
            },
            Key::Character(_) => None,
            _ => return,
        };
        let base = match event.key_without_modifiers() {
            Key::Character(text) => text.chars().next(),
            Key::Named(NamedKey::Space) => Some(' '),
            _ => None,
        };
        let mut bits = self.mods();
        let mut bytes = event.text_with_all_modifiers().map(|t| t.as_bytes().to_vec()).unwrap_or_default();
        if bytes.is_empty() {
            if let Some(c) = base.filter(|_| bits & mods::CONTROL != 0) {
                bytes = c.to_string().into_bytes();
            }
        }
        if cfg!(target_os = "macos") && bits & mods::OPTION != 0 {
            // Option is Meta: the key itself, not the character macOS composes.
            if let Some(c) = base.filter(|c| c.is_ascii_graphic()) {
                let c = if bits & mods::SHIFT != 0 { c.to_ascii_uppercase() } else { c };
                bytes = c.to_string().into_bytes();
            }
        }
        if cfg!(target_os = "linux")
            && bits & mods::CONTROL != 0
            && bits & mods::SHIFT != 0
            && named != Some(Named::Tab)
        {
            // Ctrl+Shift is the shortcut modifier, as in other Linux terminals.
            bits = (bits & !(mods::CONTROL | mods::SHIFT)) | mods::COMMAND;
        }
        self.send(UiEvent::Key(KeyEvent { named, mods: bits, raw_char: base.map_or(0, |c| c as u32), bytes }));
    }
}

impl Handler {
    fn win(&mut self, no: u32) -> Option<&mut Win> {
        self.windows.iter_mut().find(|win| win.no == no)
    }

    /// Opens a window: the first one where the last launch left it, with the
    /// core main started; another cascading from window `from`, with a new
    /// core whose first tab opens in `cwd`.
    fn open_window(&mut self, event_loop: &ActiveEventLoop, from: Option<u32>, cwd: Option<String>) {
        let no = self.next;
        let mut attributes = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
            // Settings ▸ Background Opacity lets the desktop show through.
            .with_transparent(cfg!(target_os = "macos"));
        // The window's content runs up under the title bar, which Temino
        // draws itself at TITLEBAR points.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes =
                attributes.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true);
        }
        let parent = from.and_then(|from| self.windows.iter().find(|win| win.no == from));
        let place = match parent {
            Some(parent) => {
                // A little down and right of the window it came from, its size.
                let window = &parent.window;
                let position =
                    if cfg!(target_os = "macos") { window.inner_position() } else { window.outer_position() };
                let size = window.inner_size().to_logical::<f64>(parent.scale);
                position.ok().map(|p| {
                    let p = p.to_logical::<f64>(parent.scale);
                    (p.x + CASCADE, p.y + CASCADE, size.width, size.height)
                })
            }
            None => saved_frame(),
        };
        if let Some((x, y, width, height)) = place {
            attributes =
                attributes.with_position(LogicalPosition::new(x, y)).with_inner_size(LogicalSize::new(width, height));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                crate::fatal(&format!("opening the window: {error}"));
                if self.windows.is_empty() {
                    event_loop.exit();
                }
                return;
            }
        };
        self.next += 1;
        window.set_cursor(CursorIcon::Text);
        // Pinyin and the other input methods.
        window.set_ime_allowed(true);
        #[cfg(not(target_os = "macos"))]
        window.set_blur(BLUR.load(Ordering::Acquire));
        #[cfg(target_os = "macos")]
        if self.windows.is_empty() && self.first.is_some() {
            menu::install();
        }
        let (sender, receiver) = match self.first.take() {
            Some(sender) => (sender, None),
            None => {
                let (sender, receiver) = std::sync::mpsc::channel();
                (sender, Some(receiver))
            }
        };
        let mut win = Win {
            no,
            window: window.clone(),
            surface: None,
            #[cfg(target_os = "macos")]
            on_layer: false,
            sender: sender.clone(),
            scale: window.scale_factor(),
            modifiers: ModifiersState::empty(),
            pointer: (0.0, 0.0),
            buttons: 0,
            last_click: None,
            clicks: 0,
            composing: false,
        };
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) {
                unsafe {
                    temino_metal_attach(handle.ns_view.as_ptr(), no);
                    temino_titlebar(handle.ns_view.as_ptr(), TITLEBAR as f64);
                    temino_blur(handle.ns_view.as_ptr(), BLUR.load(Ordering::Acquire));
                    temino_viewer_init(handle.ns_view.as_ptr(), no, viewer_event);
                }
                win.on_layer = true;
            }
        }
        #[cfg(target_os = "macos")]
        let needs_surface = !win.on_layer;
        #[cfg(not(target_os = "macos"))]
        let needs_surface = true;
        if needs_surface {
            let surface = softbuffer::Context::new(window.clone())
                .and_then(|context| softbuffer::Surface::new(&context, window.clone()));
            match surface {
                Ok(surface) => win.surface = Some(surface),
                Err(error) => {
                    crate::fatal(&format!("setting up the window surface: {error}"));
                    if self.windows.is_empty() {
                        event_loop.exit();
                    }
                    return;
                }
            }
        }
        crate::trace(&format!("window {no}: opened"));
        SENDERS.lock().unwrap().push((no, sender.clone()));
        win.send_size();
        self.windows.push(win);
        WINDOWS.store(self.windows.len(), Ordering::Release);
        if let (Some(receiver), Some(start)) = (receiver, NEW_CORE.get().copied()) {
            let started = std::thread::Builder::new().name(format!("temino core {no}")).spawn(move || {
                WINDOW.with(|window| window.set(no));
                start(receiver, sender, cwd);
            });
            if started.is_err() {
                self.close_window(event_loop, no);
            }
        }
    }

    /// Closes window `no` (its core is done with it); the oldest window
    /// left takes over keeping the tabs, and with none left the app quits.
    fn close_window(&mut self, event_loop: &ActiveEventLoop, no: u32) {
        let Some(index) = self.windows.iter().position(|win| win.no == no) else { return };
        let win = self.windows.remove(index);
        #[cfg(target_os = "macos")]
        unsafe {
            temino_viewer_detach(no);
            temino_metal_detach(no);
        }
        FRAMES.lock().unwrap().retain(|(n, ..)| *n != no);
        SENDERS.lock().unwrap().retain(|(n, _)| *n != no);
        drop(win);
        WINDOWS.store(self.windows.len(), Ordering::Release);
        crate::trace(&format!("window {no}: closed, {} left", self.windows.len()));
        let Some(next) = self.windows.first() else {
            event_loop.exit();
            return;
        };
        if no == self.primary {
            self.primary = next.no;
            next.send(UiEvent::Primary);
        }
    }
}

/// How far down and right a new window opens from the one it came from.
const CASCADE: f64 = 26.0;

impl ApplicationHandler<(u32, Request)> for Handler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.first.is_some() && self.windows.is_empty() {
            self.open_window(event_loop, None, None);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (no, request): (u32, Request)) {
        match request {
            Request::Present => {
                if let Some(win) = self.win(no) {
                    win.draw();
                }
            }
            Request::Title(title) => {
                if let Some(win) = self.win(no) {
                    win.window.set_title(&title);
                }
            }
            Request::Cursor(cursor) => {
                if let Some(win) = self.win(no) {
                    win.window.set_cursor(match cursor {
                        Cursor::IBeam => CursorIcon::Text,
                        Cursor::Arrow => CursorIcon::Default,
                        Cursor::ResizeColumn => CursorIcon::ColResize,
                        Cursor::ResizeRow => CursorIcon::RowResize,
                        Cursor::Link => CursorIcon::Pointer,
                    });
                }
            }
            Request::Blur(on) => {
                if let Some(win) = self.win(no) {
                    #[cfg(not(target_os = "macos"))]
                    win.window.set_blur(on);
                    #[cfg(target_os = "macos")]
                    {
                        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        if let Ok(RawWindowHandle::AppKit(handle)) = win.window.window_handle().map(|h| h.as_raw()) {
                            unsafe { temino_blur(handle.ns_view.as_ptr(), on) };
                        }
                    }
                }
            }
            Request::Caret(x, y, height) => {
                if let Some(win) = self.win(no) {
                    win.window.set_ime_cursor_area(LogicalPosition::new(x, y), LogicalSize::new(1, height.max(1)));
                }
            }
            #[cfg(target_os = "macos")]
            Request::ViewerOpen(id, kind, target, style) => {
                let target = std::ffi::CString::new(target.replace('\0', "")).unwrap_or_default();
                unsafe { temino_viewer_open(id, kind as i32, target.as_ptr(), &style) };
            }
            #[cfg(target_os = "macos")]
            Request::ViewerPlace(frames) => {
                for (id, [x, y, w, h], visible) in frames {
                    unsafe { temino_viewer_place(id, x as f64, y as f64, w as f64, h as f64, visible) };
                }
            }
            #[cfg(target_os = "macos")]
            Request::ViewerClose(id) => unsafe { temino_viewer_close(id) },
            #[cfg(target_os = "macos")]
            Request::ViewerFocus(id) => unsafe { temino_viewer_focus(id.unwrap_or(0)) },
            #[cfg(not(target_os = "macos"))]
            Request::ViewerOpen(..) | Request::ViewerPlace(_) | Request::ViewerClose(_) | Request::ViewerFocus(_) => {}
            Request::NewWindow(cwd) => self.open_window(event_loop, Some(no), cwd),
            Request::CloseWindow => self.close_window(event_loop, no),
            Request::Quit => {
                // The others are asked to finish too (one with unsaved work
                // stays open), then this one closes.
                for win in self.windows.iter().filter(|win| win.no != no) {
                    win.send(UiEvent::Quit);
                }
                self.close_window(event_loop, no);
            }
            #[cfg(target_os = "macos")]
            Request::Hotkey(key) => {
                let (code, modifiers) = key.unwrap_or((u32::MAX, 0));
                unsafe { temino_hotkey(code, modifiers) }
            }
            #[cfg(target_os = "macos")]
            Request::ToggleWindow => unsafe { temino_toggle_window() },
            #[cfg(not(target_os = "macos"))]
            Request::Hotkey(_) | Request::ToggleWindow => {}
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let primary = self.primary;
        let Some(win) = self.windows.iter_mut().find(|win| win.window.id() == id) else { return };
        match event {
            WindowEvent::CloseRequested => win.send(UiEvent::Close),
            WindowEvent::Resized(_) => {
                win.send_size();
                if win.no == primary {
                    win.save_frame();
                }
            }
            WindowEvent::Moved(_) => {
                if win.no == primary {
                    win.save_frame();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                win.scale = scale_factor;
                win.send_size();
            }
            WindowEvent::RedrawRequested => win.draw(),
            WindowEvent::Focused(focused) => {
                if focused {
                    // Menu picks go to the window in front.
                    *MENU_SENDER.lock().unwrap() = Some(win.sender.clone());
                }
                win.send(UiEvent::Focus(focused));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                win.modifiers = modifiers.state();
                win.send(UiEvent::Modifiers(win.mods()));
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Preedit(text, _) => {
                    win.composing = !text.is_empty();
                    win.send(UiEvent::Preedit(text));
                }
                Ime::Commit(text) => {
                    win.composing = false;
                    win.send(UiEvent::Text(text));
                }
                Ime::Disabled => {
                    win.composing = false;
                    win.send(UiEvent::Preedit(String::new()));
                }
                Ime::Enabled => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !win.composing {
                    win.key(&event);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                win.pointer = ((position.x / win.scale) as f32, (position.y / win.scale) as f32);
                let (x, y) = win.pointer;
                win.send(UiEvent::MouseMoved { x, y, buttons: win.buttons });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => buttons::PRIMARY,
                    MouseButton::Right => buttons::SECONDARY,
                    MouseButton::Middle => buttons::TERTIARY,
                    _ => return,
                };
                let (x, y) = win.pointer;
                if state == ElementState::Pressed {
                    let now = Instant::now();
                    let repeat = win.last_click.is_some_and(|(at, (px, py), b)| {
                        b == bit
                            && now.duration_since(at) < DOUBLE_CLICK
                            && (px - x).abs() < 4.0
                            && (py - y).abs() < 4.0
                    });
                    win.clicks = if repeat { win.clicks + 1 } else { 1 };
                    win.last_click = Some((now, (x, y), bit));
                    win.buttons |= bit;
                    win.send(UiEvent::MouseDown { x, y, buttons: bit, clicks: win.clicks, mods: win.mods() });
                } else {
                    win.buttons &= !bit;
                    win.send(UiEvent::MouseUp { x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive dy scrolls down, in lines, as on Haiku.
                let (dx, dy, precise) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x, -y, false),
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => {
                        let line = 16.0 * win.scale;
                        ((-x / line) as f32 / 3.0, (-y / line) as f32 / 3.0, true)
                    }
                };
                let (x, y) = win.pointer;
                win.send(UiEvent::Wheel { dx, dy, x, y, mods: win.mods(), precise });
            }
            _ => {}
        }
    }
}

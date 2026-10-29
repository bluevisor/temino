//! Linux and macOS: a winit window, the clipboard through arboard, and a
//! Unix pseudo-terminal. On Linux each finished frame is shown through
//! softbuffer; on macOS frames render straight into the window's Metal
//! layer (src/macos/shim.m, which also has the menu bar).
//!
//! winit wants the main thread, so the core thread talks to it through an
//! event-loop proxy: `present` parks the newest frame and wakes the loop,
//! which copies it into the window. Events go the other way as `UiEvent`s.

use std::cell::RefCell;
use std::io;
use std::num::NonZeroU32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent as WinitKey, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

use super::{buttons, mods, Cursor, KeyEvent, UiEvent};
use crate::keys::Named;
use crate::Msg;

#[cfg(target_os = "macos")]
extern "C" {
    /// Gives the view the Metal layer that src/metal.rs renders into.
    fn temino_metal_attach(view: *mut std::ffi::c_void);
}

/// What the core thread asks of the window thread.
#[derive(Debug)]
enum Request {
    Present,
    Title(String),
    Cursor(Cursor),
    Blur(bool),
    Quit,
}

struct Frame {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

static PROXY: OnceLock<Mutex<EventLoopProxy<Request>>> = OnceLock::new();
static FRAME: Mutex<Option<Frame>> = Mutex::new(None);
/// Set while a Present request is queued, so frames don't pile up.
static PRESENT_PENDING: AtomicBool = AtomicBool::new(false);
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);
/// Whether the window blurs what's behind it, kept for when it opens.
static BLUR: AtomicBool = AtomicBool::new(false);
/// Where menu picks go; the menu calls back on the main thread.
static MENU_SENDER: Mutex<Option<Sender<Msg>>> = Mutex::new(None);

/// The window `window_new` asked for: title, size, where events go.
type Setup = (String, u32, u32, Sender<Msg>);

thread_local! {
    static EVENT_LOOP: RefCell<Option<EventLoop<Request>>> = const { RefCell::new(None) };
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}

fn request(request: Request) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.lock().unwrap().send_event(request);
    }
}

pub fn app_new(_signature: &str) {
    let mut builder = EventLoop::<Request>::with_user_event();
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
    let mut handler = Handler {
        title,
        size: (width, height),
        sender,
        window: None,
        surface: None,
        #[cfg(target_os = "macos")]
        on_layer: false,
        scale: 1.0,
        modifiers: ModifiersState::empty(),
        pointer: (0.0, 0.0),
        buttons: 0,
        last_click: None,
        clicks: 0,
    };
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
        ("Shell", Some("New Tab"), "t", 0, None, 't'),
        ("Shell", Some("Split Right"), "d", 0, None, 'd'),
        ("Shell", Some("Split Down"), "e", 0, None, 'e'),
        ("Shell", None, "", 0, None, '\0'),
        ("Shell", Some("Close Pane"), "w", 0, None, 'w'),
        ("Edit", Some("Copy"), "c", 0, None, 'c'),
        ("Edit", Some("Copy Command"), "c", SHIFT, None, 'c'),
        ("Edit", Some("Paste"), "v", 0, None, 'v'),
        ("Edit", Some("Select All"), "a", 0, None, 'a'),
        ("Edit", None, "", 0, None, '\0'),
        ("Edit", Some("Clear"), "k", 0, None, 'k'),
        ("View", Some("Show/Hide Tabs"), "b", 0, None, 'b'),
        ("View", Some("Previous Tab"), "{", SHIFT, None, '['),
        ("View", Some("Next Tab"), "}", SHIFT, None, ']'),
        ("View", Some("Previous Pane"), "[", 0, None, '['),
        ("View", Some("Next Pane"), "]", 0, None, ']'),
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
    {
        let mut slot = FRAME.lock().unwrap();
        let frame = slot.get_or_insert_with(|| Frame { pixels: Vec::new(), width: 0, height: 0 });
        frame.pixels.clear();
        // Little-endian BGRA is 0xAARRGGBB; softbuffer wants the top byte clear.
        frame.pixels.extend(
            pixels[..(width * height * 4) as usize].chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0])),
        );
        frame.width = width;
        frame.height = height;
    }
    if !PRESENT_PENDING.swap(true, Ordering::AcqRel) {
        request(Request::Present);
    }
}

pub fn set_title(title: &str) {
    request(Request::Title(title.replace('\0', "")));
}

pub fn set_cursor(cursor: Cursor) {
    request(Request::Cursor(cursor));
}

pub fn set_text_caret(_x: i32, _y: i32, _height: i32) {}

pub fn beep() {}

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

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

struct Handler {
    title: String,
    size: (u32, u32),
    sender: Sender<Msg>,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// Frames go straight to the window's Metal layer, not through softbuffer.
    #[cfg(target_os = "macos")]
    on_layer: bool,
    scale: f64,
    modifiers: ModifiersState,
    /// The pointer, in logical pixels.
    pointer: (f32, f32),
    buttons: i32,
    last_click: Option<(Instant, (f32, f32), i32)>,
    clicks: i32,
}

impl Handler {
    fn send(&self, event: UiEvent) {
        let _ = self.sender.send(Msg::Ui(event));
    }

    /// Remembers where the window is and how big, for the next launch.
    fn save_frame(&self) {
        let Some(window) = &self.window else { return };
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
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
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
        #[cfg(target_os = "macos")]
        if self.on_layer {
            return;
        }
        let Some(surface) = &mut self.surface else { return };
        let slot = FRAME.lock().unwrap();
        let Some(frame) = slot.as_ref() else { return };
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

impl ApplicationHandler<Request> for Handler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attributes = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
            // Settings ▸ Background Opacity lets the desktop show through.
            .with_transparent(cfg!(target_os = "macos"));
        if let Some((x, y, width, height)) = saved_frame() {
            attributes =
                attributes.with_position(LogicalPosition::new(x, y)).with_inner_size(LogicalSize::new(width, height));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                crate::fatal(&format!("opening the window: {error}"));
                event_loop.exit();
                return;
            }
        };
        window.set_cursor(CursorIcon::Text);
        window.set_blur(BLUR.load(Ordering::Acquire));
        #[cfg(target_os = "macos")]
        menu::install();
        self.scale = window.scale_factor();
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) {
                unsafe { temino_metal_attach(handle.ns_view.as_ptr()) };
                self.on_layer = true;
                self.window = Some(window);
                self.send_size();
                return;
            }
        }
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|context| softbuffer::Surface::new(&context, window.clone()));
        match surface {
            Ok(surface) => self.surface = Some(surface),
            Err(error) => {
                crate::fatal(&format!("setting up the window surface: {error}"));
                event_loop.exit();
                return;
            }
        }
        self.window = Some(window);
        self.send_size();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, request: Request) {
        match request {
            Request::Present => {
                PRESENT_PENDING.store(false, Ordering::Release);
                self.draw();
            }
            Request::Title(title) => {
                if let Some(window) = &self.window {
                    window.set_title(&title);
                }
            }
            Request::Cursor(cursor) => {
                if let Some(window) = &self.window {
                    window.set_cursor(match cursor {
                        Cursor::IBeam => CursorIcon::Text,
                        Cursor::Arrow => CursorIcon::Default,
                        Cursor::ResizeColumn => CursorIcon::ColResize,
                        Cursor::ResizeRow => CursorIcon::RowResize,
                    });
                }
            }
            Request::Blur(on) => {
                if let Some(window) = &self.window {
                    window.set_blur(on);
                }
            }
            Request::Quit => event_loop.exit(),
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.send(UiEvent::Close),
            WindowEvent::Resized(_) => {
                self.send_size();
                self.save_frame();
            }
            WindowEvent::Moved(_) => self.save_frame(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = scale_factor;
                self.send_size();
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Focused(focused) => self.send(UiEvent::Focus(focused)),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.key(&event);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ((position.x / self.scale) as f32, (position.y / self.scale) as f32);
                let (x, y) = self.pointer;
                self.send(UiEvent::MouseMoved { x, y, buttons: self.buttons });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => buttons::PRIMARY,
                    MouseButton::Right => buttons::SECONDARY,
                    MouseButton::Middle => buttons::TERTIARY,
                    _ => return,
                };
                let (x, y) = self.pointer;
                if state == ElementState::Pressed {
                    let now = Instant::now();
                    let repeat = self.last_click.is_some_and(|(at, (px, py), b)| {
                        b == bit
                            && now.duration_since(at) < DOUBLE_CLICK
                            && (px - x).abs() < 4.0
                            && (py - y).abs() < 4.0
                    });
                    self.clicks = if repeat { self.clicks + 1 } else { 1 };
                    self.last_click = Some((now, (x, y), bit));
                    self.buttons |= bit;
                    self.send(UiEvent::MouseDown { x, y, buttons: bit, clicks: self.clicks, mods: self.mods() });
                } else {
                    self.buttons &= !bit;
                    self.send(UiEvent::MouseUp { x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive dy scrolls down, in lines, as on Haiku.
                let (dx, dy, precise) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x, -y, false),
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => {
                        let line = 16.0 * self.scale;
                        ((-x / line) as f32 / 3.0, (-y / line) as f32 / 3.0, true)
                    }
                };
                let (x, y) = self.pointer;
                self.send(UiEvent::Wheel { dx, dy, x, y, mods: self.mods(), precise });
            }
            _ => {}
        }
    }
}

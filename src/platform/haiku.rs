//! Haiku: the C ABI of `src/haiku/shim.cpp`: the window, frame presentation, the
//! clipboard and the pseudo-terminal, plus the events the window delivers.

use std::ffi::{c_char, c_void, CString};
use std::io;
use std::sync::mpsc::Sender;

use super::{Cursor, KeyEvent, UiEvent};
use crate::keys::Named;
use crate::Msg;

#[repr(C)]
struct RawEvent {
    kind: i32,
    key: i32,
    modifiers: u32,
    buttons: i32,
    clicks: i32,
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    width: i32,
    height: i32,
    raw_char: i32,
    nbytes: i32,
    bytes: [u8; 16],
}

type Callback = extern "C" fn(*const RawEvent, *mut c_void);

extern "C" {
    fn temino_app_new(signature: *const c_char);
    fn temino_window_new(title: *const c_char, width: i32, height: i32, callback: Callback, context: *mut c_void);
    fn temino_app_run();
    fn temino_quit();
    fn temino_set_background(r: u8, g: u8, b: u8);
    fn temino_present(pixels: *const u8, width: i32, height: i32, stride: i32);
    fn temino_set_title(title: *const c_char);
    fn temino_set_cursor(kind: i32);
    fn temino_beep();
    fn temino_clipboard_set(text: *const c_char, length: i32) -> i32;
    fn temino_clipboard_get(length: *mut i32) -> *mut c_char;
    fn temino_free(pointer: *mut c_void);
    fn temino_spawn(
        program: *const c_char,
        argv: *const *const c_char,
        argc: i32,
        envp: *const *const c_char,
        cwd: *const c_char,
        columns: i32,
        rows: i32,
        master: *mut i32,
    ) -> i32;
    fn temino_pty_resize(master: i32, columns: i32, rows: i32, pixel_width: i32, pixel_height: i32) -> i32;
    fn temino_pty_exec(argc: i32, argv: *const *const c_char) -> i32;
    fn temino_alert(text: *const c_char);
    fn temino_wait(pid: i32) -> i32;
    fn temino_hangup(pid: i32);
    fn temino_user_name() -> *const c_char;
}

/// Haiku's physical key codes for the keys whose bytes collide with
/// control characters (B_HOME is ^A), so they are told apart by code.
mod code {
    pub const ESCAPE: i32 = 0x01;
    pub const F1: i32 = 0x02;
    pub const F12: i32 = 0x0d;
    pub const BACKSPACE: i32 = 0x1e;
    pub const INSERT: i32 = 0x1f;
    pub const HOME: i32 = 0x20;
    pub const PAGE_UP: i32 = 0x21;
    pub const TAB: i32 = 0x26;
    pub const DELETE: i32 = 0x34;
    pub const END: i32 = 0x35;
    pub const PAGE_DOWN: i32 = 0x36;
    pub const ENTER: i32 = 0x47;
    pub const UP: i32 = 0x57;
    pub const KEYPAD_ENTER: i32 = 0x5b;
    pub const LEFT: i32 = 0x61;
    pub const DOWN: i32 = 0x62;
    pub const RIGHT: i32 = 0x63;
}

/// The special key a Haiku key code (and its bytes) stands for.
fn named(key: i32, bytes: &[u8]) -> Option<Named> {
    let by_code = match key {
        code::UP => Some(Named::Up),
        code::DOWN => Some(Named::Down),
        code::LEFT => Some(Named::Left),
        code::RIGHT => Some(Named::Right),
        code::HOME => Some(Named::Home),
        code::END => Some(Named::End),
        code::PAGE_UP => Some(Named::PageUp),
        code::PAGE_DOWN => Some(Named::PageDown),
        code::INSERT => Some(Named::Insert),
        code::DELETE => Some(Named::Delete),
        code::BACKSPACE => Some(Named::Backspace),
        code::TAB => Some(Named::Tab),
        code::ENTER | code::KEYPAD_ENTER => Some(Named::Enter),
        code::ESCAPE => Some(Named::Escape),
        k @ code::F1..=code::F12 => Some(Named::F((k - code::F1 + 1) as u8)),
        _ => None,
    };
    if by_code.is_some() {
        return by_code;
    }
    // The keypad with Num Lock off sends navigation bytes.
    let keypad = matches!(key, 0x37..=0x39 | 0x48..=0x4a | 0x58..=0x5a | 0x64 | 0x65);
    if keypad && bytes.len() == 1 {
        return match bytes[0] {
            0x1e => Some(Named::Up),
            0x1f => Some(Named::Down),
            0x1c => Some(Named::Left),
            0x1d => Some(Named::Right),
            0x01 => Some(Named::Home),
            0x04 => Some(Named::End),
            0x0b => Some(Named::PageUp),
            0x0c => Some(Named::PageDown),
            0x05 => Some(Named::Insert),
            0x7f => Some(Named::Delete),
            _ => None,
        };
    }
    None
}

extern "C" fn on_event(raw: *const RawEvent, context: *mut c_void) {
    // SAFETY: the shim passes a valid event and the context set up in
    // `window_new`, which lives for the rest of the process.
    let (raw, sender) = unsafe { (&*raw, &*(context as *const Sender<Msg>)) };
    let event = match raw.kind {
        1 => {
            let bytes = raw.bytes[..raw.nbytes.clamp(0, 16) as usize].to_vec();
            UiEvent::Key(KeyEvent {
                named: named(raw.key, &bytes),
                mods: raw.modifiers,
                raw_char: raw.raw_char as u32,
                bytes,
            })
        }
        2 => UiEvent::MouseDown { x: raw.x, y: raw.y, buttons: raw.buttons, clicks: raw.clicks, mods: raw.modifiers },
        3 => UiEvent::MouseUp { x: raw.x, y: raw.y },
        4 => UiEvent::MouseMoved { x: raw.x, y: raw.y, buttons: raw.buttons },
        5 => UiEvent::Wheel { dx: raw.dx, dy: raw.dy, x: raw.x, y: raw.y, mods: raw.modifiers },
        6 => UiEvent::Resize { width: raw.width.max(1) as u32, height: raw.height.max(1) as u32, scale: 1.0 },
        7 => UiEvent::Focus(raw.buttons != 0),
        8 => UiEvent::Close,
        _ => return,
    };
    let _ = sender.send(Msg::Ui(event));
}

pub fn app_new(signature: &str) {
    let signature = CString::new(signature).unwrap();
    unsafe { temino_app_new(signature.as_ptr()) }
}

pub fn window_new(title: &str, width: u32, height: u32, sender: Sender<Msg>) {
    let title = CString::new(title).unwrap();
    let context = Box::into_raw(Box::new(sender)) as *mut c_void;
    unsafe { temino_window_new(title.as_ptr(), width as i32, height as i32, on_event, context) }
}

/// Runs the application loop on the calling (main) thread until quit.
pub fn app_run() {
    unsafe { temino_app_run() }
}

pub fn quit() {
    unsafe { temino_quit() }
}

pub fn set_background(rgb: [u8; 3]) {
    unsafe { temino_set_background(rgb[0], rgb[1], rgb[2]) }
}

/// Copies a BGRA frame into the window's back bitmap and shows it.
pub fn present(pixels: &[u8], width: u32, height: u32) {
    debug_assert!(pixels.len() >= (width * height * 4) as usize);
    unsafe { temino_present(pixels.as_ptr(), width as i32, height as i32, (width * 4) as i32) }
}

pub fn set_title(title: &str) {
    if let Ok(title) = CString::new(title.replace('\0', "")) {
        unsafe { temino_set_title(title.as_ptr()) }
    }
}

pub fn set_cursor(cursor: Cursor) {
    unsafe { temino_set_cursor(cursor as i32) }
}

pub fn beep() {
    unsafe { temino_beep() }
}

pub fn clipboard_set(text: &str) {
    unsafe { temino_clipboard_set(text.as_ptr() as *const c_char, text.len() as i32) };
}

pub fn clipboard_get() -> Option<String> {
    let mut length = 0;
    let pointer = unsafe { temino_clipboard_get(&mut length) };
    if pointer.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(pointer as *const u8, length as usize) };
    let text = String::from_utf8_lossy(bytes).into_owned();
    unsafe { temino_free(pointer as *mut c_void) };
    Some(text)
}

/// Starts `argv` on a new pseudo-terminal; returns (team id, master fd).
pub fn spawn(argv: &[String], env: &[String], cwd: &str, columns: u16, rows: u16) -> io::Result<(i32, i32)> {
    let to_c = |items: &[String]| -> Vec<CString> {
        items.iter().filter_map(|item| CString::new(item.as_str()).ok()).collect()
    };
    let program = std::env::current_exe()?;
    let program = CString::new(program.to_string_lossy().into_owned()).unwrap_or_default();
    let argv = to_c(argv);
    let env = to_c(env);
    let argv_ptrs: Vec<*const c_char> = argv.iter().map(|s| s.as_ptr()).collect();
    let mut env_ptrs: Vec<*const c_char> = env.iter().map(|s| s.as_ptr()).collect();
    env_ptrs.push(std::ptr::null());
    let cwd = CString::new(cwd).unwrap_or_default();
    let mut master = -1;
    let team = unsafe {
        temino_spawn(
            program.as_ptr(),
            argv_ptrs.as_ptr(),
            argv_ptrs.len() as i32,
            env_ptrs.as_ptr(),
            cwd.as_ptr(),
            columns as i32,
            rows as i32,
            &mut master,
        )
    };
    if team < 0 {
        // Haiku's error codes (errno values included) are negative.
        return Err(io::Error::from_raw_os_error(team));
    }
    Ok((team, master))
}

/// The child half of `spawn`, run as `temino --pty-exec ...`: sets up the
/// terminal and execs the program. Returns only on failure.
pub fn pty_exec(args: &[String]) -> i32 {
    let args: Vec<CString> = args.iter().filter_map(|a| CString::new(a.as_str()).ok()).collect();
    let mut pointers: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
    pointers.push(std::ptr::null());
    unsafe { temino_pty_exec(args.len() as i32, pointers.as_ptr()) }
}

pub fn alert(text: &str) {
    if let Ok(text) = CString::new(text.replace('\0', "")) {
        unsafe { temino_alert(text.as_ptr()) }
    }
}

pub fn pty_resize(master: i32, columns: u16, rows: u16, pixel_width: u32, pixel_height: u32) {
    unsafe { temino_pty_resize(master, columns as i32, rows as i32, pixel_width as i32, pixel_height as i32) };
}

/// Blocks until `pid` exits and returns its exit code.
pub fn wait(pid: i32) -> i32 {
    unsafe { temino_wait(pid) }
}

pub fn user_name() -> Option<String> {
    let name = unsafe { temino_user_name() };
    (!name.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned())
}

pub fn hangup(pid: i32) {
    unsafe { temino_hangup(pid) }
}

/// Haiku has no primary selection.
pub fn selection_set(_text: &str) {}

pub fn selection_get() -> Option<String> {
    None
}

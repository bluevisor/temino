//! Crash tracking. A panic is already logged (store.rs); this covers the
//! ways the app dies that never reach Rust: a bad pointer or a runaway
//! stack in native code, an abort, a force quit, the system killing it.
//!
//! Each launch leaves a `running-<pid>` file in the settings directory and
//! takes it away on a normal quit; one found at the next launch, its app
//! gone, is a session that didn't end by quitting. On macOS and Linux a
//! signal handler also writes what killed it (and on macOS the stack) to
//! temino.log and crash.log before the system's own crash report is made.

use crate::store;
use std::sync::OnceLock;

/// This launch's `running-<pid>` file, taken away at exit.
static MARKER: OnceLock<String> = OnceLock::new();

// Declared here, not taken from the libc crate: Haiku builds without it.
extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

/// Starts tracking: says how the last session ended if it wasn't by
/// quitting, notes this launch in temino.log and watches for a crash.
pub fn install() {
    let dir = crate::settings_dir();
    crate::create_private_dir(&dir);
    for report in unclean_exits(&dir) {
        store::log(&report);
    }
    store::log(&format!(
        "started: pid {}, {} build, {} {}",
        std::process::id(),
        if cfg!(debug_assertions) { "debug" } else { "release" },
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let marker = format!("{dir}/running-{}", std::process::id());
    if std::fs::write(&marker, store::timestamp_now()).is_ok() && MARKER.set(marker).is_ok() {
        // Any way out through exit(), AppKit's own included.
        unsafe { atexit(leave) };
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    signals::install(&dir);
}

extern "C" fn leave() {
    if let Some(marker) = MARKER.get() {
        let _ = std::fs::remove_file(marker);
    }
    crate::debug_log!("quit");
}

/// A line for each earlier session that left its `running-<pid>` file
/// behind though its app is gone; the file is cleared.
fn unclean_exits(dir: &str) -> Vec<String> {
    let mut reports = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return reports };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(marker_pid) else { continue };
        if pid == std::process::id() || is_running(pid) {
            continue;
        }
        let started = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let _ = std::fs::remove_file(entry.path());
        reports.push(format!(
            "the session started {} (pid {pid}) ended without quitting: a crash, a force quit or a kill{}",
            started.trim(),
            if cfg!(target_os = "macos") { "; see ~/Library/Logs/DiagnosticReports for a crash report" } else { "" }
        ));
    }
    reports
}

/// The pid in a `running-<pid>` file name.
fn marker_pid(name: &str) -> Option<u32> {
    name.strip_prefix("running-")?.parse().ok()
}

/// Whether a process with this pid exists (ours to signal or not).
fn is_running(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    unsafe { kill(pid, 0) == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied }
}

/// A line of the debug log from the macOS shim (debug builds only).
#[cfg(target_os = "macos")]
#[no_mangle]
extern "C" fn temino_debug_line(text: *const std::ffi::c_char) {
    if !text.is_null() {
        store::debug(&unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy());
    }
}

/// The handler for the signals a crash arrives as. Everything in it has to
/// be safe to do in a signal handler, on the small stack Rust gives each
/// thread for one: no allocating, no locks, only system calls on paths and
/// buffers made ready beforehand.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod signals {
    use crate::store::Stamp;
    use std::ffi::{c_int, c_void, CString};
    use std::fmt::Write;
    use std::sync::OnceLock;

    /// temino.log, then crash.log (the latest crash on its own).
    static PATHS: OnceLock<[CString; 2]> = OnceLock::new();

    #[cfg(target_os = "macos")]
    extern "C" {
        fn backtrace(frames: *mut *mut c_void, size: c_int) -> c_int;
        fn backtrace_symbols_fd(frames: *const *mut c_void, size: c_int, fd: c_int);
    }

    pub fn install(dir: &str) {
        let (Ok(log), Ok(crash)) =
            (CString::new(format!("{dir}/temino.log")), CString::new(format!("{dir}/crash.log")))
        else {
            return;
        };
        let _ = PATHS.set([log, crash]);
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_crash as *const () as usize;
            // On the thread's signal stack (its own may be what ran out),
            // and once: the default action is back for what comes next.
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut action.sa_mask);
            for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGFPE, libc::SIGABRT, libc::SIGTRAP] {
                libc::sigaction(signal, &action, std::ptr::null_mut());
            }
        }
    }

    /// Text built in place, cut short when it's full.
    struct Line {
        bytes: [u8; 320],
        len: usize,
    }

    impl Write for Line {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let room = self.bytes.len() - self.len;
            let mut take = text.len().min(room);
            // Cut on a character boundary, never inside a character:
            // the log would otherwise hold bytes nothing can read.
            while !text.is_char_boundary(take) {
                take -= 1;
            }
            self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
            self.len += take;
            Ok(())
        }
    }

    fn signal_name(signal: c_int) -> &'static str {
        match signal {
            libc::SIGSEGV => "SIGSEGV (bad memory access)",
            libc::SIGBUS => "SIGBUS (bad memory access)",
            libc::SIGILL => "SIGILL (illegal instruction)",
            libc::SIGFPE => "SIGFPE (arithmetic error)",
            libc::SIGABRT => "SIGABRT (abort)",
            libc::SIGTRAP => "SIGTRAP (trap)",
            _ => "a signal",
        }
    }

    /// Where and on which thread, as far as this system says.
    #[cfg(target_os = "macos")]
    unsafe fn describe_thread(line: &mut Line, address: usize) {
        let thread = libc::pthread_self();
        // The stack grows down from `top`; a fault just past its low end
        // is the stack running out.
        let top = libc::pthread_get_stackaddr_np(thread) as usize;
        let low = top.saturating_sub(libc::pthread_get_stacksize_np(thread));
        if address != 0 && address < low + 4096 && address + (1 << 20) > low {
            let _ = line.write_str(": stack overflow (runaway recursion)");
        }
        if libc::pthread_main_np() != 0 {
            let _ = line.write_str(", main thread");
            return;
        }
        let mut name = [0u8; 64];
        if libc::pthread_getname_np(thread, name.as_mut_ptr().cast(), name.len()) == 0 {
            let len = name.iter().position(|&b| b == 0).unwrap_or(0);
            if let Ok(name) = std::str::from_utf8(&name[..len]) {
                let _ = write!(line, ", thread {name:?}");
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    unsafe fn describe_thread(_line: &mut Line, _address: usize) {}

    extern "C" fn on_crash(signal: c_int, info: *mut libc::siginfo_t, _context: *mut c_void) {
        unsafe {
            #[cfg(target_os = "macos")]
            let address = if info.is_null() { 0 } else { (*info).si_addr as usize };
            #[cfg(not(target_os = "macos"))]
            let address = if info.is_null() { 0 } else { (*info).si_addr() as usize };
            let mut line = Line { bytes: [0; 320], len: 0 };
            let now = u64::try_from(libc::time(std::ptr::null_mut())).unwrap_or(0);
            let _ = write!(line, "{} {} crashed: {}", Stamp(now), env!("CARGO_PKG_VERSION"), signal_name(signal));
            if signal != libc::SIGABRT {
                let _ = write!(line, " at {address:#x}");
            }
            describe_thread(&mut line, address);
            // Leave room for the line's end whatever was cut.
            line.len = line.len.min(line.bytes.len() - 1);
            let _ = line.write_str("\n");
            #[cfg(target_os = "macos")]
            let mut frames = [std::ptr::null_mut::<c_void>(); 64];
            #[cfg(target_os = "macos")]
            let depth = backtrace(frames.as_mut_ptr(), frames.len() as c_int);
            if let Some(paths) = PATHS.get() {
                for (path, extra) in [(&paths[0], libc::O_APPEND), (&paths[1], libc::O_TRUNC)] {
                    let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CREAT | extra, 0o600 as c_int);
                    if fd < 0 {
                        continue;
                    }
                    libc::write(fd, line.bytes.as_ptr().cast(), line.len);
                    #[cfg(target_os = "macos")]
                    backtrace_symbols_fd(frames.as_ptr(), depth, fd);
                    libc::close(fd);
                }
            }
            // Returning runs the faulting instruction again (or lets abort()
            // finish), now with the default action: the app dies as it would
            // have, and the system writes its own crash report.
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_full_line_cuts_on_a_character_boundary() {
            // 319 one-byte characters, then a two-byte one across the
            // buffer's end: the line stops before it, whole.
            let mut line = Line { bytes: [0; 320], len: 0 };
            let text = format!("{}é", "x".repeat(319));
            let _ = line.write_str(&text);
            assert_eq!(line.len, 319);
            assert_eq!(std::str::from_utf8(&line.bytes[..line.len]).unwrap(), "x".repeat(319));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_name_their_pid() {
        assert_eq!(marker_pid("running-4242"), Some(4242));
        assert_eq!(marker_pid("running-"), None);
        assert_eq!(marker_pid("temino.log"), None);
    }

    #[test]
    fn a_session_that_did_not_quit_is_reported_once() {
        let dir = std::env::temp_dir().join(format!("temino-crash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // No process has this pid; ours is still running.
        std::fs::write(dir.join("running-2147483646"), "2026-11-11T06:11:02Z").unwrap();
        std::fs::write(dir.join(format!("running-{}", std::process::id())), "now").unwrap();
        assert!(is_running(std::process::id()));
        assert!(!is_running(2_147_483_646));
        let reports = unclean_exits(&dir.to_string_lossy());
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("started 2026-11-11T06:11:02Z (pid 2147483646) ended without quitting"));
        assert!(unclean_exits(&dir.to_string_lossy()).is_empty());
        assert!(!dir.join("running-2147483646").exists());
        assert!(dir.join(format!("running-{}", std::process::id())).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

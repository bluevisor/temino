//! Failing safely: files saved all at once, locks that outlive a panic on
//! another thread, an error log that's always on, and a crash handler that
//! lets a helper thread go down without taking the window with it.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// temino.log starts over past this size; the previous one is kept as
/// temino.log.1.
const LOG_LIMIT: u64 = 256 * 1024;

/// Locking that survives a panic elsewhere. A thread that panicked while
/// holding the lock leaves the data as it was, still usable here; without
/// this every later lock would panic too and bring the app down with it.
pub trait Locked<T> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> Locked<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Replaces `path` with `contents` all at once: written to a temporary file
/// beside it, then renamed over it, so a crash or a full disk mid-save
/// leaves the old file whole. `private` makes it readable only by you.
/// False (and a line in the error log) when it couldn't be saved.
pub fn write_atomic(path: &str, contents: &[u8], private: bool) -> bool {
    match try_write(path, contents, private) {
        Ok(()) => true,
        Err(error) => {
            log(&format!("can't save {path}: {error}"));
            false
        }
    }
}

fn try_write(path: &str, contents: &[u8], private: bool) -> std::io::Result<()> {
    // Unique per call: two windows in one process may save the same file.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = format!("{path}.saving-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let result =
        options.open(&temp).and_then(|mut file| file.write_all(contents)).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Appends a timestamped line to temino.log in the settings directory. Unlike
/// trace.log (TEMINO_TRACE), it's always on, for failures worth knowing about
/// afterwards: a file that couldn't be saved, a helper thread that crashed.
pub fn log(message: &str) {
    eprintln!("temino: {message}");
    let dir = crate::settings_dir();
    crate::create_private_dir(&dir);
    let path = format!("{dir}/temino.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = std::fs::rename(&path, format!("{path}.1"));
    }
    let line = format!("{} {} {message}\n", timestamp(now_secs()), env!("CARGO_PKG_VERSION"));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// The time now, as `timestamp` writes it.
pub fn timestamp_now() -> String {
    timestamp(now_secs())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `secs` since 1970 as UTC, `2026-11-07T16:24:05Z`.
pub fn timestamp(secs: u64) -> String {
    Stamp(secs).to_string()
}

/// `timestamp`, written without allocating (the crash handler can't).
pub struct Stamp(pub u64);

impl std::fmt::Display for Stamp {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (days, rest) = (self.0 / 86_400, self.0 % 86_400);
        // Days to a civil date (Howard Hinnant's algorithm).
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        write!(f, "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
    }
}

/// In a debug build, a line in temino.log for what the app is doing
/// (windows, tabs, shells; never what's typed), so a crash report has the
/// minutes before it. Nothing in a release build.
#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            $crate::store::debug(&format!($($arg)*));
        }
    };
}

/// A `debug_log!` line. One said over and over (a layout loop, say) is
/// written once, then counted when something else follows.
pub fn debug(message: &str) {
    static LAST: Mutex<(String, u32)> = Mutex::new((String::new(), 0));
    let repeats = {
        let mut last = LAST.locked();
        if last.0 == message {
            last.1 += 1;
            return;
        }
        std::mem::replace(&mut *last, (message.to_owned(), 0)).1
    };
    if repeats > 0 {
        log(&format!("debug: (the line before, {repeats} more times)"));
    }
    log(&format!("debug: {message}"));
}

/// Whether a panic on the thread called `name` takes the app down. The
/// window and its panes live on the main and core threads; a helper (the
/// relay, a download, a model request, one pane's reader) going down only
/// loses its own job.
pub fn is_fatal_thread(name: Option<&str>) -> bool {
    matches!(name, Some(name) if name == "main" || name.starts_with("temino core"))
}

/// What a panic says, and where.
fn describe(info: &std::panic::PanicHookInfo) -> String {
    let location = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_default();
    format!("{payload}{location}")
}

/// A panic on the main or a core thread shows an alert and quits, as
/// before; anywhere else it's logged and the app carries on.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let what = describe(info);
        if is_fatal_thread(thread.name()) {
            crate::fatal(&format!("crashed: {what}"));
            crate::platform::quit();
        } else {
            log(&format!("thread {} crashed: {what}", thread.name().unwrap_or("unnamed")));
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_read_as_utc() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(timestamp(1_793_982_245), "2026-11-06T16:24:05Z");
    }

    #[test]
    fn only_the_window_threads_are_fatal() {
        assert!(is_fatal_thread(Some("main")));
        assert!(is_fatal_thread(Some("temino core")));
        assert!(is_fatal_thread(Some("temino core 2")));
        assert!(!is_fatal_thread(Some("relay")));
        assert!(!is_fatal_thread(Some("temino pty reader")));
        assert!(!is_fatal_thread(None));
    }

    #[test]
    fn a_poisoned_lock_still_opens() {
        let shared = std::sync::Arc::new(Mutex::new(7));
        let held = shared.clone();
        let _ = std::thread::spawn(move || {
            let _guard = held.lock().unwrap();
            panic!("poison it");
        })
        .join();
        assert!(shared.lock().is_err());
        assert_eq!(*shared.locked(), 7);
    }

    #[test]
    fn saves_replace_the_whole_file() {
        let dir = std::env::temp_dir().join(format!("temino-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("file").to_string_lossy().into_owned();
        assert!(write_atomic(&path, b"first version, longer", true));
        assert!(write_atomic(&path, b"second", true));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // No temporary files are left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_save_keeps_the_old_file() {
        // Saving into a folder that doesn't exist fails without side effects.
        assert!(try_write("/nonexistent-temino-dir/file", b"x", false).is_err());
    }
}

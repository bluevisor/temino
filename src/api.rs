//! Temino's local API: other programs (scripts, coding agents, the `temino`
//! CLI and its MCP server) drive the windows over a Unix socket. Each
//! request is one line of JSON, `{"method": …, "params": {…}}`, and gets
//! one line back, `{"result": …}` or `{"error": "…"}`.
//!
//! The socket is `temino.sock` in the settings directory, which only you
//! can open (as with tmux, anything running as you may drive Temino).
//! Every shell gets TEMINO_SOCKET, TEMINO_WINDOW and TEMINO_PANE, so a
//! program in a pane knows where it is. `api=off` in the settings file
//! turns the socket off.
//!
//! Methods (panes are numbered per window; `window` defaults to 1):
//! - `list`: every window, its tabs and their panes
//! - `new_tab` {cwd?, command?} → {window, pane}
//! - `split` {pane?, direction: "right"|"down", command?} → {window, pane}
//! - `focus` {pane}, `close` {pane}
//! - `send_text` {pane, text}: typed into the pane (its editor at a prompt)
//! - `send_keys` {pane, keys: ["Enter", "ctrl+c", …]}
//! - `run` {pane, command, wait?, timeout?} → {block}, or the finished
//!   block when `wait` (timeout in seconds, 120 by default)
//! - `read` {pane, blocks?, lines?}: the last blocks' commands and output
//! - `block` {pane, block}: one block
//! - `notify` {title, body}: a desktop notification

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

use crate::json::{self, Value};
use crate::remote::quote;
use crate::Msg;

/// A request for one window's core, and where its answer goes.
pub struct Call {
    pub method: String,
    /// The params object, as JSON.
    pub params: String,
    pub reply: Sender<Result<String, String>>,
}

/// How long a window has to answer.
const ANSWER: Duration = Duration::from_secs(5);

/// Where the socket is: TEMINO_SOCKET (inside Temino's shells), else
/// where this settings directory's Temino listens.
pub fn socket_path() -> String {
    std::env::var("TEMINO_SOCKET").ok().filter(|path| !path.is_empty()).unwrap_or_else(own_socket)
}

/// Longest socket path every system takes (sun_path is 104 bytes on macOS
/// and Haiku, 108 on Linux, the last a NUL).
const SOCKET_PATH_MAX: usize = 103;

/// `temino.sock` in the settings directory; or, when that path is too long
/// for a socket (TEMINO_SETTINGS deep in a tree), one named for the
/// directory in the user's runtime folder.
fn own_socket() -> String {
    let dir = crate::settings_dir();
    let path = format!("{dir}/temino.sock");
    if path.len() <= SOCKET_PATH_MAX {
        return path;
    }
    let runtime = ["XDG_RUNTIME_DIR", "TMPDIR"]
        .iter()
        .find_map(|key| std::env::var(key).ok().filter(|d| !d.is_empty()))
        .unwrap_or_else(|| "/tmp".to_owned());
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dir.hash(&mut h);
        h.finish()
    };
    format!("{}/temino-{:012x}.sock", runtime.trim_end_matches('/'), hash & 0xffff_ffff_ffff)
}

/// Starts listening, once, unless the settings turn it off. Returns the
/// socket's path while it listens.
pub fn start() -> Option<&'static str> {
    static STARTED: OnceLock<Option<String>> = OnceLock::new();
    STARTED.get_or_init(listen).as_deref()
}

#[cfg(unix)]
fn listen() -> Option<String> {
    if crate::setting("api").as_deref() == Some("off") {
        return None;
    }
    let path = own_socket();
    crate::create_private_dir(&crate::settings_dir());
    let at = path.clone();
    let _ = std::thread::Builder::new().name("api".into()).spawn(move || {
        let Some(listener) = bind(&at) else { return };
        for stream in listener.incoming().flatten() {
            let _ = std::thread::Builder::new().name("api-client".into()).spawn(move || serve(stream));
        }
    });
    Some(path)
}

/// Takes the socket at `path`. One left by a Temino that's gone is taken
/// over; while another Temino answers on it (still quitting, say, when
/// this one was launched to replace it), this waits and tries again.
#[cfg(unix)]
fn bind(path: &str) -> Option<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let mut waited = Duration::ZERO;
    loop {
        if UnixStream::connect(path).is_ok() {
            if waited.is_zero() {
                crate::trace("api: another Temino has the socket; waiting for it");
            }
            std::thread::sleep(Duration::from_millis(500));
            waited += Duration::from_millis(500);
            continue;
        }
        let _ = std::fs::remove_file(path);
        match UnixListener::bind(path) {
            Ok(listener) => {
                // Yours alone, wherever it lands.
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                return Some(listener);
            }
            Err(error) => {
                crate::trace(&format!("api: can't listen on {path}: {error}"));
                return None;
            }
        }
    }
}

#[cfg(not(unix))]
fn listen() -> Option<String> {
    None
}

/// The longest request line read, in bytes. A request is one line of
/// JSON naming a method and its params — nothing real comes near this —
/// and a client that never ends its line would otherwise have the
/// socket read it into memory without limit.
const MAX_LINE: u64 = 1 << 20;

/// One line from `reader`, without its ending, or None when the client
/// hung up (or the read failed) — or sent a line past MAX_LINE, whose
/// rest would read as more lines, so the connection is given up on
/// rather than answering a truncated request.
fn request_line(reader: &mut impl BufRead) -> Option<String> {
    let mut line = String::new();
    let read = reader.by_ref().take(MAX_LINE).read_line(&mut line).ok()?;
    if read == 0 || (read as u64 >= MAX_LINE && !line.ends_with('\n')) {
        return None;
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Some(line)
}

/// One client: a request a line, an answer a line, until it hangs up.
#[cfg(unix)]
fn serve(stream: UnixStream) {
    let Ok(mut writer) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    while let Some(line) = request_line(&mut reader) {
        if line.trim().is_empty() {
            continue;
        }
        let answer = match answer(&line) {
            Ok(result) => format!(r#"{{"result":{result}}}"#),
            Err(error) => format!(r#"{{"error":{}}}"#, quote(&error)),
        };
        if writeln!(writer, "{answer}").is_err() {
            return;
        }
    }
}

/// The result of one request line, as JSON.
fn answer(line: &str) -> Result<String, String> {
    let request = json::parse(line).ok_or("the request isn't JSON")?;
    let method = request.str_at("method").ok_or("the request has no method")?;
    let params = request.get("params");
    let window = params.and_then(|p| p.num_at("window")).map_or(1, |w| w as u32);
    let params_json = params.map(write).unwrap_or_else(|| "{}".to_owned());
    match method {
        "notify" => {
            let param = |key| params.and_then(|p| p.str_at(key)).unwrap_or_default();
            crate::notify::show(param("title"), param("body"));
            Ok("true".to_owned())
        }
        "list" => {
            let windows: Vec<String> =
                crate::remote::windows().into_iter().filter_map(|no| ask(no, "list", "{}").ok()).collect();
            Ok(format!(r#"{{"windows":[{}]}}"#, windows.join(",")))
        }
        "run" if params.and_then(|p| p.get("wait")).is_some_and(Value::is_true) => {
            let started = ask(window, "run", &params_json)?;
            let block = json::parse(&started).and_then(|v| v.num_at("block")).ok_or("no block came of it")?;
            let pane = params.and_then(|p| p.num_at("pane")).unwrap_or(0.0);
            let timeout = params.and_then(|p| p.num_at("timeout")).unwrap_or(120.0).clamp(0.0, 3600.0);
            wait(window, pane as u64, block as u64, Duration::from_secs_f64(timeout))
        }
        _ => ask(window, method, &params_json),
    }
}

/// Checks on a block until it's done or `timeout` passes; its last state.
fn wait(window: u32, pane: u64, block: u64, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let params = format!(r#"{{"pane":{pane},"block":{block}}}"#);
    loop {
        let state = ask(window, "block", &params)?;
        let running = json::parse(&state).and_then(|v| v.get("running").map(Value::is_true)).unwrap_or(false);
        if !running || Instant::now() >= deadline {
            return Ok(state);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Asks window `no`'s core, and waits for its answer.
fn ask(no: u32, method: &str, params: &str) -> Result<String, String> {
    let sender = crate::remote::window_sender(no).ok_or_else(|| format!("there's no window {no}"))?;
    let (reply, answer) = mpsc::channel();
    sender
        .send(Msg::Api(Call { method: method.to_owned(), params: params.to_owned(), reply }))
        .map_err(|_| format!("window {no} closed"))?;
    answer.recv_timeout(ANSWER).map_err(|_| format!("window {no} didn't answer"))?
}

/// `value` as JSON text again.
pub fn write(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => format!("{}", *n as i64),
        Value::Number(n) => n.to_string(),
        Value::String(s) => quote(s),
        Value::Array(items) => format!("[{}]", items.iter().map(write).collect::<Vec<_>>().join(",")),
        Value::Object(fields) => format!(
            "{{{}}}",
            fields.iter().map(|(k, v)| format!("{}:{}", quote(k), write(v))).collect::<Vec<_>>().join(",")
        ),
    }
}

/// A client: sends one request to the running Temino, returns its result
/// as JSON text.
#[cfg(unix)]
pub fn call(method: &str, params: &str) -> Result<String, String> {
    let path = socket_path();
    let stream = UnixStream::connect(&path).map_err(|e| format!("Temino isn't running ({path}: {e})"))?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    writeln!(writer, r#"{{"method":{},"params":{params}}}"#, quote(method)).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let answer = json::parse(&line).ok_or("Temino's answer isn't JSON")?;
    if let Some(error) = answer.str_at("error") {
        return Err(error.to_owned());
    }
    Ok(answer.get("result").map(write).unwrap_or_else(|| "null".to_owned()))
}

#[cfg(not(unix))]
pub fn call(_: &str, _: &str) -> Result<String, String> {
    Err("Temino's API needs Unix sockets".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_goes_back_out_as_it_came() {
        let text = r#"{"pane":3,"keys":["ctrl+c","Enter"],"wait":true,"timeout":2.5,"cwd":null,"text":"a \"b\"\n"}"#;
        assert_eq!(write(&json::parse(text).unwrap()), text);
    }

    #[test]
    fn request_lines_stop_at_the_cap() {
        // Lines come back without their endings, CRLF included, and a
        // last line with no ending still counts.
        let mut lines = BufReader::new(&b"one\r\ntwo\nthree"[..]);
        assert_eq!(request_line(&mut lines).as_deref(), Some("one"));
        assert_eq!(request_line(&mut lines).as_deref(), Some("two"));
        assert_eq!(request_line(&mut lines).as_deref(), Some("three"));
        assert_eq!(request_line(&mut lines), None);

        // Past the cap with no newline: no line, however much follows.
        let endless = vec![b'x'; MAX_LINE as usize + 100];
        let mut over = BufReader::new(&endless[..]);
        assert_eq!(request_line(&mut over), None);
    }

    #[test]
    fn requests_that_dont_parse_say_why() {
        assert_eq!(answer("nope"), Err("the request isn't JSON".to_owned()));
        assert_eq!(answer(r#"{"params":{}}"#), Err("the request has no method".to_owned()));
        assert_eq!(answer(r#"{"method":"focus","params":{"window":99}}"#), Err("there's no window 99".to_owned()));
    }
}

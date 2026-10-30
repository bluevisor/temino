//! Remote control: another device drives this Temino, every window and
//! tab of it. The iPhone app is the client to come; until then the page
//! this serves (`remote.html`) does the same from any browser.
//!
//! The host listens on `remote_bind` (default 127.0.0.1:7690) while
//! `remote=on` is in the settings file. A client opens a WebSocket and
//! must first say `{"type":"hello","token":…}` with `remote_token`; then
//! it gets the outline of every window (tabs, panes, what their agents
//! are doing) whenever it changes, with the screen of only the panes it
//! says it watches: the phone shows one pane, and zooms out to the tab,
//! the window, the machine. It sends what to do: switch tabs, focus
//! panes, type, press keys. Each window's core
//! thread builds its own state (`App::remote_state`) and carries out the
//! commands meant for it, as if they'd come from its own keyboard.
//!
//! The wire format is JSON text frames; docs/remote-control.md has it all,
//! and the plan for the relay, accounts and the iPhone app.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::json::{self, Value};
use crate::keys::Named;
use crate::platform::{mods, KeyEvent};
use crate::Msg;

pub const DEFAULT_BIND: &str = "127.0.0.1:7690";
/// How often a client's socket is checked for news, and for new state.
const POLL: Duration = Duration::from_millis(40);
/// The test page, served at / to whatever isn't a WebSocket.
const PAGE: &str = include_str!("remote.html");

/// What a remote asks a window to do.
#[derive(Debug)]
pub enum Command {
    /// Switch to the tab at this index.
    Tab(usize),
    /// Give this pane the keyboard (switching to its tab).
    Focus(u64),
    /// Type this into the pane, as an input method would.
    Text(u64, String),
    /// Press this key in the pane.
    Key(u64, KeyEvent),
    NewTab,
    CloseTab(usize),
    NewWindow,
    /// Build the state now: someone new is watching, or watches more.
    Refresh,
    /// A phone came through the relay with a key not approved here: ask
    /// (its device id, its name, the code both screens show).
    Ask { device: String, name: String, code: String },
    /// That phone's request is over: it got in, or it left.
    Settled(String),
}

/// One client of the host: the test page's socket, or a phone over the
/// relay. While it lives, windows build their state; dropping it leaves.
pub struct Client {
    id: u64,
    /// The hub's version this client last got the state of.
    sent: u64,
    /// The screen lines it has, by key: a hash of each one's JSON.
    lines: HashMap<String, u64>,
}

impl Client {
    pub fn join() -> Client {
        CLIENTS.fetch_add(1, Ordering::Relaxed);
        let id = {
            let mut hub = HUB.lock().unwrap();
            hub.next_client += 1;
            let id = hub.next_client;
            hub.watching.push((id, Vec::new()));
            id
        };
        refresh();
        Client { id, sent: u64::MAX, lines: HashMap::new() }
    }

    /// What a client hears first.
    pub fn hello() -> String {
        format!(
            r#"{{"type":"hello","device":{},"version":{}}}"#,
            quote(&crate::app::host_name().unwrap_or_default()),
            quote(env!("CARGO_PKG_VERSION"))
        )
    }

    /// A message from the client: what it watches, or what to do.
    pub fn handle(&self, message: &Value) {
        if message.str_at("type") == Some("watch") {
            watch(self.id, message);
        } else {
            command(message);
        }
    }

    /// Every window's state, if it changed since the client last got it.
    pub fn state(&mut self) -> Option<String> {
        let hub = HUB.lock().unwrap();
        if hub.version == self.sent {
            return None;
        }
        self.sent = hub.version;
        // Screen lines stand in the windows' JSON as LINE-key-LINE; each goes
        // out whole the first time (and when it changes), then as just its key.
        let mut kept = HashMap::new();
        let mut windows = Vec::new();
        for (_, _, state, lines) in hub.windows.iter().filter(|w| !w.2.is_empty()) {
            let mut out = String::with_capacity(state.len());
            for (i, piece) in state.split(LINE).enumerate() {
                if i % 2 == 0 {
                    out.push_str(piece);
                    continue;
                }
                let json = lines.get(piece).map_or("[]", String::as_str);
                let hash = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    json.hash(&mut h);
                    h.finish()
                };
                if self.lines.get(piece) == Some(&hash) {
                    out.push_str(&format!(r#"{{"k":{}}}"#, quote(piece)));
                } else {
                    out.push_str(&format!(r#"{{"k":{},"v":{json}}}"#, quote(piece)));
                }
                kept.insert(piece.to_owned(), hash);
            }
            windows.push(out);
        }
        self.lines = kept;
        Some(format!(r#"{{"type":"state","windows":[{}]}}"#, windows.join(",")))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        CLIENTS.fetch_sub(1, Ordering::Relaxed);
        HUB.lock().unwrap().watching.retain(|(id, _)| *id != self.id);
    }
}

/// Shows `text` briefly in the first window.
pub fn notice(text: &str) {
    let hub = HUB.lock().unwrap();
    if let Some((_, sender, _, _)) = hub.windows.iter().min_by_key(|w| w.0) {
        let _ = sender.send(Msg::Notice(text.to_owned()));
    }
}

/// Asks the first window to show `command` (an approval) to you.
pub fn ask(command: Command) {
    let hub = HUB.lock().unwrap();
    if let Some((_, sender, _, _)) = hub.windows.iter().min_by_key(|w| w.0) {
        let _ = sender.send(Msg::Remote(command));
    }
}

/// Marks a screen line's key in a window's state: never in JSON text,
/// where control characters are escaped.
pub const LINE: char = '\u{1}';

struct Hub {
    /// Each window: its number, its core, its last state as JSON, and the
    /// screen lines that state refers to by key.
    windows: Vec<(u32, Sender<Msg>, String, HashMap<String, String>)>,
    /// Bumped whenever any window's state changes.
    version: u64,
    /// Each client's watched panes, by (window, pane), keyed by client.
    watching: Vec<(u64, Vec<(u32, u64)>)>,
    next_client: u64,
}

static HUB: Mutex<Hub> = Mutex::new(Hub { windows: Vec::new(), version: 0, watching: Vec::new(), next_client: 0 });
/// Clients past their hello: while there are none, windows don't bother
/// building their state.
static CLIENTS: AtomicUsize = AtomicUsize::new(0);
static STARTED: OnceLock<Option<String>> = OnceLock::new();

/// A window's core joins: commands for window `no` go to `sender`.
pub fn register(no: u32, sender: Sender<Msg>) {
    let mut hub = HUB.lock().unwrap();
    hub.windows.retain(|w| w.0 != no);
    hub.windows.push((no, sender, String::new(), HashMap::new()));
    hub.version += 1;
}

pub fn unregister(no: u32) {
    let mut hub = HUB.lock().unwrap();
    hub.windows.retain(|w| w.0 != no);
    hub.version += 1;
}

/// Whether anyone is watching: windows publish their state only then.
pub fn watched() -> bool {
    CLIENTS.load(Ordering::Relaxed) > 0
}

/// Whether some client watches pane `pane` of window `no`: only those
/// panes' screens go out.
pub fn watching(no: u32, pane: u64) -> bool {
    HUB.lock().unwrap().watching.iter().any(|(_, panes)| panes.contains(&(no, pane)))
}

/// Window `no`'s state, as a JSON object; clients hear of it if it changed.
pub fn publish(no: u32, state: String, lines: HashMap<String, String>) {
    let mut hub = HUB.lock().unwrap();
    let Some(slot) = hub.windows.iter_mut().find(|w| w.0 == no) else { return };
    if slot.2 != state || slot.3 != lines {
        slot.2 = state;
        slot.3 = lines;
        hub.version += 1;
    }
}

/// Starts the host, once, if the settings file turns it on. Returns where
/// it listens.
pub fn start() -> Option<&'static str> {
    #[cfg(not(target_os = "haiku"))]
    crate::relay::start();
    STARTED
        .get_or_init(|| {
            if crate::setting("remote").as_deref() != Some("on") {
                return None;
            }
            let bind = crate::setting("remote_bind").unwrap_or_else(|| DEFAULT_BIND.to_owned());
            let listener = match TcpListener::bind(&bind) {
                Ok(listener) => listener,
                Err(error) => {
                    crate::trace(&format!("remote: can't listen on {bind}: {error}"));
                    return None;
                }
            };
            let Some(token) = token() else {
                crate::trace("remote: no randomness for a token; not listening");
                return None;
            };
            let _ = std::thread::Builder::new().name("remote".into()).spawn(move || {
                for stream in listener.incoming().flatten() {
                    let token = token.clone();
                    let _ =
                        std::thread::Builder::new().name("remote-client".into()).spawn(move || serve(stream, &token));
                }
            });
            Some(bind)
        })
        .as_deref()
}

/// The secret a client must know, made and saved the first time; None
/// (and no host) if the system's randomness can't be read.
pub fn token() -> Option<String> {
    if let Some(token) = crate::setting("remote_token").filter(|t| t.len() >= 16) {
        return Some(token);
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut bytes)).ok()?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    crate::save_setting("remote_token", &token);
    Some(token)
}

/// Whether `given` is the token, in the same time whatever it holds.
fn token_matches(given: &str, token: &str) -> bool {
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// A browser says where the page that opened the socket came from: only
/// our own page may (no other site, even one rebinding its name to us).
/// The app sends no Origin.
fn origin_allowed(request: &str) -> bool {
    let header =
        |name: &str| request.lines().find_map(|line| line.strip_prefix(name).map(|value| value.trim().to_owned()));
    match (header("origin:"), header("host:")) {
        (None, _) => true,
        (Some(origin), Some(host)) => origin == format!("http://{host}") || origin == format!("https://{host}"),
        (Some(_), None) => false,
    }
}

/// One connection: the test page over plain HTTP, or a client over a
/// WebSocket.
fn serve(stream: TcpStream, token: &str) {
    let mut head = [0u8; 2048];
    let Ok(n) = stream.peek(&mut head) else { return };
    let request = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    if !request.contains("upgrade: websocket") {
        return page(stream);
    }
    if !origin_allowed(&request) {
        return;
    }
    let Ok(mut socket) = tungstenite::accept(stream) else { return };
    let _ = socket.get_mut().set_read_timeout(Some(POLL));
    let mut client: Option<Client> = None;
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                let Some(message) = json::parse(text.as_str()) else { continue };
                let Some(client) = &client else {
                    if message.str_at("type") != Some("hello")
                        || !message.str_at("token").is_some_and(|given| token_matches(given, token))
                    {
                        let _ = socket.send(tungstenite::Message::text(r#"{"type":"error","error":"bad token"}"#));
                        let _ = socket.close(None);
                        return;
                    }
                    client = Some(Client::join());
                    if socket.send(tungstenite::Message::text(Client::hello())).is_err() {
                        break;
                    }
                    continue;
                };
                client.handle(&message);
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        let Some(client) = &mut client else { continue };
        if let Some(state) = client.state() {
            if socket.send(tungstenite::Message::text(state)).is_err() {
                break;
            }
        }
    }
}

/// `{"type":"watch","panes":[[window, pane], …]}`: the panes whose screens
/// this client wants, replacing what it watched before.
fn watch(client: u64, message: &Value) {
    let panes = message
        .get("panes")
        .map(Value::array)
        .unwrap_or_default()
        .iter()
        .filter_map(|pair| match pair.array() {
            [window, pane] => Some((window.num()? as u32, pane.num()? as u64)),
            _ => None,
        })
        .collect();
    let mut hub = HUB.lock().unwrap();
    if let Some(slot) = hub.watching.iter_mut().find(|(id, _)| *id == client) {
        slot.1 = panes;
    }
    drop(hub);
    refresh();
}

/// Wakes every window to build its state: a window at rest sleeps until
/// something happens.
fn refresh() {
    for (_, sender, _, _) in &HUB.lock().unwrap().windows {
        let _ = sender.send(Msg::Remote(Command::Refresh));
    }
}

fn page(mut stream: TcpStream) {
    // Read the request off the wire before answering it.
    let mut buffer = [0u8; 2048];
    let _ = stream.read(&mut buffer);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{PAGE}",
        PAGE.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

/// A client's message, sent on to the window it's for.
fn command(message: &Value) {
    let window = message.num_at("window").unwrap_or(1.0) as u32;
    let pane = message.num_at("pane").map(|p| p as u64);
    let index = message.num_at("index").map(|i| i as usize);
    let command = match (message.str_at("type"), pane) {
        (Some("tab"), _) => index.map(Command::Tab),
        (Some("focus"), Some(pane)) => Some(Command::Focus(pane)),
        (Some("text"), Some(pane)) => message.str_at("text").map(|text| Command::Text(pane, text.to_owned())),
        (Some("key"), Some(pane)) => key(message).map(|key| Command::Key(pane, key)),
        (Some("new_tab"), _) => Some(Command::NewTab),
        (Some("close_tab"), _) => index.map(Command::CloseTab),
        (Some("new_window"), _) => Some(Command::NewWindow),
        _ => None,
    };
    let Some(command) = command else { return };
    let hub = HUB.lock().unwrap();
    // A new window comes from any window; everything else from its own.
    let target = hub.windows.iter().find(|w| w.0 == window).or(hub.windows.first());
    if let Some((_, sender, _, _)) = target {
        let _ = sender.send(Msg::Remote(command));
    }
}

/// `{"key":"Enter"}`, `{"key":"c","mods":["ctrl"]}`: a key as the platform
/// would have sent it.
fn key(message: &Value) -> Option<KeyEvent> {
    let name = message.str_at("key")?;
    let mut bits = 0;
    for m in message.get("mods").map(Value::array).unwrap_or_default() {
        bits |= match m.str() {
            Some("shift") => mods::SHIFT,
            Some("ctrl") => mods::CONTROL,
            Some("alt" | "option") => mods::OPTION,
            Some("cmd") => mods::COMMAND,
            _ => 0,
        };
    }
    let named = match name {
        "Up" => Some(Named::Up),
        "Down" => Some(Named::Down),
        "Left" => Some(Named::Left),
        "Right" => Some(Named::Right),
        "Home" => Some(Named::Home),
        "End" => Some(Named::End),
        "PageUp" => Some(Named::PageUp),
        "PageDown" => Some(Named::PageDown),
        "Delete" => Some(Named::Delete),
        "Backspace" => Some(Named::Backspace),
        "Tab" => Some(Named::Tab),
        "Enter" => Some(Named::Enter),
        "Escape" => Some(Named::Escape),
        _ => None,
    };
    let bytes = match named {
        Some(Named::Enter) => vec![b'\r'],
        Some(Named::Tab) => vec![b'\t'],
        Some(Named::Escape) => vec![0x1b],
        Some(Named::Backspace) => vec![0x7f],
        Some(_) => Vec::new(),
        None => {
            let mut chars = name.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            if bits & mods::CONTROL != 0 && c.is_ascii_alphabetic() {
                vec![c.to_ascii_lowercase() as u8 & 0x1f]
            } else {
                c.to_string().into_bytes()
            }
        }
    };
    let raw_char = if named.is_some() { 0 } else { name.chars().next()? as u32 };
    Some(KeyEvent { named, mods: bits, raw_char, bytes })
}

/// `text` as a JSON string.
pub fn quote(text: &str) -> String {
    crate::llm::quote(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_read_like_the_keyboard_sent_them() {
        let enter = key(&json::parse(r#"{"key":"Enter"}"#).unwrap()).unwrap();
        assert_eq!((enter.named, enter.bytes), (Some(Named::Enter), vec![b'\r']));
        let interrupt = key(&json::parse(r#"{"key":"c","mods":["ctrl"]}"#).unwrap()).unwrap();
        assert_eq!((interrupt.named, interrupt.bytes, interrupt.mods), (None, vec![3], mods::CONTROL));
        let tab = key(&json::parse(r#"{"key":"t","mods":["cmd"]}"#).unwrap()).unwrap();
        assert_eq!((tab.raw_char, tab.mods), ('t' as u32, mods::COMMAND));
        assert!(key(&json::parse(r#"{"key":"nope"}"#).unwrap()).is_none());
    }

    #[test]
    fn only_our_own_page_opens_a_socket() {
        let request =
            |origin: &str| format!("get / http/1.1\r\nhost: 127.0.0.1:7690\r\n{origin}upgrade: websocket\r\n\r\n");
        assert!(origin_allowed(&request("")));
        assert!(origin_allowed(&request("origin: http://127.0.0.1:7690\r\n")));
        assert!(!origin_allowed(&request("origin: https://evil.example\r\n")));
        assert!(token_matches("abc", "abc") && !token_matches("abd", "abc") && !token_matches("ab", "abc"));
    }

    #[test]
    fn state_reaches_clients_only_when_it_changes() {
        let (sender, _receiver) = std::sync::mpsc::channel();
        register(91, sender);
        let version = |no| {
            publish(no, r#"{"no":91}"#.to_owned(), HashMap::new());
            HUB.lock().unwrap().version
        };
        let first = version(91);
        assert_eq!(version(91), first);
        unregister(91);
    }

    #[test]
    fn a_client_gets_each_screen_line_once() {
        let (sender, _receiver) = std::sync::mpsc::channel();
        register(92, sender);
        let state = |title: &str, line: &str| {
            let lines = HashMap::from([("92:1:g0".to_owned(), line.to_owned())]);
            publish(92, format!(r#"{{"no":92,"title":"{title}","lines":[{LINE}92:1:g0{LINE}]}}"#), lines);
        };
        let mut client = Client::join();
        state("a", r##"[["hi","#fff",null,0]]"##);
        let first = client.state().unwrap();
        assert!(first.contains(r##"{"k":"92:1:g0","v":[["hi","#fff",null,0]]}"##), "{first}");
        // Something else changed: the line goes as its key alone.
        state("b", r##"[["hi","#fff",null,0]]"##);
        let second = client.state().unwrap();
        assert!(second.contains(r#"{"k":"92:1:g0"}"#) && !second.contains(r#""v""#), "{second}");
        // The line changed: whole again.
        state("b", r##"[["ho","#fff",null,0]]"##);
        assert!(client.state().unwrap().contains(r#""v":[["ho""#));
        drop(client);
        unregister(92);
    }
}

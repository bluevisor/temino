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
}

struct Hub {
    /// Each window: its number, its core, and its last state as JSON.
    windows: Vec<(u32, Sender<Msg>, String)>,
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
    hub.windows.retain(|(n, _, _)| *n != no);
    hub.windows.push((no, sender, String::new()));
    hub.version += 1;
}

pub fn unregister(no: u32) {
    let mut hub = HUB.lock().unwrap();
    hub.windows.retain(|(n, _, _)| *n != no);
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
pub fn publish(no: u32, state: String) {
    let mut hub = HUB.lock().unwrap();
    let Some(slot) = hub.windows.iter_mut().find(|(n, _, _)| *n == no) else { return };
    if slot.2 != state {
        slot.2 = state;
        hub.version += 1;
    }
}

/// Starts the host, once, if the settings file turns it on. Returns where
/// it listens.
pub fn start() -> Option<&'static str> {
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
            let token = token();
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

/// The secret a client must know, made and saved the first time.
pub fn token() -> String {
    if let Some(token) = crate::setting("remote_token").filter(|t| t.len() >= 16) {
        return token;
    }
    let mut bytes = [0u8; 16];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = random.read_exact(&mut bytes);
    }
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    crate::save_setting("remote_token", &token);
    token
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
    let Ok(mut socket) = tungstenite::accept(stream) else { return };
    let _ = socket.get_mut().set_read_timeout(Some(POLL));
    let mut hello = false;
    let mut client = 0;
    let mut sent = u64::MAX;
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                let Some(message) = json::parse(text.as_str()) else { continue };
                if !hello {
                    if message.str_at("type") != Some("hello") || message.str_at("token") != Some(token) {
                        let _ = socket.send(tungstenite::Message::text(r#"{"type":"error","error":"bad token"}"#));
                        let _ = socket.close(None);
                        return;
                    }
                    hello = true;
                    CLIENTS.fetch_add(1, Ordering::Relaxed);
                    client = {
                        let mut hub = HUB.lock().unwrap();
                        hub.next_client += 1;
                        let id = hub.next_client;
                        hub.watching.push((id, Vec::new()));
                        id
                    };
                    refresh();
                    let hello = format!(
                        r#"{{"type":"hello","device":{},"version":{}}}"#,
                        quote(&crate::app::host_name().unwrap_or_default()),
                        quote(env!("CARGO_PKG_VERSION"))
                    );
                    if socket.send(tungstenite::Message::text(hello)).is_err() {
                        break;
                    }
                    continue;
                }
                if message.str_at("type") == Some("watch") {
                    watch(client, &message);
                } else {
                    command(&message);
                }
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        if !hello {
            continue;
        }
        let state = {
            let hub = HUB.lock().unwrap();
            (hub.version != sent).then(|| {
                sent = hub.version;
                let windows: Vec<&str> =
                    hub.windows.iter().map(|(_, _, s)| s.as_str()).filter(|s| !s.is_empty()).collect();
                format!(r#"{{"type":"state","windows":[{}]}}"#, windows.join(","))
            })
        };
        if let Some(state) = state {
            if socket.send(tungstenite::Message::text(state)).is_err() {
                break;
            }
        }
    }
    if hello {
        CLIENTS.fetch_sub(1, Ordering::Relaxed);
        HUB.lock().unwrap().watching.retain(|(id, _)| *id != client);
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
    for (_, sender, _) in &HUB.lock().unwrap().windows {
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
    let target = hub.windows.iter().find(|(n, _, _)| *n == window).or(hub.windows.first());
    if let Some((_, sender, _)) = target {
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
    fn state_reaches_clients_only_when_it_changes() {
        let (sender, _receiver) = std::sync::mpsc::channel();
        register(91, sender);
        let version = |no| {
            publish(no, r#"{"no":91}"#.to_owned());
            HUB.lock().unwrap().version
        };
        let first = version(91);
        assert_eq!(version(91), first);
        unregister(91);
    }
}

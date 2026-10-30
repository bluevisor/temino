//! The relay: how a phone reaches this machine from anywhere. Signed in
//! (Settings ▸ Remote Control), Temino keeps a WebSocket open to the relay
//! (relay/, a Cloudflare Worker), which joins it with your other devices.
//!
//! Everything between a phone and this machine is end to end encrypted
//! (e2e.rs): the relay passes Noise frames along and reads none of them.
//! A phone whose key isn't approved here gets only a request: Temino asks
//! you, showing the code the phone shows too, and only then lets it in.
//!
//! Settings: `relay` (the relay's address), `relay_token` and
//! `relay_device` (this machine's sign-in, from `sign_in`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::e2e::{self, Keys, Session};
use crate::json::{self, Value};
use crate::remote::{self, Client, Command};

/// Where the relay is, unless the settings file says otherwise.
pub const DEFAULT_RELAY: &str = "https://temino-relay.bluevisor.workers.dev";
const POLL: Duration = Duration::from_millis(40);
/// How long the browser has to come back from signing in.
const SIGN_IN_WAIT: Duration = Duration::from_secs(600);

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Your answers to phones asking in: device id, and whether to let it in.
static DECISIONS: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

pub fn relay_url() -> String {
    crate::setting("relay").unwrap_or_else(|| DEFAULT_RELAY.to_owned()).trim_end_matches('/').to_owned()
}

pub fn signed_in() -> bool {
    crate::setting("relay_token").is_some_and(|t| !t.is_empty())
}

/// Connects to the relay, once, if this machine is signed in.
pub fn start() {
    if !signed_in() || RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = std::thread::Builder::new().name("relay".into()).spawn(|| {
        let mut wait = Duration::from_secs(1);
        while signed_in() {
            let began = Instant::now();
            if let Err(error) = serve() {
                crate::trace(&format!("relay: {error}"));
            }
            // A connection that held for a while starts the backoff over.
            if began.elapsed() > Duration::from_secs(30) {
                wait = Duration::from_secs(1);
            }
            std::thread::sleep(wait);
            wait = (wait * 2).min(Duration::from_secs(60));
        }
        RUNNING.store(false, Ordering::Release);
    });
}

/// Your answer to a phone asking to control this machine.
pub fn decide(device: &str, allow: bool) {
    DECISIONS.lock().unwrap().push((device.to_owned(), allow));
}

/// Signs this machine in: the browser goes to the relay's sign-in page,
/// which sends it back here, to a port open just for that, with the token.
pub fn sign_in() {
    let _ = std::thread::Builder::new().name("relay-sign-in".into()).spawn(|| {
        if let Err(error) = sign_in_now() {
            remote::notice(&format!("Couldn't sign in: {error}"));
        }
    });
}

fn sign_in_now() -> Result<(), String> {
    let keys = Keys::load_or_create()?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = e2e::hex(&Keys::generate()?.public[..12]);
    let name = crate::app::host_name().unwrap_or_else(|| "Temino".to_owned());
    let url = format!(
        "{}/login?return={}&state={state}&pub={}&name={}&kind=desktop",
        relay_url(),
        encode(&format!("http://127.0.0.1:{port}/done")),
        e2e::hex(&keys.public),
        encode(&name),
    );
    crate::platform::open_url(&url);
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + SIGN_IN_WAIT;
    loop {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut head = [0u8; 4096];
        let n = stream.read(&mut head).unwrap_or(0);
        let request = String::from_utf8_lossy(&head[..n]);
        let target = request.split_whitespace().nth(1).unwrap_or_default();
        let Some(query) = target.strip_prefix("/done?") else {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        };
        let params: HashMap<&str, String> =
            query.split('&').filter_map(|pair| pair.split_once('=')).map(|(k, v)| (k, decode(v))).collect();
        let ok = params.get("state") == Some(&state) && params.get("token").is_some_and(|t| t.contains('.'));
        let body = if ok {
            "<h1>Signed in</h1><p>Temino can be controlled from your phone now. You can close this tab.</p>"
        } else {
            "<h1>Sign-in didn't match</h1><p>Start again from Temino's Settings.</p>"
        };
        let page = format!(
            "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>Temino</title><body style='font:16px -apple-system,system-ui;text-align:center;padding:15vh 16px'>{body}"
        );
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            )
            .as_bytes(),
        );
        if !ok {
            return Err("the sign-in didn't come back from where it went".into());
        }
        crate::save_setting("relay_token", &params["token"]);
        crate::save_setting("relay_device", params.get("device").map_or("", String::as_str));
        remote::notice("Signed in to Remote Control");
        start();
        return Ok(());
    }
}

/// Signs this machine out here; the relay forgets it on the next connect.
pub fn sign_out() {
    crate::save_setting("relay_token", "");
}

struct Device {
    name: String,
    kind: String,
    key: Vec<u8>,
}

/// A phone talking to this machine.
struct Peer {
    session: Session,
    /// It's approved here and in: it gets the state and its commands run.
    client: Option<Client>,
    /// Waiting on your answer.
    asked: bool,
}

/// One connection to the relay, until it drops.
fn serve() -> Result<(), String> {
    let token = crate::setting("relay_token").unwrap_or_default();
    let base = relay_url();
    let ws = base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let (mut socket, _) =
        tungstenite::connect(format!("{ws}/connect?token={}", encode(&token))).map_err(|e| e.to_string())?;
    match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(POLL)),
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => stream.get_mut().set_read_timeout(Some(POLL)),
        _ => Ok(()),
    }
    .map_err(|e| e.to_string())?;
    let keys = Keys::load_or_create()?;
    let mut devices: HashMap<String, Device> = HashMap::new();
    let mut peers: HashMap<String, Peer> = HashMap::new();
    let mut out: Vec<String> = Vec::new();
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                if let Some(message) = json::parse(text.as_str()) {
                    relay_message(&message, &keys, &mut devices, &mut peers, &mut out);
                }
            }
            Ok(tungstenite::Message::Close(frame)) => {
                // Signed out on another device: forget the token here too.
                if frame.is_some_and(|f| u16::from(f.code) == 4001) {
                    sign_out();
                }
                return Err("closed".into());
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.to_string()),
        }
        if !signed_in() {
            let _ = socket.close(None);
            return Ok(());
        }
        let decisions: Vec<(String, bool)> = std::mem::take(&mut *DECISIONS.lock().unwrap());
        for (id, allow) in decisions {
            let Some(peer) = peers.get_mut(&id) else { continue };
            peer.asked = false;
            if allow {
                if let (Some(key), Some(device)) = (peer.session.peer(), devices.get(&id)) {
                    e2e::approve(&key, &device.name);
                }
                let_in(&id, peer, &mut out)?;
            } else {
                seal(&id, peer, r#"{"type":"denied"}"#, &mut out)?;
                peers.remove(&id);
            }
        }
        for (id, peer) in &mut peers {
            if let Some(state) = peer.client.as_mut().and_then(Client::state) {
                seal(id, peer, &state, &mut out)?;
            }
        }
        for message in out.drain(..) {
            socket.send(tungstenite::Message::text(message)).map_err(|e| e.to_string())?;
        }
    }
}

fn relay_message(
    message: &Value,
    keys: &Keys,
    devices: &mut HashMap<String, Device>,
    peers: &mut HashMap<String, Peer>,
    out: &mut Vec<String>,
) {
    match message.str_at("type") {
        Some("devices") => {
            devices.clear();
            for d in message.get("devices").map(Value::array).unwrap_or_default() {
                let (Some(id), Some(key)) = (d.str_at("id"), d.str_at("pub").and_then(e2e::from_hex)) else { continue };
                devices.insert(
                    id.to_owned(),
                    Device {
                        name: d.str_at("name").unwrap_or("Device").to_owned(),
                        kind: d.str_at("kind").unwrap_or_default().to_owned(),
                        key,
                    },
                );
            }
            // Phones gone from the account are gone from here.
            peers.retain(|id, peer| {
                let keep = devices.contains_key(id);
                if !keep {
                    settle(id, peer);
                }
                keep
            });
        }
        Some("recv") => {
            let (Some(from), Some(data)) = (message.str_at("from"), message.str_at("data").and_then(from_base64)) else {
                return;
            };
            let Some(device) = devices.get(from).filter(|d| d.kind == "phone") else { return };
            let from = from.to_owned();
            if let Err(error) = frame(&from, device, message.str_at("kind") == Some("hs"), &data, keys, peers, out) {
                crate::trace(&format!("relay: {from}: {error}"));
                if let Some(mut peer) = peers.remove(&from) {
                    settle(&from, &mut peer);
                }
            }
        }
        _ => {}
    }
}

/// A frame from phone `from`: a handshake message, or a sealed one.
fn frame(
    from: &str,
    device: &Device,
    handshake: bool,
    data: &[u8],
    keys: &Keys,
    peers: &mut HashMap<String, Peer>,
    out: &mut Vec<String>,
) -> Result<(), String> {
    if handshake {
        // The handshake's first message starts a session over; its third
        // finishes the one under way.
        let midway = peers.get(from).is_some_and(|p| matches!(p.session, Session::Handshake(_)));
        if !midway {
            if let Some(old) = peers.get_mut(from) {
                settle(from, old);
            }
            peers.insert(from.to_owned(), Peer { session: Session::respond(keys)?, client: None, asked: false });
        }
        let peer = peers.get_mut(from).expect("just made");
        if let Some(reply) = peer.session.handshake(data)? {
            send(from, "hs", &reply, out);
        }
        let Some(code) = peer.session.code().map(str::to_owned) else { return Ok(()) };
        // The key it proved has to be the one the relay listed for it.
        if peer.session.peer().as_deref() != Some(device.key.as_slice()) {
            return Err("its key isn't the one on the account".into());
        }
        if e2e::approved(&device.key) {
            return let_in(from, peer, out);
        }
        seal(from, peer, &format!(r#"{{"type":"pending","code":"{code}"}}"#), out)?;
        peer.asked = true;
        remote::ask(Command::Ask { device: from.to_owned(), name: device.name.clone(), code });
        return Ok(());
    }
    let peer = peers.get_mut(from).ok_or("no session")?;
    if let Some(plain) = peer.session.open_frame(data)? {
        if let (Some(client), Some(message)) = (&peer.client, json::parse(&String::from_utf8_lossy(&plain))) {
            client.handle(&message);
        }
    }
    Ok(())
}

fn let_in(id: &str, peer: &mut Peer, out: &mut Vec<String>) -> Result<(), String> {
    settle(id, peer);
    peer.client = Some(Client::join());
    seal(id, peer, &Client::hello(), out)
}

/// Takes down the request card for a phone that no longer waits.
fn settle(id: &str, peer: &mut Peer) {
    if std::mem::take(&mut peer.asked) {
        remote::ask(Command::Settled(id.to_owned()));
    }
}

fn seal(to: &str, peer: &mut Peer, message: &str, out: &mut Vec<String>) -> Result<(), String> {
    for frame in peer.session.seal(message.as_bytes())? {
        send(to, "msg", &frame, out);
    }
    Ok(())
}

fn send(to: &str, kind: &str, data: &[u8], out: &mut Vec<String>) {
    out.push(format!(r#"{{"type":"send","to":{},"kind":"{kind}","data":"{}"}}"#, remote::quote(to), base64(data)));
}

/// `text` safe in a URL's query.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| Some(hex(*bytes.get(i + 1)?)? * 16 + hex(*bytes.get(i + 2)?)?))
            .flatten();
        match (escaped, bytes[i]) {
            (Some(b), _) => {
                out.push(b as u8);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, b) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn from_base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut n = 0u32;
    let mut bits = 0;
    for c in text.bytes().filter(|&c| c != b'=') {
        n = n << 6 | ALPHABET.iter().position(|&a| a == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            assert_eq!(from_base64(&base64(data)).unwrap(), data);
        }
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(b"fo"), "Zm8=");
    }

    #[test]
    fn query_values_round_trip() {
        let text = "Johns M5 MacBook Pro/é&=";
        assert_eq!(decode(&encode(text)), text);
        assert_eq!(decode("a%2Eb"), "a.b");
    }
}

//! End-to-end encryption between a remote (the phone) and this machine,
//! so the relay in between passes along only what it can't read.
//!
//! Every device has a long-term X25519 key: this one's lives in
//! `remote_key` in the settings folder, readable only by you. A session is
//! a Noise XX handshake (`Noise_XX_25519_AESGCM_SHA256`), the phone
//! starting it: both sides prove their long-term keys and agree on fresh
//! keys for this session alone, so a key stolen later can't read what was
//! sent before.
//!
//! The handshake proves which key is at the other end, not whose key it
//! is: the relay hands out device keys, and a lying relay could hand out
//! its own. So a phone is trusted only once you approve it here, after
//! checking that both screens show the same six digits (`code`). Approved
//! keys are kept in `remote_peers`.
//!
//! Six digits alone could be ground: a relay in the middle could try a
//! million keys of its own until both sessions' digits matched. So the
//! digits also come from two random values fixed in turn (commit, then
//! reveal, as Bluetooth's numeric comparison does): the phone sends only
//! the hash of its value and its long-term key in the first message, this
//! machine sends its value in the second, and the phone reveals its value
//! and key in the third, which must match the hash. Everything the digits
//! depend on is set before the other side's value is seen, so a relay
//! can't steer them: it gets one guess in a million, and you see each try.
//!
//! Messages bigger than a Noise frame go in pieces: each piece's first
//! byte says whether more follow.

use sha2::{Digest, Sha256};
use snow::{Builder, HandshakeState, TransportState};

pub const PATTERN: &str = "Noise_XX_25519_AESGCM_SHA256";
/// Bound into the handshake: a session for anything else won't complete.
pub const PROLOGUE: &[u8] = b"temino-remote-1";
/// Noise's largest message, less the tag and the more-to-come byte.
const PIECE: usize = 65535 - 16 - 1;

/// A device's long-term key pair.
#[derive(Clone)]
pub struct Keys {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl Keys {
    pub fn generate() -> Result<Keys, String> {
        let pair = builder().generate_keypair().map_err(|e| e.to_string())?;
        Ok(Keys { private: pair.private, public: pair.public })
    }

    /// This machine's key, made and saved the first time.
    pub fn load_or_create() -> Result<Keys, String> {
        let path = format!("{}/remote_key", crate::settings_dir());
        // The private key's line, then the public key's.
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut lines = text.lines().map(|line| from_hex(line.trim()).filter(|k| k.len() == 32));
            if let (Some(Some(private)), Some(Some(public))) = (lines.next(), lines.next()) {
                return Ok(Keys { private, public });
            }
        }
        let keys = Keys::generate()?;
        crate::create_private_dir(&crate::settings_dir());
        crate::write_private(&path, format!("{}\n{}\n", hex(&keys.private), hex(&keys.public)));
        Ok(keys)
    }
}

fn builder() -> Builder<'static> {
    Builder::new(PATTERN.parse().expect("a valid Noise pattern")).prologue(PROLOGUE)
}

/// One end of a session, from the first handshake message to the last
/// message sent.
pub enum Session {
    Handshake(Box<HandshakeState>, Commit),
    Open { transport: Box<TransportState>, code: String, partial: Vec<u8> },
    /// A handshake that failed partway: the phone starts over.
    Broken,
}

/// The values the code is drawn from, as the handshake fixes them.
pub struct Commit {
    /// This end's random value.
    mine: [u8; 32],
    /// The phone's hash of its value and its long-term key (this machine's
    /// end), until revealed.
    promise: Option<Vec<u8>>,
    /// The other end's value, once known.
    theirs: Option<Vec<u8>>,
    initiator: bool,
}

impl Commit {
    fn new(initiator: bool) -> Result<Commit, String> {
        let mut mine = [0u8; 32];
        getrandom::getrandom(&mut mine).map_err(|e| e.to_string())?;
        Ok(Commit { mine, promise: None, theirs: None, initiator })
    }

    /// The code: the handshake's hash, then the phone's value, then this
    /// machine's.
    fn code(&self, hash: &[u8]) -> Result<String, String> {
        let theirs = self.theirs.as_deref().ok_or("no value from the other end")?;
        let (phone, mac) = if self.initiator { (&self.mine[..], theirs) } else { (theirs, &self.mine[..]) };
        Ok(code(&Sha256::new().chain_update(hash).chain_update(phone).chain_update(mac).finalize()))
    }
}

impl Session {
    /// The phone's end (phones run noise.js, so here only for the tests):
    /// its first message goes out at once.
    #[cfg(test)]
    pub fn initiate(keys: &Keys) -> Result<(Session, Vec<u8>), String> {
        let mut state = builder().local_private_key(&keys.private).build_initiator().map_err(|e| e.to_string())?;
        let commit = Commit::new(true)?;
        let mut out = vec![0u8; 1024];
        let promise = Sha256::new().chain_update(commit.mine).chain_update(&keys.public).finalize();
        let n = state.write_message(&promise, &mut out).map_err(|e| e.to_string())?;
        out.truncate(n);
        Ok((Session::Handshake(Box::new(state), commit), out))
    }

    /// This machine's end, waiting for the phone's first message.
    pub fn respond(keys: &Keys) -> Result<Session, String> {
        let state = builder().local_private_key(&keys.private).build_responder().map_err(|e| e.to_string())?;
        Ok(Session::Handshake(Box::new(state), Commit::new(false)?))
    }

    /// Takes a handshake message; returns the reply to send, if any. Once
    /// the handshake is done the session is open.
    pub fn handshake(&mut self, message: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let Session::Handshake(state, commit) = self else { return Err("already open".into()) };
        let mut scratch = vec![0u8; 1024];
        let n = state.read_message(message, &mut scratch).map_err(|e| e.to_string())?;
        let payload = scratch[..n].to_vec();
        if payload.len() != 32 {
            return Err("handshake without its value".into());
        }
        // First message: the phone's promise; second: this machine's value;
        // third: the phone's value, which has to keep the promise.
        match (commit.initiator, commit.promise.take()) {
            (true, _) => commit.theirs = Some(payload),
            (false, None) => commit.promise = Some(payload),
            (false, Some(promise)) => {
                // Its value, and the key it just proved, as promised before
                // it saw this machine's value.
                let key = state.get_remote_static().ok_or("no key from the phone")?;
                if Sha256::new().chain_update(&payload).chain_update(key).finalize().as_slice() != promise.as_slice() {
                    return Err("the phone's value doesn't keep its promise".into());
                }
                commit.theirs = Some(payload);
            }
        }
        let reply = if state.is_handshake_finished() {
            None
        } else {
            let n = state.write_message(&commit.mine, &mut scratch).map_err(|e| e.to_string())?;
            Some(scratch[..n].to_vec())
        };
        if state.is_handshake_finished() {
            self.open()?;
        }
        Ok(reply)
    }

    fn open(&mut self) -> Result<(), String> {
        let Session::Handshake(state, commit) = std::mem::replace(self, Session::Broken) else {
            return Ok(());
        };
        let code = commit.code(state.get_handshake_hash())?;
        let transport = state.into_transport_mode().map_err(|e| e.to_string())?;
        *self = Session::Open { transport: Box::new(transport), code, partial: Vec::new() };
        Ok(())
    }

    /// The other device's long-term public key, once it's proven.
    pub fn peer(&self) -> Option<Vec<u8>> {
        match self {
            Session::Handshake(state, _) => state.get_remote_static().map(<[u8]>::to_vec),
            Session::Open { transport, .. } => transport.get_remote_static().map(<[u8]>::to_vec),
            Session::Broken => None,
        }
    }

    /// The six digits both screens show, once the session is open.
    pub fn code(&self) -> Option<&str> {
        match self {
            Session::Open { code, .. } => Some(code),
            Session::Handshake(..) | Session::Broken => None,
        }
    }

    /// `message` sealed, in as many frames as it takes.
    pub fn seal(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let Session::Open { transport, .. } = self else { return Err("not open".into()) };
        let pieces: Vec<&[u8]> = if message.is_empty() { vec![&[]] } else { message.chunks(PIECE).collect() };
        let last = pieces.len() - 1;
        let mut frames = Vec::with_capacity(pieces.len());
        for (i, piece) in pieces.into_iter().enumerate() {
            let mut plain = Vec::with_capacity(piece.len() + 1);
            plain.push(u8::from(i == last));
            plain.extend_from_slice(piece);
            let mut out = vec![0u8; plain.len() + 16];
            let n = transport.write_message(&plain, &mut out).map_err(|e| e.to_string())?;
            out.truncate(n);
            frames.push(out);
        }
        Ok(frames)
    }

    /// Opens a frame; a whole message once its last piece is in.
    pub fn open_frame(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let Session::Open { transport, partial, .. } = self else { return Err("not open".into()) };
        let mut plain = vec![0u8; frame.len()];
        let n = transport.read_message(frame, &mut plain).map_err(|e| e.to_string())?;
        let (&last, piece) = plain[..n].split_first().ok_or("empty frame")?;
        partial.extend_from_slice(piece);
        Ok((last == 1).then(|| std::mem::take(partial)))
    }
}

/// Six digits from a hash.
fn code(hash: &[u8]) -> String {
    let n = u32::from_be_bytes([hash[0], hash[1], hash[2], hash[3]]) % 1_000_000;
    format!("{n:06}")
}

/// The phones approved to control this machine: hex key, then its name.
pub fn approved(key: &[u8]) -> bool {
    let key = hex(key);
    peers().iter().any(|(k, _)| *k == key)
}

pub fn approve(key: &[u8], name: &str) {
    let mut peers = peers();
    let key = hex(key);
    peers.retain(|(k, _)| *k != key);
    peers.push((key, name.replace(['\t', '\n'], " ")));
    let text: String = peers.iter().map(|(k, n)| format!("{k}\t{n}\n")).collect();
    crate::write_private(&format!("{}/remote_peers", crate::settings_dir()), text);
}

fn peers() -> Vec<(String, String)> {
    std::fs::read_to_string(format!("{}/remote_peers", crate::settings_dir()))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, name) = line.split_once('\t').unwrap_or((line, ""));
            (key.len() == 64).then(|| (key.to_owned(), name.to_owned()))
        })
        .collect()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Session, Session) {
        let (phone_keys, mac_keys) = (Keys::generate().unwrap(), Keys::generate().unwrap());
        let (mut phone, first) = Session::initiate(&phone_keys).unwrap();
        let mut mac = Session::respond(&mac_keys).unwrap();
        let second = mac.handshake(&first).unwrap().unwrap();
        let third = phone.handshake(&second).unwrap().unwrap();
        assert!(mac.handshake(&third).unwrap().is_none());
        assert_eq!(mac.peer().unwrap(), phone_keys.public);
        assert_eq!(phone.peer().unwrap(), mac_keys.public);
        (phone, mac)
    }

    #[test]
    fn both_ends_agree_and_show_the_same_code() {
        let (mut phone, mut mac) = pair();
        assert_eq!(phone.code().unwrap(), mac.code().unwrap());
        let frames = phone.seal(br#"{"type":"tab","index":1}"#).unwrap();
        assert_eq!(mac.open_frame(&frames[0]).unwrap().unwrap(), br#"{"type":"tab","index":1}"#);
    }

    #[test]
    fn big_messages_go_in_pieces() {
        let (mut phone, mut mac) = pair();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let frames = mac.seal(&big).unwrap();
        assert_eq!(frames.len(), 4);
        let mut whole = None;
        for frame in frames {
            whole = phone.open_frame(&frame).unwrap();
        }
        assert_eq!(whole.unwrap(), big);
    }

    #[test]
    fn a_phone_that_breaks_its_promise_is_refused() {
        let (phone_keys, mac_keys) = (Keys::generate().unwrap(), Keys::generate().unwrap());
        let (mut phone, first) = Session::initiate(&phone_keys).unwrap();
        let mut mac = Session::respond(&mac_keys).unwrap();
        let second = mac.handshake(&first).unwrap().unwrap();
        // Choosing its value after seeing the Mac's: what a relay in the
        // middle would need to steer the digits.
        if let Session::Handshake(_, commit) = &mut phone {
            commit.mine[0] ^= 1;
        }
        let third = phone.handshake(&second).unwrap().unwrap();
        assert!(mac.handshake(&third).is_err());
    }

    #[test]
    fn a_phone_that_swaps_its_key_is_refused() {
        // A relay posing as the phone would pick its key after seeing the
        // Mac's value, to steer the digits: the promise covers the key, so
        // the same value with another key than promised is turned away.
        let (promised, used, mac_keys) = (Keys::generate().unwrap(), Keys::generate().unwrap(), Keys::generate().unwrap());
        let mine = [7u8; 32];
        let promise = Sha256::new().chain_update(mine).chain_update(&promised.public).finalize();
        let mut phone = builder().local_private_key(&used.private).build_initiator().unwrap();
        let (mut out, mut scratch) = ([0u8; 1024], [0u8; 1024]);
        let n = phone.write_message(&promise, &mut out).unwrap();
        let mut mac = Session::respond(&mac_keys).unwrap();
        let second = mac.handshake(&out[..n]).unwrap().unwrap();
        phone.read_message(&second, &mut scratch).unwrap();
        let n = phone.write_message(&mine, &mut out).unwrap();
        assert!(mac.handshake(&out[..n]).is_err());
    }

    #[test]
    fn a_tampered_frame_is_refused() {
        let (mut phone, mut mac) = pair();
        let mut frame = phone.seal(b"ls").unwrap().remove(0);
        frame[3] ^= 1;
        assert!(mac.open_frame(&frame).is_err());
    }

    /// The web client's Noise (relay/public/noise.js, under Node) against
    /// this one: needs node on PATH, so it's run on its own.
    #[test]
    #[ignore]
    fn interop_with_the_web_client() {
        use std::io::{BufRead, BufReader, Write};
        let mut node = std::process::Command::new("node")
            .arg("relay/test/noise-peer.mjs")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("node");
        let mut to = node.stdin.take().unwrap();
        let mut from = BufReader::new(node.stdout.take().unwrap()).lines();
        let mut line = || from.next().unwrap().unwrap();
        let keys = Keys::generate().unwrap();
        let mut mac = Session::respond(&keys).unwrap();
        let second = mac.handshake(&from_hex(&line()).unwrap()).unwrap().unwrap();
        writeln!(to, "{}", hex(&second)).unwrap();
        assert!(mac.handshake(&from_hex(&line()).unwrap()).unwrap().is_none());
        assert_eq!(line(), mac.code().unwrap());
        assert_eq!(line(), hex(&keys.public));
        assert_eq!(from_hex(&line()).unwrap(), mac.peer().unwrap());
        for message in [b"hello".to_vec(), (0..150_000u32).map(|i| (i % 253) as u8).collect()] {
            let frames = mac.seal(&message).unwrap();
            writeln!(to, "{}", frames.len()).unwrap();
            for frame in frames {
                writeln!(to, "{}", hex(&frame)).unwrap();
            }
            let count: usize = line().parse().unwrap();
            let mut back = None;
            for _ in 0..count {
                back = mac.open_frame(&from_hex(&line()).unwrap()).unwrap();
            }
            assert_eq!(back.unwrap(), message.iter().rev().copied().collect::<Vec<u8>>());
        }
        let frames = mac.seal(b"").unwrap();
        writeln!(to, "{}\n{}", frames.len(), hex(&frames[0])).unwrap();
        drop(to);
        assert!(node.wait().unwrap().success());
    }

    #[test]
    fn keys_round_trip_through_hex() {
        let keys = Keys::generate().unwrap();
        assert_eq!(from_hex(&hex(&keys.public)).unwrap(), keys.public);
    }
}

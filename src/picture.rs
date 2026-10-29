//! Inline images: iTerm2's OSC 1337 `File=` (imgcat) and the Kitty
//! graphics protocol (APC `G`), decoded to RGBA and placed on the cells
//! under the cursor of the block that printed them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Decoded, premultiplied RGBA.
pub struct Picture {
    /// Unique for the life of the process: what the atlas caches it by.
    pub id: u64,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Picture {
    pub fn new(width: usize, height: usize, mut rgba: Vec<u8>) -> Option<Arc<Picture>> {
        if width == 0 || height == 0 || rgba.len() < width * height * 4 {
            return None;
        }
        rgba.truncate(width * height * 4);
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u32;
            for c in &mut px[..3] {
                *c = (*c as u32 * a / 255) as u8;
            }
        }
        Some(Arc::new(Picture { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), width, height, rgba }))
    }

    /// PNG, JPEG or GIF (its first frame).
    pub fn decode(bytes: &[u8]) -> Option<Arc<Picture>> {
        let image = image::load_from_memory(bytes).ok()?.into_rgba8();
        let (w, h) = image.dimensions();
        Picture::new(w as usize, h as usize, image.into_raw())
    }

    /// Scaled to `w` x `h` (box filter down, nearest up): premultiplied RGBA.
    pub fn scaled(&self, w: usize, h: usize) -> Vec<u8> {
        let mut out = vec![0u8; w * h * 4];
        for y in 0..h {
            let y0 = y * self.height / h;
            let y1 = ((y + 1) * self.height / h).max(y0 + 1).min(self.height);
            for x in 0..w {
                let x0 = x * self.width / w;
                let x1 = ((x + 1) * self.width / w).max(x0 + 1).min(self.width);
                let mut sum = [0u32; 4];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let i = (sy * self.width + sx) * 4;
                        for (total, &value) in sum.iter_mut().zip(&self.rgba[i..i + 4]) {
                            *total += value as u32;
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u32;
                let o = (y * w + x) * 4;
                for c in 0..4 {
                    out[o + c] = (sum[c] / n) as u8;
                }
            }
        }
        out
    }
}

/// How big the program asked for the image to be.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Size {
    /// In cells; None: from the image's own size.
    pub columns: Option<usize>,
    pub rows: Option<usize>,
}

/// An image to put at the cursor.
pub struct Request {
    pub picture: Arc<Picture>,
    pub size: Size,
}

/// What the scanner hands on: an image to show, and a reply for the
/// program (Kitty's acknowledgements and queries).
#[derive(Default)]
pub struct Outcome {
    pub show: Option<Request>,
    pub reply: Option<Vec<u8>>,
}

/// iTerm2's `1337;File=name=…;inline=1;width=…:<base64>`.
pub fn iterm(body: &str) -> Outcome {
    let Some(rest) = body.strip_prefix("File=") else { return Outcome::default() };
    let Some((args, data)) = rest.split_once(':') else { return Outcome::default() };
    let mut inline = false;
    let mut size = Size::default();
    for arg in args.split(';') {
        let Some((key, value)) = arg.split_once('=') else { continue };
        // Cells as a bare number; pixels and percentages go by the image.
        let cells = || value.parse::<usize>().ok().filter(|&n| n > 0);
        match key {
            "inline" => inline = value == "1",
            "width" => size.columns = cells(),
            "height" => size.rows = cells(),
            _ => {}
        }
    }
    if !inline {
        return Outcome::default();
    }
    let show = base64(data).and_then(|bytes| Picture::decode(&bytes)).map(|picture| Request { picture, size });
    Outcome { show, reply: None }
}

/// Kitty graphics: images transmitted (in chunks) and put by id.
#[derive(Default)]
pub struct Kitty {
    /// The chunks of an image still arriving, and its first chunk's keys.
    pending: Option<(HashMap<char, String>, Vec<u8>)>,
    stored: HashMap<u32, Arc<Picture>>,
}

impl Kitty {
    /// One APC `G…` command (the body after the `G`).
    pub fn command(&mut self, body: &str) -> Outcome {
        let (control, payload) = body.split_once(';').unwrap_or((body, ""));
        let mut keys: HashMap<char, String> = control
            .split(',')
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                Some((k.chars().next()?, v.to_owned()))
            })
            .collect();
        let more = keys.get(&'m').is_some_and(|m| m == "1");
        // A continuation carries only m= (and maybe q=): the first chunk's keys rule.
        let mut data = match self.pending.take() {
            Some((first, data)) => {
                let quiet = keys.remove(&'q');
                keys = first;
                if let Some(quiet) = quiet {
                    keys.insert('q', quiet);
                }
                data
            }
            None => Vec::new(),
        };
        data.extend_from_slice(payload.trim().as_bytes());
        if more {
            self.pending = Some((keys, data));
            return Outcome::default();
        }
        let get = |k: char| keys.get(&k).map(String::as_str);
        let num = |k: char| get(k).and_then(|v| v.parse::<usize>().ok());
        let id = num('i').unwrap_or(0) as u32;
        let quiet = num('q').unwrap_or(0);
        let reply = |message: &str| {
            let ok = message == "OK";
            if id == 0 || quiet >= 2 || (ok && quiet >= 1) {
                None
            } else {
                Some(format!("\x1b_Gi={id};{message}\x1b\\").into_bytes())
            }
        };
        let size = Size { columns: num('c').filter(|&n| n > 0), rows: num('r').filter(|&n| n > 0) };
        match get('a').unwrap_or("t") {
            "q" => {
                let ok = decode_kitty(&keys, &data).is_some();
                Outcome { show: None, reply: reply(if ok { "OK" } else { "EINVAL:bad image" }) }
            }
            action @ ("t" | "T") => match decode_kitty(&keys, &data) {
                Some(picture) => {
                    if id != 0 {
                        self.stored.insert(id, picture.clone());
                    }
                    let show = (action == "T").then_some(Request { picture, size });
                    Outcome { show, reply: reply("OK") }
                }
                None => Outcome { show: None, reply: reply("EINVAL:can't decode the image") },
            },
            "p" => match self.stored.get(&id) {
                Some(picture) => Outcome { show: Some(Request { picture: picture.clone(), size }), reply: reply("OK") },
                None => Outcome { show: None, reply: reply("ENOENT:no such image") },
            },
            "d" => {
                if matches!(get('d'), Some("I" | "i")) {
                    self.stored.remove(&id);
                }
                Outcome::default()
            }
            _ => Outcome::default(),
        }
    }
}

fn decode_kitty(keys: &HashMap<char, String>, data: &[u8]) -> Option<Arc<Picture>> {
    if keys.get(&'o').is_some() || keys.get(&'t').is_some_and(|t| t != "d") {
        // Compressed data, or files and shared memory: not supported.
        return None;
    }
    let bytes = base64(std::str::from_utf8(data).ok()?)?;
    let num = |k: char| keys.get(&k).and_then(|v| v.parse::<usize>().ok());
    match num('f').unwrap_or(32) {
        100 => Picture::decode(&bytes),
        24 => {
            let (w, h) = (num('s')?, num('v')?);
            let rgba = bytes.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            Picture::new(w, h, rgba)
        }
        32 => Picture::new(num('s')?, num('v')?, bytes),
        _ => None,
    }
}

/// Standard base64, padding optional, whitespace ignored.
pub fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        buffer = buffer << 6 | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8").unwrap(), b"hello");
        assert!(base64("a*b").is_none());
    }

    #[test]
    fn kitty_raw_images_arrive_in_chunks() {
        let mut kitty = Kitty::default();
        // A 1x2 RGB image: red, green, sent in two chunks.
        let data = "/wAAAP8A";
        assert!(kitty.command(&format!("a=T,f=24,s=1,v=2,i=7,m=1;{}", &data[..4])).show.is_none());
        let outcome = kitty.command(&format!("m=0;{}", &data[4..]));
        let shown = outcome.show.unwrap();
        assert_eq!((shown.picture.width, shown.picture.height), (1, 2));
        assert_eq!(&shown.picture.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(outcome.reply.unwrap(), b"\x1b_Gi=7;OK\x1b\\");
        assert!(kitty.command("a=p,i=7,q=1").show.is_some());
    }

    #[test]
    fn iterm_needs_inline() {
        assert!(iterm("File=name=eA==;inline=0:AAAA").show.is_none());
    }

    #[test]
    fn scaling_averages() {
        let picture = Picture::new(2, 1, vec![255, 255, 255, 255, 0, 0, 0, 255]).unwrap();
        assert_eq!(picture.scaled(1, 1), vec![127, 127, 127, 255]);
    }
}

use super::*;

impl Scanner {
    pub(super) const MAX_OSC: usize = 4096;
    /// Inline images come base64 in one sequence.
    pub(super) const MAX_IMAGE: usize = 64 << 20;

    pub(super) fn scan(&mut self, input: &[u8]) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut data = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let byte = input[i];
            if let Some(osc) = &mut self.osc {
                // Inside an OSC: wait for BEL or ST (ESC \).
                if byte == 0x07 || (self.pending_escape && byte == b'\\') {
                    let body = std::mem::take(osc);
                    self.osc = None;
                    self.pending_escape = false;
                    let parsed = if self.kind == b']' { parse_osc(&body) } else { parse_apc(&body) };
                    match parsed {
                        Some(segment) => {
                            if !data.is_empty() {
                                out.push(Segment::Data(std::mem::take(&mut data)));
                            }
                            out.push(segment);
                        }
                        None => {
                            data.extend_from_slice(&[0x1b, self.kind]);
                            data.extend_from_slice(&body);
                            data.extend_from_slice(if byte == 0x07 { b"\x07" } else { b"\x1b\\" });
                        }
                    }
                } else if self.pending_escape {
                    // ESC inside an OSC that isn't ST: abandon the OSC.
                    let body = std::mem::take(osc);
                    self.osc = None;
                    self.pending_escape = false;
                    data.extend_from_slice(&[0x1b, self.kind]);
                    data.extend_from_slice(&body);
                    data.push(0x1b);
                    continue; // reprocess this byte normally
                } else if byte == 0x1b {
                    self.pending_escape = true;
                } else {
                    osc.push(byte);
                    let image = osc.starts_with(b"1337;File=") || osc.starts_with(b"G");
                    if osc.len() > if image { Self::MAX_IMAGE } else { Self::MAX_OSC } {
                        let body = std::mem::take(osc);
                        self.osc = None;
                        data.extend_from_slice(&[0x1b, self.kind]);
                        data.extend_from_slice(&body);
                    }
                }
                i += 1;
                continue;
            }
            if self.pending_escape {
                self.pending_escape = false;
                if byte == b']' || byte == b'_' {
                    self.osc = Some(Vec::new());
                    self.kind = byte;
                    i += 1;
                    continue;
                }
                data.push(0x1b);
                continue; // reprocess this byte
            }
            match input[i..].iter().position(|&b| b == 0x1b) {
                Some(0) => {
                    self.pending_escape = true;
                    i += 1;
                }
                Some(n) => {
                    data.extend_from_slice(&input[i..i + n]);
                    i += n;
                }
                None => {
                    data.extend_from_slice(&input[i..]);
                    i = input.len();
                }
            }
        }
        if !data.is_empty() {
            out.push(Segment::Data(data));
        }
        out
    }
}

/// An APC: Kitty graphics commands start with `G`.
pub(super) fn parse_apc(body: &[u8]) -> Option<Segment> {
    let text = std::str::from_utf8(body.strip_prefix(b"G")?).ok()?;
    Some(Segment::Kitty(text.to_owned()))
}

pub(super) fn parse_osc(body: &[u8]) -> Option<Segment> {
    let text = std::str::from_utf8(body).ok()?;
    let (number, rest) = text.split_once(';').unwrap_or((text, ""));
    match number {
        "133" => {
            let mut parts = rest.split(';');
            match parts.next()? {
                "A" => Some(Segment::PromptStart),
                "B" => Some(Segment::PromptEnd),
                "C" => Some(Segment::OutputStart),
                "D" => Some(Segment::Done(parts.next().and_then(|code| code.parse().ok()))),
                _ => None,
            }
        }
        "7" => {
            let url = rest.strip_prefix("file://")?;
            let path = &url[url.find('/')?..];
            Some(Segment::Cwd(percent_decode(path)))
        }
        "6973" if rest == "cont" => Some(Segment::Continuation),
        "1337" if rest.starts_with("File=") => Some(Segment::Iterm(rest.to_owned())),
        "6973" => rest.strip_prefix("path;").map(|path| Segment::Path(path.to_owned())),
        "9" => match rest.strip_prefix("4;") {
            Some(progress) => Some(Segment::Progress(!matches!(progress.split(';').next(), Some("0" | "")))),
            None => Some(Segment::Notify),
        },
        "777" if rest.starts_with("notify;") => Some(Segment::Notify),
        "99" => Some(Segment::Notify),
        _ => None,
    }
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What escape-stripped output leaves on screen, line by line: a bare
/// carriage return rewinds the line (a line editor's redraw), \r\n ends it.
pub(super) fn visible_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = vec![String::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let line = lines.last_mut().unwrap();
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => line.clear(),
            '\n' => lines.push(String::new()),
            '\x08' => {
                line.pop();
            }
            c if c.is_control() => {}
            c => line.push(c),
        }
    }
    lines
}

/// The last non-blank visible line (a typed-ahead command's echo).
pub(super) fn last_visible_line(bytes: &[u8]) -> String {
    visible_lines(bytes).into_iter().rev().map(|l| l.trim().to_owned()).find(|l| !l.is_empty()).unwrap_or_default()
}

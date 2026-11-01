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
            None => Some(Segment::Notify(None, rest.to_owned())),
        },
        // OSC 777;notify;title;body (urxvt, Ghostty, WezTerm).
        "777" => {
            let mut parts = rest.strip_prefix("notify;")?.splitn(2, ';');
            let title = parts.next().unwrap_or_default();
            Some(Segment::Notify(
                Some(title.to_owned()).filter(|t| !t.is_empty()),
                parts.next().unwrap_or_default().to_owned(),
            ))
        }
        // Kitty's OSC 99;metadata;payload: p=title makes the payload the title.
        "99" => {
            let (meta, payload) = rest.split_once(';').unwrap_or(("", rest));
            if meta.split(':').any(|kv| kv == "p=title") {
                Some(Segment::Notify(Some(payload.to_owned()), String::new()))
            } else {
                Some(Segment::Notify(None, payload.to_owned()))
            }
        }
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

/// What escape-stripped output leaves on screen, line by line. The
/// screen is a cursor, not an eraser: a bare carriage return and a
/// backspace only move it, so what follows overwrites (a line editor's
/// redraw); \r\n ends the line, and ESC [ K wipes from the cursor on.
pub(super) fn visible_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = Vec::new();
    let mut line: Vec<char> = Vec::new();
    let mut cursor = 0usize;
    // Trailing blanks are not visible, however the line got them.
    let finish = |line: &Vec<char>| line.iter().collect::<String>().trim_end().to_owned();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut final_byte = None;
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            final_byte = Some(n);
                            break;
                        }
                        params.push(n);
                    }
                    if final_byte == Some('K') {
                        match params.as_str() {
                            // From the cursor to the end, or the whole line.
                            "" | "0" | "2" => {
                                line.truncate(if params == "2" { 0 } else { cursor });
                                if params == "2" {
                                    cursor = 0;
                                }
                            }
                            // The start through the cursor becomes blanks.
                            "1" => line.iter_mut().take(cursor + 1).for_each(|c| *c = ' '),
                            _ => {}
                        }
                    }
                }
                // OSC, DCS, SOS, PM and APC strings run to BEL or ESC \.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                // ESC ( B, ESC # 8 and the like: middle bytes, then one final.
                Some(' '..='/') => {
                    while chars.next_if(|n| (' '..='/').contains(n)).is_some() {}
                    chars.next();
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => cursor = 0,
            '\n' => {
                lines.push(finish(&line));
                line.clear();
                cursor = 0;
            }
            '\x08' => cursor = cursor.saturating_sub(1),
            c if c.is_control() => {}
            c => {
                if cursor == line.len() {
                    line.push(c);
                } else {
                    line[cursor] = c;
                }
                cursor += 1;
            }
        }
    }
    lines.push(finish(&line));
    lines
}

/// The last non-blank visible line (a typed-ahead command's echo).
pub(super) fn last_visible_line(bytes: &[u8]) -> String {
    visible_lines(bytes).into_iter().rev().map(|l| l.trim().to_owned()).find(|l| !l.is_empty()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Segments as readable strings, data escaped, for comparing.
    fn describe(segments: &[Segment]) -> Vec<String> {
        segments
            .iter()
            .map(|segment| match segment {
                Segment::Data(bytes) => format!("data {}", bytes.escape_ascii()),
                Segment::PromptStart => "prompt".into(),
                Segment::PromptEnd => "prompt end".into(),
                Segment::OutputStart => "output".into(),
                Segment::Done(code) => format!("done {code:?}"),
                Segment::Cwd(path) => format!("cwd {path}"),
                Segment::Continuation => "continuation".into(),
                Segment::Notify(title, body) => format!("notify {title:?} {body}"),
                Segment::Progress(busy) => format!("progress {busy}"),
                Segment::Path(path) => format!("path {path}"),
                Segment::Iterm(body) => format!("iterm {body}"),
                Segment::Kitty(body) => format!("kitty {body}"),
            })
            .collect()
    }

    fn scan(input: &[u8]) -> Vec<String> {
        describe(&Scanner::default().scan(input))
    }

    /// Feeds `chunks` to one scanner, as separate pty reads.
    fn scan_chunks(chunks: &[&[u8]]) -> Vec<String> {
        let mut scanner = Scanner::default();
        chunks.iter().flat_map(|chunk| describe(&scanner.scan(chunk))).collect()
    }

    /// Every data byte, joined: what reaches the terminal grid.
    fn data(segments: &[Segment]) -> Vec<u8> {
        segments
            .iter()
            .flat_map(|segment| match segment {
                Segment::Data(bytes) => bytes.clone(),
                _ => Vec::new(),
            })
            .collect()
    }

    #[test]
    fn plain_output_passes_through_untouched() {
        assert_eq!(scan(b"hello\r\nworld"), ["data hello\\r\\nworld"]);
        assert_eq!(scan(b"\x1b[31mred\x1b[0m"), ["data \\x1b[31mred\\x1b[0m"]);
        assert_eq!(scan(b""), Vec::<String>::new());
    }

    #[test]
    fn shell_marks_are_pulled_out_in_order() {
        assert_eq!(
            scan(b"a\x1b]133;A\x07$ \x1b]133;B\x1b\\ls\x1b]133;C\x07out\x1b]133;D;0\x07"),
            ["data a", "prompt", "data $ ", "prompt end", "data ls", "output", "data out", "done Some(0)"]
        );
    }

    #[test]
    fn command_done_carries_the_exit_code_when_there_is_one() {
        assert_eq!(scan(b"\x1b]133;D;127\x07"), ["done Some(127)"]);
        assert_eq!(scan(b"\x1b]133;D\x07"), ["done None"]);
        assert_eq!(scan(b"\x1b]133;D;oops\x07"), ["done None"]);
        // Extra prompt-mark options (fish's click_events) don't matter.
        assert_eq!(scan(b"\x1b]133;A;click_events=1\x1b\\"), ["prompt"]);
    }

    #[test]
    fn sequences_split_across_reads_are_put_back_together() {
        let whole: &[u8] = b"x\x1b]133;D;2\x1b\\y";
        let expected = scan(whole);
        assert_eq!(expected, ["data x", "done Some(2)", "data y"]);
        // Every split point, including between ESC and `]` and inside ST.
        for at in 0..=whole.len() {
            let (a, b) = whole.split_at(at);
            let mut got = scan_chunks(&[a, b]);
            // Data on either side of the split arrives as two segments.
            got.dedup_by(|b, a| {
                if let (Some(x), Some(y)) = (a.strip_prefix("data "), b.strip_prefix("data ")) {
                    *a = format!("data {x}{y}");
                    true
                } else {
                    false
                }
            });
            assert_eq!(got, expected, "split at {at}");
        }
        // One byte at a time.
        let bytes: Vec<&[u8]> = whole.chunks(1).collect();
        assert!(scan_chunks(&bytes).contains(&"done Some(2)".to_owned()));
    }

    #[test]
    fn an_unfinished_sequence_waits_for_the_next_read() {
        let mut scanner = Scanner::default();
        assert!(describe(&scanner.scan(b"\x1b]133;")).is_empty());
        assert_eq!(describe(&scanner.scan(b"A\x07")), ["prompt"]);
        // A lone ESC at the end of a read is held, then sent with what follows.
        assert_eq!(describe(&scanner.scan(b"abc\x1b")), ["data abc"]);
        assert_eq!(describe(&scanner.scan(b"[A")), ["data \\x1b[A"]);
    }

    #[test]
    fn unknown_sequences_reach_the_terminal_byte_for_byte() {
        for input in [
            &b"\x1b]0;my title\x07"[..],
            b"\x1b]2;title\x1b\\",
            b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07",
            b"\x1b]133;Z\x07",
            b"\x1b]1337;SetMark\x07",
            b"\x1b]777;other;x\x07",
            b"\x1b]7;http://host/path\x07",
            b"\x1b]6973;else\x07",
            b"\x1b_Xnot kitty\x1b\\",
        ] {
            let segments = Scanner::default().scan(input);
            assert_eq!(segments.len(), 1, "{}", input.escape_ascii());
            assert_eq!(data(&segments), input, "{}", input.escape_ascii());
        }
    }

    #[test]
    fn an_escape_inside_an_osc_abandons_it_without_losing_bytes() {
        let input = b"\x1b]133;A\x1b[31mred";
        let segments = Scanner::default().scan(input);
        assert_eq!(describe(&segments), ["data \\x1b]133;A\\x1b[31mred"]);
        // And the scanner is back to normal afterwards.
        let mut scanner = Scanner::default();
        scanner.scan(input);
        assert_eq!(describe(&scanner.scan(b"\x1b]133;B\x07")), ["prompt end"]);
    }

    #[test]
    fn an_endless_osc_is_given_up_and_passed_through() {
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'x', Scanner::MAX_OSC + 100));
        input.extend_from_slice(b"\x07after");
        let mut scanner = Scanner::default();
        let segments = scanner.scan(&input);
        assert_eq!(data(&segments), input);
        assert_eq!(describe(&scanner.scan(b"\x1b]133;C\x07")), ["output"]);
    }

    #[test]
    fn non_utf8_passes_through_and_never_parses_as_a_mark() {
        assert_eq!(data(&Scanner::default().scan(b"\xff\xfe ok \xc3")), b"\xff\xfe ok \xc3");
        let input = b"\x1b]133;\xffA\x07";
        let segments = Scanner::default().scan(input);
        assert_eq!(data(&segments), input);
        assert_eq!(scan(b"\x1b]7;file://h/caf\xc3\xa9\x07"), ["cwd /café"]);
    }

    #[test]
    fn working_directory_comes_from_osc_7() {
        assert_eq!(scan(b"\x1b]7;file://mac.local/Users/me/My%20Dir\x07"), ["cwd /Users/me/My Dir"]);
        assert_eq!(scan(b"\x1b]7;file:///tmp\x1b\\"), ["cwd /tmp"]);
        assert_eq!(scan(b"\x1b]7;file://host/%E4%B8%AD\x07"), ["cwd /中"]);
    }

    #[test]
    fn temino_marks_notifications_and_progress() {
        assert_eq!(scan(b"\x1b]6973;cont\x07"), ["continuation"]);
        assert_eq!(scan(b"\x1b]6973;path;/usr/bin:/bin\x07"), ["path /usr/bin:/bin"]);
        assert_eq!(scan(b"\x1b]9;Build done\x07"), ["notify None Build done"]);
        assert_eq!(scan(b"\x1b]777;notify;Title;Body\x07"), ["notify Some(\"Title\") Body"]);
        assert_eq!(scan(b"\x1b]99;i=1;hello\x1b\\"), ["notify None hello"]);
        assert_eq!(scan(b"\x1b]9;4;1;50\x07"), ["progress true"]);
        assert_eq!(scan(b"\x1b]9;4;3;\x07"), ["progress true"]);
        assert_eq!(scan(b"\x1b]9;4;0;\x07"), ["progress false"]);
        assert_eq!(scan(b"\x1b]9;4;\x07"), ["progress false"]);
    }

    #[test]
    fn inline_images_are_recognised_and_may_be_large() {
        assert_eq!(scan(b"\x1b]1337;File=inline=1:AAAA\x07"), ["iterm File=inline=1:AAAA"]);
        assert_eq!(scan(b"\x1b_Gf=100,a=T;AAAA\x1b\\"), ["kitty f=100,a=T;AAAA"]);
        let mut big = b"\x1b]1337;File=inline=1:".to_vec();
        big.extend(std::iter::repeat_n(b'A', Scanner::MAX_OSC * 4));
        big.push(0x07);
        let segments = Scanner::default().scan(&big);
        assert_eq!(segments.len(), 1);
        assert!(
            matches!(&segments[0], Segment::Iterm(body) if body.len() == "File=inline=1:".len() + Scanner::MAX_OSC * 4)
        );
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b"), "/a b");
        assert_eq!(percent_decode("%e4%b8%ad%E6%96%87"), "中文");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(percent_decode("%41"), "A");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
        assert_eq!(percent_decode(""), "");
    }

    #[test]
    fn visible_lines_strip_escapes_and_apply_carriage_returns() {
        assert_eq!(visible_lines(b"one\r\ntwo"), ["one", "two"]);
        assert_eq!(visible_lines(b"one\ntwo\n"), ["one", "two", ""]);
        // A bare CR is a line editor redrawing the line.
        assert_eq!(visible_lines(b"$ l\r$ ls"), ["$ ls"]);
        assert_eq!(visible_lines(b"lss\x08 \x08"), ["ls"]);
        assert_eq!(visible_lines(b"\x1b[1;32mgreen\x1b[0m \x1b[?2004hx"), ["green x"]);
        assert_eq!(visible_lines(b"\x1b]0;title\x07a\x1b]133;A\x1b\\b"), ["ab"]);
        assert_eq!(visible_lines(b"\x07bell\x00"), ["bell"]);
        assert_eq!(visible_lines("中文 ✓".as_bytes()), ["中文 ✓"]);
        assert_eq!(visible_lines(b"bad \xff byte"), ["bad \u{fffd} byte"]);
        // Truncated escapes don't run off the end.
        assert_eq!(visible_lines(b"x\x1b["), ["x"]);
        assert_eq!(visible_lines(b"x\x1b]0;unterminated"), ["x"]);
        assert_eq!(visible_lines(b"x\x1b"), ["x"]);
    }

    #[test]
    fn carriage_returns_and_backspaces_overwrite_rather_than_delete() {
        // A shorter redraw over a longer line leaves the tail on screen.
        assert_eq!(visible_lines(b"abcdef\rxy"), ["xycdef"]);
        // Two backspaces put the next letter over the first one.
        assert_eq!(visible_lines(b"ab\x08\x08c"), ["cb"]);
        // What a prompt redraw really sends: back to the start, erase to
        // the end of the line, then the new text.
        assert_eq!(visible_lines(b"abcdef\r\x1b[Kxy"), ["xy"]);
        assert_eq!(visible_lines(b"abcdef\r\x1b[2Kxy"), ["xy"]);
        // Erasing the start leaves the rest, past the blanks (after a
        // carriage return only the first cell is under the cursor).
        assert_eq!(last_visible_line(b"abcdef\r\x1b[1K"), "bcdef");
        assert_eq!(last_visible_line(b"abcdef\x08\x08\x1b[1K"), "f");
    }

    #[test]
    fn charset_selection_is_not_visible() {
        assert_eq!(visible_lines(b"\x1b(B\x1b[mls"), ["ls"]);
        assert_eq!(visible_lines(b"a\x1b#8b\x1bPq#0;2\x1b\\c\x1b7d"), ["abcd"]);
    }

    #[test]
    fn the_last_visible_line_skips_blank_ones() {
        assert_eq!(last_visible_line(b"ls -la\r\n\r\n  \r\n"), "ls -la");
        assert_eq!(last_visible_line(b"first\r\n  second  "), "second");
        assert_eq!(last_visible_line(b"\x1b[K\r\n"), "");
        assert_eq!(last_visible_line(b""), "");
        assert_eq!(last_visible_line(b"$ gti\x08\x08it\r\n"), "$ git");
    }
}

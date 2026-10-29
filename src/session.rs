//! The shell session: the pty and its threads, the shell-integration
//! markers (OSC 133 prompt/command/output/done, OSC 7 cwd), and the block
//! list. Like Warp, every command gets its own terminal grid, so output
//! never interleaves with prompts and each block can be styled, selected
//! and copied on its own.

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::shells::Kind;
use crate::theme::Theme;
use crate::Msg;

const BASH_INTEGRATION: &str = include_str!("shell/temino.bash");
const ZSH_ENV: &str = include_str!("shell/zshenv");
const ZSH_INTEGRATION: &str = include_str!("shell/temino.zsh");
const FISH_INTEGRATION: &str = include_str!("shell/temino.fish");
const HISTORY_LINES: usize = 20_000;
/// How long a PS2 continuation prompt must sit unanswered before the
/// editor comes back for the rest of the command.
const CONTINUATION_DELAY: Duration = Duration::from_millis(120);
/// Output that shows up while the prompt is idle waits this long for a
/// command start (typed-ahead input echoes) before it becomes a
/// background-output block.
const IDLE_OUTPUT_DELAY: Duration = Duration::from_millis(150);

#[derive(Clone, Copy)]
pub struct GridSize {
    pub columns: usize,
    pub lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone)]
pub struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// The shell is starting; no prompt yet.
    Boot,
    /// Between the end of a command and the prompt.
    Prompt,
    /// At the prompt: the editor owns the keyboard.
    Ready,
    /// A command was sent and its output hasn't started.
    Submitted,
    /// The shell asked for more input (PS2); the editor is back.
    Continuation,
    /// A command is running: keys go to the program.
    Running,
    /// The shell exited.
    Exited,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    Command,
    /// Output printed while the prompt was idle (background jobs).
    Background,
    /// A request to the model in AI mode, and its answer.
    Ai,
}

pub struct Block {
    pub id: u64,
    pub kind: BlockKind,
    pub command: String,
    pub cwd: String,
    pub branch: Option<String>,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub exit: Option<i32>,
    pub term: Term<Listener>,
    parser: Processor,
    /// The last character printed, for `Graphemes`.
    last: Option<char>,
    lines: Option<usize>,
    size: GridSize,
    /// `clear`: runs like any command, then wipes the block list.
    pub clears: bool,
    /// Counts changes to the output, for searches to notice them.
    pub generation: u64,
    /// Images printed into the output, on the lines they cover.
    pub pictures: Vec<Placement>,
}

/// An image in a block's output: from output line `line`, column
/// `column`, `columns` x `rows` cells (the image keeps its aspect within).
pub struct Placement {
    pub picture: std::sync::Arc<crate::picture::Picture>,
    pub line: usize,
    pub column: usize,
    pub columns: usize,
    pub rows: usize,
}

impl Block {
    fn new(
        id: u64,
        kind: BlockKind,
        command: String,
        cwd: String,
        branch: Option<String>,
        size: GridSize,
        listener: Listener,
    ) -> Block {
        let config = Config { scrolling_history: HISTORY_LINES, ..Config::default() };
        let clears = kind == BlockKind::Command && matches!(command.trim(), "clear" | "reset" | "cls");
        Block {
            id,
            kind,
            command,
            cwd,
            branch,
            started: Instant::now(),
            finished: None,
            exit: None,
            term: Term::new(config, &size, listener),
            parser: Processor::new(),
            last: None,
            lines: None,
            size,
            clears,
            generation: 0,
            pictures: Vec::new(),
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        let mut handler = crate::graphemes::Graphemes { term: &mut self.term, last: &mut self.last };
        self.parser.advance(&mut handler, bytes);
        self.lines = None;
        self.generation += 1;
    }

    pub fn running(&self) -> bool {
        self.finished.is_none()
    }

    pub fn failed(&self) -> bool {
        matches!(self.exit, Some(code) if code != 0)
    }

    pub fn duration(&self) -> Duration {
        self.finished.unwrap_or_else(Instant::now).duration_since(self.started)
    }

    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Lines of output worth showing: scrollback plus the screen down to
    /// its last non-blank line (or the cursor, while running).
    pub fn output_lines(&mut self) -> usize {
        if let Some(lines) = self.lines {
            return lines;
        }
        let grid = self.term.grid();
        let screen = grid.screen_lines();
        let cursor = grid.cursor.point;
        let mut bottom: i32 = -1;
        for line in (0..screen).rev() {
            let row = &grid[Line(line as i32)];
            let blank = (0..grid.columns()).all(|c| {
                let cell = &row[Column(c)];
                (cell.c == ' ' || cell.c == '\t')
                    && cell.bg
                        == alacritty_terminal::vte::ansi::Color::Named(
                            alacritty_terminal::vte::ansi::NamedColor::Background,
                        )
                    && !cell.flags.intersects(Flags::INVERSE | Flags::ALL_UNDERLINES)
            });
            if !blank {
                bottom = line as i32;
                break;
            }
        }
        if self.running() && (cursor.column.0 > 0 || bottom >= 0) {
            bottom = bottom.max(cursor.line.0);
        }
        let pictures = self.pictures.iter().map(|p| p.line + p.rows).max().unwrap_or(0);
        let lines = (grid.history_size() + (bottom + 1).max(0) as usize).max(pictures);
        self.lines = Some(lines);
        lines
    }

    /// Puts an image at the cursor, sized to the program's request or else
    /// its own pixels (one per logical pixel, no wider than the screen),
    /// and moves the cursor below it, as iTerm2 and Kitty do.
    pub fn place_picture(&mut self, request: crate::picture::Request, cell: (f32, f32)) {
        if self.alt_screen() {
            return;
        }
        let picture = request.picture;
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let room = grid.columns().saturating_sub(point.column.0).max(1);
        let aspect = picture.height as f32 / picture.width as f32;
        let (columns, rows) = match (request.size.columns, request.size.rows) {
            (Some(c), Some(r)) => (c.min(room), r),
            (Some(c), None) => {
                let c = c.min(room);
                (c, ((c as f32 * cell.0 * aspect) / cell.1).ceil() as usize)
            }
            (None, Some(r)) => {
                let c = ((r as f32 * cell.1 / aspect) / cell.0).ceil() as usize;
                (c.clamp(1, room), r)
            }
            (None, None) => {
                let c = ((picture.width as f32 / cell.0).ceil() as usize).clamp(1, room);
                (c, ((c as f32 * cell.0 * aspect) / cell.1).ceil() as usize)
            }
        };
        let rows = rows.clamp(1, 1000);
        let line = (grid.history_size() as i32 + point.line.0) as usize;
        self.pictures.push(Placement { picture, line, column: point.column.0, columns, rows });
        // The image's rows, then the cursor on the line under it.
        self.feed(&b"\r\n".repeat(rows));
    }

    /// The last `limit` lines of output as text with SGR colors, to be fed
    /// back in after a relaunch.
    pub fn output_ansi(&mut self, limit: usize) -> Vec<u8> {
        use alacritty_terminal::vte::ansi::{Color as C, NamedColor};
        let lines = self.output_lines();
        let grid = self.term.grid();
        let columns = grid.columns();
        let sgr = |color: C, foreground: bool| -> String {
            let base = if foreground { 30 } else { 40 };
            match color {
                C::Named(n) if (n as usize) < 8 => format!("{}", base + n as usize),
                C::Named(n) if (n as usize) < 16 => format!("{}", base + 60 + n as usize - 8),
                C::Named(NamedColor::Foreground | NamedColor::Background) | C::Named(_) => format!("{}", base + 9),
                C::Indexed(i) => format!("{};5;{i}", base + 8),
                C::Spec(rgb) => format!("{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b),
            }
        };
        let mut out = Vec::new();
        for index in lines.saturating_sub(limit)..lines {
            let row = &grid[self.grid_line(index)];
            let end = (0..columns)
                .rposition(|c| row[Column(c)].c != ' ' || row[Column(c)].bg != C::Named(NamedColor::Background));
            let mut style = String::new();
            for column in 0..end.map_or(0, |e| e + 1) {
                let cell = &row[Column(column)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut codes = vec!["0".to_owned(), sgr(cell.fg, true), sgr(cell.bg, false)];
                for (flag, code) in [
                    (Flags::BOLD, "1"),
                    (Flags::DIM, "2"),
                    (Flags::ITALIC, "3"),
                    (Flags::UNDERLINE, "4"),
                    (Flags::INVERSE, "7"),
                    (Flags::STRIKEOUT, "9"),
                ] {
                    if cell.flags.contains(flag) {
                        codes.push(code.to_owned());
                    }
                }
                let next = codes.join(";");
                if next != style {
                    out.extend_from_slice(format!("\x1b[{next}m").as_bytes());
                    style = next;
                }
                let c = if cell.c == '\t' { ' ' } else { cell.c };
                out.extend_from_slice(c.to_string().as_bytes());
                for &mark in cell.zerowidth().unwrap_or(&[]) {
                    out.extend_from_slice(mark.to_string().as_bytes());
                }
            }
            out.extend_from_slice(b"\x1b[0m");
            if index + 1 < lines {
                out.extend_from_slice(b"\r\n");
            }
        }
        out
    }

    /// Grid line for output line `index` (0 = oldest scrollback line).
    pub fn grid_line(&self, index: usize) -> Line {
        Line(index as i32 - self.term.grid().history_size() as i32)
    }

    /// Text between two (line, column) points of this block's output.
    pub fn text_between(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let start = Point::new(self.grid_line(start.0), Column(start.1));
        let end = Point::new(self.grid_line(end.0), Column(end.1.min(self.size.columns.saturating_sub(1))));
        let text = self.term.bounds_to_string(start, end);
        text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
    }

    /// The link under output cell (`line`, `column`): an OSC 8 hyperlink,
    /// or a URL in the text, followed across soft-wrapped lines.
    pub fn link_at(&self, line: usize, column: usize) -> Option<Link> {
        let grid = self.term.grid();
        let columns = grid.columns();
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32 - 1;
        let target = self.grid_line(line);
        if target.0 < top || target.0 > bottom || column >= columns {
            return None;
        }
        let output_line = |l: i32| (l - top) as usize;
        let mut first = target.0;
        let mut last = target.0;
        if let Some(link) = grid[target][Column(column)].hyperlink() {
            // Every cell carrying the same link, in reading order.
            let same = |l: i32, c: usize| grid[Line(l)][Column(c)].hyperlink().as_ref() == Some(&link);
            let (mut l0, mut c0) = (target.0, column);
            loop {
                let previous = if c0 > 0 { (l0, c0 - 1) } else { (l0 - 1, columns - 1) };
                if previous.0 < top || !same(previous.0, previous.1) {
                    break;
                }
                (l0, c0) = previous;
            }
            let (mut l1, mut c1) = (target.0, column);
            loop {
                let next = if c1 + 1 < columns { (l1, c1 + 1) } else { (l1 + 1, 0) };
                if next.0 > bottom || !same(next.0, next.1) {
                    break;
                }
                (l1, c1) = next;
            }
            let cells = (l0..=l1)
                .map(|l| {
                    let from = if l == l0 { c0 } else { 0 };
                    let to = if l == l1 { c1 } else { columns - 1 };
                    (output_line(l), from, to)
                })
                .collect();
            return Some(Link { url: link.uri().to_owned(), cells });
        }
        let wraps = |l: i32| grid[Line(l)][Column(columns - 1)].flags.contains(Flags::WRAPLINE);
        while first > top && wraps(first - 1) {
            first -= 1;
        }
        while last < bottom && wraps(last) {
            last += 1;
        }
        let mut chars = Vec::new();
        let mut at = Vec::new();
        for l in first..=last {
            for c in 0..columns {
                let cell = &grid[Line(l)][Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                chars.push(cell.c);
                at.push((l, c));
            }
        }
        let index = at.iter().position(|&(l, c)| l == target.0 && c == column)?;
        let url = find_urls(&chars).into_iter().find(|(start, end)| (*start..*end).contains(&index));
        let (start, end, url) = match url {
            Some((start, end)) => (start, end, chars[start..end].iter().collect()),
            None => {
                // A file or folder that exists, named absolutely, from ~,
                // or from where the command ran.
                let (start, end) = find_path(&chars, index)?;
                let name: String = chars[start..end].iter().collect();
                let path = if let Some(rest) = name.strip_prefix("~/") {
                    format!("{}/{rest}", std::env::var("HOME").ok()?)
                } else if name.starts_with('/') {
                    name
                } else {
                    format!("{}/{name}", self.cwd)
                };
                if !std::path::Path::new(&path).exists() {
                    return None;
                }
                (start, end, path)
            }
        };
        let mut cells: Vec<(usize, usize, usize)> = Vec::new();
        for &(l, c) in &at[start..end] {
            match cells.last_mut() {
                Some(run) if run.0 == output_line(l) => run.2 = c,
                _ => cells.push((output_line(l), c, c)),
            }
        }
        Some(Link { url, cells })
    }

    pub fn output_text(&mut self) -> String {
        let lines = self.output_lines();
        if lines == 0 {
            return String::new();
        }
        let text = self.text_between((0, 0), (lines - 1, self.size.columns));
        text.trim_end_matches('\n').to_owned()
    }

    fn resize(&mut self, size: GridSize) {
        if self.size.columns != size.columns || self.size.lines != size.lines {
            self.term.resize(size);
            self.size = size;
            self.lines = None;
            self.generation += 1;
        }
    }
}

/// What the scanner found in the pty stream.
enum Segment {
    Data(Vec<u8>),
    PromptStart,
    PromptEnd,
    OutputStart,
    Done(Option<i32>),
    Cwd(String),
    Continuation,
    /// A desktop notification (OSC 9, 777 or 99): the program wants you.
    Notify,
    /// A progress report (OSC 9;4): busy or not.
    Progress(bool),
    /// The shell's $PATH, reported at each prompt.
    Path(String),
    /// An inline image: iTerm2's OSC 1337 File= (its body), or a Kitty
    /// graphics command (the APC body after `G`).
    Iterm(String),
    Kitty(String),
}

/// Pulls Temino's own OSC sequences out of the byte stream; everything else
/// passes through untouched, in order. Sequences may span reads.
#[derive(Default)]
struct Scanner {
    pending_escape: bool,
    /// An OSC (`]`) or APC (`_`) being collected, and which.
    osc: Option<Vec<u8>>,
    kind: u8,
}

impl Scanner {
    const MAX_OSC: usize = 4096;
    /// Inline images come base64 in one sequence.
    const MAX_IMAGE: usize = 64 << 20;

    fn scan(&mut self, input: &[u8]) -> Vec<Segment> {
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
fn parse_apc(body: &[u8]) -> Option<Segment> {
    let text = std::str::from_utf8(body.strip_prefix(b"G")?).ok()?;
    Some(Segment::Kitty(text.to_owned()))
}

fn parse_osc(body: &[u8]) -> Option<Segment> {
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
fn visible_lines(bytes: &[u8]) -> Vec<String> {
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
fn last_visible_line(bytes: &[u8]) -> String {
    visible_lines(bytes).into_iter().rev().map(|l| l.trim().to_owned()).find(|l| !l.is_empty()).unwrap_or_default()
}

/// Backpressure between the pty reader and the core thread, so a flood of
/// output (`yes`) can't queue unbounded memory.
pub struct Credits {
    outstanding: Mutex<usize>,
    ready: Condvar,
}

impl Credits {
    const LIMIT: usize = 8;

    pub fn new() -> Arc<Credits> {
        Arc::new(Credits { outstanding: Mutex::new(0), ready: Condvar::new() })
    }

    fn acquire(&self) {
        let mut outstanding = self.outstanding.lock().unwrap();
        while *outstanding >= Self::LIMIT {
            outstanding = self.ready.wait(outstanding).unwrap();
        }
        *outstanding += 1;
    }

    pub fn release(&self) {
        let mut outstanding = self.outstanding.lock().unwrap();
        *outstanding = outstanding.saturating_sub(1);
        self.ready.notify_one();
    }
}

struct Shell {
    pid: i32,
    master: i32,
    writer: Sender<Vec<u8>>,
}

/// Something the UI should react to.
pub enum Notice {
    Title(String),
    Clipboard(String),
    Bell,
    Notify,
    Progress(bool),
    /// A command finished and wants the block list cleared (`clear`).
    Clear,
    Finished,
}

pub struct Session {
    shell: Option<Shell>,
    pub blocks: Vec<Block>,
    pub phase: Phase,
    pub cwd: String,
    pub branch: Option<String>,
    pub size: GridSize,
    cell: (u16, u16),
    cell_width: f32,
    cell_height: f32,
    scanner: Scanner,
    /// Kitty graphics: images stored by id, and chunks still arriving.
    kitty: crate::picture::Kitty,
    events: Rc<RefCell<Vec<Event>>>,
    next_id: u64,
    /// Bytes seen at the idle prompt, held back until we know whether a
    /// command starts (then they were its echo) or not (background output).
    idle_output: Vec<u8>,
    idle_since: Option<Instant>,
    continuation_since: Option<Instant>,
    pub exit_code: Option<i32>,
    pub boot_output: Vec<u8>,
    /// When a running program last printed, and when we last wrote to it.
    pub output_at: Option<Instant>,
    input_at: Cell<Option<Instant>>,
    /// The shell's own $PATH, which a Dock-launched Temino's lacks.
    pub path: Option<String>,
    /// The shell's file name: "zsh".
    pub shell_name: String,
}

impl Session {
    pub fn new(size: GridSize) -> Session {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into());
        Session {
            shell: None,
            blocks: Vec::new(),
            phase: Phase::Boot,
            cwd: home.clone(),
            branch: git_branch(&home),
            size,
            cell: (8, 16),
            cell_width: 8.0,
            cell_height: 16.0,
            scanner: Scanner::default(),
            kitty: Default::default(),
            events: Rc::new(RefCell::new(Vec::new())),
            next_id: 1,
            idle_output: Vec::new(),
            idle_since: None,
            continuation_since: None,
            exit_code: None,
            boot_output: Vec::new(),
            output_at: None,
            input_at: Cell::new(None),
            path: None,
            shell_name: String::new(),
        }
    }

    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        self.cell = (width as u16, height as u16);
        (self.cell_width, self.cell_height) = (width, height);
    }

    /// Starts `shell`; its output arrives as `Msg::Pty(id, ...)`.
    pub fn start(&mut self, id: u64, shell: &str, sender: Sender<Msg>, credits: Arc<Credits>) -> std::io::Result<()> {
        self.shell_name = shell.rsplit('/').next().unwrap_or(shell).to_owned();
        let mut env: Vec<String> = std::env::vars()
            .filter(|(key, _)| {
                !matches!(key.as_str(), "TERM" | "COLORTERM" | "TERM_PROGRAM" | "COLUMNS" | "LINES" | "ZDOTDIR")
            })
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let argv = integrate(shell, &mut env)?;
        env.push("TERM=xterm-256color".into());
        env.push("COLORTERM=truecolor".into());
        env.push("TERM_PROGRAM=Temino".into());
        if cfg!(target_os = "macos") {
            // The system bash's "default shell is now zsh" notice.
            env.push("BASH_SILENCE_DEPRECATION_WARNING=1".into());
        }
        env.push(format!("TERM_PROGRAM_VERSION={}", env!("CARGO_PKG_VERSION")));
        if let Some(lang) = utf8_lang(&env) {
            env.push(format!("LANG={lang}"));
        }

        let (pid, master) =
            crate::platform::spawn(&argv, &env, &self.cwd, self.size.columns as u16, self.size.lines as u16)?;
        let reader = unsafe { File::from_raw_fd(master) };
        let mut writer_file = reader.try_clone()?;

        let data_sender = sender.clone();
        thread::Builder::new().name("temino pty reader".into()).spawn(move || {
            let mut reader = reader;
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        credits.acquire();
                        if data_sender.send(Msg::Pty(id, buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            // Keep the master open (the File drops here) until the reader
            // is done; Haiku sends no SIGHUP, the exit status comes below.
        })?;

        let (writer, queue) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new().name("temino pty writer".into()).spawn(move || {
            for bytes in queue {
                if writer_file.write_all(&bytes).is_err() {
                    break;
                }
            }
        })?;

        thread::Builder::new().name("temino shell waiter".into()).spawn(move || {
            let code = crate::platform::wait(pid);
            let _ = sender.send(Msg::ShellExited(id, code));
        })?;

        self.shell = Some(Shell { pid, master, writer });
        self.phase = Phase::Boot;
        self.exit_code = None;
        Ok(())
    }

    pub fn hangup(&mut self) {
        if let Some(shell) = &self.shell {
            crate::platform::hangup(shell.pid);
        }
    }

    pub fn shell_exited(&mut self, code: i32) {
        self.exit_code = Some(code);
        self.phase = Phase::Exited;
        self.shell = None;
        let now = Instant::now();
        for block in &mut self.blocks {
            if block.finished.is_none() {
                block.finished = Some(now);
            }
        }
    }

    /// Sends what you typed (or did) to the program.
    pub fn write(&self, bytes: &[u8]) {
        self.input_at.set(Some(Instant::now()));
        self.reply(bytes);
    }

    /// Sends the terminal's own answers and reports, which aren't you acting.
    pub fn reply(&self, bytes: &[u8]) {
        if let Some(shell) = &self.shell {
            let _ = shell.writer.send(bytes.to_vec());
        }
    }

    /// The process group running in front: the command's, while one runs.
    pub fn foreground_group(&self) -> Option<i32> {
        let group = crate::platform::foreground_group(self.shell.as_ref()?.master);
        (group > 0).then_some(group)
    }

    pub fn input_at(&self) -> Option<Instant> {
        self.input_at.get()
    }

    fn new_block(&mut self, kind: BlockKind, command: String) -> &mut Block {
        let block = Block::new(
            self.next_id,
            kind,
            command,
            self.cwd.clone(),
            self.branch.clone(),
            self.size,
            Listener(self.events.clone()),
        );
        self.next_id += 1;
        self.blocks.push(block);
        self.blocks.last_mut().unwrap()
    }

    /// Writes the finished blocks down, to bring them back after a relaunch:
    /// a header line per block, then its output.
    pub fn save_blocks(&mut self, path: &str) {
        const BLOCKS: usize = 50;
        const LINES: usize = 2000;
        let mut out = Vec::new();
        let start = self.blocks.len().saturating_sub(BLOCKS);
        for block in &mut self.blocks[start..] {
            if block.running() || block.kind == BlockKind::Ai || block.alt_screen() {
                continue;
            }
            let output = block.output_ansi(LINES);
            let kind = if block.kind == BlockKind::Background { "B" } else { "C" };
            let exit = block.exit.map(|c| c.to_string()).unwrap_or_default();
            let fields = [
                kind.to_owned(),
                exit,
                block.duration().as_millis().to_string(),
                output.len().to_string(),
                block.cwd.clone(),
                block.branch.clone().unwrap_or_default(),
                block.command.replace('\\', "\\\\").replace('\n', "\\n"),
            ];
            out.extend_from_slice(fields.join("\t").as_bytes());
            out.push(b'\n');
            out.extend_from_slice(&output);
            out.push(b'\n');
        }
        let _ = std::fs::write(path, out);
    }

    /// The blocks `save_blocks` wrote, finished as they were.
    pub fn restore_blocks(&mut self, path: &str) {
        let Ok(saved) = std::fs::read(path) else { return };
        let mut rest = &saved[..];
        while let Some(end) = rest.iter().position(|&b| b == b'\n') {
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = &rest[end + 1..];
            let fields: Vec<&str> = header.splitn(7, '\t').collect();
            let [kind, exit, millis, length, cwd, branch, command] = fields[..] else { return };
            let Ok(length) = length.parse::<usize>() else { return };
            if rest.len() < length + 1 {
                return;
            }
            let output = &rest[..length];
            rest = &rest[length + 1..];
            let kind = if kind == "B" { BlockKind::Background } else { BlockKind::Command };
            let mut unescaped = String::new();
            let mut chars = command.chars();
            while let Some(c) = chars.next() {
                match (c, c == '\\') {
                    (_, true) => unescaped.push(if chars.next() == Some('n') { '\n' } else { '\\' }),
                    (c, false) => unescaped.push(c),
                }
            }
            let command = unescaped;
            let block = self.new_block(kind, command);
            block.cwd = cwd.to_owned();
            block.branch = Some(branch.to_owned()).filter(|b| !b.is_empty());
            block.exit = exit.parse().ok();
            block.feed(output);
            let now = Instant::now();
            block.started = now.checked_sub(Duration::from_millis(millis.parse().unwrap_or(0))).unwrap_or(now);
            block.finished = Some(now);
            block.clears = false;
        }
    }

    /// The block whose program owns the keyboard, if any.
    pub fn active(&self) -> Option<&Block> {
        if self.phase != Phase::Running {
            return None;
        }
        self.blocks.iter().rev().find(|b| b.kind == BlockKind::Command && b.running())
    }

    /// The last command that ran to its end.
    pub fn last_finished(&self) -> Option<&Block> {
        self.blocks.iter().rev().find(|b| b.kind == BlockKind::Command && !b.running())
    }

    pub fn active_mut(&mut self) -> Option<&mut Block> {
        if self.phase != Phase::Running {
            return None;
        }
        self.blocks.iter_mut().rev().find(|b| b.kind == BlockKind::Command && b.running())
    }

    pub fn editor_active(&self) -> bool {
        matches!(self.phase, Phase::Ready | Phase::Continuation)
    }

    /// Sends a command line from the editor.
    pub fn submit(&mut self, command: &str) {
        let continuing = self.phase == Phase::Continuation;
        if continuing {
            if let Some(block) = self.blocks.iter_mut().rev().find(|b| b.kind == BlockKind::Command && b.running()) {
                block.command.push('\n');
                block.command.push_str(command);
            }
        } else {
            self.flush_idle_output();
            self.new_block(BlockKind::Command, command.to_owned());
        }
        self.phase = Phase::Submitted;
        self.continuation_since = None;
        let mut bytes = command.replace("\r\n", "\n").into_bytes();
        bytes.push(b'\r');
        // readline takes \r as accept-line; embedded newlines continue the
        // command (PS2), which is fine: their echo is dropped too.
        for byte in bytes.iter_mut() {
            if *byte == b'\n' {
                *byte = b'\r';
            }
        }
        self.write(&bytes);
    }

    /// A block for a request in AI mode, waiting on its answer.
    pub fn ask(&mut self, request: &str) -> u64 {
        self.flush_idle_output();
        self.new_block(BlockKind::Ai, request.to_owned()).id
    }

    /// Fills in the answer to request `id`; `ok` false marks it failed.
    pub fn answer(&mut self, id: u64, text: &str, ok: bool) {
        let Some(block) = self.blocks.iter_mut().find(|b| b.id == id) else { return };
        // The model's text, not a program's: no escape sequences.
        let text: String = text.chars().filter(|&c| c == '\n' || c == '\t' || !c.is_control()).collect();
        block.feed(text.replace('\n', "\r\n").as_bytes());
        block.finished = Some(Instant::now());
        block.exit = Some(if ok { 0 } else { 1 });
    }

    /// Ctrl+C at a continuation prompt abandons the command.
    pub fn interrupt(&mut self) {
        self.write(b"\x03");
    }

    pub fn feed(&mut self, bytes: &[u8], theme: &Theme) -> Vec<Notice> {
        let mut notices = Vec::new();
        for segment in self.scanner.scan(bytes) {
            match segment {
                Segment::Data(data) => self.route(&data, theme),
                // fish 4 marks a repaint's start again after the prompt's
                // end when only the command line is redrawn: at the prompt,
                // a start mark changes nothing.
                Segment::PromptStart if self.phase == Phase::Ready => {}
                Segment::PromptStart => {
                    self.finish_running(None, &mut notices);
                    self.phase = Phase::Prompt;
                }
                Segment::PromptEnd => {
                    if matches!(self.phase, Phase::Submitted | Phase::Continuation) {
                        // An empty or comment-only line never ran.
                        self.finish_running(Some(0), &mut notices);
                    }
                    self.phase = Phase::Ready;
                }
                // fish 4 marks commands itself as well as through the
                // integration: the second mark changes nothing.
                Segment::OutputStart if self.phase == Phase::Running => {}
                Segment::OutputStart => {
                    let echo = std::mem::take(&mut self.idle_output);
                    self.idle_since = None;
                    if !matches!(self.phase, Phase::Submitted | Phase::Continuation) {
                        // Typed-ahead input started a command on its own;
                        // its echo is the best name we have.
                        let command = last_visible_line(&echo);
                        self.new_block(BlockKind::Command, command);
                    }
                    if let Some(block) =
                        self.blocks.iter_mut().rev().find(|b| b.kind == BlockKind::Command && b.running())
                    {
                        block.started = Instant::now();
                    }
                    self.phase = Phase::Running;
                    self.continuation_since = None;
                }
                Segment::Done(code) => {
                    self.finish_running(code.or(Some(0)), &mut notices);
                    self.phase = Phase::Prompt;
                }
                Segment::Cwd(path) => {
                    if path != self.cwd {
                        self.branch = git_branch(&path);
                        self.cwd = path;
                    }
                }
                Segment::Continuation => {
                    if self.phase == Phase::Submitted {
                        self.continuation_since = Some(Instant::now());
                    }
                }
                Segment::Notify => notices.push(Notice::Notify),
                Segment::Path(path) => self.path = Some(path),
                Segment::Progress(busy) => notices.push(Notice::Progress(busy)),
                Segment::Iterm(body) => self.show_picture(crate::picture::iterm(&body)),
                Segment::Kitty(body) => {
                    let outcome = self.kitty.command(&body);
                    self.show_picture(outcome);
                }
            }
        }
        self.drain_events(theme, &mut notices);
        notices
    }

    /// An image a program printed: placed at its block's cursor; and the
    /// protocol's reply sent back.
    fn show_picture(&mut self, outcome: crate::picture::Outcome) {
        if let Some(reply) = outcome.reply {
            self.reply(&reply);
        }
        let Some(request) = outcome.show else { return };
        let cell = (self.cell_width.max(1.0), self.cell_height.max(1.0));
        if self.phase == Phase::Running {
            if let Some(block) = self.active_mut() {
                block.place_picture(request, cell);
            }
        }
    }

    fn route(&mut self, data: &[u8], theme: &Theme) {
        if self.phase != Phase::Running {
            self.answer_queries(data, theme);
        }
        match self.phase {
            Phase::Boot => {
                self.boot_output.extend_from_slice(data);
                if self.boot_output.len() > 64 * 1024 {
                    self.boot_output.drain(..32 * 1024);
                }
            }
            Phase::Ready => {
                self.idle_output.extend_from_slice(data);
                self.idle_since.get_or_insert_with(Instant::now);
            }
            Phase::Running => {
                self.output_at = Some(Instant::now());
                if let Some(block) = self.active_mut() {
                    block.feed(data);
                }
            }
            Phase::Prompt | Phase::Submitted | Phase::Continuation | Phase::Exited => {}
        }
    }

    /// Between commands no block's terminal sees the shell's output, so
    /// queries there are answered here. fish asks for device attributes,
    /// the cursor and the background before each prompt, and waits.
    fn answer_queries(&mut self, data: &[u8], theme: &Theme) {
        let mut reply = Vec::new();
        let mut i = 0;
        while let Some(at) = data[i..].iter().position(|&b| b == 0x1b) {
            let start = i + at + 1;
            i = start;
            match data.get(start) {
                Some(b'[') => {
                    let Some(end) = data[start + 1..].iter().position(|b| (0x40..=0x7e).contains(b)) else { break };
                    let end = start + 1 + end;
                    match (&data[start + 1..end], data[end]) {
                        // Primary device attributes, as alacritty answers.
                        (b"" | b"0", b'c') => reply.extend_from_slice(b"\x1b[?6c"),
                        (b"5", b'n') => reply.extend_from_slice(b"\x1b[0n"),
                        (b"6", b'n') => reply.extend_from_slice(b"\x1b[1;1R"),
                        _ => {}
                    }
                    i = end + 1;
                }
                Some(b']') => {
                    let body = &data[start + 1..];
                    let end = body.iter().position(|&b| b == 0x07 || b == 0x1b).unwrap_or(body.len());
                    let color = match &body[..end] {
                        b"10;?" => Some((10, theme.text)),
                        b"11;?" => Some((11, theme.background)),
                        _ => None,
                    };
                    if let Some((number, color)) = color {
                        let [r, g, b] = color.to_rgb8();
                        reply.extend(
                            format!("\x1b]{number};rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x1b\\").bytes(),
                        );
                    }
                    i = start + 1 + end;
                }
                _ => {}
            }
        }
        if !reply.is_empty() {
            self.reply(&reply);
        }
    }

    fn finish_running(&mut self, code: Option<i32>, notices: &mut Vec<Notice>) {
        let now = Instant::now();
        let Some(index) = self.blocks.iter().rposition(|b| b.kind == BlockKind::Command && b.running()) else {
            return;
        };
        let block = &mut self.blocks[index];
        block.finished = Some(now);
        block.exit = code;
        block.lines = None;
        notices.push(if block.clears { Notice::Clear } else { Notice::Finished });
    }

    fn flush_idle_output(&mut self) {
        let output = std::mem::take(&mut self.idle_output);
        self.idle_since = None;
        if last_visible_line(&output).is_empty() {
            return;
        }
        let block = self.new_block(BlockKind::Background, String::new());
        block.feed(&output);
        block.finished = Some(Instant::now());
    }

    /// Time-based transitions; returns true when something changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some(since) = self.continuation_since {
            if self.phase == Phase::Submitted && now.duration_since(since) >= CONTINUATION_DELAY {
                self.phase = Phase::Continuation;
                self.continuation_since = None;
                changed = true;
            }
        }
        if let Some(since) = self.idle_since {
            if self.phase == Phase::Ready && now.duration_since(since) >= IDLE_OUTPUT_DELAY {
                self.flush_idle_output();
                changed = true;
            }
        }
        changed
    }

    /// The next instant `tick` needs to run, if any.
    /// Each timer only counts in its own phase, so one left over from a
    /// phase the shell moved on from never wakes the loop.
    pub fn deadline(&self) -> Option<Instant> {
        let continuation =
            self.continuation_since.filter(|_| self.phase == Phase::Submitted).map(|t| t + CONTINUATION_DELAY);
        let idle = self.idle_since.filter(|_| self.phase == Phase::Ready).map(|t| t + IDLE_OUTPUT_DELAY);
        match (continuation, idle) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn drain_events(&mut self, theme: &Theme, notices: &mut Vec<Notice>) {
        let events: Vec<Event> = self.events.borrow_mut().drain(..).collect();
        for event in events {
            match event {
                Event::PtyWrite(text) => self.reply(text.as_bytes()),
                Event::Title(title) => notices.push(Notice::Title(title)),
                Event::ResetTitle => notices.push(Notice::Title(String::new())),
                Event::ClipboardStore(_, text) => notices.push(Notice::Clipboard(text)),
                Event::Bell => notices.push(Notice::Bell),
                Event::ColorRequest(index, format) => {
                    let colors = self.active().map(|b| *b.term.colors());
                    if let Some(colors) = colors {
                        let reply = format(theme.query_color(index, &colors));
                        self.reply(reply.as_bytes());
                    }
                }
                Event::TextAreaSizeRequest(format) => {
                    let reply = format(self.window_size());
                    self.reply(reply.as_bytes());
                }
                _ => {}
            }
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.lines as u16,
            num_cols: self.size.columns as u16,
            cell_width: self.cell.0,
            cell_height: self.cell.1,
        }
    }

    /// Resizes the pty and the running program's grid right away.
    pub fn resize(&mut self, size: GridSize) {
        if size.columns == self.size.columns && size.lines == self.size.lines {
            return;
        }
        self.size = size;
        if let Some(shell) = &self.shell {
            crate::platform::pty_resize(
                shell.master,
                size.columns as u16,
                size.lines as u16,
                size.columns as u32 * self.cell.0 as u32,
                size.lines as u32 * self.cell.1 as u32,
            );
        }
        if let Some(block) = self.active_mut() {
            block.resize(size);
        }
    }

    /// Re-wraps finished blocks to the current width (deferred while the
    /// window is being dragged, since it reflows every block).
    pub fn reflow(&mut self) {
        let size = self.size;
        for block in &mut self.blocks {
            block.resize(size);
        }
    }

    /// Removes every finished block (Alt+K, `clear`).
    pub fn clear(&mut self) {
        self.blocks.retain(|b| b.running());
    }
}

/// Writes `shell`'s integration script to the settings directory and
/// returns the command line that loads it (adding to `env` what that
/// needs). Shells Temino doesn't know are started like bash.
fn integrate(shell: &str, env: &mut Vec<String>) -> std::io::Result<Vec<String>> {
    let dir = crate::settings_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(match Kind::of_path(shell) {
        Some(Kind::Zsh) => {
            // zsh has no --rcfile: point ZDOTDIR at a .zshenv that puts the
            // user's back and then loads the integration.
            let zdotdir = format!("{dir}/zsh");
            std::fs::create_dir_all(&zdotdir)?;
            std::fs::write(format!("{zdotdir}/.zshenv"), ZSH_ENV)?;
            std::fs::write(format!("{zdotdir}/temino.zsh"), ZSH_INTEGRATION)?;
            if let Ok(original) = std::env::var("ZDOTDIR") {
                env.push(format!("TEMINO_ZDOTDIR={original}"));
            }
            env.push(format!("ZDOTDIR={zdotdir}"));
            vec![shell.into(), "-l".into(), "-i".into()]
        }
        Some(Kind::Fish) => {
            let path = format!("{dir}/temino.fish");
            std::fs::write(&path, FISH_INTEGRATION)?;
            // Through the environment, so the path needs no fish quoting.
            env.push(format!("TEMINO_INTEGRATION={path}"));
            let mut argv = vec![shell.to_owned()];
            argv.extend(["--login", "--interactive", "--init-command", "source $TEMINO_INTEGRATION"].map(String::from));
            argv
        }
        Some(Kind::Bash) | None => {
            let path = format!("{dir}/temino.bash");
            std::fs::write(&path, BASH_INTEGRATION)?;
            vec![shell.into(), "--rcfile".into(), path, "-i".into()]
        }
    })
}

/// The checked-out branch of the repository containing `path`, read
/// straight from .git/HEAD (no git process).
/// A link in a block's output, and the cells it covers as
/// (output line, first column, last column) runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    pub cells: Vec<(usize, usize, usize)>,
}

/// The path-like word around `index` in `chars`, without quotes or
/// brackets around it, a `:line:column` after it, or sentence punctuation.
pub fn find_path(chars: &[char], index: usize) -> Option<(usize, usize)> {
    let stop = |c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | ',' | ';')
            || ('\u{2500}'..='\u{259f}').contains(&c)
    };
    if stop(*chars.get(index)?) {
        return None;
    }
    let mut start = index;
    while start > 0 && !stop(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && !stop(chars[end]) {
        end += 1;
    }
    // `src/main.rs:12:5` → `src/main.rs`.
    loop {
        let digits = chars[start..end].iter().rev().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && end - digits > start && chars[end - digits - 1] == ':' {
            end -= digits + 1;
        } else if end > start && matches!(chars[end - 1], '.' | ':' | '!' | '?') {
            end -= 1;
        } else {
            break;
        }
    }
    (end > start && index < end).then_some((start, end))
}

/// Where URLs sit in `chars`, as [start, end) character ranges.
pub fn find_urls(chars: &[char]) -> Vec<(usize, usize)> {
    const SCHEMES: [&str; 6] = ["https://", "http://", "file://", "ftp://", "ssh://", "mailto:"];
    let allowed = |c: char| {
        (c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '^' | '\\'))
            || (!c.is_ascii() && c.is_alphanumeric())
    };
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let scheme = SCHEMES.iter().find(|scheme| {
            let scheme: Vec<char> = scheme.chars().collect();
            chars[i..].starts_with(&scheme)
        });
        let Some(scheme) = scheme.filter(|_| boundary) else {
            i += 1;
            continue;
        };
        let body = i + scheme.len();
        let mut end = body;
        while end < chars.len() && allowed(chars[end]) {
            end += 1;
        }
        // Trailing punctuation belongs to the sentence, and a closing
        // bracket to the text around it unless the URL opened one.
        while end > body {
            let last = chars[end - 1];
            let opened = |open: char| chars[i..end].iter().filter(|&&c| c == open).count();
            let closed = |close: char| chars[i..end].iter().filter(|&&c| c == close).count();
            let unbalanced = match last {
                ')' => closed(')') > opened('('),
                ']' => closed(']') > opened('['),
                _ => false,
            };
            if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*') || unbalanced {
                end -= 1;
            } else {
                break;
            }
        }
        if end > body {
            found.push((i, end));
        }
        i = end.max(i + 1);
    }
    found
}

pub fn git_branch(path: &str) -> Option<String> {
    let mut dir = std::path::PathBuf::from(path);
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_owned(),
                None => head.chars().take(7).collect(),
            });
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// A UTF-8 `LANG` for the shell when nothing in `env` picks a locale.
/// Opened from the Dock or Finder, a macOS app gets no LANG at all, and the
/// shell and its programs fall back to ASCII: pasted Chinese turns into
/// escapes. Uses the system's region, as Terminal does.
fn utf8_lang(env: &[String]) -> Option<String> {
    let set = |key: &str| {
        env.iter().any(|entry| entry.strip_prefix(key).is_some_and(|rest| rest.starts_with('=') && rest.len() > 1))
    };
    if set("LANG") || set("LC_ALL") || set("LC_CTYPE") {
        return None;
    }
    if cfg!(target_os = "haiku") {
        return None;
    }
    if cfg!(target_os = "macos") {
        static MAC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        return Some(MAC.get_or_init(mac_lang).clone());
    }
    Some("C.UTF-8".to_owned())
}

/// The system region's UTF-8 locale.
fn mac_lang() -> String {
    let region = std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    // "en_US@rg=gbzzzz" → "en_US.UTF-8", if the system has that locale.
    let lang = region
        .map(|region| format!("{}.UTF-8", region.split('@').next().unwrap_or("")))
        .filter(|lang| lang.len() > 6 && std::path::Path::new("/usr/share/locale").join(lang).is_dir());
    lang.unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// fish 4.9 redrawing an unchanged prompt (here after SIGWINCH): its own
    /// start mark comes last, after the prompt's end.
    const FISH_REPAINT: &[u8] = b"\x1b[m\x1b[?2004h\r\r\x1b]133;A;click_events=1\x1b\\\x1b]133;B\x07\x1b]133;B\x1b\\\x1b[J\x1b]133;A;click_events=1\x1b\\";

    #[test]
    fn prompt_reports_the_shell_path_and_notifications() {
        let theme = Theme::dark();
        let mut session = Session::new(GridSize { columns: 80, lines: 24 });
        session.feed(b"\x1b]6973;path;/opt/homebrew/bin:/usr/bin\x07\x1b]133;A\x07", &theme);
        assert_eq!(session.path.as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
        let notices = session.feed(b"\x1b]9;Claude needs you\x07\x1b]9;4;3;\x07\x1b]9;4;0;\x07", &theme);
        let kinds: Vec<_> = notices
            .iter()
            .map(|n| match n {
                Notice::Notify => "notify",
                Notice::Progress(true) => "busy",
                Notice::Progress(false) => "idle",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["notify", "busy", "idle"]);
    }

    #[test]
    fn urls_are_found_without_trailing_punctuation() {
        let find = |text: &str| -> Vec<String> {
            let chars: Vec<char> = text.chars().collect();
            find_urls(&chars).into_iter().map(|(a, b)| chars[a..b].iter().collect()).collect()
        };
        assert_eq!(find("see https://example.com/a?b=1."), ["https://example.com/a?b=1"]);
        assert_eq!(
            find("(at https://en.wikipedia.org/wiki/Rust_(language))"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(find("[docs](http://x.io/y) │ file:///tmp/a b"), ["http://x.io/y", "file:///tmp/a"]);
        assert_eq!(find("nothttps://x.io https:// "), Vec::<String>::new());
    }

    #[test]
    fn paths_are_cut_from_their_surroundings() {
        let path = |text: &str, at: usize| -> Option<String> {
            let chars: Vec<char> = text.chars().collect();
            find_path(&chars, at).map(|(a, b)| chars[a..b].iter().collect())
        };
        assert_eq!(path("error at src/pane.rs:12:5: oops", 10).as_deref(), Some("src/pane.rs"));
        assert_eq!(path("open '~/notes.md'.", 8).as_deref(), Some("~/notes.md"));
        assert_eq!(path("│ /tmp/x │", 3).as_deref(), Some("/tmp/x"));
        assert_eq!(path("a  b", 1), None);
    }

    #[test]
    fn local_files_are_links() {
        let theme = Theme::dark();
        let mut session = Session::new(GridSize { columns: 40, lines: 10 });
        session.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07", &theme);
        session.submit("ls");
        session.feed(b"\x1b]133;C\x07Cargo.toml  no-such-file  /tmp\r\n", &theme);
        let block = session.blocks.last_mut().unwrap();
        block.cwd = env!("CARGO_MANIFEST_DIR").to_owned();
        let link = block.link_at(0, 2).expect("Cargo.toml exists");
        assert_eq!(link.url, format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR")));
        assert_eq!(link.cells, [(0, 0, 9)]);
        assert_eq!(block.link_at(0, 14), None);
        assert_eq!(block.link_at(0, 27).map(|l| l.url).as_deref(), Some("/tmp"));
    }

    #[test]
    fn links_follow_soft_wraps() {
        let theme = Theme::dark();
        let mut session = Session::new(GridSize { columns: 20, lines: 10 });
        session.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07", &theme);
        session.submit("echo");
        session.feed(b"\x1b]133;C\x07go to https://example.com/very/long/path now\r\n", &theme);
        let block = session.blocks.last().unwrap();
        let link = block.link_at(1, 3).expect("a link on the wrapped line");
        assert_eq!(link.url, "https://example.com/very/long/path");
        assert_eq!(link.cells, [(0, 6, 19), (1, 0, 19)]);
        assert_eq!(block.link_at(0, 1), None);
        session.feed(b"\x1b]8;;https://anthropic.com\x07here\x1b]8;;\x07\r\n", &theme);
        let block = session.blocks.last().unwrap();
        let link = block.link_at(3, 2).expect("an OSC 8 link");
        assert_eq!((link.url.as_str(), link.cells.as_slice()), ("https://anthropic.com", &[(3, 0, 3)][..]));
    }

    #[test]
    fn repaint_keeps_the_prompt() {
        let theme = Theme::dark();
        let mut session = Session::new(GridSize { columns: 80, lines: 24 });
        session.feed(b"\x1b]133;A\x07\x1b]133;B\x07", &theme);
        assert_eq!(session.phase, Phase::Ready);
        session.feed(FISH_REPAINT, &theme);
        assert_eq!(session.phase, Phase::Ready);
        session.tick(Instant::now() + IDLE_OUTPUT_DELAY);
        assert_eq!(session.deadline(), None);
    }

    #[test]
    fn saved_blocks_come_back_with_their_colors() {
        let size = GridSize { lines: 10, columns: 40 };
        let mut session = Session::new(size);
        let block = session.new_block(BlockKind::Command, "echo 'a\\b'\nls".into());
        block.feed(b"\x1b[31mred\x1b[0m plain\r\nline two");
        block.exit = Some(2);
        block.finished = Some(Instant::now());
        let path = std::env::temp_dir().join(format!("temino-restore-{}", std::process::id()));
        let path = path.to_str().unwrap();
        session.save_blocks(path);
        let mut restored = Session::new(size);
        restored.restore_blocks(path);
        let _ = std::fs::remove_file(path);
        let block = &mut restored.blocks[0];
        assert_eq!(block.command, "echo 'a\\b'\nls");
        assert_eq!(block.exit, Some(2));
        assert_eq!(block.output_text(), "red plain\nline two");
        let red = &block.term.grid()[block.grid_line(0)][Column(0)];
        assert_eq!(red.fg, alacritty_terminal::vte::ansi::Color::Named(alacritty_terminal::vte::ansi::NamedColor::Red));
    }

    #[test]
    fn stale_timers_never_wake() {
        let theme = Theme::dark();
        let mut session = Session::new(GridSize { columns: 80, lines: 24 });
        session.feed(b"\x1b]133;A\x07\x1b]133;B\x07", &theme);
        session.feed(b"background job output\r\n", &theme);
        session.phase = Phase::Prompt;
        assert_eq!(session.deadline(), None);
    }
}

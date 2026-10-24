//! The shell session: the pty and its threads, the shell-integration
//! markers (OSC 133 prompt/command/output/done, OSC 7 cwd), and the block
//! list. Like Warp, every command gets its own terminal grid, so output
//! never interleaves with prompts and each block can be styled, selected
//! and copied on its own.

use std::cell::RefCell;
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

use crate::theme::Theme;
use crate::Msg;

const SHELL_INTEGRATION: &str = include_str!("shell/temino.bash");
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
    lines: Option<usize>,
    size: GridSize,
    /// `clear`: runs like any command, then wipes the block list.
    pub clears: bool,
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
            lines: None,
            size,
            clears,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.lines = None;
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
        let lines = grid.history_size() + (bottom + 1).max(0) as usize;
        self.lines = Some(lines);
        lines
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
}

/// Pulls Temino's own OSC sequences out of the byte stream; everything else
/// passes through untouched, in order. Sequences may span reads.
#[derive(Default)]
struct Scanner {
    pending_escape: bool,
    osc: Option<Vec<u8>>,
}

impl Scanner {
    const MAX_OSC: usize = 4096;

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
                    match parse_osc(&body) {
                        Some(segment) => {
                            if !data.is_empty() {
                                out.push(Segment::Data(std::mem::take(&mut data)));
                            }
                            out.push(segment);
                        }
                        None => {
                            data.extend_from_slice(b"\x1b]");
                            data.extend_from_slice(&body);
                            data.extend_from_slice(if byte == 0x07 { b"\x07" } else { b"\x1b\\" });
                        }
                    }
                } else if self.pending_escape {
                    // ESC inside an OSC that isn't ST: abandon the OSC.
                    let body = std::mem::take(osc);
                    self.osc = None;
                    self.pending_escape = false;
                    data.extend_from_slice(b"\x1b]");
                    data.extend_from_slice(&body);
                    data.push(0x1b);
                    continue; // reprocess this byte normally
                } else if byte == 0x1b {
                    self.pending_escape = true;
                } else {
                    osc.push(byte);
                    if osc.len() > Self::MAX_OSC {
                        let body = std::mem::take(osc);
                        self.osc = None;
                        data.extend_from_slice(b"\x1b]");
                        data.extend_from_slice(&body);
                    }
                }
                i += 1;
                continue;
            }
            if self.pending_escape {
                self.pending_escape = false;
                if byte == b']' {
                    self.osc = Some(Vec::new());
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
        _ => None,
    }
}

fn percent_decode(text: &str) -> String {
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
    scanner: Scanner,
    events: Rc<RefCell<Vec<Event>>>,
    next_id: u64,
    /// Bytes seen at the idle prompt, held back until we know whether a
    /// command starts (then they were its echo) or not (background output).
    idle_output: Vec<u8>,
    idle_since: Option<Instant>,
    continuation_since: Option<Instant>,
    pub exit_code: Option<i32>,
    pub boot_output: Vec<u8>,
    integration_path: String,
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
            scanner: Scanner::default(),
            events: Rc::new(RefCell::new(Vec::new())),
            next_id: 1,
            idle_output: Vec::new(),
            idle_since: None,
            continuation_since: None,
            exit_code: None,
            boot_output: Vec::new(),
            integration_path: String::new(),
        }
    }

    pub fn set_cell_size(&mut self, width: f32, height: f32) {
        self.cell = (width as u16, height as u16);
    }

    pub fn start(&mut self, sender: Sender<Msg>, credits: Arc<Credits>) -> std::io::Result<()> {
        let dir = crate::settings_dir();
        std::fs::create_dir_all(&dir)?;
        self.integration_path = format!("{dir}/temino.bash");
        std::fs::write(&self.integration_path, SHELL_INTEGRATION)?;

        let shell = std::env::var("TEMINO_SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let argv = vec![shell, "--rcfile".into(), self.integration_path.clone(), "-i".into()];
        let mut env: Vec<String> = std::env::vars()
            .filter(|(key, _)| !matches!(key.as_str(), "TERM" | "COLORTERM" | "TERM_PROGRAM" | "COLUMNS" | "LINES"))
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        env.push("TERM=xterm-256color".into());
        env.push("COLORTERM=truecolor".into());
        env.push("TERM_PROGRAM=Temino".into());
        env.push(format!("TERM_PROGRAM_VERSION={}", env!("CARGO_PKG_VERSION")));

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
                        if data_sender.send(Msg::Pty(buffer[..n].to_vec())).is_err() {
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
            let _ = sender.send(Msg::ShellExited(code));
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

    pub fn write(&self, bytes: &[u8]) {
        if let Some(shell) = &self.shell {
            let _ = shell.writer.send(bytes.to_vec());
        }
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

    /// The block whose program owns the keyboard, if any.
    pub fn active(&self) -> Option<&Block> {
        if self.phase != Phase::Running {
            return None;
        }
        self.blocks.iter().rev().find(|b| b.kind == BlockKind::Command && b.running())
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

    /// Ctrl+C at a continuation prompt abandons the command.
    pub fn interrupt(&mut self) {
        self.write(b"\x03");
    }

    pub fn feed(&mut self, bytes: &[u8], theme: &Theme) -> Vec<Notice> {
        let mut notices = Vec::new();
        for segment in self.scanner.scan(bytes) {
            match segment {
                Segment::Data(data) => self.route(&data),
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
            }
        }
        self.drain_events(theme, &mut notices);
        notices
    }

    fn route(&mut self, data: &[u8]) {
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
                if let Some(block) = self.active_mut() {
                    block.feed(data);
                }
            }
            Phase::Prompt | Phase::Submitted | Phase::Continuation | Phase::Exited => {}
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
    pub fn deadline(&self) -> Option<Instant> {
        let continuation = self.continuation_since.map(|t| t + CONTINUATION_DELAY);
        let idle = self.idle_since.map(|t| t + IDLE_OUTPUT_DELAY);
        match (continuation, idle) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn drain_events(&mut self, theme: &Theme, notices: &mut Vec<Notice>) {
        let events: Vec<Event> = self.events.borrow_mut().drain(..).collect();
        for event in events {
            match event {
                Event::PtyWrite(text) => self.write(text.as_bytes()),
                Event::Title(title) => notices.push(Notice::Title(title)),
                Event::ResetTitle => notices.push(Notice::Title(String::new())),
                Event::ClipboardStore(_, text) => notices.push(Notice::Clipboard(text)),
                Event::Bell => notices.push(Notice::Bell),
                Event::ColorRequest(index, format) => {
                    let colors = self.active().map(|b| *b.term.colors());
                    if let Some(colors) = colors {
                        let reply = format(theme.query_color(index, &colors));
                        self.write(reply.as_bytes());
                    }
                }
                Event::TextAreaSizeRequest(format) => {
                    let reply = format(self.window_size());
                    self.write(reply.as_bytes());
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

/// The checked-out branch of the repository containing `path`, read
/// straight from .git/HEAD (no git process).
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

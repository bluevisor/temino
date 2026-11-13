//! One terminal: a shell session with its blocks, the command editor and
//! everything about how it's scrolled, selected and drawn. The window
//! (`app.rs`) arranges panes into tabs and splits; a pane draws and takes
//! the mouse in its own coordinates, as if it had the window to itself.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;

use crate::agent::{self, AgentState};
use crate::ask::{self, Reply};
use crate::complete::{self, Candidate, Commands, Completion, Kind};
use crate::editor::{Editor, History};
use crate::find::Find;
use crate::keys::{self, Named};
use crate::llm;
use crate::platform::{self, buttons, Cursor, KeyEvent};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::session::{Block, BlockKind, Credits, GridSize, Link, Notice, Phase, Session};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme, SURFACE_LIFT};
use crate::ui::{self, GridView, Span};
use crate::Msg;

mod attention;
mod draw;
mod mouse;
mod typing;
use draw::*;
use mouse::*;

pub const PAD_X: f32 = 14.0;
const ALT_PAD_Y: f32 = 8.0;
/// How wide the scroll bar's grab area is, at the pane's right edge.
const SCROLLBAR_HIT: f32 = 14.0;
const BLOCK_PAD_TOP: f32 = 10.0;
const META_HEIGHT: f32 = 18.0;
const OUTPUT_GAP: f32 = 4.0;
const BLOCK_PAD_BOTTOM: f32 = 12.0;
const CHIP_HEIGHT: f32 = 22.0;
const INPUT_PAD: f32 = 10.0;
pub const BLINK: Duration = Duration::from_millis(530);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const MAX_EDITOR_LINES: usize = 10;
/// How long an idle pane's count of changed files is trusted.
const GIT_RECHECK: Duration = Duration::from_secs(20);
const MENU_ROWS: usize = 8;
/// How many commands a pane's title lists.
const TITLE_COMMANDS: usize = 3;
/// How often an agent's screen is read, how many of its last lines, how
/// long its output counts as working, and how soon after a keystroke
/// output is only its echo.
const AGENT_SCAN: Duration = Duration::from_millis(300);
const AGENT_SCAN_LINES: usize = 10;
const AGENT_QUIET: Duration = Duration::from_millis(1500);
const AGENT_ECHO: Duration = Duration::from_millis(400);
/// How long after you press Enter output still counts as the agent
/// answering.
const AGENT_ASKED: Duration = Duration::from_secs(10);
/// A turn shorter than this was a repaint, not work: no green pop for it.
const AGENT_MIN_TURN: Duration = Duration::from_secs(3);
/// How long a recap without a summary stays up, and at most while its
/// summary is on the way.
const RECAP_SHOWN: Duration = Duration::from_secs(6);
const RECAP_WAIT: Duration = Duration::from_secs(20);
const RECAP_SYSTEM: &str = "Someone is coming back to a terminal tab after a break. From what ran there, \
say in one short sentence (at most 20 words) what they were doing and where it stands. \
Plain text, no preamble, no quotes.";
/// How much of a pane's recent history the summary is written from.
const RECAP_BLOCKS: usize = 5;
const RECAP_LINES: usize = 30;
const RECAP_CHARS: usize = 6000;
/// The launch banner: 5-pixel-tall block letters packed two pixel rows per
/// cell with half blocks, so it's 30x3 cells. Every stroke is one pixel
/// wide.
const BANNER: &[&str] =
    &["▀▀█▀▀ █▀▀▀ █▄ ▄█ ▀█▀ █▄ █ █▀▀█", "  █   █▀▀  █ ▀ █  █  █ ▀█ █  █", "  ▀   ▀▀▀▀ ▀   ▀ ▀▀▀ ▀  ▀ ▀▀▀▀"];
/// The big banner for very wide panes, in figlet's "ANSI Shadow" style.
const BANNER_LARGE: &[&str] = &[
    "████████╗███████╗███╗   ███╗██╗███╗   ██╗ ██████╗ ",
    "╚══██╔══╝██╔════╝████╗ ████║██║████╗  ██║██╔═══██╗",
    "   ██║   █████╗  ██╔████╔██║██║██╔██╗ ██║██║   ██║",
    "   ██║   ██╔══╝  ██║╚██╔╝██║██║██║╚██╗██║██║   ██║",
    "   ██║   ███████╗██║ ╚═╝ ██║██║██║ ╚████║╚██████╔╝",
    "   ╚═╝   ╚══════╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝ ╚═════╝ ",
];
/// Pane width (columns) from which `BANNER_LARGE` replaces `BANNER`.
const BANNER_LARGE_COLUMNS: usize = 150;

/// What every pane shares: fonts, colors, history, the clock for the caret.
pub struct Ctx {
    pub text: TextSystem,
    pub theme: Theme,
    pub history: History,
    /// Requests made in AI mode: what Up and Down recall there.
    pub asked: History,
    pub commands: Commands,
    pub sender: Sender<Msg>,
    pub user_host: String,
    pub device_name: String,
    pub blink_epoch: Instant,
    pub toast: Option<(String, Instant)>,
    /// The shell new panes start (Settings ▸ Shell).
    pub shell: String,
    /// An input method is composing at the cursor: the prompt's hint stays
    /// out of its way.
    pub composing: bool,
    /// Desktop notifications programs asked for, by pane: title, body. The
    /// window shows those for panes you can't see.
    pub notifications: Vec<(u64, Option<String>, String)>,
}

impl Ctx {
    pub fn show_toast(&mut self, message: &str) {
        self.toast = Some((message.to_owned(), Instant::now() + Duration::from_millis(1600)));
    }

    /// The cursor's blink phase: always on when Settings ▸ Cursor ▸ Blink is off.
    pub fn blink_on(&self) -> bool {
        if !crate::cursor::blinks() {
            return true;
        }
        let elapsed = Instant::now().duration_since(self.blink_epoch);
        (elapsed.as_millis() / BLINK.as_millis()).is_multiple_of(2)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectMode {
    Char,
    Word,
    Line,
}

/// A mouse selection inside one block: its command (in wrapped rows), its
/// output, or from one into the other.
#[derive(Clone, Copy)]
struct Selection {
    block: u64,
    anchor: Spot,
    head: Spot,
    mode: SelectMode,
}

/// A place in a block: (in the output, not the command; line; column). The
/// command comes first, so a drag from it reaches down into the output.
type Spot = (bool, usize, usize);

struct BlockLayout {
    id: u64,
    rect: Rect,
    output_x: f32,
    output_y: f32,
    lines: usize,
    command_y: f32,
    command_rows: usize,
}

#[derive(Default)]
struct Layout {
    blocks: Vec<BlockLayout>,
    list: Rect,
    input: Rect,
    editor_x: f32,
    editor_y: f32,
    menu_rows: Vec<(Rect, usize)>,
    /// (block index, grid origin) when a full-screen program is shown.
    alt: Option<(usize, f32, f32)>,
    content_height: f32,
    /// The scroll bar's track, and its thumb's height, when output overflows.
    scrollbar: Option<(Rect, f32)>,
}

pub struct Pane {
    pub id: u64,
    pub session: Session,
    editor: Editor,
    /// Commands run and requests asked in this pane: what its Up and Down
    /// recall, so one pane's history doesn't mix into another's.
    ran: History,
    asked_here: History,
    completion: Option<Completion>,
    /// The last completion hint worked out, keyed by cwd and editor text.
    hint: Option<(String, Option<String>)>,
    /// Where the pointer is over this pane, for underlining the link under it.
    pointer: Option<(f32, f32)>,
    /// A link pressed with a plain click: it opens if the button comes up
    /// without a drag.
    link_click: Option<String>,
    /// A link a plain click let go of, for the app to open in a viewer.
    pub clicked_link: Option<String>,
    /// While dragging the scroll bar: where in the thumb it was grabbed.
    bar_grab: Option<f32>,
    credits: Arc<Credits>,
    /// Where the pane sits in the window, in logical pixels.
    pub rect: Rect,
    /// Whether the launch banner shows while there are no blocks.
    welcome: bool,
    /// Distance of the viewport's bottom above the content's bottom.
    scroll: f32,
    scroll_target: f32,
    selected: Option<u64>,
    selection: Option<Selection>,
    dragging: bool,
    /// A press in the command input is dragging its selection.
    editor_dragging: bool,
    layout: Layout,
    pub bell: Option<Instant>,
    reflow_at: Option<Instant>,
    /// Content height of the previous frame, to keep a scrolled-up view
    /// still while output grows below it.
    content_height: f32,
    /// Fractions of a wheel line not yet sent to a full-screen program.
    wheel_rest: f32,
    /// A command to run once the shell is at its prompt, then its block.
    queued: Option<String>,
    queued_block: Option<u64>,
    /// What the running program last titled itself, whether it asked for
    /// you (a notification or bell) and when, and whether it reports
    /// progress: the clues to an agent's state.
    program_title: String,
    notified_at: Option<Instant>,
    progress: bool,
    /// The agent's state as last read, the bottom of its screen as last
    /// read and when, and whether it finished a turn you haven't seen.
    agent_was: Option<AgentState>,
    /// The block an agent was last seen running in: it stays an agent
    /// whatever it titles itself next.
    agent_block: Option<u64>,
    /// Which agent that is, once known: Claude Code run under an alias is
    /// only named by its resting `✳` title, which its spinner replaces
    /// while it works.
    agent_program: Option<&'static str>,
    screen: Option<(Instant, Option<AgentState>)>,
    pub agent_unseen: bool,
    /// Whether the agent's header is extended to show what it only has room
    /// to hint at: the whole prompt, the subagents' tasks, the queue.
    pub header_open: bool,
    /// How far the opened header reaches below its bar, pushing the
    /// terminal down by as much.
    pub header_reach: f32,
    /// The agent's state this frame, read once by the window.
    pub agent_now: Option<AgentState>,
    /// Since when the agent has been waiting on you or done unseen, and which.
    pub needs_since: Option<(AgentState, Instant)>,
    /// The listed program running here this frame, and since when.
    pub program_now: Option<(String, Instant)>,
    /// Claude Code's own account of the agent, read in the background.
    watcher: Option<agent::Watcher>,
    /// What's been typed to the agent since the last Enter, and the last
    /// line sent: its prompt, for agents with no records we can read.
    typed: String,
    typed_prompt: Option<String>,
    /// When the agent's current turn began, and how long the last one took.
    turn: Option<Instant>,
    last_turn: Option<Duration>,
    /// When focus last left the pane (or the window), and the recap shown
    /// on coming back to it after a while.
    left_at: Option<Instant>,
    recap: Option<Recap>,
    /// AI mode: Enter sends the editor's text to the model instead of the
    /// shell. The request on its way: its block and the model's reply.
    ai: bool,
    asking: Option<(u64, Answer)>,
    /// How many files `git status` lists in the folder, by folder; and what
    /// it was last asked for (folder and last command), and when.
    pub git_changes: Option<(String, usize)>,
    git_asked: Option<(String, Option<u64>, Instant)>,
    /// Command+F: the search bar and its matches, and the last query.
    find: Option<Find>,
    last_query: String,
}

/// The model's reply to an AI-mode request, filled in by its thread.
type Answer = Arc<Mutex<Option<Result<Reply, String>>>>;

/// "Where was I?": a line of facts about a pane you come back to, and a
/// model's sentence on what you were doing, which arrives a little later.
struct Recap {
    shown: Instant,
    facts: String,
    /// The summary, once the model answered (None if it couldn't).
    summary: Arc<Mutex<Option<Option<String>>>>,
    /// When the summary came in, so it gets its own few seconds.
    summary_at: Option<Instant>,
}

/// What a tab's mark adds to a recap: a fact on its first line, and a
/// line for the model writing the summary.
pub struct MarkNote {
    pub fact: String,
    pub context: String,
}

/// What the header over an agent's pane says.
pub struct AgentInfo {
    /// The agent, as its entry in agent::AGENTS.
    pub program: Option<&'static str>,
    pub state: AgentState,
    pub prompt: Option<String>,
    pub subagents: usize,
    /// What each subagent was sent to do.
    pub subagent_tasks: Vec<String>,
    /// Prompts waiting for the current turn to end, and their text.
    pub queued: usize,
    pub queue: Vec<String>,
    pub name: Option<String>,
    /// How long the current turn has run, or the last one took.
    pub turn: Option<Duration>,
    /// How long the agent has been running.
    pub uptime: Duration,
    /// The recap, while one is up: the header shows it in place of the
    /// prompt.
    pub recap: Option<String>,
    /// The folder it runs in, and its git branch.
    pub cwd: String,
    pub branch: Option<String>,
    /// Whether the header is extended.
    pub open: bool,
}

impl Pane {
    pub fn new(id: u64, ctx: &mut Ctx, rect: Rect, cwd: Option<String>, welcome: bool) -> Result<Pane, String> {
        let mut session = Session::new(grid_size(rect, &ctx.text));
        if let Some(cwd) = cwd.filter(|dir| std::path::Path::new(dir).is_dir()) {
            session.branch = crate::session::git_branch(&cwd);
            session.cwd = cwd;
        }
        session.set_cell_size(ctx.text.cell.width, ctx.text.cell.height);
        let credits = Credits::new();
        session
            .start(id, &ctx.shell, ctx.sender.clone(), credits.clone())
            .map_err(|e| format!("starting the shell: {e}"))?;
        Ok(Pane {
            id,
            session,
            editor: Editor::new(),
            completion: None,
            hint: None,
            pointer: None,
            link_click: None,
            clicked_link: None,
            bar_grab: None,
            credits,
            rect,
            welcome,
            scroll: 0.0,
            scroll_target: 0.0,
            selected: None,
            selection: None,
            dragging: false,
            editor_dragging: false,
            layout: Layout::default(),
            bell: None,
            reflow_at: None,
            content_height: 0.0,
            wheel_rest: 0.0,
            queued: None,
            queued_block: None,
            program_title: String::new(),
            notified_at: None,
            progress: false,
            agent_was: None,
            agent_block: None,
            agent_program: None,
            screen: None,
            agent_unseen: false,
            header_open: false,
            header_reach: 0.0,
            program_now: None,
            agent_now: None,
            needs_since: None,
            git_changes: None,
            git_asked: None,
            ran: History::local(),
            asked_here: History::local(),
            watcher: None,
            typed: String::new(),
            typed_prompt: None,
            turn: None,
            last_turn: None,
            left_at: None,
            recap: None,
            ai: false,
            asking: None,
            find: None,
            last_query: String::new(),
        })
    }

    pub fn feed(&mut self, ctx: &mut Ctx, bytes: &[u8]) {
        let notices = self.session.feed(bytes, &ctx.theme);
        self.credits.release();
        for notice in notices {
            self.notice(ctx, notice);
        }
        if self.session.phase == Phase::Ready {
            if let Some(command) = self.queued.take() {
                self.session.submit(&command);
                self.queued_block = self.session.blocks.last().map(|block| block.id);
                self.pin_to_bottom();
            }
        }
    }

    /// Runs `command` as if typed, as soon as the shell is at its prompt.
    /// The listed program running here that gets a header, and since when.
    pub fn header_program(&self) -> Option<(String, Instant)> {
        let block = self.session.active().filter(|block| block.kind == BlockKind::Command)?;
        agent::header_program(&block.command).map(|name| (name.to_owned(), block.started))
    }

    pub fn run_when_ready(&mut self, command: String) {
        self.queued = Some(command);
    }

    /// The exit status of the `run_when_ready` command, once it finished.
    pub fn take_queued_exit(&mut self) -> Option<i32> {
        let id = self.queued_block?;
        let block = self.session.blocks.iter().find(|block| block.id == id);
        if block.is_some_and(|block| block.running()) {
            return None;
        }
        self.queued_block = None;
        // Cleared away before we looked: its status is gone.
        Some(block.map_or(-1, |block| block.exit.unwrap_or(0)))
    }

    fn notice(&mut self, ctx: &mut Ctx, notice: Notice) {
        match notice {
            Notice::Title(title) => {
                if let Some(block) = self.session.active_mut() {
                    if block.command.is_empty() {
                        block.command = title.clone();
                    }
                }
                self.program_title = title;
            }
            Notice::Clipboard(text) => {
                platform::clipboard_set(&text);
                ctx.show_toast("Copied to the clipboard by the program");
            }
            Notice::Bell => {
                self.bell = Some(Instant::now());
                self.notified_at = self.bell;
            }
            Notice::Notify(title, body) => {
                self.notified_at = Some(Instant::now());
                ctx.notifications.push((self.id, title, body));
            }
            Notice::Progress(busy) => self.progress = busy,
            Notice::Clear => {
                self.forget_program();
                self.clear_blocks();
            }
            Notice::Finished => self.forget_program(),
        }
    }

    fn forget_program(&mut self) {
        self.program_title.clear();
        self.notified_at = None;
        self.progress = false;
    }

    /// Advances animations; true when the pane needs a new frame.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut dirty = self.session.tick(now) | self.take_answer();
        if let Some(recap) = &mut self.recap {
            if recap.summary_at.is_none() && recap.summary().is_some() {
                recap.summary_at = Some(now);
                dirty = true;
            }
            if recap.until().is_some_and(|until| now >= until) {
                self.recap = None;
                dirty = true;
            }
        }
        if self.reflow_at.is_some_and(|at| now >= at) {
            self.reflow_at = None;
            self.session.reflow();
            dirty = true;
        }
        if self.bell.is_some_and(|at| now.duration_since(at) > Duration::from_millis(180)) {
            self.bell = None;
            dirty = true;
        }
        if (self.scroll - self.scroll_target).abs() > 0.5 {
            self.scroll += (self.scroll_target - self.scroll) * 0.35;
            dirty = true;
        } else if self.scroll != self.scroll_target {
            self.scroll = self.scroll_target;
            dirty = true;
        }
        dirty
    }

    /// When the frame goes stale on its own: the caret blinks, a spinner turns.
    pub fn animation_deadline(&self, ctx: &Ctx, focused: bool) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut want = |at: Instant| next = Some(next.map_or(at, |n| n.min(at)));
        // A steady cursor (Settings ▸ Cursor ▸ Blink off) needs no timer.
        if focused && crate::cursor::blinks() && (self.session.editor_active() || self.session.active().is_some()) {
            let elapsed = now.duration_since(ctx.blink_epoch).as_millis() as u64;
            let period = BLINK.as_millis() as u64;
            want(ctx.blink_epoch + Duration::from_millis((elapsed / period + 1) * period));
        }
        if self.session.active().is_some_and(|b| !b.alt_screen())
            || self.session.phase == Phase::Boot
            || self.asking.is_some()
        {
            want(now + Duration::from_millis(80));
        }
        next
    }

    pub fn next_wakeup(&self) -> Option<Instant> {
        let mut times = vec![self.session.deadline(), self.reflow_at];
        times.push(self.bell.map(|at| at + Duration::from_millis(200)));
        if self.asking.is_some() {
            times.push(Some(Instant::now() + Duration::from_millis(100)));
        }
        if let Some(recap) = &self.recap {
            // Going away, or waiting on its summary.
            let until = recap.until();
            times.push(match recap.summary_at {
                None => until.map(|until| (Instant::now() + Duration::from_millis(250)).min(until)),
                Some(_) => until,
            });
        }
        if (self.scroll - self.scroll_target).abs() > 0.0 {
            times.push(Some(Instant::now() + Duration::from_millis(16)));
        }
        times.into_iter().flatten().min()
    }

    /// A short summary: the folder, then what's running and the last few
    /// distinct commands, newest first. Just the path in a fresh pane.
    pub fn title(&self) -> String {
        let recent = self.recent_commands();
        if recent.is_empty() {
            return self.folder();
        }
        format!("{} · {}", self.folder(), recent.join(", "))
    }

    /// The name of the folder it's in.
    pub fn folder(&self) -> String {
        folder_name(&self.session.cwd)
    }

    /// What's running, then the last few distinct commands run in the
    /// folder it's in now, newest first: after a `cd` the old folder's
    /// commands no longer describe it.
    pub fn recent_commands(&self) -> Vec<String> {
        let mut recent: Vec<String> = Vec::new();
        if let Some(block) = self.session.active() {
            recent.extend(block.command.lines().next().filter(|line| !line.trim().is_empty()).map(str::to_owned));
        }
        let active = self.session.active().map(|block| block.id);
        let finished =
            self.session.blocks.iter().rev().filter(|b| b.kind == BlockKind::Command && Some(b.id) != active);
        let here = finished.filter(|block| block.cwd == self.session.cwd);
        for gist in here.filter_map(|block| command_gist(&block.command)) {
            if recent.len() == TITLE_COMMANDS {
                break;
            }
            if !recent.iter().any(|seen| seen.starts_with(&gist)) {
                recent.push(gist);
            }
        }
        recent
    }

    /// Writes this pane's own history to `path` (commands) and `path.ai`.
    pub fn save_history(&self, path: &str) {
        self.ran.save_to(path);
        self.asked_here.save_to(&format!("{path}.ai"));
    }

    /// Reads what `save_history` wrote. With no file yet (saved by a build
    /// that kept one history for every pane), the commands of the pane's
    /// restored blocks stand in.
    pub fn restore_history(&mut self, path: &str) {
        let blocks = |kind: BlockKind| {
            let mut history = History::local();
            for block in self.session.blocks.iter().filter(|b| b.kind == kind) {
                history.push(&block.command);
            }
            history
        };
        self.ran = History::load_from(path).unwrap_or_else(|| blocks(BlockKind::Command));
        self.asked_here = History::load_from(&format!("{path}.ai")).unwrap_or_else(|| blocks(BlockKind::Ai));
    }

    /// Moves the pane; the grid follows now, finished blocks rewrap shortly.
    pub fn set_rect(&mut self, rect: Rect, text: &TextSystem) {
        let resized = rect.w != self.rect.w || rect.h != self.rect.h;
        self.rect = rect;
        if resized {
            self.session.resize(grid_size(rect, text));
            self.reflow_at = Some(Instant::now() + Duration::from_millis(90));
        }
    }

    /// After a font size change: new cells, new grid, rewrap everything.
    pub fn cells_changed(&mut self, text: &TextSystem) {
        self.session.set_cell_size(text.cell.width, text.cell.height);
        self.session.resize(grid_size(self.rect, text));
        self.session.reflow();
    }

    pub fn focus_changed(&self, focused: bool) {
        if let Some(block) = self.session.active() {
            if block.term.mode().contains(TermMode::FOCUS_IN_OUT) {
                self.session.reply(if focused { b"\x1b[I" } else { b"\x1b[O" });
            }
        }
    }

    pub fn hangup(&mut self) {
        self.session.hangup();
    }

    fn pin_to_bottom(&mut self) {
        self.scroll_target = 0.0;
    }

    fn max_scroll(&self) -> f32 {
        (self.layout.content_height - self.layout.list.h).max(0.0)
    }

    fn scroll_by(&mut self, delta: f32) {
        self.scroll_target = (self.scroll_target + delta).clamp(0.0, self.max_scroll());
    }

    // ---- keys ---------------------------------------------------------

    // ---- mouse (pane coordinates) -------------------------------------

    // ---- drawing ------------------------------------------------------
}

pub fn grid_size(rect: Rect, text: &TextSystem) -> GridSize {
    let cell = text.cell;
    GridSize {
        columns: (((rect.w - 2.0 * PAD_X) / cell.width).floor() as usize).max(10),
        lines: (((rect.h - 2.0 * ALT_PAD_Y) / cell.height).floor() as usize).max(3),
    }
}

/// A command boiled down for a title: the program and its first argument,
/// `cargo test` for `RUST_LOG=debug cargo test --release`. None for the
/// housekeeping (`cd`, `ls`, `clear`) that says nothing about the work.
fn command_gist(command: &str) -> Option<String> {
    const WRAPPERS: [&str; 6] = ["sudo", "time", "env", "nohup", "exec", "command"];
    const QUIET: [&str; 12] = ["cd", "ls", "ll", "la", "pwd", "clear", "reset", "cls", "exit", "z", "history", "echo"];
    let assignment = |word: &str| {
        word.split_once('=')
            .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    };
    let mut words = command
        .lines()
        .next()?
        .split_whitespace()
        .skip_while(|word| assignment(word) || WRAPPERS.contains(word))
        .take_while(|word| !matches!(*word, "|" | "||" | "&&" | ";" | "&"));
    let program = words.next()?.rsplit('/').next()?;
    if program.is_empty() || QUIET.contains(&program) {
        return None;
    }
    let argument = words
        .find(|word| !word.starts_with('-'))
        .map(|word| word.trim_matches(|c| c == '"' || c == '\'').trim_end_matches('/'))
        .and_then(|word| word.rsplit('/').next())
        .filter(|word| !word.is_empty());
    Some(match argument {
        Some(argument) if argument.chars().count() > 20 => {
            format!("{program} {}…", argument.chars().take(19).collect::<String>())
        }
        Some(argument) => format!("{program} {argument}"),
        None => program.to_owned(),
    })
}

/// The last part of a path, `~` at home.
/// How long ago, to the minute: "now", "3m", "2h", "5d".
fn ago(d: Duration) -> String {
    match d.as_secs() {
        0..60 => "now".to_owned(),
        s @ 60..3600 => format!("{}m", s / 60),
        s @ 3600..86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

fn folder_name(path: &str) -> String {
    let short = ui::short_path(path);
    match short.trim_end_matches('/').rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => short,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{ago, command_gist, command_rows};

    #[test]
    fn ago_rounds_to_the_minute() {
        let ago = |s| ago(Duration::from_secs(s));
        assert_eq!(ago(14), "now");
        assert_eq!(ago(190), "3m");
        assert_eq!(ago(7300), "2h");
        assert_eq!(ago(3 * 86400), "3d");
    }

    #[test]
    fn command_gist_keeps_program_and_first_argument() {
        assert_eq!(command_gist("RUST_LOG=debug cargo test --release").as_deref(), Some("cargo test"));
        assert_eq!(command_gist("sudo /usr/bin/vim src/app.rs").as_deref(), Some("vim app.rs"));
        assert_eq!(command_gist("make").as_deref(), Some("make"));
        assert_eq!(command_gist("git log | head").as_deref(), Some("git log"));
        assert_eq!(command_gist("cd ~/Developer"), None);
        assert_eq!(command_gist("   "), None);
    }

    #[test]
    fn command_rows_wrap_and_mark_line_ends() {
        let rows = command_rows("echo hello\n\nls", 4);
        let expected = [("echo", false), (" hel", false), ("lo", true), ("", true), ("ls", true)];
        assert_eq!(rows, expected.map(|(s, end)| (s.to_owned(), end)));
    }
}

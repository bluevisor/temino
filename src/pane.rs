//! One terminal: a shell session with its blocks, the command editor and
//! everything about how it's scrolled, selected and drawn. The window
//! (`app.rs`) arranges panes into tabs and splits; a pane draws and takes
//! the mouse in its own coordinates, as if it had the window to itself.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;

use crate::agent::{self, AgentState};
use crate::ask::{self, Reply};
use crate::complete::{self, Candidate, Commands, Completion, Kind};
use crate::editor::{Editor, History};
use crate::keys::{self, Named};
use crate::llm;
use crate::platform::{self, buttons, Cursor, KeyEvent};
use crate::scene::{Quad, Rect, Scene, LAYER_CONTENT, LAYER_OVERLAY};
use crate::session::{Block, BlockKind, Credits, GridSize, Link, Notice, Phase, Session};
use crate::text::{Style, TextSystem, UI, UI_BOLD};
use crate::theme::{Color, Theme, SURFACE_LIFT};
use crate::ui::{self, GridView, Span};
use crate::Msg;

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
/// How long a recap stays up, at most while its summary is on the way,
/// and how long it takes to fade.
const RECAP_SHOWN: Duration = Duration::from_secs(6);
const RECAP_WAIT: Duration = Duration::from_secs(20);
const RECAP_FADE: Duration = Duration::from_millis(400);
const RECAP_SYSTEM: &str = "Someone is coming back to a terminal tab after a break. From what ran there, \
say in one short sentence (at most 20 words) what they were doing and where it stands. \
Plain text, no preamble, no quotes.";
/// How much of a pane's recent history the summary is written from.
const RECAP_BLOCKS: usize = 5;
const RECAP_LINES: usize = 30;
const RECAP_CHARS: usize = 6000;
/// The launch banner, in figlet's "ANSI Shadow" style.
const BANNER: [&str; 6] = [
    "████████╗███████╗███╗   ███╗██╗███╗   ██╗ ██████╗ ",
    "╚══██╔══╝██╔════╝████╗ ████║██║████╗  ██║██╔═══██╗",
    "   ██║   █████╗  ██╔████╔██║██║██╔██╗ ██║██║   ██║",
    "   ██║   ██╔══╝  ██║╚██╔╝██║██║██║╚██╗██║██║   ██║",
    "   ██║   ███████╗██║ ╚═╝ ██║██║██║ ╚████║╚██████╔╝",
    "   ╚═╝   ╚══════╝╚═╝     ╚═╝╚═╝╚═╝  ╚═══╝ ╚═════╝ ",
];

/// What every pane shares: fonts, colors, history, the clock for the caret.
pub struct Ctx {
    pub text: TextSystem,
    pub theme: Theme,
    pub history: History,
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
}

impl Ctx {
    pub fn show_toast(&mut self, message: &str) {
        self.toast = Some((message.to_owned(), Instant::now() + Duration::from_millis(1600)));
    }

    pub fn blink_on(&self) -> bool {
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

/// A mouse selection inside one block's output (or its command, in
/// wrapped rows), in (line, column).
#[derive(Clone, Copy)]
struct Selection {
    block: u64,
    command: bool,
    anchor: (usize, usize),
    head: (usize, usize),
    mode: SelectMode,
}

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
    completion: Option<Completion>,
    /// The last completion hint worked out, keyed by cwd and editor text.
    hint: Option<(String, Option<String>)>,
    /// Where the pointer is over this pane, for underlining the link under it.
    pointer: Option<(f32, f32)>,
    /// A link pressed with a plain click: it opens if the button comes up
    /// without a drag.
    link_click: Option<String>,
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

impl Recap {
    fn summary(&self) -> Option<Option<String>> {
        self.summary.lock().unwrap().clone()
    }

    /// When it goes away: a while after it, or its summary, appeared.
    fn until(&self) -> Instant {
        match (self.summary_at, llm::enabled()) {
            (Some(at), _) => at.max(self.shown) + RECAP_SHOWN,
            (None, true) => self.shown + RECAP_WAIT,
            (None, false) => self.shown + RECAP_SHOWN,
        }
    }
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
    /// Prompts waiting for the current turn to end.
    pub queued: usize,
    pub name: Option<String>,
    /// How long the current turn has run, or the last one took.
    pub turn: Option<Duration>,
    /// How long the agent has been running.
    pub uptime: Duration,
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
            bar_grab: None,
            credits,
            rect,
            welcome,
            scroll: 0.0,
            scroll_target: 0.0,
            selected: None,
            selection: None,
            dragging: false,
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
            program_now: None,
            agent_now: None,
            needs_since: None,
            git_changes: None,
            git_asked: None,
            watcher: None,
            typed: String::new(),
            typed_prompt: None,
            turn: None,
            last_turn: None,
            left_at: None,
            recap: None,
            ai: false,
            asking: None,
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
            Notice::Notify => self.notified_at = Some(Instant::now()),
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

    /// What the coding agent running here is doing, if one is.
    pub fn agent(&mut self, now: Instant) -> Option<AgentState> {
        let running = self.session.active().filter(|block| block.kind == BlockKind::Command);
        let is_agent = running.is_some_and(|block| {
            self.agent_block == Some(block.id)
                || agent::is_agent_command(&block.command)
                || agent::is_agent_title(&self.program_title)
        });
        let block = running.filter(|_| is_agent).map(|block| block.id);
        if block != self.agent_block {
            self.agent_program = None;
        }
        self.agent_block = block;
        if !is_agent {
            self.agent_was = None;
            self.agent_unseen = false;
            self.screen = None;
            self.watcher = None;
            self.typed.clear();
            self.typed_prompt = None;
            self.turn = None;
            self.last_turn = None;
            return None;
        }
        if self.agent_program.is_none() {
            self.agent_program = running
                .and_then(|block| agent::agent_program(&block.command))
                .or_else(|| agent::title_program(&self.program_title));
        }
        if let Some(group) = self.session.foreground_group() {
            self.watcher.get_or_insert_with(agent::Watcher::new).set_group(group);
        }
        if self.screen.is_none_or(|(at, _)| now.duration_since(at) >= AGENT_SCAN) {
            self.screen = Some((now, self.screen_tail().as_deref().and_then(agent::screen_state)));
        }
        let screen = self.screen.and_then(|(_, state)| state);
        let title = agent::title_state(&self.program_title);
        // Claude Code's own record of whether it's mid-turn, for when a
        // long tool call leaves the screen and title quiet.
        let busy = self.watcher.as_ref().and_then(agent::Watcher::report).is_some_and(|report| report.busy);
        let input = self.session.input_at();
        let notified = self.notified_at.is_some_and(|at| input.is_none_or(|input| at > input));
        // Output that isn't just the echo of your typing.
        let printing = self.session.output_at.is_some_and(|at| {
            now.duration_since(at) < AGENT_QUIET && input.is_none_or(|input| at.duration_since(input) > AGENT_ECHO)
        });
        let state = if notified
            || title == Some(AgentState::Waiting)
            || (screen == Some(AgentState::Waiting) && title != Some(AgentState::Running))
        {
            AgentState::Waiting
        } else if title == Some(AgentState::Running)
            || self.progress
            || screen == Some(AgentState::Running)
            || printing
            || busy
        {
            AgentState::Running
        } else {
            AgentState::Idle
        };
        match state {
            AgentState::Idle if self.agent_was == Some(AgentState::Running) => self.agent_unseen = true,
            AgentState::Idle => {}
            _ => self.agent_unseen = false,
        }
        if input.is_some_and(|input| now.duration_since(input) < AGENT_ECHO) {
            self.agent_unseen = false;
        }
        // A turn runs from when it starts working until it's back at rest;
        // stopping to ask you something is part of it.
        if state == AgentState::Idle {
            if let Some(start) = self.turn.take() {
                self.last_turn = Some(now.duration_since(start));
            }
        } else if self.turn.is_none() {
            self.turn = Some(now);
        }
        self.agent_was = Some(state);
        Some(if state == AgentState::Idle && self.agent_unseen { AgentState::Done } else { state })
    }

    /// What the header over the agent here shows, as of the last `agent()`.
    pub fn agent_info(&self, now: Instant) -> Option<AgentInfo> {
        let state = self.agent_now?;
        let report = self.watcher.as_ref().and_then(agent::Watcher::report).unwrap_or_default();
        let started = self.session.active().map_or(now, |block| block.started);
        Some(AgentInfo {
            program: self.agent_program,
            state,
            prompt: report.prompt.or_else(|| self.typed_prompt.clone()),
            subagents: report.subagents,
            queued: report.queued,
            name: report.name,
            turn: self.turn.map(|start| now.duration_since(start)).or(self.last_turn),
            uptime: now.duration_since(started),
        })
    }

    pub fn focus_lost(&mut self, now: Instant) {
        self.left_at = Some(now);
    }

    /// Coming back to the pane: after `idle` or longer since the user last
    /// did anything in it (output doesn't count: an agent works while
    /// they're away), or when asked (`idle` None), a recap shows. `mark`
    /// is what the tab's mark adds to it.
    pub fn focus_gained(&mut self, now: Instant, idle: Option<Duration>, mark: Option<MarkNote>) {
        let last = [self.left_at, self.session.input_at()].into_iter().flatten().max();
        let left = self.left_at.take();
        let away = last.map_or(Duration::ZERO, |last| now.duration_since(last));
        let due = match idle {
            None => true,
            Some(idle) => left.is_some() && !idle.is_zero() && away >= idle,
        };
        if due {
            self.show_recap(now, away, mark);
        }
    }

    fn show_recap(&mut self, now: Instant, away: Duration, mark: Option<MarkNote>) {
        let mut facts = Vec::new();
        if away >= Duration::from_secs(60) {
            facts.push(format!("↩ away {}", ui::short_duration(away)));
        } else {
            facts.push("↩".to_owned());
        }
        if let Some(mark) = &mark {
            facts.push(mark.fact.clone());
        }
        let finished =
            self.session.blocks.iter().rev().find(|block| block.kind == BlockKind::Command && !block.running());
        match (self.session.active(), finished) {
            (Some(block), _) => facts.push(format!("running: {}", agent::one_line(&block.command))),
            (None, Some(block)) => {
                let status = match block.exit {
                    Some(0) | None => "✓".to_owned(),
                    Some(code) => format!("✗ exit {code}"),
                };
                let ago = block.finished.map(|at| format!(" · {} ago", ui::short_duration(now.duration_since(at))));
                let command = agent::one_line(&block.command);
                let command = if command.is_empty() { "command".to_owned() } else { command };
                facts.push(format!("last: {command} {status}{}", ago.unwrap_or_default()));
            }
            (None, None) => {}
        }
        let place = ui::short_path(&self.session.cwd);
        facts.push(match &self.session.branch {
            Some(branch) => format!("{place} ({branch})"),
            None => place,
        });
        if let Some(info) = self.agent_info(now) {
            facts.push(format!("agent {}", info.state.label().to_lowercase()));
        }
        let summary = Arc::new(Mutex::new(None));
        if llm::enabled() {
            let mut context = self.recap_context(now);
            if let Some(mark) = &mark {
                context = format!("{}\n{context}", mark.context);
            }
            let theirs = summary.clone();
            let _ = std::thread::Builder::new().name("recap".into()).spawn(move || {
                let answer = llm::complete(RECAP_SYSTEM, &context, 120).ok().filter(|answer| !answer.is_empty());
                *theirs.lock().unwrap() = Some(answer.map(|answer| agent::one_line(&answer)));
            });
        } else {
            *summary.lock().unwrap() = Some(None);
        }
        self.recap = Some(Recap { shown: now, facts: facts.join(" · "), summary, summary_at: None });
    }

    /// What the summary is written from: the folder, the agent's last
    /// prompt, and the last few commands with how they ended and the end
    /// of their output, newest last.
    fn recap_context(&mut self, now: Instant) -> String {
        let mut parts = vec![format!("Folder: {}", self.session.cwd)];
        if let Some(branch) = &self.session.branch {
            parts.push(format!("Git branch: {branch}"));
        }
        if let Some(info) = self.agent_info(now) {
            parts.push(format!("A coding agent runs here, {}.", info.state.label().to_lowercase()));
            if let Some(prompt) = info.prompt {
                parts.push(format!("Last prompt to the agent: {prompt}"));
            }
        }
        let first = self.session.blocks.len().saturating_sub(RECAP_BLOCKS);
        let mut blocks = Vec::new();
        for block in self.session.blocks[first..].iter_mut().filter(|block| block.kind == BlockKind::Command) {
            let status = match (block.running(), block.exit) {
                (true, _) => "still running".to_owned(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "finished".to_owned(),
            };
            let lines = block.output_lines();
            let output = if lines == 0 {
                String::new()
            } else {
                block.text_between((lines.saturating_sub(RECAP_LINES), 0), (lines - 1, usize::MAX))
            };
            let output: String =
                output.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
            blocks.push(format!("$ {} ({status})\n{output}", block.command.trim()));
        }
        parts.push(blocks.join("\n\n"));
        let context = parts.join("\n");
        // Keep the end: it's the most recent.
        let skip = context.chars().count().saturating_sub(RECAP_CHARS);
        context.chars().skip(skip).collect()
    }

    /// The recap card, over the bottom of the output (the top of a
    /// full-screen program), fading out at the end.
    fn draw_recap(&mut self, scene: &mut Scene, ctx: &mut Ctx, alt: bool) {
        let Some(recap) = &self.recap else { return };
        let now = Instant::now();
        let fade = (recap.until().saturating_duration_since(now).as_secs_f32() / RECAP_FADE.as_secs_f32()).min(1.0);
        let appear = (now.duration_since(recap.shown).as_secs_f32() / 0.18).min(1.0);
        let alpha = fade.min(appear);
        let theme = &ctx.theme;
        let text = &mut ctx.text;
        let width = (self.rect.w - 2.0 * PAD_X).max(0.0);
        let (font, line) = (12.5, 20.0);
        let height = 2.0 * line + 14.0;
        let y = if alt { ALT_PAD_Y } else { (self.layout.list.h - height - 8.0).max(4.0) };
        let card = Rect::new(PAD_X, y, width, height);
        scene.set_layer(LAYER_OVERLAY);
        scene.shadow(card, Color::hex(0x000000).alpha(0.45 * alpha), 8.0, 8.0);
        // The theme's raised surface is a light wash over what's under it:
        // put the page color down first so the output doesn't show through.
        scene.quad(Quad::new(card, theme.background.alpha(alpha)).radius(8.0));
        scene.quad(Quad::new(card, theme.raised.alpha(alpha)).radius(8.0).border(1.0, theme.divider.alpha(alpha)));
        let ascent = text.ui_ascent(font);
        let room = width - 28.0;
        let first = card.y + 7.0 + (line + ascent * 0.72) / 2.0;
        let facts = text.elide(&recap.facts, UI, font, room);
        text.draw_text(scene, &facts, card.x + 14.0, first.round(), UI, font, theme.text.alpha(alpha));
        let (summary, ink) = match recap.summary() {
            None => ("✦ Summarizing…".to_owned(), theme.text_faint),
            Some(Some(summary)) => (format!("✦ {summary}"), theme.text_dim),
            Some(None) => ("✦ No summary: the model server didn't answer".to_owned(), theme.text_faint),
        };
        let summary = text.elide(&summary, UI, font, room);
        text.draw_text(scene, &summary, card.x + 14.0, (first + line).round(), UI, font, ink.alpha(alpha));
        scene.set_layer(LAYER_CONTENT);
    }

    /// One line on what the pane is doing: the running command, or the
    /// folder at rest; and whether something runs.
    pub fn activity(&self) -> (bool, String) {
        let running = self.session.active().map(|block| agent::one_line(&block.command));
        match running {
            Some(command) if !command.is_empty() => (true, command),
            Some(_) if !self.program_title.is_empty() => (true, agent::one_line(&self.program_title)),
            running => (running.is_some(), ui::short_path(&self.session.cwd)),
        }
    }

    /// A line on an idle shell for the extended sidebar: its folder when
    /// not `base` (the tab's), the last command, how it ended and how long
    /// ago, and the git branch with its count of changed files.
    pub fn idle_line(&self, base: &str, now: Instant) -> String {
        let mut parts = Vec::new();
        let cwd = &self.session.cwd;
        if cwd != base {
            parts.push(match cwd.strip_prefix(base).filter(|rest| rest.starts_with('/')) {
                Some(rest) => format!(".{rest}"),
                None => ui::short_path(cwd),
            });
        }
        match self.session.last_finished() {
            Some(block) => {
                let mark = if block.failed() { "✗" } else { "✓" };
                let mut last = format!("{mark} {}", agent::one_line(&block.command));
                if let Some(code) = block.exit.filter(|&code| code != 0) {
                    last.push_str(&format!(" (exit {code})"));
                }
                if let Some(at) = block.finished {
                    last.push_str(&format!(" · {}", ago(now.saturating_duration_since(at))));
                }
                parts.push(last);
            }
            None if parts.is_empty() => parts.push(format!("shell · {}", self.session.shell_name)),
            None => {}
        }
        if let Some(branch) = &self.session.branch {
            match self.git_changes.as_ref().filter(|(dir, _)| dir == cwd) {
                Some((_, n)) if *n > 0 => parts.push(format!("{branch} ●{n}")),
                _ => parts.push(branch.clone()),
            }
        }
        parts.join(" · ")
    }

    /// Whether the last command failed, for the sidebar's red dot.
    pub fn last_failed(&self) -> bool {
        self.session.last_finished().is_some_and(Block::failed)
    }

    /// Asks git, off the main thread, how many files changed in this idle
    /// pane's repository: again after each command, a new folder, or a while.
    pub fn refresh_git(&mut self, sender: &Sender<Msg>, now: Instant) {
        if self.session.branch.is_none() || self.session.active().is_some() {
            return;
        }
        let cwd = self.session.cwd.clone();
        let last = self.session.last_finished().map(|block| block.id);
        let fresh = |(dir, block, at): &(String, Option<u64>, Instant)| {
            *dir == cwd && *block == last && now.saturating_duration_since(*at) < GIT_RECHECK
        };
        if self.git_asked.as_ref().is_some_and(fresh) {
            return;
        }
        self.git_asked = Some((cwd.clone(), last, now));
        let (id, sender, path) = (self.id, sender.clone(), self.session.path.clone());
        std::thread::spawn(move || {
            let mut git = std::process::Command::new("git");
            git.args(["-C", &cwd, "status", "--porcelain", "--untracked-files=normal"]);
            if let Some(path) = path {
                git.env("PATH", path);
            }
            let Ok(output) = git.stderr(std::process::Stdio::null()).output() else { return };
            if output.status.success() {
                let changed = output.stdout.split(|&b| b == b'\n').filter(|line| !line.is_empty()).count();
                let _ = sender.send(Msg::Git(id, cwd, changed));
            }
        });
    }

    /// Follows what's typed to an agent, to know its prompt when it keeps
    /// no records we read. Answers to its questions aren't prompts.
    fn track_typing(&mut self, text: &str) {
        if self.agent_block.is_none() || self.agent_was == Some(AgentState::Waiting) {
            self.typed.clear();
            return;
        }
        for c in text.chars() {
            match c {
                '\r' => {
                    let line = agent::one_line(&self.typed);
                    if !line.is_empty() {
                        self.typed_prompt = Some(line);
                    }
                    self.typed.clear();
                }
                '\u{7f}' | '\u{8}' => {
                    self.typed.pop();
                }
                // Control+C or Control+U: the line is gone.
                '\u{3}' | '\u{15}' => self.typed.clear(),
                // Arrows and the like: we can't follow the line any more.
                '\u{1b}' => return,
                c if c.is_control() => {}
                c => self.typed.push(c),
            }
        }
    }

    /// The last few lines the running program left on screen.
    fn screen_tail(&mut self) -> Option<String> {
        let block = self.session.active_mut()?;
        let lines = block.output_lines();
        if lines == 0 {
            return None;
        }
        Some(block.text_between((lines.saturating_sub(AGENT_SCAN_LINES), 0), (lines - 1, usize::MAX)))
    }

    /// Advances animations; true when the pane needs a new frame.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut dirty = self.session.tick(now) | self.take_answer();
        if let Some(recap) = &mut self.recap {
            if recap.summary_at.is_none() && recap.summary().is_some() {
                recap.summary_at = Some(now);
                dirty = true;
            }
            if now >= recap.until() {
                self.recap = None;
                dirty = true;
            } else if now + RECAP_FADE >= recap.until() {
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
        if focused && (self.session.editor_active() || self.session.active().is_some()) {
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
            // Fading, or waiting on its summary.
            let until = recap.until();
            let now = Instant::now();
            times.push(Some(match recap.summary_at {
                None => (now + Duration::from_millis(250)).min(until),
                Some(_) if now + RECAP_FADE >= until => now + Duration::from_millis(33),
                Some(_) => until - RECAP_FADE,
            }));
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

    pub fn key(&mut self, ctx: &mut Ctx, key: KeyEvent) {
        if !key.cmd() {
            self.recap = None;
        }
        let named = keys::named(&key);
        // At the prompt, ⌘←/→ and ⌘⌫ are line editing (see editor_key).
        let line_edit = key.line_mod()
            && self.session.editor_active()
            && matches!(named, Some(Named::Left | Named::Right | Named::Backspace));
        // Command shortcuts work in every mode.
        if key.cmd() && !key.ctrl() && !line_edit {
            let handled = match (named, key.base_char()) {
                (Some(Named::Up), _) => {
                    self.select_neighbor(-1);
                    true
                }
                (Some(Named::Down), _) => {
                    self.select_neighbor(1);
                    true
                }
                (_, Some('c')) => {
                    self.copy(ctx, key.shift());
                    true
                }
                (_, Some('v')) => {
                    self.paste(platform::clipboard_get());
                    true
                }
                (_, Some('k')) => {
                    self.clear_blocks();
                    true
                }
                (_, Some('a')) if self.session.editor_active() => {
                    self.editor.select_all();
                    true
                }
                (_, Some('z')) if self.session.editor_active() => {
                    self.editor.undo();
                    true
                }
                _ => false,
            };
            // Off Haiku, Command is never Meta: an unbound shortcut does nothing.
            if handled || !cfg!(target_os = "haiku") {
                return;
            }
        }

        if let Some(block) = self.session.active() {
            let alt = block.alt_screen();
            let mode = *block.term.mode();
            if !alt && key.shift() && matches!(named, Some(Named::PageUp | Named::PageDown)) {
                let page = self.layout.list.h * 0.8;
                self.scroll_by(if named == Some(Named::PageUp) { page } else { -page });
                return;
            }
            let bytes = keys::encode(&key, mode);
            if !bytes.is_empty() {
                self.track_typing(&String::from_utf8_lossy(&bytes));
                self.session.write(&bytes);
                self.selection = None;
                self.pin_to_bottom();
            }
            return;
        }

        match self.session.phase {
            Phase::Ready | Phase::Continuation => self.editor_key(ctx, key, named),
            Phase::Exited => {
                if named == Some(Named::Enter) {
                    self.restart(ctx);
                }
            }
            _ => {
                if key.ctrl() && key.base_char() == Some('c') {
                    self.session.interrupt();
                }
            }
        }
    }

    fn restart(&mut self, ctx: &mut Ctx) {
        self.session = Session::new(grid_size(self.rect, &ctx.text));
        self.session.set_cell_size(ctx.text.cell.width, ctx.text.cell.height);
        self.credits = Credits::new();
        if let Err(error) = self.session.start(self.id, &ctx.shell, ctx.sender.clone(), self.credits.clone()) {
            ctx.show_toast(&format!("Couldn't start the shell: {error}"));
        }
        self.selection = None;
        self.selected = None;
    }

    fn editor_key(&mut self, ctx: &mut Ctx, key: KeyEvent, named: Option<Named>) {
        let shift = key.shift();
        if self.completion.is_some() {
            match named {
                Some(Named::Up) => return self.move_menu(-1),
                Some(Named::Down) => return self.move_menu(1),
                Some(Named::Tab) => return self.move_menu(if shift { -1 } else { 1 }),
                Some(Named::Enter) => return self.accept_completion(),
                Some(Named::Escape) => {
                    self.completion = None;
                    return;
                }
                Some(Named::Left | Named::Right | Named::Home | Named::End) => self.completion = None,
                _ => {}
            }
        }
        // Which menu was open before this key, so typing can refilter it.
        let open_menu = self.completion.as_ref().and_then(|c| c.candidates.first()).map(|f| f.kind == Kind::History);
        let suggestion = self.suggestion(ctx);
        let word = key.ctrl() || key.word_mod();
        let line = key.line_mod();
        match named {
            Some(Named::Enter) => {
                if shift || key.option() {
                    self.editor.insert("\n");
                } else if self.ai {
                    self.ask(ctx);
                } else {
                    self.submit(ctx);
                }
            }
            Some(Named::Backspace) => {
                if line {
                    self.editor.kill_to_start()
                } else if word {
                    self.editor.delete_word_back()
                } else {
                    self.editor.backspace()
                }
            }
            Some(Named::Delete) => self.editor.delete(),
            Some(Named::Left) if line => self.editor.home(shift),
            Some(Named::Left) => self.editor.left(word, shift),
            // Like End: at the end of the line it takes the whole suggestion.
            Some(Named::Right) if line => match suggestion {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.editor.end(shift),
            },
            Some(Named::Right) => match suggestion {
                Some(rest) if !shift => {
                    let take = if word { rest.split_inclusive(' ').next().unwrap_or(&rest).to_owned() } else { rest };
                    self.editor.insert(&take);
                }
                _ => self.editor.right(word, shift),
            },
            Some(Named::Home) => self.editor.home(shift),
            Some(Named::End) => match suggestion {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.editor.end(shift),
            },
            Some(Named::Up) => {
                if !self.editor.vertical(false, shift) {
                    self.editor.browse(&ctx.history, true);
                }
            }
            Some(Named::Down) => {
                if !self.editor.vertical(true, shift) {
                    self.editor.browse(&ctx.history, false);
                }
            }
            // Tab on an empty line switches between the shell and AI mode;
            // after text it takes a grey history suggestion whole, or completes.
            Some(Named::Tab) if self.editor.text.is_empty() && self.session.phase == Phase::Ready => {
                self.ai = !self.ai;
                self.hint = None;
            }
            Some(Named::Tab) if self.ai => {}
            Some(Named::Tab) => match self.editor.suggestion(&ctx.history).map(str::to_owned) {
                Some(rest) if !shift => self.editor.insert(&rest),
                _ => self.tab(ctx),
            },
            Some(Named::Escape) if self.ai && self.editor.text.is_empty() => self.ai = false,
            Some(Named::Escape) => {
                self.selected = None;
                self.selection = None;
                self.editor.anchor = None;
            }
            Some(Named::PageUp) => self.scroll_by(self.layout.list.h * 0.8),
            Some(Named::PageDown) => self.scroll_by(-self.layout.list.h * 0.8),
            Some(Named::Insert | Named::F(_)) => {}
            None => {
                if key.ctrl() {
                    match key.base_char() {
                        Some('c') => {
                            if self.session.phase == Phase::Continuation {
                                self.session.interrupt();
                            }
                            self.editor.clear();
                            self.completion = None;
                        }
                        Some('a') => self.editor.home(shift),
                        Some('e') => match suggestion {
                            Some(rest) => self.editor.insert(&rest),
                            None => self.editor.end(shift),
                        },
                        Some('u') => self.editor.kill_to_start(),
                        Some('k') => self.editor.kill_to_end(),
                        Some('w') => self.editor.delete_word_back(),
                        Some('l') => self.clear_blocks(),
                        Some('z') => self.editor.undo(),
                        Some('r') => self.open_history_menu(ctx),
                        // Every command, or what completes the word: Tab's
                        // menu, now that Tab on an empty line means AI mode.
                        Some(' ') if shift && !self.ai => self.tab(ctx),
                        Some('d') => {
                            if self.editor.text.is_empty() {
                                self.session.submit("exit");
                            } else {
                                self.editor.delete();
                            }
                        }
                        _ => {}
                    }
                } else if key.cmd() {
                    // Unbound Command combinations: nothing at the prompt.
                } else if let Ok(text) = std::str::from_utf8(&key.bytes) {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        self.editor.insert(text);
                        self.selected = None;
                    }
                }
            }
        }
        // Keep an open menu in step with what was typed.
        let edited = matches!(named, None | Some(Named::Backspace | Named::Delete)) && !key.ctrl();
        if let (Some(history), true) = (open_menu, edited && self.completion.is_some()) {
            if history {
                self.open_history_menu(ctx);
            } else {
                ctx.commands.use_path(self.session.path.as_deref());
                self.completion =
                    complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut ctx.commands)
                        .filter(|c| c.candidates.len() > 1);
            }
        }
    }

    /// The grey text after the caret: the newest history entry that starts
    /// with what's typed, or else what Tab would fill in for the last word.
    fn suggestion(&mut self, ctx: &mut Ctx) -> Option<String> {
        if self.ai {
            return None;
        }
        if let Some(rest) = self.editor.suggestion(&ctx.history) {
            return Some(rest.to_owned());
        }
        let text = &self.editor.text;
        if text.trim().is_empty()
            || text.ends_with(char::is_whitespace)
            || self.editor.cursor != text.len()
            || text.contains('\n')
        {
            return None;
        }
        let key = format!("{}\0{text}", self.session.cwd);
        if let Some((cached, hint)) = &self.hint {
            if *cached == key {
                return hint.clone();
            }
        }
        ctx.commands.use_path(self.session.path.as_deref());
        let hint = complete::hint(text, &self.session.cwd, &mut ctx.commands);
        self.hint = Some((key, hint.clone()));
        hint
    }

    fn submit(&mut self, ctx: &mut Ctx) {
        let command = self.editor.text.trim_end().to_owned();
        self.completion = None;
        if command.trim().is_empty() {
            return;
        }
        ctx.history.push(&command);
        self.hint = None;
        self.editor.take();
        self.session.submit(&command);
        self.selected = None;
        self.selection = None;
        self.pin_to_bottom();
    }

    /// AI mode's Enter: the request goes to the model in a block of its own.
    fn ask(&mut self, ctx: &mut Ctx) {
        let request = self.editor.text.trim().to_owned();
        self.completion = None;
        if request.is_empty() || self.asking.is_some() {
            return;
        }
        self.editor.take();
        let id = self.session.ask(&request);
        self.selected = None;
        self.selection = None;
        self.pin_to_bottom();
        if !llm::enabled() {
            self.session.answer(id, "AI mode needs a model server: set llm_url in the settings file.", false);
            return;
        }
        let recent: Vec<String> = self
            .session
            .blocks
            .iter()
            .rev()
            .filter(|b| b.kind == BlockKind::Command && !b.command.trim().is_empty())
            .take(3)
            .map(|b| agent::one_line(&b.command))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let prompt = ask::prompt(&request, &ctx.shell, &self.session.cwd, &recent);
        let reply = Arc::new(Mutex::new(None));
        let theirs = reply.clone();
        let _ = std::thread::Builder::new().name("ask".into()).spawn(move || {
            *theirs.lock().unwrap() = Some(ask::run(&request, &prompt));
        });
        self.asking = Some((id, reply));
    }

    /// Shows the model's reply once it's in: a command goes into the editor
    /// to look over (Enter runs it, back in shell mode), an answer into its block.
    fn take_answer(&mut self) -> bool {
        let Some((id, reply)) = &self.asking else { return false };
        let Some(reply) = reply.lock().unwrap().take() else { return false };
        let id = *id;
        self.asking = None;
        match reply {
            Ok(Reply::Command(command)) => {
                let placed = self.editor.text.is_empty();
                let note = if placed { "Enter runs it, or edit it first" } else { "copy it from here" };
                self.session.answer(id, &format!("{command}\n\n{note}"), true);
                if placed {
                    self.editor.insert(&command);
                    self.ai = false;
                    self.hint = None;
                }
            }
            Ok(Reply::Answer(answer)) if !answer.is_empty() => self.session.answer(id, &answer, true),
            Ok(Reply::Answer(_)) => self.session.answer(id, "The model gave no answer.", false),
            Err(error) => self.session.answer(id, &format!("The model couldn't be reached: {error}"), false),
        }
        self.pin_to_bottom();
        true
    }

    fn tab(&mut self, ctx: &mut Ctx) {
        ctx.commands.use_path(self.session.path.as_deref());
        let Some(mut completion) =
            complete::complete(&self.editor.text, self.editor.cursor, &self.session.cwd, &mut ctx.commands)
        else {
            platform::beep();
            return;
        };
        if completion.candidates.len() == 1 {
            let only = completion.candidates.remove(0);
            self.replace_word(completion.start, completion.end, &only.replacement);
            return;
        }
        let prefix = complete::common_prefix(&completion.candidates);
        let current = &self.editor.text[completion.start..completion.end];
        if prefix.len() > current.len() {
            self.replace_word(completion.start, completion.end, &prefix);
            completion.end = completion.start + prefix.len();
        }
        self.completion = Some(completion);
    }

    fn open_history_menu(&mut self, ctx: &Ctx) {
        let query = self.editor.text.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let candidates: Vec<Candidate> = ctx
            .history
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.to_lowercase().contains(&query) && seen.insert(entry.as_str()))
            .take(200)
            .map(|entry| Candidate {
                replacement: entry.clone(),
                label: entry.replace('\n', " ⏎ "),
                kind: Kind::History,
            })
            .collect();
        self.completion = (!candidates.is_empty()).then_some(Completion {
            start: 0,
            end: self.editor.text.len(),
            candidates,
            selected: 0,
        });
    }

    fn replace_word(&mut self, start: usize, end: usize, with: &str) {
        self.editor.anchor = Some(start);
        self.editor.cursor = end;
        self.editor.insert(with);
    }

    fn move_menu(&mut self, delta: i32) {
        if let Some(completion) = &mut self.completion {
            let n = completion.candidates.len() as i32;
            completion.selected = ((completion.selected as i32 + delta).rem_euclid(n)) as usize;
        }
    }

    fn accept_completion(&mut self) {
        if let Some(completion) = self.completion.take() {
            let candidate = &completion.candidates[completion.selected];
            if candidate.kind == Kind::History {
                self.editor.set(&candidate.replacement);
            } else {
                let end = completion.end.min(self.editor.text.len());
                self.replace_word(completion.start, end, &candidate.replacement);
            }
        }
    }

    fn clear_blocks(&mut self) {
        self.session.clear();
        self.selected = None;
        self.selection = None;
        self.scroll = 0.0;
        self.scroll_target = 0.0;
        self.welcome = false;
    }

    fn select_neighbor(&mut self, direction: i32) {
        let visible: Vec<u64> = self.session.blocks.iter().map(|b| b.id).collect();
        if visible.is_empty() {
            return;
        }
        let index = match self.selected.and_then(|id| visible.iter().position(|&v| v == id)) {
            Some(i) => (i as i32 + direction).clamp(0, visible.len() as i32 - 1) as usize,
            None if direction < 0 => visible.len() - 1,
            None => return,
        };
        self.selected = Some(visible[index]);
        self.scroll_into_view(visible[index]);
    }

    fn scroll_into_view(&mut self, id: u64) {
        let Some(layout) = self.layout.blocks.iter().find(|b| b.id == id) else {
            // Off screen: lay out happens in render; approximate by index.
            return;
        };
        let list = self.layout.list;
        if layout.rect.y < list.y {
            self.scroll_by(list.y - layout.rect.y + 8.0);
        } else if layout.rect.bottom() > list.bottom() {
            self.scroll_by(-(layout.rect.bottom() - list.bottom()).min(layout.rect.y - list.y));
        }
    }

    /// The text under the mouse selection, if any.
    fn selection_text(&self) -> Option<String> {
        let selection = self.selection?;
        let block = self.session.blocks.iter().find(|b| b.id == selection.block)?;
        let text = if selection.command {
            let rows = command_rows(&block.command, self.session.size.columns.max(1));
            let ((l0, c0), (l1, c1)) = self.command_span(block.id)?;
            let mut text = String::new();
            for (index, (row, ends_line)) in rows.iter().enumerate().take(l1 + 1).skip(l0) {
                let chars: Vec<char> = row.chars().collect();
                let from = if index == l0 { c0.min(chars.len()) } else { 0 };
                let to = if index == l1 { (c1 + 1).min(chars.len()) } else { chars.len() };
                text.extend(&chars[from..to.max(from)]);
                if *ends_line && index < l1 {
                    text.push('\n');
                }
            }
            text
        } else {
            let (start, end) = self.selection_span(block.id).unwrap_or_else(|| normalize(selection));
            block.text_between(start, end)
        };
        Some(text).filter(|text| !text.is_empty())
    }

    fn copy(&mut self, ctx: &mut Ctx, command_only: bool) {
        let text = if let Some(text) = self.editor.selected_text().filter(|_| self.session.editor_active()) {
            Some(text.to_owned())
        } else if self.selection.is_some() {
            self.selection_text()
        } else if let Some(id) = self.selected {
            self.session.blocks.iter_mut().find(|b| b.id == id).map(|block| {
                if command_only {
                    block.command.clone()
                } else {
                    block.output_text()
                }
            })
        } else {
            None
        };
        match text {
            Some(text) if !text.is_empty() => {
                platform::clipboard_set(&text);
                let lines = text.lines().count();
                ctx.show_toast(&if lines > 1 { format!("Copied {lines} lines") } else { "Copied".to_owned() });
            }
            _ => platform::beep(),
        }
    }

    /// The pane menu's Copy, Paste and Clear.
    pub fn copy_selection(&mut self, ctx: &mut Ctx) {
        self.copy(ctx, false);
    }

    pub fn paste_clipboard(&mut self) {
        self.paste(platform::clipboard_get());
    }

    pub fn clear(&mut self) {
        self.clear_blocks();
    }

    /// Whether the full-screen program here takes mouse clicks itself.
    pub fn reports_mouse(&self) -> bool {
        self.layout.alt.is_some_and(|(index, _, _)| {
            self.session.blocks.get(index).is_some_and(|block| block.term.mode().intersects(TermMode::MOUSE_MODE))
        })
    }

    /// Text an input method committed: typed, not pasted.
    pub fn text_input(&mut self, text: &str) {
        self.recap = None;
        if self.session.active().is_some() {
            self.track_typing(text);
            self.session.write(text.as_bytes());
            self.selection = None;
            self.pin_to_bottom();
        } else if self.session.editor_active() {
            self.editor.insert(text);
            self.selected = None;
        }
    }

    fn paste(&mut self, text: Option<String>) {
        if let Some(block) = self.session.active() {
            let bracketed = block.term.mode().contains(TermMode::BRACKETED_PASTE);
            if bracketed && text.as_deref().is_none_or(str::is_empty) {
                // No text: likely an image. An empty paste still tells the
                // program to look, and Claude Code then reads the image
                // from the clipboard itself.
                self.session.write(b"\x1b[200~\x1b[201~");
                return;
            }
        }
        let Some(text) = text else { return };
        let text = text.replace("\r\n", "\n");
        if let Some(block) = self.session.active() {
            let bracketed = block.term.mode().contains(TermMode::BRACKETED_PASTE);
            let body = text.replace('\n', "\r");
            self.track_typing(&text.replace('\n', " "));
            if bracketed {
                self.session.write(format!("\x1b[200~{}\x1b[201~", body.replace("\x1b[201~", "")).as_bytes());
            } else {
                self.session.write(body.as_bytes());
            }
        } else if self.session.editor_active() {
            self.editor.insert(&text);
        }
    }

    // ---- mouse (pane coordinates) -------------------------------------

    pub fn wheel(&mut self, ctx: &Ctx, dy: f32, precise: bool, x: f32, y: f32) {
        if let Some(block) = self.session.active() {
            if block.alt_screen() {
                let mode = *block.term.mode();
                // A trackpad steps the program once a line of travel (a
                // notch is three), so it starts moving at once; a wheel
                // steps once a notch. Whole steps only: trackpads send
                // fractions, and a turn back drops what was left over.
                let steps = if precise { dy * 3.0 } else { dy };
                if steps * self.wheel_rest < 0.0 {
                    self.wheel_rest = 0.0;
                }
                self.wheel_rest += steps;
                let lines = self.wheel_rest.trunc();
                self.wheel_rest -= lines;
                if lines == 0.0 {
                    return;
                }
                if mode.intersects(TermMode::MOUSE_MODE) {
                    if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                        let button = if lines < 0.0 { 64 } else { 65 };
                        for _ in 0..lines.abs() as usize {
                            self.mouse_report(button, column, line, true);
                        }
                    }
                } else if mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let arrow: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                        if lines < 0.0 {
                            b"\x1bOA"
                        } else {
                            b"\x1bOB"
                        }
                    } else if lines < 0.0 {
                        b"\x1b[A"
                    } else {
                        b"\x1b[B"
                    };
                    let per_step = if precise { 1 } else { 3 };
                    self.session.write(&arrow.repeat(lines.abs() as usize * per_step));
                }
                return;
            }
        }
        self.scroll_by(-dy * 3.0 * ctx.text.cell.height);
        // A trackpad's deltas are already smooth: follow the fingers
        // exactly instead of easing after them.
        if precise {
            self.scroll = self.scroll_target;
        }
    }

    fn alt_cell(&self, ctx: &Ctx, x: f32, y: f32) -> Option<(usize, usize)> {
        let (_, ox, oy) = self.layout.alt?;
        let cell = ctx.text.cell;
        let column = ((x - ox) / cell.width).floor().max(0.0) as usize;
        let line = ((y - oy) / cell.height).floor().max(0.0) as usize;
        Some((column.min(self.session.size.columns - 1), line.min(self.session.size.lines - 1)))
    }

    fn mouse_report(&self, button: u8, column: usize, line: usize, pressed: bool) {
        let Some(block) = self.session.active() else { return };
        let mode = *block.term.mode();
        if mode.contains(TermMode::SGR_MOUSE) {
            let end = if pressed { 'M' } else { 'm' };
            self.session.write(format!("\x1b[<{button};{};{}{end}", column + 1, line + 1).as_bytes());
        } else if column < 223 && line < 223 {
            let code = if pressed { button } else { 3 };
            self.session.write(&[0x1b, b'[', b'M', 32 + code, 33 + column as u8, 33 + line as u8]);
        }
    }

    pub fn mouse_down(&mut self, ctx: &Ctx, x: f32, y: f32, pressed: i32, clicks: i32, mods: u32) {
        let shift = mods & platform::mods::SHIFT != 0;
        let button = if pressed & buttons::SECONDARY != 0 {
            2
        } else if pressed & buttons::TERTIARY != 0 {
            1
        } else {
            0
        };
        self.link_click = None;
        if let Some((track, thumb)) = self.layout.scrollbar.filter(|(track, _)| button == 0 && track.contains(x, y)) {
            // Grab the thumb where it was pressed; a press beside it jumps
            // there and grabs it by the middle.
            let top = self.thumb_top(track, thumb);
            let grab = if (top..top + thumb).contains(&y) { y - top } else { thumb / 2.0 };
            self.bar_grab = Some(grab);
            self.drag_scrollbar(y);
            return;
        }
        if button == 0 && clicks == 1 {
            if let Some((_, link)) = self.link_under(ctx, x, y) {
                // Cmd+click opens at once, even over programs that take the
                // mouse; a plain click waits to see it isn't a drag.
                if mods & platform::mods::COMMAND != 0 {
                    platform::open_url(&link.url);
                    return;
                }
                self.link_click = Some(link.url);
            }
        }
        if let Some((index, _, _)) = self.layout.alt {
            let block = &self.session.blocks[index];
            let mode = *block.term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) && !shift {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(button, column, line, true);
                }
                return;
            }
            if button == 1 {
                return self.paste(platform::selection_get());
            }
            if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                let id = block.id;
                self.start_selection(id, false, line, column, clicks);
            }
            return;
        }
        // Middle click pastes the primary selection, where there is one.
        if button == 1 {
            return self.paste(platform::selection_get());
        }
        if let Some((_, item)) = self.layout.menu_rows.iter().find(|(rect, _)| rect.contains(x, y)) {
            let item = *item;
            if let Some(completion) = &mut self.completion {
                completion.selected = item;
            }
            self.accept_completion();
            return;
        }
        self.completion = None;
        if self.layout.input.contains(x, y) {
            self.place_editor_cursor(ctx, x, y, shift);
            self.selected = None;
            return;
        }
        let hit = self
            .layout
            .blocks
            .iter()
            .find(|b| b.rect.contains(x, y))
            .map(|b| (b.id, b.output_x, b.output_y, b.lines, b.command_y, b.command_rows));
        let Some((id, ox, oy, lines, cy, rows)) = hit else {
            self.selected = None;
            self.selection = None;
            return;
        };
        let cell = ctx.text.cell;
        let columns = self.session.size.columns;
        let column = (((x - ox) / cell.width).max(0.0) as usize).min(columns - 1);
        if button == 0 && lines > 0 && y >= oy && y < oy + lines as f32 * cell.height {
            let line = ((y - oy) / cell.height) as usize;
            self.start_selection(id, false, line.min(lines - 1), column, clicks);
            self.selected = Some(id);
        } else if button == 0 && rows > 0 && y >= cy && y < cy + rows as f32 * cell.height {
            let row = ((y - cy) / cell.height) as usize;
            self.start_selection(id, true, row.min(rows - 1), column, clicks);
            self.selected = Some(id);
        } else {
            self.selection = None;
            self.selected = if self.selected == Some(id) && button == 0 { None } else { Some(id) };
        }
    }

    fn start_selection(&mut self, block: u64, command: bool, line: usize, column: usize, clicks: i32) {
        let mode = match clicks {
            2 => SelectMode::Word,
            n if n >= 3 => SelectMode::Line,
            _ => SelectMode::Char,
        };
        self.selection = Some(Selection { block, command, anchor: (line, column), head: (line, column), mode });
        self.dragging = true;
    }

    /// The pointer moved over (or, while dragging, away from) the pane;
    /// returns the pointer shape it wants.
    pub fn mouse_moved(&mut self, ctx: &Ctx, x: f32, y: f32, buttons: i32) -> Cursor {
        // Pointer shape: an arrow over chrome, the I-beam over text.
        let over_text = self.layout.alt.is_some()
            || self.layout.input.contains(x, y)
            || self.layout.blocks.iter().any(|b| b.rect.contains(x, y) && y >= b.command_y);
        self.pointer = Some((x, y));
        if self.bar_grab.is_some() && buttons != 0 {
            self.drag_scrollbar(y);
            return Cursor::Arrow;
        }
        if self.layout.scrollbar.is_some_and(|(track, _)| track.contains(x, y)) {
            return Cursor::Arrow;
        }
        let cursor = if buttons == 0 && self.link_under(ctx, x, y).is_some() {
            Cursor::Link
        } else if over_text {
            Cursor::IBeam
        } else {
            Cursor::Arrow
        };

        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if buttons != 0 && mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(32, column, line, true);
                }
                return cursor;
            }
        }
        if !self.dragging || buttons == 0 {
            return cursor;
        }
        let Some(mut selection) = self.selection else { return cursor };
        let cell = ctx.text.cell;
        let (line, column) = if let Some((_, ox, oy)) = self.layout.alt {
            let column = ((x - ox) / cell.width).max(0.0) as usize;
            let line = ((y - oy) / cell.height).max(0.0) as usize;
            (line.min(self.session.size.lines - 1), column)
        } else {
            let Some(layout) = self.layout.blocks.iter().find(|b| b.id == selection.block) else { return cursor };
            let (top, lines) = if selection.command {
                (layout.command_y, layout.command_rows)
            } else {
                (layout.output_y, layout.lines)
            };
            if lines == 0 {
                return cursor;
            }
            let line = ((y - top) / cell.height).floor().clamp(0.0, (lines - 1) as f32) as usize;
            let column = ((x - layout.output_x) / cell.width).max(0.0) as usize;
            (line, column)
        };
        selection.head = (line, column.min(self.session.size.columns - 1));
        self.selection = Some(selection);
        cursor
    }

    pub fn mouse_up(&mut self, ctx: &Ctx, x: f32, y: f32) {
        if let Some((index, _, _)) = self.layout.alt {
            let mode = *self.session.blocks[index].term.mode();
            if mode.intersects(TermMode::MOUSE_MODE) {
                if let Some((column, line)) = self.alt_cell(ctx, x, y) {
                    self.mouse_report(0, column, line, false);
                }
            }
        }
        self.bar_grab = None;
        let was_dragging = std::mem::take(&mut self.dragging);
        let link = self.link_click.take();
        if let Some(selection) = self.selection {
            if selection.mode == SelectMode::Char && selection.anchor == selection.head {
                self.selection = None;
                if let Some(url) = link {
                    platform::open_url(&url);
                }
            } else if was_dragging {
                if let Some(text) = self.selection_text() {
                    platform::selection_set(&text);
                }
            }
        }
    }

    /// Where the scroll bar's thumb starts, for the current scroll.
    fn thumb_top(&self, track: Rect, thumb: f32) -> f32 {
        track.y + (1.0 - self.scroll / self.max_scroll().max(1.0)) * (track.h - thumb)
    }

    /// Scrolls so the grabbed point of the thumb sits at `y`.
    fn drag_scrollbar(&mut self, y: f32) {
        let (Some((track, thumb)), Some(grab)) = (self.layout.scrollbar, self.bar_grab) else { return };
        let travel = (track.h - thumb).max(1.0);
        let fraction = ((y - grab - track.y) / travel).clamp(0.0, 1.0);
        self.scroll_target = (1.0 - fraction) * self.max_scroll();
        self.scroll = self.scroll_target;
    }

    /// The pointer left this pane.
    pub fn pointer_left(&mut self) {
        self.pointer = None;
    }

    /// The link under pane point (`x`, `y`), with the index of its block.
    fn link_under(&self, ctx: &Ctx, x: f32, y: f32) -> Option<(usize, Link)> {
        let cell = ctx.text.cell;
        let (index, ox, oy, lines) = if let Some((index, ox, oy)) = self.layout.alt {
            (index, ox, oy, self.session.size.lines)
        } else {
            let hit = self.layout.blocks.iter().find(|b| b.rect.contains(x, y))?;
            let index = self.session.blocks.iter().position(|b| b.id == hit.id)?;
            (index, hit.output_x, hit.output_y, hit.lines)
        };
        if x < ox || y < oy || self.layout.input.contains(x, y) {
            return None;
        }
        if self.layout.scrollbar.is_some_and(|(track, _)| track.contains(x, y)) {
            return None;
        }
        let column = ((x - ox) / cell.width) as usize;
        let line = ((y - oy) / cell.height) as usize;
        if column >= self.session.size.columns || line >= lines {
            return None;
        }
        let block = &self.session.blocks[index];
        // The alternate screen is drawn from its top, not its scrollback.
        let line = if self.layout.alt.is_some() { line + block.term.grid().history_size() } else { line };
        block.link_at(line, column).map(|link| (index, link))
    }

    /// Underlines the link under the pointer.
    fn draw_link(&self, scene: &mut Scene, ctx: &Ctx) {
        let Some((x, y)) = self.pointer else { return };
        let Some((index, link)) = self.link_under(ctx, x, y) else { return };
        let cell = ctx.text.cell;
        let block = &self.session.blocks[index];
        let (ox, oy, skip) = if let Some((_, ox, oy)) = self.layout.alt {
            (ox, oy, block.term.grid().history_size())
        } else {
            let Some(layout) = self.layout.blocks.iter().find(|b| b.id == block.id) else { return };
            (layout.output_x, layout.output_y, 0)
        };
        let input_top = if self.layout.input.h > 0.0 { self.layout.input.y } else { f32::MAX };
        for &(line, from, to) in &link.cells {
            let Some(line) = line.checked_sub(skip) else { continue };
            let row = oy + line as f32 * cell.height;
            if row + cell.height > input_top {
                continue;
            }
            let rect = Rect::new(
                ox + from as f32 * cell.width,
                row + cell.height - 2.0,
                (to + 1 - from) as f32 * cell.width,
                1.0,
            );
            scene.fill(rect, ctx.theme.accent);
        }
    }

    fn place_editor_cursor(&mut self, ctx: &Ctx, x: f32, y: f32, extend: bool) {
        if !self.session.editor_active() {
            return;
        }
        let cell = ctx.text.cell;
        let line = ((y - self.layout.editor_y) / cell.height).floor().max(0.0) as usize;
        let column = ((x - self.layout.editor_x) / cell.width).round().max(0.0) as usize;
        let line = line.min(self.editor.line_count() - 1);
        let to = self.editor.offset(line, column);
        if extend {
            self.editor.anchor.get_or_insert(self.editor.cursor);
        } else {
            self.editor.anchor = None;
        }
        self.editor.cursor = to;
    }

    /// The top of the command input, in pane coordinates (toasts sit above it).
    pub fn input_top(&self) -> f32 {
        if self.layout.input.h > 0.0 {
            self.layout.input.y
        } else {
            self.rect.h
        }
    }

    // ---- drawing ------------------------------------------------------

    /// Draws the pane at its place in the window.
    pub fn draw(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        scene.set_origin(self.rect.x, self.rect.y);
        scene.push_clip(Rect::new(0.0, 0.0, self.rect.w, self.rect.h));
        self.layout = Layout::default();
        let alt = self.session.active().is_some_and(|b| b.alt_screen());
        if alt {
            self.draw_alt(scene, ctx, focused);
        } else {
            self.draw_main(scene, ctx, focused);
        }
        self.draw_link(scene, ctx);
        self.draw_recap(scene, ctx, alt);
        if self.bell.is_some() {
            scene.set_layer(LAYER_OVERLAY);
            let edge = Rect::new(0.0, 0.0, self.rect.w, self.rect.h);
            scene.quad(Quad::new(edge, Color::TRANSPARENT).border(3.0, ctx.theme.warning.alpha(0.7)));
            scene.set_layer(LAYER_CONTENT);
        }
        scene.pop_clip();
        scene.set_origin(0.0, 0.0);
    }

    fn selection_span(&self, id: u64) -> Option<Span> {
        let selection = self.selection.filter(|s| s.block == id && !s.command)?;
        let block = self.session.blocks.iter().find(|b| b.id == id)?;
        let (mut start, mut end) = normalize(selection);
        let columns = self.session.size.columns;
        match selection.mode {
            SelectMode::Char => {}
            SelectMode::Line => {
                start.1 = 0;
                end.1 = columns - 1;
            }
            SelectMode::Word => {
                let grid = block.term.grid();
                let is_word = |line: usize, column: usize| {
                    let c = grid[block.grid_line(line)][alacritty_terminal::index::Column(column)].c;
                    !c.is_whitespace() && !"()[]{}<>'\"`|;,".contains(c)
                };
                while start.1 > 0 && is_word(start.0, start.1 - 1) {
                    start.1 -= 1;
                }
                while end.1 + 1 < columns && is_word(end.0, end.1 + 1) {
                    end.1 += 1;
                }
            }
        }
        Some((start, end))
    }

    /// The selection within a block's command, in its wrapped rows.
    fn command_span(&self, id: u64) -> Option<Span> {
        let selection = self.selection.filter(|s| s.block == id && s.command)?;
        let block = self.session.blocks.iter().find(|b| b.id == id)?;
        let (mut start, mut end) = normalize(selection);
        let columns = self.session.size.columns.max(1);
        match selection.mode {
            SelectMode::Char => {}
            SelectMode::Line => {
                start.1 = 0;
                end.1 = columns - 1;
            }
            SelectMode::Word => {
                let rows = command_rows(&block.command, columns);
                let row = |line: usize| rows.get(line).map(|(s, _)| s.chars().collect::<Vec<_>>()).unwrap_or_default();
                let is_word = |chars: &[char], column: usize| {
                    chars.get(column).is_some_and(|&c| !c.is_whitespace() && !"()[]{}<>'\"`|;,".contains(c))
                };
                let (first, last) = (row(start.0), row(end.0));
                while start.1 > 0 && is_word(&first, start.1 - 1) {
                    start.1 -= 1;
                }
                while end.1 + 1 < columns && is_word(&last, end.1 + 1) {
                    end.1 += 1;
                }
            }
        }
        Some((start, end))
    }

    fn draw_alt(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        let Some(index) = self.session.blocks.iter().rposition(|b| b.running() && b.kind == BlockKind::Command) else {
            return;
        };
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let blink = ctx.blink_on();
        let (x, y) = (PAD_X, ALT_PAD_Y);
        self.layout.alt = Some((index, x, y));
        let lines = self.session.blocks[index].term.grid().screen_lines();
        let view = GridView {
            block: &self.session.blocks[index],
            x,
            y,
            first: 0,
            last: lines,
            cursor: true,
            focused,
            selection,
            blink_on: blink,
        };
        ui::draw_grid(scene, &mut ctx.text, &ctx.theme, view);
    }

    fn block_height(&mut self, text: &TextSystem, index: usize) -> (f32, usize) {
        let cell = text.cell;
        let columns = self.session.size.columns.max(1);
        let block = &mut self.session.blocks[index];
        let command_lines = if block.kind != BlockKind::Background {
            block.command.split('\n').map(|line| crate::editor::columns(line).div_ceil(columns).max(1)).sum()
        } else {
            0
        };
        let output = if block.alt_screen() { 0 } else { block.output_lines() };
        let mut height = BLOCK_PAD_TOP + META_HEIGHT + command_lines as f32 * cell.height + BLOCK_PAD_BOTTOM;
        if output > 0 {
            height += OUTPUT_GAP + output as f32 * cell.height;
        }
        (height + 1.0, output)
    }

    fn input_height(&self, text: &TextSystem) -> f32 {
        let lines = if self.session.editor_active() { self.editor.line_count().min(MAX_EDITOR_LINES) } else { 1 };
        INPUT_PAD + CHIP_HEIGHT + 8.0 + lines as f32 * text.cell.height + INPUT_PAD + 2.0
    }

    fn draw_main(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool) {
        let (width, height) = (self.rect.w, self.rect.h);
        let input_height = self.input_height(&ctx.text);
        let welcome = self.welcome && self.session.blocks.is_empty();
        let welcome_height = if welcome { self.welcome_height(&ctx.text) } else { 0.0 };

        let mut heights = Vec::with_capacity(self.session.blocks.len());
        let mut total = welcome_height;
        for index in 0..self.session.blocks.len() {
            let h = self.block_height(&ctx.text, index);
            total += h.0;
            heights.push(h);
        }
        // Keep the view still when output grows while scrolled up.
        let list_max = (height - input_height).max(0.0);
        if self.scroll_target > 0.0 && self.content_height > 0.0 && total > self.content_height {
            let grown = total - self.content_height;
            self.scroll += grown;
            self.scroll_target += grown;
        }
        self.content_height = total;
        let max_scroll = (total - list_max).max(0.0);
        self.scroll_target = self.scroll_target.min(max_scroll);
        self.scroll = self.scroll.min(max_scroll);
        let (top, input_y) =
            if total <= list_max { (0.0, total) } else { (list_max - total + self.scroll.round(), list_max) };
        self.layout.list = Rect::new(0.0, 0.0, width, input_y);
        self.layout.content_height = total;

        scene.push_clip(Rect::new(0.0, 0.0, width, input_y));
        if welcome_height > 0.0 {
            self.draw_welcome(scene, ctx, top);
        }
        let mut y = top + welcome_height;
        for (index, (block_height, output)) in heights.into_iter().enumerate() {
            if y + block_height > 0.0 && y < input_y {
                self.draw_block(scene, ctx, focused, index, y, block_height, output);
            }
            y += block_height;
        }
        scene.pop_clip();

        if total > list_max && list_max > 40.0 {
            let thumb = (list_max / total * list_max).max(24.0);
            let position = (1.0 - self.scroll / max_scroll.max(1.0)) * (list_max - thumb);
            let track = Rect::new(width - SCROLLBAR_HIT, 0.0, SCROLLBAR_HIT, list_max);
            self.layout.scrollbar = Some((track, thumb));
            // Wider and easier to grab under the pointer.
            let hovered = self.bar_grab.is_some() || self.pointer.is_some_and(|(x, y)| track.contains(x, y));
            if hovered {
                scene.fill(track, ctx.theme.text_faint.alpha(0.06));
                let bar = Rect::new(width - 10.0, position + 2.0, 7.0, thumb - 4.0);
                let alpha = if self.bar_grab.is_some() { 0.85 } else { 0.65 };
                scene.quad(Quad::new(bar, ctx.theme.text_faint.alpha(alpha)).radius(3.5));
            } else {
                let bar = Rect::new(width - 6.0, position + 2.0, 3.0, thumb - 4.0);
                scene.quad(Quad::new(bar, ctx.theme.text_faint.alpha(0.45)).radius(1.5));
            }
        }
        self.draw_input(scene, ctx, focused, input_y, input_height);
    }

    /// Whether the pane is wide enough for the ASCII-art banner.
    fn banner_fits(&self) -> bool {
        self.session.size.columns >= BANNER[0].chars().count() + 2
    }

    fn welcome_height(&self, text: &TextSystem) -> f32 {
        let title = if self.banner_fits() { BANNER.len() as f32 * text.cell.height } else { 30.0 };
        24.0 + title + 30.0 + 3.0 * 28.0 + 12.0
    }

    fn draw_welcome(&mut self, scene: &mut Scene, ctx: &mut Ctx, top: f32) {
        let banner = self.banner_fits();
        let Ctx { text, theme, device_name, .. } = ctx;
        let x = PAD_X + 6.0;
        let mut y = top + 24.0;
        if banner {
            // The three leaves of the Haiku logo, left to right: green,
            // orange, peach, blended by hue so green turns yellow on its way
            // to orange. Top rows catch a little light like the leaves'
            // upper edges; the box-drawing "shadow" strokes sit darker.
            let leaves = [Color::hex(0x5ae202), Color::hex(0xff8203), Color::hex(0xffc468)];
            let columns = BANNER[0].chars().count() as f32 - 1.0;
            for (row, line) in BANNER.iter().enumerate() {
                let light = 0.22 * (1.0 - row as f32 / (BANNER.len() - 1) as f32);
                for (column, c) in line.chars().enumerate() {
                    let t = column as f32 / columns * 2.0;
                    let leaf =
                        if t < 1.0 { leaves[0].mix_hue(leaves[1], t) } else { leaves[1].mix_hue(leaves[2], t - 1.0) };
                    let color = if c == '█' {
                        leaf.mix(Color::hex(0xffffff), light)
                    } else {
                        leaf.mix(theme.background, 0.55)
                    };
                    let cx = x + column as f32 * text.cell.width;
                    text.draw_cell(scene, c, cx, y, Style::default(), color, 1);
                }
                y += text.cell.height;
            }
        } else {
            text.draw_text(scene, "Temino", x, y + 26.0, UI_BOLD, 28.0, theme.text);
            y += 30.0;
        }
        let tagline = format!("A modern GPU-accelerated terminal for {}", platform::OS_NAME);
        let width = text.draw_text(scene, &tagline, x, y + 22.0, UI, 13.0, theme.text_dim);
        let api = if cfg!(target_os = "macos") { "Metal" } else { "Vulkan" };
        let device = format!("  ·  {api} on {device_name}  ·  v{}", env!("CARGO_PKG_VERSION"));
        text.draw_text(scene, &device, x + width, y + 22.0, UI, 13.0, theme.text_faint);
        let y = y + 36.0;
        let cmd = |key: &str| format!("{}+{key}", platform::COMMAND_NAME);
        let rows: [Vec<(String, &str)>; 3] = [
            vec![
                ("Enter".into(), "run"),
                ("Shift+Enter".into(), "new line"),
                ("Tab".into(), "complete · AI on empty"),
                ("↑ ↓".into(), "history"),
                ("Ctrl+R".into(), "search history"),
            ],
            vec![
                (cmd("T"), "new tab"),
                (cmd("D"), "split right"),
                (cmd("E"), "split down"),
                (cmd("[ ]"), "switch pane"),
                (cmd("W"), "close"),
            ],
            vec![
                (cmd("K"), "clear"),
                (cmd("↑ ↓"), "select blocks"),
                (cmd("C"), "copy"),
                (cmd("V"), "paste"),
                (cmd("+ −"), "font size"),
            ],
        ];
        for (row, hints) in rows.iter().enumerate() {
            let mut hx = x;
            let hy = y + row as f32 * 28.0;
            for (key, label) in hints.iter() {
                hx += ui::hint(scene, text, theme, key, label, hx, hy) + 18.0;
            }
        }
    }

    fn draw_block(
        &mut self,
        scene: &mut Scene,
        ctx: &mut Ctx,
        focused: bool,
        index: usize,
        y: f32,
        block_height: f32,
        output: usize,
    ) {
        let width = self.rect.w;
        let pane_height = self.rect.h;
        let cell = ctx.text.cell;
        let blink = ctx.blink_on();
        let id = self.session.blocks[index].id;
        let selection = self.selection_span(id);
        let command_selection = self.command_span(id);
        let selected = self.selected == Some(id);
        let user_columns = self.session.size.columns.max(1);
        let Ctx { text, theme, .. } = ctx;
        let block = &self.session.blocks[index];
        let rect = Rect::new(0.0, y, width, block_height - 1.0);

        if selected {
            scene.fill(rect, theme.accent.alpha(0.07));
            scene.fill(Rect::new(0.0, y, 3.0, rect.h), theme.accent);
        } else if block.failed() {
            scene.fill(rect, theme.error.alpha(0.055));
            scene.fill(Rect::new(0.0, y, 3.0, rect.h), theme.error.alpha(0.8));
        }

        // Meta line: where it ran, and how it went.
        let ascent = text.ui_ascent(12.0);
        let baseline = (y + BLOCK_PAD_TOP + (META_HEIGHT + ascent * 0.72) / 2.0).round();
        let mut x = PAD_X;
        if block.kind == BlockKind::Background {
            text.draw_text(scene, "Background output", x, baseline, UI, 12.0, theme.text_faint);
        } else {
            if block.kind == BlockKind::Ai {
                let label = if text.has_glyph('✦', UI_BOLD) { "✦ AI  " } else { "AI  " };
                x += text.draw_text(scene, label, x, baseline, UI_BOLD, 12.0, theme.variable);
            }
            x += text.draw_text(scene, &ui::short_path(&block.cwd), x, baseline, UI, 12.0, theme.text_dim);
            if let Some(branch) = &block.branch {
                x += 10.0;
                x += text.draw_text(scene, "⎇ ", x, baseline, UI, 12.0, theme.variable.alpha(0.9));
                text.draw_text(scene, branch, x, baseline, UI, 12.0, theme.variable.alpha(0.9));
            }
            let (status, color) = if block.running() {
                let frame = (block.started.elapsed().as_millis() / 80) as usize % SPINNER.len();
                (format!("{}  {}", SPINNER[frame], ui::format_duration(block.duration())), theme.accent)
            } else {
                match block.exit {
                    Some(0) | None => (format!("{}  ✓", ui::format_duration(block.duration())), theme.success),
                    Some(_) if block.kind == BlockKind::Ai => {
                        (format!("{}  failed", ui::format_duration(block.duration())), theme.error)
                    }
                    Some(code) => (format!("{}  exit {code}", ui::format_duration(block.duration())), theme.error),
                }
            };
            let status_width = text.measure(&status, UI, 12.0);
            text.draw_text(scene, &status, width - PAD_X - status_width, baseline, UI, 12.0, color);
        }

        // The command, wrapped to the grid width.
        let command_y = y + BLOCK_PAD_TOP + META_HEIGHT;
        let mut line_y = command_y;
        let mut command_rows_drawn = 0;
        if block.kind != BlockKind::Background {
            for (index, (row, _)) in command_rows(&block.command, user_columns).iter().enumerate() {
                if let Some(((l0, c0), (l1, c1))) =
                    command_selection.filter(|((l0, _), (l1, _))| (*l0..=*l1).contains(&index))
                {
                    let len = row.chars().count();
                    let from = if index == l0 { c0.min(len) } else { 0 };
                    let to = if index == l1 { (c1 + 1).min(len) } else { len };
                    if to > from {
                        let x = PAD_X + from as f32 * cell.width;
                        scene.fill(Rect::new(x, line_y, (to - from) as f32 * cell.width, cell.height), theme.selection);
                    }
                }
                text.draw_mono(scene, row, PAD_X, line_y, Style::BOLD, theme.text);
                line_y += cell.height;
                command_rows_drawn += 1;
            }
        }

        let output_y = line_y + OUTPUT_GAP;
        if output > 0 {
            // Only the visible slice of long output is drawn.
            let first = ((-output_y) / cell.height).floor().max(0.0) as usize;
            let last = (((pane_height - output_y) / cell.height).ceil().max(0.0) as usize).min(output);
            if first < last {
                let view = GridView {
                    block,
                    x: PAD_X,
                    y: output_y + first as f32 * cell.height,
                    first,
                    last,
                    cursor: block.running() && block.kind == BlockKind::Command,
                    focused,
                    selection,
                    blink_on: blink,
                };
                ui::draw_grid(scene, text, theme, view);
            }
        }
        scene.fill(Rect::new(0.0, y + block_height - 1.0, width, 1.0), theme.divider);
        self.layout.blocks.push(BlockLayout {
            id,
            rect,
            output_x: PAD_X,
            output_y,
            lines: output,
            command_y,
            command_rows: command_rows_drawn,
        });
    }

    fn draw_input(&mut self, scene: &mut Scene, ctx: &mut Ctx, focused: bool, y: f32, height: f32) {
        let width = self.rect.w;
        let cell = ctx.text.cell;
        let blink = ctx.blink_on() && focused;
        let phase = self.session.phase;
        let hint = if self.completion.is_none() { self.suggestion(ctx) } else { None };
        let Ctx { text, theme, commands, user_host, .. } = ctx;
        let Pane { session, editor, layout, completion, ai, .. } = self;
        let ai = *ai && phase == Phase::Ready;
        let panel = Rect::new(0.0, y, width, height);
        layout.input = panel;
        scene.fill(panel, theme.panel(SURFACE_LIFT));
        scene.fill(Rect::new(0.0, y, width, 1.0), theme.divider);

        // Context chips.
        let chip_y = y + INPUT_PAD;
        let mut x = PAD_X;
        x += ui::chip(scene, text, &ui::short_path(&session.cwd), x, chip_y, CHIP_HEIGHT, theme.raised, theme.accent)
            + 6.0;
        if let Some(branch) = &session.branch {
            x += ui::chip(scene, text, &format!("⎇ {branch}"), x, chip_y, CHIP_HEIGHT, theme.raised, theme.variable)
                + 6.0;
        }
        ui::chip(scene, text, user_host, x, chip_y, CHIP_HEIGHT, theme.raised, theme.text_dim);

        let editor_y = chip_y + CHIP_HEIGHT + 8.0;
        let text_x = PAD_X + 2.0 * cell.width;
        layout.editor_x = text_x;
        layout.editor_y = editor_y;
        let prompt_color = match phase {
            Phase::Continuation => theme.warning,
            _ if !focused => theme.text_faint,
            _ if ai => theme.variable,
            _ => theme.accent,
        };
        let prompt = match (ai, text.has_glyph('❯', 0)) {
            (true, _) if text.has_glyph('✦', 0) => "✦",
            (true, _) => "?",
            (false, true) => "❯",
            (false, false) => ">",
        };
        text.draw_mono(scene, prompt, PAD_X, editor_y, Style::BOLD, prompt_color);

        let status = |scene: &mut Scene, text: &mut TextSystem, message: &str, color: Color| {
            let baseline = editor_y + text.cell.baseline;
            text.draw_text(scene, message, text_x, baseline, UI, 13.0, color);
        };
        match phase {
            Phase::Boot => status(scene, text, "Starting bash…", theme.text_faint),
            Phase::Exited => {
                let code = session.exit_code.unwrap_or(0);
                status(
                    scene,
                    text,
                    &format!("The shell exited (status {code}). Press Enter to start a new one."),
                    theme.text_dim,
                )
            }
            Phase::Running => {
                let message = format!(
                    "Running · keystrokes go to the program · Ctrl+C interrupts · {}+C copies",
                    platform::COMMAND_NAME
                );
                status(scene, text, &message, theme.text_faint)
            }
            Phase::Prompt | Phase::Submitted => {}
            Phase::Ready | Phase::Continuation => {
                let first_line = editor.position(editor.cursor).0.saturating_sub(MAX_EDITOR_LINES - 1);
                if editor.text.is_empty() && !ctx.composing {
                    let hint = if phase == Phase::Continuation {
                        "The command needs more input; Ctrl+C cancels it"
                    } else if ai {
                        "Ask AI · describe a command, or ask anything · Tab back to the shell"
                    } else {
                        "Type a command · Tab here for AI · Tab completes · ↑ history"
                    };
                    status(scene, text, hint, theme.text_faint);
                }
                // Selection.
                if let Some((a, b)) = editor.selection() {
                    let (l0, c0) = editor.position(a);
                    let (l1, c1) = editor.position(b);
                    for line in l0..=l1 {
                        if line < first_line || line >= first_line + MAX_EDITOR_LINES {
                            continue;
                        }
                        let from = if line == l0 { c0 } else { 0 };
                        let to = if line == l1 {
                            c1
                        } else {
                            editor.text.split('\n').nth(line).map_or(0, crate::editor::columns) + 1
                        };
                        let ly = editor_y + (line - first_line) as f32 * cell.height;
                        let rect = Rect::new(
                            text_x + from as f32 * cell.width,
                            ly,
                            (to.saturating_sub(from)) as f32 * cell.width,
                            cell.height,
                        );
                        scene.fill(rect, theme.selection);
                    }
                }
                // Text with syntax colors.
                commands.use_path(self.session.path.as_deref());
                let spans = if ai { Vec::new() } else { ui::highlight(&editor.text, editor.cursor, commands, theme) };
                let mut colors = vec![theme.text; editor.text.len()];
                for (start, end, color) in spans {
                    for slot in &mut colors[start..end.min(editor.text.len())] {
                        *slot = color;
                    }
                }
                let (mut line, mut column) = (0usize, 0usize);
                for (offset, c) in editor.text.char_indices() {
                    if c == '\n' {
                        line += 1;
                        column = 0;
                        continue;
                    }
                    let span = crate::editor::width(c);
                    if line >= first_line && line < first_line + MAX_EDITOR_LINES && span > 0 {
                        let cx = text_x + column as f32 * cell.width;
                        let cy = editor_y + (line - first_line) as f32 * cell.height;
                        text.draw_cell(scene, c, cx, cy, Style::default(), colors[offset], span);
                    }
                    column += span;
                }
                if let Some(rest) = hint.as_deref() {
                    let (sl, sc) = editor.position(editor.text.len());
                    if sl >= first_line {
                        let sy = editor_y + (sl - first_line) as f32 * cell.height;
                        text.draw_mono(
                            scene,
                            rest,
                            text_x + sc as f32 * cell.width,
                            sy,
                            Style::default(),
                            theme.text_faint,
                        );
                    }
                }
                let (cl, cc) = editor.position(editor.cursor);
                if cl >= first_line {
                    let cy = editor_y + (cl - first_line) as f32 * cell.height;
                    let caret = Rect::new(text_x + cc as f32 * cell.width - 1.0, cy + 1.0, 2.0, cell.height - 2.0);
                    if focused {
                        scene.mark_caret(Rect::new(caret.x + 1.0, cy, cell.width, cell.height));
                    }
                    if blink {
                        scene.fill(caret, theme.cursor);
                    }
                }
                if let Some(completion) = completion {
                    let (_, word_column) = editor.position(completion.start.min(editor.text.len()));
                    draw_menu(
                        scene,
                        text,
                        theme,
                        layout,
                        completion,
                        width,
                        text_x + word_column as f32 * cell.width,
                        editor_y,
                    );
                }
            }
        }
    }
}

fn draw_menu(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    layout: &mut Layout,
    completion: &Completion,
    pane_width: f32,
    x: f32,
    editor_y: f32,
) {
    let cell = text.cell;
    scene.set_layer(LAYER_OVERLAY);
    let rows = completion.candidates.len().min(MENU_ROWS);
    let row_height = cell.height + 6.0;
    let first = completion.selected.saturating_sub(rows - 1);
    let label_width = completion.candidates[first..first + rows]
        .iter()
        .map(|c| c.label.chars().count().min(80) as f32 * cell.width)
        .fold(0.0, f32::max);
    let width = (label_width + 90.0).clamp(220.0, (pane_width - 2.0 * PAD_X).max(220.0));
    let footer = if completion.candidates.len() > rows { 18.0 } else { 0.0 };
    let height = rows as f32 * row_height + 8.0 + footer;
    let x = (x - 10.0).min(pane_width - width - PAD_X).max(PAD_X);
    let y = (editor_y - height - 10.0).max(4.0);
    let rect = Rect::new(x, y, width, height);
    scene.shadow(rect, Color::hex(0x000000).alpha(0.55), 8.0, 10.0);
    scene.quad(Quad::new(rect, theme.raised).radius(8.0).border(1.0, theme.divider));
    scene.push_clip(rect.inset(4.0, 4.0));
    for row in 0..rows {
        let item = first + row;
        let candidate = &completion.candidates[item];
        let ry = y + 4.0 + row as f32 * row_height;
        let row_rect = Rect::new(x + 4.0, ry, width - 8.0, row_height);
        if item == completion.selected {
            scene.quad(Quad::new(row_rect, theme.accent.alpha(0.22)).radius(5.0));
        }
        let color = match candidate.kind {
            Kind::Directory => theme.accent,
            Kind::Command => theme.success,
            _ => theme.text,
        };
        let label: String = candidate.label.chars().take(80).collect();
        text.draw_mono(scene, &label, x + 12.0, ry + 3.0, Style::default(), color);
        let tag = match candidate.kind {
            Kind::Command => "command",
            Kind::Directory => "folder",
            Kind::File => "file",
            Kind::History => "history",
        };
        let tag_width = text.measure(tag, UI, 11.0);
        text.draw_text(
            scene,
            tag,
            x + width - tag_width - 12.0,
            ry + 3.0 + text.cell.baseline - 1.0,
            UI,
            11.0,
            theme.text_faint,
        );
        layout.menu_rows.push((row_rect, item));
    }
    if completion.candidates.len() > rows {
        let more = format!("{} of {}", completion.selected + 1, completion.candidates.len());
        let w = text.measure(&more, UI, 10.0);
        text.draw_text(scene, &more, x + width - w - 12.0, y + height - 8.0, UI, 10.0, theme.text_faint);
    }
    scene.pop_clip();
    scene.set_layer(LAYER_CONTENT);
}

/// A command's rows as drawn, wrapped to the grid width; the flag marks
/// the last row of each of its lines.
fn command_rows(command: &str, columns: usize) -> Vec<(String, bool)> {
    let mut rows = Vec::new();
    for line in command.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let chunks: Vec<&[char]> = chars.chunks(columns).chain(chars.is_empty().then_some(&[][..])).collect();
        let last = chunks.len() - 1;
        rows.extend(chunks.into_iter().enumerate().map(|(i, chunk)| (chunk.iter().collect(), i == last)));
    }
    rows
}

fn normalize(selection: Selection) -> ((usize, usize), (usize, usize)) {
    if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    }
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
